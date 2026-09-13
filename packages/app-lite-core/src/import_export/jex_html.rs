//! Strict, XHTML-like subset for JEX HTML bodies. Every source node and
//! attribute is accounted for before canonical DTO construction.

use std::collections::BTreeMap;

use quick_xml::{
    Reader,
    events::{BytesStart, Event},
};

use crate::document::{
    Block, BlockStyle, CanonicalDocument, HeadingLevel, ImagePresentation, Inline, ListItem,
    ListKind, Marks,
};
use crate::resource::ResourceId;

use super::jex_body::{
    JexBodyBlockerKind as Kind, JexBodyConversion, JexBodyFidelityBlocker, JexVerifiedResource,
};

const MAX_NODES: usize = 50_000;
const MAX_DEPTH: usize = 64;
const MAX_URL_BYTES: usize = 8 * 1024;
const MAX_TOTAL_URL_BYTES: usize = 64 * 1024;

type Result<T> = std::result::Result<T, JexBodyFidelityBlocker>;

#[derive(Debug)]
enum Node {
    Text(String),
    Element(Element),
}

#[derive(Debug)]
struct Element {
    tag: String,
    attrs: BTreeMap<String, String>,
    children: Vec<Node>,
}

fn blocked(note_id: &str, path: &str, kind: Kind, reason: &'static str) -> JexBodyFidelityBlocker {
    JexBodyFidelityBlocker {
        kind,
        source_note_id: note_id.to_owned(),
        source_path: path.to_owned(),
        reason: reason.to_owned(),
    }
}

fn formatting_whitespace(text: &str) -> bool {
    text.bytes()
        .all(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
}

fn element_from_start(start: &BytesStart<'_>, note_id: &str, path: &str) -> Result<Element> {
    let tag = std::str::from_utf8(start.name().as_ref())
        .map_err(|_| {
            blocked(
                note_id,
                path,
                Kind::UnsupportedStructure,
                "HTML tag name is not UTF-8",
            )
        })?
        .to_ascii_lowercase();
    let mut attrs = BTreeMap::new();
    for attribute in start.attributes().with_checks(true) {
        let attribute = attribute.map_err(|_| {
            blocked(
                note_id,
                path,
                Kind::UnsupportedAttribute,
                "Malformed or duplicate HTML attribute",
            )
        })?;
        let name = std::str::from_utf8(attribute.key.as_ref())
            .map_err(|_| {
                blocked(
                    note_id,
                    path,
                    Kind::UnsupportedAttribute,
                    "HTML attribute name is not UTF-8",
                )
            })?
            .to_ascii_lowercase();
        let value = attribute
            .unescape_value()
            .map_err(|_| {
                blocked(
                    note_id,
                    path,
                    Kind::UnsupportedAttribute,
                    "Unknown or malformed HTML attribute entity",
                )
            })?
            .into_owned();
        if attrs.insert(name, value).is_some() {
            return Err(blocked(
                note_id,
                path,
                Kind::UnsupportedAttribute,
                "Case-insensitive duplicate HTML attribute",
            ));
        }
    }
    Ok(Element {
        tag,
        attrs,
        children: Vec::new(),
    })
}

fn append(node: Node, stack: &mut [Element], root: &mut Vec<Node>) {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(node);
    } else {
        root.push(node);
    }
}

fn parse(body: &str, note_id: &str, path: &str) -> Result<Vec<Node>> {
    let mut reader = Reader::from_reader(body.as_bytes());
    let mut buffer = Vec::new();
    let mut root = Vec::new();
    let mut stack: Vec<Element> = Vec::new();
    let mut nodes = 0usize;
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(event @ (Event::Start(_) | Event::Empty(_))) => {
                let is_empty = matches!(event, Event::Empty(_));
                let start = match event {
                    Event::Start(start) | Event::Empty(start) => start,
                    _ => unreachable!(),
                };
                let element = element_from_start(&start, note_id, path)?;
                nodes += 1;
                if nodes > MAX_NODES {
                    return Err(blocked(
                        note_id,
                        path,
                        Kind::ParserBudget,
                        "HTML node budget exceeded",
                    ));
                }
                if matches!(element.tag.as_str(), "br" | "img") || is_empty {
                    append(Node::Element(element), &mut stack, &mut root);
                } else {
                    if stack.len() >= MAX_DEPTH {
                        return Err(blocked(
                            note_id,
                            path,
                            Kind::ParserBudget,
                            "HTML depth budget exceeded",
                        ));
                    }
                    stack.push(element);
                }
            }
            Ok(Event::End(end)) => {
                let tag = std::str::from_utf8(end.name().as_ref())
                    .map_err(|_| {
                        blocked(
                            note_id,
                            path,
                            Kind::UnsupportedStructure,
                            "HTML end tag name is not UTF-8",
                        )
                    })?
                    .to_ascii_lowercase();
                let element = stack.pop().ok_or_else(|| {
                    blocked(
                        note_id,
                        path,
                        Kind::UnsupportedStructure,
                        "Unexpected HTML end tag",
                    )
                })?;
                if tag != element.tag {
                    return Err(blocked(
                        note_id,
                        path,
                        Kind::UnsupportedStructure,
                        "Mismatched HTML end tag",
                    ));
                }
                append(Node::Element(element), &mut stack, &mut root);
            }
            Ok(Event::Text(text)) => {
                let text = text
                    .unescape()
                    .map_err(|_| {
                        blocked(
                            note_id,
                            path,
                            Kind::UnsupportedStructure,
                            "Unknown or malformed HTML entity",
                        )
                    })?
                    .into_owned();
                nodes += 1;
                if nodes > MAX_NODES {
                    return Err(blocked(
                        note_id,
                        path,
                        Kind::ParserBudget,
                        "HTML node budget exceeded",
                    ));
                }
                append(Node::Text(text), &mut stack, &mut root);
            }
            Ok(Event::Eof) => break,
            Ok(_) => {
                return Err(blocked(
                    note_id,
                    path,
                    Kind::UnsupportedStructure,
                    "HTML declaration, comment, CDATA, or processing instruction is unsupported",
                ));
            }
            Err(_) => {
                return Err(blocked(
                    note_id,
                    path,
                    Kind::UnsupportedStructure,
                    "Malformed HTML fragment",
                ));
            }
        }
        buffer.clear();
    }
    if !stack.is_empty() {
        return Err(blocked(
            note_id,
            path,
            Kind::UnsupportedStructure,
            "Unclosed HTML element",
        ));
    }
    Ok(root)
}

struct Context<'a> {
    note_id: &'a str,
    path: &'a str,
    resources: &'a BTreeMap<String, JexVerifiedResource>,
    occurrences: Vec<ResourceId>,
    url_bytes: usize,
}

impl Context<'_> {
    fn block<T>(&self, kind: Kind, reason: &'static str) -> Result<T> {
        Err(blocked(self.note_id, self.path, kind, reason))
    }

    fn attrs(&self, element: &Element, allowed: &[&str]) -> Result<()> {
        if element
            .attrs
            .keys()
            .any(|key| !allowed.contains(&key.as_str()))
        {
            return self.block(
                Kind::UnsupportedAttribute,
                "HTML element has an unsupported visual or semantic attribute",
            );
        }
        Ok(())
    }

    fn charge_url(&mut self, url: &str) -> Result<()> {
        self.url_bytes = self.url_bytes.saturating_add(url.len());
        if url.len() > MAX_URL_BYTES || self.url_bytes > MAX_TOTAL_URL_BYTES {
            return self.block(Kind::UrlBudget, "HTML URL exceeds the safe byte budget");
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

    fn external_url(&mut self, url: &str) -> Result<String> {
        self.charge_url(url)?;
        let safe_scheme =
            url.starts_with("https://") || url.starts_with("http://") || url.starts_with("mailto:");
        let safe_bytes = !url.bytes().any(|byte| {
            byte.is_ascii_control() || matches!(byte, b' ' | b'<' | b'>' | b'\'' | b'\"')
        });
        let target = url
            .split_once(':')
            .is_some_and(|(_, value)| !value.trim_start_matches('/').is_empty());
        if !safe_scheme || !safe_bytes || !target {
            return self.block(Kind::UnsafeLink, "HTML link is not a safe absolute URL");
        }
        Ok(url.to_owned())
    }

    fn image(&mut self, element: &Element, in_link: bool) -> Result<Inline> {
        if in_link {
            return self.block(
                Kind::LinkedImage,
                "Linked image cannot preserve both targets",
            );
        }
        self.attrs(element, &["src", "alt"])?;
        if !element.children.is_empty() {
            return self.block(
                Kind::UnsupportedStructure,
                "HTML image cannot contain children",
            );
        }
        let src = element.attrs.get("src").ok_or_else(|| {
            blocked(
                self.note_id,
                self.path,
                Kind::UnsupportedAttribute,
                "HTML image missing src",
            )
        })?;
        self.charge_url(src)?;
        if !src.starts_with(":/") {
            return self.block(
                Kind::UnsupportedImageSource,
                "Only verified JEX resource images are supported",
            );
        }
        let resource = match self.resource(src) {
            Some(resource) => resource.clone(),
            None => {
                return self.block(
                    Kind::UnverifiedResource,
                    "HTML image target is not a verified JEX resource",
                );
            }
        };
        if !matches!(
            resource.mime.as_str(),
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        ) {
            return self.block(
                Kind::AmbiguousAttachment,
                "HTML image target has noncanonical image media type",
            );
        }
        self.occurrences.push(resource.destination_id.clone());
        Ok(Inline::Image {
            resource_id: resource.destination_id,
            alt: element.attrs.get("alt").cloned().unwrap_or_default(),
        })
    }

    fn inline(
        &mut self,
        node: &Node,
        marks: &Marks,
        in_link: bool,
        out: &mut Vec<Inline>,
    ) -> Result<()> {
        let element = match node {
            Node::Text(text) => {
                out.push(Inline::Text {
                    text: text.clone(),
                    marks: marks.clone(),
                });
                return Ok(());
            }
            Node::Element(element) => element,
        };
        if element.tag == "img" {
            out.push(self.image(element, in_link)?);
            return Ok(());
        }
        if element.tag == "br" {
            self.attrs(element, &[])?;
            if !element.children.is_empty() {
                return self.block(
                    Kind::UnsupportedStructure,
                    "HTML break cannot contain children",
                );
            }
            out.push(Inline::SoftBreak);
            return Ok(());
        }
        let mut nested = marks.clone();
        match element.tag.as_str() {
            "b" | "strong" => nested.bold = true,
            "i" | "em" => nested.italic = true,
            "u" => nested.underline = true,
            "s" | "strike" | "del" => nested.strikethrough = true,
            "mark" => nested.highlight = true,
            "code" => nested.inline_code = true,
            "a" => {
                if in_link {
                    return self.block(
                        Kind::UnsupportedStructure,
                        "Nested HTML links are unsupported",
                    );
                }
                self.attrs(element, &["href"])?;
                let href = element.attrs.get("href").ok_or_else(|| {
                    blocked(
                        self.note_id,
                        self.path,
                        Kind::UnsupportedAttribute,
                        "HTML link missing href",
                    )
                })?;
                if href.starts_with(":/") {
                    if self.resource(href).is_some() {
                        return self.block(
                            Kind::AmbiguousAttachment,
                            "Attachment link must occupy its own block",
                        );
                    }
                    return self.block(
                        Kind::InternalNoteLink,
                        "Internal note link has no verified canonical target",
                    );
                }
                nested.link = Some(self.external_url(href)?);
            }
            _ => {
                return self.block(
                    Kind::UnsupportedStructure,
                    "Unsupported HTML inline element",
                );
            }
        }
        if element.tag != "a" {
            self.attrs(element, &[])?;
        }
        if element.children.is_empty() {
            return self.block(
                Kind::UnsupportedStructure,
                "Empty HTML inline wrapper would lose structure",
            );
        }
        for child in &element.children {
            self.inline(child, &nested, in_link || element.tag == "a", out)?;
        }
        Ok(())
    }

    fn attachment(&mut self, element: &Element) -> Result<Block> {
        self.attrs(element, &["href"])?;
        let href = element.attrs.get("href").ok_or_else(|| {
            blocked(
                self.note_id,
                self.path,
                Kind::UnsupportedAttribute,
                "Attachment link missing href",
            )
        })?;
        self.charge_url(href)?;
        let resource = match self.resource(href) {
            Some(resource) => resource.clone(),
            None => {
                return self.block(
                    Kind::InternalNoteLink,
                    "Internal link is not a verified attachment resource",
                );
            }
        };
        if resource.mime.is_empty() || resource.mime.starts_with("image/") {
            return self.block(
                Kind::AmbiguousAttachment,
                "Attachment target is an image or has no media type",
            );
        }
        if element.children.len() != 1
            || !matches!(&element.children[0], Node::Text(label) if label == &resource.filename)
        {
            return self.block(
                Kind::AmbiguousAttachment,
                "Standalone attachment label must match verified filename",
            );
        }
        self.occurrences.push(resource.destination_id.clone());
        Ok(Block::Attachment {
            resource_id: resource.destination_id,
            filename: resource.filename,
            media_type: resource.mime,
        })
    }

    fn list(&mut self, element: &Element) -> Result<Block> {
        let checklist = element.tag == "ul"
            && element
                .attrs
                .get("data-type")
                .is_some_and(|value| value == "checklist");
        self.attrs(element, if checklist { &["data-type"] } else { &[] })?;
        let kind = if checklist {
            ListKind::Checklist
        } else if element.tag == "ol" {
            ListKind::Ordered
        } else {
            ListKind::Unordered
        };
        let mut items = Vec::new();
        for child in &element.children {
            let item = match child {
                Node::Text(text) if formatting_whitespace(text) => continue,
                Node::Element(item) if item.tag == "li" => item,
                _ => {
                    return self.block(
                        Kind::UnsupportedStructure,
                        "HTML list contains non-item content",
                    );
                }
            };
            self.attrs(item, if checklist { &["data-checked"] } else { &[] })?;
            let checked = if checklist {
                match item.attrs.get("data-checked").map(String::as_str) {
                    Some("true") => Some(true),
                    Some("false") => Some(false),
                    _ => {
                        return self.block(
                            Kind::UnsupportedAttribute,
                            "Checklist item requires true/false data-checked",
                        );
                    }
                }
            } else {
                None
            };
            let mut inlines = Vec::new();
            for child in &item.children {
                self.inline(child, &Marks::default(), false, &mut inlines)?;
            }
            items.push(ListItem {
                checked,
                style: BlockStyle::default(),
                inlines,
            });
        }
        if items.is_empty() {
            return self.block(
                Kind::UnsupportedStructure,
                "Empty HTML list has no canonical items",
            );
        }
        Ok(Block::List { kind, items })
    }

    fn element_block(&mut self, element: &Element) -> Result<Block> {
        match element.tag.as_str() {
            "p" | "div" | "h1" | "h2" | "h3" => {
                self.attrs(element, &[])?;
                if element.tag == "p" && element.children.len() == 1 {
                    if let Node::Element(anchor) = &element.children[0] {
                        if anchor.tag == "a"
                            && anchor
                                .attrs
                                .get("href")
                                .is_some_and(|href| href.starts_with(":/"))
                        {
                            return self.attachment(anchor);
                        }
                    }
                }
                let mut inlines = Vec::new();
                for child in &element.children {
                    self.inline(child, &Marks::default(), false, &mut inlines)?;
                }
                let style = BlockStyle::default();
                match element.tag.as_str() {
                    "h1" => Ok(Block::Heading {
                        level: HeadingLevel::One,
                        style,
                        inlines,
                    }),
                    "h2" => Ok(Block::Heading {
                        level: HeadingLevel::Two,
                        style,
                        inlines,
                    }),
                    "h3" => Ok(Block::Heading {
                        level: HeadingLevel::Three,
                        style,
                        inlines,
                    }),
                    _ => Ok(Block::Paragraph { style, inlines }),
                }
            }
            "h4" | "h5" | "h6" => self.block(
                Kind::UnsupportedHeading,
                "HTML heading level exceeds canonical support",
            ),
            "ul" | "ol" => self.list(element),
            "a" if element
                .attrs
                .get("href")
                .is_some_and(|href| href.starts_with(":/")) =>
            {
                self.attachment(element)
            }
            "img" => {
                let image = self.image(element, false)?;
                if let Inline::Image { resource_id, alt } = image {
                    Ok(Block::Image {
                        resource_id,
                        alt,
                        presentation: ImagePresentation::default(),
                    })
                } else {
                    unreachable!()
                }
            }
            _ => self.block(Kind::UnsupportedStructure, "Unsupported HTML block element"),
        }
    }
}

fn source_visible_text(node: &Node) -> String {
    match node {
        Node::Text(text) => text.clone(),
        Node::Element(element) => match element.tag.as_str() {
            "img" => element.attrs.get("alt").cloned().unwrap_or_default(),
            "br" => "\n".into(),
            "ul" | "ol" => element
                .children
                .iter()
                .filter_map(|child| match child {
                    Node::Element(item) if item.tag == "li" => Some(source_visible_text(child)),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => element.children.iter().map(source_visible_text).collect(),
        },
    }
}

pub(super) fn convert_html_body(
    note_id: &str,
    path: &str,
    body: &str,
    resources: &BTreeMap<String, JexVerifiedResource>,
) -> Result<JexBodyConversion> {
    let mut root = parse(body, note_id, path)?;
    root.retain(|node| !matches!(node, Node::Text(text) if formatting_whitespace(text)));
    if root.len() == 1 && matches!(&root[0], Node::Element(element) if element.tag == "html") {
        let Node::Element(html) = root.remove(0) else {
            unreachable!()
        };
        if !html.attrs.is_empty() {
            return Err(blocked(
                note_id,
                path,
                Kind::UnsupportedAttribute,
                "HTML wrapper attributes are unsupported",
            ));
        }
        let mut children = html
            .children
            .into_iter()
            .filter(|node| !matches!(node, Node::Text(text) if formatting_whitespace(text)));
        let Some(Node::Element(body_element)) = children.next() else {
            return Err(blocked(
                note_id,
                path,
                Kind::UnsupportedStructure,
                "HTML wrapper requires a body",
            ));
        };
        if children.next().is_some() || body_element.tag != "body" {
            return Err(blocked(
                note_id,
                path,
                Kind::UnsupportedStructure,
                "HTML wrapper has unsupported head or extra content",
            ));
        }
        if !body_element.attrs.is_empty() {
            return Err(blocked(
                note_id,
                path,
                Kind::UnsupportedAttribute,
                "HTML body attributes are unsupported",
            ));
        }
        root = body_element.children;
    }
    let mut context = Context {
        note_id,
        path,
        resources,
        occurrences: Vec::new(),
        url_bytes: 0,
    };
    let mut blocks = Vec::new();
    let mut source_segments = Vec::new();
    for node in &root {
        match node {
            Node::Text(text) if formatting_whitespace(text) => continue,
            Node::Text(_) => {
                return context.block(
                    Kind::UnsupportedStructure,
                    "Visible HTML text outside a block",
                );
            }
            Node::Element(element) => {
                source_segments.push(source_visible_text(node));
                blocks.push(context.element_block(element)?);
            }
        }
    }
    let source_search_text = source_segments.join("\n");
    let document = CanonicalDocument::from_blocks(blocks);
    let canonical_html = document.to_canonical_html().as_str().to_owned();
    let search_text = document.search_text().as_str().to_owned();
    let roundtrip = CanonicalDocument::parse_html(&canonical_html).map_err(|_| {
        blocked(
            note_id,
            path,
            Kind::UnsupportedStructure,
            "Canonical HTML could not be reparsed",
        )
    })?;
    if source_search_text != search_text
        || document.blocks().len() != source_segments.len()
        || roundtrip != document
        || roundtrip.search_text().as_str() != search_text
        || document.resource_ids() != context.occurrences
    {
        return Err(blocked(
            note_id,
            path,
            Kind::UnsupportedStructure,
            "HTML source text, structure, or resource order would be lost",
        ));
    }
    Ok(JexBodyConversion {
        document,
        canonical_html,
        search_text,
        ordered_resource_occurrences: context.occurrences,
    })
}
