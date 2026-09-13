//! Read-only ENEX evidence scanner. The outer XML has exactly one bounded,
//! chunk-callback parser; only a retained (at most 4 MiB) ENML body uses quick-xml.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use md5::Md5;
use quick_xml::{
    Reader,
    events::{BytesStart, Event},
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io,
    path::Path,
};
use thiserror::Error;
use xml_syntax_reader::{QName, Span, Visitor, parse_read_with_capacity};

pub const MAX_ENEX_ENTITIES: usize = 50_000;
pub const MAX_ENEX_CONTENT_BYTES: usize = 4 * 1024 * 1024;
/// Kept for API compatibility; this is the input callback capacity, not a data limit.
pub const MAX_ENEX_DATA_BASE64_BYTES: usize = 64 * 1024;
const MAX_FIELD_BYTES: usize = 16 * 1024;
const MAX_XML_NAME_BYTES: usize = 256;
const MAX_RETAINED_REPORT_BYTES: usize = 8 * 1024 * 1024;
const MAX_ATTRIBUTES_PER_ELEMENT: usize = 256;
const MAX_TAGS_PER_NOTE: usize = 1024;
const INPUT_BYTES: usize = 64 * 1024;
const MAX_RESOURCE_BYTES: usize = crate::resource::MAX_RESOURCE_BYTES;

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
    #[error("ENEX resource {resource_ordinal} exceeds {limit} decoded bytes")]
    ResourceTooLarge {
        resource_ordinal: usize,
        limit: usize,
    },
    #[error("ENEX report-retained metadata exceeds {limit} bytes")]
    ReportTooLarge { limit: usize },
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
    pub md5: String,
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
    seen: BTreeSet<String>,
}

struct ResourceBuilder {
    ordinal: usize,
    note_ordinal: usize,
    mime: String,
    filename: String,
    seen: BTreeSet<String>,
    saw_data: bool,
    md5: Md5,
    sha256: Sha256,
    byte_count: usize,
    carry: [u8; 4],
    carry_len: usize,
    padded: bool,
}
impl ResourceBuilder {
    fn new(ordinal: usize, note_ordinal: usize) -> Self {
        Self {
            ordinal,
            note_ordinal,
            mime: String::new(),
            filename: String::new(),
            seen: BTreeSet::new(),
            saw_data: false,
            md5: Md5::new(),
            sha256: Sha256::new(),
            byte_count: 0,
            carry: [0; 4],
            carry_len: 0,
            padded: false,
        }
    }
    fn decode(&mut self, bytes: &[u8]) -> Result<(), EnexScanError> {
        // The 64 KiB input callback is consumed directly. Only one quartet and
        // three decoded bytes are ever held outside the digest states.
        for &byte in bytes {
            if byte.is_ascii_whitespace() {
                continue;
            }
            if self.padded {
                return Err(self.bad_base64("characters after padding"));
            }
            if !byte.is_ascii_alphanumeric() && !matches!(byte, b'+' | b'/' | b'=') {
                return Err(self.bad_base64("non-base64 character"));
            }
            self.carry[self.carry_len] = byte;
            self.carry_len += 1;
            if self.carry_len == 4 {
                let mut out = [0; 3];
                let len = STANDARD
                    .decode_slice(self.carry, &mut out)
                    .map_err(|error| self.bad_base64(&error.to_string()))?;
                if self.byte_count.saturating_add(len) > MAX_RESOURCE_BYTES {
                    return Err(EnexScanError::ResourceTooLarge {
                        resource_ordinal: self.ordinal,
                        limit: MAX_RESOURCE_BYTES,
                    });
                }
                self.md5.update(&out[..len]);
                self.sha256.update(&out[..len]);
                self.byte_count += len;
                self.padded = self.carry.contains(&b'=');
                self.carry_len = 0;
            }
        }
        Ok(())
    }
    fn bad_base64(&self, reason: &str) -> EnexScanError {
        EnexScanError::InvalidBase64 {
            resource_ordinal: self.ordinal,
            reason: reason.to_owned(),
        }
    }
    fn finish(self) -> Result<EnexScannedResource, EnexScanError> {
        if !self.saw_data {
            return Err(EnexScanError::MissingResourceData {
                resource_ordinal: self.ordinal,
            });
        }
        if self.carry_len != 0 {
            return Err(self.bad_base64("incomplete base64 quartet"));
        }
        Ok(EnexScannedResource {
            ordinal: self.ordinal,
            note_ordinal: self.note_ordinal,
            mime: self.mime,
            filename: self.filename,
            md5: format!("{:x}", self.md5.finalize()),
            sha256: format!("{:x}", self.sha256.finalize()),
            byte_count: self.byte_count,
        })
    }
}

#[derive(Default)]
struct EnexVisitor {
    stack: Vec<String>,
    root_seen: bool,
    doctype_seen: bool,
    note: Option<NoteBuilder>,
    resource: Option<ResourceBuilder>,
    field: Option<String>,
    field_text: String,
    attr_name: Option<String>,
    attr_text: String,
    attr_seen: BTreeSet<String>,
    ignored_text_bytes: usize,
    retained_report_bytes: usize,
    doctype: Vec<u8>,
    comment_bytes: usize,
    pi_bytes: usize,
    report: EnexScanReport,
}

impl EnexVisitor {
    fn name(name: QName<'_>) -> Result<String, EnexScanError> {
        if name.as_bytes().len() > MAX_XML_NAME_BYTES {
            return Err(EnexScanError::FieldTooLarge {
                limit: MAX_XML_NAME_BYTES,
            });
        }
        let raw = std::str::from_utf8(name.as_bytes())
            .map_err(|e| EnexScanError::MalformedXml(e.to_string()))?;
        if raw.is_empty() || raw.contains(':') || raw != raw.to_ascii_lowercase() {
            return Err(EnexScanError::Structure(format!(
                "unsupported XML name {raw}"
            )));
        }
        Ok(raw.to_owned())
    }
    fn charge_report(&mut self, amount: usize) -> Result<(), EnexScanError> {
        self.retained_report_bytes = self.retained_report_bytes.saturating_add(amount);
        if self.retained_report_bytes > MAX_RETAINED_REPORT_BYTES {
            Err(EnexScanError::ReportTooLarge {
                limit: MAX_RETAINED_REPORT_BYTES,
            })
        } else {
            Ok(())
        }
    }
    fn begin(&mut self, name: String) -> Result<(), EnexScanError> {
        if self.stack.len() >= 64 {
            return Err(EnexScanError::Structure("XML nesting exceeds 64".into()));
        }
        let parent = self.stack.last().map(String::as_str);
        let valid = match (parent, name.as_str()) {
            (None, "en-export") if !self.root_seen => {
                self.root_seen = true;
                true
            }
            (Some("en-export"), "note") => {
                if self.report.counts.notes >= MAX_ENEX_ENTITIES {
                    return Err(EnexScanError::TooManyEntities);
                }
                self.note = Some(NoteBuilder {
                    ordinal: self.report.counts.notes + 1,
                    ..Default::default()
                });
                true
            }
            (Some("note"), "resource") => {
                if self.report.counts.resource_occurrences >= MAX_ENEX_ENTITIES {
                    return Err(EnexScanError::TooManyEntities);
                }
                self.resource = Some(ResourceBuilder::new(
                    self.report.counts.resource_occurrences + 1,
                    self.note.as_ref().unwrap().ordinal,
                ));
                true
            }
            (
                Some("note"),
                "title" | "created" | "updated" | "tag" | "content" | "note-attributes",
            ) => {
                if name == "tag" && self.note.as_ref().unwrap().tags.len() >= MAX_TAGS_PER_NOTE {
                    return Err(EnexScanError::TooManyEntities);
                }
                true
            }
            (
                Some("resource"),
                "data" | "mime" | "resource-attributes" | "width" | "height" | "duration",
            ) => true,
            (Some("resource-attributes"), "file-name") => true,
            (
                Some("note-attributes"),
                "subject-date" | "latitude" | "longitude" | "altitude" | "author" | "source"
                | "source-url" | "source-application" | "share-date" | "reminder-order"
                | "reminder-time" | "reminder-done-time" | "place-name" | "content-class",
            ) => true,
            (
                Some("resource-attributes"),
                "source-url" | "timestamp" | "latitude" | "longitude" | "altitude" | "camera-make"
                | "camera-model" | "client-will-index" | "reco-type" | "attachment",
            ) => true,
            _ => false,
        };
        if !valid {
            return Err(EnexScanError::Structure(format!(
                "{name} not allowed below {parent:?}"
            )));
        }
        if matches!(
            name.as_str(),
            "title" | "created" | "updated" | "content" | "data" | "mime" | "file-name"
        ) {
            let seen = if let Some(resource) = self.resource.as_mut() {
                &mut resource.seen
            } else {
                &mut self.note.as_mut().unwrap().seen
            };
            if !seen.insert(name.clone()) {
                return Err(EnexScanError::Structure(format!("duplicate {name}")));
            }
        }
        if matches!(
            name.as_str(),
            "title" | "created" | "updated" | "tag" | "content" | "data" | "mime" | "file-name"
        ) {
            self.field = Some(name.clone());
            self.field_text.clear();
        }
        self.attr_seen.clear();
        self.ignored_text_bytes = 0;
        self.stack.push(name);
        Ok(())
    }
    fn close(&mut self, name: &str) -> Result<(), EnexScanError> {
        if self.stack.pop().as_deref() != Some(name) {
            return Err(EnexScanError::MalformedXml(
                "mismatched element nesting".into(),
            ));
        }
        if self.field.as_deref() == Some(name) {
            let text = std::mem::take(&mut self.field_text);
            if matches!(
                name,
                "title" | "created" | "updated" | "tag" | "mime" | "file-name"
            ) {
                self.charge_report(text.len() + if name == "tag" { 24 } else { 0 })?;
            }
            if let Some(resource) = self.resource.as_mut() {
                match name {
                    "data" => {
                        resource.saw_data = true;
                    }
                    "mime" => resource.mime = text.trim().to_owned(),
                    "file-name" => resource.filename = text.trim().to_owned(),
                    _ => {}
                }
            } else if let Some(note) = self.note.as_mut() {
                match name {
                    "title" => note.title = text.trim().to_owned(),
                    "created" => note.created_raw = text.trim().to_owned(),
                    "updated" => note.updated_raw = text.trim().to_owned(),
                    "tag" => note.tags.push(text.trim().to_owned()),
                    _ => {}
                }
            }
            self.field = None;
        }
        if name == "resource" {
            let resource = self.resource.take().unwrap().finish()?;
            self.charge_report(256)?;
            self.report.resources.push(resource);
            self.report.counts.resource_occurrences += 1;
        }
        if name == "note" {
            let note = self.note.take().unwrap();
            let (media_references, unsupported_fidelity_constructs) = if note.content.is_empty() {
                (Vec::new(), Vec::new())
            } else {
                inspect_enml(&note.content)?
            };
            self.charge_report(
                256 + media_references
                    .iter()
                    .map(|r| 64 + r.hash_md5.len() + r.mime.len())
                    .sum::<usize>(),
            )?;
            self.report.notes.push(EnexScannedNote {
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
            self.report.counts.notes += 1;
        }
        Ok(())
    }
    fn text(&mut self, value: &[u8]) -> Result<(), EnexScanError> {
        let value =
            std::str::from_utf8(value).map_err(|e| EnexScanError::MalformedXml(e.to_string()))?;
        match self.field.as_deref() {
            Some("data") => self.resource.as_mut().unwrap().decode(value.as_bytes()),
            Some("content") => {
                let content = &mut self.note.as_mut().unwrap().content;
                if content.len().saturating_add(value.len()) > MAX_ENEX_CONTENT_BYTES {
                    return Err(EnexScanError::ContentTooLarge {
                        limit: MAX_ENEX_CONTENT_BYTES,
                    });
                }
                content.push_str(value);
                Ok(())
            }
            Some(_) => {
                if self.field_text.len().saturating_add(value.len()) > MAX_FIELD_BYTES {
                    return Err(EnexScanError::FieldTooLarge {
                        limit: MAX_FIELD_BYTES,
                    });
                }
                self.field_text.push_str(value);
                Ok(())
            }
            None if value.trim().is_empty() => Ok(()),
            None if self
                .stack
                .iter()
                .rev()
                .nth(1)
                .is_some_and(|s| s == "note-attributes" || s == "resource-attributes")
                || self
                    .stack
                    .last()
                    .is_some_and(|s| s == "width" || s == "height" || s == "duration") =>
            {
                self.ignored_text_bytes = self.ignored_text_bytes.saturating_add(value.len());
                if self.ignored_text_bytes > MAX_FIELD_BYTES {
                    Err(EnexScanError::FieldTooLarge {
                        limit: MAX_FIELD_BYTES,
                    })
                } else {
                    Ok(())
                }
            }
            None => Err(EnexScanError::Structure(
                "non-whitespace outside field".into(),
            )),
        }
    }
    fn attr_text(&mut self, value: &[u8]) -> Result<(), EnexScanError> {
        let value =
            std::str::from_utf8(value).map_err(|e| EnexScanError::MalformedXml(e.to_string()))?;
        if self.attr_text.len().saturating_add(value.len()) > MAX_FIELD_BYTES {
            return Err(EnexScanError::FieldTooLarge {
                limit: MAX_FIELD_BYTES,
            });
        }
        self.attr_text.push_str(value);
        Ok(())
    }
    fn finish_attr(&mut self) -> Result<(), EnexScanError> {
        let name = self
            .attr_name
            .take()
            .ok_or_else(|| EnexScanError::Structure("attribute without name".into()))?;
        if self.stack.last().is_some_and(|s| s == "data")
            && name == "encoding"
            && !self.attr_text.eq_ignore_ascii_case("base64")
        {
            return Err(EnexScanError::UnsupportedDataEncoding {
                encoding: self.attr_text.clone(),
            });
        }
        self.attr_text.clear();
        Ok(())
    }
}

impl Visitor for EnexVisitor {
    type Error = EnexScanError;
    fn start_tag_open(&mut self, name: QName<'_>) -> Result<(), Self::Error> {
        self.begin(Self::name(name)?)
    }
    fn start_tag_close(&mut self, _: Span) -> Result<(), Self::Error> {
        Ok(())
    }
    fn empty_element_end(&mut self, _: Span) -> Result<(), Self::Error> {
        let name = self
            .stack
            .last()
            .cloned()
            .ok_or_else(|| EnexScanError::Structure("empty element without start".into()))?;
        self.close(&name)
    }
    fn end_tag(&mut self, name: QName<'_>) -> Result<(), Self::Error> {
        self.close(&Self::name(name)?)
    }
    fn attribute_name(&mut self, name: QName<'_>) -> Result<(), Self::Error> {
        let name = Self::name(name)?;
        if !self.attr_seen.insert(name.clone()) {
            return Err(EnexScanError::Structure("duplicate attribute".into()));
        }
        if self.attr_seen.len() > MAX_ATTRIBUTES_PER_ELEMENT {
            return Err(EnexScanError::TooManyEntities);
        }
        self.attr_name = Some(name);
        self.attr_text.clear();
        Ok(())
    }
    fn attribute_value(&mut self, value: &[u8], _: Span) -> Result<(), Self::Error> {
        self.attr_text(value)
    }
    fn attribute_end(&mut self, _: Span) -> Result<(), Self::Error> {
        self.finish_attr()
    }
    fn attribute_entity_ref(&mut self, name: &[u8], _: Span) -> Result<(), Self::Error> {
        self.attr_text(entity(name)?.as_bytes())
    }
    fn attribute_char_ref(&mut self, value: &[u8], _: Span) -> Result<(), Self::Error> {
        self.attr_text(char_ref(value)?.as_bytes())
    }
    fn characters(&mut self, value: &[u8], _: Span) -> Result<(), Self::Error> {
        self.text(value)
    }
    fn cdata_content(&mut self, value: &[u8], _: Span) -> Result<(), Self::Error> {
        self.text(value)
    }
    fn entity_ref(&mut self, name: &[u8], _: Span) -> Result<(), Self::Error> {
        self.text(entity(name)?.as_bytes())
    }
    fn char_ref(&mut self, value: &[u8], _: Span) -> Result<(), Self::Error> {
        self.text(char_ref(value)?.as_bytes())
    }
    fn xml_declaration(
        &mut self,
        _: &[u8],
        encoding: Option<&[u8]>,
        _: Option<bool>,
        _: Span,
    ) -> Result<(), Self::Error> {
        if encoding.is_some_and(|s| !s.eq_ignore_ascii_case(b"utf-8")) {
            Err(EnexScanError::UnsafeXml("non-UTF-8 XML declaration".into()))
        } else {
            Ok(())
        }
    }
    fn comment_start(&mut self, _: Span) -> Result<(), Self::Error> {
        self.comment_bytes = 0;
        Ok(())
    }
    fn comment_content(&mut self, bytes: &[u8], _: Span) -> Result<(), Self::Error> {
        self.comment_bytes = self.comment_bytes.saturating_add(bytes.len());
        if self.comment_bytes > MAX_FIELD_BYTES {
            Err(EnexScanError::FieldTooLarge {
                limit: MAX_FIELD_BYTES,
            })
        } else {
            Ok(())
        }
    }
    fn pi_start(&mut self, target: &[u8], _: Span) -> Result<(), Self::Error> {
        if target.len() > MAX_XML_NAME_BYTES {
            return Err(EnexScanError::FieldTooLarge {
                limit: MAX_XML_NAME_BYTES,
            });
        }
        self.pi_bytes = target.len();
        Ok(())
    }
    fn pi_content(&mut self, bytes: &[u8], _: Span) -> Result<(), Self::Error> {
        self.pi_bytes = self.pi_bytes.saturating_add(bytes.len());
        if self.pi_bytes > MAX_FIELD_BYTES {
            Err(EnexScanError::FieldTooLarge {
                limit: MAX_FIELD_BYTES,
            })
        } else {
            Ok(())
        }
    }
    fn doctype_start(&mut self, name: &[u8], _: Span) -> Result<(), Self::Error> {
        if name != b"en-export" || self.root_seen || self.doctype_seen {
            return Err(EnexScanError::UnsafeXml("unexpected DOCTYPE".into()));
        }
        self.doctype_seen = true;
        self.doctype.extend_from_slice(name);
        Ok(())
    }
    fn doctype_content(&mut self, bytes: &[u8], _: Span) -> Result<(), Self::Error> {
        if self.doctype.len().saturating_add(bytes.len()) > MAX_FIELD_BYTES {
            return Err(EnexScanError::FieldTooLarge {
                limit: MAX_FIELD_BYTES,
            });
        }
        self.doctype.extend_from_slice(bytes);
        Ok(())
    }
    fn doctype_end(&mut self, _: Span) -> Result<(), Self::Error> {
        if self.doctype.contains(&b'[')
            || self
                .doctype
                .windows(8)
                .any(|s| s.eq_ignore_ascii_case(b"<!ENTITY"))
        {
            return Err(EnexScanError::UnsafeXml("internal DTD subset".into()));
        }
        self.doctype.clear();
        Ok(())
    }
}

fn entity(name: &[u8]) -> Result<&'static str, EnexScanError> {
    match name {
        b"amp" => Ok("&"),
        b"lt" => Ok("<"),
        b"gt" => Ok(">"),
        b"apos" => Ok("'"),
        b"quot" => Ok("\""),
        _ => Err(EnexScanError::UnsafeXml("general entity reference".into())),
    }
}
fn char_ref(value: &[u8]) -> Result<String, EnexScanError> {
    let raw = std::str::from_utf8(value).map_err(|e| EnexScanError::MalformedXml(e.to_string()))?;
    let number = if let Some(hex) = raw.strip_prefix('x').or_else(|| raw.strip_prefix('X')) {
        u32::from_str_radix(hex, 16)
    } else {
        raw.parse::<u32>()
    };
    let value = number
        .ok()
        .and_then(char::from_u32)
        .ok_or_else(|| EnexScanError::MalformedXml("invalid character reference".into()))?;
    if value == '\0' {
        return Err(EnexScanError::MalformedXml("NUL reference".into()));
    }
    Ok(value.to_string())
}

pub fn scan_enex_file(path: impl AsRef<Path>) -> Result<EnexScanReport, EnexScanError> {
    let mut visitor = EnexVisitor::default();
    parse_read_with_capacity(File::open(path)?, &mut visitor, INPUT_BYTES).map_err(|error| {
        match error {
            xml_syntax_reader::ReadError::Visitor(error) => error,
            xml_syntax_reader::ReadError::Io(error) => EnexScanError::Io(error),
            xml_syntax_reader::ReadError::Xml(error) => {
                EnexScanError::MalformedXml(format!("{error:?}"))
            }
        }
    })?;
    if !visitor.root_seen
        || !visitor.stack.is_empty()
        || visitor.note.is_some()
        || visitor.resource.is_some()
    {
        return Err(EnexScanError::MalformedXml("unexpected EOF".into()));
    }
    correlate_resources(&mut visitor.report, &mut visitor.retained_report_bytes)?;
    Ok(visitor.report)
}

fn charge_derived(used: &mut usize, amount: usize) -> Result<(), EnexScanError> {
    *used = used.saturating_add(amount);
    if *used > MAX_RETAINED_REPORT_BYTES {
        Err(EnexScanError::ReportTooLarge {
            limit: MAX_RETAINED_REPORT_BYTES,
        })
    } else {
        Ok(())
    }
}

fn correlate_resources(report: &mut EnexScanReport, used: &mut usize) -> Result<(), EnexScanError> {
    let mut by_md5 = BTreeMap::<String, Vec<&EnexScannedResource>>::new();
    let mut by_note_and_md5 = BTreeMap::<(usize, String), Vec<&EnexScannedResource>>::new();
    for resource in &report.resources {
        by_md5
            .entry(resource.md5.clone())
            .or_default()
            .push(resource);
        by_note_and_md5
            .entry((resource.note_ordinal, resource.md5.clone()))
            .or_default()
            .push(resource);
    }
    for (hash, entries) in &by_md5 {
        if entries.len() > 1 {
            charge_derived(used, 24 + hash.len())?;
            report.duplicate_resource_md5s.push(hash.clone());
        }
    }
    let mut referenced = BTreeSet::new();
    for note in &report.notes {
        for media in &note.media_references {
            match by_note_and_md5.get(&(note.ordinal, media.hash_md5.clone())) {
                None => {
                    charge_derived(used, 64 + media.hash_md5.len() + media.mime.len())?;
                    report
                        .unresolved_media_references
                        .push(EnexUnresolvedMediaReference {
                            note_ordinal: note.ordinal,
                            hash_md5: media.hash_md5.clone(),
                            mime: media.mime.clone(),
                        });
                }
                Some(resources) => {
                    for resource in resources {
                        referenced.insert(resource.ordinal);
                        if !resource.mime.is_empty()
                            && !media.mime.is_empty()
                            && resource.mime != media.mime
                        {
                            charge_derived(used, 64 + resource.mime.len() + media.mime.len())?;
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
    for resource in &report.resources {
        if !referenced.contains(&resource.ordinal) {
            charge_derived(used, 8)?;
            report.unreferenced_resource_ordinals.push(resource.ordinal);
        }
    }
    Ok(())
}

fn inspect_enml(enml: &str) -> Result<(Vec<EnexMediaReference>, Vec<String>), EnexScanError> {
    let mut reader = Reader::from_reader(enml.as_bytes());
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut references = Vec::new();
    let mut unsupported = BTreeSet::new();
    let mut root_seen = false;
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                let name = event_name(event.name().as_ref())?;
                if stack.is_empty() {
                    if root_seen || name != "en-note" {
                        return Err(EnexScanError::Structure(
                            "ENML requires one en-note root".into(),
                        ));
                    }
                    root_seen = true;
                }
                stack.push(inspect_enml_start(
                    &event,
                    &mut references,
                    &mut unsupported,
                )?);
            }
            Ok(Event::Empty(event)) => {
                let name = event_name(event.name().as_ref())?;
                if stack.is_empty() {
                    if root_seen || name != "en-note" {
                        return Err(EnexScanError::Structure(
                            "ENML requires one en-note root".into(),
                        ));
                    }
                    root_seen = true;
                }
                inspect_enml_start(&event, &mut references, &mut unsupported)?;
            }
            Ok(Event::End(event)) => {
                let name = event_name(event.name().as_ref())?;
                if stack.pop().as_deref() != Some(&name) {
                    return Err(EnexScanError::MalformedXml(
                        "mismatched ENML nesting".into(),
                    ));
                }
            }
            Ok(Event::DocType(doctype)) => {
                if root_seen || !doctype.as_ref().starts_with(b"en-note") {
                    return Err(EnexScanError::UnsafeXml("unexpected ENML DOCTYPE".into()));
                }
                if doctype.as_ref().contains(&b'[') {
                    return Err(EnexScanError::UnsafeXml(
                        "internal ENML entity declaration".into(),
                    ));
                }
            }
            Ok(Event::Text(text)) => {
                let decoded = text
                    .unescape()
                    .map_err(|error| EnexScanError::UnsafeXml(error.to_string()))?;
                if stack.is_empty() && !decoded.trim().is_empty() {
                    return Err(EnexScanError::Structure("text outside ENML root".into()));
                }
            }
            Ok(Event::CData(text)) => {
                if stack.is_empty() && !text.as_ref().iter().all(u8::is_ascii_whitespace) {
                    return Err(EnexScanError::Structure("CDATA outside ENML root".into()));
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(EnexScanError::MalformedXml(error.to_string())),
        }
        buffer.clear();
    }
    if !root_seen || !stack.is_empty() {
        return Err(EnexScanError::MalformedXml("unexpected ENML EOF".into()));
    }
    Ok((references, unsupported.into_iter().collect()))
}

fn event_name(name: &[u8]) -> Result<String, EnexScanError> {
    std::str::from_utf8(name)
        .map(|name| name.to_ascii_lowercase())
        .map_err(|error| EnexScanError::MalformedXml(error.to_string()))
}
fn inspect_enml_start(
    event: &BytesStart<'_>,
    references: &mut Vec<EnexMediaReference>,
    unsupported: &mut BTreeSet<String>,
) -> Result<String, EnexScanError> {
    let name = event_name(event.name().as_ref())?;
    if name == "table" {
        unsupported.insert("table".into());
    }
    if name == "en-media" {
        let mut hash = None;
        let mut mime = None;
        for attribute in event.attributes().with_checks(true) {
            let attribute =
                attribute.map_err(|error| EnexScanError::MalformedXml(error.to_string()))?;
            let value = std::str::from_utf8(attribute.value.as_ref())
                .map_err(|error| EnexScanError::MalformedXml(error.to_string()))?
                .to_owned();
            if attribute.key.as_ref().eq_ignore_ascii_case(b"hash") {
                hash = Some(value);
            } else if attribute.key.as_ref().eq_ignore_ascii_case(b"type") {
                mime = Some(value);
            }
        }
        references.push(EnexMediaReference {
            hash_md5: hash
                .ok_or_else(|| EnexScanError::Structure("en-media without hash".into()))?
                .to_ascii_lowercase(),
            mime: mime.unwrap_or_default(),
        });
    }
    Ok(name)
}
