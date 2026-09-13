//! Source-provenant JEX tags and note-tag relations for an owned profile.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};

use crate::{LibraryRepository, TagId};

use super::super::{JexPreparedSource, JexRawSourceItem, parse_item, valid_joplin_id};
use super::{
    JexStageError, JexStageReport, JexStagedRelation, JexStagedTag, accepts_known_exporter_default,
    is_optional_timestamp_default, parse_joplin_utc_millis,
};

struct SourceTag {
    source_id: String,
    source_path: String,
    title: String,
    raw_sha256: String,
    created: i64,
    updated: i64,
    user_created: Option<i64>,
    user_updated: Option<i64>,
}

struct SourceRelation {
    source_id: String,
    source_path: String,
    note_id: String,
    tag_id: String,
    raw_sha256: String,
    created: i64,
    updated: i64,
    user_created: Option<i64>,
    user_updated: Option<i64>,
}

pub(super) struct TagPlan {
    tags: Vec<SourceTag>,
    relations: Vec<SourceRelation>,
}

fn blocked_tag(id: &str, path: &str, reason: &'static str) -> JexStageError {
    JexStageError::UnsupportedTag {
        source_id: id.to_owned(),
        source_path: path.to_owned(),
        reason,
    }
}

pub(super) fn blocked_relation(id: &str, path: &str, reason: &'static str) -> JexStageError {
    JexStageError::UnsupportedRelation {
        source_id: id.to_owned(),
        source_path: path.to_owned(),
        reason,
    }
}

fn source_times(
    props: &BTreeMap<String, String>,
    id: &str,
    path: &str,
    tag: bool,
) -> Result<(i64, i64, Option<i64>, Option<i64>), JexStageError> {
    let blocked = |reason| {
        if tag {
            blocked_tag(id, path, reason)
        } else {
            blocked_relation(id, path, reason)
        }
    };
    let required = |key| {
        props
            .get(key)
            .and_then(|value| parse_joplin_utc_millis(value))
            .ok_or_else(|| blocked("missing or invalid source timestamp"))
    };
    let optional = |key| match props.get(key) {
        None => Ok(None),
        Some(value) if is_optional_timestamp_default(value) => Ok(None),
        Some(value) => parse_joplin_utc_millis(value)
            .map(Some)
            .ok_or_else(|| blocked("invalid optional source timestamp")),
    };
    Ok((
        required("created_time")?,
        required("updated_time")?,
        optional("user_created_time")?,
        optional("user_updated_time")?,
    ))
}

fn safe_title(content: &str, id: &str, path: &str) -> Result<String, JexStageError> {
    let separator = if content.contains("\r\n") {
        "\r\n\r\n"
    } else {
        "\n\n"
    };
    if content.matches(separator).count() != 1
        || (separator == "\r\n\r\n"
            && content.as_bytes().iter().enumerate().any(|(index, byte)| {
                *byte == b'\n' && (index == 0 || content.as_bytes()[index - 1] != b'\r')
            }))
    {
        return Err(blocked_tag(
            id,
            path,
            "tag body or mixed line endings would be discarded",
        ));
    }
    let (title, _) = content
        .split_once(separator)
        .ok_or_else(|| blocked_tag(id, path, "tag title is missing"))?;
    if title.is_empty()
        || title != title.trim()
        || title.chars().count() > 255
        || title.chars().any(char::is_control)
        || title.contains(['\n', '\r'])
    {
        return Err(blocked_tag(id, path, "tag title is unsafe or ambiguous"));
    }
    Ok(title.to_owned())
}

fn zero_or_empty(props: &BTreeMap<String, String>, key: &str) -> bool {
    props
        .get(key)
        .is_none_or(|value| value.is_empty() || value == "0")
}

fn parse_tag(raw: &JexRawSourceItem) -> Result<SourceTag, JexStageError> {
    let id = &raw.source_id;
    let path = &raw.archive_path;
    let content = std::str::from_utf8(&raw.raw_bytes)
        .map_err(|_| blocked_tag(id, path, "tag metadata is not UTF-8"))?;
    let parsed = parse_item(path, content)
        .map_err(|_| blocked_tag(id, path, "tag metadata is malformed"))?;
    if parsed.item_type != 5 || !parsed.id.eq_ignore_ascii_case(id) {
        return Err(blocked_tag(id, path, "tag identity changed"));
    }
    let allowed = [
        "id",
        "type_",
        "parent_id",
        "created_time",
        "updated_time",
        "user_created_time",
        "user_updated_time",
        "encryption_applied",
        "is_shared",
        "user_data",
    ];
    if parsed.properties.iter().any(|(key, value)| {
        !allowed.contains(&key.as_str()) && !accepts_known_exporter_default(5, key, value)
    }) {
        return Err(blocked_tag(
            id,
            path,
            "tag metadata is outside the bounded mapping",
        ));
    }
    for key in ["parent_id", "user_data"] {
        if parsed
            .properties
            .get(key)
            .is_some_and(|value| !value.is_empty())
        {
            return Err(blocked_tag(
                id,
                path,
                "tag parent or user data is unsupported",
            ));
        }
    }
    for key in ["encryption_applied", "is_shared"] {
        if !zero_or_empty(&parsed.properties, key) {
            return Err(blocked_tag(
                id,
                path,
                "tag sharing or encryption is unsupported",
            ));
        }
    }
    let title = safe_title(content, id, path)?;
    let (created, updated, user_created, user_updated) =
        source_times(&parsed.properties, id, path, true)?;
    Ok(SourceTag {
        source_id: id.clone(),
        source_path: path.clone(),
        title,
        raw_sha256: raw.raw_sha256.clone(),
        created,
        updated,
        user_created,
        user_updated,
    })
}

fn parse_relation(raw: &JexRawSourceItem) -> Result<SourceRelation, JexStageError> {
    let id = &raw.source_id;
    let path = &raw.archive_path;
    let content = std::str::from_utf8(&raw.raw_bytes)
        .map_err(|_| blocked_relation(id, path, "relation metadata is not UTF-8"))?;
    let parsed = parse_item(path, content)
        .map_err(|_| blocked_relation(id, path, "relation metadata is malformed"))?;
    if parsed.item_type != 6 || !parsed.id.eq_ignore_ascii_case(id) {
        return Err(blocked_relation(id, path, "relation identity changed"));
    }
    let allowed = [
        "id",
        "type_",
        "note_id",
        "tag_id",
        "created_time",
        "updated_time",
        "user_created_time",
        "user_updated_time",
        "encryption_applied",
        "is_shared",
    ];
    if parsed.properties.iter().any(|(key, value)| {
        !allowed.contains(&key.as_str()) && !accepts_known_exporter_default(6, key, value)
    }) {
        return Err(blocked_relation(
            id,
            path,
            "relation metadata is outside the bounded mapping",
        ));
    }
    if !zero_or_empty(&parsed.properties, "encryption_applied")
        || !zero_or_empty(&parsed.properties, "is_shared")
    {
        return Err(blocked_relation(
            id,
            path,
            "encrypted or shared relation is unsupported",
        ));
    }
    // NoteTag is a property-only JEX item. Any blank line or extra title/body
    // would be ignored by BaseItem.unserialize and must not be lost here.
    let property_lines = content.trim_end_matches(['\r', '\n']);
    if property_lines.is_empty()
        || property_lines.lines().any(|line| line.trim().is_empty())
        || (content.contains("\r\n")
            && content.as_bytes().iter().enumerate().any(|(index, byte)| {
                *byte == b'\n' && (index == 0 || content.as_bytes()[index - 1] != b'\r')
            }))
    {
        return Err(blocked_relation(
            id,
            path,
            "relation title, body or mixed line endings would be discarded",
        ));
    }
    let endpoint = |key| {
        parsed
            .properties
            .get(key)
            .filter(|value| valid_joplin_id(value))
            .cloned()
            .ok_or_else(|| blocked_relation(id, path, "missing or invalid relation endpoint"))
    };
    let (created, updated, user_created, user_updated) =
        source_times(&parsed.properties, id, path, false)?;
    Ok(SourceRelation {
        source_id: id.clone(),
        source_path: path.clone(),
        note_id: endpoint("note_id")?,
        tag_id: endpoint("tag_id")?,
        raw_sha256: raw.raw_sha256.clone(),
        created,
        updated,
        user_created,
        user_updated,
    })
}

pub(super) fn validate_source_item(raw: &JexRawSourceItem) -> Result<(), JexStageError> {
    match raw.item_type {
        5 => parse_tag(raw).map(|_| ()),
        6 => parse_relation(raw).map(|_| ()),
        _ => Err(blocked_relation(
            &raw.source_id,
            &raw.archive_path,
            "source item is not a tag or relation",
        )),
    }
}

/// All relationships are validated before any library child is created.
pub(super) fn preflight(prepared: &JexPreparedSource) -> Result<TagPlan, JexStageError> {
    let source = prepared.report();
    let note_ids = source
        .source_ids
        .notes
        .iter()
        .map(|id| id.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let mut tags = Vec::new();
    let mut tag_ids = BTreeSet::new();
    let mut titles = BTreeSet::new();
    for id in &source.source_ids.tags {
        let raw = prepared
            .raw_item(id)?
            .ok_or_else(|| JexStageError::Verification("verified tag disappeared".into()))?;
        let tag = parse_tag(&raw)?;
        if !titles.insert(tag.title.to_lowercase()) {
            return Err(blocked_tag(
                id,
                &raw.archive_path,
                "distinct source tags have a colliding title",
            ));
        }
        tag_ids.insert(id.to_ascii_lowercase());
        tags.push(tag);
    }
    let mut relations = Vec::new();
    let mut pairs = BTreeSet::new();
    for id in &source.source_ids.note_tag_relations {
        let raw = prepared
            .raw_item(id)?
            .ok_or_else(|| JexStageError::Verification("verified relation disappeared".into()))?;
        let relation = parse_relation(&raw)?;
        let pair = (
            relation.note_id.to_ascii_lowercase(),
            relation.tag_id.to_ascii_lowercase(),
        );
        if !note_ids.contains(&pair.0) || !tag_ids.contains(&pair.1) {
            return Err(blocked_relation(
                id,
                &raw.archive_path,
                "relation endpoint is missing",
            ));
        }
        if !pairs.insert(pair) {
            return Err(blocked_relation(
                id,
                &raw.archive_path,
                "duplicate note-tag relation pair",
            ));
        }
        relations.push(relation);
    }
    Ok(TagPlan { tags, relations })
}

pub(super) fn create_tags(
    plan: &TagPlan,
    prepared: &JexPreparedSource,
    repo: &LibraryRepository,
    audit: &Connection,
    report: &mut JexStageReport,
) -> Result<BTreeMap<String, TagId>, JexStageError> {
    let mut mapped = BTreeMap::new();
    for tag in &plan.tags {
        let raw = prepared.raw_item(&tag.source_id)?.ok_or_else(|| {
            JexStageError::Verification("verified tag disappeared during creation".into())
        })?;
        if raw.raw_sha256 != tag.raw_sha256 {
            return Err(JexStageError::Verification(
                "tag source digest changed".into(),
            ));
        }
        let dest = repo.create_tag(&tag.title)?;
        audit.execute(
            "UPDATE tags SET created_time=?2,updated_time=?3 WHERE id=?1",
            params![dest.id.as_str(), tag.created, tag.updated],
        )?;
        audit.execute("INSERT INTO jex_stage_tag_audit
            (source_id,source_path,tag_id,title,raw_item_bytes,raw_sha256,created_time,updated_time,user_created_time,user_updated_time)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![tag.source_id,tag.source_path,dest.id.as_str(),tag.title,raw.raw_bytes,tag.raw_sha256,
                tag.created,tag.updated,tag.user_created,tag.user_updated])?;
        mapped.insert(tag.source_id.to_ascii_lowercase(), dest.id.clone());
        report.tags.push(JexStagedTag {
            source_id: tag.source_id.clone(),
            source_path: tag.source_path.clone(),
            title: tag.title.clone(),
            destination_id: dest.id,
            raw_sha256: tag.raw_sha256.clone(),
            created_time: tag.created,
            updated_time: tag.updated,
            user_created_time: tag.user_created,
            user_updated_time: tag.user_updated,
        });
    }
    Ok(mapped)
}

pub(super) fn create_relations(
    plan: &TagPlan,
    prepared: &JexPreparedSource,
    repo: &LibraryRepository,
    audit: &Connection,
    report: &mut JexStageReport,
    tag_map: &BTreeMap<String, TagId>,
) -> Result<(), JexStageError> {
    let note_map = report
        .notes
        .iter()
        .map(|note| {
            (
                note.source_id.to_ascii_lowercase(),
                note.destination_id.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for relation in &plan.relations {
        let raw = prepared.raw_item(&relation.source_id)?.ok_or_else(|| {
            JexStageError::Verification("verified relation disappeared during creation".into())
        })?;
        if raw.raw_sha256 != relation.raw_sha256 {
            return Err(JexStageError::Verification(
                "relation source digest changed".into(),
            ));
        }
        let note_id = note_map
            .get(&relation.note_id.to_ascii_lowercase())
            .ok_or_else(|| {
                blocked_relation(
                    &relation.source_id,
                    &relation.source_path,
                    "destination note is missing",
                )
            })?;
        let tag_id = tag_map
            .get(&relation.tag_id.to_ascii_lowercase())
            .ok_or_else(|| {
                blocked_relation(
                    &relation.source_id,
                    &relation.source_path,
                    "destination tag is missing",
                )
            })?;
        repo.add_note_tag(note_id, tag_id)?;
        audit.execute("INSERT INTO jex_stage_relation_audit
            (source_id,source_path,source_note_id,source_tag_id,note_id,tag_id,raw_item_bytes,raw_sha256,created_time,updated_time,user_created_time,user_updated_time)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![relation.source_id,relation.source_path,relation.note_id,relation.tag_id,note_id.as_str(),tag_id.as_str(),
                raw.raw_bytes,relation.raw_sha256,relation.created,relation.updated,relation.user_created,relation.user_updated])?;
        report.relations.push(JexStagedRelation {
            source_id: relation.source_id.clone(),
            source_path: relation.source_path.clone(),
            source_note_id: relation.note_id.clone(),
            source_tag_id: relation.tag_id.clone(),
            destination_note_id: note_id.clone(),
            destination_tag_id: tag_id.clone(),
            raw_sha256: relation.raw_sha256.clone(),
            created_time: relation.created,
            updated_time: relation.updated,
            user_created_time: relation.user_created,
            user_updated_time: relation.user_updated,
        });
    }
    Ok(())
}

pub(super) fn verify_all(db: &Connection, report: &JexStageReport) -> Result<(), JexStageError> {
    let note_destinations = report
        .notes
        .iter()
        .map(|note| {
            (
                note.source_id.to_ascii_lowercase(),
                note.destination_id.as_str(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let tag_destinations = report
        .tags
        .iter()
        .map(|tag| {
            (
                tag.source_id.to_ascii_lowercase(),
                tag.destination_id.as_str(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for tag in &report.tags {
        let (path,title,raw,sha,created,updated,user_created,user_updated): (String,String,Vec<u8>,String,i64,i64,Option<i64>,Option<i64>) = db.query_row(
            "SELECT source_path,title,raw_item_bytes,raw_sha256,created_time,updated_time,user_created_time,user_updated_time
             FROM jex_stage_tag_audit WHERE source_id=?1 AND tag_id=?2",
            params![tag.source_id,tag.destination_id.as_str()],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?)),
        )?;
        if path != tag.source_path
            || title != tag.title
            || sha != tag.raw_sha256
            || format!("{:x}", Sha256::digest(&raw)) != sha
            || created != tag.created_time
            || updated != tag.updated_time
            || user_created != tag.user_created_time
            || user_updated != tag.user_updated_time
        {
            return Err(JexStageError::Verification(
                "tag source audit differs".into(),
            ));
        }
        let replay = parse_tag(&JexRawSourceItem {
            archive_path: path,
            source_id: tag.source_id.clone(),
            item_type: 5,
            byte_count: raw.len() as u64,
            raw_sha256: sha,
            canonical_note_body_sha256: None,
            raw_bytes: raw,
        })?;
        if replay.title != title
            || replay.created != created
            || replay.updated != updated
            || replay.user_created != user_created
            || replay.user_updated != user_updated
        {
            return Err(JexStageError::Verification(
                "replayed tag metadata differs".into(),
            ));
        }
        let (stored_title, stored_created, stored_updated): (String, i64, i64) = db.query_row(
            "SELECT title,created_time,updated_time FROM tags WHERE id=?1 AND deleted_time=0",
            [tag.destination_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if stored_title != title || stored_created != created || stored_updated != updated {
            return Err(JexStageError::Verification("reopened tag differs".into()));
        }
    }
    for relation in &report.relations {
        if note_destinations.get(&relation.source_note_id.to_ascii_lowercase())
            != Some(&relation.destination_note_id.as_str())
            || tag_destinations.get(&relation.source_tag_id.to_ascii_lowercase())
                != Some(&relation.destination_tag_id.as_str())
        {
            return Err(JexStageError::Verification(
                "relation endpoint destination map differs".into(),
            ));
        }
        let (path,source_note,source_tag,note_id,tag_id,raw,sha,created,updated,user_created,user_updated):
            (String,String,String,String,String,Vec<u8>,String,i64,i64,Option<i64>,Option<i64>) = db.query_row(
            "SELECT source_path,source_note_id,source_tag_id,note_id,tag_id,raw_item_bytes,raw_sha256,created_time,updated_time,user_created_time,user_updated_time
             FROM jex_stage_relation_audit WHERE source_id=?1", [&relation.source_id],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?,row.get(10)?)),
        )?;
        if path != relation.source_path
            || source_note != relation.source_note_id
            || source_tag != relation.source_tag_id
            || note_id != relation.destination_note_id.as_str()
            || tag_id != relation.destination_tag_id.as_str()
            || sha != relation.raw_sha256
            || format!("{:x}", Sha256::digest(&raw)) != sha
            || created != relation.created_time
            || updated != relation.updated_time
            || user_created != relation.user_created_time
            || user_updated != relation.user_updated_time
        {
            return Err(JexStageError::Verification(
                "relation source audit differs".into(),
            ));
        }
        let replay = parse_relation(&JexRawSourceItem {
            archive_path: path,
            source_id: relation.source_id.clone(),
            item_type: 6,
            byte_count: raw.len() as u64,
            raw_sha256: sha,
            canonical_note_body_sha256: None,
            raw_bytes: raw,
        })?;
        if replay.note_id != source_note
            || replay.tag_id != source_tag
            || replay.created != created
            || replay.updated != updated
            || replay.user_created != user_created
            || replay.user_updated != user_updated
        {
            return Err(JexStageError::Verification(
                "replayed relation differs".into(),
            ));
        }
        let exists: i64 = db.query_row(
            "SELECT count(*) FROM note_tags WHERE note_id=?1 AND tag_id=?2",
            params![note_id, tag_id],
            |row| row.get(0),
        )?;
        if exists != 1 {
            return Err(JexStageError::Verification(
                "reopened note-tag pair differs".into(),
            ));
        }
    }
    Ok(())
}
