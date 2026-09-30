//! Fail-closed, side-effect-free conversion of one verified JEX Markdown body.
//! HTML JEX notes deliberately remain blocked until their separate converter exists.

use std::collections::BTreeMap;

use pulldown_cmark::{Event, HeadingLevel as MdHeadingLevel, Options, Parser, Tag, TagEnd};

use crate::document::{
    Block, BlockStyle, CanonicalDocument, HeadingLevel, Inline, ListItem, ListKind, Marks,
    TableCell, TableRow,
};
use crate::resource::ResourceId;

const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_EVENTS: usize = 20_000;
const MAX_DEPTH: usize = 64;
const MAX_URL_BYTES: usize = 8 * 1024;
const MAX_TOTAL_URL_BYTES: usize = 64 * 1024;
// Same limits as canonical tables; larger ones stay degraded.
const MAX_TABLE_ROWS: usize = 1000;
const MAX_TABLE_COLUMNS: usize = 64;
// Canonical data-indent and editor list depth both stop at 8.
const MAX_LIST_INDENT: u8 = 8;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JexBodyBlockerKind {
    UnsupportedStructure,
    RawHtml,
    UnsupportedHeading,
    PluginDirective,
    UnverifiedResource,
    /// Two source resource IDs differ only by ASCII case.
    ResourceIdCollision,
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
        let safe_scheme = url.starts_with("https://")
            || url.starts_with("http://")
            || url.starts_with("mailto:")
            || (url.starts_with("tel:") && crate::document::valid_link(url));
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
        self.inlines_until(InlineEnd::Tag(end), marks, in_link)
    }

    fn inlines_until(
        &mut self,
        end: InlineEnd,
        marks: Marks,
        in_link: bool,
    ) -> Result<Vec<Inline>> {
        let mut out = Vec::new();
        loop {
            let event = match self.peek() {
                Some(Event::End(found)) if end == InlineEnd::Tag(*found) => break,
                Some(Event::InlineHtml(html))
                    if matches!(end, InlineEnd::Html(name)
                        if InlineHtmlTag::parse(html)
                            .is_some_and(|tag| tag.closing && tag.name == name)) =>
                {
                    self.next();
                    return Ok(out);
                }
                // A tight item's text ends where its nested list starts.
                Some(Event::Start(Tag::List(_))) if end == InlineEnd::Tag(TagEnd::Item) => {
                    return Ok(out);
                }
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
                        self.url(&dest_url)?;
                        let Some(resource) = self.resource(&dest_url).cloned() else {
                            return self.blocked(
                                JexBodyBlockerKind::InternalNoteLink,
                                "Internal link is not a verified attachment resource",
                            );
                        };
                        if self.is_self_linked_image(&dest_url) {
                            out.extend(self.inlines(TagEnd::Link, marks.clone(), false)?);
                            continue;
                        }
                        let label = self.inlines(TagEnd::Link, marks.clone(), true)?;
                        self.resource_inline(resource, label, &marks, &mut out);
                        continue;
                    }
                    // An empty or in-note fragment target points nowhere the
                    // product can represent; the link text is the content.
                    if dest_url.is_empty() || dest_url.starts_with('#') {
                        out.extend(self.inlines(TagEnd::Link, marks.clone(), true)?);
                        continue;
                    }
                    let url = self.external_link(&dest_url)?;
                    let mut nested = marks.clone();
                    nested.link = Some(url);
                    out.extend(self.inlines(TagEnd::Link, nested, true)?);
                }
                Event::Start(Tag::Image {
                    dest_url, title, ..
                }) => {
                    // Only an external link survives as the image's own link;
                    // a resource or fragment target has no image representation.
                    if in_link && marks.link.is_none() {
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
                    let is_image = matches!(
                        resource.mime.as_str(),
                        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
                    );
                    // A file card has no link of its own.
                    if !is_image && marks.link.is_some() {
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
                    if !is_image {
                        // Joplin writes `![name](:/id)` for any dropped file.
                        let label = if alt.is_empty() {
                            Vec::new()
                        } else {
                            vec![Inline::Text {
                                text: alt,
                                marks: marks.clone(),
                            }]
                        };
                        self.resource_inline(resource, label, &marks, &mut out);
                        continue;
                    }
                    self.occurrences.push(resource.destination_id.clone());
                    out.push(Inline::Image {
                        resource_id: resource.destination_id,
                        alt,
                        display_width: None,
                        link: marks.link.clone(),
                    });
                }
                Event::InlineHtml(html) if is_line_break(&html) => out.push(Inline::SoftBreak),
                Event::InlineHtml(html)
                    if self.inline_html(&html, &marks, in_link, &mut out)? => {}
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
        match end {
            InlineEnd::Tag(end) => self.expect_end(end)?,
            InlineEnd::Html(_) => unreachable!("an HTML end returns from the loop"),
        }
        Ok(out)
    }

    /// Inline HTML that Evernote parses into marks or an image (common-editor
    /// textformatter/schema.ts 320-349 for b/strong, i/em, u, s/strike/del,
    /// sup/sub, mark; link/schema.ts 43-57 takes only `a[href]`, so an id or
    /// name anchor keeps its text). Returns false for anything else, which
    /// stays a raw-HTML blocker rather than being dropped.
    fn inline_html(
        &mut self,
        html: &str,
        marks: &Marks,
        in_link: bool,
        out: &mut Vec<Inline>,
    ) -> Result<bool> {
        let Some(tag) = InlineHtmlTag::parse(html) else {
            return Ok(false);
        };
        if tag.closing {
            return Ok(false);
        }
        if tag.name == "img" {
            if in_link && marks.link.is_none() {
                return self.blocked(
                    JexBodyBlockerKind::LinkedImage,
                    "Linked image cannot retain both targets",
                );
            }
            // The strict HTML converter owns image attribute, width and
            // resource checks, exactly as for an HTML block.
            let converted = super::jex_html::convert_html_body(
                self.note_id,
                self.path,
                &format!("<p>{}</p>", html.trim()),
                self.resources,
            )
            .map_err(|mut error| {
                error.kind = JexBodyBlockerKind::RawHtml;
                error
            })?;
            let [Block::Paragraph { inlines, .. }] = converted.document.blocks() else {
                return Ok(false);
            };
            let [
                Inline::Image {
                    resource_id,
                    alt,
                    display_width,
                    ..
                },
            ] = inlines.as_slice()
            else {
                return Ok(false);
            };
            self.occurrences
                .extend(converted.ordered_resource_occurrences);
            out.push(Inline::Image {
                resource_id: resource_id.clone(),
                alt: alt.clone(),
                display_width: *display_width,
                link: marks.link.clone(),
            });
            return Ok(true);
        }
        let mut nested = marks.clone();
        let name = match tag.name.as_str() {
            "a" if !tag.attributes.is_empty()
                && tag
                    .attributes
                    .iter()
                    .all(|attribute| matches!(attribute.as_str(), "id" | "name")) =>
            {
                "a"
            }
            _ if !tag.attributes.is_empty() => return Ok(false),
            "b" => {
                nested.bold = true;
                "b"
            }
            "strong" => {
                nested.bold = true;
                "strong"
            }
            "i" => {
                nested.italic = true;
                "i"
            }
            "em" => {
                nested.italic = true;
                "em"
            }
            "u" => {
                nested.underline = true;
                "u"
            }
            "s" => {
                nested.strikethrough = true;
                "s"
            }
            "strike" => {
                nested.strikethrough = true;
                "strike"
            }
            "del" => {
                nested.strikethrough = true;
                "del"
            }
            "mark" => {
                nested.highlight = true;
                "mark"
            }
            "sup" => {
                nested.script = Some(crate::document::Script::Superscript);
                "sup"
            }
            "sub" => {
                nested.script = Some(crate::document::Script::Subscript);
                "sub"
            }
            _ => return Ok(false),
        };
        out.extend(self.inlines_until(InlineEnd::Html(name), nested, in_link)?);
        Ok(true)
    }

    fn paragraph(&mut self) -> Result<Block> {
        // A paragraph that is exactly `[filename](:/id)` is a whole-block card;
        // any other resource link stays inline (see resource_inline).
        let standalone_card = matches!(
            (
                self.events.get(self.cursor + 1),
                self.events.get(self.cursor + 2),
                self.events.get(self.cursor + 3),
            ),
            (Some(Event::Text(label)), Some(Event::End(TagEnd::Link)), Some(Event::End(TagEnd::Paragraph)))
                if matches!(self.peek(), Some(Event::Start(Tag::Link { dest_url, .. }))
                    if self.resource(dest_url).is_some_and(|resource| {
                        !resource.mime.starts_with("image/") && resource.filename == label.as_ref()
                    }))
        );
        if let Some(Event::Start(Tag::Link {
            dest_url, title, ..
        })) = self.peek().cloned()
            && standalone_card
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

    /// The link's only content is an image of the same resource, so the link
    /// adds no second target.
    fn is_self_linked_image(&self, link_target: &str) -> bool {
        let Some(Event::Start(Tag::Image { dest_url, .. })) = self.peek() else {
            return false;
        };
        if !dest_url.eq_ignore_ascii_case(link_target) {
            return false;
        }
        let Some(end) = self.events[self.cursor..]
            .iter()
            .position(|event| *event == Event::End(TagEnd::Image))
        else {
            return false;
        };
        self.events.get(self.cursor + end + 1) == Some(&Event::End(TagEnd::Link))
    }

    /// A GFM table: the first row is its header; cells hold inline content.
    fn table(&mut self) -> Result<Block> {
        let mut rows = Vec::new();
        loop {
            match self.next() {
                Some(Event::Start(Tag::TableHead)) => rows.push(self.table_row(TagEnd::TableHead)?),
                Some(Event::Start(Tag::TableRow)) => rows.push(self.table_row(TagEnd::TableRow)?),
                Some(Event::End(TagEnd::Table)) => break,
                _ => {
                    return self.blocked(
                        JexBodyBlockerKind::UnsupportedStructure,
                        "Markdown table structure did not close as expected",
                    );
                }
            }
        }
        if rows.len() > MAX_TABLE_ROWS || rows.iter().any(|row| row.cells.len() > MAX_TABLE_COLUMNS)
        {
            return self.blocked(
                JexBodyBlockerKind::UnsupportedStructure,
                "Table exceeds the canonical size limits",
            );
        }
        Ok(Block::Table { rows, header: true })
    }

    fn table_row(&mut self, end: TagEnd) -> Result<TableRow> {
        let mut cells = Vec::new();
        loop {
            match self.next() {
                Some(Event::Start(Tag::TableCell)) => cells.push(TableCell {
                    inlines: self.inlines(TagEnd::TableCell, Marks::default(), false)?,
                }),
                Some(Event::End(found)) if found == end => break,
                _ => {
                    return self.blocked(
                        JexBodyBlockerKind::UnsupportedStructure,
                        "Markdown table structure did not close as expected",
                    );
                }
            }
        }
        Ok(TableRow { cells })
    }

    /// Joplin resource link inside text. The resource keeps its position; a
    /// label other than the filename stays visible before it.
    fn resource_inline(
        &mut self,
        resource: JexVerifiedResource,
        label: Vec<Inline>,
        context: &Marks,
        out: &mut Vec<Inline>,
    ) {
        let plain: String = label
            .iter()
            .map(|inline| match inline {
                Inline::Text { text, .. } => text.as_str(),
                _ => "",
            })
            .collect();
        let only_filename = plain == resource.filename
            && label
                .iter()
                .all(|inline| matches!(inline, Inline::Text { marks, .. } if marks == context));
        if !only_filename {
            out.extend(label);
        }
        self.occurrences.push(resource.destination_id.clone());
        if matches!(
            resource.mime.as_str(),
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        ) {
            out.push(Inline::Image {
                resource_id: resource.destination_id,
                alt: resource.filename,
                display_width: None,
                link: None,
            });
        } else {
            out.push(Inline::Attachment {
                resource_id: resource.destination_id,
                filename: resource.filename,
                media_type: resource.mime,
            });
        }
    }

    /// Items of one Markdown list; a nested list of the same kind continues
    /// the flat canonical list with `indent` = nesting level (editor depth).
    fn list_items(
        &mut self,
        ordered_start: Option<u64>,
        depth: u8,
        items: &mut Vec<ListItem>,
    ) -> Result<()> {
        // Nesting flattens into one canonical list, which has one start.
        if depth > 0 && ordered_start.is_some_and(|start| start != 1) {
            return self.blocked(
                JexBodyBlockerKind::UnsupportedStructure,
                "Ordered list start number is not representable",
            );
        }
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
            items.push(ListItem {
                checked,
                style: BlockStyle {
                    indent: depth,
                    ..BlockStyle::default()
                },
                inlines,
            });
            if let Some(Event::Start(Tag::List(nested))) = self.peek().cloned() {
                if nested.is_some() != ordered_start.is_some() || depth >= MAX_LIST_INDENT {
                    return self.blocked(
                        JexBodyBlockerKind::UnsupportedStructure,
                        "Nested list of another kind or too deep",
                    );
                }
                self.next();
                self.list_items(nested, depth + 1, items)?;
            }
            // Tight lists consume Item in inlines() unless a nested list
            // follows; loose lists and nested lists end it here.
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
        }
        self.expect_end(TagEnd::List(ordered_start.is_some()))
    }

    fn list(&mut self, ordered_start: Option<u64>) -> Result<Block> {
        let mut items = Vec::new();
        self.list_items(ordered_start, 0, &mut items)?;
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
        let start = match ordered_start.map(u32::try_from) {
            Some(Err(_)) => {
                return self.blocked(
                    JexBodyBlockerKind::UnsupportedStructure,
                    "Ordered list start number is not representable",
                );
            }
            start => start.and_then(std::result::Result::ok),
        };
        Ok(Block::List { kind, items, start })
    }

    fn blocks(&mut self) -> Result<Vec<Block>> {
        let mut blocks = Vec::new();
        while let Some(event) = self.next() {
            match event {
                Event::Rule => blocks.push(Block::Divider),
                Event::Start(Tag::BlockQuote(kind)) => {
                    if kind.is_some() {
                        return self.blocked(
                            JexBodyBlockerKind::UnsupportedStructure,
                            "Alert quote type has no canonical mapping",
                        );
                    }
                    // Each paragraph of a quote becomes its own adjacent quote block.
                    loop {
                        self.expect_start_paragraph()?;
                        let inlines = self.inlines(TagEnd::Paragraph, Marks::default(), false)?;
                        blocks.push(Block::Quote {
                            style: BlockStyle::default(),
                            inlines,
                        });
                        if !matches!(self.peek(), Some(Event::Start(Tag::Paragraph))) {
                            break;
                        }
                    }
                    self.expect_end(TagEnd::BlockQuote(kind))?;
                }
                Event::Start(Tag::CodeBlock(kind)) => {
                    // A fence's info string is Evernote's code block
                    // syntaxLanguage when it is one language name; anything
                    // more (attributes, several words) has no mapping. Joplin
                    // renders mermaid, abc and fountain fences as diagrams,
                    // music and screenplay, not as code.
                    let language = match &kind {
                        pulldown_cmark::CodeBlockKind::Fenced(info) if !info.is_empty() => {
                            if !crate::document::valid_code_language(info)
                                || matches!(
                                    info.to_ascii_lowercase().as_str(),
                                    "mermaid" | "abc" | "fountain"
                                )
                            {
                                return self.blocked(
                                    JexBodyBlockerKind::UnsupportedStructure,
                                    "Code language metadata has no canonical mapping",
                                );
                            }
                            Some(info.to_string())
                        }
                        _ => None,
                    };
                    let inlines = self.inlines(TagEnd::CodeBlock, Marks::default(), false)?;
                    blocks.push(Block::Code {
                        style: BlockStyle::default(),
                        inlines,
                        language,
                    });
                }
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
                        MdHeadingLevel::H4 => HeadingLevel::Four,
                        MdHeadingLevel::H5 => HeadingLevel::Five,
                        MdHeadingLevel::H6 => HeadingLevel::Six,
                    };
                    let inlines = self.inlines(
                        TagEnd::Heading(match level {
                            HeadingLevel::One => MdHeadingLevel::H1,
                            HeadingLevel::Two => MdHeadingLevel::H2,
                            HeadingLevel::Three => MdHeadingLevel::H3,
                            HeadingLevel::Four => MdHeadingLevel::H4,
                            HeadingLevel::Five => MdHeadingLevel::H5,
                            HeadingLevel::Six => MdHeadingLevel::H6,
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
                Event::Start(Tag::Table(_)) => blocks.push(self.table()?),
                Event::Start(Tag::HtmlBlock) => {
                    // Only what the strict HTML converter maps losslessly.
                    let mut fragment = String::new();
                    loop {
                        match self.next() {
                            Some(Event::Html(html)) => fragment.push_str(&html),
                            Some(Event::End(TagEnd::HtmlBlock)) => break,
                            _ => {
                                return self.blocked(
                                    JexBodyBlockerKind::RawHtml,
                                    "Raw HTML in Markdown body is not supported",
                                );
                            }
                        }
                    }
                    let stripped = strip_bare_document_wrappers(&fragment);
                    if stripped.trim().is_empty() {
                        continue;
                    }
                    // The browser puts text left after the wrapper in an
                    // anonymous block; a div is the canonical equivalent.
                    let fragment = if stripped == fragment || stripped.trim_start().starts_with('<')
                    {
                        stripped
                    } else {
                        format!("<div>{}</div>", stripped.trim_end())
                    };
                    let converted = super::jex_html::convert_html_body(
                        self.note_id,
                        self.path,
                        &fragment,
                        self.resources,
                    )
                    // RawHtml routes the degrade path to local HTML conversion.
                    .map_err(|mut error| {
                        error.kind = JexBodyBlockerKind::RawHtml;
                        error
                    })?;
                    self.occurrences
                        .extend(converted.ordered_resource_occurrences);
                    blocks.extend_from_slice(converted.document.blocks());
                }
                Event::Html(_) | Event::InlineHtml(_) => {
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

    fn expect_start_paragraph(&mut self) -> Result<()> {
        if matches!(self.next(), Some(Event::Start(Tag::Paragraph))) {
            Ok(())
        } else {
            self.blocked(
                JexBodyBlockerKind::UnsupportedStructure,
                "Quote contains non-paragraph structure",
            )
        }
    }
}

/// Removes attribute-free `<html>`/`<body>` open/close tags, which Joplin's
/// renderer leaves to the browser to ignore (e.g. residue of forwarded mail).
/// Text they precede stays, as Joplin shows it.
fn strip_bare_document_wrappers(html: &str) -> String {
    const TAGS: [&str; 4] = ["<html>", "</html>", "<body>", "</body>"];
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(index) = rest.find('<') {
        out.push_str(&rest[..index]);
        rest = &rest[index..];
        match TAGS.into_iter().find(|tag| {
            rest.get(..tag.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(tag))
        }) {
            Some(tag) => rest = &rest[tag.len()..],
            None => {
                out.push('<');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InlineEnd {
    Tag(TagEnd),
    /// The matching closing tag of an inline HTML element.
    Html(&'static str),
}

/// One whole HTML tag as CommonMark reports inline HTML: its lowercase name,
/// whether it closes, and its attribute names. Anything else is not a tag.
struct InlineHtmlTag {
    name: String,
    closing: bool,
    attributes: Vec<String>,
}

impl InlineHtmlTag {
    fn parse(html: &str) -> Option<Self> {
        let inner = html.trim().strip_prefix('<')?.strip_suffix('>')?;
        let (closing, inner) = match inner.strip_prefix('/') {
            Some(rest) => (true, rest),
            None => (false, inner),
        };
        let inner = inner.strip_suffix('/').unwrap_or(inner);
        let name_end = inner
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(inner.len());
        let name = inner[..name_end].to_ascii_lowercase();
        if name.is_empty() {
            return None;
        }
        let mut attributes = Vec::new();
        let mut rest = inner[name_end..].trim_start();
        if closing && !rest.is_empty() {
            return None;
        }
        while !rest.is_empty() {
            let attribute_end = rest
                .find(|c: char| c == '=' || c.is_whitespace())
                .unwrap_or(rest.len());
            let attribute = &rest[..attribute_end];
            if attribute.is_empty()
                || !attribute
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':')
            {
                return None;
            }
            attributes.push(attribute.to_ascii_lowercase());
            rest = rest[attribute_end..].trim_start();
            if let Some(value) = rest.strip_prefix('=') {
                let value = value.trim_start();
                let consumed = match value.chars().next()? {
                    quote @ ('"' | '\'') => value[1..].find(quote)? + 2,
                    _ => value.find(char::is_whitespace).unwrap_or(value.len()),
                };
                rest = value[consumed..].trim_start();
            }
        }
        Some(Self {
            name,
            closing,
            attributes,
        })
    }
}

fn is_line_break(html: &str) -> bool {
    matches!(
        html.trim().to_ascii_lowercase().as_str(),
        "<br>" | "<br/>" | "<br />"
    )
}

/// Like [`convert_jex_note_body`], but unsupported Markdown blocks degrade
/// locally to source text/cards; representable neighboring blocks survive.
/// HTML containers are traversed locally, preserving supported subtrees.
/// The original blocker remains in the report; staging retains source bytes.
/// Invalid trees, parser budgets and ambiguous resource identities fail closed.
pub fn convert_jex_note_body_or_degrade(
    source_note_id: &str,
    source_path: &str,
    markup_language: i64,
    body: &str,
    resources: &BTreeMap<String, JexVerifiedResource>,
) -> Result<(JexBodyConversion, Option<JexBodyFidelityBlocker>)> {
    let blocker = match convert_jex_note_body(
        source_note_id,
        source_path,
        markup_language,
        body,
        resources,
    ) {
        Ok(conversion) => return Ok((conversion, None)),
        Err(blocker) => blocker,
    };
    if matches!(
        blocker.kind,
        JexBodyBlockerKind::BodyTooLarge
            | JexBodyBlockerKind::ResourceIdCollision
            | JexBodyBlockerKind::ParserBudget
            | JexBodyBlockerKind::UrlBudget
    ) {
        return Err(blocker);
    }
    let normalized: BTreeMap<_, _> = resources
        .iter()
        .map(|(id, value)| (id.to_ascii_lowercase(), value.clone()))
        .collect();
    if markup_language == 1 {
        // Use events from the full parser so reference-style links keep their
        // resolved targets. Only the failing top-level block is degraded.
        let mut blocks = Vec::new();
        let mut events = Vec::new();
        let mut depth = 0usize;
        let mut start = 0;
        let mut count = 0usize;
        for (event, range) in Parser::new_ext(body, markdown_options()).into_offset_iter() {
            count += 1;
            if count > MAX_EVENTS {
                return Err(blocker);
            }
            if events.is_empty() {
                start = range.start;
            }
            match &event {
                Event::Start(_) => depth += 1,
                Event::End(_) => depth = depth.saturating_sub(1),
                _ => {}
            }
            if depth > MAX_DEPTH {
                return Err(blocker);
            }
            events.push(event);
            if depth == 0 {
                let mut converter = Converter {
                    note_id: source_note_id,
                    path: source_path,
                    events: std::mem::take(&mut events),
                    cursor: 0,
                    resources: &normalized,
                    occurrences: Vec::new(),
                    url_bytes: 0,
                };
                // Markdown table source is not HTML; reading it as HTML would
                // scatter the cells instead of showing readable source.
                let is_table =
                    matches!(converter.events.first(), Some(Event::Start(Tag::Table(_))));
                match converter.blocks() {
                    Ok(part) => blocks.extend(part),
                    Err(error) => {
                        let fragment = &body[start..range.end];
                        let html_part = if error.kind == JexBodyBlockerKind::RawHtml && !is_table {
                            super::jex_html::convert_html_locally_degraded(
                                source_note_id,
                                source_path,
                                fragment,
                                &normalized,
                            )
                            .ok()
                        } else {
                            None
                        };
                        let part = match html_part {
                            Some(part) => part,
                            None => degraded_fragment(fragment, &normalized, &error)?,
                        };
                        blocks.extend_from_slice(part.document.blocks());
                    }
                }
            }
        }
        return Ok((conversion_from_blocks(blocks), Some(blocker)));
    }
    if markup_language == 2 {
        // A malformed HTML tree cannot be safely split. Refuse it rather than
        // silently turning the whole note into escaped markup.
        let conversion = super::jex_html::convert_html_locally_degraded(
            source_note_id,
            source_path,
            body,
            &normalized,
        )?;
        return Ok((conversion, Some(blocker)));
    }
    Err(blocker)
}

pub(super) fn conversion_from_blocks(blocks: Vec<Block>) -> JexBodyConversion {
    let document = CanonicalDocument::from_blocks(blocks);
    JexBodyConversion {
        canonical_html: document.to_canonical_html().as_str().to_owned(),
        search_text: document.search_text().as_str().to_owned(),
        ordered_resource_occurrences: document.resource_ids(),
        document,
    }
}

fn degraded_fragment(
    body: &str,
    resources: &BTreeMap<String, JexVerifiedResource>,
    blocker: &JexBodyFidelityBlocker,
) -> Result<JexBodyConversion> {
    let mut html = String::new();
    for line in body
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
    {
        html.push_str("<p>");
        for ch in line.chars() {
            match ch {
                '&' => html.push_str("&amp;"),
                '<' => html.push_str("&lt;"),
                '>' => html.push_str("&gt;"),
                '"' => html.push_str("&quot;"),
                _ => html.push(ch),
            }
        }
        html.push_str("</p>");
    }
    let parsed = CanonicalDocument::parse_html(&html).map_err(|_| blocker.clone())?;
    let lowered: BTreeMap<String, &JexVerifiedResource> = resources
        .iter()
        .map(|(id, resource)| (id.to_ascii_lowercase(), resource))
        .collect();
    let mut blocks = parsed.blocks().to_vec();
    let mut attached = Vec::new();
    let bytes = body.as_bytes();
    for (index, _) in body.match_indices(":/") {
        let Some(id) = bytes.get(index + 2..index + 34) else {
            continue;
        };
        if !id.iter().all(u8::is_ascii_hexdigit) {
            continue;
        }
        let id = String::from_utf8_lossy(id).to_ascii_lowercase();
        if let Some(resource) = lowered.get(&id)
            && !attached.contains(&resource.destination_id)
        {
            attached.push(resource.destination_id.clone());
            blocks.push(Block::Attachment {
                resource_id: resource.destination_id.clone(),
                filename: resource.filename.clone(),
                media_type: resource.mime.clone(),
            });
        }
    }
    let document = CanonicalDocument::from_blocks(blocks);
    let conversion = JexBodyConversion {
        canonical_html: document.to_canonical_html().as_str().to_owned(),
        search_text: document.search_text().as_str().to_owned(),
        ordered_resource_occurrences: document.resource_ids(),
        document,
    };
    Ok(conversion)
}

fn markdown_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_GFM
        | Options::ENABLE_MATH
        | Options::ENABLE_DEFINITION_LIST
        | Options::ENABLE_WIKILINKS
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
    if markup_language != 1 && markup_language != 2 {
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
                JexBodyBlockerKind::ResourceIdCollision,
                "Case-fold conflict in verified source resource map",
            ));
        }
    }
    if markup_language == 2 {
        return super::jex_html::convert_html_body(
            source_note_id,
            source_path,
            body,
            &normalized_resources,
        );
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
