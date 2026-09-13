//! Human-readable, fail-closed library export bundles.
//!
//! This is the first deliberately narrow Task-8 restore slice: active notes
//! in the default notebook with no tags. The JSON manifest is authoritative
//! for recovery; the matching HTML files are there for a person to inspect.
//! Every resource is copied as its original bytes and verified by SHA-256
//! before an export is published or a restore profile is opened.
//!
//! Behavioral source crosswalk: reconstructed Evernote
//! `11354__enex-exporter.js` exports note metadata, content and attachments
//! per note. This independent implementation keeps that readable shape but
//! deliberately does *not* copy its "record attachment read failure and
//! continue" behavior: one absent/corrupt attachment aborts the whole bundle.

use crate::{BlobHash, CanonicalDocument, LibraryRepository, NoteId, ResourceId};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use tempfile::Builder;
use thiserror::Error;

const BUNDLE_VERSION: u32 = 1;
const COPY_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadableExportReport {
    pub note_count: usize,
    pub resource_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadableRestoreReport {
    pub note_count: usize,
    pub resource_count: usize,
}

#[derive(Debug, Error)]
pub enum ReadableExportError {
    #[error("readable export I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("readable export JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("readable export repository operation failed: {0}")]
    Repository(#[from] crate::LibraryError),
    #[error("export target must not already exist")]
    TargetExists,
    #[error("readable export has no selected notes")]
    NoNotes,
    #[error("readable export repeats note ID {0}")]
    DuplicateNoteId(String),
    #[error("readable export only supports active notes in the default notebook without tags: {0}")]
    UnsupportedOrganization(String),
    #[error("readable export manifest is invalid: {0}")]
    InvalidManifest(String),
    #[error("readable export restore destination must be an existing empty directory")]
    RestoreDestinationNotEmpty,
    #[error("readable export restore profile is not pristine")]
    RestoreProfileNotPristine,
    #[error("readable export resource verification failed: {0}")]
    ResourceVerification(String),
    #[error("readable export SQLite restore failed: {0}")]
    Sql(#[from] rusqlite::Error),
}

#[derive(Debug, Serialize, Deserialize)]
struct BundleManifest {
    format: String,
    version: u32,
    default_notebook: ManifestNotebook,
    notes: Vec<ManifestNote>,
    resources: Vec<ManifestResource>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ManifestNotebook {
    id: String,
    title: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ManifestNote {
    id: String,
    title: String,
    html_path: String,
    body_html_sha256: String,
    body_text: String,
    created_time: i64,
    updated_time: i64,
    revision: i64,
    resource_ids: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ManifestResource {
    id: String,
    sha256: String,
    size: i64,
    title: String,
    mime: String,
    file_extension: String,
    revision: i64,
    relative_path: String,
}

/// Exports selected active, untagged notes from the default notebook into a
/// newly-created bundle directory. The bundle path must not exist already.
pub fn export_readable_library(
    repository: &LibraryRepository,
    note_ids: &[NoteId],
    destination: impl AsRef<Path>,
) -> Result<ReadableExportReport, ReadableExportError> {
    if note_ids.is_empty() {
        return Err(ReadableExportError::NoNotes);
    }
    let destination = destination.as_ref();
    if destination.exists() {
        return Err(ReadableExportError::TargetExists);
    }
    let parent = destination.parent().ok_or_else(|| {
        ReadableExportError::InvalidManifest("export destination has no parent".into())
    })?;
    fs::create_dir_all(parent)?;
    let default = repository.default_notebook()?;
    let mut seen_notes = BTreeSet::new();
    let mut notes = Vec::with_capacity(note_ids.len());
    let mut resources = BTreeMap::<String, ManifestResource>::new();
    let temporary = Builder::new()
        .prefix("readable-export-")
        .tempdir_in(parent)?;
    fs::create_dir(temporary.path().join("notes"))?;
    fs::create_dir(temporary.path().join("resources"))?;

    for note_id in note_ids {
        if !seen_notes.insert(note_id.as_str().to_owned()) {
            return Err(ReadableExportError::DuplicateNoteId(
                note_id.as_str().into(),
            ));
        }
        let note = repository.load_note(note_id)?.ok_or_else(|| {
            ReadableExportError::InvalidManifest(format!("note missing: {note_id:?}"))
        })?;
        if note.deleted_time.is_some() || note.notebook_id != default.id || !note.tag_ids.is_empty()
        {
            return Err(ReadableExportError::UnsupportedOrganization(
                note.id.as_str().into(),
            ));
        }
        let html_path = format!("notes/{}.html", note.id.as_str());
        write_synced(
            &temporary.path().join(&html_path),
            note.body_html.as_bytes(),
        )?;
        for resource_id in &note.resource_ids {
            let resource = repository.resource_metadata(resource_id)?.ok_or_else(|| {
                ReadableExportError::ResourceVerification(format!(
                    "metadata missing: {}",
                    resource_id.as_str()
                ))
            })?;
            let key = resource_id.as_str().to_owned();
            if resources.contains_key(&key) {
                continue;
            }
            let relative_path = format!(
                "resources/{}--{}--{}",
                resource.sha256.as_str(),
                resource.id.as_str(),
                safe_display_name(&resource.title, &resource.file_extension)
            );
            let (_, mut input) = repository
                .open_verified_resource_file(resource_id)?
                .ok_or_else(|| {
                    ReadableExportError::ResourceVerification(format!(
                        "bytes missing: {}",
                        resource_id.as_str()
                    ))
                })?;
            let actual = copy_and_hash(&mut input, &temporary.path().join(&relative_path))?;
            if actual.0 != resource.sha256.as_str() || actual.1 != resource.size as u64 {
                return Err(ReadableExportError::ResourceVerification(format!(
                    "copied bytes differ: {}",
                    resource_id.as_str()
                )));
            }
            resources.insert(
                key,
                ManifestResource {
                    id: resource.id.as_str().into(),
                    sha256: resource.sha256.as_str().into(),
                    size: resource.size,
                    title: resource.title,
                    mime: resource.mime,
                    file_extension: resource.file_extension,
                    revision: resource.revision,
                    relative_path,
                },
            );
        }
        notes.push(ManifestNote {
            id: note.id.as_str().into(),
            title: note.title,
            html_path,
            body_html_sha256: sha256_hex(note.body_html.as_bytes()),
            body_text: note.body_text,
            created_time: note.created_time,
            updated_time: note.updated_time,
            revision: note.revision,
            resource_ids: note
                .resource_ids
                .iter()
                .map(|id| id.as_str().into())
                .collect(),
        });
    }
    let manifest = BundleManifest {
        format: "app-lite-readable-export".into(),
        version: BUNDLE_VERSION,
        default_notebook: ManifestNotebook {
            id: default.id.as_str().into(),
            title: default.title,
        },
        notes,
        resources: resources.into_values().collect(),
    };
    write_synced(
        &temporary.path().join("manifest.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    fs::rename(temporary.path(), destination)?;
    let report = ReadableExportReport {
        note_count: manifest.notes.len(),
        resource_count: manifest.resources.len(),
    };
    // The directory was atomically renamed. Prevent TempDir cleanup from
    // removing the caller-owned published bundle.
    std::mem::forget(temporary);
    Ok(report)
}

/// Verifies an entire bundle before opening a newly-created empty profile.
/// It restores only the current default-notebook/no-tag active-note slice.
pub fn restore_readable_export(
    bundle: impl AsRef<Path>,
    empty_profile: impl AsRef<Path>,
) -> Result<ReadableRestoreReport, ReadableExportError> {
    let bundle = bundle.as_ref();
    let empty_profile = empty_profile.as_ref();
    ensure_existing_empty_directory(empty_profile)?;
    let manifest: BundleManifest =
        serde_json::from_slice(&fs::read(bundle.join("manifest.json"))?)?;
    validate_manifest(bundle, &manifest)?;

    let database = empty_profile.join("library.sqlite");
    let repository = LibraryRepository::open(&database)?;
    let default = repository.default_notebook()?;
    let mut generated_resource_ids = Vec::with_capacity(manifest.resources.len());
    for resource in &manifest.resources {
        let source = File::open(bundle.join(&resource.relative_path))?;
        let generated = repository.import_resource_reader(
            source,
            usize::try_from(resource.size).map_err(|_| {
                ReadableExportError::InvalidManifest("resource size is invalid".into())
            })?,
            &resource.title,
            &resource.mime,
            &resource.file_extension,
        )?;
        let actual = repository.resource_metadata(&generated)?.ok_or_else(|| {
            ReadableExportError::ResourceVerification("staged resource vanished".into())
        })?;
        if actual.sha256.as_str() != resource.sha256 || actual.size != resource.size {
            return Err(ReadableExportError::ResourceVerification(format!(
                "resource changed while restoring: {}",
                resource.id
            )));
        }
        generated_resource_ids.push(generated);
    }
    drop(repository);

    let mut db = Connection::open(&database)?;
    db.pragma_update(None, "foreign_keys", "ON")?;
    let tx = db.transaction()?;
    let default_count: i64 =
        tx.query_row("SELECT count(*) FROM notebooks", [], |row| row.get(0))?;
    let note_count: i64 = tx.query_row("SELECT count(*) FROM notes", [], |row| row.get(0))?;
    if default_count != 1 || note_count != 0 {
        return Err(ReadableExportError::RestoreProfileNotPristine);
    }
    tx.execute(
        "UPDATE notebooks SET id=?1, title=?2 WHERE id=?3 AND is_default=1",
        params![
            manifest.default_notebook.id,
            manifest.default_notebook.title,
            default.id.as_str()
        ],
    )?;
    for (resource, generated_id) in manifest.resources.iter().zip(&generated_resource_ids) {
        tx.execute(
            "UPDATE resources SET id=?1, revision=?2 WHERE id=?3",
            params![resource.id, resource.revision, generated_id.as_str()],
        )?;
        for table in [
            "resource_search_rows",
            "resource_filename_unicode",
            "resource_filename_trigram",
        ] {
            tx.execute(
                &format!("UPDATE {table} SET resource_id=?1 WHERE resource_id=?2"),
                params![resource.id, generated_id.as_str()],
            )?;
        }
    }
    for note in &manifest.notes {
        tx.execute(
            "INSERT INTO notes (id,title,body_html,body_text,snippet,notebook_id,created_time,updated_time,revision)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![note.id, note.title, read_html(bundle, note)?, note.body_text, snippet(&note.body_text), manifest.default_notebook.id, note.created_time, note.updated_time, note.revision],
        )?;
        for (position, resource_id) in note.resource_ids.iter().enumerate() {
            tx.execute(
                "INSERT INTO note_resources(note_id,position,resource_id,is_associated) VALUES(?1,?2,?3,1)",
                params![note.id, position as i64, resource_id],
            )?;
        }
        tx.execute(
            "INSERT INTO note_revisions(note_id,revision,title,body_html,body_text,created_time)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                note.id,
                note.revision,
                note.title,
                read_html(bundle, note)?,
                note.body_text,
                note.updated_time
            ],
        )?;
        tx.execute(
            "INSERT INTO search_queue(note_id,updated_time,reason) VALUES(?1,?2,'restore')",
            params![note.id, note.updated_time],
        )?;
    }
    tx.execute("DELETE FROM sync_outbox", [])?;
    tx.commit()?;
    drop(db);

    let repository = LibraryRepository::open(&database)?;
    while repository.has_pending_search_jobs()? {
        if repository.process_search_jobs()? == 0 {
            return Err(ReadableExportError::InvalidManifest(
                "restored search queue stalled".into(),
            ));
        }
    }
    Ok(ReadableRestoreReport {
        note_count: manifest.notes.len(),
        resource_count: manifest.resources.len(),
    })
}

fn validate_manifest(bundle: &Path, manifest: &BundleManifest) -> Result<(), ReadableExportError> {
    if manifest.format != "app-lite-readable-export" || manifest.version != BUNDLE_VERSION {
        return Err(ReadableExportError::InvalidManifest(
            "unsupported bundle format/version".into(),
        ));
    }
    if manifest.notes.is_empty() {
        return Err(ReadableExportError::InvalidManifest(
            "bundle contains no notes".into(),
        ));
    }
    crate::NotebookId::parse(&manifest.default_notebook.id)
        .map_err(|_| ReadableExportError::InvalidManifest("invalid default notebook ID".into()))?;
    let mut note_ids = BTreeSet::new();
    let mut resources = BTreeMap::new();
    for resource in &manifest.resources {
        let id = ResourceId::new(&resource.id)
            .map_err(|_| ReadableExportError::InvalidManifest("invalid resource ID".into()))?;
        let hash = BlobHash::new(&resource.sha256)
            .map_err(|_| ReadableExportError::InvalidManifest("invalid resource SHA-256".into()))?;
        if resource.size < 0 || !resources.insert(id.as_str().to_owned(), resource).is_none() {
            return Err(ReadableExportError::InvalidManifest(
                "duplicate or invalid resource".into(),
            ));
        }
        let relative = safe_relative_path(&resource.relative_path)?;
        if !relative.starts_with("resources/") {
            return Err(ReadableExportError::InvalidManifest(
                "resource path escapes resources".into(),
            ));
        }
        let (actual_hash, actual_size) = hash_file(&bundle.join(relative))?;
        if actual_hash != hash.as_str() || actual_size != resource.size as u64 {
            return Err(ReadableExportError::ResourceVerification(format!(
                "{}",
                resource.id
            )));
        }
    }
    for note in &manifest.notes {
        let id = NoteId::parse(&note.id)
            .map_err(|_| ReadableExportError::InvalidManifest("invalid note ID".into()))?;
        if note.revision < 1 || !note_ids.insert(id.as_str().to_owned()) {
            return Err(ReadableExportError::InvalidManifest(
                "duplicate or invalid note".into(),
            ));
        }
        let html = read_html(bundle, note)?;
        if sha256_hex(html.as_bytes()) != note.body_html_sha256 {
            return Err(ReadableExportError::InvalidManifest(
                "HTML digest differs".into(),
            ));
        }
        let document = CanonicalDocument::parse_html(&html)
            .map_err(|_| ReadableExportError::InvalidManifest("HTML is not canonical".into()))?;
        if document.search_text().as_str() != note.body_text
            || document
                .resource_ids()
                .iter()
                .map(ResourceId::as_str)
                .collect::<Vec<_>>()
                != note
                    .resource_ids
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
            || note
                .resource_ids
                .iter()
                .any(|id| !resources.contains_key(id))
        {
            return Err(ReadableExportError::InvalidManifest(
                "note body/resources differ".into(),
            ));
        }
    }
    Ok(())
}

fn ensure_existing_empty_directory(path: &Path) -> Result<(), ReadableExportError> {
    if !path.is_dir() || fs::read_dir(path)?.next().is_some() {
        return Err(ReadableExportError::RestoreDestinationNotEmpty);
    }
    Ok(())
}

fn read_html(bundle: &Path, note: &ManifestNote) -> Result<String, ReadableExportError> {
    let path = safe_relative_path(&note.html_path)?;
    if !path.starts_with("notes/") {
        return Err(ReadableExportError::InvalidManifest(
            "note HTML path escapes notes".into(),
        ));
    }
    String::from_utf8(fs::read(bundle.join(path))?)
        .map_err(|_| ReadableExportError::InvalidManifest("note HTML is not UTF-8".into()))
}

fn safe_relative_path(path: &str) -> Result<PathBuf, ReadableExportError> {
    let path = Path::new(path);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ReadableExportError::InvalidManifest(
            "unsafe bundle path".into(),
        ));
    }
    Ok(path.to_owned())
}

fn safe_display_name(title: &str, extension: &str) -> String {
    let mut value = title
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .take(80)
        .collect::<String>();
    if value.is_empty() {
        value.push_str("attachment");
    }
    if !extension.is_empty() && !value.ends_with(extension) {
        value.push('.');
        value.push_str(extension.trim_matches('.'));
    }
    value
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn copy_and_hash(input: &mut dyn Read, destination: &Path) -> io::Result<(String, u64)> {
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count])?;
        digest.update(&buffer[..count]);
        size += count as u64;
    }
    output.sync_all()?;
    Ok((format!("{:x}", digest.finalize()), size))
}

fn hash_file(path: &Path) -> io::Result<(String, u64)> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bundle resource is not a regular file",
        ));
    }
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        size += count as u64;
    }
    Ok((format!("{:x}", digest.finalize()), size))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn snippet(text: &str) -> String {
    text.chars().take(280).collect()
}
