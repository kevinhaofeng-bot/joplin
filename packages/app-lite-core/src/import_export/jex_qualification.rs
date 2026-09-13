//! Bounded, read-only qualification of a verified exporter-shaped JEX source.
//! An audited source field is not thereby preserved in user-visible behavior.

use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use thiserror::Error;

use super::jex_qualification_source::{JexQualificationSource, prepare_jex_qualification_source};
use super::{
    JexPrepareError, JexPreparedSource, JexRawSourceItem, JexScanCounts, JexScanReport,
    JexScannedResource, JexVerifiedResource, convert_jex_note_body, parse_item,
};
use crate::ResourceId;

const SAMPLE_LIMIT: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JexQualificationBlockerKind {
    PreflightIntegrity,
    PreflightEncrypted,
    PreflightUnsupportedType,
    PreflightStoreLimit,
    ExporterFieldGap,
    UnmappedSemanticField,
    UnknownField,
    InvalidSourceTimestamp,
    StageValidation,
    BodyFidelity,
    FolderHierarchy,
    RelationGraph,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JexFieldDisposition {
    Mapped,
    SourceAuditOnly,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JexQualifiedField {
    pub item_type: i64,
    pub field: String,
    pub disposition: JexFieldDisposition,
    pub occurrence_count: usize,
    pub nondefault_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JexQualificationSample {
    pub source_id: String,
    pub source_path: String,
    /// Referenced internal target when the blocker is a dangling source link.
    pub related_source_id: Option<String>,
    pub field: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JexQualificationCategory {
    pub kind: JexQualificationBlockerKind,
    pub item_count: usize,
    pub finding_count: usize,
    pub samples: Vec<JexQualificationSample>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JexQualificationReport {
    pub counts: JexScanCounts,
    /// Every metadata item seen by the scanner, including encrypted and
    /// unsupported types excluded from the legacy supported-class counts.
    pub metadata_item_type_counts: Vec<(i64, usize)>,
    /// Types whose encrypted payload or unsupported class cannot be
    /// semantically classified, even though their raw bytes were verified.
    pub unclassifiable_item_type_counts: Vec<(i64, usize)>,
    /// Bounded MIME distribution for the completed semantic pass only.
    pub resource_mime_counts: Vec<(String, usize)>,
    pub categories: Vec<JexQualificationCategory>,
    pub fields: Vec<JexQualifiedField>,
    /// False means C2c-1 preflight prevented the item-by-item semantic pass.
    pub semantic_scan_completed: bool,
    /// Only the current strict synthetic stage subset, not a real-export claim.
    pub ready_for_current_stage: bool,
}

impl JexQualificationReport {
    pub fn category(&self, kind: JexQualificationBlockerKind) -> Option<&JexQualificationCategory> {
        self.categories
            .iter()
            .find(|category| category.kind == kind)
    }
}

#[derive(Debug, Error)]
pub enum JexQualificationError {
    #[error("JEX source preparation failed: {0}")]
    Prepare(#[from] JexPrepareError),
    #[error("JEX qualification was cancelled")]
    Cancelled,
    #[error("verified JEX source changed or disappeared: {0}")]
    Source(String),
}

#[derive(Default)]
struct ReportBuilder {
    categories: BTreeMap<JexQualificationBlockerKind, JexQualificationCategory>,
    fields: BTreeMap<(i64, String, JexFieldDisposition), (usize, usize)>,
}

impl ReportBuilder {
    fn finding(
        &mut self,
        seen: &mut BTreeSet<JexQualificationBlockerKind>,
        kind: JexQualificationBlockerKind,
        id: &str,
        path: &str,
        field: Option<&str>,
        reason: impl Into<String>,
    ) {
        let category = self
            .categories
            .entry(kind)
            .or_insert_with(|| JexQualificationCategory {
                kind,
                item_count: 0,
                finding_count: 0,
                samples: Vec::new(),
            });
        if seen.insert(kind) {
            category.item_count += 1;
        }
        category.finding_count += 1;
        if category.samples.len() < SAMPLE_LIMIT {
            category.samples.push(JexQualificationSample {
                source_id: id.to_owned(),
                source_path: path.to_owned(),
                related_source_id: None,
                field: field.map(str::to_owned),
                reason: reason.into(),
            });
        }
    }
    fn field(
        &mut self,
        kind: i64,
        field: &str,
        disposition: JexFieldDisposition,
        nondefault: bool,
    ) {
        let counts = self
            .fields
            .entry((kind, field.to_owned(), disposition))
            .or_default();
        counts.0 += 1;
        counts.1 += usize::from(nondefault);
    }
    fn finish(self, source: &JexScanReport) -> JexQualificationReport {
        let categories = self.categories.into_values().collect::<Vec<_>>();
        let fields = self
            .fields
            .into_iter()
            .map(
                |((item_type, field, disposition), (occurrence_count, nondefault_count))| {
                    JexQualifiedField {
                        item_type,
                        field,
                        disposition,
                        occurrence_count,
                        nondefault_count,
                    }
                },
            )
            .collect();
        JexQualificationReport {
            counts: source.counts.clone(),
            metadata_item_type_counts: item_type_counts(source),
            unclassifiable_item_type_counts: unclassifiable_counts(source),
            resource_mime_counts: Vec::new(),
            ready_for_current_stage: categories.is_empty(),
            categories,
            fields,
            semantic_scan_completed: true,
        }
    }
}

fn item_type_counts(source: &JexScanReport) -> Vec<(i64, usize)> {
    let mut counts = BTreeMap::new();
    for item in &source.metadata_items {
        *counts.entry(item.item_type).or_insert(0) += 1;
    }
    counts.into_iter().collect()
}

fn unclassifiable_counts(source: &JexScanReport) -> Vec<(i64, usize)> {
    let encrypted = source
        .encrypted_item_ids
        .iter()
        .map(|id| id.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let mut counts = BTreeMap::new();
    for item in &source.metadata_items {
        if encrypted.contains(&item.source_id.to_ascii_lowercase())
            || ![1, 2, 4, 5, 6].contains(&item.item_type)
        {
            *counts.entry(item.item_type).or_insert(0) += 1;
        }
    }
    counts.into_iter().collect()
}

fn preflight_report(source: JexScanReport) -> JexQualificationReport {
    let paths = source
        .metadata_items
        .iter()
        .map(|item| {
            (
                item.source_id.to_ascii_lowercase(),
                item.archive_path.as_str(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut builder = ReportBuilder::default();
    let mut seen = BTreeMap::<JexQualificationBlockerKind, BTreeSet<String>>::new();
    // A preflight report is a coverage boundary, not a claim that later
    // exporter fields or bodies were inspected. Samples remain ID/path only.
    let mut add = |kind,
                   id: &str,
                   path: &str,
                   related: Option<&str>,
                   field: Option<&str>,
                   reason: &'static str| {
        let key = if id.is_empty() { path } else { id };
        let category = builder
            .categories
            .entry(kind)
            .or_insert_with(|| JexQualificationCategory {
                kind,
                item_count: 0,
                finding_count: 0,
                samples: Vec::new(),
            });
        if seen.entry(kind).or_default().insert(key.to_owned()) {
            category.item_count += 1;
        }
        category.finding_count += 1;
        if category.samples.len() < SAMPLE_LIMIT {
            category.samples.push(JexQualificationSample {
                source_id: id.to_owned(),
                source_path: path.to_owned(),
                related_source_id: related.map(str::to_owned),
                field: field.map(str::to_owned),
                reason: reason.to_owned(),
            });
        }
    };
    for id in &source.encrypted_item_ids {
        add(
            JexQualificationBlockerKind::PreflightEncrypted,
            id,
            paths.get(&id.to_ascii_lowercase()).copied().unwrap_or(""),
            None,
            None,
            "encrypted source item blocks verified spool",
        );
    }
    for item in &source.unsupported_items {
        add(
            JexQualificationBlockerKind::PreflightUnsupportedType,
            &item.source_id,
            paths
                .get(&item.source_id.to_ascii_lowercase())
                .copied()
                .unwrap_or(""),
            None,
            Some("type_"),
            "item type is outside supported JEX classes",
        );
    }
    for item in &source.store_compatibility_blockers {
        add(
            JexQualificationBlockerKind::PreflightStoreLimit,
            &item.source_id,
            &item.archive_path,
            None,
            None,
            "resource exceeds current store compatibility limit",
        );
    }
    for path in &source.duplicate_archive_paths {
        add(
            JexQualificationBlockerKind::PreflightIntegrity,
            "",
            path,
            None,
            None,
            "duplicate archive path",
        );
    }
    for id in &source.duplicate_item_ids {
        add(
            JexQualificationBlockerKind::PreflightIntegrity,
            id,
            paths.get(&id.to_ascii_lowercase()).copied().unwrap_or(""),
            None,
            None,
            "duplicate source item ID",
        );
    }
    for path in &source.missing_resource_files {
        add(
            JexQualificationBlockerKind::PreflightIntegrity,
            "",
            path,
            None,
            None,
            "resource metadata has no physical file",
        );
    }
    for path in &source.orphan_physical_resource_files {
        add(
            JexQualificationBlockerKind::PreflightIntegrity,
            "",
            path,
            None,
            None,
            "physical resource has no metadata",
        );
    }
    for id in &source.orphan_note_tag_relations {
        add(
            JexQualificationBlockerKind::PreflightIntegrity,
            id,
            paths.get(&id.to_ascii_lowercase()).copied().unwrap_or(""),
            None,
            None,
            "relation endpoint is absent",
        );
    }
    for reference in &source.unresolved_note_body_internal_references {
        add(
            JexQualificationBlockerKind::PreflightIntegrity,
            &reference.note_id,
            paths
                .get(&reference.note_id.to_ascii_lowercase())
                .copied()
                .unwrap_or(""),
            Some(&reference.resource_id),
            Some("body"),
            "note body refers to missing internal item",
        );
    }
    let mut report = builder.finish(&source);
    report.semantic_scan_completed = false;
    report.ready_for_current_stage = false;
    report
}

// JoplinDatabase table fieldNames() and BaseItem.serialize(), not the narrow
// fields selected by Note.minimalSerializeForDisplay(). Unknown fields block.
fn exporter_fields(kind: i64) -> &'static [&'static str] {
    match kind {
        1 => &[
            "id",
            "type_",
            "title",
            "body",
            "parent_id",
            "created_time",
            "updated_time",
            "user_created_time",
            "user_updated_time",
            "markup_language",
            "is_conflict",
            "latitude",
            "longitude",
            "altitude",
            "author",
            "source_url",
            "is_todo",
            "todo_due",
            "todo_completed",
            "source",
            "source_application",
            "application_data",
            "order",
            "deleted_time",
            "encryption_applied",
            "encryption_cipher_text",
            "master_key_id",
            "share_id",
            "is_shared",
            "is_locked",
            "extracted_resource_ids",
            "conflict_original_id",
            "user_data",
        ],
        2 => &[
            "id",
            "type_",
            "title",
            "parent_id",
            "created_time",
            "updated_time",
            "user_created_time",
            "user_updated_time",
            "deleted_time",
            "encryption_applied",
            "encryption_cipher_text",
            "icon",
            "is_shared",
            "master_key_id",
            "share_id",
            "user_data",
        ],
        4 => &[
            "id",
            "type_",
            "title",
            "mime",
            "file_extension",
            "filename",
            "size",
            "created_time",
            "updated_time",
            "user_created_time",
            "user_updated_time",
            "blob_updated_time",
            "encryption_applied",
            "encryption_blob_encrypted",
            "encryption_cipher_text",
            "is_locked",
            "is_shared",
            "master_key_id",
            "share_id",
            "user_data",
            "ocr_text",
            "ocr_status",
            "ocr_error",
            "ocr_details",
            "ocr_driver_id",
        ],
        5 => &[
            "id",
            "type_",
            "title",
            "parent_id",
            "created_time",
            "updated_time",
            "user_created_time",
            "user_updated_time",
            "encryption_applied",
            "encryption_cipher_text",
            "is_shared",
            "user_data",
        ],
        6 => &[
            "id",
            "type_",
            "note_id",
            "tag_id",
            "created_time",
            "updated_time",
            "user_created_time",
            "user_updated_time",
            "encryption_applied",
            "encryption_cipher_text",
            "is_shared",
        ],
        _ => &[],
    }
}

fn stage_accepts(kind: i64, key: &str) -> bool {
    let accepted: &[&str] = match kind {
        1 => &[
            "id",
            "type_",
            "title",
            "body",
            "parent_id",
            "markup_language",
            "created_time",
            "updated_time",
            "user_created_time",
            "user_updated_time",
            "encryption_applied",
            "is_todo",
        ],
        2 => &[
            "id",
            "type_",
            "title",
            "parent_id",
            "created_time",
            "updated_time",
            "encryption_applied",
        ],
        4 => return true, // 3b accepts but does not map all resource properties.
        5 => &[
            "id",
            "type_",
            "title",
            "parent_id",
            "created_time",
            "updated_time",
            "user_created_time",
            "user_updated_time",
            "encryption_applied",
            "is_shared",
            "user_data",
        ],
        6 => &[
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
        ],
        _ => &[],
    };
    accepted.contains(&key)
}

fn mapped(kind: i64, key: &str) -> bool {
    match kind {
        1 => [
            "id",
            "type_",
            "title",
            "body",
            "parent_id",
            "markup_language",
            "user_created_time",
            "user_updated_time",
        ]
        .contains(&key),
        2 => [
            "id",
            "type_",
            "title",
            "parent_id",
            "created_time",
            "updated_time",
        ]
        .contains(&key),
        4 => ["id", "type_", "title", "mime", "file_extension"].contains(&key),
        5 => ["id", "type_", "title", "created_time", "updated_time"].contains(&key),
        6 => ["note_id", "tag_id"].contains(&key),
        _ => false,
    }
}

fn audit_only_even_nondefault(kind: i64, key: &str) -> bool {
    match kind {
        1 => ["created_time", "updated_time"].contains(&key),
        2 => ["user_created_time", "user_updated_time"].contains(&key),
        4 => [
            "created_time",
            "updated_time",
            "user_created_time",
            "user_updated_time",
            "blob_updated_time",
            "size",
        ]
        .contains(&key),
        5 => ["user_created_time", "user_updated_time"].contains(&key),
        6 => [
            "id",
            "type_",
            "created_time",
            "updated_time",
            "user_created_time",
            "user_updated_time",
        ]
        .contains(&key),
        _ => false,
    }
}

fn nondefault(kind: i64, key: &str, value: &str) -> bool {
    // Joplin's schema/migrations: resources.size=-1, notes.markup_language=1,
    // and resources.ocr_driver_id=1. A field-level default is essential:
    // the last value is present in every ordinary exported resource.
    let schema_default = match (kind, key) {
        (4, "size") => "-1",
        (1, "markup_language") | (4, "ocr_driver_id") => "1",
        _ => "0",
    };
    !value.is_empty() && value != schema_default
}

fn classify_fields(
    builder: &mut ReportBuilder,
    seen: &mut BTreeSet<JexQualificationBlockerKind>,
    kind: i64,
    id: &str,
    path: &str,
    props: &BTreeMap<String, String>,
) -> bool {
    let mut has_gap = false;
    for (key, value) in props {
        let nondefault = nondefault(kind, key, value);
        if !exporter_fields(kind).contains(&key.as_str()) {
            // All unknown names share one bounded bucket: the raw item remains
            // in the spool, but report cardinality cannot grow with input bytes.
            builder.field(kind, "<unknown>", JexFieldDisposition::Blocked, nondefault);
            builder.finding(
                seen,
                JexQualificationBlockerKind::UnknownField,
                id,
                path,
                None,
                "field is absent from exporter schema",
            );
            continue;
        }
        let mapped = mapped(kind, key);
        let semantic = nondefault && !mapped && !audit_only_even_nondefault(kind, key);
        let disposition = if semantic {
            JexFieldDisposition::Blocked
        } else if mapped {
            JexFieldDisposition::Mapped
        } else {
            JexFieldDisposition::SourceAuditOnly
        };
        builder.field(kind, key, disposition, nondefault);
        if semantic {
            builder.finding(
                seen,
                JexQualificationBlockerKind::UnmappedSemanticField,
                id,
                path,
                Some(key),
                "nondefault exporter field has no user-visible native mapping",
            );
        }
        if !stage_accepts(kind, key) {
            has_gap = true;
            builder.finding(
                seen,
                JexQualificationBlockerKind::ExporterFieldGap,
                id,
                path,
                Some(key),
                "emitted exporter field is rejected by current strict stage parser",
            );
        }
    }
    has_gap
}

fn bounded_parent(
    props: &BTreeMap<String, String>,
    builder: &mut ReportBuilder,
    seen: &mut BTreeSet<JexQualificationBlockerKind>,
    id: &str,
    path: &str,
) -> String {
    let parent = props
        .get("parent_id")
        .map(String::as_str)
        .unwrap_or_default();
    if parent.is_empty() || super::valid_joplin_id(parent) {
        return parent.to_owned();
    }
    builder.finding(
        seen,
        JexQualificationBlockerKind::StageValidation,
        id,
        path,
        Some("parent_id"),
        "parent_id is malformed",
    );
    String::new()
}

fn title_key(title: &str) -> [u8; 32] {
    Sha256::digest(title.to_lowercase().as_bytes()).into()
}

fn check_times(
    builder: &mut ReportBuilder,
    seen: &mut BTreeSet<JexQualificationBlockerKind>,
    kind: i64,
    id: &str,
    path: &str,
    props: &BTreeMap<String, String>,
) {
    let required: &[&str] = match kind {
        1 => &[
            "created_time",
            "updated_time",
            "user_created_time",
            "user_updated_time",
        ],
        2 | 5 | 6 => &["created_time", "updated_time"],
        _ => &[],
    };
    for key in required {
        if props
            .get(*key)
            .and_then(|value| super::jex_stage::parse_joplin_utc_millis(value))
            .is_none()
        {
            builder.finding(
                seen,
                JexQualificationBlockerKind::InvalidSourceTimestamp,
                id,
                path,
                Some(key),
                "required UTC ISO millisecond timestamp is missing or invalid",
            );
        }
    }
    if kind != 1 {
        for key in ["user_created_time", "user_updated_time"] {
            if props.get(key).is_some_and(|value| !value.is_empty())
                && props
                    .get(key)
                    .and_then(|value| super::jex_stage::parse_joplin_utc_millis(value))
                    .is_none()
            {
                builder.finding(
                    seen,
                    JexQualificationBlockerKind::InvalidSourceTimestamp,
                    id,
                    path,
                    Some(key),
                    "optional UTC ISO millisecond timestamp is invalid",
                );
            }
        }
    }
}

fn check_cancel(cancel: Option<&AtomicBool>) -> Result<(), JexQualificationError> {
    if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        Err(JexQualificationError::Cancelled)
    } else {
        Ok(())
    }
}

trait QualificationRead {
    fn report(&self) -> &JexScanReport;
    fn raw_item(&self, id: &str) -> Result<Option<JexRawSourceItem>, JexPrepareError>;
    fn verified_resource_prefix(
        &self,
        source: &JexScannedResource,
    ) -> Result<Vec<u8>, JexPrepareError>;
}

impl QualificationRead for JexPreparedSource {
    fn report(&self) -> &JexScanReport {
        self.report()
    }
    fn raw_item(&self, id: &str) -> Result<Option<JexRawSourceItem>, JexPrepareError> {
        self.raw_item(id)
    }
    fn verified_resource_prefix(
        &self,
        source: &JexScannedResource,
    ) -> Result<Vec<u8>, JexPrepareError> {
        let (physical, mut file) = self
            .open_verified_resource(&source.source_id)?
            .ok_or_else(|| JexPrepareError::Verification("verified resource disappeared".into()))?;
        if physical.archive_path != source.archive_path
            || physical.byte_count != source.byte_count
            || physical.sha256 != source.sha256
        {
            return Err(JexPrepareError::Verification(
                "resource source evidence differs".into(),
            ));
        }
        let mut prefix = [0u8; 8];
        let count = file.read(&mut prefix)?;
        Ok(prefix[..count].to_vec())
    }
}

impl QualificationRead for JexQualificationSource {
    fn report(&self) -> &JexScanReport {
        self.report()
    }
    fn raw_item(&self, id: &str) -> Result<Option<JexRawSourceItem>, JexPrepareError> {
        self.raw_item(id)
    }
    fn verified_resource_prefix(
        &self,
        source: &JexScannedResource,
    ) -> Result<Vec<u8>, JexPrepareError> {
        self.verified_prefix(&source.source_id)
            .map(|prefix| prefix.to_vec())
            .ok_or_else(|| {
                JexPrepareError::Verification("qualified resource prefix disappeared".into())
            })
    }
}

/// Inspects one bounded UTF-8 raw item at a time; only IDs, parent IDs,
/// titles, counts and bounded samples persist, never aggregate note bodies.
fn qualify_prepared<S: QualificationRead>(
    prepared: &S,
    cancel: Option<&AtomicBool>,
) -> Result<JexQualificationReport, JexQualificationError> {
    let source = prepared.report();
    let mut builder = ReportBuilder::default();
    let mut folder_nodes = BTreeMap::<String, (String, String, String)>::new();
    let mut sibling_titles = BTreeMap::<(String, [u8; 32]), String>::new();
    let mut note_parents = Vec::<(String, String, String)>::new();
    let mut tag_titles = BTreeMap::<[u8; 32], String>::new();
    let mut relation_pairs = BTreeSet::new();
    let mut resource_map = BTreeMap::new();
    let mut resource_validation = BTreeMap::new();
    let mut mime_counts = BTreeMap::<String, usize>::new();
    let encrypted = source
        .encrypted_item_ids
        .iter()
        .map(|id| id.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    for scanned in &source.resources {
        check_cancel(cancel)?;
        let raw = prepared
            .raw_item(&scanned.source_id)?
            .ok_or_else(|| JexQualificationError::Source(scanned.source_id.clone()))?;
        let content = std::str::from_utf8(&raw.raw_bytes)
            .map_err(|_| JexQualificationError::Source(raw.archive_path.clone()))?;
        let parsed = parse_item(&raw.archive_path, content)
            .map_err(|_| JexQualificationError::Source(raw.archive_path.clone()))?;
        let destination_id = ResourceId::new(&scanned.source_id)
            .map_err(|_| JexQualificationError::Source(scanned.source_id.clone()))?;
        let mime = parsed
            .properties
            .get("mime")
            .map(String::as_str)
            .unwrap_or_default();
        let mime_label = if !mime.is_empty()
            && mime.len() <= 128
            && mime
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"/+-._".contains(&byte))
        {
            mime
        } else {
            "<other>"
        };
        let mime_label = if mime_counts.contains_key(mime_label) || mime_counts.len() < 64 {
            mime_label
        } else {
            "<other>"
        };
        *mime_counts.entry(mime_label.to_owned()).or_default() += 1;
        let filename = content.lines().next().unwrap_or_default();
        resource_map.insert(
            scanned.source_id.to_ascii_lowercase(),
            JexVerifiedResource {
                destination_id,
                mime: if mime.len() <= 64 {
                    mime.to_owned()
                } else {
                    String::new()
                },
                filename: if filename.len() <= 255 {
                    filename.to_owned()
                } else {
                    String::new()
                },
            },
        );
        let prefix = prepared.verified_resource_prefix(scanned)?;
        if let Err(error) = super::jex_stage::validate_source_item(&raw).and_then(|_| {
            super::jex_stage::verify_source_signature_prefix(&scanned.source_id, mime, &prefix)
        }) {
            resource_validation.insert(scanned.source_id.to_ascii_lowercase(), error.to_string());
        }
    }
    for item in &source.metadata_items {
        check_cancel(cancel)?;
        let raw = prepared
            .raw_item(&item.source_id)?
            .ok_or_else(|| JexQualificationError::Source(item.source_id.clone()))?;
        if raw.archive_path != item.archive_path || raw.raw_sha256 != item.raw_sha256 {
            return Err(JexQualificationError::Source(item.source_id.clone()));
        }
        if encrypted.contains(&item.source_id.to_ascii_lowercase())
            || ![1, 2, 4, 5, 6].contains(&item.item_type)
        {
            continue;
        }
        let content = std::str::from_utf8(&raw.raw_bytes)
            .map_err(|_| JexQualificationError::Source(raw.archive_path.clone()))?;
        let parsed = parse_item(&raw.archive_path, content)
            .map_err(|_| JexQualificationError::Source(raw.archive_path.clone()))?;
        let mut seen = BTreeSet::new();
        let gap = classify_fields(
            &mut builder,
            &mut seen,
            raw.item_type,
            &raw.source_id,
            &raw.archive_path,
            &parsed.properties,
        );
        check_times(
            &mut builder,
            &mut seen,
            raw.item_type,
            &raw.source_id,
            &raw.archive_path,
            &parsed.properties,
        );
        if [1, 2, 4, 5].contains(&raw.item_type) {
            let title = content.lines().next().unwrap_or_default();
            builder.field(
                raw.item_type,
                "title",
                JexFieldDisposition::Mapped,
                !title.is_empty(),
            );
            if title.is_empty() || title != title.trim() || title.contains(['\n', '\r']) {
                builder.finding(
                    &mut seen,
                    JexQualificationBlockerKind::StageValidation,
                    &raw.source_id,
                    &raw.archive_path,
                    Some("title"),
                    "title is empty or would be changed by native normalization",
                );
            }
        }
        match raw.item_type {
            1 => {
                builder.field(
                    1,
                    "body",
                    JexFieldDisposition::Mapped,
                    !parsed.note_body.is_empty(),
                );
                let parent = bounded_parent(
                    &parsed.properties,
                    &mut builder,
                    &mut seen,
                    &raw.source_id,
                    &raw.archive_path,
                );
                note_parents.push((raw.source_id.clone(), raw.archive_path.clone(), parent));
                if let Err(error) = super::jex_stage::validate_source_note_body(&raw) {
                    builder.finding(
                        &mut seen,
                        JexQualificationBlockerKind::StageValidation,
                        &raw.source_id,
                        &raw.archive_path,
                        Some("body"),
                        error.to_string(),
                    );
                }
                let markup = parsed
                    .properties
                    .get("markup_language")
                    .and_then(|value| value.parse::<i64>().ok())
                    .unwrap_or(-1);
                if let Err(error) = convert_jex_note_body(
                    &raw.source_id,
                    &raw.archive_path,
                    markup,
                    &parsed.note_body,
                    &resource_map,
                ) {
                    builder.finding(
                        &mut seen,
                        JexQualificationBlockerKind::BodyFidelity,
                        &raw.source_id,
                        &raw.archive_path,
                        None,
                        format!("{:?}: {}", error.kind, error.reason),
                    );
                }
            }
            2 => {
                let parent = bounded_parent(
                    &parsed.properties,
                    &mut builder,
                    &mut seen,
                    &raw.source_id,
                    &raw.archive_path,
                );
                let title = content.lines().next().unwrap_or_default();
                if title.chars().count() <= 255 {
                    if let Some(first) = sibling_titles.insert(
                        (parent.to_ascii_lowercase(), title_key(title)),
                        raw.source_id.clone(),
                    ) {
                        builder.finding(
                            &mut seen,
                            JexQualificationBlockerKind::FolderHierarchy,
                            &raw.source_id,
                            &raw.archive_path,
                            Some("title"),
                            format!("duplicate sibling folder title with {first}"),
                        );
                    }
                } else {
                    builder.finding(
                        &mut seen,
                        JexQualificationBlockerKind::StageValidation,
                        &raw.source_id,
                        &raw.archive_path,
                        Some("title"),
                        "folder title exceeds native staging limit",
                    );
                }
                folder_nodes.insert(
                    raw.source_id.to_ascii_lowercase(),
                    (raw.source_id.clone(), raw.archive_path.clone(), parent),
                );
            }
            5 => {
                let title = content.lines().next().unwrap_or_default();
                if title.chars().count() <= 255 {
                    if let Some(first) = tag_titles.insert(title_key(title), raw.source_id.clone())
                    {
                        builder.finding(
                            &mut seen,
                            JexQualificationBlockerKind::RelationGraph,
                            &raw.source_id,
                            &raw.archive_path,
                            Some("title"),
                            format!("distinct source tag title collides with {first}"),
                        );
                    }
                } else {
                    builder.finding(
                        &mut seen,
                        JexQualificationBlockerKind::StageValidation,
                        &raw.source_id,
                        &raw.archive_path,
                        Some("title"),
                        "tag title exceeds native staging limit",
                    );
                }
            }
            6 => {
                let pair = (
                    parsed
                        .properties
                        .get("note_id")
                        .cloned()
                        .unwrap_or_default()
                        .to_ascii_lowercase(),
                    parsed
                        .properties
                        .get("tag_id")
                        .cloned()
                        .unwrap_or_default()
                        .to_ascii_lowercase(),
                );
                if !relation_pairs.insert(pair) {
                    builder.finding(
                        &mut seen,
                        JexQualificationBlockerKind::RelationGraph,
                        &raw.source_id,
                        &raw.archive_path,
                        None,
                        "duplicate note-tag pair would lose source relation provenance",
                    );
                }
            }
            4 => {
                if let Some(reason) = resource_validation.get(&raw.source_id.to_ascii_lowercase()) {
                    builder.finding(
                        &mut seen,
                        JexQualificationBlockerKind::StageValidation,
                        &raw.source_id,
                        &raw.archive_path,
                        None,
                        reason.clone(),
                    );
                }
            }
            _ => {}
        }
        // Exact stage parser is reused when field-list compatibility permits;
        // otherwise the per-field gaps above explain why it would stop early.
        if !gap {
            if let Err(error) = super::jex_stage::validate_source_item(&raw) {
                builder.finding(
                    &mut seen,
                    JexQualificationBlockerKind::StageValidation,
                    &raw.source_id,
                    &raw.archive_path,
                    None,
                    error.to_string(),
                );
            }
        }
    }
    let folders_with_children = folder_nodes
        .values()
        .filter_map(|(_, _, parent)| (!parent.is_empty()).then(|| parent.to_ascii_lowercase()))
        .collect::<BTreeSet<_>>();
    let folders_with_notes = note_parents
        .iter()
        .filter_map(|(_, _, parent)| (!parent.is_empty()).then(|| parent.to_ascii_lowercase()))
        .collect::<BTreeSet<_>>();
    for (key, (id, path, _)) in &folder_nodes {
        check_cancel(cancel)?;
        let mut seen = BTreeSet::new();
        if folders_with_notes.contains(key) && folders_with_children.contains(key) {
            builder.finding(
                &mut seen,
                JexQualificationBlockerKind::FolderHierarchy,
                id,
                path,
                None,
                "folder has both own notes and child folders; stack mapping would flatten notes",
            );
        }
        let mut cursor = key.clone();
        let mut visited = BTreeSet::new();
        let mut depth = 0;
        while let Some((_, _, parent)) = folder_nodes.get(&cursor) {
            if !visited.insert(cursor.clone()) {
                builder.finding(
                    &mut seen,
                    JexQualificationBlockerKind::FolderHierarchy,
                    id,
                    path,
                    None,
                    "folder parent cycle",
                );
                break;
            }
            if parent.is_empty() {
                break;
            }
            depth += 1;
            cursor = parent.to_ascii_lowercase();
            if !folder_nodes.contains_key(&cursor) {
                builder.finding(
                    &mut seen,
                    JexQualificationBlockerKind::FolderHierarchy,
                    id,
                    path,
                    Some("parent_id"),
                    "folder parent is missing",
                );
                break;
            }
            if depth > 1 {
                builder.finding(
                    &mut seen,
                    JexQualificationBlockerKind::FolderHierarchy,
                    id,
                    path,
                    Some("parent_id"),
                    "folder depth exceeds two-level native model",
                );
                break;
            }
        }
    }
    for (id, path, parent) in &note_parents {
        if !parent.is_empty() && !folder_nodes.contains_key(&parent.to_ascii_lowercase()) {
            let mut seen = BTreeSet::new();
            builder.finding(
                &mut seen,
                JexQualificationBlockerKind::FolderHierarchy,
                id,
                path,
                Some("parent_id"),
                "note source parent folder is missing",
            );
        }
    }
    if source.counts.notes == 0 {
        let mut seen = BTreeSet::new();
        builder.finding(
            &mut seen,
            JexQualificationBlockerKind::StageValidation,
            "",
            "<archive>",
            None,
            "JEX contains no supported notes",
        );
    }
    check_cancel(cancel)?;
    let mut report = builder.finish(source);
    report.resource_mime_counts = mime_counts.into_iter().collect();
    Ok(report)
}

pub fn qualify_prepared_jex_source(
    prepared: &JexPreparedSource,
) -> Result<JexQualificationReport, JexQualificationError> {
    qualify_prepared(prepared, None)
}

pub fn qualify_jex_archive(
    source: impl AsRef<Path>,
    staging_parent: impl AsRef<Path>,
) -> Result<JexQualificationReport, JexQualificationError> {
    qualify_archive_inner(source.as_ref(), staging_parent.as_ref(), None)
}

pub fn qualify_jex_archive_with_cancel(
    source: impl AsRef<Path>,
    staging_parent: impl AsRef<Path>,
    cancel: Arc<AtomicBool>,
) -> Result<JexQualificationReport, JexQualificationError> {
    qualify_archive_inner(source.as_ref(), staging_parent.as_ref(), Some(cancel))
}

fn qualify_archive_inner(
    archive: &Path,
    staging_parent: &Path,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<JexQualificationReport, JexQualificationError> {
    let source = prepare_jex_qualification_source(archive, staging_parent, cancel.clone())?;
    let mut report = qualify_prepared(&source, cancel.as_deref())?;
    if !source.report().is_clean() {
        let preflight = preflight_report(source.report().clone());
        report.categories.extend(preflight.categories);
        report.categories.sort_by_key(|category| category.kind);
        report.ready_for_current_stage = false;
    }
    Ok(report)
}
