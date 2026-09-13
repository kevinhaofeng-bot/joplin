//! Isolated, verified source spool for a clean JEX archive. This is not a
//! library profile and deliberately has no migration or repository writes.

use super::{
    JexPhysicalResourceFile, JexScanError, JexScanReport, JexScannedMetadataItem,
    MAX_JEX_ARCHIVE_ENTRIES, MAX_JEX_ITEM_BYTES, MAX_JEX_RESOURCE_BYTES, STREAM_BUFFER_BYTES,
    checked_archive_path, parse_item, physical_resource_id, raw_archive_path_for_error, read_item,
    root_item_id, scan_jex_archive, tar_entry_kind, valid_joplin_id,
};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tar::Archive;
use tempfile::{Builder, TempDir};
use thiserror::Error;

const SPOOL_DATABASE: &str = "jex-source.sqlite";

#[derive(Debug, Error)]
pub enum JexPrepareError {
    #[error("JEX preflight failed: {0}")]
    Scan(#[from] JexScanError),
    #[error("JEX preflight has compatibility or integrity blockers")]
    PreflightBlocked { report: Box<JexScanReport> },
    #[error("staging parent must be an existing non-profile directory")]
    InvalidStagingParent,
    #[error("JEX source changed between preflight and spool at {entity}")]
    SourceChanged { entity: String },
    #[error("source spool I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("source spool SQLite failed: {0}")]
    Storage(#[from] rusqlite::Error),
    #[error("source spool verification failed: {0}")]
    Verification(String),
    #[error("source spool cancelled")]
    Cancelled,
}

/// One individually bounded raw source item recovered from the reopened spool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JexRawSourceItem {
    pub archive_path: String,
    pub source_id: String,
    pub item_type: i64,
    pub byte_count: u64,
    pub raw_sha256: String,
    pub canonical_note_body_sha256: Option<String>,
    pub raw_bytes: Vec<u8>,
}

/// Owns only a newly generated child of the requested staging parent. Its
/// `jex-source.sqlite` schema is source-only; no `library.sqlite` exists.
pub struct JexPreparedSource {
    directory: TempDir,
    report: JexScanReport,
    metadata_index: BTreeMap<String, usize>,
    resource_index: BTreeMap<String, usize>,
}

impl JexPreparedSource {
    pub fn staging_path(&self) -> &Path {
        self.directory.path()
    }

    pub fn spool_database_path(&self) -> PathBuf {
        self.directory.path().join(SPOOL_DATABASE)
    }

    pub fn report(&self) -> &JexScanReport {
        &self.report
    }

    /// Read one raw item, never the full archive, with the preflight digest
    /// rechecked before returning bytes to a later importer.
    pub fn raw_item(&self, source_id: &str) -> Result<Option<JexRawSourceItem>, JexPrepareError> {
        if !valid_joplin_id(source_id) {
            return Ok(None);
        }
        let db = Connection::open(self.spool_database_path())?;
        let size: Option<i64> = db
            .query_row(
                "SELECT byte_count FROM jex_source_items WHERE source_id=?1",
                [source_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(size) = size else { return Ok(None) };
        if size < 0 || size as u64 > MAX_JEX_ITEM_BYTES {
            return Err(JexPrepareError::Verification(
                "raw item size is out of bounds".into(),
            ));
        }
        let item = db.query_row(
            "SELECT archive_path,source_id,item_type,byte_count,raw_sha256,canonical_note_body_sha256,raw_bytes FROM jex_source_items WHERE source_id=?1",
            [source_id],
            |row| Ok(JexRawSourceItem {
                archive_path: row.get(0)?, source_id: row.get(1)?, item_type: row.get(2)?,
                byte_count: row.get::<_, i64>(3)? as u64, raw_sha256: row.get(4)?,
                canonical_note_body_sha256: row.get(5)?, raw_bytes: row.get(6)?,
            }),
        )?;
        let expected = self
            .metadata_index
            .get(&source_id.to_ascii_lowercase())
            .and_then(|index| self.report.metadata_items.get(*index));
        if expected != Some(&metadata_of(&item))
            || item.raw_bytes.len() as u64 != item.byte_count
            || format!("{:x}", Sha256::digest(&item.raw_bytes)) != item.raw_sha256
        {
            return Err(JexPrepareError::Verification(format!(
                "raw source item {source_id} changed"
            )));
        }
        Ok(Some(item))
    }

    /// Open a specific physical resource at offset zero after independently
    /// verifying its source ID, path, size, and full file SHA-256.
    pub fn open_verified_resource(
        &self,
        source_id: &str,
    ) -> Result<Option<(JexPhysicalResourceFile, File)>, JexPrepareError> {
        if !valid_joplin_id(source_id) {
            return Ok(None);
        }
        let Some(expected) = self
            .resource_index
            .get(&source_id.to_ascii_lowercase())
            .and_then(|index| self.report.physical_resource_files.get(*index))
        else {
            return Ok(None);
        };
        let mut file = File::open(resource_spool_path(
            self.directory.path(),
            &expected.source_id,
        ))?;
        verify_resource_file(&mut file, expected)?;
        file.seek(SeekFrom::Start(0))?;
        Ok(Some((expected.clone(), file)))
    }
}

fn metadata_of(item: &JexRawSourceItem) -> JexScannedMetadataItem {
    JexScannedMetadataItem {
        archive_path: item.archive_path.clone(),
        source_id: item.source_id.clone(),
        item_type: item.item_type,
        byte_count: item.byte_count,
        raw_sha256: item.raw_sha256.clone(),
        canonical_note_body_sha256: item.canonical_note_body_sha256.clone(),
    }
}

pub fn prepare_jex_source_archive(
    source: impl AsRef<Path>,
    staging_parent: impl AsRef<Path>,
) -> Result<JexPreparedSource, JexPrepareError> {
    prepare_inner(source.as_ref(), staging_parent.as_ref(), None, || {})
}

pub fn prepare_jex_source_archive_with_cancel(
    source: impl AsRef<Path>,
    staging_parent: impl AsRef<Path>,
    cancel: Arc<AtomicBool>,
) -> Result<JexPreparedSource, JexPrepareError> {
    prepare_inner(
        source.as_ref(),
        staging_parent.as_ref(),
        Some(cancel),
        || {},
    )
}

fn prepare_inner(
    source: &Path,
    staging_parent: &Path,
    cancel: Option<Arc<AtomicBool>>,
    after_preflight: impl FnOnce(),
) -> Result<JexPreparedSource, JexPrepareError> {
    let parent =
        fs::canonicalize(staging_parent).map_err(|_| JexPrepareError::InvalidStagingParent)?;
    if !parent.is_dir()
        || parent.join("library.sqlite").exists()
        || parent.join("resources").exists()
    {
        return Err(JexPrepareError::InvalidStagingParent);
    }
    check_cancel(&cancel)?;
    let report = scan_jex_archive(source)?;
    if !report.is_clean() {
        return Err(JexPrepareError::PreflightBlocked {
            report: Box::new(report),
        });
    }
    check_cancel(&cancel)?;
    after_preflight();
    let directory = Builder::new().prefix("jex-source-").tempdir_in(parent)?;
    fs::create_dir(directory.path().join("resources"))?;
    let database = directory.path().join(SPOOL_DATABASE);
    let mut db = Connection::open(&database)?;
    db.execute_batch(
        "PRAGMA foreign_keys=ON;
        CREATE TABLE jex_source_items (
            source_id TEXT PRIMARY KEY NOT NULL COLLATE NOCASE, archive_path TEXT NOT NULL UNIQUE,
            item_type INTEGER NOT NULL, byte_count INTEGER NOT NULL,
            raw_sha256 TEXT NOT NULL, canonical_note_body_sha256 TEXT,
            raw_bytes BLOB NOT NULL);
        CREATE TABLE jex_source_resources (
            source_id TEXT PRIMARY KEY NOT NULL COLLATE NOCASE, archive_path TEXT NOT NULL UNIQUE,
            byte_count INTEGER NOT NULL, sha256 TEXT NOT NULL,
            relative_path TEXT NOT NULL UNIQUE);",
    )?;
    ingest_second_pass(source, directory.path(), &mut db, &report, &cancel)?;
    drop(db);
    verify_spool(directory.path(), &report)?;
    check_cancel(&cancel)?;
    let metadata_index = report
        .metadata_items
        .iter()
        .enumerate()
        .map(|(index, item)| (item.source_id.to_ascii_lowercase(), index))
        .collect();
    let resource_index = report
        .physical_resource_files
        .iter()
        .enumerate()
        .map(|(index, resource)| (resource.source_id.to_ascii_lowercase(), index))
        .collect();
    Ok(JexPreparedSource {
        directory,
        report,
        metadata_index,
        resource_index,
    })
}

fn check_cancel(cancel: &Option<Arc<AtomicBool>>) -> Result<(), JexPrepareError> {
    if cancel.as_ref().is_some_and(|v| v.load(Ordering::Relaxed)) {
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

fn resource_spool_path(directory: &Path, id: &str) -> PathBuf {
    directory
        .join("resources")
        .join(format!("{}.blob", id.to_ascii_lowercase()))
}

fn ingest_second_pass(
    source: &Path,
    directory: &Path,
    db: &mut Connection,
    expected: &JexScanReport,
    cancel: &Option<Arc<AtomicBool>>,
) -> Result<(), JexPrepareError> {
    let file = File::open(source).map_err(|e| changed(format!("archive open: {e}")))?;
    let mut archive = Archive::new(file);
    let tx = db.transaction()?;
    let mut paths = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let expected_items: BTreeMap<_, _> = expected
        .metadata_items
        .iter()
        .map(|item| (item.archive_path.as_str(), item))
        .collect();
    let expected_resources: BTreeMap<_, _> = expected
        .physical_resource_files
        .iter()
        .map(|resource| (resource.archive_path.as_str(), resource))
        .collect();
    let mut metadata = Vec::new();
    let mut resources = Vec::new();
    let mut entry_count = 0usize;
    let entries = archive
        .entries()
        .map_err(|e| changed(format!("tar entries: {e}")))?;
    for entry in entries.raw(true) {
        check_cancel(cancel)?;
        if entry_count >= MAX_JEX_ARCHIVE_ENTRIES {
            return Err(changed("entry count exceeds preflight"));
        }
        entry_count += 1;
        let mut entry = entry.map_err(|e| changed(format!("tar entry {entry_count}: {e}")))?;
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            return Err(changed(format!(
                "unsafe {} entry {}",
                tar_entry_kind(kind),
                raw_archive_path_for_error(&entry)
            )));
        }
        let path = checked_archive_path(&entry).map_err(|e| changed(e.to_string()))?;
        if kind.is_dir() {
            if path != "resources" && path != "resources/" {
                return Err(changed(path));
            }
            continue;
        }
        if !paths.insert(path.clone()) {
            return Err(changed(format!("duplicate path {path}")));
        }
        if let Some(path_id) = root_item_id(&path) {
            let raw = read_item(&mut entry, &path).map_err(|e| changed(e.to_string()))?;
            let item = parse_item(&path, &raw).map_err(|e| changed(e.to_string()))?;
            if item.normalized_id != path_id.to_ascii_lowercase()
                || !ids.insert(item.normalized_id.clone())
            {
                return Err(changed(path));
            }
            let evidence = JexScannedMetadataItem {
                archive_path: path.clone(),
                source_id: item.id.clone(),
                item_type: item.item_type,
                byte_count: raw.len() as u64,
                raw_sha256: format!("{:x}", Sha256::digest(raw.as_bytes())),
                canonical_note_body_sha256: (item.item_type == 1)
                    .then(|| format!("{:x}", Sha256::digest(item.note_body.as_bytes()))),
            };
            if expected_items.get(path.as_str()).copied() != Some(&evidence) {
                return Err(changed(path));
            }
            tx.execute("INSERT INTO jex_source_items (source_id,archive_path,item_type,byte_count,raw_sha256,canonical_note_body_sha256,raw_bytes) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![item.id, path, item.item_type, evidence.byte_count as i64,
                    evidence.raw_sha256, evidence.canonical_note_body_sha256, raw.as_bytes()])?;
            metadata.push(evidence);
        } else if super::is_resource_path(&path) {
            let id = physical_resource_id(&path).map_err(|e| changed(e.to_string()))?;
            let Some(preflight_resource) = expected_resources.get(path.as_str()).copied() else {
                return Err(changed(path));
            };
            if preflight_resource.source_id != id {
                return Err(changed(path));
            }
            let relative = format!("resources/{}.blob", id.to_ascii_lowercase());
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(directory.join(&relative))?;
            let mut hash = Sha256::new();
            let mut size = 0u64;
            let mut buffer = [0u8; STREAM_BUFFER_BYTES];
            loop {
                check_cancel(cancel)?;
                let n = entry
                    .read(&mut buffer)
                    .map_err(|e| changed(format!("resource {path}: {e}")))?;
                if n == 0 {
                    break;
                }
                size = size
                    .checked_add(n as u64)
                    .ok_or_else(|| changed(path.clone()))?;
                if size > MAX_JEX_RESOURCE_BYTES {
                    return Err(changed(path));
                }
                hash.update(&buffer[..n]);
                output.write_all(&buffer[..n])?;
            }
            output.sync_all()?;
            let evidence = JexPhysicalResourceFile {
                source_id: id.clone(),
                archive_path: path.clone(),
                byte_count: size,
                sha256: format!("{:x}", hash.finalize()),
            };
            if &evidence != preflight_resource {
                return Err(changed(path));
            }
            tx.execute("INSERT INTO jex_source_resources (source_id,archive_path,byte_count,sha256,relative_path) VALUES (?1,?2,?3,?4,?5)",
                params![id, path, size as i64, evidence.sha256, relative])?;
            resources.push(evidence);
        } else {
            return Err(changed(path));
        }
    }
    metadata.sort_by(|a, b| a.archive_path.cmp(&b.archive_path));
    resources.sort_by(|a, b| a.archive_path.cmp(&b.archive_path));
    if entry_count != expected.archive_entry_count
        || metadata != expected.metadata_items
        || resources != expected.physical_resource_files
        || metadata.len()
            != expected.counts.notes
                + expected.counts.folders
                + expected.counts.resource_metadata
                + expected.counts.tags
                + expected.counts.note_tag_relations
        || resources.len() != expected.counts.physical_resource_files
    {
        return Err(changed("final item/resource/entry counts"));
    }
    tx.commit()?;
    Ok(())
}

fn verify_resource_file(
    file: &mut File,
    expected: &JexPhysicalResourceFile,
) -> Result<(), JexPrepareError> {
    if file.metadata()?.len() != expected.byte_count {
        return Err(JexPrepareError::Verification(format!(
            "resource {} size changed",
            expected.source_id
        )));
    }
    let mut hash = Sha256::new();
    let size = io::copy(file, &mut hash)?;
    if size != expected.byte_count || format!("{:x}", hash.finalize()) != expected.sha256 {
        return Err(JexPrepareError::Verification(format!(
            "resource {} hash changed",
            expected.source_id
        )));
    }
    Ok(())
}

fn verify_spool(directory: &Path, expected: &JexScanReport) -> Result<(), JexPrepareError> {
    let db = Connection::open(directory.join(SPOOL_DATABASE))?;
    let integrity: String = db.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(JexPrepareError::Verification(format!(
            "integrity_check: {integrity}"
        )));
    }
    let foreign_errors: i64 =
        db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    if foreign_errors != 0 {
        return Err(JexPrepareError::Verification(
            "foreign_key_check failed".into(),
        ));
    }
    let item_count: i64 = db.query_row("SELECT count(*) FROM jex_source_items", [], |row| {
        row.get(0)
    })?;
    let resource_count: i64 =
        db.query_row("SELECT count(*) FROM jex_source_resources", [], |row| {
            row.get(0)
        })?;
    if item_count != expected.metadata_items.len() as i64
        || resource_count != expected.physical_resource_files.len() as i64
        || fs::read_dir(directory.join("resources"))?.count()
            != expected.physical_resource_files.len()
    {
        return Err(JexPrepareError::Verification(
            "source item/resource count mismatch".into(),
        ));
    }
    for item in &expected.metadata_items {
        let (id, kind, size, raw_hash, body_hash, raw): (String, i64, i64, String, Option<String>, Vec<u8>) = db.query_row(
            "SELECT source_id,item_type,byte_count,raw_sha256,canonical_note_body_sha256,raw_bytes FROM jex_source_items WHERE archive_path=?1",
            [&item.archive_path], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)))?;
        if id != item.source_id
            || kind != item.item_type
            || size != item.byte_count as i64
            || raw.len() as u64 != item.byte_count
            || raw_hash != item.raw_sha256
            || body_hash != item.canonical_note_body_sha256
            || format!("{:x}", Sha256::digest(&raw)) != item.raw_sha256
        {
            return Err(JexPrepareError::Verification(format!(
                "item {} mismatch",
                item.archive_path
            )));
        }
        let text = String::from_utf8(raw).map_err(|_| {
            JexPrepareError::Verification(format!("item {} UTF-8 changed", item.archive_path))
        })?;
        let parsed = parse_item(&item.archive_path, &text)
            .map_err(|e| JexPrepareError::Verification(e.to_string()))?;
        if (parsed.item_type == 1)
            .then(|| format!("{:x}", Sha256::digest(parsed.note_body.as_bytes())))
            != item.canonical_note_body_sha256
        {
            return Err(JexPrepareError::Verification(format!(
                "item {} body changed",
                item.archive_path
            )));
        }
    }
    for resource in &expected.physical_resource_files {
        let (id, size, sha, relative): (String,i64,String,String) = db.query_row(
            "SELECT source_id,byte_count,sha256,relative_path FROM jex_source_resources WHERE archive_path=?1",
            [&resource.archive_path], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?;
        if id != resource.source_id
            || size != resource.byte_count as i64
            || sha != resource.sha256
            || relative != format!("resources/{}.blob", id.to_ascii_lowercase())
        {
            return Err(JexPrepareError::Verification(format!(
                "resource {} metadata mismatch",
                resource.archive_path
            )));
        }
        let mut file = File::open(directory.join(relative))?;
        verify_resource_file(&mut file, resource)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tar::{Builder as TarBuilder, EntryType, Header};
    use tempfile::tempdir;

    const NOTE: &str = "11111111111111111111111111111111";
    const RESOURCE: &str = "22222222222222222222222222222222";

    fn append(builder: &mut TarBuilder<File>, path: &str, bytes: &[u8]) {
        let mut header = Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, path, Cursor::new(bytes))
            .unwrap();
    }

    fn write_archive(path: &Path, note: &str, blob: &[u8], extra: bool) {
        let mut builder = TarBuilder::new(File::create(path).unwrap());
        append(&mut builder, &format!("resources/{RESOURCE}.png"), blob);
        append(
            &mut builder,
            &format!("{RESOURCE}.md"),
            format!("图.png\n\nid: {RESOURCE}\ntype_: 4\nmime: image/png\nfile_extension: png\n")
                .as_bytes(),
        );
        append(&mut builder, &format!("{NOTE}.md"), note.as_bytes());
        if extra {
            let mut header = Header::new_gnu();
            header.set_entry_type(EntryType::Directory);
            header.set_size(0);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, "resources/", Cursor::new([]))
                .unwrap();
        }
        builder.finish().unwrap();
    }

    #[test]
    fn changed_note_after_resource_spool_refuses_handle_and_removes_child() {
        // Mutation caught: a raw item changing after preflight while earlier
        // resource and metadata writes have already succeeded.
        let parent = tempdir().unwrap();
        let source = parent.path().join("source.jex");
        let staging_parent = tempdir().unwrap();
        let sentinel = staging_parent.path().join("sibling");
        fs::create_dir(&sentinel).unwrap();
        fs::write(sentinel.join("keep.bin"), b"untouched").unwrap();
        let original = format!("标题\n\n原文\n\nid: {NOTE}\ntype_: 1\n");
        let changed_note = format!("标题\n\n改文\n\nid: {NOTE}\ntype_: 1\n");
        write_archive(&source, &original, b"image", false);
        let result = prepare_inner(&source, staging_parent.path(), None, || {
            write_archive(&source, &changed_note, b"image", false);
        });
        assert!(
            matches!(result, Err(JexPrepareError::SourceChanged { entity }) if entity == format!("{NOTE}.md"))
        );
        assert_eq!(fs::read(sentinel.join("keep.bin")).unwrap(), b"untouched");
        assert_eq!(fs::read_dir(staging_parent.path()).unwrap().count(), 1);
    }

    #[test]
    fn changed_resource_or_archive_count_after_preflight_is_source_changed() {
        // Mutation caught: accepting a changed physical blob or an added tar
        // entity merely because the metadata item list still matches.
        let parent = tempdir().unwrap();
        let source = parent.path().join("source.jex");
        let staging_parent = tempdir().unwrap();
        let note = format!("标题\n\n原文\n\nid: {NOTE}\ntype_: 1\n");
        write_archive(&source, &note, b"image", false);
        let result = prepare_inner(&source, staging_parent.path(), None, || {
            write_archive(&source, &note, b"IMAge", false);
        });
        assert!(
            matches!(result, Err(JexPrepareError::SourceChanged { entity }) if entity == format!("resources/{RESOURCE}.png"))
        );
        assert_eq!(fs::read_dir(staging_parent.path()).unwrap().count(), 0);

        write_archive(&source, &note, b"image", false);
        let result = prepare_inner(&source, staging_parent.path(), None, || {
            write_archive(&source, &note, b"image", true);
        });
        assert!(
            matches!(result, Err(JexPrepareError::SourceChanged { entity }) if entity == "final item/resource/entry counts")
        );
        assert_eq!(fs::read_dir(staging_parent.path()).unwrap().count(), 0);
    }
}
