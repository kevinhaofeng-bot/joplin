//! C2c-3a: a deliberately note-only JEX stage. Unsupported entity classes
//! block before publication; this is not a complete JEX importer.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use tempfile::{Builder, TempDir};
use thiserror::Error;

use crate::{CreateNote, LibraryError, LibraryRepository, NoteId};

use super::{
    JexBodyFidelityBlocker, JexPrepareError, JexScanCounts, convert_jex_note_body, parse_item,
    prepare_jex_source_archive,
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
    pub destination_id: NoteId,
    pub markup_language: i64,
    pub raw_sha256: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JexStageReport {
    pub preflight_counts: JexScanCounts,
    pub notes: Vec<JexStagedNote>,
    pub verified_sync_outbox_rows: i64,
    pub search_index_drained: bool,
}

#[derive(Debug, Error)]
pub enum JexStageError {
    #[error("JEX source preparation failed: {0}")]
    Prepare(#[from] JexPrepareError),
    #[error("JEX item class {item_type} at {source_id} is not supported by note-only staging")]
    UnsupportedEntity { source_id: String, item_type: i64 },
    #[error("JEX note {source_id} at {source_path} is not supported: {reason}")]
    UnsupportedNote {
        source_id: String,
        source_path: String,
        reason: &'static str,
    },
    #[error("JEX body fidelity blocked: {0:?}")]
    Fidelity(JexBodyFidelityBlocker),
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
fn parse_joplin_utc_millis(raw: &str) -> Option<i64> {
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
            "source note has metadata not yet mapped by note-only staging",
        ));
    }
    if parsed
        .properties
        .get("parent_id")
        .is_some_and(|value| !value.is_empty())
    {
        return Err(invalid_note(
            source_id,
            path,
            "source notebook mapping requires a later staging cut",
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
                "encrypted or task note is unsupported by note-only staging",
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
    if body != parsed.note_body {
        return Err(invalid_note(
            source_id,
            path,
            "raw body and Joplin note parser disagree",
        ));
    }
    let title = content.lines().next().unwrap_or_default().to_owned();
    Ok(ParsedNote {
        source_id,
        source_path: path,
        title,
        body,
        raw_body,
        markup,
        created,
        updated,
        user_created,
        user_updated,
    })
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
    if count(&db, "notes")? != report.notes.len() as i64
        || count(&db, "jex_stage_note_audit")? != report.notes.len() as i64
        || count(&db, "note_resources")? != 0
        || count(&db, "resources")? != 0
        || count(&db, "tags")? != 0
        || count(&db, "note_tags")? != 0
        || count(&db, "stacks")? != 0
        || count(&db, "notebooks")? != 1
    {
        return Err(JexStageError::Verification(
            "staged entity count mismatch".into(),
        ));
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
            &BTreeMap::new(),
        )
        .map_err(JexStageError::Fidelity)?;
        if note.title != parsed.title
            || note.body_html != converted.canonical_html
            || note.body_text != converted.search_text
            || note.created_time != user_created
            || note.updated_time != user_updated
            || !note.resource_ids.is_empty()
            || !note.tag_ids.is_empty()
            || !converted.ordered_resource_occurrences.is_empty()
        {
            return Err(JexStageError::Verification(
                "reopened note content or times differ".into(),
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
    Ok(())
}

/// Creates a uniquely owned note-only profile after verified JEX source
/// spooling. Unsupported JEX entity classes fail closed in C2c-3a.
pub fn stage_jex_file(
    source: impl AsRef<Path>,
    staging_parent: impl AsRef<Path>,
) -> Result<JexStagedProfile, JexStageError> {
    let parent = fs::canonicalize(staging_parent.as_ref())
        .map_err(|_| JexStageError::Prepare(JexPrepareError::InvalidStagingParent))?;
    let prepared = prepare_jex_source_archive(source, &parent)?;
    let scanned = prepared.report();
    for (ids, kind) in [
        (&scanned.source_ids.note_tag_relations, 6),
        (&scanned.source_ids.folders, 2),
        (&scanned.source_ids.resource_metadata, 4),
        (&scanned.source_ids.tags, 5),
    ] {
        if let Some(source_id) = ids.first() {
            return Err(JexStageError::UnsupportedEntity {
                source_id: source_id.clone(),
                item_type: kind,
            });
        }
    }
    if scanned.counts.notes == 0 {
        return Err(JexStageError::Verification(
            "JEX contains no supported notes".into(),
        ));
    }
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
        user_updated_time INTEGER NOT NULL);",
    )?;
    let mut report = JexStageReport {
        preflight_counts: scanned.counts.clone(),
        ..Default::default()
    };
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
            &BTreeMap::new(),
        )
        .map_err(JexStageError::Fidelity)?;
        if !converted.ordered_resource_occurrences.is_empty() {
            return Err(invalid_note(
                source_id,
                &raw.archive_path,
                "resources require a later staging cut",
            ));
        }
        let note = repo.create_note(CreateNote {
            title: parsed.title.clone(),
            notebook_id: None,
            document: converted.document,
        })?;
        audit.execute("INSERT INTO jex_stage_note_audit (source_id,source_path,note_id,source_title,raw_item_bytes,raw_body_bytes,markup_language,created_time,updated_time,user_created_time,user_updated_time)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![source_id, raw.archive_path, note.id.as_str(), parsed.title, raw.raw_bytes, parsed.raw_body, parsed.markup, parsed.created, parsed.updated, parsed.user_created, parsed.user_updated])?;
        report.notes.push(JexStagedNote {
            source_id: source_id.clone(),
            source_path: raw.archive_path.clone(),
            destination_id: note.id,
            markup_language: parsed.markup,
            raw_sha256: raw.raw_sha256,
        });
    }
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
