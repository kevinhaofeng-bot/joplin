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

use crate::repository::{
    readable_export_query_default_notebook, readable_export_query_note,
    readable_export_query_note_state, readable_export_query_resource,
};
use crate::{BlobHash, CanonicalDocument, LibraryRepository, NoteId, ResourceId};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use tempfile::Builder;
use thiserror::Error;

const BUNDLE_VERSION: u32 = 2;
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
    revision: i64,
    created_time: i64,
    updated_time: i64,
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
    created_time: i64,
    updated_time: i64,
    relative_path: String,
}

struct ValidatedBundle {
    manifest: BundleManifest,
    current_html: BTreeMap<String, String>,
    dirs: BundleDirs,
}

struct BundleDirs {
    // Holding these descriptors keeps every read anchored to the same bundle
    // tree even if a caller replaces one of its pathnames during restore.
    _root: File,
    notes: File,
    resources: File,
}

/// Exports selected active, untagged notes from the default notebook into a
/// newly-created bundle directory. The bundle path must not exist already.
pub fn export_readable_selection(
    repository: &LibraryRepository,
    note_ids: &[NoteId],
    destination: impl AsRef<Path>,
) -> Result<ReadableExportReport, ReadableExportError> {
    export_readable_selection_with_hooks(repository, note_ids, destination, || {}, || {})
}

fn export_readable_selection_with_hooks(
    repository: &LibraryRepository,
    note_ids: &[NoteId],
    destination: impl AsRef<Path>,
    after_first_snapshot_read: impl FnOnce(),
    before_blob_copy: impl FnOnce(),
) -> Result<ReadableExportReport, ReadableExportError> {
    if note_ids.is_empty() {
        return Err(ReadableExportError::NoNotes);
    }
    if note_ids.len() > MAX_BUNDLE_NOTES {
        return Err(ReadableExportError::InvalidManifest(
            "note count exceeds restore limit".into(),
        ));
    }
    let destination = destination.as_ref();
    if destination.exists() {
        return Err(ReadableExportError::TargetExists);
    }
    let parent = destination.parent().ok_or_else(|| {
        ReadableExportError::InvalidManifest("export destination has no parent".into())
    })?;
    fs::create_dir_all(parent)?;
    let manifest = repository.with_readable_export_snapshot(|connection| {
        let default = readable_export_query_default_notebook(connection)?;
        after_first_snapshot_read();
        if default.has_stack {
            return Err(ReadableExportError::UnsupportedOrganization(
                "default notebook belongs to a stack".into(),
            ));
        }
        let mut manifest_minimum_bytes = 0;
        ensure_manifest_minimum_within_limit(
            &mut manifest_minimum_bytes,
            checked_string_bytes([
                "app-lite-readable-export",
                default.id.as_str(),
                default.title.as_str(),
            ])
            .ok_or_else(|| ReadableExportError::InvalidManifest("manifest size overflow".into()))?,
        )?;
        let mut seen_notes = BTreeSet::new();
        let mut notes = Vec::with_capacity(note_ids.len());
        let mut resources = BTreeMap::<String, ManifestResource>::new();

        for note_id in note_ids {
            if !seen_notes.insert(note_id.as_str().to_owned()) {
                return Err(ReadableExportError::DuplicateNoteId(
                    note_id.as_str().into(),
                ));
            }
            let note = readable_export_query_note(connection, note_id)?.ok_or_else(|| {
                ReadableExportError::InvalidManifest(format!("note missing: {note_id:?}"))
            })?;
            if note.deleted_time.is_some()
                || note.notebook_id != default.id
                || !note.tag_ids.is_empty()
            {
                return Err(ReadableExportError::UnsupportedOrganization(
                    note.id.as_str().into(),
                ));
            }
            let state = readable_export_query_note_state(
                connection,
                note_id,
                MAX_NOTE_REVISIONS,
                MAX_NOTE_HISTORY_BYTES,
            )?
            .ok_or_else(|| {
                ReadableExportError::InvalidManifest(format!("note state missing: {note_id:?}"))
            })?;
            if state.revisions.is_empty() || state.revisions.len() > MAX_NOTE_REVISIONS {
                return Err(ReadableExportError::InvalidManifest(
                    "invalid revision count".into(),
                ));
            }
            if note.body_html.len() as u64 > MAX_NOTE_HTML_BYTES
                || note.body_text.len() > MAX_NOTE_TEXT_BYTES
            {
                return Err(ReadableExportError::InvalidManifest(
                    "current note exceeds restore byte limits".into(),
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
            let current_document =
                CanonicalDocument::parse_html(&note.body_html).map_err(|_| {
                    ReadableExportError::InvalidManifest("current note HTML is invalid".into())
                })?;
            if current_document.to_canonical_html().as_str() != note.body_html
                || current_document.search_text().as_str() != note.body_text
                || current_document
                    .resource_ids()
                    .iter()
                    .map(ResourceId::as_str)
                    .collect::<Vec<_>>()
                    != note
                        .resource_ids
                        .iter()
                        .map(ResourceId::as_str)
                        .collect::<Vec<_>>()
            {
                return Err(ReadableExportError::InvalidManifest(
                    "current note HTML/resources differ".into(),
                ));
            }
            let html_path = expected_html_path(note.id.as_str());
            let mut referenced_resource_ids = current_document
                .resource_ids()
                .into_iter()
                .map(|id| id.as_str().to_owned())
                .collect::<BTreeSet<_>>();
            for revision in &state.revisions {
                if revision.body_html.len() as u64 > MAX_NOTE_HTML_BYTES
                    || revision.body_text.len() > MAX_NOTE_TEXT_BYTES
                {
                    return Err(ReadableExportError::InvalidManifest(
                        "revision exceeds restore byte limits".into(),
                    ));
                }
                let document =
                    CanonicalDocument::parse_html(&revision.body_html).map_err(|_| {
                        ReadableExportError::InvalidManifest("revision HTML is invalid".into())
                    })?;
                if document.to_canonical_html().as_str() != revision.body_html
                    || document.search_text().as_str() != revision.body_text
                {
                    return Err(ReadableExportError::InvalidManifest(
                        "revision HTML/resources differ".into(),
                    ));
                }
                referenced_resource_ids.extend(
                    document
                        .resource_ids()
                        .into_iter()
                        .map(|id| id.as_str().to_owned()),
                );
            }
            for resource_id in referenced_resource_ids {
                if resources.contains_key(&resource_id) {
                    continue;
                }
                if resources.len() >= MAX_BUNDLE_RESOURCES {
                    return Err(ReadableExportError::InvalidManifest(
                        "resource count exceeds restore limit".into(),
                    ));
                }
                let resource_id = ResourceId::new(resource_id).map_err(|_| {
                    ReadableExportError::InvalidManifest("invalid referenced resource ID".into())
                })?;
                let resource = readable_export_query_resource(connection, &resource_id)?
                    .ok_or_else(|| {
                        ReadableExportError::ResourceVerification(format!(
                            "metadata missing: {}",
                            resource_id.as_str()
                        ))
                    })?;
                if resource.size < 0 || resource.size as u64 > crate::MAX_RESOURCE_BYTES as u64 {
                    return Err(ReadableExportError::InvalidManifest(
                        "resource size exceeds restore limit".into(),
                    ));
                }
                let key = resource_id.as_str().to_owned();
                let relative_path = format!(
                    "resources/{}--{}--{}",
                    resource.sha256.as_str(),
                    resource.id.as_str(),
                    safe_display_name(&resource.title, &resource.file_extension)
                );
                let resource_string_bytes = checked_string_bytes([
                    resource.id.as_str(),
                    resource.sha256.as_str(),
                    resource.title.as_str(),
                    resource.mime.as_str(),
                    resource.file_extension.as_str(),
                    relative_path.as_str(),
                ])
                .ok_or_else(|| {
                    ReadableExportError::InvalidManifest("manifest size overflow".into())
                })?;
                ensure_manifest_minimum_within_limit(
                    &mut manifest_minimum_bytes,
                    resource_string_bytes,
                )?;
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
                        created_time: resource.created_time,
                        updated_time: resource.updated_time,
                        relative_path,
                    },
                );
            }
            let bundle_note = ManifestNote {
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
            };
            ensure_manifest_minimum_within_limit(
                &mut manifest_minimum_bytes,
                manifest_note_string_bytes(&bundle_note).ok_or_else(|| {
                    ReadableExportError::InvalidManifest("manifest size overflow".into())
                })?,
            )?;
            notes.push(bundle_note);
        }
        Ok(BundleManifest {
            format: "app-lite-readable-export".into(),
            version: BUNDLE_VERSION,
            default_notebook: ManifestNotebook {
                id: default.id.as_str().into(),
                title: default.title,
                revision: default.revision,
                created_time: default.created_time,
                updated_time: default.updated_time,
            },
            notes,
            resources: resources.into_values().collect(),
        })
    })?;
    // The SQLite transaction and Rust mutex have ended. Blob I/O uses only
    // hashes captured in the manifest and cannot refresh metadata mid-export.
    let temporary = Builder::new()
        .prefix("readable-export-")
        .tempdir_in(parent)?;
    fs::create_dir(temporary.path().join("notes"))?;
    fs::create_dir(temporary.path().join("resources"))?;
    for note in &manifest.notes {
        let current = note.revisions.last().expect("snapshot checked non-empty");
        write_synced(
            &temporary.path().join(&note.html_path),
            current.body_html.as_bytes(),
        )?;
    }
    before_blob_copy();
    for resource in &manifest.resources {
        let hash = BlobHash::new(&resource.sha256).map_err(|_| {
            ReadableExportError::InvalidManifest("invalid captured resource hash".into())
        })?;
        let captured_size = usize::try_from(resource.size).map_err(|_| {
            ReadableExportError::InvalidManifest("invalid captured resource size".into())
        })?;
        let mut input = repository.open_readable_export_blob(&hash, captured_size)?;
        let actual = copy_and_hash(
            &mut input,
            &temporary.path().join(&resource.relative_path),
            resource.size as u64,
        )?;
        if actual.0 != resource.sha256 || actual.1 != resource.size as u64 {
            return Err(ReadableExportError::ResourceVerification(format!(
                "copied bytes differ: {}",
                resource.id
            )));
        }
    }
    let mut manifest_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temporary.path().join("manifest.json"))?;
    let mut buffered_manifest = io::BufWriter::with_capacity(COPY_BUFFER_BYTES, &mut manifest_file);
    write_manifest_bounded(&manifest, &mut buffered_manifest, MAX_MANIFEST_BYTES)?;
    buffered_manifest.flush()?;
    drop(buffered_manifest);
    manifest_file.sync_all()?;
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
    restore_validated_bundle(validated, empty_profile)
}

fn restore_validated_bundle(
    validated: ValidatedBundle,
    empty_profile: &Path,
) -> Result<ReadableRestoreReport, ReadableExportError> {
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
        let source = open_child_regular(&validated.dirs.resources, &resource_file_name(resource))?;
        let generated = repository.import_resource_reader(
            source.take(resource.size as u64 + 1),
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
        "UPDATE notebooks SET id=?1, title=?2, revision=?3, created_time=?4, updated_time=?5
         WHERE id=?6 AND is_default=1",
        params![
            validated.manifest.default_notebook.id,
            validated.manifest.default_notebook.title,
            validated.manifest.default_notebook.revision,
            validated.manifest.default_notebook.created_time,
            validated.manifest.default_notebook.updated_time,
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
            "UPDATE resources SET id=?1, revision=?2, created_time=?3, updated_time=?4 WHERE id=?5",
            params![
                resource.id,
                resource.revision,
                resource.created_time,
                resource.updated_time,
                generated_id.as_str()
            ],
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
    let dirs = BundleDirs::open(bundle)?;
    let manifest_bytes = read_regular_file_bounded(
        open_child_regular(&dirs._root, "manifest.json")?,
        MAX_MANIFEST_BYTES,
    )?;
    #[derive(Deserialize)]
    struct BundleHeader {
        format: String,
        version: u32,
    }
    let header: BundleHeader = serde_json::from_slice(&manifest_bytes)?;
    if header.format != "app-lite-readable-export" {
        return Err(ReadableExportError::InvalidManifest(
            "unsupported bundle format".into(),
        ));
    }
    if header.version != BUNDLE_VERSION {
        return Err(ReadableExportError::InvalidManifest(format!(
            "incompatible bundle version {}; expected {}",
            header.version, BUNDLE_VERSION
        )));
    }
    let manifest: BundleManifest = serde_json::from_slice(&manifest_bytes)?;
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
    for resource in &manifest.resources {
        let id = ResourceId::new(&resource.id)
            .map_err(|_| ReadableExportError::InvalidManifest("invalid resource ID".into()))?;
        let hash = BlobHash::new(&resource.sha256)
            .map_err(|_| ReadableExportError::InvalidManifest("invalid resource SHA-256".into()))?;
        if resource.size < 0
            || resource.size as u64 > crate::MAX_RESOURCE_BYTES as u64
            || !resources.insert(id.as_str().to_owned(), resource).is_none()
        {
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
        let (actual_hash, actual_size) = hash_regular_file_bounded(
            open_child_regular(&dirs.resources, &resource_file_name(resource))?,
            resource.size as u64,
        )?;
        if actual_hash != hash.as_str() || actual_size != resource.size as u64 {
            return Err(ReadableExportError::ResourceVerification(format!(
                "{}",
                resource.id
            )));
        }
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
            open_child_regular(&dirs.notes, &format!("{}.html", note.id))?,
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
                || revision.body_html.len() as u64 > MAX_NOTE_HTML_BYTES
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
            if document
                .resource_ids()
                .iter()
                .any(|id| !resources.contains_key(id.as_str()))
            {
                return Err(ReadableExportError::InvalidManifest(
                    "revision references a resource missing from the bundle".into(),
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
        dirs,
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
    repository.with_readable_export_snapshot(|connection| {
        let notebook = readable_export_query_default_notebook(connection)?;
        let expected = &validated.manifest.default_notebook;
        if notebook.id.as_str() != expected.id
            || notebook.title != expected.title
            || notebook.revision != expected.revision
            || notebook.created_time != expected.created_time
            || notebook.updated_time != expected.updated_time
            || notebook.has_stack
        {
            return Err(ReadableExportError::InvalidManifest(
                "restored default notebook metadata differs".into(),
            ));
        }
        for expected in &validated.manifest.resources {
            let id = ResourceId::new(&expected.id).map_err(|_| {
                ReadableExportError::InvalidManifest("invalid restored resource ID".into())
            })?;
            let actual = readable_export_query_resource(connection, &id)?.ok_or_else(|| {
                ReadableExportError::InvalidManifest("restored resource missing".into())
            })?;
            if actual.revision != expected.revision
                || actual.created_time != expected.created_time
                || actual.updated_time != expected.updated_time
            {
                return Err(ReadableExportError::InvalidManifest(
                    "restored resource metadata differs".into(),
                ));
            }
        }
        Ok(())
    })?;
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

impl BundleDirs {
    fn open(bundle: &Path) -> Result<Self, ReadableExportError> {
        // A trailing slash or `/.` makes the kernel resolve a symlink before
        // applying O_NOFOLLOW to the final component. Strip only those suffixes;
        // normal parent components retain their ordinary filesystem meaning.
        let mut path_bytes = bundle.as_os_str().as_bytes();
        loop {
            if path_bytes.ends_with(b"/") {
                path_bytes = &path_bytes[..path_bytes.len() - 1];
            } else if path_bytes.ends_with(b"/.") {
                path_bytes = &path_bytes[..path_bytes.len() - 2];
            } else {
                break;
            }
        }
        if path_bytes.is_empty() {
            return Err(ReadableExportError::InvalidManifest(
                "invalid bundle path".into(),
            ));
        }
        let path = std::ffi::CString::new(path_bytes)
            .map_err(|_| ReadableExportError::InvalidManifest("invalid bundle path".into()))?;
        let root_fd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if root_fd < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let root = unsafe { File::from_raw_fd(root_fd) };
        let notes = open_child_directory(&root, "notes")?;
        let resources = open_child_directory(&root, "resources")?;
        Ok(Self {
            _root: root,
            notes,
            resources,
        })
    }
}

fn expected_html_path(note_id: &str) -> String {
    format!("notes/{note_id}.html")
}

fn expected_resource_path(resource: &ManifestResource) -> String {
    format!("resources/{}", resource_file_name(resource))
}

fn resource_file_name(resource: &ManifestResource) -> String {
    format!(
        "{}--{}--{}",
        resource.sha256,
        resource.id,
        safe_display_name(&resource.title, &resource.file_extension)
    )
}

fn open_child_directory(parent: &File, name: &str) -> Result<File, ReadableExportError> {
    open_child(parent, name, libc::O_DIRECTORY)
}

fn open_child_regular(parent: &File, name: &str) -> Result<File, ReadableExportError> {
    let file = open_child(parent, name, libc::O_NONBLOCK)?;
    if !file.metadata()?.is_file() {
        return Err(ReadableExportError::InvalidManifest(
            "bundle entry is not a regular file".into(),
        ));
    }
    Ok(file)
}

fn open_child(parent: &File, name: &str, extra_flags: i32) -> Result<File, ReadableExportError> {
    if name.is_empty() || name == "." || name == ".." || name.as_bytes().contains(&b'/') {
        return Err(ReadableExportError::InvalidManifest(
            "bundle entry is not one path component".into(),
        ));
    }
    let name = std::ffi::CString::new(name)
        .map_err(|_| ReadableExportError::InvalidManifest("invalid bundle entry".into()))?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | extra_flags,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn read_regular_file_bounded(file: File, maximum: u64) -> Result<Vec<u8>, ReadableExportError> {
    let expected_size = file.metadata()?.len();
    read_bounded_stream(file, expected_size, maximum)
}

fn read_bounded_stream(
    input: impl Read,
    expected_size: u64,
    maximum: u64,
) -> Result<Vec<u8>, ReadableExportError> {
    if expected_size > maximum {
        return Err(ReadableExportError::InvalidManifest(
            "bundle file exceeds bound".into(),
        ));
    }
    let mut bytes = Vec::with_capacity(expected_size as usize);
    input
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != expected_size || bytes.len() as u64 > maximum {
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

fn copy_and_hash(
    input: &mut dyn Read,
    destination: &Path,
    captured_size: u64,
) -> io::Result<(String, u64)> {
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    let mut limited = input.take(captured_size.checked_add(1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "captured resource size overflow",
        )
    })?);
    loop {
        let count = limited.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size += count as u64;
        if size > captured_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "resource exceeds captured size while copying",
            ));
        }
        output.write_all(&buffer[..count])?;
        digest.update(&buffer[..count]);
    }
    output.sync_all()?;
    Ok((format!("{:x}", digest.finalize()), size))
}

fn hash_regular_file_bounded(
    file: File,
    maximum: u64,
) -> Result<(String, u64), ReadableExportError> {
    if file.metadata()?.len() > maximum {
        return Err(ReadableExportError::InvalidManifest(
            "bundle resource exceeds expected size".into(),
        ));
    }
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    let mut input = file.take(maximum + 1);
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size += count as u64;
        if size > maximum {
            return Err(ReadableExportError::InvalidManifest(
                "bundle resource grew beyond expected size".into(),
            ));
        }
        digest.update(&buffer[..count]);
    }
    Ok((format!("{:x}", digest.finalize()), size))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn snippet(text: &str) -> String {
    text.chars().take(280).collect()
}

struct BoundedWriter<'a, W> {
    inner: &'a mut W,
    written: u64,
    maximum: u64,
}

fn checked_string_bytes<'a>(strings: impl IntoIterator<Item = &'a str>) -> Option<usize> {
    strings
        .into_iter()
        .try_fold(0_usize, |total, value| total.checked_add(value.len()))
}

fn manifest_note_string_bytes(note: &ManifestNote) -> Option<usize> {
    let mut bytes = checked_string_bytes([
        note.id.as_str(),
        note.title.as_str(),
        note.html_path.as_str(),
        note.body_html_sha256.as_str(),
        note.body_text.as_str(),
    ])?;
    for value in note
        .resource_ids
        .iter()
        .map(String::as_str)
        .chain(note.selected_thumbnail_id.iter().map(String::as_str))
    {
        bytes = bytes.checked_add(value.len())?;
    }
    for revision in &note.revisions {
        let revision_bytes = checked_string_bytes([
            revision.title.as_str(),
            revision.body_html.as_str(),
            revision.body_html_sha256.as_str(),
            revision.body_text.as_str(),
        ])?;
        bytes = bytes.checked_add(revision_bytes)?;
    }
    Some(bytes)
}

fn ensure_manifest_minimum_within_limit(
    total: &mut usize,
    addition: usize,
) -> Result<(), ReadableExportError> {
    let next = total
        .checked_add(addition)
        .ok_or_else(|| ReadableExportError::InvalidManifest("manifest size overflow".into()))?;
    if next as u64 > MAX_MANIFEST_BYTES {
        return Err(ReadableExportError::InvalidManifest(
            "manifest content exceeds restore limit".into(),
        ));
    }
    *total = next;
    Ok(())
}

impl<W: Write> Write for BoundedWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = self
            .written
            .checked_add(bytes.len() as u64)
            .filter(|size| *size <= self.maximum)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "manifest exceeds limit"))?;
        let written = self.inner.write(bytes)?;
        self.written = self
            .written
            .checked_add(written as u64)
            .filter(|size| *size <= self.maximum)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "manifest exceeds limit"))?;
        debug_assert!(self.written <= next);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn write_manifest_bounded<W: Write>(
    manifest: &BundleManifest,
    writer: &mut W,
    maximum: u64,
) -> Result<(), serde_json::Error> {
    serde_json::to_writer_pretty(
        &mut BoundedWriter {
            inner: writer,
            written: 0,
            maximum,
        },
        manifest,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Block;
    use crate::{CreateNote, SaveNote};
    use std::sync::{Arc, Barrier, mpsc};
    use std::thread;
    use std::time::Duration;
    use tempfile::tempdir;

    fn manifest_with_text(text: &str) -> BundleManifest {
        let html = "<p></p>";
        BundleManifest {
            format: "app-lite-readable-export".into(),
            version: BUNDLE_VERSION,
            default_notebook: ManifestNotebook {
                id: "notebook-id".into(),
                title: "Default".into(),
                revision: 1,
                created_time: 1,
                updated_time: 1,
            },
            notes: vec![ManifestNote {
                id: "note-id".into(),
                title: "Note".into(),
                html_path: "notes/note-id.html".into(),
                body_html_sha256: sha256_hex(html.as_bytes()),
                body_text: text.into(),
                created_time: 1,
                updated_time: 1,
                revision: 1,
                selected_thumbnail_id: None,
                resource_ids: Vec::new(),
                revisions: vec![ManifestRevision {
                    revision: 1,
                    title: "Note".into(),
                    body_html: html.into(),
                    body_html_sha256: sha256_hex(html.as_bytes()),
                    body_text: text.into(),
                    created_time: 1,
                }],
            }],
            resources: Vec::new(),
        }
    }

    #[test]
    fn growing_reader_stops_after_declared_size_plus_one_byte() {
        struct EndlessReader {
            reads: usize,
        }
        impl Read for EndlessReader {
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                assert!(self.reads < 9, "read past declared size plus one byte");
                self.reads += 1;
                output[0] = b'x';
                Ok(1)
            }
        }
        let parent = tempdir().unwrap();
        let mut reader = EndlessReader { reads: 0 };
        assert!(copy_and_hash(&mut reader, &parent.path().join("copy"), 8).is_err());
        assert_eq!(reader.reads, 9);
    }

    #[test]
    fn snapshot_remains_coherent_when_another_connection_commits_after_first_read() {
        let profile = tempdir().unwrap();
        let database = profile.path().join("library.sqlite");
        let source = Arc::new(LibraryRepository::open(&database).unwrap());
        let writer = LibraryRepository::open(&database).unwrap();
        let original_default = source.default_notebook().unwrap();
        let before_notebook_title = original_default.title.clone();
        let resource = source
            .import_resource(
                b"snapshot bytes",
                "capture.bin",
                "application/octet-stream",
                "bin",
            )
            .unwrap();
        let original = source
            .create_note(CreateNote {
                title: "Before".into(),
                notebook_id: None,
                document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                    resource_id: resource.clone(),
                    filename: "capture.bin".into(),
                    media_type: "application/octet-stream".into(),
                }]),
            })
            .unwrap();
        let entered = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let worker_entered = Arc::clone(&entered);
        let worker_resume = Arc::clone(&resume);
        let note_id = original.id.clone();
        let worker = thread::spawn(move || {
            worker_entered.wait();
            writer
                .rename_notebook(&original_default.id, "After")
                .unwrap();
            writer
                .save_note(SaveNote {
                    id: note_id,
                    expected_revision: original.revision,
                    title: "After".into(),
                    document: CanonicalDocument::from_blocks(vec![Block::Paragraph {
                        style: Default::default(),
                        inlines: vec![crate::document::Inline::Text {
                            text: "changed".into(),
                            marks: Default::default(),
                        }],
                    }]),
                    resource_ids: Vec::new(),
                    selected_thumbnail_id: None,
                })
                .unwrap();
            worker_resume.wait();
        });
        let bundle = profile.path().join("bundle");
        let same_handle = Arc::clone(&source);
        export_readable_selection_with_hooks(
            &source,
            &[original.id.clone()],
            &bundle,
            || {
                entered.wait();
                resume.wait();
            },
            move || {
                let (sent, received) = mpsc::channel();
                thread::spawn(move || {
                    sent.send(same_handle.write_setting("snapshot-lock-release", "yes"))
                        .unwrap();
                });
                received
                    .recv_timeout(Duration::from_secs(2))
                    .expect("resource copy must not hold the repository mutex")
                    .unwrap();
            },
        )
        .unwrap();
        worker.join().unwrap();
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(bundle.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["default_notebook"]["title"], before_notebook_title);
        assert_eq!(manifest["notes"][0]["title"], "Before");
        assert_eq!(manifest["notes"][0]["revision"], 1);
        assert_eq!(
            manifest["notes"][0]["revisions"].as_array().unwrap().len(),
            1
        );
        assert_eq!(manifest["resources"][0]["id"], resource.as_str());
    }

    #[test]
    fn failed_snapshot_releases_transaction_and_source_lock() {
        let profile = tempdir().unwrap();
        let source = LibraryRepository::open(profile.path().join("library.sqlite")).unwrap();
        let note = source
            .create_note(CreateNote {
                title: "Still writable".into(),
                notebook_id: None,
                document: CanonicalDocument::from_blocks(vec![Block::Paragraph {
                    style: Default::default(),
                    inlines: vec![crate::document::Inline::Text {
                        text: "before".into(),
                        marks: Default::default(),
                    }],
                }]),
            })
            .unwrap();
        let missing = NoteId::parse("ffffffffffffffffffffffffffffffff").unwrap();
        let bundle = profile.path().join("must-not-publish");
        assert!(export_readable_selection(&source, &[note.id.clone(), missing], &bundle).is_err());
        assert!(!bundle.exists());
        let changed = source
            .save_note(SaveNote {
                id: note.id,
                expected_revision: note.revision,
                title: "Still writable".into(),
                document: CanonicalDocument::from_blocks(vec![Block::Paragraph {
                    style: Default::default(),
                    inlines: vec![crate::document::Inline::Text {
                        text: "after".into(),
                        marks: Default::default(),
                    }],
                }]),
                resource_ids: Vec::new(),
                selected_thumbnail_id: None,
            })
            .unwrap();
        assert_eq!(changed.revision, 2);
    }

    #[test]
    fn bounded_manifest_writer_accepts_exact_limit_and_rejects_one_byte_less() {
        let manifest = manifest_with_text("bounded");
        let expected = serde_json::to_vec_pretty(&manifest).unwrap();
        let mut exact = Vec::new();
        write_manifest_bounded(&manifest, &mut exact, expected.len() as u64).unwrap();
        assert_eq!(exact, expected);

        let mut too_small = Vec::new();
        assert!(
            write_manifest_bounded(&manifest, &mut too_small, expected.len() as u64 - 1).is_err()
        );
        assert!(too_small.len() <= expected.len() - 1);
    }

    #[test]
    fn bounded_manifest_writer_counts_json_escape_expansion() {
        let plain = manifest_with_text(&"a".repeat(64));
        let escaped = manifest_with_text(&"\"".repeat(64));
        let plain_size = serde_json::to_vec_pretty(&plain).unwrap().len();
        let escaped_size = serde_json::to_vec_pretty(&escaped).unwrap().len();
        assert!(escaped_size > plain_size);

        let mut output = Vec::new();
        assert!(write_manifest_bounded(&escaped, &mut output, plain_size as u64).is_err());
        assert!(output.len() <= plain_size);
    }

    #[test]
    fn manifest_string_budget_accepts_exact_limit_and_rejects_overflow() {
        let mut exact = MAX_MANIFEST_BYTES as usize - 1;
        ensure_manifest_minimum_within_limit(&mut exact, 1).unwrap();
        assert_eq!(exact as u64, MAX_MANIFEST_BYTES);
        assert!(ensure_manifest_minimum_within_limit(&mut exact, 1).is_err());

        let mut arithmetic_overflow = usize::MAX;
        assert!(ensure_manifest_minimum_within_limit(&mut arithmetic_overflow, 1).is_err());
    }

    #[test]
    fn growing_stream_stops_after_one_byte_over_limit() {
        assert!(read_bounded_stream(io::repeat(b'x'), 1, 8).is_err());
    }

    #[test]
    fn pinned_bundle_directories_ignore_replaced_pathnames() {
        let parent = tempdir().unwrap();
        let bundle = parent.path().join("bundle");
        fs::create_dir(&bundle).unwrap();
        fs::create_dir(bundle.join("notes")).unwrap();
        fs::create_dir(bundle.join("resources")).unwrap();
        fs::write(bundle.join("notes/note.html"), b"original").unwrap();
        let dirs = BundleDirs::open(&bundle).unwrap();

        let moved = parent.path().join("moved");
        fs::rename(&bundle, &moved).unwrap();
        fs::create_dir(&bundle).unwrap();
        fs::create_dir(bundle.join("notes")).unwrap();
        fs::write(bundle.join("notes/note.html"), b"replacement").unwrap();

        let file = open_child_regular(&dirs.notes, "note.html").unwrap();
        assert_eq!(read_bounded_stream(file, 8, 16).unwrap(), b"original");
    }

    #[test]
    fn resource_rewrite_after_validation_fails_without_publishing_profile() {
        let source_profile = tempdir().unwrap();
        let source = LibraryRepository::open(source_profile.path().join("library.sqlite")).unwrap();
        let resource_id = source
            .import_resource(
                b"original bytes",
                "file.bin",
                "application/octet-stream",
                "bin",
            )
            .unwrap();
        let note = source
            .create_note(CreateNote {
                title: "with resource".into(),
                notebook_id: None,
                document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                    resource_id,
                    filename: "file.bin".into(),
                    media_type: "application/octet-stream".into(),
                }]),
            })
            .unwrap();
        let parent = tempdir().unwrap();
        let bundle = parent.path().join("bundle");
        export_readable_selection(&source, &[note.id], &bundle).unwrap();
        let validated = validate_manifest(&bundle).unwrap();
        let resource_path = &validated.manifest.resources[0].relative_path;
        fs::write(bundle.join(resource_path), b"modified bytes").unwrap();

        let target = parent.path().join("target");
        fs::create_dir(&target).unwrap();
        assert!(restore_validated_bundle(validated, &target).is_err());
        assert!(fs::read_dir(&target).unwrap().next().is_none());
    }
}
