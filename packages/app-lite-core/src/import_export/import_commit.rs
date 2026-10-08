//! Publish a verified import staging profile as a new, separate library.
//!
//! Independent design (not from Evernote, whose importer writes each note
//! straight into the live account): the staged profile is re-verified, made
//! durable, then renamed to a destination that must not exist. The active
//! profile is never opened, merged into or replaced here; switching to the
//! new library is the caller's decision after this returns.

use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use rusqlite::{Connection, OpenFlags};
use tempfile::TempDir;
use thiserror::Error;

use super::{EnexStagedProfile, JexStagedProfile};
use crate::{BlobHash, ResourceError, ResourceStore};

/// A staging profile that owns its temporary directory.
pub trait StagedLibrary: sealed::Sealed {
    fn profile_path(&self) -> &Path;
    #[doc(hidden)]
    fn into_directory(self) -> TempDir;
    /// Restore keeps its existing lifecycle; only import producers carry this.
    #[doc(hidden)]
    fn take_import_lease(&mut self) -> Option<File> { None }
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::EnexStagedProfile {}
    impl Sealed for super::JexStagedProfile {}
    impl Sealed for super::super::RestoredLibrary {}
}

impl StagedLibrary for EnexStagedProfile {
    fn take_import_lease(&mut self) -> Option<File> { self.take_import_lease() }
    fn profile_path(&self) -> &Path {
        EnexStagedProfile::profile_path(self)
    }
    fn into_directory(self) -> TempDir {
        self.into_directory()
    }
}

impl StagedLibrary for JexStagedProfile {
    fn take_import_lease(&mut self) -> Option<File> { self.take_import_lease() }
    fn profile_path(&self) -> &Path {
        JexStagedProfile::profile_path(self)
    }
    fn into_directory(self) -> TempDir {
        self.into_directory()
    }
}

/// Entity counts of one library profile, read without loading bodies/blobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LibraryCounts {
    pub notes: u64,
    pub trashed_notes: u64,
    pub notebooks: u64,
    pub stacks: u64,
    pub tags: u64,
    pub note_tags: u64,
    pub resources: u64,
    pub blobs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedLibrary {
    pub path: PathBuf,
    pub counts: LibraryCounts,
}

#[derive(Debug, Error)]
pub enum PublishError {
    #[error("目标资料库目录已存在")]
    DestinationExists,
    #[error("目标资料库的上级目录不存在")]
    DestinationParentMissing,
    #[error("导入已取消")]
    Cancelled,
    #[error("暂存资料库校验失败：{0}")]
    Integrity(String),
    #[error("附件内容与记录不符：{sha256}")]
    BlobMismatch { sha256: String },
    #[error("发布后计数不一致")]
    CountsChanged,
    #[error("文件操作失败：{0}")]
    Io(#[from] io::Error),
    #[error("SQLite 失败：{0}")]
    Storage(#[from] rusqlite::Error),
    #[error("附件存储失败：{0}")]
    Resource(#[from] ResourceError),
}

pub fn publish_staged_library<S: StagedLibrary>(
    staged: S,
    destination: &Path,
    cancel: &AtomicBool,
) -> Result<PublishedLibrary, PublishError> {
    publish_staged_library_with_hook(staged, destination, cancel, || Ok(()))
}

/// `before_rename` runs after verification and fsync, immediately before the
/// publishing rename; it exists so callers and tests can inject failures.
pub fn publish_staged_library_with_hook<S: StagedLibrary>(
    mut staged: S,
    destination: &Path,
    cancel: &AtomicBool,
    before_rename: impl FnOnce() -> io::Result<()>,
) -> Result<PublishedLibrary, PublishError> {
    // Dropping `directory` on any early return removes the staging copy.
    let import_lease = staged.take_import_lease();
    let directory = staged.into_directory();
    let source = directory.path();
    if fs::symlink_metadata(destination).is_ok() {
        return Err(PublishError::DestinationExists);
    }
    let parent = destination
        .parent()
        .filter(|parent| parent.is_dir())
        .ok_or(PublishError::DestinationParentMissing)?;
    if cancel.load(Ordering::Relaxed) {
        return Err(PublishError::Cancelled);
    }
    let counts = verify_profile(source, cancel)?;
    sync_tree(source, cancel)?;
    if cancel.load(Ordering::Relaxed) {
        return Err(PublishError::Cancelled);
    }
    before_rename()?;
    check_cancel(cancel)?;
    let kept = directory.keep();
    // rename(2) refuses to replace a non-empty directory, and the existence
    // check above covers files; a racing creator makes this fail cleanly.
    if let Err(error) = fs::rename(&kept, destination) {
        let _ = fs::remove_dir_all(&kept);
        return Err(if destination.exists() {
            PublishError::DestinationExists
        } else {
            PublishError::Io(error)
        });
    }
    File::open(parent)?.sync_all()?;
    if library_counts(destination)? != counts {
        return Err(PublishError::CountsChanged);
    }
    if import_lease.is_some() { super::import_staging::finish(destination); }
    drop(import_lease);
    Ok(PublishedLibrary {
        path: destination.to_path_buf(),
        counts,
    })
}

/// Read-only entity counts for a profile directory.
pub fn library_counts(profile: &Path) -> Result<LibraryCounts, PublishError> {
    let db = open_read_only(profile)?;
    count(&db)
}

fn open_read_only(profile: &Path) -> Result<Connection, PublishError> {
    Ok(Connection::open_with_flags(
        profile.join("library.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?)
}

fn count(db: &Connection) -> Result<LibraryCounts, PublishError> {
    let one = |sql: &str| -> Result<u64, PublishError> {
        Ok(db.query_row(sql, [], |row| row.get::<_, i64>(0))? as u64)
    };
    Ok(LibraryCounts {
        notes: one("SELECT COUNT(*) FROM notes WHERE deleted_time = 0")?,
        trashed_notes: one("SELECT COUNT(*) FROM notes WHERE deleted_time != 0")?,
        notebooks: one("SELECT COUNT(*) FROM notebooks WHERE deleted_time = 0")?,
        stacks: one("SELECT COUNT(*) FROM stacks WHERE deleted_time = 0")?,
        tags: one("SELECT COUNT(*) FROM tags WHERE deleted_time = 0")?,
        note_tags: one("SELECT COUNT(*) FROM note_tags")?,
        resources: one("SELECT COUNT(*) FROM resources WHERE deleted_time = 0")?,
        blobs: one("SELECT COUNT(*) FROM resource_blobs")?,
    })
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), PublishError> {
    if cancel.load(Ordering::Relaxed) {
        Err(PublishError::Cancelled)
    } else {
        Ok(())
    }
}

fn verify_profile(profile: &Path, cancel: &AtomicBool) -> Result<LibraryCounts, PublishError> {
    check_cancel(cancel)?;
    let db = open_read_only(profile)?;
    let integrity: String = db.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(PublishError::Integrity(integrity));
    }
    let violations: i64 =
        db.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    if violations != 0 {
        return Err(PublishError::Integrity(format!("{violations} 条外键违例")));
    }
    let blobs: Vec<(String, i64)> = db
        .prepare("SELECT sha256, size FROM resource_blobs ORDER BY sha256")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let store = ResourceStore::new(profile)?;
    for (sha256, size) in blobs {
        check_cancel(cancel)?;
        let mismatch = || PublishError::BlobMismatch {
            sha256: sha256.clone(),
        };
        let hash = BlobHash::new(&sha256).map_err(|_| mismatch())?;
        let file = match store.open_verified(&hash) {
            Ok(file) => file,
            Err(ResourceError::CorruptBlob) => return Err(mismatch()),
            Err(ResourceError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Err(mismatch());
            }
            Err(error) => return Err(error.into()),
        };
        if file.metadata()?.len() != size as u64 {
            return Err(mismatch());
        }
    }
    count(&db)
}

/// fsync every file and directory so the rename publishes durable content.
fn sync_tree(root: &Path, cancel: &AtomicBool) -> Result<(), PublishError> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        check_cancel(cancel)?;
        for entry in fs::read_dir(&dir)? {
            check_cancel(cancel)?;
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                File::open(entry.path())?.sync_all()?;
            }
        }
        File::open(&dir)?.sync_all()?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportLibraryOutcome {
    pub library: PublishedLibrary,
    /// Human-readable descriptions of notes imported as plain text.
    pub degraded: Vec<String>,
}

#[derive(Debug, Error)]
pub enum ImportLibraryError {
    #[error("只支持 .enex（Evernote）或 .jex（Joplin）文件")]
    UnsupportedFormat,
    #[error("ENEX 暂存失败：{0}")]
    Enex(#[from] super::EnexStageError),
    #[error("JEX 暂存失败：{0:?}")]
    Jex(super::JexStageError),
    #[error(transparent)]
    Publish(#[from] PublishError),
    #[error("文件操作失败：{0}")]
    Io(#[from] io::Error),
}

/// Stage `source` (.enex or .jex, chosen by extension) under
/// `imports_dir/.staging` and publish it as a new directory in `imports_dir`
/// named after the source file. The source is only read.
pub fn import_library_file(
    source: &Path,
    imports_dir: &Path,
    cancel: &std::sync::Arc<AtomicBool>,
) -> Result<ImportLibraryOutcome, ImportLibraryError> {
    check_cancel(cancel)?;
    let extension = source
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    let is_enex = match extension.as_deref() {
        Some("enex") => true,
        Some("jex") => false,
        _ => return Err(ImportLibraryError::UnsupportedFormat),
    };
    let staging = imports_dir.join(".staging");
    fs::create_dir_all(&staging)?;
    super::cleanup_abandoned_import_staging(&staging)?;
    let destination = unique_library_destination(imports_dir, source)?;
    if is_enex {
        let staged = super::stage_enex_file_with_cancel(source, &staging, cancel.clone())?;
        let degraded = staged
            .report()
            .degraded_notes
            .iter()
            .map(|note| format!("第 {} 篇：{}", note.note_ordinal, note.reason))
            .collect();
        let library = publish_staged_library(staged, &destination, cancel)?;
        Ok(ImportLibraryOutcome { library, degraded })
    } else {
        let staged = super::stage_jex_file_with_cancel(source, &staging, cancel.clone()).map_err(
            |error| match error {
                super::JexStageError::Cancelled
                | super::JexStageError::Prepare(super::JexPrepareError::Cancelled) => {
                    ImportLibraryError::Publish(PublishError::Cancelled)
                }
                error => ImportLibraryError::Jex(error),
            },
        )?;
        let degraded = staged
            .report()
            .degraded_notes
            .iter()
            .map(|note| format!("{}：{}", note.source_id, note.reason))
            .collect();
        let library = publish_staged_library(staged, &destination, cancel)?;
        Ok(ImportLibraryOutcome { library, degraded })
    }
}

/// A not-yet-existing directory in `imports_dir` named after `source`'s
/// stem plus a timestamp (and a counter if needed).
pub fn unique_library_destination(imports_dir: &Path, source: &Path) -> io::Result<PathBuf> {
    let stem: String = source
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|ch| {
            if ch == '/' || ch == '\\' || ch.is_control() {
                '_'
            } else {
                ch
            }
        })
        .take(64)
        .collect();
    let stem = if stem.trim().is_empty() || stem.starts_with('.') {
        format!("导入{stem}")
    } else {
        stem
    };
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    for attempt in 0..1000_u32 {
        let name = if attempt == 0 {
            format!("{stem}-{stamp}")
        } else {
            format!("{stem}-{stamp}-{attempt}")
        };
        let candidate = imports_dir.join(name);
        if fs::symlink_metadata(&candidate).is_err() {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free import destination name",
    ))
}
