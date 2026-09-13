//! Conservative two-level JEX folder graph for an owned isolated profile.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};

use crate::{LibraryRepository, NotebookId, StackId};

use super::super::{JexPreparedSource, JexRawSourceItem, parse_item, valid_joplin_id};
use super::{
    JexFolderDestination, JexStageError, JexStageReport, JexStagedFolder,
    accepts_known_exporter_default, canonical_exporter_properties, invalid_note,
    is_optional_timestamp_default, parse_joplin_utc_millis, parse_note,
};

#[derive(Clone)]
struct Folder {
    source_id: String,
    source_path: String,
    parent_id: String,
    title: String,
    created: i64,
    updated: i64,
    raw_sha256: String,
    has_children: bool,
}

pub(super) struct FolderPlan {
    folders: BTreeMap<String, Folder>,
}

fn blocked(id: &str, path: &str, reason: &'static str) -> JexStageError {
    JexStageError::UnsupportedFolder {
        source_id: id.to_owned(),
        source_path: path.to_owned(),
        reason,
    }
}

fn parse_folder(raw: &JexRawSourceItem) -> Result<Folder, JexStageError> {
    let id = &raw.source_id;
    let path = &raw.archive_path;
    let content = std::str::from_utf8(&raw.raw_bytes)
        .map_err(|_| blocked(id, path, "folder metadata is not UTF-8"))?;
    let parsed =
        parse_item(path, content).map_err(|_| blocked(id, path, "folder metadata is malformed"))?;
    if parsed.item_type != 2 || !parsed.id.eq_ignore_ascii_case(id) {
        return Err(blocked(id, path, "folder identity changed"));
    }
    if !canonical_exporter_properties(&parsed) {
        return Err(blocked(
            id,
            path,
            "folder property syntax or value whitespace is not exporter-canonical",
        ));
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
    ];
    if parsed.properties.iter().any(|(key, value)| {
        (!allowed.contains(&key.as_str()) && !accepts_known_exporter_default(2, key, value))
            || (matches!(key.as_str(), "user_created_time" | "user_updated_time")
                && !is_optional_timestamp_default(value))
    }) {
        return Err(blocked(
            id,
            path,
            "folder metadata is outside the two-level mapping",
        ));
    }
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
        return Err(blocked(
            id,
            path,
            "folder body or mixed line endings would be discarded",
        ));
    }
    let (title, _) = content
        .split_once(separator)
        .ok_or_else(|| blocked(id, path, "folder title is missing"))?;
    if title.is_empty()
        || title != title.trim()
        || title.chars().count() > 255
        || title.chars().any(char::is_control)
        || title.contains(['\n', '\r'])
    {
        return Err(blocked(id, path, "folder title is unsafe or ambiguous"));
    }
    let parent_id = parsed
        .properties
        .get("parent_id")
        .ok_or_else(|| blocked(id, path, "folder parent_id is missing"))?
        .clone();
    if !parent_id.is_empty() && !valid_joplin_id(&parent_id) {
        return Err(blocked(id, path, "folder parent_id is malformed"));
    }
    if parsed
        .properties
        .get("encryption_applied")
        .is_some_and(|value| value != "0" && !value.is_empty())
    {
        return Err(blocked(id, path, "encrypted folder is unsupported"));
    }
    let time = |key: &str| {
        parsed
            .properties
            .get(key)
            .and_then(|raw| parse_joplin_utc_millis(raw))
            .ok_or_else(|| blocked(id, path, "missing or invalid folder timestamp"))
    };
    Ok(Folder {
        source_id: id.clone(),
        source_path: path.clone(),
        parent_id,
        title: title.to_owned(),
        created: time("created_time")?,
        updated: time("updated_time")?,
        raw_sha256: raw.raw_sha256.clone(),
        has_children: false,
    })
}

pub(super) fn validate_source_item(raw: &JexRawSourceItem) -> Result<(), JexStageError> {
    parse_folder(raw).map(|_| ())
}

/// Reads one bounded raw item at a time and rejects any folder topology that
/// cannot be represented exactly by root notebooks or stack/child notebooks.
pub(super) fn preflight(prepared: &JexPreparedSource) -> Result<FolderPlan, JexStageError> {
    let mut folders = BTreeMap::new();
    for source_id in &prepared.report().source_ids.folders {
        let raw = prepared
            .raw_item(source_id)?
            .ok_or_else(|| JexStageError::Verification("verified folder disappeared".into()))?;
        let folder = parse_folder(&raw)?;
        folders.insert(source_id.to_ascii_lowercase(), folder);
    }
    let mut sibling_titles = BTreeSet::new();
    for folder in folders.values() {
        let parent = folder.parent_id.to_ascii_lowercase();
        if !sibling_titles.insert((parent, folder.title.to_lowercase())) {
            return Err(blocked(
                &folder.source_id,
                &folder.source_path,
                "duplicate sibling folder title",
            ));
        }
        let mut seen = BTreeSet::new();
        let mut cursor = folder.source_id.to_ascii_lowercase();
        let mut depth = 0;
        loop {
            if !seen.insert(cursor.clone()) {
                return Err(blocked(
                    &folder.source_id,
                    &folder.source_path,
                    "folder parent cycle",
                ));
            }
            let current = &folders[&cursor];
            if current.parent_id.is_empty() {
                break;
            }
            let parent = current.parent_id.to_ascii_lowercase();
            if !folders.contains_key(&parent) {
                return Err(blocked(
                    &folder.source_id,
                    &folder.source_path,
                    "folder parent is missing",
                ));
            }
            depth += 1;
            if depth > folders.len() {
                return Err(blocked(
                    &folder.source_id,
                    &folder.source_path,
                    "folder parent cycle",
                ));
            }
            cursor = parent;
        }
        if depth > 1 {
            return Err(blocked(
                &folder.source_id,
                &folder.source_path,
                "folder depth exceeds native two-level model",
            ));
        }
    }
    let children = folders
        .values()
        .filter(|folder| !folder.parent_id.is_empty())
        .map(|folder| folder.parent_id.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    for parent_id in children {
        folders
            .get_mut(&parent_id)
            .expect("parent validated")
            .has_children = true;
    }
    let mut note_counts = BTreeMap::<String, usize>::new();
    for source_id in &prepared.report().source_ids.notes {
        let raw = prepared
            .raw_item(source_id)?
            .ok_or_else(|| JexStageError::Verification("verified note disappeared".into()))?;
        let note = parse_note(source_id, &raw.archive_path, &raw.raw_bytes)?;
        if !note.parent_id.is_empty() {
            let parent = note.parent_id.to_ascii_lowercase();
            if !folders.contains_key(&parent) {
                return Err(invalid_note(
                    source_id,
                    &raw.archive_path,
                    "source note parent folder is missing",
                ));
            }
            *note_counts.entry(parent).or_default() += 1;
        }
    }
    for (id, folder) in &folders {
        if folder.has_children && note_counts.get(id).is_some_and(|count| *count > 0) {
            return Err(blocked(
                &folder.source_id,
                &folder.source_path,
                "folder has both child folders and own notes",
            ));
        }
    }
    Ok(FolderPlan { folders })
}

fn create_one(
    prepared: &JexPreparedSource,
    folder: &Folder,
    target: JexFolderDestination,
    audit: &Connection,
    report: &mut JexStageReport,
) -> Result<(), JexStageError> {
    let raw = prepared.raw_item(&folder.source_id)?.ok_or_else(|| {
        JexStageError::Verification("verified folder disappeared during creation".into())
    })?;
    if raw.raw_sha256 != folder.raw_sha256 {
        return Err(JexStageError::Verification(
            "folder source digest changed".into(),
        ));
    }
    let (kind, id, table) = match &target {
        JexFolderDestination::Stack(id) => ("stack", id.as_str(), "stacks"),
        JexFolderDestination::Notebook(id) => ("notebook", id.as_str(), "notebooks"),
    };
    audit.execute("INSERT INTO jex_stage_folder_audit
        (source_id,source_path,source_parent_id,title,raw_item_bytes,raw_sha256,destination_kind,destination_id,created_time,updated_time)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![folder.source_id,folder.source_path,folder.parent_id,folder.title,raw.raw_bytes,folder.raw_sha256,kind,id,folder.created,folder.updated])?;
    audit.execute(
        &format!("UPDATE {table} SET created_time=?2,updated_time=?3 WHERE id=?1"),
        params![id, folder.created, folder.updated],
    )?;
    report.folders.push(JexStagedFolder {
        source_id: folder.source_id.clone(),
        source_path: folder.source_path.clone(),
        source_parent_id: folder.parent_id.clone(),
        title: folder.title.clone(),
        destination: target,
        raw_sha256: folder.raw_sha256.clone(),
        created_time: folder.created,
        updated_time: folder.updated,
    });
    Ok(())
}

/// Creates stacks first, then their leaf notebooks and independent root leaves.
/// The returned normalized map contains only note-bearing notebook targets.
pub(super) fn create_all(
    plan: &FolderPlan,
    prepared: &JexPreparedSource,
    repo: &LibraryRepository,
    audit: &Connection,
    report: &mut JexStageReport,
) -> Result<BTreeMap<String, NotebookId>, JexStageError> {
    let mut stacks = BTreeMap::<String, StackId>::new();
    let mut notebooks = BTreeMap::new();
    for (key, folder) in &plan.folders {
        if folder.parent_id.is_empty() && folder.has_children {
            let stack = repo.create_stack(&folder.title)?;
            create_one(
                prepared,
                folder,
                JexFolderDestination::Stack(stack.id.clone()),
                audit,
                report,
            )?;
            stacks.insert(key.clone(), stack.id);
        }
    }
    for (key, folder) in &plan.folders {
        if !folder.has_children {
            let stack = if folder.parent_id.is_empty() {
                None
            } else {
                Some(
                    stacks
                        .get(&folder.parent_id.to_ascii_lowercase())
                        .ok_or_else(|| {
                            blocked(
                                &folder.source_id,
                                &folder.source_path,
                                "child parent is not a stack",
                            )
                        })?,
                )
            };
            let notebook = repo.create_notebook(&folder.title, stack)?;
            create_one(
                prepared,
                folder,
                JexFolderDestination::Notebook(notebook.id.clone()),
                audit,
                report,
            )?;
            notebooks.insert(key.clone(), notebook.id);
        }
    }
    report.folders.sort_by(|a, b| a.source_id.cmp(&b.source_id));
    Ok(notebooks)
}

pub(super) fn verify_one(
    db: &Connection,
    folder: &JexStagedFolder,
    mapped: &BTreeMap<String, JexFolderDestination>,
) -> Result<(), JexStageError> {
    let (source_path,parent,title,raw,sha,kind,id,created,updated): (String,String,String,Vec<u8>,String,String,String,i64,i64) = db.query_row(
        "SELECT source_path,source_parent_id,title,raw_item_bytes,raw_sha256,destination_kind,destination_id,created_time,updated_time
         FROM jex_stage_folder_audit WHERE source_id=?1", [&folder.source_id],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?)),
    )?;
    if source_path != folder.source_path
        || parent != folder.source_parent_id
        || title != folder.title
        || sha != folder.raw_sha256
        || format!("{:x}", Sha256::digest(&raw)) != sha
        || created != folder.created_time
        || updated != folder.updated_time
    {
        return Err(JexStageError::Verification(
            "folder source audit differs".into(),
        ));
    }
    let replay = parse_folder(&JexRawSourceItem {
        archive_path: source_path,
        source_id: folder.source_id.clone(),
        item_type: 2,
        byte_count: raw.len() as u64,
        raw_sha256: sha,
        canonical_note_body_sha256: None,
        raw_bytes: raw,
    })?;
    if replay.title != title
        || replay.parent_id != parent
        || replay.created != created
        || replay.updated != updated
    {
        return Err(JexStageError::Verification(
            "folder metadata replay differs".into(),
        ));
    }
    match &folder.destination {
        JexFolderDestination::Stack(dest) => {
            if kind != "stack" || id != dest.as_str() || !parent.is_empty() {
                return Err(JexStageError::Verification(
                    "folder stack mapping differs".into(),
                ));
            }
            let (stored_title, stored_created, stored_updated): (String, i64, i64) = db.query_row(
                "SELECT title,created_time,updated_time FROM stacks WHERE id=?1 AND deleted_time=0",
                [dest.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            if stored_title != title || stored_created != created || stored_updated != updated {
                return Err(JexStageError::Verification("reopened stack differs".into()));
            }
        }
        JexFolderDestination::Notebook(dest) => {
            if kind != "notebook" || id != dest.as_str() {
                return Err(JexStageError::Verification(
                    "folder notebook mapping differs".into(),
                ));
            }
            let (stored_title,stored_stack,stored_created,stored_updated): (String,Option<String>,i64,i64) = db.query_row(
                "SELECT title,stack_id,created_time,updated_time FROM notebooks WHERE id=?1 AND deleted_time=0", [dest.as_str()],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
            )?;
            let expected_stack = if parent.is_empty() {
                None
            } else {
                match mapped.get(&parent.to_ascii_lowercase()) {
                    Some(JexFolderDestination::Stack(id)) => Some(id.as_str().to_owned()),
                    _ => {
                        return Err(JexStageError::Verification(
                            "notebook source parent is not mapped stack".into(),
                        ));
                    }
                }
            };
            if stored_title != title
                || stored_stack != expected_stack
                || stored_created != created
                || stored_updated != updated
            {
                return Err(JexStageError::Verification(
                    "reopened notebook differs".into(),
                ));
            }
        }
    }
    Ok(())
}
