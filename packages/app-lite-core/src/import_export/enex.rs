//! Read-only, bounded evidence collection for Evernote ENEX files.
//!
//! This is deliberately a scanner, rather than an importer: it has no profile,
//! repository, or blob-store dependency. `quick-xml` emits a whole text event,
//! so a fixed-buffer preflight refuses `<data>` fields beyond the safe event
//! size before XML parsing starts. Such files return `NeedsContext`; this stage
//! does not claim to stream-decode them.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{self, BufReader, Read},
    path::Path,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use md5::Md5;
use quick_xml::{
    Reader,
    events::{BytesStart, Event},
};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Number of notes or resource occurrences retained by one scan.
pub const MAX_ENEX_ENTITIES: usize = 50_000;
/// Retained ENML bytes for a single note.
pub const MAX_ENEX_CONTENT_BYTES: usize = 4 * 1024 * 1024;
/// Largest base64 text event this scanner will allow `quick-xml` to materialize.
pub const MAX_ENEX_DATA_BASE64_BYTES: usize = 4 * 1024 * 1024;
const MAX_FIELD_BYTES: usize = 16 * 1024;
const PREFLIGHT_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Error)]
pub enum EnexScanError {
    #[error("cannot read ENEX file: {0}")]
    Io(#[from] io::Error),
    #[error("malformed ENEX XML: {0}")]
    MalformedXml(String),
    #[error("unsafe XML construct: {0}")]
    UnsafeXml(String),
    #[error(
        "ENEX data needs a streaming decoder: resource {resource_ordinal} exceeds {limit} base64 bytes"
    )]
    NeedsContext {
        resource_ordinal: usize,
        limit: usize,
    },
    #[error("ENEX note content exceeds {limit} bytes")]
    ContentTooLarge { limit: usize },
    #[error("ENEX metadata field exceeds {limit} bytes")]
    FieldTooLarge { limit: usize },
    #[error("ENEX exceeds the {MAX_ENEX_ENTITIES} entity limit")]
    TooManyEntities,
    #[error("invalid base64 data in resource {resource_ordinal}: {reason}")]
    InvalidBase64 {
        resource_ordinal: usize,
        reason: String,
    },
    #[error("resource data uses unsupported encoding {encoding}")]
    UnsupportedDataEncoding { encoding: String },
    #[error("resource {resource_ordinal} has no base64 data")]
    MissingResourceData { resource_ordinal: usize },
    #[error("unexpected ENEX structure: {0}")]
    Structure(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnexScanCounts {
    pub notes: usize,
    pub resource_occurrences: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnexMediaReference {
    pub hash_md5: String,
    pub mime: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnexScannedNote {
    pub ordinal: usize,
    pub title: String,
    pub created_raw: String,
    pub updated_raw: String,
    pub tags: Vec<String>,
    pub content_size: usize,
    pub content_sha256: String,
    pub media_references: Vec<EnexMediaReference>,
    pub unsupported_fidelity_constructs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnexScannedResource {
    pub ordinal: usize,
    pub note_ordinal: usize,
    pub mime: String,
    pub filename: String,
    /// ENEX identity used by `<en-media hash>`.
    pub md5: String,
    /// Separate content-integrity digest used by a future local store.
    pub sha256: String,
    pub byte_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnexUnresolvedMediaReference {
    pub note_ordinal: usize,
    pub hash_md5: String,
    pub mime: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnexMimeMismatch {
    pub note_ordinal: usize,
    pub resource_ordinal: usize,
    pub declared_mime: String,
    pub referenced_mime: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnexScanReport {
    pub counts: EnexScanCounts,
    pub notes: Vec<EnexScannedNote>,
    /// Occurrence order is preserved, including identical bytes/MD5s.
    pub resources: Vec<EnexScannedResource>,
    pub duplicate_resource_md5s: Vec<String>,
    pub unresolved_media_references: Vec<EnexUnresolvedMediaReference>,
    pub unreferenced_resource_ordinals: Vec<usize>,
    pub mime_mismatches: Vec<EnexMimeMismatch>,
}

#[derive(Default)]
struct NoteBuilder {
    ordinal: usize,
    title: String,
    created_raw: String,
    updated_raw: String,
    tags: Vec<String>,
    content: String,
}

#[derive(Default)]
struct ResourceBuilder {
    ordinal: usize,
    note_ordinal: usize,
    mime: String,
    filename: String,
    data: String,
    saw_data: bool,
}

/// Scan an ENEX file without opening, writing, or otherwise touching a profile.
///
/// The scanner accepts ordinary ENEX/ENML external-DOCTYPE declarations but
/// does not resolve external entities or access the network.
pub fn scan_enex_file(path: impl AsRef<Path>) -> Result<EnexScanReport, EnexScanError> {
    preflight_large_data(path.as_ref())?;
    let file = File::open(path)?;
    let mut reader = Reader::from_reader(BufReader::new(file));
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut current_note: Option<NoteBuilder> = None;
    let mut current_resource: Option<ResourceBuilder> = None;
    let mut field: Option<String> = None;
    let mut field_text = String::new();
    let mut report = EnexScanReport::default();
    let mut saw_root = false;

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                let name = event_name(event.name().as_ref())?;
                if name == "data" {
                    ensure_base64_data(&event)?;
                }
                if stack.is_empty() {
                    if name != "en-export" || saw_root {
                        return Err(EnexScanError::Structure(
                            "root must be one en-export element".to_owned(),
                        ));
                    }
                    saw_root = true;
                }
                begin_element(
                    &name,
                    &mut stack,
                    &mut current_note,
                    &mut current_resource,
                    &mut report,
                    &mut field,
                    &mut field_text,
                )?;
            }
            Ok(Event::Empty(event)) => {
                let name = event_name(event.name().as_ref())?;
                if name == "data" {
                    ensure_base64_data(&event)?;
                }
                begin_element(
                    &name,
                    &mut stack,
                    &mut current_note,
                    &mut current_resource,
                    &mut report,
                    &mut field,
                    &mut field_text,
                )?;
                end_element(
                    &name,
                    &mut stack,
                    &mut current_note,
                    &mut current_resource,
                    &mut report,
                    &mut field,
                    &mut field_text,
                )?;
            }
            Ok(Event::Text(text)) => append_text(
                std::str::from_utf8(text.as_ref())
                    .map_err(|error| EnexScanError::MalformedXml(error.to_string()))?,
                &field,
                &mut field_text,
                &mut current_note,
                &mut current_resource,
            )?,
            Ok(Event::CData(text)) => append_text(
                std::str::from_utf8(text.as_ref())
                    .map_err(|error| EnexScanError::MalformedXml(error.to_string()))?,
                &field,
                &mut field_text,
                &mut current_note,
                &mut current_resource,
            )?,
            Ok(Event::End(event)) => {
                let name = event_name(event.name().as_ref())?;
                end_element(
                    &name,
                    &mut stack,
                    &mut current_note,
                    &mut current_resource,
                    &mut report,
                    &mut field,
                    &mut field_text,
                )?;
            }
            // An external ENEX/ENML DTD is accepted as inert: `quick-xml`
            // never resolves it. Internal entity declarations are rejected
            // rather than permitting entity-expansion semantics.
            Ok(Event::DocType(doctype)) => {
                if doctype
                    .as_ref()
                    .windows(b"<!ENTITY".len())
                    .any(|part| part.eq_ignore_ascii_case(b"<!ENTITY"))
                {
                    return Err(EnexScanError::UnsafeXml(
                        "internal entity declaration".to_owned(),
                    ));
                }
            }
            Ok(Event::Decl(_)) | Ok(Event::Comment(_)) | Ok(Event::PI(_)) => {}
            Ok(Event::Eof) => break,
            Err(error) => return Err(EnexScanError::MalformedXml(error.to_string())),
        }
        buffer.clear();
    }
    if !saw_root || !stack.is_empty() || current_note.is_some() || current_resource.is_some() {
        return Err(EnexScanError::MalformedXml("unexpected EOF".to_owned()));
    }
    correlate_resources(&mut report);
    Ok(report)
}

fn begin_element(
    name: &str,
    stack: &mut Vec<String>,
    note: &mut Option<NoteBuilder>,
    resource: &mut Option<ResourceBuilder>,
    report: &mut EnexScanReport,
    field: &mut Option<String>,
    field_text: &mut String,
) -> Result<(), EnexScanError> {
    if name == "note" {
        if note.is_some()
            || resource.is_some()
            || stack.last().map(String::as_str) != Some("en-export")
        {
            return Err(EnexScanError::Structure(
                "note must be a direct en-export child".to_owned(),
            ));
        }
        if report.counts.notes >= MAX_ENEX_ENTITIES {
            return Err(EnexScanError::TooManyEntities);
        }
        *note = Some(NoteBuilder {
            ordinal: report.counts.notes + 1,
            ..Default::default()
        });
    }
    if name == "resource" {
        let Some(note) = note.as_ref() else {
            return Err(EnexScanError::Structure("resource outside note".to_owned()));
        };
        if resource.is_some() || report.counts.resource_occurrences >= MAX_ENEX_ENTITIES {
            return Err(EnexScanError::TooManyEntities);
        }
        *resource = Some(ResourceBuilder {
            ordinal: report.counts.resource_occurrences + 1,
            note_ordinal: note.ordinal,
            ..Default::default()
        });
    }
    if matches!(
        name,
        "title" | "created" | "updated" | "tag" | "content" | "data" | "mime" | "file-name"
    ) {
        *field = Some(name.to_owned());
        field_text.clear();
    }
    stack.push(name.to_owned());
    Ok(())
}

fn append_text(
    value: &str,
    field: &Option<String>,
    field_text: &mut String,
    note: &mut Option<NoteBuilder>,
    resource: &mut Option<ResourceBuilder>,
) -> Result<(), EnexScanError> {
    let Some(field) = field.as_deref() else {
        return Ok(());
    };
    if field == "content" {
        let note = note
            .as_mut()
            .ok_or_else(|| EnexScanError::Structure("content outside note".to_owned()))?;
        if note.content.len().saturating_add(value.len()) > MAX_ENEX_CONTENT_BYTES {
            return Err(EnexScanError::ContentTooLarge {
                limit: MAX_ENEX_CONTENT_BYTES,
            });
        }
        note.content.push_str(value);
    } else {
        if field_text.len().saturating_add(value.len()) > MAX_FIELD_BYTES {
            return Err(EnexScanError::FieldTooLarge {
                limit: MAX_FIELD_BYTES,
            });
        }
        field_text.push_str(value);
        if field == "data" {
            let resource = resource
                .as_mut()
                .ok_or_else(|| EnexScanError::Structure("data outside resource".to_owned()))?;
            resource.saw_data = true;
        }
    }
    Ok(())
}

fn end_element(
    name: &str,
    stack: &mut Vec<String>,
    note: &mut Option<NoteBuilder>,
    resource: &mut Option<ResourceBuilder>,
    report: &mut EnexScanReport,
    field: &mut Option<String>,
    field_text: &mut String,
) -> Result<(), EnexScanError> {
    if stack.pop().as_deref() != Some(name) {
        return Err(EnexScanError::MalformedXml(
            "mismatched element nesting".to_owned(),
        ));
    }
    if field.as_deref() == Some(name) {
        finish_field(name, field_text, note, resource)?;
        *field = None;
        field_text.clear();
    }
    if name == "resource" {
        let resource = resource.take().expect("resource start was checked");
        if !resource.saw_data {
            return Err(EnexScanError::MissingResourceData {
                resource_ordinal: resource.ordinal,
            });
        }
        let compact: String = resource
            .data
            .chars()
            .filter(|character| !character.is_ascii_whitespace())
            .collect();
        let bytes = STANDARD
            .decode(compact)
            .map_err(|error| EnexScanError::InvalidBase64 {
                resource_ordinal: resource.ordinal,
                reason: error.to_string(),
            })?;
        report.resources.push(EnexScannedResource {
            ordinal: resource.ordinal,
            note_ordinal: resource.note_ordinal,
            mime: resource.mime,
            filename: resource.filename,
            md5: format!("{:x}", Md5::digest(&bytes)),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            byte_count: bytes.len(),
        });
        report.counts.resource_occurrences += 1;
    }
    if name == "note" {
        let note = note.take().expect("note start was checked");
        let (media_references, unsupported_fidelity_constructs) = inspect_enml(&note.content)?;
        report.notes.push(EnexScannedNote {
            ordinal: note.ordinal,
            title: note.title,
            created_raw: note.created_raw,
            updated_raw: note.updated_raw,
            tags: note.tags,
            content_size: note.content.len(),
            content_sha256: format!("{:x}", Sha256::digest(note.content.as_bytes())),
            media_references,
            unsupported_fidelity_constructs,
        });
        report.counts.notes += 1;
    }
    Ok(())
}

fn finish_field(
    name: &str,
    text: &mut String,
    note: &mut Option<NoteBuilder>,
    resource: &mut Option<ResourceBuilder>,
) -> Result<(), EnexScanError> {
    if name == "content" {
        return Ok(());
    }
    if name == "data" {
        let resource = resource
            .as_mut()
            .ok_or_else(|| EnexScanError::Structure("data outside resource".to_owned()))?;
        resource.data = std::mem::take(text);
        return Ok(());
    }
    if let Some(resource) = resource.as_mut() {
        match name {
            "mime" => resource.mime = std::mem::take(text).trim().to_owned(),
            "file-name" => resource.filename = std::mem::take(text).trim().to_owned(),
            _ => {}
        }
        return Ok(());
    }
    let note = note
        .as_mut()
        .ok_or_else(|| EnexScanError::Structure(format!("{name} outside note")))?;
    match name {
        "title" => note.title = std::mem::take(text).trim().to_owned(),
        "created" => note.created_raw = std::mem::take(text).trim().to_owned(),
        "updated" => note.updated_raw = std::mem::take(text).trim().to_owned(),
        "tag" => note.tags.push(std::mem::take(text).trim().to_owned()),
        _ => {}
    }
    Ok(())
}

fn event_name(name: &[u8]) -> Result<String, EnexScanError> {
    std::str::from_utf8(name)
        .map(|name| name.to_ascii_lowercase())
        .map_err(|error| EnexScanError::MalformedXml(error.to_string()))
}

fn ensure_base64_data(event: &BytesStart<'_>) -> Result<(), EnexScanError> {
    for attribute in event.attributes().with_checks(true) {
        let attribute =
            attribute.map_err(|error| EnexScanError::MalformedXml(error.to_string()))?;
        if attribute.key.as_ref().eq_ignore_ascii_case(b"encoding") {
            let encoding = std::str::from_utf8(attribute.value.as_ref())
                .map_err(|error| EnexScanError::MalformedXml(error.to_string()))?;
            if !encoding.eq_ignore_ascii_case("base64") {
                return Err(EnexScanError::UnsupportedDataEncoding {
                    encoding: encoding.to_owned(),
                });
            }
        }
    }
    Ok(())
}

fn correlate_resources(report: &mut EnexScanReport) {
    let mut by_md5 = BTreeMap::<String, Vec<&EnexScannedResource>>::new();
    for resource in &report.resources {
        by_md5
            .entry(resource.md5.clone())
            .or_default()
            .push(resource);
    }
    report.duplicate_resource_md5s = by_md5
        .iter()
        .filter_map(|(hash, entries)| (entries.len() > 1).then_some(hash.clone()))
        .collect();
    let mut referenced = BTreeSet::new();
    for note in &report.notes {
        for media in &note.media_references {
            match by_md5.get(&media.hash_md5) {
                None => report
                    .unresolved_media_references
                    .push(EnexUnresolvedMediaReference {
                        note_ordinal: note.ordinal,
                        hash_md5: media.hash_md5.clone(),
                        mime: media.mime.clone(),
                    }),
                Some(resources) => {
                    for resource in resources {
                        referenced.insert(resource.ordinal);
                        if !resource.mime.is_empty()
                            && !media.mime.is_empty()
                            && resource.mime != media.mime
                        {
                            report.mime_mismatches.push(EnexMimeMismatch {
                                note_ordinal: note.ordinal,
                                resource_ordinal: resource.ordinal,
                                declared_mime: resource.mime.clone(),
                                referenced_mime: media.mime.clone(),
                            });
                        }
                    }
                }
            }
        }
    }
    report.unreferenced_resource_ordinals = report
        .resources
        .iter()
        .filter_map(|resource| {
            (!referenced.contains(&resource.ordinal)).then_some(resource.ordinal)
        })
        .collect();
}

fn inspect_enml(enml: &str) -> Result<(Vec<EnexMediaReference>, Vec<String>), EnexScanError> {
    let mut references = Vec::new();
    let mut unsupported = BTreeSet::new();
    let bytes = enml.as_bytes();
    let mut position = 0;
    while let Some(relative_start) = bytes[position..].iter().position(|byte| *byte == b'<') {
        let start = position + relative_start;
        let Some(end) = tag_end(bytes, start) else {
            return Err(EnexScanError::MalformedXml(
                "unterminated ENML tag".to_owned(),
            ));
        };
        let tag = std::str::from_utf8(&bytes[start + 1..end])
            .map_err(|error| EnexScanError::MalformedXml(error.to_string()))?;
        let trimmed = tag.trim();
        if !trimmed.starts_with('/') && !trimmed.starts_with('!') && !trimmed.starts_with('?') {
            let name_end = trimmed
                .find(|character: char| character.is_ascii_whitespace() || character == '/')
                .unwrap_or(trimmed.len());
            let name = trimmed[..name_end].to_ascii_lowercase();
            if name == "table" {
                unsupported.insert("table".to_owned());
            }
            if name == "en-media" {
                let attributes = parse_attributes(&trimmed[name_end..]);
                let hash_md5 = attributes
                    .get("hash")
                    .cloned()
                    .ok_or_else(|| EnexScanError::Structure("en-media without hash".to_owned()))?;
                references.push(EnexMediaReference {
                    hash_md5: hash_md5.to_ascii_lowercase(),
                    mime: attributes.get("type").cloned().unwrap_or_default(),
                });
            }
        }
        position = end + 1;
    }
    Ok((references, unsupported.into_iter().collect()))
}

fn tag_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut quote = None;
    for (offset, byte) in bytes[start + 1..].iter().enumerate() {
        match (*byte, quote) {
            (b'\'' | b'\"', None) => quote = Some(*byte),
            (byte, Some(current)) if byte == current => quote = None,
            (b'>', None) => return Some(start + offset + 1),
            _ => {}
        }
    }
    None
}

fn parse_attributes(input: &str) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    let mut rest = input.trim();
    while !rest.is_empty() && rest != "/" {
        let Some(equal) = rest.find('=') else {
            break;
        };
        let key = rest[..equal].trim();
        rest = rest[equal + 1..].trim_start();
        let Some(quote) = rest
            .chars()
            .next()
            .filter(|quote| *quote == '\'' || *quote == '\"')
        else {
            break;
        };
        rest = &rest[quote.len_utf8()..];
        let Some(end) = rest.find(quote) else {
            break;
        };
        attributes.insert(key.to_ascii_lowercase(), rest[..end].to_owned());
        rest = rest[end + quote.len_utf8()..].trim_start();
    }
    attributes
}

fn preflight_large_data(path: &Path) -> Result<(), EnexScanError> {
    let mut reader = BufReader::with_capacity(PREFLIGHT_BUFFER_BYTES, File::open(path)?);
    let mut buffer = [0_u8; PREFLIGHT_BUFFER_BYTES];
    let mut state = PreflightState::Outside;
    let mut resource_ordinal = 0;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        for byte in &buffer[..read] {
            match &mut state {
                PreflightState::Outside => {
                    if *byte == b'<' {
                        state = PreflightState::Tag(vec![*byte]);
                    }
                }
                PreflightState::Tag(tag) => {
                    tag.push(*byte);
                    if tag.len() > 256 {
                        return Err(EnexScanError::MalformedXml("oversized XML tag".to_owned()));
                    }
                    if *byte == b'>' {
                        let opening = tag.starts_with(b"<data")
                            && tag
                                .get(5)
                                .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b'>');
                        if opening {
                            resource_ordinal += 1;
                            state = PreflightState::Data {
                                bytes: 0,
                                close: 0,
                                ordinal: resource_ordinal,
                            };
                        } else {
                            state = PreflightState::Outside;
                        }
                    }
                }
                PreflightState::Data {
                    bytes,
                    close,
                    ordinal,
                } => {
                    const END: &[u8] = b"</data>";
                    *bytes += 1;
                    if *bytes > MAX_ENEX_DATA_BASE64_BYTES {
                        return Err(EnexScanError::NeedsContext {
                            resource_ordinal: *ordinal,
                            limit: MAX_ENEX_DATA_BASE64_BYTES,
                        });
                    }
                    if *byte == END[*close] {
                        *close += 1;
                        if *close == END.len() {
                            state = PreflightState::Outside;
                        }
                    } else {
                        *close = usize::from(*byte == END[0]);
                    }
                }
            }
        }
    }
    if !matches!(state, PreflightState::Outside) {
        return Err(EnexScanError::MalformedXml(
            "unterminated XML tag or data".to_owned(),
        ));
    }
    Ok(())
}

enum PreflightState {
    Outside,
    Tag(Vec<u8>),
    Data {
        bytes: usize,
        close: usize,
        ordinal: usize,
    },
}
