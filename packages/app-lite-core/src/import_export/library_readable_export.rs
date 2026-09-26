//! Whole-library readable export: every stack, notebook, tag (with order),
//! note including trash, retained history and resource (including ones no
//! note references) as UTF-8 HTML/JSON plus original resource bytes, and a
//! restore of that bundle into a new, empty profile.
//!
//! Unlike `library_backup` (a SQLite snapshot) the bundle needs no SQLite to
//! read: `index.html` and `readable/` are browsable pages, `manifest.json`,
//! `notes/` and `history/` are the authoritative data. Sync identity, pending
//! outbox, edit journal and machine-local view settings are not exported.
//! Evernote's `11354__enex-exporter.js` skips an attachment it cannot read;
//! here any missing or changed byte aborts the export or restore.

use super::readable_export::{
    COPY_BUFFER_BYTES, MAX_BUNDLE_NOTES, MAX_BUNDLE_RESOURCES, MAX_NOTE_HTML_BYTES,
    MAX_NOTE_REVISIONS, ReadableExportError, copy_and_hash, ensure_existing_empty_directory,
    hash_regular_file_bounded, open_bundle_root, open_child_directory, open_child_regular,
    read_regular_file_bounded, safe_display_name, sha256_hex, snippet, write_html_escaped,
    write_readable_page, write_synced,
};
use crate::{
    BlobHash, CanonicalDocument, LibraryRepository, NoteId, NotebookId, ResourceId, StackId, TagId,
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use tempfile::Builder;

const FORMAT: &str = "app-lite-library-readable-export";
const VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;
const MAX_HISTORY_FILE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryReadableReport {
    pub note_count: usize,
    pub resource_count: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    format: String,
    version: u32,
    stacks: Vec<Container>,
    notebooks: Vec<Notebook>,
    tags: Vec<Container>,
    notes: Vec<Note>,
    resources: Vec<Resource>,
    shortcuts: Vec<Shortcut>,
}

// Stacks and tags share this shape.
#[derive(Debug, Serialize, Deserialize)]
struct Container {
    id: String,
    title: String,
    revision: i64,
    created_time: i64,
    updated_time: i64,
    deleted_time: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Notebook {
    id: String,
    title: String,
    stack_id: Option<String>,
    is_default: bool,
    revision: i64,
    created_time: i64,
    updated_time: i64,
    deleted_time: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Note {
    id: String,
    title: String,
    notebook_id: String,
    tag_ids: Vec<String>,
    created_time: i64,
    updated_time: i64,
    deleted_time: i64,
    revision: i64,
    selected_thumbnail_id: Option<String>,
    resource_ids: Vec<String>,
    body_html_sha256: String,
    history_sha256: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Revision {
    revision: i64,
    title: String,
    body_html: String,
    created_time: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Resource {
    id: String,
    sha256: String,
    size: i64,
    title: String,
    mime: String,
    file_extension: String,
    revision: i64,
    created_time: i64,
    updated_time: i64,
    deleted_time: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Shortcut {
    id: String,
    entity_type: String,
    entity_id: String,
    position: i64,
    created_time: i64,
}

struct Snapshot {
    manifest: Manifest,
    bodies: BTreeMap<String, String>,
    histories: BTreeMap<String, Vec<Revision>>,
}

fn invalid(reason: &str) -> ReadableExportError {
    ReadableExportError::InvalidManifest(reason.into())
}

fn resource_file_name(resource: &Resource) -> String {
    format!(
        "{}--{}--{}",
        resource.sha256,
        resource.id,
        safe_display_name(&resource.title, &resource.file_extension)
    )
}

/// Exports the whole library into a new directory that must not exist yet.
pub fn export_library_readable(
    repository: &LibraryRepository,
    destination: impl AsRef<Path>,
) -> Result<LibraryReadableReport, ReadableExportError> {
    let destination = destination.as_ref();
    if destination.exists() {
        return Err(ReadableExportError::TargetExists);
    }
    let parent = destination
        .parent()
        .ok_or_else(|| invalid("export destination has no parent"))?;
    fs::create_dir_all(parent)?;
    let snapshot = repository.with_readable_export_snapshot(read_snapshot)?;

    let temporary = Builder::new()
        .prefix("library-readable-export-")
        .tempdir_in(parent)?;
    let root = temporary.path();
    for directory in ["notes", "history", "resources", "readable"] {
        fs::create_dir(root.join(directory))?;
    }
    for note in &snapshot.manifest.notes {
        write_synced(
            &root.join(format!("notes/{}.html", note.id)),
            snapshot.bodies[&note.id].as_bytes(),
        )?;
        let history = serde_json::to_vec(&snapshot.histories[&note.id])?;
        if history.len() as u64 > MAX_HISTORY_FILE_BYTES {
            return Err(invalid("note history exceeds restore limit"));
        }
        write_synced(&root.join(format!("history/{}.json", note.id)), &history)?;
    }
    for resource in &snapshot.manifest.resources {
        let hash = BlobHash::new(&resource.sha256).map_err(|_| invalid("invalid captured hash"))?;
        let size = usize::try_from(resource.size).map_err(|_| invalid("invalid size"))?;
        let mut input = repository.open_readable_export_blob(&hash, size)?;
        let (sha256, copied) = copy_and_hash(
            &mut input,
            &root.join("resources").join(resource_file_name(resource)),
            resource.size as u64,
        )?;
        if sha256 != resource.sha256 || copied != resource.size as u64 {
            return Err(ReadableExportError::ResourceVerification(
                resource.id.clone(),
            ));
        }
    }
    let resource_paths = snapshot
        .manifest
        .resources
        .iter()
        .map(|resource| {
            (
                resource.id.clone(),
                format!("../resources/{}", resource_file_name(resource)),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for note in &snapshot.manifest.notes {
        write_readable_page(
            &root.join(format!("readable/{}.html", note.id)),
            &note.title,
            &snapshot.bodies[&note.id],
            &resource_paths,
        )?;
    }
    write_index(&root.join("index.html"), &snapshot.manifest)?;
    let manifest = serde_json::to_vec(&snapshot.manifest)?;
    if manifest.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(invalid("manifest exceeds restore limit"));
    }
    write_synced(&root.join("manifest.json"), &manifest)?;
    fs::rename(root, destination)?;
    std::mem::forget(temporary);
    Ok(LibraryReadableReport {
        note_count: snapshot.manifest.notes.len(),
        resource_count: snapshot.manifest.resources.len(),
    })
}

fn read_snapshot(connection: &Connection) -> Result<Snapshot, ReadableExportError> {
    let pending: i64 =
        connection.query_row("SELECT count(*) FROM edit_journal", [], |row| row.get(0))?;
    if pending != 0 {
        return Err(invalid("unsaved edits must be flushed before export"));
    }
    let containers = |sql: &str| -> Result<Vec<Container>, ReadableExportError> {
        let mut statement = connection.prepare(sql)?;
        let rows = statement.query_map([], |row| {
            Ok(Container {
                id: row.get(0)?,
                title: row.get(1)?,
                revision: row.get(2)?,
                created_time: row.get(3)?,
                updated_time: row.get(4)?,
                deleted_time: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    };
    let stacks = containers(
        "SELECT id,title,revision,created_time,updated_time,deleted_time FROM stacks ORDER BY id",
    )?;
    let tags = containers(
        "SELECT id,title,revision,created_time,updated_time,deleted_time FROM tags ORDER BY id",
    )?;
    let notebooks = connection
        .prepare(
            "SELECT id,title,stack_id,is_default,revision,created_time,updated_time,deleted_time
             FROM notebooks ORDER BY id",
        )?
        .query_map([], |row| {
            Ok(Notebook {
                id: row.get(0)?,
                title: row.get(1)?,
                stack_id: row.get(2)?,
                is_default: row.get::<_, i64>(3)? != 0,
                revision: row.get(4)?,
                created_time: row.get(5)?,
                updated_time: row.get(6)?,
                deleted_time: row.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let resources = connection
        .prepare(
            "SELECT id,sha256,size,title,mime,file_extension,revision,created_time,updated_time,deleted_time
             FROM resources ORDER BY id",
        )?
        .query_map([], |row| {
            Ok(Resource {
                id: row.get(0)?,
                sha256: row.get(1)?,
                size: row.get(2)?,
                title: row.get(3)?,
                mime: row.get(4)?,
                file_extension: row.get(5)?,
                revision: row.get(6)?,
                created_time: row.get(7)?,
                updated_time: row.get(8)?,
                deleted_time: row.get(9)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let shortcuts = connection
        .prepare(
            "SELECT id,entity_type,entity_id,position,created_time FROM shortcuts ORDER BY id",
        )?
        .query_map([], |row| {
            Ok(Shortcut {
                id: row.get(0)?,
                entity_type: row.get(1)?,
                entity_id: row.get(2)?,
                position: row.get(3)?,
                created_time: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if resources.len() > MAX_BUNDLE_RESOURCES {
        return Err(invalid("resource count exceeds restore limit"));
    }

    let mut notes = Vec::new();
    let mut bodies = BTreeMap::new();
    let mut histories = BTreeMap::new();
    let mut statement = connection.prepare(
        "SELECT id,title,body_html,body_text,notebook_id,selected_thumbnail_id,created_time,updated_time,deleted_time,revision
         FROM notes ORDER BY id",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let title: String = row.get(1)?;
        let body_html: String = row.get(2)?;
        let body_text: String = row.get(3)?;
        let revision: i64 = row.get(9)?;
        if notes.len() >= MAX_BUNDLE_NOTES || body_html.len() as u64 > MAX_NOTE_HTML_BYTES {
            return Err(invalid("note exceeds restore limits"));
        }
        let document =
            CanonicalDocument::parse_html(&body_html).map_err(|_| invalid("note HTML invalid"))?;
        if document.to_canonical_html().as_str() != body_html
            || document.search_text().as_str() != body_text
        {
            return Err(invalid("note HTML is not canonical"));
        }
        let resource_ids = connection
            .prepare(
                "SELECT resource_id FROM note_resources WHERE note_id=?1 AND is_associated=1 ORDER BY position",
            )?
            .query_map([&id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if document
            .resource_ids()
            .iter()
            .map(ResourceId::as_str)
            .ne(resource_ids.iter().map(String::as_str))
        {
            return Err(invalid("note HTML and resource relations differ"));
        }
        let tag_ids = connection
            .prepare("SELECT tag_id FROM note_tags WHERE note_id=?1 ORDER BY position, tag_id")?
            .query_map([&id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let history = connection
            .prepare(
                "SELECT revision,title,body_html,body_text,created_time FROM note_revisions
                 WHERE note_id=?1 ORDER BY revision",
            )?
            .query_map([&id], |row| {
                Ok((
                    Revision {
                        revision: row.get(0)?,
                        title: row.get(1)?,
                        body_html: row.get(2)?,
                        created_time: row.get(4)?,
                    },
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if history.is_empty() || history.len() > MAX_NOTE_REVISIONS {
            return Err(invalid("invalid revision count"));
        }
        for (revision, text) in &history {
            let document = CanonicalDocument::parse_html(&revision.body_html)
                .map_err(|_| invalid("revision HTML invalid"))?;
            if document.to_canonical_html().as_str() != revision.body_html
                || document.search_text().as_str() != text
            {
                return Err(invalid("revision HTML is not canonical"));
            }
        }
        let last = &history.last().expect("non-empty").0;
        // Moves, tags and trash bump notes.revision without a history row.
        if last.revision > revision
            || last.title != title
            || last.body_html != body_html
            || !history
                .windows(2)
                .all(|pair| pair[0].0.revision < pair[1].0.revision)
        {
            return Err(invalid("note revision is not current"));
        }
        let history: Vec<Revision> = history.into_iter().map(|(revision, _)| revision).collect();
        let history_sha256 = sha256_hex(&serde_json::to_vec(&history)?);
        notes.push(Note {
            id: id.clone(),
            title,
            notebook_id: row.get(4)?,
            tag_ids,
            created_time: row.get(6)?,
            updated_time: row.get(7)?,
            deleted_time: row.get(8)?,
            revision,
            selected_thumbnail_id: row.get(5)?,
            resource_ids,
            body_html_sha256: sha256_hex(body_html.as_bytes()),
            history_sha256,
        });
        bodies.insert(id.clone(), body_html);
        histories.insert(id, history);
    }
    Ok(Snapshot {
        manifest: Manifest {
            format: FORMAT.into(),
            version: VERSION,
            stacks,
            notebooks,
            tags,
            notes,
            resources,
            shortcuts,
        },
        bodies,
        histories,
    })
}

fn write_index(path: &Path, manifest: &Manifest) -> Result<(), ReadableExportError> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let mut output = io::BufWriter::with_capacity(COPY_BUFFER_BYTES, &mut file);
    output.write_all(b"<!doctype html>\n<html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>\xe8\xb5\x84\xe6\x96\x99\xe5\xba\x93</title><style>body{font:16px/1.6 system-ui,sans-serif;max-width:48rem;margin:2rem auto;padding:0 1rem}li{margin:.3rem 0}small{color:#666}</style></head><body><h1>\xe8\xb5\x84\xe6\x96\x99\xe5\xba\x93</h1>")?;
    let tags = manifest
        .tags
        .iter()
        .map(|tag| (tag.id.as_str(), tag.title.as_str()))
        .collect::<BTreeMap<_, _>>();
    let write_notes = |output: &mut dyn Write, notes: &[&Note]| -> io::Result<()> {
        output.write_all(b"<ul>")?;
        for note in notes {
            write!(output, "<li><a href=\"readable/{}.html\">", note.id)?;
            write_html_escaped(output, &note.title, true)?;
            output.write_all(b"</a>")?;
            for tag in &note.tag_ids {
                output.write_all(b" <small>#")?;
                write_html_escaped(output, tags.get(tag.as_str()).copied().unwrap_or(""), true)?;
                output.write_all(b"</small>")?;
            }
            output.write_all(b"</li>\n")?;
        }
        output.write_all(b"</ul>")
    };
    let active_in = |notebook: &str| {
        manifest
            .notes
            .iter()
            .filter(|note| note.deleted_time == 0 && note.notebook_id == notebook)
            .collect::<Vec<_>>()
    };
    let mut sections = manifest
        .stacks
        .iter()
        .map(|stack| (Some(stack), stack.title.as_str()))
        .collect::<Vec<_>>();
    sections.push((None, ""));
    for (stack, title) in sections {
        let notebooks = manifest
            .notebooks
            .iter()
            .filter(|notebook| notebook.stack_id.as_deref() == stack.map(|stack| stack.id.as_str()))
            .collect::<Vec<_>>();
        if notebooks.is_empty() {
            continue;
        }
        if stack.is_some() {
            output.write_all(b"<h2>")?;
            write_html_escaped(&mut output, title, true)?;
            output.write_all(b"</h2>")?;
        }
        for notebook in notebooks {
            output.write_all(b"<h3>")?;
            write_html_escaped(&mut output, &notebook.title, true)?;
            output.write_all(b"</h3>")?;
            write_notes(&mut output, &active_in(&notebook.id))?;
        }
    }
    let trash = manifest
        .notes
        .iter()
        .filter(|note| note.deleted_time != 0)
        .collect::<Vec<_>>();
    if !trash.is_empty() {
        output.write_all("<h2>回收站</h2>".as_bytes())?;
        write_notes(&mut output, &trash)?;
    }
    output.write_all(b"</body></html>\n")?;
    output.flush()?;
    drop(output);
    file.sync_all()?;
    Ok(())
}

struct Validated {
    manifest: Manifest,
    bodies: BTreeMap<String, String>,
    histories: BTreeMap<String, Vec<Revision>>,
    resources_dir: File,
}

/// Verifies the whole bundle, rebuilds it in a sibling staging profile, checks
/// the result and only then moves it into `empty_profile`.
pub fn restore_library_readable(
    bundle: impl AsRef<Path>,
    empty_profile: impl AsRef<Path>,
) -> Result<LibraryReadableReport, ReadableExportError> {
    let empty_profile = empty_profile.as_ref();
    ensure_existing_empty_directory(empty_profile)?;
    let validated = validate(bundle.as_ref())?;
    let parent = empty_profile
        .parent()
        .ok_or(ReadableExportError::RestoreDestinationNotEmpty)?;
    let staging = Builder::new()
        .prefix("library-readable-restore-")
        .tempdir_in(parent)?;
    let database = staging.path().join("library.sqlite");
    rebuild(&validated, &database)?;
    verify(&validated, &database)?;
    fs::rename(staging.path(), empty_profile)?;
    std::mem::forget(staging);
    Ok(LibraryReadableReport {
        note_count: validated.manifest.notes.len(),
        resource_count: validated.manifest.resources.len(),
    })
}

fn validate(bundle: &Path) -> Result<Validated, ReadableExportError> {
    let root = open_bundle_root(bundle)?;
    let notes_dir = open_child_directory(&root, "notes")?;
    let history_dir = open_child_directory(&root, "history")?;
    let resources_dir = open_child_directory(&root, "resources")?;
    let bytes = read_regular_file_bounded(
        open_child_regular(&root, "manifest.json")?,
        MAX_MANIFEST_BYTES,
    )?;
    #[derive(Deserialize)]
    struct Header {
        format: String,
        version: u32,
    }
    let header: Header = serde_json::from_slice(&bytes)?;
    if header.format != FORMAT || header.version != VERSION {
        return Err(invalid("unsupported library export format or version"));
    }
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    if manifest.notes.len() > MAX_BUNDLE_NOTES || manifest.resources.len() > MAX_BUNDLE_RESOURCES {
        return Err(invalid("bundle exceeds restore limits"));
    }
    let stacks = unique_ids(
        manifest.stacks.iter().map(|stack| stack.id.as_str()),
        |id| StackId::parse(id).is_ok(),
    )?;
    let notebooks = unique_ids(
        manifest
            .notebooks
            .iter()
            .map(|notebook| notebook.id.as_str()),
        |id| NotebookId::parse(id).is_ok(),
    )?;
    let tags = unique_ids(manifest.tags.iter().map(|tag| tag.id.as_str()), |id| {
        TagId::parse(id).is_ok()
    })?;
    let resources = unique_ids(
        manifest
            .resources
            .iter()
            .map(|resource| resource.id.as_str()),
        |id| ResourceId::new(id).is_ok(),
    )?;
    unique_ids(manifest.notes.iter().map(|note| note.id.as_str()), |id| {
        NoteId::parse(id).is_ok()
    })?;
    if manifest
        .notebooks
        .iter()
        .filter(|notebook| notebook.is_default)
        .count()
        != 1
        || manifest.notebooks.iter().any(|notebook| {
            notebook
                .stack_id
                .as_ref()
                .is_some_and(|stack| !stacks.contains(stack))
        })
    {
        return Err(invalid("notebook organization is invalid"));
    }
    for resource in &manifest.resources {
        BlobHash::new(&resource.sha256).map_err(|_| invalid("invalid resource hash"))?;
        if resource.size < 0 || resource.size as u64 > crate::MAX_RESOURCE_BYTES as u64 {
            return Err(invalid("invalid resource size"));
        }
        let (sha256, size) = hash_regular_file_bounded(
            open_child_regular(&resources_dir, &resource_file_name(resource))?,
            resource.size as u64,
        )?;
        if sha256 != resource.sha256 || size != resource.size as u64 {
            return Err(ReadableExportError::ResourceVerification(
                resource.id.clone(),
            ));
        }
    }
    let mut bodies = BTreeMap::new();
    let mut histories = BTreeMap::new();
    for note in &manifest.notes {
        if !notebooks.contains(&note.notebook_id)
            || note.tag_ids.iter().any(|tag| !tags.contains(tag))
            || note.resource_ids.iter().any(|id| !resources.contains(id))
            || note
                .selected_thumbnail_id
                .as_ref()
                .is_some_and(|id| !note.resource_ids.contains(id))
        {
            return Err(invalid("note relations are invalid"));
        }
        let body = String::from_utf8(read_regular_file_bounded(
            open_child_regular(&notes_dir, &format!("{}.html", note.id))?,
            MAX_NOTE_HTML_BYTES,
        )?)
        .map_err(|_| invalid("note HTML is not UTF-8"))?;
        if sha256_hex(body.as_bytes()) != note.body_html_sha256 {
            return Err(invalid("note HTML digest differs"));
        }
        let document =
            CanonicalDocument::parse_html(&body).map_err(|_| invalid("note HTML invalid"))?;
        if document.to_canonical_html().as_str() != body
            || document
                .resource_ids()
                .iter()
                .map(ResourceId::as_str)
                .ne(note.resource_ids.iter().map(String::as_str))
        {
            return Err(invalid("note body and resources differ"));
        }
        let history_bytes = read_regular_file_bounded(
            open_child_regular(&history_dir, &format!("{}.json", note.id))?,
            MAX_HISTORY_FILE_BYTES,
        )?;
        if sha256_hex(&history_bytes) != note.history_sha256 {
            return Err(invalid("history digest differs"));
        }
        let history: Vec<Revision> = serde_json::from_slice(&history_bytes)?;
        if history.is_empty() || history.len() > MAX_NOTE_REVISIONS {
            return Err(invalid("invalid revision count"));
        }
        for (index, revision) in history.iter().enumerate() {
            let document = CanonicalDocument::parse_html(&revision.body_html)
                .map_err(|_| invalid("revision HTML invalid"))?;
            if revision.revision < 1
                || index > 0 && revision.revision <= history[index - 1].revision
                || document.to_canonical_html().as_str() != revision.body_html
                || document
                    .resource_ids()
                    .iter()
                    .any(|id| !resources.contains(id.as_str()))
            {
                return Err(invalid("revision is invalid"));
            }
        }
        let last = history.last().expect("non-empty");
        if last.revision > note.revision || last.title != note.title || last.body_html != body {
            return Err(invalid("current note differs from history"));
        }
        bodies.insert(note.id.clone(), body);
        histories.insert(note.id.clone(), history);
    }
    Ok(Validated {
        manifest,
        bodies,
        histories,
        resources_dir,
    })
}

fn unique_ids<'a>(
    ids: impl Iterator<Item = &'a str>,
    check: fn(&str) -> bool,
) -> Result<BTreeSet<String>, ReadableExportError> {
    let mut seen = BTreeSet::new();
    for id in ids {
        if !check(id) || !seen.insert(id.to_owned()) {
            return Err(invalid("duplicate or invalid ID"));
        }
    }
    Ok(seen)
}

fn search_text(html: &str) -> Result<String, ReadableExportError> {
    Ok(CanonicalDocument::parse_html(html)
        .map_err(|_| invalid("HTML invalid"))?
        .search_text()
        .as_str()
        .to_owned())
}

fn rebuild(validated: &Validated, database: &Path) -> Result<(), ReadableExportError> {
    let manifest = &validated.manifest;
    let repository = LibraryRepository::open(database)?;
    let generated_default = repository.default_notebook()?.id;
    let mut generated = Vec::with_capacity(manifest.resources.len());
    for resource in &manifest.resources {
        let source = open_child_regular(&validated.resources_dir, &resource_file_name(resource))?;
        generated.push(repository.import_resource_reader(
            source.take(resource.size as u64 + 1),
            resource.size as usize,
            &resource.title,
            &resource.mime,
            &resource.file_extension,
        )?);
    }
    drop(repository);

    let mut db = Connection::open(database)?;
    db.pragma_update(None, "foreign_keys", "ON")?;
    let tx = db.transaction()?;
    let note_count: i64 = tx.query_row("SELECT count(*) FROM notes", [], |row| row.get(0))?;
    if note_count != 0 {
        return Err(ReadableExportError::RestoreProfileNotPristine);
    }
    for stack in &manifest.stacks {
        tx.execute(
            "INSERT INTO stacks(id,title,revision,created_time,updated_time,deleted_time) VALUES(?1,?2,?3,?4,?5,?6)",
            params![stack.id, stack.title, stack.revision, stack.created_time, stack.updated_time, stack.deleted_time],
        )?;
    }
    for notebook in &manifest.notebooks {
        if notebook.is_default {
            tx.execute(
                "UPDATE notebooks SET id=?1,title=?2,stack_id=?3,revision=?4,created_time=?5,updated_time=?6,deleted_time=?7
                 WHERE id=?8 AND is_default=1",
                params![notebook.id, notebook.title, notebook.stack_id, notebook.revision, notebook.created_time, notebook.updated_time, notebook.deleted_time, generated_default.as_str()],
            )?;
        } else {
            tx.execute(
                "INSERT INTO notebooks(id,title,stack_id,is_default,revision,created_time,updated_time,deleted_time)
                 VALUES(?1,?2,?3,0,?4,?5,?6,?7)",
                params![notebook.id, notebook.title, notebook.stack_id, notebook.revision, notebook.created_time, notebook.updated_time, notebook.deleted_time],
            )?;
        }
    }
    for tag in &manifest.tags {
        tx.execute(
            "INSERT INTO tags(id,title,revision,created_time,updated_time,deleted_time) VALUES(?1,?2,?3,?4,?5,?6)",
            params![tag.id, tag.title, tag.revision, tag.created_time, tag.updated_time, tag.deleted_time],
        )?;
    }
    for (resource, generated) in manifest.resources.iter().zip(&generated) {
        tx.execute(
            "UPDATE resources SET id=?1,revision=?2,created_time=?3,updated_time=?4 WHERE id=?5",
            params![
                resource.id,
                resource.revision,
                resource.created_time,
                resource.updated_time,
                generated.as_str()
            ],
        )?;
        for table in [
            "resource_search_rows",
            "resource_filename_unicode",
            "resource_filename_trigram",
        ] {
            tx.execute(
                &format!("UPDATE {table} SET resource_id=?1 WHERE resource_id=?2"),
                params![resource.id, generated.as_str()],
            )?;
        }
    }
    // Separate from the ID rename: this column fires the filename-search
    // trigger, which must see the final ID.
    for resource in manifest
        .resources
        .iter()
        .filter(|resource| resource.deleted_time != 0)
    {
        tx.execute(
            "UPDATE resources SET deleted_time=?1 WHERE id=?2",
            params![resource.deleted_time, resource.id],
        )?;
    }
    for note in &manifest.notes {
        let body = &validated.bodies[&note.id];
        let text = search_text(body)?;
        tx.execute(
            "INSERT INTO notes(id,title,body_html,body_text,snippet,notebook_id,selected_thumbnail_id,created_time,updated_time,deleted_time,revision)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![note.id, note.title, body, text, snippet(&text), note.notebook_id, note.selected_thumbnail_id, note.created_time, note.updated_time, note.deleted_time, note.revision],
        )?;
        for (position, tag) in note.tag_ids.iter().enumerate() {
            tx.execute(
                "INSERT INTO note_tags(note_id,tag_id,position) VALUES(?1,?2,?3)",
                params![note.id, tag, position as i64],
            )?;
        }
        for (position, resource) in note.resource_ids.iter().enumerate() {
            tx.execute(
                "INSERT INTO note_resources(note_id,position,resource_id,is_associated) VALUES(?1,?2,?3,1)",
                params![note.id, position as i64, resource],
            )?;
        }
        for revision in &validated.histories[&note.id] {
            tx.execute(
                "INSERT INTO note_revisions(note_id,revision,title,body_html,body_text,created_time) VALUES(?1,?2,?3,?4,?5,?6)",
                params![note.id, revision.revision, revision.title, revision.body_html, search_text(&revision.body_html)?, revision.created_time],
            )?;
        }
        tx.execute(
            "INSERT INTO search_queue(note_id,updated_time,reason) VALUES(?1,?2,'restore')",
            params![note.id, note.updated_time],
        )?;
    }
    for shortcut in &manifest.shortcuts {
        tx.execute(
            "INSERT INTO shortcuts(id,entity_type,entity_id,position,created_time) VALUES(?1,?2,?3,?4,?5)",
            params![shortcut.id, shortcut.entity_type, shortcut.entity_id, shortcut.position, shortcut.created_time],
        )?;
    }
    tx.execute("DELETE FROM sync_outbox", [])?;
    tx.commit()?;
    drop(db);

    let repository = LibraryRepository::open(database)?;
    while repository.has_pending_search_jobs()? {
        if repository.process_search_jobs()? == 0 {
            return Err(invalid("restored search queue stalled"));
        }
    }
    Ok(())
}

fn verify(validated: &Validated, database: &Path) -> Result<(), ReadableExportError> {
    let manifest = &validated.manifest;
    let db = Connection::open(database)?;
    let count = |sql: &str| db.query_row(sql, [], |row| row.get::<_, i64>(0));
    let associations: usize = manifest
        .notes
        .iter()
        .map(|note| note.resource_ids.len())
        .sum();
    let note_tags: usize = manifest.notes.iter().map(|note| note.tag_ids.len()).sum();
    let revisions: usize = validated.histories.values().map(Vec::len).sum();
    if count("SELECT count(*) FROM stacks")? != manifest.stacks.len() as i64
        || count("SELECT count(*) FROM notebooks")? != manifest.notebooks.len() as i64
        || count("SELECT count(*) FROM tags")? != manifest.tags.len() as i64
        || count("SELECT count(*) FROM notes")? != manifest.notes.len() as i64
        || count("SELECT count(*) FROM note_tags")? != note_tags as i64
        || count("SELECT count(*) FROM note_resources")? != associations as i64
        || count("SELECT count(*) FROM note_revisions")? != revisions as i64
        || count("SELECT count(*) FROM resources")? != manifest.resources.len() as i64
        || count("SELECT count(*) FROM shortcuts")? != manifest.shortcuts.len() as i64
    {
        return Err(invalid("restored counts differ"));
    }
    let integrity: String = db.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" || count("SELECT count(*) FROM pragma_foreign_key_check")? != 0 {
        return Err(invalid("restored SQLite integrity verification failed"));
    }
    drop(db);
    let repository = LibraryRepository::open(database)?;
    for note in &manifest.notes {
        let id = NoteId::parse(&note.id).map_err(|_| invalid("invalid note ID"))?;
        let actual = repository
            .load_note(&id)?
            .ok_or_else(|| invalid("restored note missing"))?;
        if actual.body_html != validated.bodies[&note.id]
            || actual.notebook_id.as_str() != note.notebook_id
            || actual
                .tag_ids
                .iter()
                .map(TagId::as_str)
                .ne(note.tag_ids.iter().map(String::as_str))
        {
            return Err(invalid("restored note differs"));
        }
    }
    for resource in &manifest.resources {
        let id = ResourceId::new(&resource.id).map_err(|_| invalid("invalid resource ID"))?;
        let actual = repository
            .resource_metadata(&id)?
            .ok_or_else(|| invalid("restored resource missing"))?;
        if actual.sha256.as_str() != resource.sha256 || actual.size != resource.size {
            return Err(ReadableExportError::ResourceVerification(
                resource.id.clone(),
            ));
        }
    }
    Ok(())
}
