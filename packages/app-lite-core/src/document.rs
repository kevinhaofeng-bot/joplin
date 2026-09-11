use crate::resource::ResourceId;
use html5ever::interface::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::{Attribute, ParseOpts, QualName, local_name, ns, parse_fragment};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::{borrow::Cow, fmt};
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DocumentError {
    #[error("HTML parser failed")]
    Parse,
    #[error("HTML document exceeds the maximum DOM depth of {limit}")]
    DepthLimit { limit: usize },
    #[error("HTML document exceeds the maximum DOM node count of {limit}")]
    NodeLimit { limit: usize },
}

// These limits protect the projection and destruction paths without imposing a
// small byte limit on ordinary long notes. The parser itself remains HTML5;
// only the in-memory tree we are willing to project is bounded.
const MAX_DOM_DEPTH: usize = 4096;
const MAX_DOM_NODES: usize = 1_000_000;
const MAX_LINK_LENGTH: usize = 8 * 1024;
const MAX_RETAINED_LINK_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalHtml(String);

impl CanonicalHtml {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchText(String);

impl SearchText {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CanonicalDocument {
    blocks: Vec<Block>,
}

impl CanonicalDocument {
    pub fn from_blocks(blocks: Vec<Block>) -> Self {
        Self {
            blocks: normalize_blocks(blocks),
        }
    }

    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    fn normalized(&self) -> Self {
        Self::from_blocks(self.blocks.clone())
    }

    pub fn parse_html(input: &str) -> Result<Self, DocumentError> {
        parse_html(input)
    }

    pub fn to_canonical_html(&self) -> CanonicalHtml {
        CanonicalHtml(serialize_html(self))
    }

    pub fn search_text(&self) -> SearchText {
        SearchText(search_text(self))
    }

    pub fn resource_ids(&self) -> Vec<ResourceId> {
        resource_ids(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph {
        style: BlockStyle,
        inlines: Vec<Inline>,
    },
    Heading {
        level: HeadingLevel,
        style: BlockStyle,
        inlines: Vec<Inline>,
    },
    List {
        kind: ListKind,
        items: Vec<ListItem>,
    },
    /// A quoted text block. Keeping quote as a first-class canonical node
    /// avoids representing it as indentation, which the native codec must
    /// deliberately reject rather than flatten.
    Quote {
        style: BlockStyle,
        inlines: Vec<Inline>,
    },
    /// A preformatted text block. The text remains represented by the same
    /// safe inline DTO, so marks and soft breaks have one deterministic path.
    Code {
        style: BlockStyle,
        inlines: Vec<Inline>,
    },
    /// A resource-backed image at block position. Inline images remain part
    /// of the legacy HTML projection; Task 4's native editor maps this
    /// structural form without inventing an import transaction.
    Image {
        resource_id: ResourceId,
        alt: String,
    },
    /// A resource-backed attachment card at block position.
    Attachment {
        resource_id: ResourceId,
        filename: String,
        media_type: String,
    },
    /// A semantic horizontal divider.
    Divider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadingLevel {
    One,
    Two,
    Three,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockStyle {
    pub alignment: Alignment,
    pub indent: u8,
}

impl Default for BlockStyle {
    fn default() -> Self {
        Self {
            alignment: Alignment::Left,
            indent: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKind {
    Unordered,
    Ordered,
    Checklist,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItem {
    pub checked: Option<bool>,
    pub style: BlockStyle,
    pub inlines: Vec<Inline>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Marks {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub highlight: bool,
    pub link: Option<String>,
    pub inline_code: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    Text {
        text: String,
        marks: Marks,
    },
    SoftBreak,
    Image {
        resource_id: ResourceId,
        alt: String,
    },
}

fn parse_html(input: &str) -> Result<CanonicalDocument, DocumentError> {
    let sink = DomSink::default();
    let root = parse_fragment(
        sink,
        ParseOpts::default(),
        QualName::new(None, ns!(html), local_name!(body)),
        Vec::new(),
        false,
    )
    .from_utf8()
    .one(input.as_bytes());

    if let Err(error) = check_dom_limits(&root) {
        drain_dom(root);
        return Err(error);
    }

    let document = project_dom(&root);
    drain_dom(root);
    Ok(document)
}

fn serialize_html(document: &CanonicalDocument) -> String {
    let document = document.normalized();
    if document.blocks.iter().all(block_is_empty) {
        return String::new();
    }
    let mut output = String::new();
    for block in &document.blocks {
        match block {
            Block::Paragraph { style, inlines } => {
                serialize_block("p", style, inlines, &mut output)
            }
            Block::Heading {
                level,
                style,
                inlines,
            } => {
                let tag = match level {
                    HeadingLevel::One => "h1",
                    HeadingLevel::Two => "h2",
                    HeadingLevel::Three => "h3",
                };
                serialize_block(tag, style, inlines, &mut output);
            }
            Block::List { kind, items } => {
                let (tag, checklist) = match kind {
                    ListKind::Unordered => ("ul", false),
                    ListKind::Ordered => ("ol", false),
                    ListKind::Checklist => ("ul", true),
                };
                output.push('<');
                output.push_str(tag);
                if checklist {
                    output.push_str(" data-type=\"checklist\"");
                }
                output.push('>');
                for item in items {
                    output.push_str("<li");
                    if checklist {
                        output.push_str(" data-checked=\"");
                        output.push_str(if item.checked == Some(true) {
                            "true"
                        } else {
                            "false"
                        });
                        output.push('"');
                    }
                    serialize_style_attributes(&item.style, &mut output);
                    output.push('>');
                    if item.inlines.is_empty() {
                        output.push_str("<br data-joplin-lite-empty-item=\"true\">");
                    } else if item.inlines.len() == 1
                        && matches!(item.inlines[0], Inline::SoftBreak)
                    {
                        output.push_str("<br data-joplin-lite-soft-break=\"true\">");
                    } else {
                        serialize_inlines(&item.inlines, &mut output);
                    }
                    output.push_str("</li>");
                }
                output.push_str("</");
                output.push_str(tag);
                output.push('>');
            }
            // These markers distinguish the native editor's structural
            // blocks from legacy HTML. In particular, old `<pre>` content
            // remains a preformatted paragraph instead of changing shape on
            // its next save.
            Block::Quote { style, inlines } => serialize_marked_block(
                "blockquote",
                "data-joplin-lite-block-quote",
                style,
                inlines,
                &mut output,
            ),
            Block::Code { style, inlines } => serialize_marked_block(
                "pre",
                "data-joplin-lite-block-code",
                style,
                inlines,
                &mut output,
            ),
            Block::Image { resource_id, alt } => {
                output.push_str("<img data-joplin-lite-block-image=\"true\" src=\":/");
                escape_attribute(resource_id.as_str(), &mut output);
                output.push_str("\" alt=\"");
                escape_attribute(alt, &mut output);
                output.push_str("\">");
            }
            Block::Attachment {
                resource_id,
                filename,
                media_type,
            } => {
                output.push_str("<a data-joplin-lite-block-attachment=\"true\" href=\":/");
                escape_attribute(resource_id.as_str(), &mut output);
                output.push_str("\" data-resource-id=\"");
                escape_attribute(resource_id.as_str(), &mut output);
                output.push_str("\" data-filename=\"");
                escape_attribute(filename, &mut output);
                output.push_str("\" data-media-type=\"");
                escape_attribute(media_type, &mut output);
                output.push_str("\">");
                escape_text_run(filename, &[], 0, &mut output);
                output.push_str("</a>");
            }
            Block::Divider => output.push_str("<hr data-joplin-lite-block-divider=\"true\">"),
        }
    }
    output
}

fn serialize_block(tag: &str, style: &BlockStyle, inlines: &[Inline], output: &mut String) {
    serialize_optional_marked_block(tag, None, style, inlines, output);
}

fn serialize_marked_block(
    tag: &str,
    marker: &str,
    style: &BlockStyle,
    inlines: &[Inline],
    output: &mut String,
) {
    serialize_optional_marked_block(tag, Some(marker), style, inlines, output);
}

fn serialize_optional_marked_block(
    tag: &str,
    marker: Option<&str>,
    style: &BlockStyle,
    inlines: &[Inline],
    output: &mut String,
) {
    output.push('<');
    output.push_str(tag);
    if let Some(marker) = marker {
        output.push(' ');
        output.push_str(marker);
        output.push_str("=\"true\"");
    }
    serialize_style_attributes(style, output);
    output.push('>');
    if inlines.is_empty() {
        output.push_str("<br>");
    } else if inlines.len() == 1 && matches!(inlines[0], Inline::SoftBreak) {
        output.push_str("<br data-joplin-lite-soft-break=\"true\">");
    } else {
        serialize_inlines(inlines, output);
    }
    output.push_str("</");
    output.push_str(tag);
    output.push('>');
}

fn serialize_style_attributes(style: &BlockStyle, output: &mut String) {
    match style.alignment {
        Alignment::Center => output.push_str(" data-align=\"center\""),
        Alignment::Right => output.push_str(" data-align=\"right\""),
        Alignment::Left => {}
    }
    let indent = style.indent.min(8);
    if indent > 0 {
        output.push_str(" data-indent=\"");
        output.push_str(&indent.to_string());
        output.push('"');
    }
}

fn search_text(document: &CanonicalDocument) -> String {
    let mut output = String::new();
    for (index, block) in document.blocks.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        match block {
            Block::Paragraph { inlines, .. }
            | Block::Heading { inlines, .. }
            | Block::Quote { inlines, .. }
            | Block::Code { inlines, .. } => {
                append_search_inlines(inlines, &mut output);
            }
            Block::List { items, .. } => {
                for (item_index, item) in items.iter().enumerate() {
                    if item_index > 0 {
                        output.push('\n');
                    }
                    append_search_inlines(&item.inlines, &mut output);
                }
            }
            Block::Image { alt, .. } => output.push_str(alt),
            Block::Attachment { filename, .. } => output.push_str(filename),
            Block::Divider => {}
        }
    }
    output
}

fn append_search_inlines(inlines: &[Inline], output: &mut String) {
    for inline in inlines {
        match inline {
            Inline::Text { text, .. } => output.push_str(text),
            Inline::SoftBreak => output.push('\n'),
            Inline::Image { alt, .. } => output.push_str(alt),
        }
    }
}

fn resource_ids(document: &CanonicalDocument) -> Vec<ResourceId> {
    document
        .blocks
        .iter()
        .flat_map(|block| match block {
            Block::Paragraph { inlines, .. }
            | Block::Heading { inlines, .. }
            | Block::Quote { inlines, .. }
            | Block::Code { inlines, .. } => inlines
                .iter()
                .filter_map(|inline| match inline {
                    Inline::Image { resource_id, .. } => Some(resource_id.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            Block::List { items, .. } => items
                .iter()
                .flat_map(|item| item.inlines.iter())
                .filter_map(|inline| match inline {
                    Inline::Image { resource_id, .. } => Some(resource_id.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            Block::Image { resource_id, .. } | Block::Attachment { resource_id, .. } => {
                vec![resource_id.clone()]
            }
            Block::Divider => Vec::new(),
        })
        .collect()
}

fn block_is_empty(block: &Block) -> bool {
    match block {
        Block::Paragraph { style, inlines } if *style == BlockStyle::default() => {
            inlines.iter().all(|inline| match inline {
                Inline::Text { text, .. } => text.is_empty(),
                Inline::SoftBreak => false,
                Inline::Image { .. } => false,
            })
        }
        Block::Paragraph { .. } => false,
        // Headings and lists are semantic blocks even when they have no
        // visible text. They must retain a reversible placeholder instead of
        // disappearing as an all-empty document.
        Block::Heading { .. }
        | Block::List { .. }
        | Block::Quote { .. }
        | Block::Code { .. }
        | Block::Image { .. }
        | Block::Attachment { .. }
        | Block::Divider => false,
    }
}

fn normalize_blocks(blocks: Vec<Block>) -> Vec<Block> {
    blocks
        .into_iter()
        .map(|block| match block {
            Block::Paragraph { style, inlines } => Block::Paragraph {
                style: normalize_style(style),
                inlines: normalize_inlines(inlines),
            },
            Block::Heading {
                level,
                style,
                inlines,
            } => Block::Heading {
                level,
                style: normalize_style(style),
                inlines: normalize_inlines(inlines),
            },
            Block::List { kind, items } => Block::List {
                kind,
                items: items
                    .into_iter()
                    .map(|item| ListItem {
                        checked: if kind == ListKind::Checklist {
                            Some(item.checked.unwrap_or(false))
                        } else {
                            None
                        },
                        style: normalize_style(item.style),
                        inlines: normalize_inlines(item.inlines),
                    })
                    .collect(),
            },
            Block::Quote { style, inlines } => Block::Quote {
                style: normalize_style(style),
                inlines: normalize_inlines(inlines),
            },
            Block::Code { style, inlines } => Block::Code {
                style: normalize_style(style),
                inlines: normalize_inlines(inlines),
            },
            Block::Image { resource_id, alt } => Block::Image { resource_id, alt },
            Block::Attachment {
                resource_id,
                filename,
                media_type,
            } => Block::Attachment {
                resource_id,
                filename,
                media_type,
            },
            Block::Divider => Block::Divider,
        })
        .fold(Vec::new(), |mut normalized, block| {
            if let (
                Some(Block::List {
                    kind: previous_kind,
                    items: previous_items,
                }),
                Block::List { kind, items },
            ) = (normalized.last_mut(), &block)
                && previous_kind == kind
            {
                previous_items.extend(items.clone());
            } else {
                normalized.push(block);
            }
            normalized
        })
}

fn normalize_style(mut style: BlockStyle) -> BlockStyle {
    style.indent = style.indent.min(8);
    style
}

fn normalize_inlines(inlines: Vec<Inline>) -> Vec<Inline> {
    let mut normalized = Vec::new();
    for inline in inlines {
        match inline {
            Inline::Text { text, marks } => {
                let mut current = String::new();
                for character in text.chars() {
                    match character {
                        '\r' => current.push('\n'),
                        '\t' => current.push_str("    "),
                        '\n' => {
                            append_normalized_text(&mut normalized, &current, &marks);
                            current.clear();
                            normalized.push(Inline::SoftBreak);
                        }
                        _ => current.push(character),
                    }
                }
                append_normalized_text(&mut normalized, &current, &marks);
            }
            other => normalized.push(other),
        }
    }
    let snapshot = normalized.clone();
    for (index, inline) in normalized.iter_mut().enumerate() {
        let Inline::Text { text, .. } = inline else {
            continue;
        };
        let mut characters: Vec<char> = text.chars().collect();
        for position in 0..characters.len() {
            if characters[position] == ' '
                && !ordinary_space_can_collapse(&characters, position, &snapshot, index)
            {
                characters[position] = '\u{00a0}';
            }
        }
        *text = characters.into_iter().collect();
    }
    normalized
}

fn append_normalized_text(inlines: &mut Vec<Inline>, text: &str, marks: &Marks) {
    if text.is_empty() {
        return;
    }
    let marks = normalize_marks(marks);
    if let Some(Inline::Text {
        text: previous,
        marks: previous_marks,
    }) = inlines.last_mut()
        && *previous_marks == marks
    {
        previous.push_str(text);
        return;
    }
    inlines.push(Inline::Text {
        text: text.to_owned(),
        marks,
    });
}

fn normalize_marks(marks: &Marks) -> Marks {
    Marks {
        bold: marks.bold,
        italic: marks.italic,
        underline: marks.underline,
        strikethrough: marks.strikethrough,
        highlight: marks.highlight,
        link: marks.link.clone().filter(|value| valid_link(value)),
        inline_code: marks.inline_code,
    }
}

fn serialize_inlines(inlines: &[Inline], output: &mut String) {
    for (index, inline) in inlines.iter().enumerate() {
        match inline {
            Inline::Text { text, marks } => serialize_text(text, marks, inlines, index, output),
            Inline::SoftBreak => output.push_str("<br>"),
            Inline::Image { resource_id, alt } => {
                output.push_str("<img src=\":/");
                escape_attribute(resource_id.as_str(), output);
                output.push_str("\" alt=\"");
                escape_attribute(alt, output);
                output.push_str("\">");
            }
        }
    }
}

fn serialize_text(
    text: &str,
    marks: &Marks,
    inlines: &[Inline],
    index: usize,
    output: &mut String,
) {
    if let Some(link) = marks.link.as_deref() {
        output.push_str("<a href=\"");
        escape_attribute(link, output);
        output.push_str("\">");
    }
    if marks.highlight {
        output.push_str("<mark>");
    }
    if marks.strikethrough {
        output.push_str("<s>");
    }
    if marks.bold {
        output.push_str("<strong>");
    }
    if marks.italic {
        output.push_str("<em>");
    }
    if marks.underline {
        output.push_str("<u>");
    }
    if marks.inline_code {
        output.push_str("<code>");
    }
    escape_text_run(text, inlines, index, output);
    if marks.inline_code {
        output.push_str("</code>");
    }
    if marks.underline {
        output.push_str("</u>");
    }
    if marks.italic {
        output.push_str("</em>");
    }
    if marks.bold {
        output.push_str("</strong>");
    }
    if marks.strikethrough {
        output.push_str("</s>");
    }
    if marks.highlight {
        output.push_str("</mark>");
    }
    if marks.link.is_some() {
        output.push_str("</a>");
    }
}

fn escape_text_run(text: &str, inlines: &[Inline], index: usize, output: &mut String) {
    let characters: Vec<char> = text.chars().collect();
    for (position, character) in characters.iter().copied().enumerate() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#39;"),
            '\u{00a0}' => output.push_str("&nbsp;"),
            ' ' if !ordinary_space_can_collapse(&characters, position, inlines, index) => {
                output.push_str("&nbsp;")
            }
            _ => output.push(character),
        }
    }
}

fn ordinary_space_can_collapse(
    characters: &[char],
    position: usize,
    inlines: &[Inline],
    index: usize,
) -> bool {
    let previous = if position > 0 {
        Some(characters[position - 1])
    } else {
        previous_flow_char(inlines, index)
    };
    let next = if position + 1 < characters.len() {
        Some(characters[position + 1])
    } else {
        next_flow_char(inlines, index)
    };
    previous.is_some_and(|character| !matches!(character, ' ' | '\u{00a0}'))
        && next.is_some_and(|character| !matches!(character, ' ' | '\u{00a0}'))
}

fn previous_flow_char(inlines: &[Inline], index: usize) -> Option<char> {
    match index
        .checked_sub(1)
        .and_then(|previous| inlines.get(previous))
    {
        Some(Inline::Text { text, .. }) => text.chars().next_back(),
        // Images occupy an inline position in the rendered flow. Treat them
        // as a non-whitespace boundary so ordinary spaces around an image do
        // not become NBSP merely because there is no adjacent text node.
        Some(Inline::Image { .. }) => Some('\u{fffc}'),
        _ => None,
    }
}

fn next_flow_char(inlines: &[Inline], index: usize) -> Option<char> {
    match inlines.get(index + 1) {
        Some(Inline::Text { text, .. }) => text.chars().next(),
        Some(Inline::Image { .. }) => Some('\u{fffc}'),
        _ => None,
    }
}

fn escape_attribute(text: &str, output: &mut String) {
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#39;"),
            _ => output.push(character),
        }
    }
}

#[derive(Clone)]
struct DomNode {
    data: DomData,
    children: RefCell<Vec<DomHandle>>,
    parent: RefCell<Option<Weak<DomNode>>>,
}

type DomHandle = Rc<DomNode>;

#[allow(dead_code)]
#[derive(Clone)]
enum DomData {
    CanonicalDocument,
    Doctype {
        name: StrTendril,
        public_id: StrTendril,
        system_id: StrTendril,
    },
    Element {
        name: QualName,
        attrs: RefCell<Vec<Attribute>>,
        template_contents: RefCell<Option<DomHandle>>,
        mathml_annotation_xml_integration_point: bool,
    },
    Text(RefCell<StrTendril>),
    Comment(StrTendril),
    ProcessingInstruction {
        target: StrTendril,
        data: StrTendril,
    },
}

impl fmt::Debug for DomNode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("DomNode").finish_non_exhaustive()
    }
}

struct DomSink {
    document: RefCell<Option<DomHandle>>,
    quirks_mode: Cell<QuirksMode>,
}

impl Default for DomSink {
    fn default() -> Self {
        Self {
            document: RefCell::new(None),
            quirks_mode: Cell::new(QuirksMode::NoQuirks),
        }
    }
}

impl DomSink {
    fn node(data: DomData) -> DomHandle {
        Rc::new(DomNode {
            data,
            children: RefCell::new(Vec::new()),
            parent: RefCell::new(None),
        })
    }

    fn parent(handle: &DomHandle) -> Option<DomHandle> {
        handle.parent.borrow().as_ref().and_then(Weak::upgrade)
    }

    fn detach(handle: &DomHandle) {
        let Some(parent) = Self::parent(handle) else {
            return;
        };
        parent
            .children
            .borrow_mut()
            .retain(|child| !Rc::ptr_eq(child, handle));
        *handle.parent.borrow_mut() = None;
    }

    fn set_parent(parent: &DomHandle, child: &DomHandle) {
        *child.parent.borrow_mut() = Some(Rc::downgrade(parent));
    }

    fn append_node(parent: &DomHandle, child: DomHandle) {
        Self::detach(&child);
        Self::set_parent(parent, &child);
        parent.children.borrow_mut().push(child);
    }

    fn append_text(parent: &DomHandle, text: StrTendril) {
        if text.is_empty() {
            return;
        }
        let mut children = parent.children.borrow_mut();
        if let Some(last) = children.last()
            && let DomData::Text(contents) = &last.data
        {
            contents.borrow_mut().push_tendril(&text);
            return;
        }
        let child = Self::node(DomData::Text(RefCell::new(text)));
        Self::set_parent(parent, &child);
        children.push(child);
    }

    fn append_to(parent: &DomHandle, child: NodeOrText<DomHandle>) {
        match child {
            NodeOrText::AppendNode(node) => Self::append_node(parent, node),
            NodeOrText::AppendText(text) => Self::append_text(parent, text),
        }
    }
}

impl TreeSink for DomSink {
    type Handle = DomHandle;
    type Output = DomHandle;
    type ElemName<'a> = &'a QualName;

    fn finish(self) -> Self::Output {
        self.document
            .into_inner()
            .expect("html5ever always creates a document node")
    }

    fn parse_error(&self, _msg: Cow<'static, str>) {}

    fn get_document(&self) -> Self::Handle {
        if let Some(document) = self.document.borrow().as_ref() {
            return document.clone();
        }
        let document = Self::node(DomData::CanonicalDocument);
        *self.document.borrow_mut() = Some(document.clone());
        document
    }

    fn elem_name<'a>(&'a self, target: &'a Self::Handle) -> Self::ElemName<'a> {
        match &target.data {
            DomData::Element { name, .. } => name,
            _ => panic!("elem_name called for a non-element node"),
        }
    }

    fn create_element(
        &self,
        name: QualName,
        attrs: Vec<Attribute>,
        flags: ElementFlags,
    ) -> Self::Handle {
        let template_contents = RefCell::new(
            flags
                .template
                .then(|| Self::node(DomData::CanonicalDocument)),
        );
        Self::node(DomData::Element {
            name,
            attrs: RefCell::new(attrs),
            template_contents,
            mathml_annotation_xml_integration_point: flags.mathml_annotation_xml_integration_point,
        })
    }

    fn create_comment(&self, text: StrTendril) -> Self::Handle {
        Self::node(DomData::Comment(text))
    }

    fn create_pi(&self, target: StrTendril, data: StrTendril) -> Self::Handle {
        Self::node(DomData::ProcessingInstruction { target, data })
    }

    fn append(&self, parent: &Self::Handle, child: NodeOrText<Self::Handle>) {
        Self::append_to(parent, child);
    }

    fn append_based_on_parent_node(
        &self,
        element: &Self::Handle,
        prev_element: &Self::Handle,
        child: NodeOrText<Self::Handle>,
    ) {
        if Self::parent(element).is_some() {
            // The HTML5 tree builder uses this hook for foster parenting. A
            // table's stray text belongs immediately before the table, not at
            // the end of its parent.
            self.append_before_sibling(element, child);
        } else {
            Self::append_to(prev_element, child);
        }
    }

    fn append_doctype_to_document(
        &self,
        name: StrTendril,
        public_id: StrTendril,
        system_id: StrTendril,
    ) {
        let document = self.get_document();
        Self::append_node(
            &document,
            Self::node(DomData::Doctype {
                name,
                public_id,
                system_id,
            }),
        );
    }

    fn get_template_contents(&self, target: &Self::Handle) -> Self::Handle {
        match &target.data {
            DomData::Element {
                template_contents, ..
            } => template_contents
                .borrow()
                .as_ref()
                .expect("template contents")
                .clone(),
            _ => panic!("template contents requested for a non-template node"),
        }
    }

    fn same_node(&self, x: &Self::Handle, y: &Self::Handle) -> bool {
        Rc::ptr_eq(x, y)
    }

    fn set_quirks_mode(&self, mode: QuirksMode) {
        self.quirks_mode.set(mode);
    }

    fn append_before_sibling(&self, sibling: &Self::Handle, new_node: NodeOrText<Self::Handle>) {
        let Some(parent) = Self::parent(sibling) else {
            return;
        };
        let new_node = match new_node {
            NodeOrText::AppendNode(node) => {
                Self::detach(&node);
                NodeOrText::AppendNode(node)
            }
            NodeOrText::AppendText(text) => NodeOrText::AppendText(text),
        };
        let mut children = parent.children.borrow_mut();
        let Some(index) = children.iter().position(|child| Rc::ptr_eq(child, sibling)) else {
            return;
        };
        match new_node {
            NodeOrText::AppendNode(node) => {
                Self::set_parent(&parent, &node);
                children.insert(index, node);
            }
            NodeOrText::AppendText(text) => {
                if text.is_empty() {
                    return;
                }
                if index > 0
                    && let DomData::Text(contents) = &children[index - 1].data
                {
                    contents.borrow_mut().push_tendril(&text);
                } else {
                    let node = Self::node(DomData::Text(RefCell::new(text)));
                    Self::set_parent(&parent, &node);
                    children.insert(index, node);
                }
            }
        }
    }

    fn add_attrs_if_missing(&self, target: &Self::Handle, attrs: Vec<Attribute>) {
        let DomData::Element {
            attrs: existing, ..
        } = &target.data
        else {
            return;
        };
        let mut existing = existing.borrow_mut();
        for attr in attrs {
            if !existing.iter().any(|current| current.name == attr.name) {
                existing.push(attr);
            }
        }
    }

    fn remove_from_parent(&self, target: &Self::Handle) {
        Self::detach(target);
    }

    fn reparent_children(&self, node: &Self::Handle, new_parent: &Self::Handle) {
        let children = std::mem::take(&mut *node.children.borrow_mut());
        for child in children {
            *child.parent.borrow_mut() = None;
            Self::append_node(new_parent, child);
        }
    }

    fn is_mathml_annotation_xml_integration_point(&self, handle: &Self::Handle) -> bool {
        match &handle.data {
            DomData::Element {
                mathml_annotation_xml_integration_point,
                ..
            } => *mathml_annotation_xml_integration_point,
            _ => false,
        }
    }
}

fn check_dom_limits(root: &DomHandle) -> Result<(), DocumentError> {
    let mut pending = vec![(root.clone(), 0usize)];
    let mut count = 0usize;
    while let Some((node, depth)) = pending.pop() {
        count += 1;
        if count > MAX_DOM_NODES {
            return Err(DocumentError::NodeLimit {
                limit: MAX_DOM_NODES,
            });
        }
        if depth > MAX_DOM_DEPTH {
            return Err(DocumentError::DepthLimit {
                limit: MAX_DOM_DEPTH,
            });
        }
        let children = node.children.borrow();
        for child in children.iter().rev() {
            pending.push((child.clone(), depth + 1));
        }
        if let DomData::Element {
            template_contents, ..
        } = &node.data
            && let Some(contents) = template_contents.borrow().as_ref()
        {
            pending.push((contents.clone(), depth + 1));
        }
    }
    Ok(())
}

fn drain_dom(root: DomHandle) {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        *node.parent.borrow_mut() = None;
        let children = std::mem::take(&mut *node.children.borrow_mut());
        pending.extend(children);
        if let DomData::Element {
            template_contents, ..
        } = &node.data
            && let Some(contents) = template_contents.borrow_mut().take()
        {
            pending.push(contents);
        }
    }
}

fn project_dom(root: &DomHandle) -> CanonicalDocument {
    let mut projection = Projection::default();
    let mut pending = vec![ProjectionFrame::Visit {
        node: root.clone(),
        marks: ProjectionMarks::default(),
        preformatted: false,
    }];

    while let Some(frame) = pending.pop() {
        match frame {
            ProjectionFrame::FinishBlock {
                blocks_before,
                kind,
                style,
            } => projection.finish_block(blocks_before, kind, style),
            ProjectionFrame::FinishList => projection.finish_list(),
            ProjectionFrame::FinishListItem => projection.finish_list_item(),
            ProjectionFrame::FinishListBlock { preserve_empty } => {
                projection.finish_list_block(preserve_empty)
            }
            ProjectionFrame::Visit {
                node,
                marks,
                preformatted,
            } => match &node.data {
                DomData::Text(text) => {
                    projection.text(&text.borrow(), &marks, preformatted);
                }
                DomData::Element { name, attrs, .. } => {
                    let tag = name.local.to_string().to_ascii_lowercase();
                    if matches!(tag.as_str(), "script" | "style" | "head" | "title") {
                        continue;
                    }
                    let children = node.children.borrow().clone();
                    if tag == "ul" || tag == "ol" {
                        let kind = if tag == "ol" {
                            ListKind::Ordered
                        } else if attribute(&attrs.borrow(), "data-type").as_deref()
                            == Some("checklist")
                        {
                            ListKind::Checklist
                        } else {
                            ListKind::Unordered
                        };
                        projection.begin_list(kind);
                        pending.push(ProjectionFrame::FinishList);
                        for child in children.into_iter().rev() {
                            pending.push(ProjectionFrame::Visit {
                                node: child,
                                marks: marks.clone(),
                                preformatted,
                            });
                        }
                        continue;
                    }
                    if tag == "li" && !projection.list_contexts.is_empty() {
                        let style = block_style(&attrs.borrow(), 0);
                        let checked = if projection
                            .list_contexts
                            .last()
                            .is_some_and(|list| list.kind == ListKind::Checklist)
                        {
                            Some(
                                attribute(&attrs.borrow(), "data-checked").as_deref()
                                    == Some("true"),
                            )
                        } else {
                            None
                        };
                        projection.begin_list_item(style, checked);
                        pending.push(ProjectionFrame::FinishListItem);
                        for child in children.into_iter().rev() {
                            pending.push(ProjectionFrame::Visit {
                                node: child,
                                marks: marks.clone(),
                                preformatted,
                            });
                        }
                        continue;
                    }
                    if let Some(level) = heading_level(&tag) {
                        if !projection.list_contexts.is_empty() {
                            projection.begin_list_block();
                            projection.ensure_current();
                            pending.push(ProjectionFrame::FinishListBlock {
                                preserve_empty: true,
                            });
                            for child in children.into_iter().rev() {
                                pending.push(ProjectionFrame::Visit {
                                    node: child,
                                    marks: marks.clone(),
                                    preformatted,
                                });
                            }
                            continue;
                        }
                        let blocks_before = projection.document.blocks.len();
                        let style = block_style(&attrs.borrow(), 0);
                        projection.begin_block(BlockKind::Heading(level), style);
                        pending.push(ProjectionFrame::FinishBlock {
                            blocks_before,
                            kind: BlockKind::Heading(level),
                            style,
                        });
                        for child in children.into_iter().rev() {
                            pending.push(ProjectionFrame::Visit {
                                node: child,
                                marks: marks.clone(),
                                preformatted,
                            });
                        }
                        continue;
                    }
                    // The native editor writes these explicit structural
                    // markers. They are accepted only at document level: a
                    // resource card nested inside a list is not a shape our
                    // canonical model can preserve, so it remains ordinary
                    // visible inline content rather than being silently
                    // hoisted across list boundaries.
                    if projection.list_contexts.is_empty()
                        && tag == "img"
                        && attribute(&attrs.borrow(), "data-joplin-lite-block-image").as_deref()
                            == Some("true")
                    {
                        projection.block_image(&attrs.borrow());
                        continue;
                    }
                    if projection.list_contexts.is_empty()
                        && tag == "a"
                        && attribute(&attrs.borrow(), "data-joplin-lite-block-attachment")
                            .as_deref()
                            == Some("true")
                    {
                        projection.block_attachment(&attrs.borrow());
                        continue;
                    }
                    if projection.list_contexts.is_empty()
                        && tag == "hr"
                        && attribute(&attrs.borrow(), "data-joplin-lite-block-divider").as_deref()
                            == Some("true")
                    {
                        projection.block_divider();
                        continue;
                    }
                    let native_structural_text = (tag == "blockquote"
                        && attribute(&attrs.borrow(), "data-joplin-lite-block-quote").as_deref()
                            == Some("true"))
                        || (tag == "pre"
                            && attribute(&attrs.borrow(), "data-joplin-lite-block-code")
                                .as_deref()
                                == Some("true"));
                    if projection.list_contexts.is_empty() && native_structural_text {
                        let blocks_before = projection.document.blocks.len();
                        let style = block_style(&attrs.borrow(), 0);
                        let kind = if tag == "blockquote" {
                            BlockKind::Quote
                        } else {
                            BlockKind::Code
                        };
                        projection.begin_block(kind, style);
                        pending.push(ProjectionFrame::FinishBlock {
                            blocks_before,
                            kind,
                            style,
                        });
                        for child in children.into_iter().rev() {
                            pending.push(ProjectionFrame::Visit {
                                node: child,
                                marks: marks.clone(),
                                preformatted: preformatted || tag == "pre",
                            });
                        }
                        continue;
                    }
                    if is_block_element(&tag)
                        || matches!(
                            tag.as_str(),
                            "div" | "section" | "article" | "header" | "footer"
                        )
                    {
                        if !projection.list_contexts.is_empty() {
                            let is_list_block = is_list_block_element(&tag);
                            let preserve_empty = is_list_semantic_block(&tag);
                            if is_list_block {
                                projection.begin_list_block();
                            }
                            projection.ensure_current();
                            if is_list_block {
                                pending.push(ProjectionFrame::FinishListBlock { preserve_empty });
                            }
                            let child_preformatted = preformatted || tag == "pre";
                            for child in children.into_iter().rev() {
                                pending.push(ProjectionFrame::Visit {
                                    node: child,
                                    marks: marks.clone(),
                                    preformatted: child_preformatted,
                                });
                            }
                            continue;
                        }
                        let blocks_before = projection.document.blocks.len();
                        let style =
                            block_style(&attrs.borrow(), if tag == "blockquote" { 1 } else { 0 });
                        projection.begin_block(BlockKind::Paragraph, style);
                        pending.push(ProjectionFrame::FinishBlock {
                            blocks_before,
                            kind: BlockKind::Paragraph,
                            style,
                        });
                        let child_preformatted = preformatted || tag == "pre";
                        for child in children.into_iter().rev() {
                            pending.push(ProjectionFrame::Visit {
                                node: child,
                                marks: marks.clone(),
                                preformatted: child_preformatted,
                            });
                        }
                        continue;
                    }
                    if tag == "br" {
                        if attribute(&attrs.borrow(), "data-joplin-lite-empty-item").as_deref()
                            == Some("true")
                        {
                            projection.mark_empty_item_placeholder();
                        } else {
                            if attribute(&attrs.borrow(), "data-joplin-lite-soft-break").as_deref()
                                == Some("true")
                            {
                                projection.mark_explicit_softbreak();
                            }
                            projection.ensure_current().push(Inline::SoftBreak);
                            projection.flow_has_visible = true;
                            projection.current_item_has_content = true;
                        }
                        continue;
                    }
                    if tag == "img" {
                        projection.image(&attrs.borrow(), &marks);
                        continue;
                    }
                    let next_marks = ProjectionMarks {
                        bold: marks.bold || matches!(tag.as_str(), "strong" | "b"),
                        italic: marks.italic || matches!(tag.as_str(), "em" | "i"),
                        underline: marks.underline || tag == "u",
                        strikethrough: marks.strikethrough
                            || matches!(tag.as_str(), "del" | "s" | "strike"),
                        highlight: marks.highlight || tag == "mark",
                        inline_code: marks.inline_code || tag == "code",
                        link: if marks.link.is_some() {
                            marks.link.clone()
                        } else if tag == "a" {
                            attribute(&attrs.borrow(), "href")
                                .filter(|value| valid_link(value))
                                .map(Rc::from)
                        } else {
                            None
                        },
                    };
                    for child in children.into_iter().rev() {
                        pending.push(ProjectionFrame::Visit {
                            node: child,
                            marks: next_marks.clone(),
                            preformatted,
                        });
                    }
                }
                DomData::CanonicalDocument
                | DomData::Doctype { .. }
                | DomData::Comment(_)
                | DomData::ProcessingInstruction { .. } => {
                    let children = node.children.borrow().clone();
                    for child in children.into_iter().rev() {
                        pending.push(ProjectionFrame::Visit {
                            node: child,
                            marks: marks.clone(),
                            preformatted,
                        });
                    }
                }
            },
        }
    }

    projection.finish()
}

enum ProjectionFrame {
    Visit {
        node: DomHandle,
        marks: ProjectionMarks,
        preformatted: bool,
    },
    FinishBlock {
        blocks_before: usize,
        kind: BlockKind,
        style: BlockStyle,
    },
    FinishList,
    FinishListItem,
    FinishListBlock {
        preserve_empty: bool,
    },
}

#[derive(Clone, Default)]
struct ProjectionMarks {
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
    highlight: bool,
    link: Option<Rc<str>>,
    inline_code: bool,
}

fn projection_marks_match(public: &Marks, projected: &ProjectionMarks) -> bool {
    public.bold == projected.bold
        && public.italic == projected.italic
        && public.underline == projected.underline
        && public.strikethrough == projected.strikethrough
        && public.highlight == projected.highlight
        && public.link.as_deref() == projected.link.as_deref()
        && public.inline_code == projected.inline_code
}

#[derive(Clone, Copy, Default)]
enum BlockKind {
    #[default]
    Paragraph,
    Heading(HeadingLevel),
    Quote,
    Code,
}

struct ListContext {
    kind: ListKind,
    items: Vec<ListItem>,
    pending_style: BlockStyle,
    pending_checked: Option<bool>,
    split_from_nested: bool,
}

#[derive(Default)]
struct Projection {
    document: CanonicalDocument,
    current: Option<Vec<Inline>>,
    current_style: BlockStyle,
    current_kind: BlockKind,
    list_contexts: Vec<ListContext>,
    current_item_has_content: bool,
    retained_link_bytes: usize,
    explicit_softbreak: bool,
    pending_space: bool,
    pending_marks: Option<ProjectionMarks>,
    flow_has_visible: bool,
}

impl Projection {
    fn finish(mut self) -> CanonicalDocument {
        while !self.list_contexts.is_empty() {
            self.finish_list();
        }
        self.flush();
        CanonicalDocument::from_blocks(self.document.blocks)
    }

    fn ensure_current(&mut self) -> &mut Vec<Inline> {
        if self.current.is_none() {
            let pending = self
                .list_contexts
                .last()
                .map(|context| (context.pending_style, context.pending_checked));
            if let Some((style, checked)) = pending {
                self.begin_list_item(style, checked);
            }
        }
        self.current.get_or_insert_with(Vec::new)
    }

    fn flush(&mut self) {
        if !self.list_contexts.is_empty() {
            return;
        }
        self.pending_space = false;
        self.pending_marks = None;
        self.flow_has_visible = false;
        if let Some(inlines) = self.current.take()
            && !inlines.is_empty()
        {
            self.document.blocks.push(Block::Paragraph {
                style: self.current_style,
                inlines,
            });
        }
    }

    fn begin_block(&mut self, kind: BlockKind, style: BlockStyle) {
        self.flush();
        self.current = Some(Vec::new());
        self.current_kind = kind;
        self.current_style = style;
        self.explicit_softbreak = false;
    }

    fn finish_block(&mut self, blocks_before: usize, kind: BlockKind, style: BlockStyle) {
        let Some(inlines) = self.current.take() else {
            self.explicit_softbreak = false;
            self.pending_space = false;
            self.pending_marks = None;
            self.flow_has_visible = false;
            if self.document.blocks.len() == blocks_before {
                self.push_block(kind, style, Vec::new());
            }
            return;
        };
        let inlines = normalize_inlines(inlines);
        if inlines.len() == 1 && matches!(inlines[0], Inline::SoftBreak) && !self.explicit_softbreak
        {
            self.push_block(kind, style, Vec::new());
        } else if !inlines.is_empty() {
            self.push_block(kind, style, inlines);
        } else if self.document.blocks.len() == blocks_before {
            self.push_block(kind, style, Vec::new());
        }
        self.explicit_softbreak = false;
        self.pending_space = false;
        self.pending_marks = None;
        self.flow_has_visible = false;
    }

    fn push_block(&mut self, kind: BlockKind, style: BlockStyle, inlines: Vec<Inline>) {
        self.document.blocks.push(match kind {
            BlockKind::Paragraph => Block::Paragraph { style, inlines },
            BlockKind::Heading(level) => Block::Heading {
                level,
                style,
                inlines,
            },
            BlockKind::Quote => Block::Quote { style, inlines },
            BlockKind::Code => Block::Code { style, inlines },
        });
    }

    fn begin_list(&mut self, kind: ListKind) {
        if !self.list_contexts.is_empty() {
            self.finish_item_before_nested_list();
            self.flush_list_segment();
        } else {
            self.flush();
        }
        self.list_contexts.push(ListContext {
            kind,
            items: Vec::new(),
            pending_style: BlockStyle::default(),
            pending_checked: (kind == ListKind::Checklist).then_some(false),
            split_from_nested: false,
        });
        self.current = None;
        self.current_item_has_content = false;
    }

    fn begin_list_item(&mut self, style: BlockStyle, checked: Option<bool>) {
        if self.current.is_some() {
            self.finish_list_item();
        }
        if let Some(context) = self.list_contexts.last_mut() {
            context.pending_style = style;
            context.pending_checked = checked;
        }
        self.current = Some(Vec::new());
        self.current_item_has_content = false;
        self.explicit_softbreak = false;
        self.pending_space = false;
        self.pending_marks = None;
        self.flow_has_visible = false;
    }

    fn flush_list_segment(&mut self) {
        if let Some(context) = self.list_contexts.last_mut() {
            context.split_from_nested = true;
            let items = std::mem::take(&mut context.items);
            if !items.is_empty() {
                self.document.blocks.push(Block::List {
                    kind: context.kind,
                    items,
                });
            }
        }
        self.current = None;
        self.current_item_has_content = false;
    }

    fn finish_list_item(&mut self) {
        let Some(inlines) = self.current.take() else {
            return;
        };
        let Some(context) = self.list_contexts.last_mut() else {
            return;
        };
        let inlines = normalize_inlines(inlines);
        context.items.push(ListItem {
            checked: context.pending_checked,
            style: context.pending_style,
            inlines,
        });
        self.pending_space = false;
        self.pending_marks = None;
        self.flow_has_visible = false;
        self.explicit_softbreak = false;
        self.current_item_has_content = false;
    }

    fn finish_list_block(&mut self, preserve_empty: bool) {
        if self.current.is_some() {
            if !self.current_item_has_content && !preserve_empty {
                self.discard_current_list_item();
            } else {
                self.finish_list_item();
            }
        }
    }

    fn discard_current_list_item(&mut self) {
        if self.current.is_some() {
            self.current = None;
            self.pending_space = false;
            self.pending_marks = None;
            self.flow_has_visible = false;
            self.explicit_softbreak = false;
            self.current_item_has_content = false;
        }
    }

    fn finish_list(&mut self) {
        if self.current.is_some() {
            self.finish_list_item();
        }
        if let Some(context) = self.list_contexts.pop()
            && (!context.items.is_empty() || !context.split_from_nested)
        {
            self.document.blocks.push(Block::List {
                kind: context.kind,
                items: context.items,
            });
        }
        self.current = None;
        self.current_item_has_content = false;
        self.pending_space = false;
        self.pending_marks = None;
        self.flow_has_visible = false;
    }

    fn finish_item_before_nested_list(&mut self) {
        if !self.current_item_has_content {
            self.current = None;
            self.pending_space = false;
            self.pending_marks = None;
            self.flow_has_visible = false;
            self.explicit_softbreak = false;
            self.current_item_has_content = false;
        } else {
            self.finish_list_item();
        }
    }

    fn begin_list_block(&mut self) {
        if self.current.is_some() {
            if self.current_item_has_content {
                self.finish_list_item();
            } else {
                self.current = None;
                self.pending_space = false;
                self.pending_marks = None;
                self.flow_has_visible = false;
                self.explicit_softbreak = false;
                self.current_item_has_content = false;
            }
        }
    }

    fn mark_explicit_softbreak(&mut self) {
        self.explicit_softbreak = true;
    }

    fn mark_empty_item_placeholder(&mut self) {
        self.ensure_current();
    }

    fn text(&mut self, text: &str, marks: &ProjectionMarks, preformatted: bool) {
        if text.is_empty() {
            return;
        }
        if !preformatted && is_formatting_whitespace(text) {
            if self.flow_has_visible {
                self.pending_space = true;
                self.pending_marks = Some(marks.clone());
            }
            return;
        }
        let mut normalized = String::new();
        for character in text.chars() {
            match character {
                '\r' => normalized.push('\n'),
                '\t' => normalized.push_str("    "),
                _ => normalized.push(character),
            }
        }
        let mut pieces = normalized.split('\n').peekable();
        while let Some(piece) = pieces.next() {
            if !piece.is_empty() {
                self.flush_pending_space();
                self.push_text(piece, marks);
            }
            if pieces.peek().is_some() {
                self.ensure_current().push(Inline::SoftBreak);
                self.flow_has_visible = true;
                self.current_item_has_content = true;
            }
        }
    }

    fn flush_pending_space(&mut self) {
        if !self.pending_space || !self.flow_has_visible {
            self.pending_space = false;
            self.pending_marks = None;
            return;
        }
        let marks = self.pending_marks.take().unwrap_or_default();
        self.pending_space = false;
        self.push_text(" ", &marks);
    }

    fn push_text(&mut self, text: &str, marks: &ProjectionMarks) {
        if let Some(Inline::Text {
            text: previous,
            marks: previous_marks,
        }) = self.current.as_mut().and_then(|inlines| inlines.last_mut())
            && projection_marks_match(previous_marks, marks)
        {
            previous.push_str(text);
        } else {
            let public_marks = self.materialize_marks(marks);
            self.ensure_current().push(Inline::Text {
                text: text.to_owned(),
                marks: public_marks,
            });
        }
        self.flow_has_visible = true;
        self.current_item_has_content = true;
    }

    fn materialize_marks(&mut self, projected: &ProjectionMarks) -> Marks {
        let link = projected.link.as_ref().and_then(|link| {
            let length = link.len();
            let within_budget = self
                .retained_link_bytes
                .checked_add(length)
                .is_some_and(|total| total <= MAX_RETAINED_LINK_BYTES);
            if within_budget {
                self.retained_link_bytes += length;
                Some(link.to_string())
            } else {
                None
            }
        });
        Marks {
            bold: projected.bold,
            italic: projected.italic,
            underline: projected.underline,
            strikethrough: projected.strikethrough,
            highlight: projected.highlight,
            link,
            inline_code: projected.inline_code,
        }
    }

    fn image(&mut self, attrs: &[Attribute], marks: &ProjectionMarks) {
        let source = attribute(attrs, "src");
        let alt = attribute(attrs, "alt").unwrap_or_default();
        let Some(source) = source else {
            self.text(&alt, marks, true);
            return;
        };
        let Some(resource_id) = source.strip_prefix(":/") else {
            self.text(&alt, marks, true);
            return;
        };
        let Ok(resource_id) = ResourceId::new(resource_id) else {
            self.text(&alt, marks, true);
            return;
        };
        self.flush_pending_space();
        self.ensure_current()
            .push(Inline::Image { resource_id, alt });
        self.flow_has_visible = true;
        self.current_item_has_content = true;
    }

    fn block_image(&mut self, attrs: &[Attribute]) {
        let source = attribute(attrs, "src");
        let alt = attribute(attrs, "alt").unwrap_or_default();
        let Some(resource_id) =
            source.and_then(|source| source.strip_prefix(":/").map(str::to_owned))
        else {
            return;
        };
        let Ok(resource_id) = ResourceId::new(resource_id) else {
            return;
        };
        self.flush();
        self.document.blocks.push(Block::Image { resource_id, alt });
        self.pending_space = false;
        self.pending_marks = None;
        self.flow_has_visible = false;
    }

    fn block_attachment(&mut self, attrs: &[Attribute]) {
        let raw_resource_id = attribute(attrs, "data-resource-id").or_else(|| {
            attribute(attrs, "href").and_then(|href| href.strip_prefix(":/").map(str::to_owned))
        });
        let Some(raw_resource_id) = raw_resource_id else {
            return;
        };
        let Ok(resource_id) = ResourceId::new(raw_resource_id) else {
            return;
        };
        let Some(filename) = attribute(attrs, "data-filename") else {
            return;
        };
        let Some(media_type) = attribute(attrs, "data-media-type") else {
            return;
        };
        self.flush();
        self.document.blocks.push(Block::Attachment {
            resource_id,
            filename,
            media_type,
        });
        self.pending_space = false;
        self.pending_marks = None;
        self.flow_has_visible = false;
    }

    fn block_divider(&mut self) {
        self.flush();
        self.document.blocks.push(Block::Divider);
        self.pending_space = false;
        self.pending_marks = None;
        self.flow_has_visible = false;
    }
}

fn attribute(attrs: &[Attribute], name: &str) -> Option<String> {
    attrs
        .iter()
        .find(|attribute| attribute.name.local.to_string().eq_ignore_ascii_case(name))
        .map(|attribute| attribute.value.to_string())
}

fn valid_link(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_LINK_LENGTH || value.chars().any(char::is_control) {
        return false;
    }
    if let Some(target) =
        strip_ascii_prefix(value, "http://").or_else(|| strip_ascii_prefix(value, "https://"))
    {
        return valid_http_target(target);
    }
    strip_ascii_prefix(value, "mailto:").is_some_and(valid_mailto_target)
}

fn strip_ascii_prefix<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &value[prefix.len()..])
}

fn valid_http_target(target: &str) -> bool {
    let authority = target.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.chars().any(char::is_whitespace) {
        return false;
    }
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if host_port.is_empty() {
        return false;
    }
    if host_port.starts_with('[') {
        let Some(close) = host_port.find(']') else {
            return false;
        };
        if close == 1 {
            return false;
        }
        let remainder = &host_port[close + 1..];
        remainder.is_empty()
            || remainder
                .strip_prefix(':')
                .is_some_and(|port| !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()))
    } else {
        let (host, port) = host_port
            .split_once(':')
            .map_or((host_port, None), |(host, port)| (host, Some(port)));
        !host.is_empty()
            && port.is_none_or(|port| !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()))
    }
}

fn valid_mailto_target(target: &str) -> bool {
    let mailbox = target.split(['?', '#']).next().unwrap_or_default();
    let Some((local, domain)) = mailbox.rsplit_once('@') else {
        return false;
    };
    !local.is_empty() && !domain.is_empty() && !mailbox.chars().any(char::is_whitespace)
}

fn block_style(attrs: &[Attribute], default_indent: u8) -> BlockStyle {
    let alignment = match attribute(attrs, "data-align").as_deref() {
        Some("center") => Alignment::Center,
        Some("right") => Alignment::Right,
        _ => Alignment::Left,
    };
    let indent = attribute(attrs, "data-indent")
        .and_then(|value| value.parse::<u8>().ok())
        .unwrap_or(default_indent)
        .min(8);
    BlockStyle { alignment, indent }
}

fn heading_level(tag: &str) -> Option<HeadingLevel> {
    match tag {
        "h1" => Some(HeadingLevel::One),
        "h2" => Some(HeadingLevel::Two),
        "h3" => Some(HeadingLevel::Three),
        _ => None,
    }
}

fn is_block_element(name: &str) -> bool {
    matches!(
        name,
        "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "li" | "pre" | "blockquote"
    )
}

fn is_list_block_element(name: &str) -> bool {
    matches!(
        name,
        "p" | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "pre"
            | "blockquote"
            | "div"
            | "section"
            | "article"
            | "header"
            | "footer"
    )
}

fn is_list_semantic_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "pre" | "blockquote"
    )
}

fn is_formatting_whitespace(text: &str) -> bool {
    text.chars()
        .all(|character| matches!(character, ' ' | '\t' | '\r' | '\n'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESOURCE_ID: &str = "0123456789abcdef0123456789abcdef";
    const SECOND_RESOURCE_ID: &str = "fedcba9876543210fedcba9876543210";

    fn paragraph(inlines: Vec<Inline>) -> Block {
        Block::Paragraph {
            style: BlockStyle::default(),
            inlines,
        }
    }

    #[test]
    fn semantic_blocks_and_marks_have_canonical_html() {
        let document = CanonicalDocument::from_blocks(vec![
            Block::Heading {
                level: HeadingLevel::Two,
                style: BlockStyle {
                    alignment: Alignment::Center,
                    indent: 2,
                },
                inlines: vec![Inline::Text {
                    text: "标题".into(),
                    marks: Marks {
                        strikethrough: true,
                        highlight: true,
                        link: Some("https://example.com/a".into()),
                        ..Marks::default()
                    },
                }],
            },
            Block::List {
                kind: ListKind::Checklist,
                items: vec![
                    ListItem {
                        checked: Some(false),
                        style: BlockStyle::default(),
                        inlines: vec![Inline::Text {
                            text: "待办".into(),
                            marks: Marks::default(),
                        }],
                    },
                    ListItem {
                        checked: Some(true),
                        style: BlockStyle::default(),
                        inlines: vec![Inline::Text {
                            text: "完成".into(),
                            marks: Marks::default(),
                        }],
                    },
                ],
            },
        ]);
        let html = serialize_html(&document);
        assert_eq!(
            html,
            "<h2 data-align=\"center\" data-indent=\"2\"><a href=\"https://example.com/a\"><mark><s>标题</s></mark></a></h2><ul data-type=\"checklist\"><li data-checked=\"false\">待办</li><li data-checked=\"true\">完成</li></ul>"
        );
        assert_eq!(serialize_html(&parse_html(&html).unwrap()), html);
        assert_eq!(search_text(&parse_html(&html).unwrap()), "标题\n待办\n完成");
    }

    #[test]
    fn explicit_native_quote_and_code_markers_round_trip_without_reclassifying_legacy_html() {
        let document = CanonicalDocument::from_blocks(vec![
            Block::Quote {
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "引用".into(),
                    marks: Marks::default(),
                }],
            },
            Block::Code {
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "let x = 1;".into(),
                    marks: Marks {
                        inline_code: true,
                        ..Marks::default()
                    },
                }],
            },
        ]);
        let html = document.to_canonical_html();
        assert!(
            html.as_str()
                .contains("data-joplin-lite-block-quote=\"true\"")
        );
        assert!(
            html.as_str()
                .contains("data-joplin-lite-block-code=\"true\"")
        );
        assert_eq!(parse_html(html.as_str()).unwrap(), document);

        // Imported legacy HTML retains the long-standing paragraph fallback;
        // it never gains an editor-only block kind merely by being opened.
        assert!(matches!(
            parse_html("<blockquote>legacy</blockquote>")
                .unwrap()
                .blocks
                .as_slice(),
            [Block::Paragraph { .. }]
        ));
    }

    #[test]
    fn nested_lists_flatten_without_losing_order_or_resources() {
        let html = format!(
            "<ul><li>keep</li><li>outer<img src=\":/{RESOURCE_ID}\" alt=\"outer-image\"><ol><li><p>inner-before</p><h2>inner-after<img src=\":/{RESOURCE_ID}\" alt=\"inner-image\"></h2></li></ol>outer-after</li></ul>"
        );
        let document = parse_html(&html).unwrap();

        assert_eq!(
            search_text(&document),
            "keep\nouterouter-image\ninner-before\ninner-afterinner-image\nouter-after"
        );
        assert_eq!(
            resource_ids(&document),
            vec![
                ResourceId::new(RESOURCE_ID).unwrap(),
                ResourceId::new(RESOURCE_ID).unwrap()
            ]
        );
        assert!(matches!(
            document.blocks.as_slice(),
            [
                Block::List {
                    kind: ListKind::Unordered,
                    ..
                },
                Block::List {
                    kind: ListKind::Ordered,
                    ..
                },
                Block::List {
                    kind: ListKind::Unordered,
                    ..
                }
            ]
        ));
    }

    #[test]
    fn nested_list_without_outer_after_does_not_create_an_empty_item() {
        let document = parse_html("<ul><li>outer<ol><li>inner</li></ol></li></ul>").unwrap();
        assert_eq!(search_text(&document), "outer\ninner");
        assert!(matches!(
            document.blocks.as_slice(),
            [
                Block::List {
                    items,
                    ..
                },
                Block::List {
                    items: inner_items,
                    ..
                }
            ] if items.len() == 1 && inner_items.len() == 1
        ));
        assert_eq!(
            serialize_html(&document),
            "<ul><li>outer</li></ul><ol><li>inner</li></ol>"
        );
    }

    #[test]
    fn nested_list_resource_order_preserves_a_b_a_occurrences() {
        let document = parse_html(&format!(
            "<ul><li>before<img src=\":/{RESOURCE_ID}\" alt=\"a\"><ol><li>inner<img src=\":/{SECOND_RESOURCE_ID}\" alt=\"b\"></li></ol>after<img src=\":/{RESOURCE_ID}\" alt=\"a-again\"></li></ul>"
        ))
        .unwrap();
        assert_eq!(
            resource_ids(&document),
            vec![
                ResourceId::new(RESOURCE_ID).unwrap(),
                ResourceId::new(SECOND_RESOURCE_ID).unwrap(),
                ResourceId::new(RESOURCE_ID).unwrap()
            ]
        );
        assert_eq!(search_text(&document), "beforea\ninnerb\naftera-again");
    }

    #[test]
    fn list_block_end_separates_following_inline_content() {
        let document = parse_html("<ul><li><p>one</p>two</li></ul>").unwrap();
        assert_eq!(search_text(&document), "one\ntwo");
        let Block::List { items, .. } = &document.blocks[0] else {
            panic!("expected list");
        };
        assert_eq!(items.len(), 2);
        assert_eq!(
            serialize_html(&document),
            "<ul><li>one</li><li>two</li></ul>"
        );

        let empty_child = parse_html("<ul><li><p></p><h2>two</h2></li></ul>").unwrap();
        assert_eq!(search_text(&empty_child), "\ntwo");
        assert_eq!(
            serialize_html(&empty_child),
            "<ul><li><br data-joplin-lite-empty-item=\"true\"></li><li>two</li></ul>"
        );
    }

    #[test]
    fn softbreaks_before_blocks_and_nested_lists_are_not_dropped() {
        let explicit = parse_html(
            "<ul><li><br data-joplin-lite-soft-break=\"true\"><ol><li>inner</li></ol></li></ul>",
        )
        .unwrap();
        assert_eq!(search_text(&explicit), "\n\ninner");

        let consecutive = parse_html("<ul><li><br><br><p>two</p></li></ul>").unwrap();
        assert_eq!(search_text(&consecutive), "\n\n\ntwo");
        assert_eq!(
            serialize_html(&consecutive),
            "<ul><li><br><br></li><li>two</li></ul>"
        );
    }

    #[test]
    fn list_block_wrappers_preserve_boundaries_without_wrapper_empty_items() {
        let inline_wrapper = parse_html("<ul><li>before<div>inside</div>after</li></ul>").unwrap();
        assert_eq!(search_text(&inline_wrapper), "before\ninside\nafter");
        let Block::List {
            items: inline_items,
            ..
        } = &inline_wrapper.blocks[0]
        else {
            panic!("expected list");
        };
        assert_eq!(inline_items.len(), 3);

        let wrapped_blocks =
            parse_html("<ul><li><div><p>one</p><h4>two</h4></div></li></ul>").unwrap();
        assert_eq!(search_text(&wrapped_blocks), "one\ntwo");
        let Block::List {
            items: wrapped_items,
            ..
        } = &wrapped_blocks.blocks[0]
        else {
            panic!("expected list");
        };
        assert_eq!(wrapped_items.len(), 2);

        let empty_wrapper = parse_html("<ul><li><div></div>after</li></ul>").unwrap();
        assert_eq!(search_text(&empty_wrapper), "after");
        let Block::List {
            items: empty_items, ..
        } = &empty_wrapper.blocks[0]
        else {
            panic!("expected list");
        };
        assert_eq!(empty_items.len(), 1);
    }

    #[test]
    fn adjacent_wrappers_keep_marks_images_and_resource_order() {
        let document = parse_html(&format!(
            "<ul><li><section><strong>one</strong><img src=\":/{RESOURCE_ID}\" alt=\"a\"></section><article><em>two</em><img src=\":/{SECOND_RESOURCE_ID}\" alt=\"b\"></article></li></ul>"
        ))
        .unwrap();
        assert_eq!(search_text(&document), "onea\ntwob");
        assert_eq!(
            resource_ids(&document),
            vec![
                ResourceId::new(RESOURCE_ID).unwrap(),
                ResourceId::new(SECOND_RESOURCE_ID).unwrap()
            ]
        );
        let Block::List { items, .. } = &document.blocks[0] else {
            panic!("expected list");
        };
        assert_eq!(items.len(), 2);
        assert!(matches!(
            &items[0].inlines[0],
            Inline::Text { marks, .. } if marks.bold
        ));
        assert!(matches!(
            &items[1].inlines[0],
            Inline::Text { marks, .. } if marks.italic
        ));
    }

    #[test]
    fn nested_list_after_item_keeps_outer_style_and_check_state() {
        let document = parse_html(
            "<ul data-type=\"checklist\"><li data-checked=\"true\" data-align=\"right\" data-indent=\"2\">outer<ol><li>inner</li></ol>after</li></ul>",
        )
        .unwrap();
        let Block::List { items, .. } = &document.blocks[0] else {
            panic!("expected outer list segment");
        };
        let Block::List {
            items: after_items, ..
        } = &document.blocks[2]
        else {
            panic!("expected resumed outer list segment");
        };
        assert_eq!(items[0].checked, Some(true));
        assert_eq!(items[0].style.indent, 2);
        assert_eq!(items[0].style.alignment, Alignment::Right);
        assert_eq!(after_items[0].checked, Some(true));
        assert_eq!(after_items[0].style, items[0].style);
        assert_eq!(search_text(&document), "outer\ninner\nafter");
    }

    #[test]
    fn long_links_are_bounded_before_marks_are_projected_to_children() {
        let href = format!(
            "https://example.com/{}",
            "x".repeat(MAX_LINK_LENGTH.saturating_add(1))
        );
        let html = format!(
            "<p><a href=\"{href}\">{}</a></p>",
            "<span>x</span>".repeat(1_000)
        );
        let document = parse_html(&html).unwrap();
        assert_eq!(search_text(&document), "x".repeat(1_000));
        assert!(!serialize_html(&document).contains("<a href="));
    }

    #[test]
    fn retained_link_budget_prevents_per_run_href_amplification() {
        let href = format!(
            "https://example.com/{}",
            "x".repeat(MAX_LINK_LENGTH.saturating_sub(20))
        );
        let html = format!(
            "<p><a href=\"{href}\">{}</a></p>",
            "<b>x</b><i>x</i>".repeat(1_000)
        );
        let document = parse_html(&html).unwrap();
        let canonical = serialize_html(&document);
        assert_eq!(search_text(&document), "xx".repeat(1_000));
        assert!(canonical.matches(" href=\"").count() < 32);
    }

    #[test]
    fn styled_empty_paragraphs_round_trip_without_being_collapsed() {
        let document = CanonicalDocument::from_blocks(vec![
            paragraph(Vec::new()),
            Block::Paragraph {
                style: BlockStyle {
                    alignment: Alignment::Center,
                    indent: 2,
                },
                inlines: Vec::new(),
            },
            Block::Paragraph {
                style: BlockStyle {
                    alignment: Alignment::Right,
                    indent: 1,
                },
                inlines: Vec::new(),
            },
        ]);
        let html = serialize_html(&document);
        assert_eq!(
            html,
            "<p><br></p><p data-align=\"center\" data-indent=\"2\"><br></p><p data-align=\"right\" data-indent=\"1\"><br></p>"
        );
        assert_eq!(parse_html(&html).unwrap(), document);
    }

    #[test]
    fn links_require_a_structurally_valid_absolute_target() {
        let document = parse_html(
            r#"<p><a href="http://example.com/path">http</a><a href="https://localhost:8443">https</a><a href="mailto:user@example.com">mail</a><a href="http://">no-host</a><a href="https://?q=1">no-host</a><a href="https://:443">no-host</a><a href="mailto:@example.com">no-mailbox</a><a href="mailto:user">no-domain</a></p>"#,
        )
        .unwrap();
        assert_eq!(
            serialize_html(&document),
            "<p><a href=\"http://example.com/path\">http</a><a href=\"https://localhost:8443\">https</a><a href=\"mailto:user@example.com\">mail</a>no-hostno-hostno-hostno-mailboxno-domain</p>"
        );
    }

    #[test]
    fn unsafe_links_and_css_are_visible_or_discarded() {
        let document = parse_html(
            r#"<p style="color:red" onclick="alert(1)"><a href="javascript:alert(1)">危险</a><a href="mailto:a@example.com">安全</a><a href="http://safe
bad">控制字符</a><a href="//relative">相对路径</a></p>"#,
        )
        .unwrap();
        assert_eq!(search_text(&document), "危险安全控制字符相对路径");
        assert_eq!(
            serialize_html(&document),
            "<p>危险<a href=\"mailto:a@example.com\">安全</a>控制字符相对路径</p>"
        );
    }

    #[test]
    fn escapes_unicode_text_and_preserves_significant_whitespace() {
        let document = CanonicalDocument::from_blocks(vec![paragraph(vec![Inline::Text {
            text: "  中文 😀 & < > \" '  ".into(),
            marks: Marks::default(),
        }])]);
        let html = serialize_html(&document);
        assert_eq!(
            html,
            "<p>&nbsp;&nbsp;中文 😀 &amp; &lt; &gt; &quot; &#39;&nbsp;&nbsp;</p>"
        );
        assert_eq!(parse_html(&html).unwrap(), document);
    }

    #[test]
    fn paragraphs_marks_breaks_and_images_round_trip_in_order() {
        let document = CanonicalDocument::from_blocks(vec![
            paragraph(vec![
                Inline::Text {
                    text: "前".into(),
                    marks: Marks {
                        bold: true,
                        italic: false,
                        underline: false,
                        ..Marks::default()
                    },
                },
                Inline::SoftBreak,
                Inline::Image {
                    resource_id: ResourceId::new(RESOURCE_ID).unwrap(),
                    alt: "截图 & 证据.png".into(),
                },
                Inline::Text {
                    text: "后".into(),
                    marks: Marks {
                        bold: false,
                        italic: true,
                        underline: true,
                        ..Marks::default()
                    },
                },
            ]),
            paragraph(Vec::new()),
        ]);
        let html = serialize_html(&document);
        assert_eq!(
            html,
            "<p><strong>前</strong><br><img src=\":/0123456789abcdef0123456789abcdef\" alt=\"截图 &amp; 证据.png\"><em><u>后</u></em></p><p><br></p>"
        );
        assert_eq!(parse_html(&html).unwrap(), document);
    }

    #[test]
    fn identical_documents_have_identical_html_bytes() {
        let document = CanonicalDocument::from_blocks(vec![paragraph(vec![Inline::Text {
            text: "稳定输出".into(),
            marks: Marks {
                bold: true,
                italic: true,
                underline: true,
                ..Marks::default()
            },
        }])]);
        assert_eq!(serialize_html(&document), serialize_html(&document));
        assert_eq!(
            serialize_html(&parse_html(&serialize_html(&document)).unwrap()),
            serialize_html(&document)
        );
    }

    #[test]
    fn only_local_valid_image_ids_survive_as_resource_nodes() {
        let html = format!(
            "<p><img src=\":/{RESOURCE_ID}\" alt=\"ok\"><img src=\"data:image/png;base64,AAAA\" alt=\"data\"><img src=\"https://example.com/a.png\" alt=\"remote\"><img src=\":/NOT-VALID\" alt=\"invalid\"></p>"
        );
        let document = parse_html(&html).unwrap();
        assert_eq!(
            resource_ids(&document),
            vec![ResourceId::new(RESOURCE_ID).unwrap()]
        );
        assert_eq!(
            document,
            CanonicalDocument::from_blocks(vec![paragraph(vec![
                Inline::Image {
                    resource_id: ResourceId::new(RESOURCE_ID).unwrap(),
                    alt: "ok".into(),
                },
                Inline::Text {
                    text: "dataremoteinvalid".into(),
                    marks: Marks::default(),
                },
            ])])
        );
    }

    #[test]
    fn unsafe_elements_attributes_and_urls_are_downgraded_to_visible_text() {
        let document = parse_html(
            r#"<p onclick="alert(1)"><script>secret()</script><style>.x{display:none}</style><mark>visible</mark><a href="javascript:alert(1)">link</a></p>"#,
        )
        .unwrap();
        assert_eq!(search_text(&document), "visiblelink");
        assert!(!serialize_html(&document).contains("script"));
        assert!(!serialize_html(&document).contains("onclick"));
        assert!(!serialize_html(&document).contains("javascript:"));
    }

    #[test]
    fn search_projection_contains_visible_text_and_image_alt_but_not_markup() {
        let document = parse_html(&format!(
            "<p>正文 <strong>重点</strong><img src=\":/{RESOURCE_ID}\" alt=\"付款截图.png\"></p>"
        ))
        .unwrap();
        assert_eq!(search_text(&document), "正文 重点付款截图.png");
        assert!(!search_text(&document).contains(RESOURCE_ID));
        assert!(!search_text(&document).contains("strong"));
    }

    #[test]
    fn empty_semantic_content_serializes_to_empty_html() {
        assert_eq!(serialize_html(&CanonicalDocument::default()), "");
        assert_eq!(serialize_html(&CanonicalDocument::from_blocks(vec![])), "");
        assert_eq!(
            serialize_html(&CanonicalDocument::from_blocks(vec![paragraph(vec![])])),
            ""
        );
    }

    #[test]
    fn empty_semantic_blocks_and_items_remain_reversible() {
        let document = CanonicalDocument::from_blocks(vec![
            Block::Heading {
                level: HeadingLevel::One,
                style: BlockStyle::default(),
                inlines: Vec::new(),
            },
            Block::List {
                kind: ListKind::Unordered,
                items: vec![ListItem {
                    checked: None,
                    style: BlockStyle::default(),
                    inlines: Vec::new(),
                }],
            },
        ]);
        let html = serialize_html(&document);
        assert_eq!(
            html,
            "<h1><br></h1><ul><li><br data-joplin-lite-empty-item=\"true\"></li></ul>"
        );
        assert_eq!(parse_html(&html).unwrap(), document);

        let empty_list = CanonicalDocument::from_blocks(vec![Block::List {
            kind: ListKind::Ordered,
            items: Vec::new(),
        }]);
        assert_eq!(serialize_html(&empty_list), "<ol></ol>");
        assert_eq!(parse_html("<ol></ol>").unwrap(), empty_list);
    }

    #[test]
    fn all_list_kinds_and_item_styles_have_stable_projection() {
        let document = CanonicalDocument::from_blocks(vec![
            Block::List {
                kind: ListKind::Unordered,
                items: vec![ListItem {
                    checked: Some(true),
                    style: BlockStyle {
                        alignment: Alignment::Right,
                        indent: 9,
                    },
                    inlines: vec![Inline::Text {
                        text: "bullet".into(),
                        marks: Marks::default(),
                    }],
                }],
            },
            Block::List {
                kind: ListKind::Ordered,
                items: vec![ListItem {
                    checked: Some(false),
                    style: BlockStyle::default(),
                    inlines: vec![Inline::Text {
                        text: "number".into(),
                        marks: Marks::default(),
                    }],
                }],
            },
        ]);
        let html = serialize_html(&document);
        assert_eq!(
            html,
            "<ul><li data-align=\"right\" data-indent=\"8\">bullet</li></ul><ol><li>number</li></ol>"
        );
        assert_eq!(serialize_html(&parse_html(&html).unwrap()), html);
        assert_eq!(search_text(&parse_html(&html).unwrap()), "bullet\nnumber");
    }

    #[test]
    fn raw_text_elements_are_not_projected_as_markup() {
        let textarea = parse_html(&format!(
            r#"<textarea>before<img src=":/{RESOURCE_ID}" alt="fake">after</textarea>"#
        ))
        .unwrap();
        assert_eq!(
            search_text(&textarea),
            "before<img src=\":/0123456789abcdef0123456789abcdef\" alt=\"fake\">after"
        );
        assert!(resource_ids(&textarea).is_empty());

        let script = parse_html(
            r#"<p><strong>before<script>const x = '</strong>';</script>after</strong></p>"#,
        )
        .unwrap();
        assert_eq!(search_text(&script), "beforeafter");
        assert_eq!(
            script.blocks,
            vec![paragraph(vec![Inline::Text {
                text: "beforeafter".into(),
                marks: Marks {
                    bold: true,
                    ..Marks::default()
                }
            }])]
        );

        let style =
            parse_html(r#"<p>before<style>.x:after {content: '</p>';}</style>after</p>"#).unwrap();
        assert_eq!(style.blocks.len(), 1);
        assert_eq!(search_text(&style), "beforeafter");
    }

    #[test]
    fn html5_tree_builder_handles_malformed_blocks_and_marks() {
        let document = parse_html("<p><p>text</p>").unwrap();
        assert_eq!(
            document,
            CanonicalDocument::from_blocks(vec![
                paragraph(Vec::new()),
                paragraph(vec![Inline::Text {
                    text: "text".into(),
                    marks: Marks::default(),
                }]),
            ])
        );

        let nested = parse_html("<p><strong>one<p>two</strong>three").unwrap();
        assert_eq!(search_text(&nested), "one\ntwothree");
        assert_eq!(nested.blocks.len(), 2);
        assert!(matches!(
            nested.blocks[0],
            Block::Paragraph { ref inlines, .. }
                if matches!(&inlines[0], Inline::Text { marks, .. } if marks.bold)
        ));
        assert!(matches!(
            nested.blocks[1],
            Block::Paragraph { ref inlines, .. }
                if matches!(&inlines[0], Inline::Text { text, marks } if text == "two" && marks.bold)
                    && matches!(&inlines[1], Inline::Text { text, marks } if text == "three" && !marks.bold)
        ));
    }

    #[test]
    fn nbsp_tabs_and_newlines_have_deterministic_visible_model_forms() {
        let document = parse_html("<p>&nbsp;a\tb\r\nc&nbsp;</p>").unwrap();
        assert_eq!(
            document,
            CanonicalDocument::from_blocks(vec![paragraph(vec![
                Inline::Text {
                    text: "\u{00a0}a    b".into(),
                    marks: Marks::default(),
                },
                Inline::SoftBreak,
                Inline::Text {
                    text: "c\u{00a0}".into(),
                    marks: Marks::default(),
                },
            ])])
        );
        assert_eq!(
            serialize_html(&document),
            "<p>&nbsp;a&nbsp;&nbsp;&nbsp;&nbsp;b<br>c&nbsp;</p>"
        );
        assert_eq!(
            parse_html("&nbsp;").map(|doc| search_text(&doc)).unwrap(),
            "\u{00a0}"
        );
    }

    #[test]
    fn preformatted_whitespace_is_downgraded_to_the_same_canonical_model() {
        let document = parse_html("<pre>  first\n  second</pre>").unwrap();
        assert_eq!(
            document,
            CanonicalDocument::from_blocks(vec![paragraph(vec![
                Inline::Text {
                    text: "  first".into(),
                    marks: Marks::default(),
                },
                Inline::SoftBreak,
                Inline::Text {
                    text: "  second".into(),
                    marks: Marks::default(),
                },
            ])])
        );
        assert_eq!(
            serialize_html(&document),
            "<p>&nbsp;&nbsp;first<br>&nbsp;&nbsp;second</p>"
        );
    }

    #[test]
    fn ordinary_spaces_remain_natural_breaks_across_mark_runs() {
        let document = CanonicalDocument::from_blocks(vec![paragraph(vec![
            Inline::Text {
                text: "one ".into(),
                marks: Marks::default(),
            },
            Inline::Text {
                text: "two".into(),
                marks: Marks {
                    bold: true,
                    ..Marks::default()
                },
            },
            Inline::Text {
                text: " three".into(),
                marks: Marks::default(),
            },
        ])]);
        assert_eq!(
            serialize_html(&document),
            "<p>one <strong>two</strong> three</p>"
        );

        let repeated = CanonicalDocument::from_blocks(vec![paragraph(vec![
            Inline::Text {
                text: "a ".into(),
                marks: Marks::default(),
            },
            Inline::Text {
                text: " ".into(),
                marks: Marks {
                    italic: true,
                    ..Marks::default()
                },
            },
            Inline::Text {
                text: "b".into(),
                marks: Marks::default(),
            },
        ])]);
        assert_eq!(serialize_html(&repeated), "<p>a&nbsp;<em>&nbsp;</em>b</p>");
    }

    #[test]
    fn root_inline_whitespace_survives_wrappers_and_comments() {
        assert_eq!(
            search_text(&parse_html("<strong>one</strong><span> </span><em>two</em>").unwrap()),
            "one two"
        );
        assert_eq!(
            search_text(&parse_html("<strong>one</strong><!-- comment --> <em>two</em>").unwrap()),
            "one two"
        );
    }

    #[test]
    fn collapsible_whitespace_follows_inline_flow_matrix() {
        let cases = [
            ("<strong>one</strong> <em>two</em>", "one two"),
            ("<span><strong>one</strong> </span><em>two</em>", "one two"),
            (
                "<strong>one</strong><!-- comment --> <em>two</em>",
                "one two",
            ),
            (
                "<strong>one</strong> <img src=\":/0123456789abcdef0123456789abcdef\" alt=\"pic\"> <em>two</em>",
                "one pic two",
            ),
            (" <em>two</em>", "two"),
            ("<strong>one</strong> ", "one"),
            ("<p>one</p> \n <p>two</p>", "one\ntwo"),
            ("<p><strong>one</strong> <em>two</em></p>", "one two"),
        ];
        for (html, expected) in cases {
            assert_eq!(search_text(&parse_html(html).unwrap()), expected, "{html}");
        }
    }

    #[test]
    fn block_indentation_whitespace_is_not_promoted_to_content() {
        let document = parse_html("<p>one</p>\n  <div>two</div>\n  <p>three</p>").unwrap();
        assert_eq!(search_text(&document), "one\ntwo\nthree");
    }

    #[test]
    fn public_text_boundaries_normalize_tabs_newlines_and_adjacent_runs() {
        let document = CanonicalDocument::from_blocks(vec![paragraph(vec![
            Inline::Text {
                text: "a\tb".into(),
                marks: Marks::default(),
            },
            Inline::Text {
                text: "c\nd".into(),
                marks: Marks::default(),
            },
            Inline::Text {
                text: String::new(),
                marks: Marks::default(),
            },
        ])]);
        assert_eq!(
            document,
            CanonicalDocument::from_blocks(vec![paragraph(vec![
                Inline::Text {
                    text: "a    bc".into(),
                    marks: Marks::default(),
                },
                Inline::SoftBreak,
                Inline::Text {
                    text: "d".into(),
                    marks: Marks::default(),
                },
            ])])
        );
    }

    #[test]
    fn empty_paragraphs_have_a_visible_reversible_placeholder() {
        let document = CanonicalDocument::from_blocks(vec![
            paragraph(vec![Inline::Text {
                text: "first".into(),
                marks: Marks::default(),
            }]),
            paragraph(Vec::new()),
            paragraph(vec![Inline::Text {
                text: "last".into(),
                marks: Marks::default(),
            }]),
        ]);
        let html = serialize_html(&document);
        assert_eq!(html, "<p>first</p><p><br></p><p>last</p>");
        assert_eq!(parse_html(&html).unwrap(), document);
        assert_eq!(
            serialize_html(&CanonicalDocument::from_blocks(vec![
                paragraph(Vec::new()),
                paragraph(Vec::new()),
            ])),
            ""
        );
        assert_eq!(parse_html("<div><p>x</p></div>").unwrap().blocks.len(), 1);
    }

    #[test]
    fn table_foster_parenting_keeps_stray_text_before_table_cells() {
        let document = parse_html("<table>before<tr><td>cell</td></tr>after</table>").unwrap();
        assert_eq!(search_text(&document), "beforeaftercell");
    }

    #[test]
    fn deeply_nested_html_is_rejected_without_recursive_drop() {
        let depth = MAX_DOM_DEPTH + 1;
        let input = format!("{}x{}", "<div>".repeat(depth), "</div>".repeat(depth));
        assert_eq!(
            parse_html(&input),
            Err(DocumentError::DepthLimit {
                limit: MAX_DOM_DEPTH
            })
        );
    }

    #[test]
    fn accepted_boundary_html_survives_a_small_stack_worker() {
        let depth = MAX_DOM_DEPTH - 2;
        let input = format!("{}x{}", "<div>".repeat(depth), "</div>".repeat(depth));
        let worker = std::thread::Builder::new()
            .name("html-boundary-test".into())
            .stack_size(2 * 1024 * 1024)
            .spawn(move || parse_html(&input).map(|document| search_text(&document)))
            .expect("small-stack worker should start");
        assert_eq!(worker.join().expect("worker must not abort").unwrap(), "x");
    }

    #[test]
    fn large_flat_html_remains_accepted() {
        let input = format!("<p>{}</p>", "<span>x</span>".repeat(100_000));
        let document = parse_html(&input).unwrap();
        assert_eq!(search_text(&document).len(), 100_000);
    }

    #[test]
    fn html5_adoption_agency_preserves_nested_marks_without_panicking() {
        let document = parse_html("<p><strong><em>x</strong>y</em>z</p>").unwrap();
        assert_eq!(
            document,
            CanonicalDocument::from_blocks(vec![paragraph(vec![
                Inline::Text {
                    text: "x".into(),
                    marks: Marks {
                        bold: true,
                        italic: true,
                        ..Marks::default()
                    },
                },
                Inline::Text {
                    text: "y".into(),
                    marks: Marks {
                        italic: true,
                        ..Marks::default()
                    },
                },
                Inline::Text {
                    text: "z".into(),
                    marks: Marks::default(),
                },
            ])])
        );
    }

    #[test]
    fn tree_sink_can_move_an_existing_sibling_without_borrow_panics() {
        let sink = DomSink::default();
        let parent = DomSink::node(DomData::CanonicalDocument);
        let sibling = DomSink::node(DomData::Text(RefCell::new("sibling".into())));
        let moved = DomSink::node(DomData::Text(RefCell::new("moved".into())));
        DomSink::append_node(&parent, sibling.clone());
        DomSink::append_node(&parent, moved.clone());

        sink.append_before_sibling(&sibling, NodeOrText::AppendNode(moved.clone()));

        let children = parent.children.borrow();
        assert!(Rc::ptr_eq(&children[0], &moved));
        assert!(Rc::ptr_eq(&children[1], &sibling));
    }
}
