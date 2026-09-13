//! Fail-closed, side-effect-free conversion of one verified JEX Markdown body.
//! HTML JEX notes deliberately remain blocked until their separate converter exists.

use std::collections::BTreeMap;

use pulldown_cmark::{Event, HeadingLevel as MdHeadingLevel, Options, Parser, Tag, TagEnd};

use crate::document::{
    Block, BlockStyle, CanonicalDocument, HeadingLevel, Inline, ListItem, ListKind, Marks,
};
use crate::resource::ResourceId;

const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_EVENTS: usize = 20_000;
const MAX_DEPTH: usize = 64;
const MAX_URL_BYTES: usize = 8 * 1024;
const MAX_TOTAL_URL_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JexVerifiedResource {
    pub destination_id: ResourceId,
    pub mime: String,
    pub filename: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JexBodyConversion {
    pub document: CanonicalDocument,
    pub canonical_html: String,
    pub search_text: String,
    /// Source-body occurrence order, including repeated references.
    pub ordered_resource_occurrences: Vec<ResourceId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JexBodyBlockerKind {
    UnsupportedStructure,
    RawHtml,
    UnsupportedHeading,
    PluginDirective,
    UnverifiedResource,
    InternalNoteLink,
    LinkedImage,
    UnsupportedImageSource,
    UnsafeLink,
    AmbiguousAttachment,
    UnsupportedAttribute,
    HtmlNotImplemented,
    UnsupportedMarkupLanguage,
    BodyTooLarge,
    UrlBudget,
    ParserBudget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JexBodyFidelityBlocker {
    pub kind: JexBodyBlockerKind,
    pub source_note_id: String,
    pub source_path: String,
    pub reason: String,
}

type Result<T> = std::result::Result<T, JexBodyFidelityBlocker>;

struct Converter<'a> {
    note_id: &'a str,
    path: &'a str,
    events: Vec<Event<'a>>,
    cursor: usize,
    resources: &'a BTreeMap<String, JexVerifiedResource>,
    occurrences: Vec<ResourceId>,
    url_bytes: usize,
}

impl<'a> Converter<'a> {
    fn blocked<T>(&self, kind: JexBodyBlockerKind, reason: &'static str) -> Result<T> {
        Err(JexBodyFidelityBlocker {
            kind,
            source_note_id: self.note_id.to_owned(),
            source_path: self.path.to_owned(),
            reason: reason.to_owned(),
        })
    }

    fn next(&mut self) -> Option<Event<'a>> {
        let event = self.events.get(self.cursor)?;
        self.cursor += 1;
        Some(event.clone())
    }

    fn peek(&self) -> Option<&Event<'a>> {
        self.events.get(self.cursor)
    }

    fn expect_end(&mut self, end: TagEnd) -> Result<()> {
        if self.next() == Some(Event::End(end)) {
            Ok(())
        } else {
            self.blocked(
                JexBodyBlockerKind::UnsupportedStructure,
                "Markdown structure did not close as expected",
            )
        }
    }

    fn url(&mut self, value: &str) -> Result<()> {
        self.url_bytes = self.url_bytes.saturating_add(value.len());
        if value.len() > MAX_URL_BYTES || self.url_bytes > MAX_TOTAL_URL_BYTES {
            return self.blocked(
                JexBodyBlockerKind::UrlBudget,
                "Markdown link URL exceeds the safe budget",
            );
        }
        Ok(())
    }

    fn resource(&self, url: &str) -> Option<&JexVerifiedResource> {
        let id = url.strip_prefix(":/")?;
        if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        self.resources.get(&id.to_ascii_lowercase())
    }

    fn external_link(&mut self, url: &str) -> Result<String> {
        self.url(url)?;
        let safe_scheme =
            url.starts_with("https://") || url.starts_with("http://") || url.starts_with("mailto:");
        let safe_bytes = !url.bytes().any(|byte| {
            byte.is_ascii_control()
                || byte == b' '
                || byte == b'<'
                || byte == b'>'
                || byte == b'\''
                || byte == b'\"'
        });
        let nonempty_target = url
            .split_once(':')
            .is_some_and(|(_, target)| !target.trim_start_matches('/').is_empty());
        if !safe_scheme || !safe_bytes || !nonempty_target {
            return self.blocked(
                JexBodyBlockerKind::UnsafeLink,
                "Markdown link is not a safe absolute URL",
            );
        }
        Ok(url.to_owned())
    }

    fn inlines(&mut self, end: TagEnd, marks: Marks, in_link: bool) -> Result<Vec<Inline>> {
        let mut out = Vec::new();
        loop {
            let event = match self.peek() {
                Some(Event::End(found)) if *found == end => break,
                Some(_) => self.next().expect("peeked event exists"),
                None => {
                    return self.blocked(
                        JexBodyBlockerKind::UnsupportedStructure,
                        "Unclosed inline Markdown structure",
                    );
                }
            };
            match event {
                Event::Text(text) => out.push(Inline::Text {
                    text: text.into_string(),
                    marks: marks.clone(),
                }),
                Event::Code(text) => {
                    let mut code_marks = marks.clone();
                    code_marks.inline_code = true;
                    out.push(Inline::Text {
                        text: text.into_string(),
                        marks: code_marks,
                    });
                }
                Event::SoftBreak | Event::HardBreak => out.push(Inline::SoftBreak),
                Event::Start(Tag::Emphasis) => {
                    let mut nested = marks.clone();
                    nested.italic = true;
                    out.extend(self.inlines(TagEnd::Emphasis, nested, in_link)?);
                }
                Event::Start(Tag::Strong) => {
                    let mut nested = marks.clone();
                    nested.bold = true;
                    out.extend(self.inlines(TagEnd::Strong, nested, in_link)?);
                }
                Event::Start(Tag::Strikethrough) => {
                    let mut nested = marks.clone();
                    nested.strikethrough = true;
                    out.extend(self.inlines(TagEnd::Strikethrough, nested, in_link)?);
                }
                Event::Start(Tag::Link {
                    dest_url, title, ..
                }) => {
                    if !title.is_empty() {
                        return self.blocked(
                            JexBodyBlockerKind::UnsupportedAttribute,
                            "Markdown link title has no canonical representation",
                        );
                    }
                    if in_link {
                        return self.blocked(
                            JexBodyBlockerKind::UnsupportedStructure,
                            "Nested Markdown links are unsupported",
                        );
                    }
                    if dest_url.starts_with(":/") {
                        return self.blocked(
                            JexBodyBlockerKind::AmbiguousAttachment,
                            "Resource attachment must occupy its own paragraph",
                        );
                    }
                    let url = self.external_link(&dest_url)?;
                    let mut nested = marks.clone();
                    nested.link = Some(url);
                    out.extend(self.inlines(TagEnd::Link, nested, true)?);
                }
                Event::Start(Tag::Image {
                    dest_url, title, ..
                }) => {
                    if in_link {
                        return self.blocked(
                            JexBodyBlockerKind::LinkedImage,
                            "Linked image cannot retain both targets",
                        );
                    }
                    if !title.is_empty() {
                        return self.blocked(
                            JexBodyBlockerKind::UnsupportedAttribute,
                            "Markdown image title has no canonical representation",
                        );
                    }
                    self.url(&dest_url)?;
                    if !dest_url.starts_with(":/") {
                        return self.blocked(
                            JexBodyBlockerKind::UnsupportedImageSource,
                            "Only verified JEX resource images are supported",
                        );
                    }
                    let resource = match self.resource(&dest_url) {
                        Some(value) => value.clone(),
                        None => {
                            return self.blocked(
                                JexBodyBlockerKind::UnverifiedResource,
                                "Image target is not a verified JEX resource",
                            );
                        }
                    };
                    if !matches!(
                        resource.mime.as_str(),
                        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
                    ) {
                        return self.blocked(
                            JexBodyBlockerKind::AmbiguousAttachment,
                            "Non-image resource uses image syntax",
                        );
                    }
                    let mut alt = String::new();
                    loop {
                        match self.next() {
                            Some(Event::End(TagEnd::Image)) => break,
                            Some(Event::Text(text)) | Some(Event::Code(text)) => {
                                alt.push_str(&text)
                            }
                            Some(Event::SoftBreak) | Some(Event::HardBreak) => alt.push(' '),
                            _ => {
                                return self.blocked(
                                    JexBodyBlockerKind::UnsupportedStructure,
                                    "Image alt contains unsupported Markdown structure",
                                );
                            }
                        }
                    }
                    self.occurrences.push(resource.destination_id.clone());
                    out.push(Inline::Image {
                        resource_id: resource.destination_id,
                        alt,
                    });
                }
                Event::Html(_) | Event::InlineHtml(_) | Event::Start(Tag::HtmlBlock) => {
                    return self.blocked(
                        JexBodyBlockerKind::RawHtml,
                        "Raw HTML in Markdown body is not supported",
                    );
                }
                _ => {
                    return self.blocked(
                        JexBodyBlockerKind::UnsupportedStructure,
                        "Markdown inline construct has no lossless canonical mapping",
                    );
                }
            }
        }
        self.expect_end(end)?;
        Ok(out)
    }

    fn paragraph(&mut self) -> Result<Block> {
        // A verified PDF link is only representable as a whole-block card.
        if let Some(Event::Start(Tag::Link {
            dest_url, title, ..
        })) = self.peek().cloned()
        {
            if dest_url.starts_with(":/") {
                if !title.is_empty() {
                    return self.blocked(
                        JexBodyBlockerKind::UnsupportedAttribute,
                        "Attachment link title has no canonical representation",
                    );
                }
                self.url(&dest_url)?;
                let resource = match self.resource(&dest_url) {
                    Some(value) => value.clone(),
                    None => {
                        return self.blocked(
                            JexBodyBlockerKind::InternalNoteLink,
                            "Internal link is not a verified attachment resource",
                        );
                    }
                };
                self.next();
                let label = match self.next() {
                    Some(Event::Text(label)) => label.into_string(),
                    _ => {
                        return self.blocked(
                            JexBodyBlockerKind::AmbiguousAttachment,
                            "Attachment label must be plain text",
                        );
                    }
                };
                self.expect_end(TagEnd::Link)?;
                if !matches!(self.peek(), Some(Event::End(TagEnd::Paragraph)))
                    || resource.mime.starts_with("image/")
                    || label != resource.filename
                {
                    return self.blocked(JexBodyBlockerKind::AmbiguousAttachment, "Attachment must be a standalone non-image link labelled with its verified filename");
                }
                self.expect_end(TagEnd::Paragraph)?;
                self.occurrences.push(resource.destination_id.clone());
                return Ok(Block::Attachment {
                    resource_id: resource.destination_id,
                    filename: resource.filename,
                    media_type: resource.mime,
                });
            }
        }
        let inlines = self.inlines(TagEnd::Paragraph, Marks::default(), false)?;
        Ok(Block::Paragraph {
            style: BlockStyle::default(),
            inlines,
        })
    }

    fn list(&mut self, ordered_start: Option<u64>) -> Result<Block> {
        if ordered_start.is_some_and(|start| start != 1) {
            return self.blocked(
                JexBodyBlockerKind::UnsupportedStructure,
                "Ordered list start number is not representable",
            );
        }
        let mut items = Vec::new();
        while matches!(self.peek(), Some(Event::Start(Tag::Item))) {
            self.next();
            let has_paragraph = matches!(self.peek(), Some(Event::Start(Tag::Paragraph)));
            if has_paragraph {
                self.next();
            }
            let checked = match self.peek() {
                Some(Event::TaskListMarker(value)) => {
                    let checked = *value;
                    self.next();
                    Some(checked)
                }
                _ => None,
            };
            let inlines = if has_paragraph {
                self.inlines(TagEnd::Paragraph, Marks::default(), false)?
            } else {
                self.inlines(TagEnd::Item, Marks::default(), false)?
            };
            // Tight lists consume Item in inlines(); loose lists consume it here.
            if matches!(self.peek(), Some(Event::End(TagEnd::Item))) {
                self.next();
            }
            if !matches!(
                self.peek(),
                Some(Event::Start(Tag::Item)) | Some(Event::End(TagEnd::List(_)))
            ) {
                return self.blocked(
                    JexBodyBlockerKind::UnsupportedStructure,
                    "List item has multiple or nested blocks",
                );
            }
            items.push(ListItem {
                checked,
                style: BlockStyle::default(),
                inlines,
            });
        }
        self.expect_end(TagEnd::List(ordered_start.is_some()))?;
        let all_checked = items.iter().all(|item| item.checked.is_some());
        let none_checked = items.iter().all(|item| item.checked.is_none());
        let kind = match ordered_start {
            Some(_) if none_checked => ListKind::Ordered,
            None if all_checked => ListKind::Checklist,
            None if none_checked => ListKind::Unordered,
            _ => {
                return self.blocked(
                    JexBodyBlockerKind::UnsupportedStructure,
                    "Mixed task/plain or ordered checklist items cannot be represented as one canonical list",
                );
            }
        };
        Ok(Block::List { kind, items })
    }

    fn blocks(&mut self) -> Result<Vec<Block>> {
        let mut blocks = Vec::new();
        while let Some(event) = self.next() {
            match event {
                Event::Start(Tag::Paragraph) => blocks.push(self.paragraph()?),
                Event::Start(Tag::Heading {
                    level,
                    id,
                    classes,
                    attrs,
                }) => {
                    if id.is_some() || !classes.is_empty() || !attrs.is_empty() {
                        return self.blocked(
                            JexBodyBlockerKind::UnsupportedAttribute,
                            "Heading attributes have no canonical representation",
                        );
                    }
                    let level = match level {
                        MdHeadingLevel::H1 => HeadingLevel::One,
                        MdHeadingLevel::H2 => HeadingLevel::Two,
                        MdHeadingLevel::H3 => HeadingLevel::Three,
                        _ => {
                            return self.blocked(
                                JexBodyBlockerKind::UnsupportedHeading,
                                "Heading level exceeds canonical support",
                            );
                        }
                    };
                    let inlines = self.inlines(
                        TagEnd::Heading(match level {
                            HeadingLevel::One => MdHeadingLevel::H1,
                            HeadingLevel::Two => MdHeadingLevel::H2,
                            HeadingLevel::Three => MdHeadingLevel::H3,
                        }),
                        Marks::default(),
                        false,
                    )?;
                    blocks.push(Block::Heading {
                        level,
                        style: BlockStyle::default(),
                        inlines,
                    });
                }
                Event::Start(Tag::List(start)) => blocks.push(self.list(start)?),
                Event::Html(_) | Event::InlineHtml(_) | Event::Start(Tag::HtmlBlock) => {
                    return self.blocked(
                        JexBodyBlockerKind::RawHtml,
                        "Raw HTML in Markdown body is not supported",
                    );
                }
                _ => {
                    return self.blocked(
                        JexBodyBlockerKind::UnsupportedStructure,
                        "Markdown block construct has no lossless canonical mapping",
                    );
                }
            }
        }
        Ok(blocks)
    }
}

pub fn convert_jex_note_body(
    source_note_id: &str,
    source_path: &str,
    markup_language: i64,
    body: &str,
    resources: &BTreeMap<String, JexVerifiedResource>,
) -> Result<JexBodyConversion> {
    let blocked = |kind, reason: &'static str| JexBodyFidelityBlocker {
        kind,
        source_note_id: source_note_id.to_owned(),
        source_path: source_path.to_owned(),
        reason: reason.to_owned(),
    };
    if markup_language == 2 {
        return Err(blocked(
            JexBodyBlockerKind::HtmlNotImplemented,
            "HTML JEX body conversion belongs to C2c-2b",
        ));
    }
    if markup_language != 1 {
        return Err(blocked(
            JexBodyBlockerKind::UnsupportedMarkupLanguage,
            "Unknown JEX markup language",
        ));
    }
    if body.len() > MAX_BODY_BYTES {
        return Err(blocked(
            JexBodyBlockerKind::BodyTooLarge,
            "Markdown body exceeds the safe byte budget",
        ));
    }
    // C2c-1 retains the source's original ID spelling; the body URI may use
    // different ASCII case. Reject collisions instead of choosing one target.
    let mut normalized_resources = BTreeMap::new();
    for (source_id, resource) in resources {
        if normalized_resources
            .insert(source_id.to_ascii_lowercase(), resource.clone())
            .is_some()
        {
            return Err(blocked(
                JexBodyBlockerKind::UnverifiedResource,
                "Case-fold conflict in verified source resource map",
            ));
        }
    }
    if body
        .lines()
        .any(|line| line.trim_start().starts_with(":::"))
    {
        return Err(blocked(
            JexBodyBlockerKind::PluginDirective,
            "Joplin plugin directive has no canonical mapping",
        ));
    }

    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_GFM
        | Options::ENABLE_MATH
        | Options::ENABLE_DEFINITION_LIST
        | Options::ENABLE_WIKILINKS;
    let mut events = Vec::new();
    let mut depth = 0usize;
    for event in Parser::new_ext(body, options) {
        if events.len() >= MAX_EVENTS {
            return Err(blocked(
                JexBodyBlockerKind::ParserBudget,
                "Markdown parser event budget exceeded",
            ));
        }
        match event {
            Event::Start(_) => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err(blocked(
                        JexBodyBlockerKind::ParserBudget,
                        "Markdown nesting budget exceeded",
                    ));
                }
            }
            Event::End(_) => depth = depth.saturating_sub(1),
            _ => {}
        }
        events.push(event);
    }
    let mut converter = Converter {
        note_id: source_note_id,
        path: source_path,
        events,
        cursor: 0,
        resources: &normalized_resources,
        occurrences: Vec::new(),
        url_bytes: 0,
    };
    let blocks = converter.blocks()?;
    let document = CanonicalDocument::from_blocks(blocks);
    let canonical_html = document.to_canonical_html().as_str().to_owned();
    let search_text = document.search_text().as_str().to_owned();
    let roundtrip = CanonicalDocument::parse_html(&canonical_html).map_err(|_| {
        blocked(
            JexBodyBlockerKind::UnsupportedStructure,
            "Canonical HTML could not be reparsed",
        )
    })?;
    if roundtrip != document
        || roundtrip.search_text().as_str() != search_text
        || document.resource_ids() != converter.occurrences
    {
        return Err(blocked(
            JexBodyBlockerKind::UnsupportedStructure,
            "Canonical roundtrip would lose Markdown structure or resource order",
        ));
    }
    Ok(JexBodyConversion {
        document,
        canonical_html,
        search_text,
        ordered_resource_occurrences: converter.occurrences,
    })
}
