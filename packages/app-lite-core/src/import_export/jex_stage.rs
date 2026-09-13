//! C2c-3c: a bounded note, resource, two-level folder, tag and relation JEX stage.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use tempfile::{Builder, TempDir};
use thiserror::Error;

use crate::{
    CreateNote, LibraryError, LibraryRepository, NoteId, NotebookId, ResourceId, StackId, TagId,
};

#[path = "jex_stage_folders.rs"]
mod folders;

#[path = "jex_stage_resources.rs"]
mod resources;

#[path = "jex_stage_tags.rs"]
mod tags;

use super::{
    JexBodyFidelityBlocker, JexPrepareError, JexScanCounts, JexVerifiedResource,
    convert_jex_note_body, parse_item, prepare_jex_source_archive,
};

#[derive(Debug)]
pub struct JexStagedProfile {
    directory: TempDir,
    report: JexStageReport,
}

impl JexStagedProfile {
    pub fn profile_path(&self) -> &Path {
        self.directory.path()
    }
    pub fn report(&self) -> &JexStageReport {
        &self.report
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JexStagedNote {
    pub source_id: String,
    pub source_path: String,
    pub source_parent_id: String,
    pub destination_id: NoteId,
    pub markup_language: i64,
    pub raw_sha256: String,
    pub resource_ids: Vec<ResourceId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JexStagedResource {
    pub source_id: String,
    pub source_path: String,
    pub physical_path: String,
    pub destination_id: ResourceId,
    pub title: String,
    pub mime: String,
    pub file_extension: String,
    pub raw_metadata_sha256: String,
    pub sha256: String,
    pub byte_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JexFolderDestination {
    Stack(StackId),
    Notebook(NotebookId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JexStagedFolder {
    pub source_id: String,
    pub source_path: String,
    pub source_parent_id: String,
    pub title: String,
    pub destination: JexFolderDestination,
    pub raw_sha256: String,
    pub created_time: i64,
    pub updated_time: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JexStagedTag {
    pub source_id: String,
    pub source_path: String,
    pub title: String,
    pub destination_id: TagId,
    pub raw_sha256: String,
    pub created_time: i64,
    pub updated_time: i64,
    pub user_created_time: Option<i64>,
    pub user_updated_time: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JexStagedRelation {
    pub source_id: String,
    pub source_path: String,
    pub source_note_id: String,
    pub source_tag_id: String,
    pub destination_note_id: NoteId,
    pub destination_tag_id: TagId,
    pub raw_sha256: String,
    pub created_time: i64,
    pub updated_time: i64,
    pub user_created_time: Option<i64>,
    pub user_updated_time: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JexStageReport {
    pub preflight_counts: JexScanCounts,
    pub notes: Vec<JexStagedNote>,
    pub resources: Vec<JexStagedResource>,
    pub folders: Vec<JexStagedFolder>,
    pub tags: Vec<JexStagedTag>,
    pub relations: Vec<JexStagedRelation>,
    pub verified_sync_outbox_rows: i64,
    pub search_index_drained: bool,
}

#[derive(Debug, Error)]
pub enum JexStageError {
    #[error("JEX source preparation failed: {0}")]
    Prepare(#[from] JexPrepareError),
    #[error("JEX item class {item_type} at {source_id} is not supported by this staging cut")]
    UnsupportedEntity { source_id: String, item_type: i64 },
    #[error("JEX note {source_id} at {source_path} is not supported: {reason}")]
    UnsupportedNote {
        source_id: String,
        source_path: String,
        reason: &'static str,
    },
    #[error("JEX body fidelity blocked: {0:?}")]
    Fidelity(JexBodyFidelityBlocker),
    #[error("JEX resource {source_id} is not supported: {reason}")]
    UnsupportedResource {
        source_id: String,
        reason: &'static str,
    },
    #[error("JEX folder {source_id} at {source_path} is not supported: {reason}")]
    UnsupportedFolder {
        source_id: String,
        source_path: String,
        reason: &'static str,
    },
    #[error("JEX tag {source_id} at {source_path} is not supported: {reason}")]
    UnsupportedTag {
        source_id: String,
        source_path: String,
        reason: &'static str,
    },
    #[error("JEX note-tag relation {source_id} at {source_path} is not supported: {reason}")]
    UnsupportedRelation {
        source_id: String,
        source_path: String,
        reason: &'static str,
    },
    #[error("JEX staging I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("JEX staging SQLite failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("JEX staging repository failed: {0}")]
    Repository(#[from] LibraryError),
    #[error("JEX staging verification failed: {0}")]
    Verification(String),
}

struct ParsedNote<'a> {
    source_id: &'a str,
    source_path: &'a str,
    title: String,
    body: String,
    raw_body: Vec<u8>,
    parent_id: String,
    markup: i64,
    created: i64,
    updated: i64,
    user_created: i64,
    user_updated: i64,
}

fn invalid_note(source_id: &str, source_path: &str, reason: &'static str) -> JexStageError {
    JexStageError::UnsupportedNote {
        source_id: source_id.to_owned(),
        source_path: source_path.to_owned(),
        reason,
    }
}

// BaseItem.serialize_format emits UTC ISO milliseconds, while
// BaseItem.unserialize_format converts the same value back to Unix millis.
// Empty/missing values are refused in this bounded stage instead of becoming
// a new profile's current time.
pub(super) fn parse_joplin_utc_millis(raw: &str) -> Option<i64> {
    let bytes = raw.as_bytes();
    if bytes.len() != 24
        || [
            (4, b'-'),
            (7, b'-'),
            (10, b'T'),
            (13, b':'),
            (16, b':'),
            (19, b'.'),
            (23, b'Z'),
        ]
        .iter()
        .any(|(index, expected)| bytes[*index] != *expected)
    {
        return None;
    }
    if bytes
        .iter()
        .enumerate()
        .any(|(index, byte)| ![4, 7, 10, 13, 16, 19, 23].contains(&index) && !byte.is_ascii_digit())
    {
        return None;
    }
    let number = |range: std::ops::Range<usize>| raw.get(range)?.parse::<i64>().ok();
    let (year, month, day, hour, minute, second, millis) = (
        number(0..4)?,
        number(5..7)?,
        number(8..10)?,
        number(11..13)?,
        number(14..16)?,
        number(17..19)?,
        number(20..23)?,
    );
    if !(1970..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(1..=days_in_month).contains(&day) {
        return None;
    }
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some((days * 86400 + hour * 3600 + minute * 60 + second) * 1000 + millis)
}

fn source_time(
    props: &BTreeMap<String, String>,
    key: &'static str,
    source_id: &str,
    path: &str,
) -> Result<i64, JexStageError> {
    props
        .get(key)
        .and_then(|value| parse_joplin_utc_millis(value))
        .ok_or_else(|| invalid_note(source_id, path, "missing or invalid source timestamp"))
}

fn parse_note<'a>(
    source_id: &'a str,
    path: &'a str,
    raw: &[u8],
) -> Result<ParsedNote<'a>, JexStageError> {
    let content = std::str::from_utf8(raw)
        .map_err(|_| invalid_note(source_id, path, "source item is not UTF-8"))?;
    let parsed = parse_item(path, content)
        .map_err(|_| invalid_note(source_id, path, "source item changed or is malformed"))?;
    if parsed.item_type != 1 || !parsed.id.eq_ignore_ascii_case(source_id) {
        return Err(invalid_note(
            source_id,
            path,
            "source note ID or item type changed",
        ));
    }
    let allowed = [
        "id",
        "type_",
        "parent_id",
        "markup_language",
        "created_time",
        "updated_time",
        "user_created_time",
        "user_updated_time",
        "encryption_applied",
        "is_todo",
    ];
    if parsed
        .properties
        .keys()
        .any(|key| !allowed.contains(&key.as_str()))
    {
        return Err(invalid_note(
            source_id,
            path,
            "source note has metadata not yet mapped by bounded staging",
        ));
    }
    let parent_id = parsed
        .properties
        .get("parent_id")
        .ok_or_else(|| invalid_note(source_id, path, "source parent_id is missing"))?
        .clone();
    if !parent_id.is_empty() && !super::valid_joplin_id(&parent_id) {
        return Err(invalid_note(
            source_id,
            path,
            "source parent_id is malformed",
        ));
    }
    for flag in ["encryption_applied", "is_todo"] {
        if parsed
            .properties
            .get(flag)
            .is_some_and(|value| value != "0" && !value.is_empty())
        {
            return Err(invalid_note(
                source_id,
                path,
                "encrypted or task note is unsupported by bounded staging",
            ));
        }
    }
    let markup = parsed
        .properties
        .get("markup_language")
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value == 1 || *value == 2)
        .ok_or_else(|| invalid_note(source_id, path, "missing or unsupported markup_language"))?;
    let created = source_time(&parsed.properties, "created_time", source_id, path)?;
    let updated = source_time(&parsed.properties, "updated_time", source_id, path)?;
    let user_created = source_time(&parsed.properties, "user_created_time", source_id, path)?;
    let user_updated = source_time(&parsed.properties, "user_updated_time", source_id, path)?;
    let raw_body = validate_raw_note_body(&content, &parsed.note_body, source_id, path)?;
    let body = if content.contains("\r\n") {
        String::from_utf8(raw_body.clone())
            .expect("source UTF-8 checked")
            .replace("\r\n", "\n")
    } else {
        String::from_utf8(raw_body.clone()).expect("source UTF-8 checked")
    };
    let title = content.lines().next().unwrap_or_default().to_owned();
    Ok(ParsedNote {
        source_id,
        source_path: path,
        title,
        body,
        raw_body,
        parent_id,
        markup,
        created,
        updated,
        user_created,
        user_updated,
    })
}

/// Exact source separator/body check shared by stage and exporter qualification.
/// It does not perform any metadata-field acceptance or user-visible mapping.
fn validate_raw_note_body(
    content: &str,
    parsed_body: &str,
    source_id: &str,
    path: &str,
) -> Result<Vec<u8>, JexStageError> {
    let separator = if content.contains("\r\n") {
        "\r\n\r\n"
    } else {
        "\n\n"
    };
    if separator == "\r\n\r\n"
        && content
            .as_bytes()
            .windows(1)
            .enumerate()
            .any(|(i, w)| w == b"\n" && (i == 0 || content.as_bytes()[i - 1] != b'\r'))
    {
        return Err(invalid_note(
            source_id,
            path,
            "mixed source item newline styles",
        ));
    }
    let first = content
        .find(separator)
        .ok_or_else(|| invalid_note(source_id, path, "source note has no title/body separator"))?;
    let last = content
        .rfind(separator)
        .ok_or_else(|| invalid_note(source_id, path, "source note has no property separator"))?;
    let raw_body = if first == last {
        Vec::new()
    } else {
        content.as_bytes()[first + separator.len()..last].to_vec()
    };
    let body = if separator == "\r\n\r\n" {
        String::from_utf8(raw_body.clone())
            .expect("source UTF-8 checked")
            .replace("\r\n", "\n")
    } else {
        String::from_utf8(raw_body.clone()).expect("source UTF-8 checked")
    };
    if body != parsed_body {
        return Err(invalid_note(
            source_id,
            path,
            "raw body and Joplin note parser disagree",
        ));
    }
    Ok(raw_body)
}

pub(super) fn validate_source_note_body(
    raw: &super::JexRawSourceItem,
) -> Result<(), JexStageError> {
    let content = std::str::from_utf8(&raw.raw_bytes).map_err(|_| {
        invalid_note(
            &raw.source_id,
            &raw.archive_path,
            "source item is not UTF-8",
        )
    })?;
    let parsed = super::parse_item(&raw.archive_path, content).map_err(|_| {
        invalid_note(
            &raw.source_id,
            &raw.archive_path,
            "source note parser rejected item",
        )
    })?;
    validate_raw_note_body(
        content,
        &parsed.note_body,
        &raw.source_id,
        &raw.archive_path,
    )
    .map(|_| ())
}

/// Reuses the exact stage parsers for pure qualification without opening a
/// destination repository. Graph and body checks are performed separately.
pub(super) fn validate_source_item(raw: &super::JexRawSourceItem) -> Result<(), JexStageError> {
    match raw.item_type {
        1 => parse_note(&raw.source_id, &raw.archive_path, &raw.raw_bytes).map(|_| ()),
        2 => folders::validate_source_item(raw),
        4 => resources::validate_source_item(raw),
        5 | 6 => tags::validate_source_item(raw),
        _ => Err(JexStageError::UnsupportedEntity {
            source_id: raw.source_id.clone(),
            item_type: raw.item_type,
        }),
    }
}

pub(super) fn verify_source_signature_prefix(
    source_id: &str,
    mime: &str,
    prefix: &[u8],
) -> Result<(), JexStageError> {
    resources::verify_signature_prefix(source_id, mime, prefix)
}

fn count(db: &Connection, table: &str) -> Result<i64, JexStageError> {
    Ok(
        db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })?,
    )
}

fn verify(database: &Path, report: &mut JexStageReport) -> Result<(), JexStageError> {
    let repo = LibraryRepository::open(database)?;
    let db = Connection::open(database)?;
    let integrity: String = db.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" || count(&db, "pragma_foreign_key_check")? != 0 {
        return Err(JexStageError::Verification(
            "SQLite integrity or foreign keys failed".into(),
        ));
    }
    if report.notes.len() != report.preflight_counts.notes
        || report.resources.len() != report.preflight_counts.resource_metadata
        || report.folders.len() != report.preflight_counts.folders
        || report.tags.len() != report.preflight_counts.tags
        || report.relations.len() != report.preflight_counts.note_tag_relations
        || count(&db, "notes")? != report.notes.len() as i64
        || count(&db, "jex_stage_note_audit")? != report.notes.len() as i64
        || count(&db, "note_resources")?
            != report
                .notes
                .iter()
                .map(|note| note.resource_ids.len())
                .sum::<usize>() as i64
        || count(&db, "resources")? != report.resources.len() as i64
        || count(&db, "resource_blobs")?
            != report
                .resources
                .iter()
                .map(|resource| resource.sha256.as_str())
                .collect::<BTreeSet<_>>()
                .len() as i64
        || count(&db, "jex_stage_resource_audit")? != report.resources.len() as i64
        || count(&db, "jex_stage_folder_audit")? != report.folders.len() as i64
        || count(&db, "tags")? != report.tags.len() as i64
        || count(&db, "jex_stage_tag_audit")? != report.tags.len() as i64
        || count(&db, "note_tags")? != report.relations.len() as i64
        || count(&db, "jex_stage_relation_audit")? != report.relations.len() as i64
        || count(&db, "stacks")?
            != report
                .folders
                .iter()
                .filter(|f| matches!(f.destination, JexFolderDestination::Stack(_)))
                .count() as i64
        || count(&db, "notebooks")?
            != 1 + report
                .folders
                .iter()
                .filter(|f| matches!(f.destination, JexFolderDestination::Notebook(_)))
                .count() as i64
    {
        return Err(JexStageError::Verification(
            "staged entity count mismatch".into(),
        ));
    }
    let verified_resources = report
        .resources
        .iter()
        .map(|resource| {
            (
                resource.source_id.to_ascii_lowercase(),
                JexVerifiedResource {
                    destination_id: resource.destination_id.clone(),
                    mime: resource.mime.clone(),
                    filename: resource.title.clone(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let folder_destinations = report
        .folders
        .iter()
        .map(|folder| {
            (
                folder.source_id.to_ascii_lowercase(),
                folder.destination.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for folder in &report.folders {
        folders::verify_one(&db, folder, &folder_destinations)?;
    }
    tags::verify_all(&db, report)?;
    let mut expected_tag_membership = BTreeMap::<String, Vec<TagId>>::new();
    for relation in &report.relations {
        expected_tag_membership
            .entry(relation.destination_note_id.as_str().to_owned())
            .or_default()
            .push(relation.destination_tag_id.clone());
    }
    for resource in &report.resources {
        resources::verify_one(&repo, &db, resource)?;
    }
    for entry in &report.notes {
        let note = repo
            .load_note(&entry.destination_id)?
            .ok_or_else(|| JexStageError::Verification("reopened note missing".into()))?;
        let (raw_item, raw_body, markup, source_title, source_created, source_updated, user_created, user_updated): (Vec<u8>, Vec<u8>, i64, String, i64, i64, i64, i64) = db.query_row(
            "SELECT raw_item_bytes,raw_body_bytes,markup_language,source_title,created_time,updated_time,user_created_time,user_updated_time FROM jex_stage_note_audit WHERE source_id=?1 AND note_id=?2 AND source_path=?3",
            params![entry.source_id, entry.destination_id.as_str(), entry.source_path],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?)),
        )?;
        if format!("{:x}", Sha256::digest(&raw_item)) != entry.raw_sha256 {
            return Err(JexStageError::Verification(
                "raw source item audit digest differs".into(),
            ));
        }
        let parsed = parse_note(&entry.source_id, &entry.source_path, &raw_item)?;
        if parsed.raw_body != raw_body
            || parsed.parent_id != entry.source_parent_id
            || parsed.markup != markup
            || parsed.title != source_title
            || parsed.created != source_created
            || parsed.updated != source_updated
            || parsed.user_created != user_created
            || parsed.user_updated != user_updated
        {
            return Err(JexStageError::Verification(
                "reopened source audit differs".into(),
            ));
        }
        let converted = convert_jex_note_body(
            &entry.source_id,
            &entry.source_path,
            markup,
            &parsed.body,
            &verified_resources,
        )
        .map_err(JexStageError::Fidelity)?;
        let expected_notebook = if entry.source_parent_id.is_empty() {
            repo.default_notebook()?.id
        } else {
            match folder_destinations.get(&entry.source_parent_id.to_ascii_lowercase()) {
                Some(JexFolderDestination::Notebook(id)) => id.clone(),
                _ => {
                    return Err(JexStageError::Verification(
                        "note source parent is not a mapped notebook".into(),
                    ));
                }
            }
        };
        if note.title != parsed.title
            || note.notebook_id != expected_notebook
            || note.body_html != converted.canonical_html
            || note.body_text != converted.search_text
            || note.created_time != user_created
            || note.updated_time != user_updated
            || note.resource_ids != entry.resource_ids
            || note.resource_ids != converted.ordered_resource_occurrences
            || note.tag_ids
                != expected_tag_membership
                    .get(note.id.as_str())
                    .cloned()
                    .unwrap_or_default()
        {
            return Err(JexStageError::Verification(
                "reopened note content or times differ".into(),
            ));
        }
        let selected: Option<String> = db.query_row(
            "SELECT selected_thumbnail_id FROM notes WHERE id=?1",
            [note.id.as_str()],
            |row| row.get(0),
        )?;
        let mut expected_thumbnail = None;
        for resource_id in &entry.resource_ids {
            let resource = repo.resource_metadata(resource_id)?.ok_or_else(|| {
                JexStageError::Verification("note resource metadata missing".into())
            })?;
            if resource.mime.starts_with("image/") {
                expected_thumbnail = Some(resource_id.as_str().to_owned());
                break;
            }
        }
        if selected != expected_thumbnail {
            return Err(JexStageError::Verification(
                "reopened selected thumbnail differs".into(),
            ));
        }
    }
    if repo.outbox_count()? != 0 {
        return Err(JexStageError::Verification(
            "migration outbox is not empty".into(),
        ));
    }
    while repo.has_pending_search_jobs()? {
        if repo.process_search_jobs()? == 0 {
            return Err(JexStageError::Verification(
                "search queue did not drain".into(),
            ));
        }
    }
    report.search_index_drained = !repo.has_pending_search_jobs()?;
    if count(&db, "search_unicode")? != report.notes.len() as i64
        || count(&db, "search_trigram")? != report.notes.len() as i64
    {
        return Err(JexStageError::Verification(
            "search projection count differs".into(),
        ));
    }
    for entry in &report.notes {
        let note = repo
            .load_note(&entry.destination_id)?
            .ok_or_else(|| JexStageError::Verification("indexed note missing".into()))?;
        for table in ["search_unicode", "search_trigram"] {
            let (title, body): (String, String) = db.query_row(
                &format!("SELECT title,body FROM {table} WHERE note_id=?1"),
                [note.id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if title != note.title || body != note.body_text {
                return Err(JexStageError::Verification(
                    "search projection text differs".into(),
                ));
            }
        }
    }
    report.verified_sync_outbox_rows = repo.outbox_count()?;
    if report.verified_sync_outbox_rows != 0 {
        return Err(JexStageError::Verification(
            "migration outbox changed during indexing".into(),
        ));
    }
    Ok(())
}

/// Creates a uniquely owned note, resource and bounded-organization profile
/// after verified JEX source spooling. No live profile is opened or promoted.
pub fn stage_jex_file(
    source: impl AsRef<Path>,
    staging_parent: impl AsRef<Path>,
) -> Result<JexStagedProfile, JexStageError> {
    let parent = fs::canonicalize(staging_parent.as_ref())
        .map_err(|_| JexStageError::Prepare(JexPrepareError::InvalidStagingParent))?;
    let prepared = match prepare_jex_source_archive(source, &parent) {
        Ok(prepared) => prepared,
        Err(JexPrepareError::PreflightBlocked { report })
            if !report.orphan_note_tag_relations.is_empty() =>
        {
            let mut remainder = (*report).clone();
            remainder.orphan_note_tag_relations.clear();
            if remainder.is_clean() {
                let id = &report.orphan_note_tag_relations[0];
                let path = report
                    .metadata_items
                    .iter()
                    .find(|item| item.source_id.eq_ignore_ascii_case(id))
                    .map(|item| item.archive_path.as_str())
                    .unwrap_or("");
                return Err(tags::blocked_relation(
                    id,
                    path,
                    "relation endpoint is missing",
                ));
            }
            return Err(JexStageError::Prepare(JexPrepareError::PreflightBlocked {
                report,
            }));
        }
        Err(error) => return Err(JexStageError::Prepare(error)),
    };
    let scanned = prepared.report();
    if scanned.counts.notes == 0 {
        return Err(JexStageError::Verification(
            "JEX contains no supported notes".into(),
        ));
    }
    let folder_plan = folders::preflight(&prepared)?;
    let tag_plan = tags::preflight(&prepared)?;
    let directory = Builder::new().prefix("jex-stage-").tempdir_in(&parent)?;
    let database: PathBuf = directory.path().join("library.sqlite");
    let repo = LibraryRepository::open(&database)?;
    let audit = Connection::open(&database)?;
    audit.execute_batch(
        "CREATE TABLE jex_stage_note_audit (
        source_id TEXT PRIMARY KEY NOT NULL COLLATE NOCASE,
        source_path TEXT NOT NULL UNIQUE,
        note_id TEXT NOT NULL UNIQUE REFERENCES notes(id),
        source_title TEXT NOT NULL,
        raw_item_bytes BLOB NOT NULL,
        raw_body_bytes BLOB NOT NULL,
        markup_language INTEGER NOT NULL,
        created_time INTEGER NOT NULL,
        updated_time INTEGER NOT NULL,
        user_created_time INTEGER NOT NULL,
        user_updated_time INTEGER NOT NULL);
        CREATE TABLE jex_stage_resource_audit (
        source_id TEXT PRIMARY KEY NOT NULL COLLATE NOCASE,
        source_path TEXT NOT NULL UNIQUE,
        physical_path TEXT NOT NULL UNIQUE,
        resource_id TEXT NOT NULL UNIQUE REFERENCES resources(id),
        raw_metadata_bytes BLOB NOT NULL,
        raw_metadata_sha256 TEXT NOT NULL,
        title TEXT NOT NULL,
        mime TEXT NOT NULL,
        file_extension TEXT NOT NULL,
        physical_sha256 TEXT NOT NULL,
        physical_size INTEGER NOT NULL);
        CREATE TABLE jex_stage_folder_audit (
        source_id TEXT PRIMARY KEY NOT NULL COLLATE NOCASE,
        source_path TEXT NOT NULL UNIQUE,
        source_parent_id TEXT NOT NULL,
        title TEXT NOT NULL,
        raw_item_bytes BLOB NOT NULL,
        raw_sha256 TEXT NOT NULL,
        destination_kind TEXT NOT NULL CHECK(destination_kind IN ('stack','notebook')),
        destination_id TEXT NOT NULL UNIQUE,
        created_time INTEGER NOT NULL,
        updated_time INTEGER NOT NULL);
        CREATE TABLE jex_stage_tag_audit (
        source_id TEXT PRIMARY KEY NOT NULL COLLATE NOCASE,
        source_path TEXT NOT NULL UNIQUE,
        tag_id TEXT NOT NULL UNIQUE REFERENCES tags(id),
        title TEXT NOT NULL,
        raw_item_bytes BLOB NOT NULL,
        raw_sha256 TEXT NOT NULL,
        created_time INTEGER NOT NULL,
        updated_time INTEGER NOT NULL,
        user_created_time INTEGER,
        user_updated_time INTEGER);
        CREATE TABLE jex_stage_relation_audit (
        source_id TEXT PRIMARY KEY NOT NULL COLLATE NOCASE,
        source_path TEXT NOT NULL UNIQUE,
        source_note_id TEXT NOT NULL,
        source_tag_id TEXT NOT NULL,
        note_id TEXT NOT NULL REFERENCES notes(id),
        tag_id TEXT NOT NULL REFERENCES tags(id),
        raw_item_bytes BLOB NOT NULL,
        raw_sha256 TEXT NOT NULL,
        created_time INTEGER NOT NULL,
        updated_time INTEGER NOT NULL,
        user_created_time INTEGER,
        user_updated_time INTEGER,
        UNIQUE(note_id,tag_id));",
    )?;
    let mut report = JexStageReport {
        preflight_counts: scanned.counts.clone(),
        ..Default::default()
    };
    let notebook_map = folders::create_all(&folder_plan, &prepared, &repo, &audit, &mut report)?;
    let tag_map = tags::create_tags(&tag_plan, &prepared, &repo, &audit, &mut report)?;
    let mut verified_resources = BTreeMap::new();
    for source in &scanned.resources {
        let (mapped, verified) = resources::import_one(&prepared, source, &repo, &audit)?;
        verified_resources.insert(mapped.source_id.to_ascii_lowercase(), verified);
        report.resources.push(mapped);
    }
    for source_id in &scanned.source_ids.notes {
        let raw = prepared
            .raw_item(source_id)?
            .ok_or_else(|| JexStageError::Verification("verified note item disappeared".into()))?;
        let parsed = parse_note(source_id, &raw.archive_path, &raw.raw_bytes)?;
        let converted = convert_jex_note_body(
            parsed.source_id,
            parsed.source_path,
            parsed.markup,
            &parsed.body,
            &verified_resources,
        )
        .map_err(JexStageError::Fidelity)?;
        let resource_ids = converted.ordered_resource_occurrences;
        let notebook_id = if parsed.parent_id.is_empty() {
            None
        } else {
            Some(
                notebook_map
                    .get(&parsed.parent_id.to_ascii_lowercase())
                    .cloned()
                    .ok_or_else(|| {
                        invalid_note(
                            source_id,
                            &raw.archive_path,
                            "source note parent is not a leaf notebook",
                        )
                    })?,
            )
        };
        let note = repo.create_note(CreateNote {
            title: parsed.title.clone(),
            notebook_id,
            document: converted.document,
        })?;
        audit.execute("INSERT INTO jex_stage_note_audit (source_id,source_path,note_id,source_title,raw_item_bytes,raw_body_bytes,markup_language,created_time,updated_time,user_created_time,user_updated_time)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![source_id, raw.archive_path, note.id.as_str(), parsed.title, raw.raw_bytes, parsed.raw_body, parsed.markup, parsed.created, parsed.updated, parsed.user_created, parsed.user_updated])?;
        report.notes.push(JexStagedNote {
            source_id: source_id.clone(),
            source_path: raw.archive_path.clone(),
            source_parent_id: parsed.parent_id,
            destination_id: note.id,
            markup_language: parsed.markup,
            raw_sha256: raw.raw_sha256,
            resource_ids,
        });
    }
    tags::create_relations(&tag_plan, &prepared, &repo, &audit, &mut report, &tag_map)?;
    drop(audit);
    drop(repo);
    let mut db = Connection::open(&database)?;
    let tx = db.transaction()?;
    tx.execute("UPDATE notes SET created_time=(SELECT user_created_time FROM jex_stage_note_audit WHERE note_id=notes.id), updated_time=(SELECT user_updated_time FROM jex_stage_note_audit WHERE note_id=notes.id) WHERE id IN (SELECT note_id FROM jex_stage_note_audit)", [])?;
    tx.execute("UPDATE note_revisions SET created_time=(SELECT user_updated_time FROM jex_stage_note_audit WHERE note_id=note_revisions.note_id) WHERE note_id IN (SELECT note_id FROM jex_stage_note_audit)", [])?;
    tx.execute("DELETE FROM sync_outbox", [])?;
    tx.commit()?;
    drop(db);
    verify(&database, &mut report)?;
    drop(prepared);
    Ok(JexStagedProfile { directory, report })
}
