//! Qualification-only second pass. This type cannot be passed to the strict
//! JEX stage: it owns only metadata SQLite and bounded resource signatures.

use super::{
    JexPhysicalResourceFile, JexPrepareError, JexRawSourceItem, JexScanReport,
    JexScannedMetadataItem, MAX_JEX_ARCHIVE_ENTRIES, MAX_JEX_RESOURCE_BYTES, STREAM_BUFFER_BYTES,
    checked_archive_path, is_resource_path, parse_item, physical_resource_id,
    raw_archive_path_for_error, read_item, root_item_id, scan_jex_archive, tar_entry_kind,
    valid_joplin_id,
};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tar::Archive;
use tempfile::{Builder, TempDir};

const DATABASE_NAME: &str = "jex-qualification.sqlite";

#[derive(Clone, Copy)]
struct Prefix {
    bytes: [u8; 8],
    len: usize,
}

/// Non-stage authority. Physical resources are streamed twice and never
/// copied to the temporary child; only raw metadata lives in SQLite.
pub(super) struct JexQualificationSource {
    directory: TempDir,
    report: JexScanReport,
    metadata_index: BTreeMap<String, usize>,
    prefixes: BTreeMap<String, Prefix>,
}

impl JexQualificationSource {
    pub(super) fn report(&self) -> &JexScanReport {
        &self.report
    }

    fn database_path(&self) -> PathBuf {
        self.directory.path().join(DATABASE_NAME)
    }

    pub(super) fn raw_item(
        &self,
        source_id: &str,
    ) -> Result<Option<JexRawSourceItem>, JexPrepareError> {
        if !valid_joplin_id(source_id) {
            return Ok(None);
        }
        let Some(expected) = self
            .metadata_index
            .get(&source_id.to_ascii_lowercase())
            .and_then(|index| self.report.metadata_items.get(*index))
        else {
            return Ok(None);
        };
        let db = Connection::open(self.database_path())?;
        let item = db.query_row(
            "SELECT archive_path,source_id,item_type,byte_count,raw_sha256,canonical_note_body_sha256,raw_bytes
             FROM jex_qualification_items WHERE source_id=?1",[source_id],|row| {
                Ok(JexRawSourceItem {
                    archive_path: row.get(0)?,source_id: row.get(1)?,item_type: row.get(2)?,
                    byte_count: row.get::<_,i64>(3)? as u64,raw_sha256: row.get(4)?,
                    canonical_note_body_sha256: row.get(5)?,raw_bytes: row.get(6)?,
                })
            }).optional()?;
        let Some(item) = item else {
            return Err(JexPrepareError::Verification(format!(
                "metadata {source_id} disappeared"
            )));
        };
        if item.archive_path != expected.archive_path
            || item.source_id != expected.source_id
            || item.item_type != expected.item_type
            || item.byte_count != expected.byte_count
            || item.raw_sha256 != expected.raw_sha256
            || item.canonical_note_body_sha256 != expected.canonical_note_body_sha256
            || item.raw_bytes.len() as u64 != expected.byte_count
            || format!("{:x}", Sha256::digest(&item.raw_bytes)) != expected.raw_sha256
        {
            return Err(JexPrepareError::Verification(format!(
                "metadata {source_id} changed"
            )));
        }
        Ok(Some(item))
    }

    pub(super) fn verified_prefix(&self, source_id: &str) -> Option<&[u8]> {
        self.prefixes
            .get(&source_id.to_ascii_lowercase())
            .map(|prefix| &prefix.bytes[..prefix.len])
    }
}

fn check_cancel(cancel: &Option<Arc<AtomicBool>>) -> Result<(), JexPrepareError> {
    if cancel
        .as_ref()
        .is_some_and(|flag| flag.load(Ordering::Relaxed))
    {
        Err(JexPrepareError::Cancelled)
    } else {
        Ok(())
    }
}

fn changed(entity: impl Into<String>) -> JexPrepareError {
    JexPrepareError::SourceChanged {
        entity: entity.into(),
    }
}

pub(super) fn prepare_jex_qualification_source(
    archive: &Path,
    staging_parent: &Path,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<JexQualificationSource, JexPrepareError> {
    prepare_inner(archive, staging_parent, cancel, || {})
}

fn prepare_inner(
    archive: &Path,
    staging_parent: &Path,
    cancel: Option<Arc<AtomicBool>>,
    after_scan: impl FnOnce(),
) -> Result<JexQualificationSource, JexPrepareError> {
    let parent =
        fs::canonicalize(staging_parent).map_err(|_| JexPrepareError::InvalidStagingParent)?;
    if !parent.is_dir()
        || parent.join("library.sqlite").exists()
        || parent.join("resources").exists()
    {
        return Err(JexPrepareError::InvalidStagingParent);
    }
    check_cancel(&cancel)?;
    let report = scan_jex_archive(archive)?;
    // Repeated identities cannot be represented by the metadata primary key
    // and are integrity failures, not qualification findings to guess around.
    if !report.duplicate_item_ids.is_empty() || !report.duplicate_archive_paths.is_empty() {
        return Err(JexPrepareError::PreflightBlocked {
            report: Box::new(report),
        });
    }
    check_cancel(&cancel)?;
    after_scan();
    let directory = Builder::new()
        .prefix("jex-qualification-")
        .tempdir_in(parent)?;
    let mut db = Connection::open(directory.path().join(DATABASE_NAME))?;
    db.execute_batch(
        "PRAGMA foreign_keys=ON;
        CREATE TABLE jex_qualification_items (
            source_id TEXT PRIMARY KEY NOT NULL COLLATE NOCASE,
            archive_path TEXT NOT NULL UNIQUE,item_type INTEGER NOT NULL,
            byte_count INTEGER NOT NULL,raw_sha256 TEXT NOT NULL,
            canonical_note_body_sha256 TEXT,raw_bytes BLOB NOT NULL); ",
    )?;
    let prefixes = ingest_verified_second_pass(archive, &mut db, &report, &cancel)?;
    drop(db);
    check_cancel(&cancel)?;
    let metadata_index = report
        .metadata_items
        .iter()
        .enumerate()
        .map(|(index, item)| (item.source_id.to_ascii_lowercase(), index))
        .collect();
    Ok(JexQualificationSource {
        directory,
        report,
        metadata_index,
        prefixes,
    })
}

fn ingest_verified_second_pass(
    source: &Path,
    db: &mut Connection,
    expected: &JexScanReport,
    cancel: &Option<Arc<AtomicBool>>,
) -> Result<BTreeMap<String, Prefix>, JexPrepareError> {
    let file = File::open(source).map_err(|error| changed(format!("archive open: {error}")))?;
    let mut archive = Archive::new(file);
    let entries = archive
        .entries()
        .map_err(|error| changed(format!("tar entries: {error}")))?;
    let mut paths = BTreeSet::new();
    let expected_items = expected
        .metadata_items
        .iter()
        .map(|item| (item.archive_path.as_str(), item))
        .collect::<BTreeMap<_, _>>();
    let expected_resources = expected
        .physical_resource_files
        .iter()
        .map(|item| (item.archive_path.as_str(), item))
        .collect::<BTreeMap<_, _>>();
    let mut item_count = 0usize;
    let mut resource_count = 0usize;
    let mut entry_count = 0usize;
    let mut prefixes = BTreeMap::new();
    let tx = db.transaction()?;
    for entry in entries.raw(true) {
        check_cancel(cancel)?;
        if entry_count >= MAX_JEX_ARCHIVE_ENTRIES {
            return Err(changed("archive entry count exceeds first scan"));
        }
        entry_count += 1;
        let mut entry =
            entry.map_err(|error| changed(format!("tar entry {entry_count}: {error}")))?;
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            return Err(changed(format!(
                "unsafe {} entry {}",
                tar_entry_kind(kind),
                raw_archive_path_for_error(&entry)
            )));
        }
        let path = checked_archive_path(&entry).map_err(|error| changed(error.to_string()))?;
        if kind.is_dir() {
            if path != "resources" && path != "resources/" {
                return Err(changed(path));
            }
            continue;
        }
        if !paths.insert(path.clone()) {
            return Err(changed(path));
        }
        if let Some(id) = root_item_id(&path) {
            let raw = read_item(&mut entry, &path).map_err(|error| changed(error.to_string()))?;
            let parsed = parse_item(&path, &raw).map_err(|error| changed(error.to_string()))?;
            let evidence = JexScannedMetadataItem {
                archive_path: path.clone(),
                source_id: parsed.id.clone(),
                item_type: parsed.item_type,
                byte_count: raw.len() as u64,
                raw_sha256: format!("{:x}", Sha256::digest(raw.as_bytes())),
                canonical_note_body_sha256: (parsed.item_type == 1)
                    .then(|| format!("{:x}", Sha256::digest(parsed.note_body.as_bytes()))),
            };
            if !parsed.id.eq_ignore_ascii_case(&id)
                || expected_items.get(path.as_str()).copied() != Some(&evidence)
            {
                return Err(changed(path));
            }
            tx.execute("INSERT INTO jex_qualification_items
                (source_id,archive_path,item_type,byte_count,raw_sha256,canonical_note_body_sha256,raw_bytes)
                VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![parsed.id,path,parsed.item_type,evidence.byte_count as i64,
                    evidence.raw_sha256,evidence.canonical_note_body_sha256,raw.as_bytes()])?;
            item_count += 1;
        } else if is_resource_path(&path) {
            let id = physical_resource_id(&path).map_err(|error| changed(error.to_string()))?;
            let Some(first) = expected_resources.get(path.as_str()).copied() else {
                return Err(changed(path));
            };
            if first.source_id != id {
                return Err(changed(path));
            }
            let prefix = stream_verified_resource(&mut entry, first, cancel)?;
            if prefixes.insert(id.to_ascii_lowercase(), prefix).is_some() {
                return Err(changed(path));
            }
            resource_count += 1;
        } else {
            return Err(changed(path));
        }
    }
    if entry_count != expected.archive_entry_count
        || item_count != expected.metadata_items.len()
        || resource_count != expected.physical_resource_files.len()
    {
        return Err(changed("final item/resource/entry counts"));
    }
    tx.commit()?;
    let integrity: String = db.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(JexPrepareError::Verification(
            "qualification SQLite integrity check failed".into(),
        ));
    }
    Ok(prefixes)
}

fn stream_verified_resource(
    entry: &mut impl Read,
    expected: &JexPhysicalResourceFile,
    cancel: &Option<Arc<AtomicBool>>,
) -> Result<Prefix, JexPrepareError> {
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut prefix = Prefix {
        bytes: [0; 8],
        len: 0,
    };
    let mut buffer = [0u8; STREAM_BUFFER_BYTES];
    loop {
        check_cancel(cancel)?;
        let n = entry
            .read(&mut buffer)
            .map_err(|error| changed(format!("{}: {error}", expected.archive_path)))?;
        if n == 0 {
            break;
        }
        size = size
            .checked_add(n as u64)
            .ok_or_else(|| changed(expected.archive_path.clone()))?;
        if size > MAX_JEX_RESOURCE_BYTES {
            return Err(changed(expected.archive_path.clone()));
        }
        if prefix.len < 8 {
            let count = (8 - prefix.len).min(n);
            prefix.bytes[prefix.len..prefix.len + count].copy_from_slice(&buffer[..count]);
            prefix.len += count;
        }
        hash.update(&buffer[..n]);
    }
    if size != expected.byte_count || format!("{:x}", hash.finalize()) != expected.sha256 {
        return Err(changed(expected.archive_path.clone()));
    }
    Ok(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tar::{Builder as TarBuilder, Header};
    use tempfile::tempdir;

    const NOTE: &str = "11111111111111111111111111111111";
    const RESOURCE: &str = "22222222222222222222222222222222";
    const MISSING: &str = "33333333333333333333333333333333";

    fn append(tar: &mut TarBuilder<File>, path: &str, bytes: &[u8]) {
        let mut header = Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, path, Cursor::new(bytes))
            .unwrap();
    }

    fn write_archive(path: &Path, body: &str, resource: &[u8]) {
        let mut tar = TarBuilder::new(File::create(path).unwrap());
        append(
            &mut tar,
            &format!("{NOTE}.md"),
            format!("标题\n\n{body}\n\nid: {NOTE}\ntype_: 1\n").as_bytes(),
        );
        append(&mut tar,&format!("{RESOURCE}.md"),format!("附件.bin\n\nid: {RESOURCE}\ntype_: 4\nmime: application/octet-stream\nfile_extension: bin\n").as_bytes());
        append(&mut tar, &format!("resources/{RESOURCE}.bin"), resource);
        tar.finish().unwrap();
    }

    fn only_sentinel(parent: &Path) {
        let names = fs::read_dir(parent)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, vec![std::ffi::OsString::from("sentinel.bin")]);
        assert_eq!(fs::read(parent.join("sentinel.bin")).unwrap(), b"keep");
    }

    struct CancelAfterFirstRead {
        cancel: Arc<AtomicBool>,
        read_once: bool,
    }
    impl Read for CancelAfterFirstRead {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.read_once {
                return Ok(0);
            }
            self.read_once = true;
            buffer[..8].copy_from_slice(b"12345678");
            self.cancel.store(true, Ordering::Relaxed);
            Ok(8)
        }
    }

    #[test]
    fn cancellation_during_resource_stream_remains_typed_cancelled() {
        // Mutation caught: converting cancellation to io::Interrupted and
        // then misreporting it as a source-changed error.
        let cancel = Arc::new(AtomicBool::new(false));
        let mut reader = CancelAfterFirstRead {
            cancel: cancel.clone(),
            read_once: false,
        };
        let expected = JexPhysicalResourceFile {
            source_id: "11111111111111111111111111111111".into(),
            archive_path: "resources/11111111111111111111111111111111.bin".into(),
            byte_count: 8,
            sha256: String::new(),
        };
        let result = stream_verified_resource(&mut reader, &expected, &Some(cancel));
        assert!(matches!(result, Err(JexPrepareError::Cancelled)));
    }

    #[test]
    fn nonclean_archive_changed_after_scan_never_returns_a_qualification_source() {
        // Mutation caught: trusting the first scan digest when raw metadata
        // or a resource changes before the independent second pass.
        let source_dir = tempdir().unwrap();
        let archive = source_dir.path().join("source.jex");
        let body = format!("[缺失](:/{MISSING})");
        write_archive(&archive, &body, b"original");
        let parent = tempdir().unwrap();
        fs::write(parent.path().join("sentinel.bin"), b"keep").unwrap();
        let changed_body = format!("[缺失](:/{MISSING}) 改文");
        let result = prepare_inner(&archive, parent.path(), None, || {
            write_archive(&archive, &changed_body, b"original");
        });
        assert!(matches!(result, Err(JexPrepareError::SourceChanged { .. })));
        only_sentinel(parent.path());

        write_archive(&archive, &body, b"original");
        let result = prepare_inner(&archive, parent.path(), None, || {
            write_archive(&archive, &body, b"changed!");
        });
        assert!(matches!(result, Err(JexPrepareError::SourceChanged { .. })));
        only_sentinel(parent.path());
    }

    #[test]
    fn cancelled_after_scan_cleans_owned_sqlite_without_touching_sibling() {
        let source_dir = tempdir().unwrap();
        let archive = source_dir.path().join("source.jex");
        write_archive(&archive, &format!("[缺失](:/{MISSING})"), b"original");
        let parent = tempdir().unwrap();
        fs::write(parent.path().join("sentinel.bin"), b"keep").unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let result = prepare_inner(&archive, parent.path(), Some(cancel.clone()), || {
            cancel.store(true, Ordering::Relaxed);
        });
        assert!(matches!(result, Err(JexPrepareError::Cancelled)));
        only_sentinel(parent.path());
    }
}
