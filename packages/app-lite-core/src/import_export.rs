//! Bounded, read-only preflight support for portable Joplin exports.
//!
//! A JEX file is a tar archive produced from Joplin's raw exporter.  This
//! module deliberately has no repository dependency: it never extracts files
//! to disk and never opens a profile database.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{self, Read},
    path::Path,
};

use sha2::{Digest, Sha256};
use tar::{Archive, EntryType};
use thiserror::Error;

mod enex;
pub use enex::*;

/// Maximum number of tar entries accepted by a preflight scan.
pub const MAX_JEX_ARCHIVE_ENTRIES: usize = 50_000;
/// Maximum bytes held while parsing one serialized metadata item.
pub const MAX_JEX_ITEM_BYTES: u64 = 4 * 1024 * 1024;
/// Maximum bytes accepted for one physical resource file.
pub const MAX_JEX_RESOURCE_BYTES: u64 = 512 * 1024 * 1024;
/// Maximum canonical internal resource references retained across note bodies.
pub const MAX_JEX_NOTE_RESOURCE_REFERENCES: usize = 50_000;
const STREAM_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Error)]
pub enum JexScanError {
    #[error("cannot read JEX archive: {0}")]
    Io(#[from] io::Error),
    #[error("unsafe archive path: {0}")]
    UnsafeArchivePath(String),
    #[error("unsupported tar entry type at {path}: {kind}")]
    UnsafeArchiveEntry { path: String, kind: String },
    #[error("duplicate archive path: {0}")]
    DuplicateArchivePath(String),
    #[error("unexpected JEX archive path: {0}")]
    UnexpectedArchivePath(String),
    #[error("JEX archive contains more than {MAX_JEX_ARCHIVE_ENTRIES} entries")]
    TooManyEntries,
    #[error("JEX item is too large at {path}: {size} bytes (maximum {MAX_JEX_ITEM_BYTES})")]
    ItemTooLarge { path: String, size: u64 },
    #[error("JEX resource is too large at {path}: {size} bytes (maximum {MAX_JEX_RESOURCE_BYTES})")]
    ResourceTooLarge { path: String, size: u64 },
    #[error("malformed JEX item at {path}: {reason}")]
    MalformedItem { path: String, reason: String },
    #[error(
        "JEX archive contains more than {MAX_JEX_NOTE_RESOURCE_REFERENCES} note-body resource references"
    )]
    TooManyNoteBodyResourceReferences,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JexScanCounts {
    pub notes: usize,
    pub folders: usize,
    pub resource_metadata: usize,
    pub tags: usize,
    pub note_tag_relations: usize,
    pub physical_resource_files: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JexSourceIds {
    pub notes: Vec<String>,
    pub folders: Vec<String>,
    pub resource_metadata: Vec<String>,
    pub tags: Vec<String>,
    pub note_tag_relations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JexScannedResource {
    pub source_id: String,
    pub archive_path: String,
    pub byte_count: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JexPhysicalResourceFile {
    pub source_id: String,
    pub archive_path: String,
    pub byte_count: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JexStoreCompatibilityBlocker {
    pub source_id: String,
    pub archive_path: String,
    pub byte_count: u64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JexUnsupportedItem {
    pub source_id: String,
    pub item_type: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JexNoteBodyResourceReference {
    pub note_id: String,
    pub resource_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JexScanReport {
    pub counts: JexScanCounts,
    pub source_ids: JexSourceIds,
    pub resources: Vec<JexScannedResource>,
    pub physical_resource_files: Vec<JexPhysicalResourceFile>,
    /// Resources that are structurally valid JEX entries but exceed the
    /// currently configured ResourceStore import limit.
    pub store_compatibility_blockers: Vec<JexStoreCompatibilityBlocker>,
    pub duplicate_archive_paths: Vec<String>,
    pub duplicate_item_ids: Vec<String>,
    pub missing_resource_files: Vec<String>,
    pub orphan_note_tag_relations: Vec<String>,
    pub orphan_physical_resource_files: Vec<String>,
    pub note_body_resource_references: Vec<JexNoteBodyResourceReference>,
    /// Canonical `:/<id>` refs with no exported note or resource metadata.
    pub unresolved_note_body_internal_references: Vec<JexNoteBodyResourceReference>,
    pub unsupported_items: Vec<JexUnsupportedItem>,
    pub encrypted_item_ids: Vec<String>,
}

impl JexScanReport {
    /// A clean scan is structurally safe and contains no skipped or incomplete
    /// source archive entities. It validates the bounded canonical internal
    /// references it recognizes, but it does not prove full note-body fidelity
    /// or anything about a later import operation.
    pub fn is_clean(&self) -> bool {
        self.duplicate_archive_paths.is_empty()
            && self.duplicate_item_ids.is_empty()
            && self.missing_resource_files.is_empty()
            && self.orphan_note_tag_relations.is_empty()
            && self.orphan_physical_resource_files.is_empty()
            && self.unresolved_note_body_internal_references.is_empty()
            && self.unsupported_items.is_empty()
            && self.encrypted_item_ids.is_empty()
            && self.store_compatibility_blockers.is_empty()
    }
}

#[derive(Debug)]
struct ParsedItem {
    id: String,
    normalized_id: String,
    item_type: i64,
    properties: BTreeMap<String, String>,
    note_body: String,
}

#[derive(Debug)]
struct ResourceMetadata {
    source_id: String,
    archive_path: String,
    mime: String,
}

#[derive(Debug)]
struct NoteTagRelation {
    source_id: String,
    note_id: String,
    tag_id: String,
}

#[derive(Debug)]
struct PhysicalResource {
    source_id: String,
    byte_count: u64,
    sha256: String,
}

/// Scans a JEX tar archive without extracting it or touching a Joplin profile.
///
/// Metadata files are bounded at [`MAX_JEX_ITEM_BYTES`]. Resource files are
/// hashed in fixed-size chunks and bounded at [`MAX_JEX_RESOURCE_BYTES`].
pub fn scan_jex_archive(path: impl AsRef<Path>) -> Result<JexScanReport, JexScanError> {
    let file = File::open(path)?;
    let mut archive = Archive::new(file);
    let mut report = JexScanReport::default();
    let mut seen_paths = BTreeSet::new();
    let mut seen_ids = BTreeSet::new();
    let mut metadata_resources = Vec::new();
    let mut physical_resources = BTreeMap::new();
    let mut note_tag_relations = Vec::new();
    let mut note_ids = BTreeSet::new();
    let mut tag_ids = BTreeSet::new();
    let mut resource_ids = BTreeSet::new();
    let mut known_item_ids = BTreeSet::new();
    let mut pending_note_body_references = Vec::new();

    for (index, entry) in archive.entries()?.raw(true).enumerate() {
        if index >= MAX_JEX_ARCHIVE_ENTRIES {
            return Err(JexScanError::TooManyEntries);
        }
        let mut entry = entry?;
        let entry_type = entry.header().entry_type();
        // Raw iteration exposes extension records before tar can materialize
        // their payload. JEX's fixed ASCII paths never require them.
        if !entry_type.is_file() && !entry_type.is_dir() {
            return Err(JexScanError::UnsafeArchiveEntry {
                path: raw_archive_path_for_error(&entry),
                kind: tar_entry_kind(entry_type),
            });
        }
        let archive_path = checked_archive_path(&entry)?;
        if entry_type.is_dir() {
            if archive_path != "resources" && archive_path != "resources/" {
                return Err(JexScanError::UnexpectedArchivePath(archive_path));
            }
            continue;
        }
        if !seen_paths.insert(archive_path.clone()) {
            return Err(JexScanError::DuplicateArchivePath(archive_path));
        }

        if let Some(id) = root_item_id(&archive_path) {
            let item = parse_item(&archive_path, read_item(&mut entry, &archive_path)?)?;
            if item.normalized_id != id.to_ascii_lowercase() {
                return Err(JexScanError::MalformedItem {
                    path: archive_path,
                    reason: "metadata id does not match its root file name".to_owned(),
                });
            }
            if !seen_ids.insert(item.normalized_id.clone()) {
                report.duplicate_item_ids.push(item.id.clone());
            }
            known_item_ids.insert(item.normalized_id.clone());
            if is_encrypted(&item.properties) {
                report.encrypted_item_ids.push(item.id);
                continue;
            }
            match item.item_type {
                1 => {
                    note_ids.insert(item.normalized_id);
                    let remaining = MAX_JEX_NOTE_RESOURCE_REFERENCES
                        .checked_sub(pending_note_body_references.len())
                        .ok_or(JexScanError::TooManyNoteBodyResourceReferences)?;
                    let references = extract_canonical_resource_refs(&item.note_body, remaining)?;
                    pending_note_body_references.extend(references.into_iter().map(
                        |resource_id| JexNoteBodyResourceReference {
                            note_id: item.id.clone(),
                            resource_id,
                        },
                    ));
                    add_id(
                        &mut report.source_ids.notes,
                        &mut report.counts.notes,
                        item.id,
                    );
                }
                2 => add_id(
                    &mut report.source_ids.folders,
                    &mut report.counts.folders,
                    item.id,
                ),
                4 => {
                    resource_ids.insert(item.normalized_id.clone());
                    let archive_path = resource_archive_path(&item, &archive_path)?;
                    metadata_resources.push(ResourceMetadata {
                        source_id: item.id.clone(),
                        archive_path,
                        mime: item.properties.get("mime").cloned().unwrap_or_default(),
                    });
                    add_id(
                        &mut report.source_ids.resource_metadata,
                        &mut report.counts.resource_metadata,
                        item.id,
                    );
                }
                5 => {
                    tag_ids.insert(item.normalized_id);
                    add_id(
                        &mut report.source_ids.tags,
                        &mut report.counts.tags,
                        item.id,
                    );
                }
                6 => {
                    validate_note_tag(&item, &archive_path)?;
                    note_tag_relations.push(NoteTagRelation {
                        source_id: item.id.clone(),
                        note_id: item.properties["note_id"].to_ascii_lowercase(),
                        tag_id: item.properties["tag_id"].to_ascii_lowercase(),
                    });
                    add_id(
                        &mut report.source_ids.note_tag_relations,
                        &mut report.counts.note_tag_relations,
                        item.id,
                    );
                }
                item_type => report.unsupported_items.push(JexUnsupportedItem {
                    source_id: item.id,
                    item_type,
                }),
            }
        } else if is_resource_path(&archive_path) {
            let source_id = physical_resource_id(&archive_path)?;
            let mut physical = stream_resource(&mut entry, &archive_path)?;
            physical.source_id = source_id;
            report.counts.physical_resource_files += 1;
            physical_resources.insert(archive_path, physical);
        } else {
            return Err(JexScanError::UnexpectedArchivePath(archive_path));
        }
    }

    let mut resource_paths_with_metadata = BTreeSet::new();
    for metadata in metadata_resources {
        let archive_path = metadata.archive_path;
        if let Some(physical) = physical_resources.get(&archive_path) {
            resource_paths_with_metadata.insert(archive_path.clone());
            add_compatibility_blocker(
                &mut report,
                &metadata.source_id,
                &archive_path,
                physical.byte_count,
                resource_store_limit_for_mime(&metadata.mime),
            );
            report.resources.push(JexScannedResource {
                source_id: metadata.source_id,
                archive_path,
                byte_count: physical.byte_count,
                sha256: physical.sha256.clone(),
            });
        } else {
            report.missing_resource_files.push(archive_path);
        }
    }

    for relation in note_tag_relations {
        if !note_ids.contains(&relation.note_id) || !tag_ids.contains(&relation.tag_id) {
            report.orphan_note_tag_relations.push(relation.source_id);
        }
    }

    for reference in pending_note_body_references {
        if resource_ids.contains(&reference.resource_id.to_ascii_lowercase()) {
            report.note_body_resource_references.push(reference);
        } else if !known_item_ids.contains(&reference.resource_id.to_ascii_lowercase()) {
            report
                .unresolved_note_body_internal_references
                .push(reference);
        }
    }

    for (archive_path, physical) in physical_resources {
        if !resource_paths_with_metadata.contains(&archive_path) {
            report
                .orphan_physical_resource_files
                .push(archive_path.clone());
            add_compatibility_blocker(
                &mut report,
                &physical.source_id,
                &archive_path,
                physical.byte_count,
                crate::resource::MAX_RESOURCE_BYTES as u64,
            );
        }
        report
            .physical_resource_files
            .push(JexPhysicalResourceFile {
                source_id: physical.source_id,
                archive_path,
                byte_count: physical.byte_count,
                sha256: physical.sha256,
            });
    }

    sort_report(&mut report);
    Ok(report)
}

fn raw_archive_path_for_error<R: Read>(entry: &tar::Entry<'_, R>) -> String {
    String::from_utf8_lossy(entry.path_bytes().as_ref()).into_owned()
}

fn resource_store_limit_for_mime(mime: &str) -> u64 {
    if mime.to_ascii_lowercase().starts_with("image/") {
        crate::resource::MAX_IMAGE_BYTES as u64
    } else {
        crate::resource::MAX_RESOURCE_BYTES as u64
    }
}

fn add_compatibility_blocker(
    report: &mut JexScanReport,
    source_id: &str,
    archive_path: &str,
    byte_count: u64,
    limit: u64,
) {
    if byte_count == 0 || byte_count > limit {
        report
            .store_compatibility_blockers
            .push(JexStoreCompatibilityBlocker {
                source_id: source_id.to_owned(),
                archive_path: archive_path.to_owned(),
                byte_count,
                reason: if byte_count == 0 {
                    "zero-byte resource is rejected by the current ResourceStore".to_owned()
                } else {
                    format!("exceeds current ResourceStore limit of {limit} bytes")
                },
            });
    }
}

fn checked_archive_path<R: Read>(entry: &tar::Entry<'_, R>) -> Result<String, JexScanError> {
    let bytes = entry.path_bytes();
    let path = std::str::from_utf8(bytes.as_ref())
        .map_err(|_| JexScanError::UnsafeArchivePath("non-UTF-8 path".to_owned()))?;
    let normalized = path.strip_suffix('/').unwrap_or(path);
    if normalized.is_empty()
        || path.starts_with('/')
        || path.starts_with('\\')
        || normalized
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || path.contains('\\')
    {
        return Err(JexScanError::UnsafeArchivePath(path.to_owned()));
    }
    Ok(path.to_owned())
}

fn root_item_id(path: &str) -> Option<&str> {
    let id = path.strip_suffix(".md")?;
    (!id.contains('/')).then_some(id)
}

fn is_resource_path(path: &str) -> bool {
    path.strip_prefix("resources/")
        .is_some_and(|name| !name.is_empty() && !name.contains('/'))
}

fn physical_resource_id(path: &str) -> Result<String, JexScanError> {
    let filename = path
        .strip_prefix("resources/")
        .expect("resource path checked first");
    let id = filename.split_once('.').map_or(filename, |(id, _)| id);
    if !valid_joplin_id(id) {
        return Err(JexScanError::MalformedItem {
            path: path.to_owned(),
            reason: "resource file name does not begin with a 32-character hexadecimal source id"
                .to_owned(),
        });
    }
    Ok(id.to_owned())
}

fn read_item<R: Read>(entry: &mut tar::Entry<'_, R>, path: &str) -> Result<String, JexScanError> {
    let declared_size = entry.size();
    if declared_size > MAX_JEX_ITEM_BYTES {
        return Err(JexScanError::ItemTooLarge {
            path: path.to_owned(),
            size: declared_size,
        });
    }
    let mut bytes = Vec::with_capacity(declared_size as usize);
    entry.take(MAX_JEX_ITEM_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_JEX_ITEM_BYTES {
        return Err(JexScanError::ItemTooLarge {
            path: path.to_owned(),
            size: bytes.len() as u64,
        });
    }
    String::from_utf8(bytes).map_err(|_| JexScanError::MalformedItem {
        path: path.to_owned(),
        reason: "item is not valid UTF-8".to_owned(),
    })
}

fn parse_item(path: &str, content: String) -> Result<ParsedItem, JexScanError> {
    let lines: Vec<&str> = content.lines().collect();
    let properties_start = lines
        .iter()
        .rposition(|line| line.trim().is_empty())
        .map(|index| index + 1)
        .unwrap_or(0);
    let mut properties = BTreeMap::new();
    for line in &lines[properties_start..] {
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| JexScanError::MalformedItem {
                path: path.to_owned(),
                reason: format!("invalid property line: {line:?}"),
            })?;
        let key = key.trim();
        if key.is_empty() {
            return Err(JexScanError::MalformedItem {
                path: path.to_owned(),
                reason: "empty property name".to_owned(),
            });
        }
        if properties
            .insert(key.to_owned(), value.trim().to_owned())
            .is_some()
        {
            return Err(JexScanError::MalformedItem {
                path: path.to_owned(),
                reason: format!("duplicate property: {key}"),
            });
        }
    }
    let id = properties
        .get("id")
        .filter(|id| valid_joplin_id(id))
        .cloned()
        .ok_or_else(|| JexScanError::MalformedItem {
            path: path.to_owned(),
            reason: "missing or malformed id".to_owned(),
        })?;
    let item_type = properties
        .get("type_")
        .ok_or_else(|| JexScanError::MalformedItem {
            path: path.to_owned(),
            reason: "missing type_".to_owned(),
        })?
        .parse::<i64>()
        .map_err(|_| JexScanError::MalformedItem {
            path: path.to_owned(),
            reason: "malformed type_".to_owned(),
        })?;
    // Mirrors BaseItem.unserialize: title consumes the first two pre-property
    // lines, leaving the remainder as a Note body. Property-only NoteTag
    // files have no separator and therefore no body.
    let note_body = if item_type == 1 {
        let before_properties = &lines[..properties_start.saturating_sub(1)];
        before_properties
            .get(2..)
            .map(|lines| lines.join("\n"))
            .unwrap_or_default()
    } else {
        String::new()
    };
    Ok(ParsedItem {
        normalized_id: id.to_ascii_lowercase(),
        id,
        item_type,
        properties,
        note_body,
    })
}

fn extract_canonical_resource_refs(
    body: &str,
    limit: usize,
) -> Result<BTreeSet<String>, JexScanError> {
    let mut ids = BTreeSet::new();
    for marker in ["]("] {
        let mut remainder = body;
        while let Some(index) = remainder.find(marker) {
            let candidate = &remainder[index + marker.len()..];
            let end = candidate.find([')', '\n', '\r']).unwrap_or(candidate.len());
            if let Some(id) = parse_resource_url(candidate[..end].trim()) {
                insert_reference(&mut ids, id, limit)?;
            }
            remainder = &candidate[end..];
            if remainder.is_empty() {
                break;
            }
        }
    }
    let mut remainder = body;
    while let Some(index) = remainder.find("]:") {
        let candidate = &remainder[index + 2..];
        let end = candidate.find(['\n', '\r']).unwrap_or(candidate.len());
        if let Some(id) = parse_resource_url(candidate[..end].trim()) {
            insert_reference(&mut ids, id, limit)?;
        }
        remainder = &candidate[end..];
        if remainder.is_empty() {
            break;
        }
    }
    extract_html_resource_refs(body, &mut ids, limit)?;
    extract_jsoncanvas_resource_refs(body, &mut ids, limit)?;
    Ok(ids)
}

fn insert_reference(
    ids: &mut BTreeSet<String>,
    id: String,
    limit: usize,
) -> Result<(), JexScanError> {
    if !ids.contains(&id) && ids.len() >= limit {
        return Err(JexScanError::TooManyNoteBodyResourceReferences);
    }
    ids.insert(id);
    Ok(())
}

fn extract_html_resource_refs(
    body: &str,
    ids: &mut BTreeSet<String>,
    limit: usize,
) -> Result<(), JexScanError> {
    let lowercase = body.to_ascii_lowercase();
    for (tag_name, attribute) in [("img", "src"), ("a", "href")] {
        let mut offset = 0;
        let marker = format!("<{tag_name}");
        while let Some(found) = lowercase[offset..].find(&marker) {
            let start = offset + found;
            let after_name = start + marker.len();
            if lowercase
                .as_bytes()
                .get(after_name)
                .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'>' && *byte != b'/')
            {
                offset = after_name;
                continue;
            }
            let Some(end) = html_tag_end(body, after_name) else {
                break;
            };
            let tag = &body[after_name..end];
            if let Some(value) = html_attribute_value(tag, attribute) {
                if let Some(id) = parse_html_resource_url(value) {
                    insert_reference(ids, id, limit)?;
                }
            }
            offset = end + 1;
        }
    }
    Ok(())
}

fn html_tag_end(body: &str, start: usize) -> Option<usize> {
    let mut quote = None;
    for (offset, character) in body[start..].char_indices() {
        match (quote, character) {
            (None, '\'' | '"') => quote = Some(character),
            (Some(open), close) if open == close => quote = None,
            (None, '>') => return Some(start + offset),
            _ => {}
        }
    }
    None
}

fn html_attribute_value<'a>(tag: &'a str, attribute: &str) -> Option<&'a str> {
    let lowercase = tag.to_ascii_lowercase();
    let mut offset = 0;
    while let Some(found) = lowercase[offset..].find(attribute) {
        let start = offset + found;
        let preceding = lowercase.as_bytes().get(start.wrapping_sub(1));
        let after = start + attribute.len();
        if preceding
            .is_none_or(|byte| !byte.is_ascii_alphanumeric() && *byte != b'-' && *byte != b'_')
            && lowercase.as_bytes().get(after) == Some(&b'=')
            && matches!(
                lowercase.as_bytes().get(after + 1),
                Some(b'\'') | Some(b'"')
            )
        {
            let quote = lowercase.as_bytes()[after + 1] as char;
            let value_start = after + 2;
            let value_end = tag[value_start..].find(quote)? + value_start;
            return Some(&tag[value_start..value_end]);
        }
        offset = after;
    }
    None
}

fn parse_html_resource_url(value: &str) -> Option<String> {
    value
        .starts_with(":/")
        .then(|| parse_resource_url(value))
        .flatten()
}

fn parse_resource_url(value: &str) -> Option<String> {
    let value = value.trim();
    let id = value
        .strip_prefix(":/")
        .or_else(|| value.strip_prefix("joplin://"))?;
    let id_prefix = id.get(..32)?;
    let suffix = id.get(32..)?;
    if !valid_joplin_reference_id(id_prefix) {
        return None;
    }
    let suffix = if suffix.starts_with('#') {
        let hash_end = suffix.find(char::is_whitespace).unwrap_or(suffix.len());
        &suffix[hash_end..]
    } else {
        suffix
    };
    if suffix.is_empty()
        || (suffix.chars().next().is_some_and(char::is_whitespace)
            && suffix.trim().starts_with('"')
            && suffix.trim().ends_with('"'))
    {
        return Some(id_prefix.to_owned());
    }
    None
}

fn valid_joplin_reference_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn extract_jsoncanvas_resource_refs(
    body: &str,
    ids: &mut BTreeSet<String>,
    limit: usize,
) -> Result<(), JexScanError> {
    let Some(open) = body.find("```jsoncanvas") else {
        return Ok(());
    };
    let Some(json_start) = body[open..].find('\n').map(|offset| open + offset + 1) else {
        return Ok(());
    };
    let Some(close) = body[json_start..]
        .find("\n```")
        .map(|offset| json_start + offset)
    else {
        return Ok(());
    };
    let Ok(canvas) = serde_json::from_str::<serde_json::Value>(&body[json_start..close]) else {
        return Ok(());
    };
    let Some(nodes) = canvas.get("nodes").and_then(serde_json::Value::as_array) else {
        return Ok(());
    };
    for node in nodes {
        if node.get("type").and_then(serde_json::Value::as_str) != Some("file") {
            continue;
        }
        if node
            .get("id")
            .and_then(serde_json::Value::as_str)
            .is_none_or(str::is_empty)
            || !["x", "y", "width", "height"]
                .iter()
                .all(|key| node.get(*key).and_then(serde_json::Value::as_f64).is_some())
        {
            continue;
        }
        if let Some(id) = node
            .get("file")
            .and_then(serde_json::Value::as_str)
            .and_then(parse_whiteboard_resource_url)
        {
            insert_reference(ids, id, limit)?;
        }
    }
    Ok(())
}

fn parse_whiteboard_resource_url(value: &str) -> Option<String> {
    let id = value.strip_prefix(":/")?;
    let id_prefix = id.get(..32)?;
    let suffix = id.get(32..)?;
    if valid_joplin_id(id_prefix) && (suffix.is_empty() || suffix.starts_with('#')) {
        Some(id_prefix.to_owned())
    } else {
        None
    }
}

fn resource_archive_path(item: &ParsedItem, item_path: &str) -> Result<String, JexScanError> {
    let encrypted_blob = is_truthy(item.properties.get("encryption_blob_encrypted"));
    let extension = if encrypted_blob {
        Some("crypted".to_owned())
    } else if let Some(extension) = item
        .properties
        .get("file_extension")
        .filter(|value| !value.is_empty())
    {
        if extension.contains('/') || extension.contains('\\') || extension.contains("..") {
            return Err(JexScanError::MalformedItem {
                path: item_path.to_owned(),
                reason: "unsafe resource file_extension".to_owned(),
            });
        }
        Some(extension.clone())
    } else {
        joplin_extension_for_mime(item.properties.get("mime").map(String::as_str))
            .map(str::to_owned)
    };
    Ok(match extension {
        Some(extension) => format!("resources/{}.{}", item.id, extension),
        None => format!("resources/{}", item.id),
    })
}

const JOPLIN_MIME_EXTENSIONS: &str = include_str!("joplin_mime_extensions.tsv");

fn joplin_extension_for_mime(mime: Option<&str>) -> Option<&'static str> {
    let mime = mime?.to_ascii_lowercase();
    JOPLIN_MIME_EXTENSIONS.lines().find_map(|line| {
        let (known_mime, extension) = line.split_once('\t')?;
        (known_mime == mime).then_some(extension)
    })
}

fn stream_resource<R: Read>(
    entry: &mut tar::Entry<'_, R>,
    path: &str,
) -> Result<PhysicalResource, JexScanError> {
    let declared_size = entry.size();
    if declared_size > MAX_JEX_RESOURCE_BYTES {
        return Err(JexScanError::ResourceTooLarge {
            path: path.to_owned(),
            size: declared_size,
        });
    }
    let mut hasher = Sha256::new();
    let mut byte_count = 0u64;
    let mut buffer = [0u8; STREAM_BUFFER_BYTES];
    loop {
        let read = entry.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        byte_count =
            byte_count
                .checked_add(read as u64)
                .ok_or_else(|| JexScanError::ResourceTooLarge {
                    path: path.to_owned(),
                    size: u64::MAX,
                })?;
        if byte_count > MAX_JEX_RESOURCE_BYTES {
            return Err(JexScanError::ResourceTooLarge {
                path: path.to_owned(),
                size: byte_count,
            });
        }
        hasher.update(&buffer[..read]);
    }
    Ok(PhysicalResource {
        source_id: String::new(),
        byte_count,
        sha256: format!("{:x}", hasher.finalize()),
    })
}

fn validate_note_tag(item: &ParsedItem, path: &str) -> Result<(), JexScanError> {
    for key in ["note_id", "tag_id"] {
        if !item
            .properties
            .get(key)
            .is_some_and(|id| valid_joplin_id(id))
        {
            return Err(JexScanError::MalformedItem {
                path: path.to_owned(),
                reason: format!("missing or malformed {key}"),
            });
        }
    }
    Ok(())
}

fn add_id(target: &mut Vec<String>, count: &mut usize, id: String) {
    *count += 1;
    target.push(id);
}

fn valid_joplin_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_encrypted(properties: &BTreeMap<String, String>) -> bool {
    is_truthy(properties.get("encryption_applied"))
        || is_truthy(properties.get("encryption_blob_encrypted"))
}

fn is_truthy(value: Option<&String>) -> bool {
    value.is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

fn tar_entry_kind(entry_type: EntryType) -> String {
    if entry_type.is_symlink() {
        "symlink".to_owned()
    } else if entry_type.is_hard_link() {
        "hardlink".to_owned()
    } else if entry_type.is_gnu_longname() {
        "gnu-longname".to_owned()
    } else if entry_type.is_gnu_longlink() {
        "gnu-longlink".to_owned()
    } else if entry_type.is_pax_local_extensions() || entry_type.is_pax_global_extensions() {
        "pax-extension".to_owned()
    } else {
        format!("{:?}", entry_type)
    }
}

fn sort_report(report: &mut JexScanReport) {
    report.source_ids.notes.sort();
    report.source_ids.folders.sort();
    report.source_ids.resource_metadata.sort();
    report.source_ids.tags.sort();
    report.source_ids.note_tag_relations.sort();
    report
        .resources
        .sort_by(|left, right| left.source_id.cmp(&right.source_id));
    report
        .physical_resource_files
        .sort_by(|left, right| left.archive_path.cmp(&right.archive_path));
    report
        .store_compatibility_blockers
        .sort_by(|left, right| left.archive_path.cmp(&right.archive_path));
    report.duplicate_archive_paths.sort();
    report.duplicate_item_ids.sort();
    report.missing_resource_files.sort();
    report.orphan_note_tag_relations.sort();
    report.orphan_physical_resource_files.sort();
    report.note_body_resource_references.sort_by(|left, right| {
        (&left.note_id, &left.resource_id).cmp(&(&right.note_id, &right.resource_id))
    });
    report
        .unresolved_note_body_internal_references
        .sort_by(|left, right| {
            (&left.note_id, &left.resource_id).cmp(&(&right.note_id, &right.resource_id))
        });
    report
        .unsupported_items
        .sort_by(|left, right| left.source_id.cmp(&right.source_id));
    report.encrypted_item_ids.sort();
}

#[cfg(test)]
mod generated_mime_table_tests {
    use super::JOPLIN_MIME_EXTENSIONS;

    #[test]
    fn retains_the_full_generated_joplin_projection_and_key_suffix_choices() {
        let rows: Vec<_> = JOPLIN_MIME_EXTENSIONS
            .lines()
            .filter(|line| !line.starts_with('#') && !line.is_empty())
            .collect();
        assert_eq!(rows.len(), 770);
        assert!(rows.contains(&"audio/mpeg\tmp2"));
        assert!(rows.contains(&"image/jpeg\tjpg"));
        assert!(rows.contains(&"text/markdown\tmd"));
    }
}
