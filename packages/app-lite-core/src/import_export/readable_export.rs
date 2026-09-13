//! Human-readable, fail-closed note-selection export bundles.
//!
//! This is the first deliberately narrow Task-8 restore slice: an explicit
//! selection of active notes in the default notebook with no tags. It is not
//! a full-library backup API. The JSON manifest is authoritative
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
use std::path::Path;
use tempfile::Builder;
use thiserror::Error;

const BUNDLE_VERSION: u32 = 1;
const COPY_BUFFER_BYTES: usize = 64 * 1024;
const MAX_MANIFEST_BYTES: u64 = 32 * 1024 * 1024;
const MAX_BUNDLE_NOTES: usize = 10_000;
const MAX_BUNDLE_RESOURCES: usize = 50_000;
const MAX_NOTE_HTML_BYTES: u64 = 4 * 1024 * 1024;
const MAX_NOTE_TEXT_BYTES: usize = 4 * 1024 * 1024;
const MAX_NOTE_REVISIONS: usize = 10_000;
const MAX_NOTE_HISTORY_BYTES: usize = 16 * 1024 * 1024;

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
    selected_thumbnail_id: Option<String>,
    resource_ids: Vec<String>,
    revisions: Vec<ManifestRevision>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ManifestRevision {
    revision: i64,
    title: String,
    body_html: String,
    body_html_sha256: String,
    body_text: String,
    created_time: i64,
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

struct ValidatedBundle {
    manifest: BundleManifest,
    current_html: BTreeMap<String, String>,
    resource_files: BTreeMap<String, File>,
}

/// Exports selected active, untagged notes from the default notebook into a
/// newly-created bundle directory. The bundle path must not exist already.
pub fn export_readable_selection(
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
    let mut observed_notes = Vec::with_capacity(note_ids.len());
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
        let state = repository
            .readable_export_note_state(note_id, MAX_NOTE_REVISIONS, MAX_NOTE_HISTORY_BYTES)?
            .ok_or_else(|| {
                ReadableExportError::InvalidManifest(format!("note state missing: {note_id:?}"))
            })?;
        if state.revisions.is_empty() || state.revisions.len() > MAX_NOTE_REVISIONS {
            return Err(ReadableExportError::InvalidManifest(
                "invalid revision count".into(),
            ));
        }
        let last = state.revisions.last().expect("checked non-empty");
        if last.revision != note.revision
            || last.title != note.title
            || last.body_html != note.body_html
            || last.body_text != note.body_text
        {
            return Err(ReadableExportError::InvalidManifest(
                "note revision is not current".into(),
            ));
        }
        let html_path = expected_html_path(note.id.as_str());
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
            selected_thumbnail_id: state.selected_thumbnail_id.map(|id| id.as_str().into()),
            revisions: state
                .revisions
                .into_iter()
                .map(|revision| ManifestRevision {
                    revision: revision.revision,
                    title: revision.title,
                    body_html_sha256: sha256_hex(revision.body_html.as_bytes()),
                    body_html: revision.body_html,
                    body_text: revision.body_text,
                    created_time: revision.created_time,
                })
                .collect(),
        });
        observed_notes.push((note_id.clone(), note.revision, note.updated_time));
    }
    // An explicit selection is not a database-wide MVCC snapshot. It is still
    // fail-closed against a visible selection changing while files are copied.
    for (id, revision, updated_time) in &observed_notes {
        let reloaded = repository.load_note(id)?.ok_or_else(|| {
            ReadableExportError::InvalidManifest("selected note disappeared while exporting".into())
        })?;
        if reloaded.revision != *revision || reloaded.updated_time != *updated_time {
            return Err(ReadableExportError::InvalidManifest(
                "selected note changed while exporting; flush and retry".into(),
            ));
        }
        repository
            .readable_export_note_state(id, MAX_NOTE_REVISIONS, MAX_NOTE_HISTORY_BYTES)?
            .ok_or_else(|| {
                ReadableExportError::InvalidManifest("selected note state disappeared".into())
            })?;
    }
    for resource in resources.values() {
        let id = ResourceId::new(&resource.id).map_err(|_| {
            ReadableExportError::InvalidManifest("resource ID changed while exporting".into())
        })?;
        let reloaded = repository.resource_metadata(&id)?.ok_or_else(|| {
            ReadableExportError::ResourceVerification("resource disappeared while exporting".into())
        })?;
        if reloaded.sha256.as_str() != resource.sha256
            || reloaded.size != resource.size
            || reloaded.revision != resource.revision
        {
            return Err(ReadableExportError::ResourceVerification(
                "resource changed while exporting".into(),
            ));
        }
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

/// Verifies an entire bundle before opening an owned sibling staging profile.
/// It restores only the current default-notebook/no-tag active-note slice.
pub fn restore_readable_export(
    bundle: impl AsRef<Path>,
    empty_profile: impl AsRef<Path>,
) -> Result<ReadableRestoreReport, ReadableExportError> {
    let bundle = bundle.as_ref();
    let empty_profile = empty_profile.as_ref();
    ensure_existing_empty_directory(empty_profile)?;
    let validated = validate_manifest(bundle)?;
    let parent = empty_profile
        .parent()
        .ok_or_else(|| ReadableExportError::RestoreDestinationNotEmpty)?;
    let staging = Builder::new()
        .prefix("readable-restore-")
        .tempdir_in(parent)?;
    let database = staging.path().join("library.sqlite");
    let repository = LibraryRepository::open(&database)?;
    let default = repository.default_notebook()?;
    let mut generated_resource_ids = Vec::with_capacity(validated.manifest.resources.len());
    for resource in &validated.manifest.resources {
        let mut source = validated
            .resource_files
            .get(&resource.id)
            .ok_or_else(|| {
                ReadableExportError::InvalidManifest("verified resource disappeared".into())
            })?
            .try_clone()?;
        let generated = repository.import_resource_reader(
            &mut source,
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
            validated.manifest.default_notebook.id,
            validated.manifest.default_notebook.title,
            default.id.as_str()
        ],
    )?;
    for (resource, generated_id) in validated
        .manifest
        .resources
        .iter()
        .zip(&generated_resource_ids)
    {
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
    for note in &validated.manifest.notes {
        let current_html = validated.current_html.get(&note.id).ok_or_else(|| {
            ReadableExportError::InvalidManifest("verified note HTML disappeared".into())
        })?;
        tx.execute(
            "INSERT INTO notes (id,title,body_html,body_text,snippet,notebook_id,selected_thumbnail_id,created_time,updated_time,revision)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![note.id, note.title, current_html, note.body_text, snippet(&note.body_text), validated.manifest.default_notebook.id, note.selected_thumbnail_id, note.created_time, note.updated_time, note.revision],
        )?;
        for (position, resource_id) in note.resource_ids.iter().enumerate() {
            tx.execute(
                "INSERT INTO note_resources(note_id,position,resource_id,is_associated) VALUES(?1,?2,?3,1)",
                params![note.id, position as i64, resource_id],
            )?;
        }
        for revision in &note.revisions {
            tx.execute(
                "INSERT INTO note_revisions(note_id,revision,title,body_html,body_text,created_time)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                params![note.id, revision.revision, revision.title, revision.body_html, revision.body_text, revision.created_time],
            )?;
        }
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
    verify_restored(&repository, &validated)?;
    drop(repository);
    let verification = Connection::open(&database)?;
    let integrity: String =
        verification.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    let foreign_key_violations: i64 =
        verification.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    if integrity != "ok" || foreign_key_violations != 0 {
        return Err(ReadableExportError::InvalidManifest(
            "restored SQLite integrity verification failed".into(),
        ));
    }
    drop(verification);
    fs::rename(staging.path(), empty_profile)?;
    let report = ReadableRestoreReport {
        note_count: validated.manifest.notes.len(),
        resource_count: validated.manifest.resources.len(),
    };
    std::mem::forget(staging);
    Ok(report)
}

fn validate_manifest(bundle: &Path) -> Result<ValidatedBundle, ReadableExportError> {
    ensure_bundle_root(bundle)?;
    let manifest_bytes =
        read_regular_file_bounded(&bundle.join("manifest.json"), MAX_MANIFEST_BYTES)?;
    let manifest: BundleManifest = serde_json::from_slice(&manifest_bytes)?;
    if manifest.format != "app-lite-readable-export" || manifest.version != BUNDLE_VERSION {
        return Err(ReadableExportError::InvalidManifest(
            "unsupported bundle format/version".into(),
        ));
    }
    if manifest.notes.is_empty()
        || manifest.notes.len() > MAX_BUNDLE_NOTES
        || manifest.resources.len() > MAX_BUNDLE_RESOURCES
    {
        return Err(ReadableExportError::InvalidManifest(
            "bundle contains no notes".into(),
        ));
    }
    crate::NotebookId::parse(&manifest.default_notebook.id)
        .map_err(|_| ReadableExportError::InvalidManifest("invalid default notebook ID".into()))?;
    let mut note_ids = BTreeSet::new();
    let mut resources = BTreeMap::new();
    let mut resource_files = BTreeMap::new();
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
        let expected_path = expected_resource_path(resource);
        if resource.relative_path != expected_path {
            return Err(ReadableExportError::InvalidManifest(
                "resource path does not match resource identity".into(),
            ));
        }
        let (file, actual_hash, actual_size) =
            open_and_hash_regular_file(&bundle.join(&expected_path))?;
        if actual_hash != hash.as_str() || actual_size != resource.size as u64 {
            return Err(ReadableExportError::ResourceVerification(format!(
                "{}",
                resource.id
            )));
        }
        resource_files.insert(resource.id.clone(), file);
    }
    let mut current_html = BTreeMap::new();
    for note in &manifest.notes {
        let id = NoteId::parse(&note.id)
            .map_err(|_| ReadableExportError::InvalidManifest("invalid note ID".into()))?;
        if note.revision < 1 || !note_ids.insert(id.as_str().to_owned()) {
            return Err(ReadableExportError::InvalidManifest(
                "duplicate or invalid note".into(),
            ));
        }
        if note.html_path != expected_html_path(&note.id)
            || note.body_text.len() > MAX_NOTE_TEXT_BYTES
        {
            return Err(ReadableExportError::InvalidManifest(
                "note metadata exceeds selection bounds".into(),
            ));
        }
        let html = String::from_utf8(read_regular_file_bounded(
            &bundle.join(&note.html_path),
            MAX_NOTE_HTML_BYTES,
        )?)
        .map_err(|_| ReadableExportError::InvalidManifest("note HTML is not UTF-8".into()))?;
        if sha256_hex(html.as_bytes()) != note.body_html_sha256 {
            return Err(ReadableExportError::InvalidManifest(
                "HTML digest differs".into(),
            ));
        }
        let document = CanonicalDocument::parse_html(&html)
            .map_err(|_| ReadableExportError::InvalidManifest("HTML is not canonical".into()))?;
        if document.to_canonical_html().as_str() != html
            || document.search_text().as_str() != note.body_text
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
        if let Some(selected) = &note.selected_thumbnail_id {
            if !note.resource_ids.contains(selected) || !resources.contains_key(selected) {
                return Err(ReadableExportError::InvalidManifest(
                    "selected thumbnail differs".into(),
                ));
            }
        }
        if note.revisions.is_empty() || note.revisions.len() > MAX_NOTE_REVISIONS {
            return Err(ReadableExportError::InvalidManifest(
                "revision count exceeds bound".into(),
            ));
        }
        let mut expected_revision = 1_i64;
        for revision in &note.revisions {
            if revision.revision != expected_revision
                || revision.body_text.len() > MAX_NOTE_TEXT_BYTES
                || sha256_hex(revision.body_html.as_bytes()) != revision.body_html_sha256
            {
                return Err(ReadableExportError::InvalidManifest(
                    "revision metadata differs".into(),
                ));
            }
            let document = CanonicalDocument::parse_html(&revision.body_html).map_err(|_| {
                ReadableExportError::InvalidManifest("revision HTML is invalid".into())
            })?;
            if document.to_canonical_html().as_str() != revision.body_html
                || document.search_text().as_str() != revision.body_text
            {
                return Err(ReadableExportError::InvalidManifest(
                    "revision HTML is not canonical".into(),
                ));
            }
            expected_revision += 1;
        }
        let current = note.revisions.last().expect("checked non-empty");
        if current.revision != note.revision
            || current.title != note.title
            || current.body_html != html
            || current.body_text != note.body_text
            || current.created_time != note.updated_time
        {
            return Err(ReadableExportError::InvalidManifest(
                "current note differs from history".into(),
            ));
        }
        current_html.insert(note.id.clone(), html);
    }
    Ok(ValidatedBundle {
        manifest,
        current_html,
        resource_files,
    })
}

fn ensure_existing_empty_directory(path: &Path) -> Result<(), ReadableExportError> {
    if !path.is_dir() || fs::read_dir(path)?.next().is_some() {
        return Err(ReadableExportError::RestoreDestinationNotEmpty);
    }
    Ok(())
}

fn verify_restored(
    repository: &LibraryRepository,
    validated: &ValidatedBundle,
) -> Result<(), ReadableExportError> {
    for note in &validated.manifest.notes {
        let actual = repository
            .load_note(
                &NoteId::parse(&note.id)
                    .map_err(|_| ReadableExportError::InvalidManifest("invalid note ID".into()))?,
            )?
            .ok_or_else(|| ReadableExportError::InvalidManifest("restored note missing".into()))?;
        let html = validated
            .current_html
            .get(&note.id)
            .expect("validated note HTML exists");
        if actual.title != note.title
            || actual.body_html != *html
            || actual.body_text != note.body_text
            || actual
                .resource_ids
                .iter()
                .map(ResourceId::as_str)
                .collect::<Vec<_>>()
                != note
                    .resource_ids
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
            || actual.revision != note.revision
        {
            return Err(ReadableExportError::InvalidManifest(
                "restored note differs".into(),
            ));
        }
        let state = repository
            .readable_export_note_state(&actual.id, MAX_NOTE_REVISIONS, MAX_NOTE_HISTORY_BYTES)?
            .ok_or_else(|| {
                ReadableExportError::InvalidManifest("restored note state missing".into())
            })?;
        if state.selected_thumbnail_id.as_ref().map(ResourceId::as_str)
            != note.selected_thumbnail_id.as_deref()
            || state.revisions.len() != note.revisions.len()
            || state
                .revisions
                .iter()
                .zip(&note.revisions)
                .any(|(actual, expected)| {
                    actual.revision != expected.revision
                        || actual.title != expected.title
                        || actual.body_html != expected.body_html
                        || actual.body_text != expected.body_text
                        || actual.created_time != expected.created_time
                })
        {
            return Err(ReadableExportError::InvalidManifest(
                "restored history differs".into(),
            ));
        }
    }
    for resource in &validated.manifest.resources {
        let id = ResourceId::new(&resource.id)
            .map_err(|_| ReadableExportError::InvalidManifest("invalid resource ID".into()))?;
        let actual = repository.resource_metadata(&id)?.ok_or_else(|| {
            ReadableExportError::InvalidManifest("restored resource missing".into())
        })?;
        if actual.sha256.as_str() != resource.sha256
            || actual.size != resource.size
            || actual.revision != resource.revision
            || sha256_hex(&repository.read_resource_bytes(&id)?.ok_or_else(|| {
                ReadableExportError::InvalidManifest("restored resource bytes missing".into())
            })?) != resource.sha256
        {
            return Err(ReadableExportError::ResourceVerification(
                "restored resource differs".into(),
            ));
        }
    }
    if repository.outbox_count()? != 0 {
        return Err(ReadableExportError::InvalidManifest(
            "restored profile queued sync work".into(),
        ));
    }
    Ok(())
}

fn ensure_bundle_root(bundle: &Path) -> Result<(), ReadableExportError> {
    for entry in [bundle, &bundle.join("notes"), &bundle.join("resources")] {
        let metadata = fs::symlink_metadata(entry)?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err(ReadableExportError::InvalidManifest(
                "bundle directory is unsafe".into(),
            ));
        }
    }
    Ok(())
}

fn expected_html_path(note_id: &str) -> String {
    format!("notes/{note_id}.html")
}

fn expected_resource_path(resource: &ManifestResource) -> String {
    format!(
        "resources/{}--{}--{}",
        resource.sha256,
        resource.id,
        safe_display_name(&resource.title, &resource.file_extension)
    )
}

fn read_regular_file_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, ReadableExportError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > maximum
    {
        return Err(ReadableExportError::InvalidManifest(
            "bundle file is unsafe or exceeds bound".into(),
        ));
    }
    let mut file = File::open(path)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata.len() || bytes.len() as u64 > maximum {
        return Err(ReadableExportError::InvalidManifest(
            "bundle file changed or exceeds bound".into(),
        ));
    }
    Ok(bytes)
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

fn open_and_hash_regular_file(path: &Path) -> Result<(File, String, u64), ReadableExportError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(ReadableExportError::InvalidManifest(
            "bundle resource is not a regular file".into(),
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
    use std::io::Seek;
    file.rewind()?;
    Ok((file, format!("{:x}", digest.finalize()), size))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn snippet(text: &str) -> String {
    text.chars().take(280).collect()
}
