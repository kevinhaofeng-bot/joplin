//! Whole-library backup and restore into a new library.
//!
//! Independent design (Evernote's ENEX exporter streams notes without
//! notebooks/stacks and skips unreadable attachments): a backup is a
//! consistent SQLite snapshot (`VACUUM INTO`, so the running app is not
//! blocked) plus every content-addressed blob the snapshot references and a
//! versioned manifest with their hashes. It is assembled in a temporary
//! sibling and renamed into a target that must not exist. Restore verifies
//! every hash, drops pending sync state so a new client does not replay the
//! old device's operations, and publishes a new library through
//! [`super::publish_staged_library`]; it never touches an existing library.
//! The search index travels inside the snapshot.

use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use thiserror::Error;

use super::{LibraryCounts, PublishError, PublishedLibrary, library_counts};
use crate::{BlobHash, LibraryRepository, ResourceError, ResourceStore};

pub const LIBRARY_BACKUP_FORMAT: &str = "joplin-lite-library-backup";
pub const LIBRARY_BACKUP_VERSION: u64 = 1;
const MANIFEST: &str = "manifest.json";
const DATABASE: &str = "library.sqlite";
const BLOBS: &str = "blobs";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupReport {
    pub counts: LibraryCounts,
    pub blobs: usize,
    pub bytes: u64,
}

#[derive(Debug, Error)]
pub enum BackupError {
    #[error("目标目录已存在")]
    TargetExists,
    #[error("目标目录的上级目录不存在")]
    TargetParentMissing,
    #[error("已取消")]
    Cancelled,
    #[error("备份清单无效：{0}")]
    InvalidManifest(String),
    #[error("不支持的备份版本 {0}")]
    UnsupportedVersion(u64),
    #[error("备份中的数据库与清单不符")]
    DatabaseMismatch,
    #[error("附件缺失或内容与清单不符：{sha256}")]
    BlobMismatch { sha256: String },
    #[error("恢复后的计数与清单不符")]
    CountsMismatch,
    #[error("文件操作失败：{0}")]
    Io(#[from] io::Error),
    #[error("SQLite 失败：{0}")]
    Storage(#[from] rusqlite::Error),
    #[error("附件存储失败：{0}")]
    Resource(#[from] ResourceError),
    #[error("资料库失败：{0}")]
    Library(#[from] crate::LibraryError),
    #[error(transparent)]
    Publish(#[from] PublishError),
}

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    format: String,
    version: u64,
    created_time_ms: i64,
    counts: ManifestCounts,
    database: ManifestFile,
    blobs: Vec<ManifestFile>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ManifestFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file: Option<String>,
    sha256: String,
    size: u64,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct ManifestCounts {
    notes: u64,
    trashed_notes: u64,
    notebooks: u64,
    stacks: u64,
    tags: u64,
    note_tags: u64,
    resources: u64,
    blobs: u64,
}

impl From<LibraryCounts> for ManifestCounts {
    fn from(c: LibraryCounts) -> Self {
        Self {
            notes: c.notes,
            trashed_notes: c.trashed_notes,
            notebooks: c.notebooks,
            stacks: c.stacks,
            tags: c.tags,
            note_tags: c.note_tags,
            resources: c.resources,
            blobs: c.blobs,
        }
    }
}

/// A verified restore profile awaiting publication.
pub struct RestoredLibrary(TempDir);

impl super::import_commit::StagedLibrary for RestoredLibrary {
    fn profile_path(&self) -> &Path {
        self.0.path()
    }
    fn into_directory(self) -> TempDir {
        self.0
    }
}

fn check(cancel: &AtomicBool) -> Result<(), BackupError> {
    if cancel.load(Ordering::Relaxed) {
        Err(BackupError::Cancelled)
    } else {
        Ok(())
    }
}

fn free_target(target: &Path) -> Result<&Path, BackupError> {
    if fs::symlink_metadata(target).is_ok() {
        return Err(BackupError::TargetExists);
    }
    target
        .parent()
        .filter(|parent| parent.is_dir())
        .ok_or(BackupError::TargetParentMissing)
}

/// Copy `source` to a new file at `target`, returning (sha256, bytes).
fn copy_hashing_bounded(
    source: &mut impl Read,
    target: &Path,
    expected_size: u64,
    cancel: &AtomicBool,
) -> Result<(String, u64), BackupError> {
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        check(cancel)?;
        let limit = (expected_size - total)
            .saturating_add(1)
            .min(buffer.len() as u64) as usize;
        let count = source.read(&mut buffer[..limit])?;
        check(cancel)?;
        if count == 0 {
            break;
        }
        if count as u64 > expected_size - total {
            return Err(BackupError::InvalidManifest("文件超出声明大小".into()));
        }
        digest.update(&buffer[..count]);
        out.write_all(&buffer[..count])?;
        total += count as u64;
    }
    out.sync_all()?;
    Ok((format!("{:x}", digest.finalize()), total))
}

fn open_regular(path: &Path) -> io::Result<File> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a regular file",
        ));
    }
    File::open(path)
}

fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

/// Back up the library at `profile` into a new directory `destination`.
pub fn backup_library(
    profile: &Path,
    destination: &Path,
    cancel: &AtomicBool,
) -> Result<BackupReport, BackupError> {
    let parent = free_target(destination)?;
    check(cancel)?;
    let temp = tempfile::Builder::new()
        .prefix(".backup-")
        .tempdir_in(parent)?;
    let snapshot = temp.path().join(DATABASE);
    {
        let source = Connection::open(profile.join(DATABASE))?;
        source.execute(
            "VACUUM INTO ?1",
            [snapshot.to_str().ok_or(BackupError::TargetParentMissing)?],
        )?;
    }
    let counts = library_counts(temp.path())?;
    let rows: Vec<(String, i64)> =
        Connection::open_with_flags(&snapshot, OpenFlags::SQLITE_OPEN_READ_ONLY)?
            .prepare("SELECT sha256, size FROM resource_blobs ORDER BY sha256")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
    let blobs_dir = temp.path().join(BLOBS);
    fs::create_dir(&blobs_dir)?;
    let store = ResourceStore::new(profile)?;
    let mut blobs = Vec::with_capacity(rows.len());
    let mut bytes = 0_u64;
    for (sha256, size) in rows {
        check(cancel)?;
        let mismatch = || BackupError::BlobMismatch {
            sha256: sha256.clone(),
        };
        let hash = BlobHash::new(&sha256).map_err(|_| mismatch())?;
        let mut file = match store.open_verified(&hash) {
            Ok(file) => file,
            Err(ResourceError::CorruptBlob) => return Err(mismatch()),
            Err(ResourceError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Err(mismatch());
            }
            Err(error) => return Err(error.into()),
        };
        let expected = u64::try_from(size).map_err(|_| mismatch())?;
        let (copied, len) =
            copy_hashing_bounded(&mut file, &blobs_dir.join(&sha256), expected, cancel)?;
        if copied != sha256 || len != size as u64 {
            return Err(mismatch());
        }
        bytes += len;
        blobs.push(ManifestFile {
            file: None,
            sha256,
            size: len,
        });
    }
    sync_dir(&blobs_dir)?;
    let (database_sha, database_size) = {
        let mut file = File::open(&snapshot)?;
        file.sync_all()?;
        let mut digest = Sha256::new();
        let size = io::copy(&mut file, &mut digest)?;
        (format!("{:x}", digest.finalize()), size)
    };
    bytes += database_size;
    let blob_count = blobs.len();
    let manifest = Manifest {
        format: LIBRARY_BACKUP_FORMAT.into(),
        version: LIBRARY_BACKUP_VERSION,
        created_time_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or_default(),
        counts: counts.into(),
        database: ManifestFile {
            file: Some(DATABASE.into()),
            sha256: database_sha,
            size: database_size,
        },
        blobs,
    };
    {
        let mut file = File::create(temp.path().join(MANIFEST))?;
        serde_json::to_writer_pretty(&mut file, &manifest)
            .map_err(|error| BackupError::InvalidManifest(error.to_string()))?;
        file.sync_all()?;
    }
    sync_dir(temp.path())?;
    check(cancel)?;
    let kept = temp.keep();
    if let Err(error) = fs::rename(&kept, destination) {
        let _ = fs::remove_dir_all(&kept);
        return Err(if destination.exists() {
            BackupError::TargetExists
        } else {
            error.into()
        });
    }
    sync_dir(parent)?;
    Ok(BackupReport {
        counts,
        blobs: blob_count,
        bytes,
    })
}

fn read_manifest(backup: &Path) -> Result<Manifest, BackupError> {
    let file = open_regular(&backup.join(MANIFEST))
        .map_err(|error| BackupError::InvalidManifest(error.to_string()))?;
    let mut text = String::new();
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_string(&mut text)?;
    if text.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(BackupError::InvalidManifest("清单过大".into()));
    }
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|error| BackupError::InvalidManifest(error.to_string()))?;
    if value.get("format").and_then(|f| f.as_str()) != Some(LIBRARY_BACKUP_FORMAT) {
        return Err(BackupError::InvalidManifest(
            "不是 Joplin Lite 资料库备份".into(),
        ));
    }
    let version = value
        .get("version")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| BackupError::InvalidManifest("缺少版本".into()))?;
    if version != LIBRARY_BACKUP_VERSION {
        return Err(BackupError::UnsupportedVersion(version));
    }
    serde_json::from_value(value).map_err(|error| BackupError::InvalidManifest(error.to_string()))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Verify `backup` and assemble a restore profile next to `destination`.
fn prepare_restore(
    backup: &Path,
    parent: &Path,
    cancel: &AtomicBool,
) -> Result<(RestoredLibrary, ManifestCounts), BackupError> {
    let manifest = read_manifest(backup)?;
    let temp = tempfile::Builder::new()
        .prefix(".restore-")
        .tempdir_in(parent)?;
    let mut database =
        open_regular(&backup.join(DATABASE)).map_err(|_| BackupError::DatabaseMismatch)?;
    if database.metadata()?.len() != manifest.database.size {
        return Err(BackupError::DatabaseMismatch);
    }
    let (sha, size) = copy_hashing_bounded(
        &mut database,
        &temp.path().join(DATABASE),
        manifest.database.size,
        cancel,
    )?;
    if sha != manifest.database.sha256 || size != manifest.database.size {
        return Err(BackupError::DatabaseMismatch);
    }
    let blobs_dir = temp.path().join("resources").join("blobs");
    fs::create_dir_all(&blobs_dir)?;
    let mut listed = BTreeSet::new();
    for blob in &manifest.blobs {
        check(cancel)?;
        let mismatch = || BackupError::BlobMismatch {
            sha256: blob.sha256.clone(),
        };
        if !valid_sha256(&blob.sha256) || !listed.insert(blob.sha256.clone()) {
            return Err(mismatch());
        }
        let mut source =
            open_regular(&backup.join(BLOBS).join(&blob.sha256)).map_err(|_| mismatch())?;
        if source.metadata()?.len() != blob.size {
            return Err(mismatch());
        }
        let (sha, size) = copy_hashing_bounded(
            &mut source,
            &blobs_dir.join(&blob.sha256),
            blob.size,
            cancel,
        )?;
        if sha != blob.sha256 || size != blob.size {
            return Err(mismatch());
        }
    }
    sync_dir(&blobs_dir)?;
    {
        let db = Connection::open(temp.path().join(DATABASE))?;
        let integrity: String = db.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(BackupError::DatabaseMismatch);
        }
        let referenced: BTreeSet<String> = db
            .prepare("SELECT sha256 FROM resource_blobs")?
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        if referenced != listed {
            return Err(BackupError::DatabaseMismatch);
        }
    }
    // Opening runs schema migrations for older snapshots. A restored library
    // is a new client: new identity, no replay of the old device's queue or
    // pull position (after migration, so older snapshots have the tables).
    LibraryRepository::open(temp.path().join(DATABASE))?.sync_reset_identity()?;
    let counts = ManifestCounts::from(library_counts(temp.path())?);
    if counts != manifest.counts {
        return Err(BackupError::CountsMismatch);
    }
    Ok((RestoredLibrary(temp), counts))
}

/// Restore `backup` into a new library directory `destination`.
pub fn restore_library_backup(
    backup: &Path,
    destination: &Path,
    cancel: &AtomicBool,
) -> Result<PublishedLibrary, BackupError> {
    let parent: PathBuf = free_target(destination)?.to_path_buf();
    check(cancel)?;
    let (restored, _) = prepare_restore(backup, &parent, cancel)?;
    Ok(super::publish_staged_library(
        restored,
        destination,
        cancel,
    )?)
}

#[cfg(test)]
mod copy_tests {
    use super::*;

    #[test]
    fn bounded_copy_stops_after_declared_size_plus_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut input = io::Cursor::new(vec![7_u8; 1024 * 1024]);
        let error = copy_hashing_bounded(
            &mut input,
            &dir.path().join("out"),
            10,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(matches!(error, BackupError::InvalidManifest(_)));
        assert_eq!(input.position(), 11);
        assert!(fs::metadata(dir.path().join("out")).unwrap().len() <= 10);
    }

    #[test]
    fn bounded_copy_observes_cancel_during_one_file() {
        struct CancellingReader<'a>(&'a AtomicBool);
        impl Read for CancellingReader<'_> {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                self.0.store(true, Ordering::Relaxed);
                out[0] = 1;
                Ok(1)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let error = copy_hashing_bounded(
            &mut CancellingReader(&cancel),
            &dir.path().join("out"),
            10,
            &cancel,
        )
        .unwrap_err();
        assert!(matches!(error, BackupError::Cancelled));
    }
}
