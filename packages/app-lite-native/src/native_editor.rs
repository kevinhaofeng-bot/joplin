//! Native semantic editing bridge.
//!
//! `Document` is the persisted model.  `TextDocument` is only the live edit
//! model used by TextKit.  In particular, this module deliberately does not
//! call any of text-document's markup exporters: doing so would make a
//! presentation format the source of truth and loses list nesting.

use crate::core::StoredResource;
use crate::html_body::{
    Alignment, Block, BlockStyle, Document, HeadingLevel, Inline, ListItem, ListKind, Marks,
};
use objc2::{AnyThread, rc::Retained};
use objc2_app_kit::{
    NSAttributedStringAttachmentConveniences, NSBackgroundColorAttributeName, NSColor, NSFont,
    NSFontAttributeName, NSFontDescriptorSymbolicTraits, NSImage, NSMutableParagraphStyle,
    NSParagraphStyleAttributeName, NSStrikethroughStyleAttributeName, NSTextAlignment,
    NSTextAttachment, NSTextList, NSTextListMarkerBox, NSTextListMarkerCheck,
    NSTextListMarkerDecimal, NSTextListMarkerDisc, NSTextListOptions, NSUnderlineStyle,
    NSUnderlineStyleAttributeName,
};
use objc2_core_foundation::{
    CFBoolean, CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};
use objc2_core_graphics::CGImage;
use objc2_foundation::{
    NSAttributedString, NSAttributedStringKey, NSMutableAttributedString, NSNumber, NSRange,
    NSSize, NSString, NSURL,
};
use objc2_image_io::{
    CGImageSource, kCGImageSourceCreateThumbnailFromImageAlways,
    kCGImageSourceCreateThumbnailWithTransform, kCGImageSourceThumbnailMaxPixelSize,
};
use std::collections::HashMap;
use std::sync::OnceLock;
use text_document::{
    Alignment as TdAlignment, BlockFormat, FlowElement, FragmentContent, ListStyle, MarkerType,
    MoveMode, TextCursor, TextDocument, TextFormat,
};
use thiserror::Error;

const RESOURCE_ID_KEY: &str = "com.kevinhao.joplin-lite.resource-id";
const RESOURCE_ALT_KEY: &str = "com.kevinhao.joplin-lite.resource-alt";
const RESOURCE_WIDTH_KEY: &str = "com.kevinhao.joplin-lite.resource-width";
const RESOURCE_HEIGHT_KEY: &str = "com.kevinhao.joplin-lite.resource-height";
const MISSING_RESOURCE_KEY: &str = "com.kevinhao.joplin-lite.missing-resource";
pub const INLINE_IMAGE_MAX_PIXEL_SIZE: usize = 1280;
pub const INLINE_IMAGE_MAX_WIDTH: f64 = 640.0;

pub fn image_paragraph_tail_indent(available_width: f64) -> f64 {
    -(available_width - INLINE_IMAGE_MAX_WIDTH).max(0.0)
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum EditorCodecError {
    #[error("text document error: {0}")]
    Model(String),
    #[error("invalid UTF-16 range")]
    InvalidUtf16Range,
    #[error("replacement contains invalid UTF-8")]
    InvalidReplacement,
    #[error("unsupported editor content")]
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockCommand {
    Paragraph,
    Heading(HeadingLevel),
    UnorderedList,
    OrderedList,
    Checklist,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineCommand {
    Bold,
    Italic,
    Underline,
    Strikethrough,
    Highlight,
    Clear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParagraphCommand {
    Align(Alignment),
    IncreaseIndent,
    DecreaseIndent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionState {
    Inactive,
    Active,
    Mixed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSelectionState {
    pub state: SelectionState,
    pub has_linkable_text: bool,
}

/// The one owner of the live edit history.  AppKit's text storage is a
/// projection and must not register a second undo manager for these edits.
pub struct NativeEditorSession {
    _backend: text_document::DocumentBackend,
    pub(crate) text: TextDocument,
    pub(crate) revision: u64,
    image_dimensions: HashMap<String, (u32, u32)>,
    highlighted_ranges: Vec<(usize, usize)>,
    highlight_undo: Vec<Vec<(usize, usize)>>,
    highlight_redo: Vec<Vec<(usize, usize)>>,
    typing_format: TextFormat,
    typing_override: Option<(usize, TextFormat)>,
    typing_undo: Vec<TypingState>,
    typing_redo: Vec<TypingState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TypingState {
    format: TextFormat,
    override_at: Option<(usize, TextFormat)>,
}

const MAX_EDITOR_UNDO_ENTRIES: usize = 200;

static EDITOR_BACKEND: OnceLock<text_document::DocumentBackend> = OnceLock::new();

fn editor_backend() -> text_document::DocumentBackend {
    EDITOR_BACKEND
        .get_or_init(text_document::DocumentBackend::new)
        .clone()
}

impl NativeEditorSession {
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn text_document(&self) -> &TextDocument {
        &self.text
    }

    pub fn image_dimensions(&self, resource_id: &str) -> Option<(u32, u32)> {
        self.image_dimensions.get(resource_id).copied()
    }

    pub fn can_undo(&self) -> bool {
        self.text.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.text.can_redo()
    }

    pub fn undo(&mut self) -> Result<(), EditorCodecError> {
        self.text.undo().map_err(model_error)?;
        if let Some(previous) = self.highlight_undo.pop() {
            self.highlight_redo.push(self.highlighted_ranges.clone());
            self.highlighted_ranges = previous;
        }
        if let Some(previous) = self.typing_undo.pop() {
            self.typing_redo.push(self.typing_state());
            self.restore_typing_state(previous);
        }
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    pub fn redo(&mut self) -> Result<(), EditorCodecError> {
        self.text.redo().map_err(model_error)?;
        if let Some(next) = self.highlight_redo.pop() {
            self.highlight_undo.push(self.highlighted_ranges.clone());
            self.highlighted_ranges = next;
        }
        if let Some(next) = self.typing_redo.pop() {
            self.typing_undo.push(self.typing_state());
            self.restore_typing_state(next);
        }
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    fn begin_command(&mut self) -> (TextCursor, Vec<(usize, usize)>, TypingState) {
        self.text.break_undo_merge();
        let cursor = self.text.cursor_at(0);
        cursor.begin_edit_block();
        (cursor, self.highlighted_ranges.clone(), self.typing_state())
    }

    fn finish_command(
        &mut self,
        cursor: TextCursor,
        previous: Vec<(usize, usize)>,
        previous_typing: TypingState,
    ) {
        cursor.end_edit_block();
        self.text.break_undo_merge();
        if self.highlight_undo.len() >= MAX_EDITOR_UNDO_ENTRIES {
            self.highlight_undo.remove(0);
        }
        self.highlight_undo.push(previous);
        self.highlight_redo.clear();
        if self.typing_undo.len() >= MAX_EDITOR_UNDO_ENTRIES {
            self.typing_undo.remove(0);
        }
        self.typing_undo.push(previous_typing);
        self.typing_redo.clear();
        self.revision = self.revision.wrapping_add(1);
    }

    fn is_highlighted(&self, position: usize) -> bool {
        self.highlighted_ranges
            .iter()
            .any(|(start, end)| *start <= position && position < *end)
    }

    fn typing_state(&self) -> TypingState {
        TypingState {
            format: self.typing_format.clone(),
            override_at: self.typing_override.clone(),
        }
    }

    fn restore_typing_state(&mut self, state: TypingState) {
        self.typing_format = state.format;
        self.typing_override = state.override_at;
    }

    fn inherited_typing_format(&self, position: usize) -> TextFormat {
        for element in self.text.flow() {
            let FlowElement::Block(block) = element else {
                continue;
            };
            let snapshot = block.snapshot();
            if position < snapshot.position || position > snapshot.position + snapshot.length {
                continue;
            }
            let local = position.saturating_sub(snapshot.position);
            let candidate = if local == 0 {
                snapshot
                    .fragments
                    .iter()
                    .find_map(|fragment| match fragment {
                        FragmentContent::Text {
                            offset: 0, format, ..
                        }
                        | FragmentContent::Image {
                            offset: 0, format, ..
                        }
                        | FragmentContent::FootnoteReference {
                            offset: 0, format, ..
                        } => Some(format.clone()),
                        _ => None,
                    })
            } else {
                snapshot
                    .fragments
                    .iter()
                    .find_map(|fragment| match fragment {
                        FragmentContent::Text {
                            offset,
                            length,
                            format,
                            ..
                        } if *offset < local && local <= offset + length => Some(format.clone()),
                        FragmentContent::Image { offset, format, .. }
                            if *offset < local && local <= offset + 1 =>
                        {
                            Some(format.clone())
                        }
                        FragmentContent::FootnoteReference { offset, format, .. }
                            if *offset < local && local <= offset + 1 =>
                        {
                            Some(format.clone())
                        }
                        _ => None,
                    })
            };
            return candidate.unwrap_or_default();
        }
        TextFormat::default()
    }

    fn effective_typing_format(&self, position: usize) -> TextFormat {
        self.typing_override
            .as_ref()
            .filter(|(override_at, _)| *override_at == position)
            .map(|(_, format)| format.clone())
            .unwrap_or_else(|| self.inherited_typing_format(position))
    }

    pub fn sync_caret_context(&mut self, selection: NSRange) {
        let Ok(text) = self.text.to_addressable_text() else {
            return;
        };
        let Ok((start, end)) = utf16_range(&text, selection) else {
            return;
        };
        if start != end {
            self.typing_override = None;
            self.typing_format = TextFormat::default();
            return;
        }
        if self
            .typing_override
            .as_ref()
            .is_some_and(|(override_at, _)| *override_at != start)
        {
            self.typing_override = None;
        }
        self.typing_format = self.effective_typing_format(start);
    }
}

fn run_edit_command<T, F>(session: &mut NativeEditorSession, action: F) -> T
where
    F: FnOnce(&mut NativeEditorSession) -> T,
{
    // All validation and fallible discovery happens before this point. The
    // edit block therefore accepts only an infallible action; an unexpected
    // text-document failure is handled by `must_apply`, which fail-stops
    // rather than returning an ordinary error after partial mutation.
    let (command_cursor, previous_highlights, previous_typing) = session.begin_command();
    let value = action(session);
    session.finish_command(command_cursor, previous_highlights, previous_typing);
    value
}

fn must_apply<T>(result: Result<T, impl std::fmt::Display>, operation: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("unexpected atomic editor failure in {operation}: {error}"),
    }
}

pub struct RenderedDocument {
    pub attributed: Retained<NSMutableAttributedString>,
    pub missing_resources: usize,
    pub empty_block_carriers: Vec<EmptyBlockCarrier>,
    pub attachments: Vec<RenderedAttachment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedAttachment {
    pub addressable_offset: usize,
    pub resource_id: String,
}

/// Projection metadata for a semantic block with no addressable characters.
/// TextKit cannot attach a paragraph attribute to a zero-length range, so the
/// renderer keeps the native paragraph/list carrier beside the attributed
/// string. It is display-only and is never decoded back into the model.
#[derive(Clone)]
pub struct EmptyBlockCarrier {
    pub addressable_offset: usize,
    pub paragraph: Retained<NSMutableParagraphStyle>,
}

fn model_error(error: impl std::fmt::Display) -> EditorCodecError {
    EditorCodecError::Model(error.to_string())
}

#[derive(Debug, Clone)]
struct LogicalBlock {
    text: String,
    block: Block,
    images: Vec<(usize, String, String)>,
}

fn logical_blocks(document: &Document) -> Vec<LogicalBlock> {
    let mut result = Vec::new();
    for block in &document.blocks {
        match block {
            Block::Paragraph { .. } | Block::Heading { .. } => {
                for block in split_image_blocks(block) {
                    result.push(logical_block(block));
                }
            }
            Block::List { kind, items } => {
                for item in items {
                    result.push(logical_block(Block::List {
                        kind: *kind,
                        items: vec![item.clone()],
                    }));
                }
            }
        }
    }
    result
}

/// NSTextAttachment participates in the line fragment that contains it.  If
/// an image remains inside a heading (or any other text block), TextKit uses
/// that block's font metrics for the attachment and can clip the image or
/// leave following text on the same line.  Normalize image-bearing prose
/// blocks into an image paragraph so the attachment receives a full line and
/// the next text block starts at the normal leading edge.  The persisted HTML
/// remains deterministic: the native edit boundary simply makes the image's
/// block structure explicit on the next save.
fn split_image_blocks(block: &Block) -> Vec<Block> {
    let (kind, style, inlines) = match block {
        Block::Paragraph { style, inlines } => (None, *style, inlines.as_slice()),
        Block::Heading {
            level,
            style,
            inlines,
        } => (Some(*level), *style, inlines.as_slice()),
        Block::List { .. } => return vec![block.clone()],
    };
    if !inlines
        .iter()
        .any(|inline| matches!(inline, Inline::Image { .. }))
    {
        return vec![block.clone()];
    }

    let mut blocks = Vec::new();
    let mut text_inlines = Vec::new();
    let push_text = |blocks: &mut Vec<Block>, text_inlines: &mut Vec<Inline>| {
        if text_inlines.is_empty() {
            return;
        }
        let inlines = std::mem::take(text_inlines);
        blocks.push(match kind {
            Some(level) => Block::Heading {
                level,
                style,
                inlines,
            },
            None => Block::Paragraph { style, inlines },
        });
    };
    for inline in inlines {
        if matches!(inline, Inline::Image { .. }) {
            push_text(&mut blocks, &mut text_inlines);
            blocks.push(Block::Paragraph {
                style,
                inlines: vec![inline.clone()],
            });
        } else {
            text_inlines.push(inline.clone());
        }
    }
    push_text(&mut blocks, &mut text_inlines);
    blocks
}

fn logical_block(block: Block) -> LogicalBlock {
    let inlines = match &block {
        Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. } => inlines,
        Block::List { items, .. } => &items[0].inlines,
    };
    let mut text = String::new();
    let mut images = Vec::new();
    for inline in inlines {
        match inline {
            Inline::Text { text: value, .. } => text.push_str(value),
            Inline::SoftBreak => text.push('\u{2028}'),
            Inline::Image { resource_id, alt } => {
                let position = text.chars().count();
                images.push((position, resource_id.clone(), alt.clone()));
            }
        }
    }
    LogicalBlock {
        text,
        block,
        images,
    }
}

fn highlight_ranges(blocks: &[LogicalBlock]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut document_offset = 0usize;
    for logical in blocks {
        let mut local_offset = 0usize;
        for inline in block_inlines(&logical.block) {
            match inline {
                Inline::Text { text, marks } => {
                    let length = text.chars().count();
                    if marks.highlight && length > 0 {
                        ranges.push((
                            document_offset + local_offset,
                            document_offset + local_offset + length,
                        ));
                    }
                    local_offset += length;
                }
                Inline::SoftBreak | Inline::Image { .. } => local_offset += 1,
            }
        }
        document_offset += logical.text.chars().count() + logical.images.len() + 1;
    }
    ranges
}

pub fn session_from_document(document: &Document) -> Result<NativeEditorSession, EditorCodecError> {
    let blocks = logical_blocks(document);
    let text = blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let backend = editor_backend();
    let model = TextDocument::try_new_in(&backend).map_err(model_error)?;
    model.set_plain_text(&text).map_err(model_error)?;

    let mut plain_offsets = Vec::with_capacity(blocks.len());
    let mut plain_lengths = Vec::with_capacity(blocks.len());
    let mut addressable_offsets = Vec::with_capacity(blocks.len());
    let mut plain_offset = 0usize;
    let mut addressable_offset = 0usize;
    for logical in &blocks {
        let plain_length = logical.text.chars().count();
        plain_offsets.push(plain_offset);
        plain_lengths.push(plain_length);
        addressable_offsets.push(addressable_offset);
        plain_offset += plain_length + 1;
        addressable_offset += plain_length + logical.images.len() + 1;
    }
    let mut index = 0usize;
    while index < blocks.len() {
        let Some(kind) = (match &blocks[index].block {
            Block::List { kind, .. } => Some(*kind),
            _ => None,
        }) else {
            index += 1;
            continue;
        };
        let run_start = index;
        index += 1;
        while index < blocks.len()
            && matches!(&blocks[index].block, Block::List { kind: next, .. } if *next == kind)
        {
            index += 1;
        }
        let start = plain_offsets[run_start];
        let end = plain_offsets[index - 1] + plain_lengths[index - 1];
        let cursor = model.cursor_at(start);
        cursor.set_position(end, MoveMode::KeepAnchor);
        let style = match kind {
            ListKind::Ordered => ListStyle::Decimal,
            ListKind::Unordered | ListKind::Checklist => ListStyle::Disc,
        };
        cursor.create_list(style).map_err(model_error)?;
    }

    let mut image_dimensions = HashMap::new();
    for (index, logical) in blocks.iter().enumerate() {
        let plain_offset = plain_offsets[index];
        let addressable_offset = addressable_offsets[index];
        let block_len = logical.text.chars().count();
        let cursor = model.cursor_at(plain_offset);
        cursor.set_position(plain_offset + block_len, MoveMode::KeepAnchor);
        apply_block_style(&cursor, &logical.block)?;
        apply_list_item_style(&cursor, &logical.block)?;

        let mut local_offset = 0usize;
        let inlines = block_inlines(&logical.block);
        for inline in inlines {
            let scalar_len = match inline {
                Inline::Text { text, marks } => {
                    let len = text.chars().count();
                    if len > 0 {
                        let range = model.cursor_at(plain_offset + local_offset);
                        range.set_position(plain_offset + local_offset + len, MoveMode::KeepAnchor);
                        range
                            .set_char_format(&text_format(marks))
                            .map_err(model_error)?;
                    }
                    len
                }
                Inline::SoftBreak => 1,
                Inline::Image {
                    resource_id: _,
                    alt: _,
                } => 0,
            };
            local_offset += scalar_len;
        }
        // An image is an object anchor, not a literal U+FFFC character.  Insert
        // anchors from right to left so their positions are stable and never
        // require feeding an object sentinel through set_plain_text.
        for (position, resource_id, alt) in logical.images.iter().rev() {
            let image = model.cursor_at(addressable_offset + *position);
            // TextDocument requires positive dimensions for an image anchor;
            // actual pixels stay in ResourceStore and are never copied here.
            image
                .insert_image(resource_id, alt, 1, 1)
                .map_err(model_error)?;
            image_dimensions
                .entry(resource_id.clone())
                .or_insert((1, 1));
        }
    }

    // Formatting and object-anchor setup above is construction, not user
    // editing. Never expose it as undoable history.
    model.clear_undo_redo();
    model.set_undo_limit(Some(MAX_EDITOR_UNDO_ENTRIES));

    Ok(NativeEditorSession {
        _backend: backend,
        text: model,
        revision: 0,
        image_dimensions,
        highlighted_ranges: highlight_ranges(&blocks),
        highlight_undo: Vec::new(),
        highlight_redo: Vec::new(),
        typing_format: TextFormat::default(),
        typing_override: None,
        typing_undo: Vec::new(),
        typing_redo: Vec::new(),
    })
}

fn block_inlines(block: &Block) -> &[Inline] {
    match block {
        Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. } => inlines,
        Block::List { items, .. } => &items[0].inlines,
    }
}

fn apply_block_style(cursor: &TextCursor, block: &Block) -> Result<(), EditorCodecError> {
    let style = match block {
        Block::Paragraph { style, .. } | Block::Heading { style, .. } => style,
        Block::List { items, .. } => &items[0].style,
    };
    let heading_level = match block {
        Block::Heading { level, .. } => Some(match level {
            HeadingLevel::One => 1,
            HeadingLevel::Two => 2,
            HeadingLevel::Three => 3,
        }),
        _ => None,
    };
    cursor
        .set_block_format(&BlockFormat {
            alignment: Some(td_alignment(style.alignment)),
            heading_level,
            indent: Some(style.indent),
            ..Default::default()
        })
        .map_err(model_error)
}

fn apply_list_item_style(cursor: &TextCursor, block: &Block) -> Result<(), EditorCodecError> {
    let Some(kind) = (match block {
        Block::List { kind, .. } => Some(*kind),
        _ => None,
    }) else {
        return Ok(());
    };
    if let ListKind::Checklist = kind {
        let marker = match block {
            Block::List { items, .. } => {
                if items[0].checked == Some(true) {
                    MarkerType::Checked
                } else {
                    MarkerType::Unchecked
                }
            }
            _ => MarkerType::Unchecked,
        };
        cursor
            .set_block_format(&BlockFormat {
                marker: Some(marker),
                ..Default::default()
            })
            .map_err(model_error)?;
    }
    // ListFormat belongs to the whole native list.  Per-item nesting lives on
    // each block's BlockFormat, which keeps adjacent items independently
    // round-trippable without mutating one shared list for every item.
    Ok(())
}

fn td_alignment(alignment: Alignment) -> TdAlignment {
    match alignment {
        Alignment::Left => TdAlignment::Left,
        Alignment::Center => TdAlignment::Center,
        Alignment::Right => TdAlignment::Right,
    }
}

fn html_alignment(alignment: Option<TdAlignment>) -> Alignment {
    match alignment.unwrap_or(TdAlignment::Left) {
        TdAlignment::Center => Alignment::Center,
        TdAlignment::Right => Alignment::Right,
        TdAlignment::Left | TdAlignment::Justify => Alignment::Left,
    }
}

fn text_format(marks: &Marks) -> TextFormat {
    TextFormat {
        font_bold: Some(marks.bold),
        font_italic: Some(marks.italic),
        font_underline: Some(marks.underline),
        font_strikeout: Some(marks.strikethrough),
        background_color: marks
            .highlight
            .then_some(text_document::Color::rgb(255, 235, 130)),
        anchor_href: marks.link.clone(),
        ..Default::default()
    }
}

fn marks_from_format(format: &TextFormat) -> Marks {
    Marks {
        bold: format.font_bold == Some(true)
            || format.font_weight.is_some_and(|weight| weight >= 600),
        italic: format.font_italic == Some(true),
        underline: format.font_underline == Some(true),
        strikethrough: format.font_strikeout == Some(true),
        highlight: format.background_color.is_some_and(|color| color.alpha > 0),
        link: format.anchor_href.clone(),
    }
}

pub fn document_from_session(session: &NativeEditorSession) -> Result<Document, EditorCodecError> {
    let mut blocks = Vec::new();
    for element in session.text.flow() {
        let FlowElement::Block(block) = element else {
            continue;
        };
        let snapshot = block.snapshot();
        let mut inlines = Vec::new();
        for fragment in snapshot.fragments {
            match fragment {
                FragmentContent::Text {
                    text,
                    format,
                    offset,
                    ..
                } => {
                    for (index, character) in text.chars().enumerate() {
                        if character == '\u{2028}' || character == '\u{000b}' {
                            inlines.push(Inline::SoftBreak);
                        } else if character != '\r' {
                            let mut marks = marks_from_format(&format);
                            if session.is_highlighted(snapshot.position + offset + index) {
                                marks.highlight = true;
                            }
                            append_text(&mut inlines, &character.to_string(), &marks);
                        }
                    }
                }
                FragmentContent::Image { name, alt, .. } => {
                    inlines.push(Inline::Image {
                        resource_id: name,
                        alt,
                    });
                }
                FragmentContent::FootnoteReference { .. } => {
                    return Err(EditorCodecError::Unsupported);
                }
            }
        }
        let style = BlockStyle {
            alignment: html_alignment(snapshot.block_format.alignment),
            indent: snapshot
                .block_format
                .indent
                .or(snapshot.list_info.as_ref().map(|info| info.indent))
                .unwrap_or(0)
                .min(8),
        };
        if let Some(list) = snapshot.list_info {
            let kind = if matches!(
                snapshot.block_format.marker,
                Some(MarkerType::Checked | MarkerType::Unchecked)
            ) {
                ListKind::Checklist
            } else if matches!(
                list.style,
                ListStyle::Decimal
                    | ListStyle::LowerAlpha
                    | ListStyle::UpperAlpha
                    | ListStyle::LowerRoman
                    | ListStyle::UpperRoman
            ) {
                ListKind::Ordered
            } else {
                ListKind::Unordered
            };
            let checked = match snapshot.block_format.marker {
                Some(MarkerType::Checked) => Some(true),
                Some(MarkerType::Unchecked) => Some(false),
                _ => None,
            };
            if let Some(Block::List {
                kind: previous_kind,
                items,
            }) = blocks.last_mut()
                && *previous_kind == kind
            {
                items.push(ListItem {
                    checked,
                    style,
                    inlines,
                });
                continue;
            }
            blocks.push(Block::List {
                kind,
                items: vec![ListItem {
                    checked,
                    style,
                    inlines,
                }],
            });
        } else if let Some(level) = snapshot.block_format.heading_level {
            let level = match level {
                1 => HeadingLevel::One,
                2 => HeadingLevel::Two,
                _ => HeadingLevel::Three,
            };
            blocks.push(Block::Heading {
                level,
                style,
                inlines,
            });
        } else {
            blocks.push(Block::Paragraph { style, inlines });
        }
    }
    Ok(Document::from_blocks(blocks))
}

fn append_text(inlines: &mut Vec<Inline>, text: &str, marks: &Marks) {
    if let Some(Inline::Text {
        text: previous,
        marks: previous_marks,
    }) = inlines.last_mut()
        && previous_marks == marks
    {
        previous.push_str(text);
    } else {
        inlines.push(Inline::Text {
            text: text.to_owned(),
            marks: marks.clone(),
        });
    }
}

fn utf16_range(text: &str, range: NSRange) -> Result<(usize, usize), EditorCodecError> {
    let end = range
        .location
        .checked_add(range.length)
        .ok_or(EditorCodecError::InvalidUtf16Range)?;
    let mut start_scalar = None;
    let mut end_scalar = None;
    let mut offset = 0usize;
    // Zero is a valid boundary for both insertion and replacement, including
    // a non-empty document.  Seed both ends before scanning so NSRange(0, 0)
    // does not depend on seeing a character after the boundary.
    if range.location == 0 {
        start_scalar = Some(0);
    }
    if end == 0 {
        end_scalar = Some(0);
    }
    for (scalar, character) in text.chars().enumerate() {
        if offset == range.location {
            start_scalar = Some(scalar);
        }
        offset += character.len_utf16();
        if offset == end {
            end_scalar = Some(scalar + 1);
        }
    }
    if start_scalar.is_none() && range.location == offset {
        start_scalar = Some(text.chars().count());
    }
    if end == offset {
        end_scalar = Some(text.chars().count());
    }
    match (start_scalar, end_scalar) {
        (Some(start), Some(end)) => Ok((start, end)),
        _ => Err(EditorCodecError::InvalidUtf16Range),
    }
}

fn adjust_highlight_ranges(
    ranges: &mut Vec<(usize, usize)>,
    start: usize,
    end: usize,
    replacement_length: usize,
) {
    let removed = end.saturating_sub(start);
    let delta = replacement_length as isize - removed as isize;
    let mut adjusted = Vec::new();
    for (range_start, range_end) in ranges.drain(..) {
        if range_end <= start {
            adjusted.push((range_start, range_end));
        } else if range_start >= end {
            let shifted_start = (range_start as isize + delta) as usize;
            let shifted_end = (range_end as isize + delta) as usize;
            adjusted.push((shifted_start, shifted_end));
        } else {
            if range_start < start {
                adjusted.push((range_start, start));
            }
            if range_end > end {
                let suffix_start = start + replacement_length;
                adjusted.push((suffix_start, suffix_start + range_end - end));
            }
        }
    }
    *ranges = adjusted;
}

fn toggle_highlight_range(session: &mut NativeEditorSession, start: usize, end: usize) {
    let active = (start..end).all(|position| session.is_highlighted(position));
    if active {
        let mut result = Vec::new();
        for (range_start, range_end) in session.highlighted_ranges.drain(..) {
            if range_end <= start || range_start >= end {
                result.push((range_start, range_end));
                continue;
            }
            if range_start < start {
                result.push((range_start, start));
            }
            if range_end > end {
                result.push((end, range_end));
            }
        }
        session.highlighted_ranges = result;
    } else {
        session.highlighted_ranges.push((start, end));
    }
}

fn clear_highlight_range(session: &mut NativeEditorSession, start: usize, end: usize) {
    let mut result = Vec::new();
    for (range_start, range_end) in session.highlighted_ranges.drain(..) {
        if range_end <= start || range_start >= end {
            result.push((range_start, range_end));
            continue;
        }
        if range_start < start {
            result.push((range_start, start));
        }
        if range_end > end {
            result.push((end, range_end));
        }
    }
    session.highlighted_ranges = result;
}

fn selection_needs_clear(session: &NativeEditorSession, start: usize, end: usize) -> bool {
    if (start..end).any(|position| session.is_highlighted(position)) {
        return true;
    }
    for element in session.text.flow() {
        let FlowElement::Block(block) = element else {
            continue;
        };
        let snapshot = block.snapshot();
        for fragment in snapshot.fragments {
            let FragmentContent::Text {
                offset,
                length,
                format,
                ..
            } = fragment
            else {
                continue;
            };
            let from = snapshot.position + offset;
            let to = from + length;
            if from >= end || to <= start {
                continue;
            }
            if format.font_bold == Some(true)
                || format.font_italic == Some(true)
                || format.font_underline == Some(true)
                || format.font_strikeout == Some(true)
                || format.anchor_href.is_some()
                || format.background_color.is_some_and(|color| color.alpha > 0)
            {
                return true;
            }
        }
    }
    false
}

pub fn apply_committed_text_delta(
    session: &mut NativeEditorSession,
    range: NSRange,
    replacement: &str,
) -> Result<(), EditorCodecError> {
    if replacement.contains('\0') {
        return Err(EditorCodecError::InvalidReplacement);
    }
    if replacement.contains('\u{fffc}') {
        return Err(EditorCodecError::Unsupported);
    }
    let text = session.text.to_addressable_text().map_err(model_error)?;
    let (start, end) = utf16_range(&text, range)?;
    let current: String = text.chars().skip(start).take(end - start).collect();
    if current == replacement {
        return Ok(());
    }
    let typing_format = session.effective_typing_format(start);
    run_edit_command(session, |session| {
        let cursor = session.text.cursor_at(start);
        cursor.set_position(end, MoveMode::KeepAnchor);
        must_apply(cursor.insert_text(replacement), "insert committed text");
        if !replacement.is_empty() && typing_format != TextFormat::default() {
            let format_cursor = session.text.cursor_at(start);
            format_cursor.set_position(start + replacement.chars().count(), MoveMode::KeepAnchor);
            must_apply(
                format_cursor.merge_char_format(&typing_format),
                "apply pending typing format",
            );
        }
        adjust_highlight_ranges(
            &mut session.highlighted_ranges,
            start,
            end,
            replacement.chars().count(),
        );
    });
    Ok(())
}

fn toggled_typing_format(current: &TextFormat, command: InlineCommand) -> TextFormat {
    let mut next = current.clone();
    match command {
        InlineCommand::Bold => next.font_bold = Some(current.font_bold != Some(true)),
        InlineCommand::Italic => next.font_italic = Some(current.font_italic != Some(true)),
        InlineCommand::Underline => {
            next.font_underline = Some(current.font_underline != Some(true))
        }
        InlineCommand::Strikethrough => {
            next.font_strikeout = Some(current.font_strikeout != Some(true))
        }
        InlineCommand::Highlight => {
            let active = current
                .background_color
                .as_ref()
                .is_some_and(|color| color.alpha != 0);
            next.background_color = if active {
                Some(text_document::Color::rgba(0, 0, 0, 0))
            } else {
                Some(text_document::Color::rgb(255, 235, 130))
            };
        }
        InlineCommand::Clear => {
            next = TextFormat {
                font_bold: Some(false),
                font_italic: Some(false),
                font_underline: Some(false),
                font_strikeout: Some(false),
                background_color: Some(text_document::Color::rgba(0, 0, 0, 0)),
                clear_link: true,
                ..Default::default()
            };
        }
    }
    next
}

/// Insert a resource-backed object anchor. Pixel data stays in ResourceStore;
/// TextDocument receives only the stable resource name, alt text and display
/// dimensions.
pub fn insert_image_anchor(
    session: &mut NativeEditorSession,
    selection: NSRange,
    resource_id: &str,
    alt: &str,
    width: u32,
    height: u32,
) -> Result<(), EditorCodecError> {
    let text = session.text.to_addressable_text().map_err(model_error)?;
    let (start, end) = utf16_range(&text, selection)?;
    run_edit_command(session, |session| {
        let cursor = session.text.cursor_at(start);
        cursor.set_position(end, MoveMode::KeepAnchor);
        must_apply(
            cursor.insert_image(resource_id, alt, width.max(1), height.max(1)),
            "insert image anchor",
        );
        adjust_highlight_ranges(&mut session.highlighted_ranges, start, end, 1);
        session
            .image_dimensions
            .insert(resource_id.to_owned(), (width.max(1), height.max(1)));
    });
    Ok(())
}

/// Insert a resource-backed image as its own paragraph. The selection is
/// replaced by a block break, the image is inserted into that new block, and
/// a second block break restores the following text. All three native edits
/// are owned by the surrounding edit block and therefore undo as one command.
pub fn insert_image_block_anchor(
    session: &mut NativeEditorSession,
    selection: NSRange,
    resource_id: &str,
    alt: &str,
    width: u32,
    height: u32,
) -> Result<(), EditorCodecError> {
    let text = session.text.to_addressable_text().map_err(model_error)?;
    let (start, end) = utf16_range(&text, selection)?;
    let old_length = text.chars().count();
    run_edit_command(session, |session| {
        let cursor = session.text.cursor_at(start);
        cursor.set_position(end, MoveMode::KeepAnchor);
        must_apply(cursor.insert_block(), "split image block");
        must_apply(
            cursor.insert_image(resource_id, alt, width.max(1), height.max(1)),
            "insert block image anchor",
        );
        if !cursor.at_end() {
            must_apply(cursor.insert_block(), "close image block");
        }
        let new_length = must_apply(
            session.text.to_addressable_text(),
            "read inserted image block",
        )
        .chars()
        .count();
        let replaced_length = end.saturating_sub(start);
        let replacement_length = new_length.saturating_sub(old_length - replaced_length);
        adjust_highlight_ranges(
            &mut session.highlighted_ranges,
            start,
            end,
            replacement_length,
        );
        session
            .image_dimensions
            .insert(resource_id.to_owned(), (width.max(1), height.max(1)));
    });
    Ok(())
}

fn link_command_is_noop(
    session: &NativeEditorSession,
    start: usize,
    end: usize,
    url: Option<&str>,
) -> bool {
    if start == end {
        return false;
    }
    for element in session.text.flow() {
        let FlowElement::Block(block) = element else {
            continue;
        };
        let snapshot = block.snapshot();
        for fragment in snapshot.fragments {
            let FragmentContent::Text {
                text,
                offset,
                format,
                ..
            } = fragment
            else {
                continue;
            };
            for (index, character) in text.chars().enumerate() {
                if matches!(character, '\u{2028}' | '\u{000b}' | '\r') {
                    continue;
                }
                let position = snapshot.position + offset + index;
                if position < start || position >= end {
                    continue;
                }
                if format.anchor_href.as_deref() != url {
                    return false;
                }
            }
        }
    }
    // A range containing only a non-persistent soft-break carrier has no
    // linkable text and therefore must not create an undo entry.
    true
}

/// Remove exactly one resource-backed object anchor. Live text synchronization
/// may call this only after proving that the changed slice is a lone U+FFFC;
/// arbitrary attributed-string flattening is never used for image edits.
pub fn delete_image_anchor(
    session: &mut NativeEditorSession,
    selection: NSRange,
) -> Result<(), EditorCodecError> {
    delete_image_anchor_if_identity(session, selection, None)
}

pub fn delete_image_anchor_if_identity(
    session: &mut NativeEditorSession,
    selection: NSRange,
    expected_resource_id: Option<&str>,
) -> Result<(), EditorCodecError> {
    let text = session.text.to_addressable_text().map_err(model_error)?;
    let (start, end) = utf16_range(&text, selection)?;
    if end != start + 1 || text.chars().nth(start) != Some('\u{fffc}') {
        return Err(EditorCodecError::Unsupported);
    }
    if let Some(expected_resource_id) = expected_resource_id
        && image_resource_id_at(session, start).as_deref() != Some(expected_resource_id)
    {
        return Err(EditorCodecError::Unsupported);
    }
    run_edit_command(session, |session| {
        let cursor = session.text.cursor_at(start);
        cursor.set_position(end, MoveMode::KeepAnchor);
        let deleted = must_apply(cursor.remove_selected_text(), "delete image anchor");
        debug_assert_eq!(deleted, "\u{fffc}");
        adjust_highlight_ranges(&mut session.highlighted_ranges, start, end, 0);
    });
    Ok(())
}

fn image_resource_id_at(session: &NativeEditorSession, position: usize) -> Option<String> {
    for element in session.text.flow() {
        let FlowElement::Block(block) = element else {
            continue;
        };
        let snapshot = block.snapshot();
        for fragment in snapshot.fragments {
            if let FragmentContent::Image { name, offset, .. } = fragment
                && snapshot.position + offset == position
            {
                return Some(name);
            }
        }
    }
    None
}

pub fn apply_link(
    session: &mut NativeEditorSession,
    selection: NSRange,
    url: Option<&str>,
) -> Result<(), EditorCodecError> {
    if let Some(url) = url
        && !valid_editor_link(url)
    {
        return Err(EditorCodecError::Unsupported);
    }
    let text = session.text.to_addressable_text().map_err(model_error)?;
    let (start, end) = utf16_range(&text, selection)?;
    if link_command_is_noop(session, start, end, url) {
        return Ok(());
    }
    run_edit_command(session, |session| {
        let cursor = session.text.cursor_at(start);
        cursor.set_position(end, MoveMode::KeepAnchor);
        let format = match url {
            Some(url) => TextFormat {
                anchor_href: Some(url.to_owned()),
                ..Default::default()
            },
            None => TextFormat {
                clear_link: true,
                ..Default::default()
            },
        };
        must_apply(cursor.merge_char_format(&format), "apply link");
    });
    Ok(())
}

fn block_command_is_noop(
    session: &NativeEditorSession,
    start: usize,
    end: usize,
    command: BlockCommand,
) -> bool {
    if !matches!(command, BlockCommand::Paragraph | BlockCommand::Heading(_)) {
        return false;
    }
    let mut found = false;
    for element in session.text.flow() {
        let FlowElement::Block(block) = element else {
            continue;
        };
        let snapshot = block.snapshot();
        let overlaps = if start == end {
            snapshot.position <= start && start <= snapshot.position + snapshot.length
        } else {
            snapshot.position < end && snapshot.position + snapshot.length > start
        };
        if !overlaps {
            continue;
        }
        found = true;
        let same = match command {
            BlockCommand::Paragraph => {
                snapshot.list_info.is_none()
                    && snapshot
                        .block_format
                        .heading_level
                        .is_none_or(|level| level == 0)
                    && snapshot
                        .block_format
                        .marker
                        .is_none_or(|marker| marker == MarkerType::NoMarker)
            }
            BlockCommand::Heading(level) => {
                let expected = match level {
                    HeadingLevel::One => 1,
                    HeadingLevel::Two => 2,
                    HeadingLevel::Three => 3,
                };
                snapshot.list_info.is_none()
                    && snapshot.block_format.heading_level == Some(expected)
            }
            BlockCommand::UnorderedList | BlockCommand::OrderedList | BlockCommand::Checklist => {
                false
            }
        };
        if !same {
            return false;
        }
    }
    found
}

fn actual_list_command(snapshot: &text_document::BlockSnapshot) -> Option<BlockCommand> {
    let list = snapshot.list_info.as_ref()?;
    Some(match list.style {
        ListStyle::Decimal => BlockCommand::OrderedList,
        _ if matches!(
            snapshot.block_format.marker,
            Some(MarkerType::Checked | MarkerType::Unchecked)
        ) =>
        {
            BlockCommand::Checklist
        }
        _ => BlockCommand::UnorderedList,
    })
}

fn list_command_is_active(
    session: &NativeEditorSession,
    start: usize,
    end: usize,
    command: BlockCommand,
) -> bool {
    if !matches!(
        command,
        BlockCommand::UnorderedList | BlockCommand::OrderedList | BlockCommand::Checklist
    ) {
        return false;
    }
    let mut found = false;
    for element in session.text.flow() {
        let FlowElement::Block(block) = element else {
            continue;
        };
        let snapshot = block.snapshot();
        let overlaps = if start == end {
            snapshot.position <= start && start <= snapshot.position + snapshot.length
        } else {
            snapshot.position < end && snapshot.position + snapshot.length > start
        };
        if overlaps {
            found = true;
            if actual_list_command(&snapshot) != Some(command) {
                return false;
            }
        }
    }
    found
}

pub fn query_block_state(
    session: &NativeEditorSession,
    selection: NSRange,
    command: BlockCommand,
) -> Result<SelectionState, EditorCodecError> {
    let text = session.text.to_addressable_text().map_err(model_error)?;
    let (start, end) = utf16_range(&text, selection)?;
    let mut values = Vec::new();
    for element in session.text.flow() {
        let FlowElement::Block(block) = element else {
            continue;
        };
        let snapshot = block.snapshot();
        let overlaps = if start == end {
            snapshot.position <= start && start <= snapshot.position + snapshot.length
        } else {
            snapshot.position < end && snapshot.position + snapshot.length > start
        };
        if !overlaps {
            continue;
        }
        let matches = match command {
            BlockCommand::Paragraph => {
                snapshot.list_info.is_none()
                    && snapshot
                        .block_format
                        .heading_level
                        .is_none_or(|level| level == 0)
                    && snapshot
                        .block_format
                        .marker
                        .is_none_or(|marker| marker == MarkerType::NoMarker)
            }
            BlockCommand::Heading(level) => {
                let expected = match level {
                    HeadingLevel::One => 1,
                    HeadingLevel::Two => 2,
                    HeadingLevel::Three => 3,
                };
                snapshot.list_info.is_none()
                    && snapshot.block_format.heading_level == Some(expected)
            }
            BlockCommand::UnorderedList | BlockCommand::OrderedList | BlockCommand::Checklist => {
                actual_list_command(&snapshot) == Some(command)
            }
        };
        values.push(matches);
    }
    if values.is_empty() || values.iter().all(|value| !*value) {
        return Ok(SelectionState::Inactive);
    }
    if values.iter().all(|value| *value) {
        Ok(SelectionState::Active)
    } else {
        Ok(SelectionState::Mixed)
    }
}

pub fn query_paragraph_command_state(
    session: &NativeEditorSession,
    selection: NSRange,
    command: ParagraphCommand,
) -> Result<SelectionState, EditorCodecError> {
    let text = session.text.to_addressable_text().map_err(model_error)?;
    let (start, end) = utf16_range(&text, selection)?;
    let mut values = Vec::new();
    for element in session.text.flow() {
        let FlowElement::Block(block) = element else {
            continue;
        };
        let snapshot = block.snapshot();
        let overlaps = if start == end {
            snapshot.position <= start && start <= snapshot.position + snapshot.length
        } else {
            snapshot.position < end && snapshot.position + snapshot.length > start
        };
        if !overlaps {
            continue;
        }
        let indent = snapshot.block_format.indent.unwrap_or(0);
        values.push(match command {
            ParagraphCommand::Align(alignment) => {
                html_alignment(snapshot.block_format.alignment) == alignment
            }
            ParagraphCommand::IncreaseIndent => indent >= 8,
            ParagraphCommand::DecreaseIndent => indent == 0,
        });
    }
    if values.is_empty() || values.iter().all(|value| !*value) {
        return Ok(SelectionState::Inactive);
    }
    if values.iter().all(|value| *value) {
        Ok(SelectionState::Active)
    } else {
        Ok(SelectionState::Mixed)
    }
}

fn format_needs_clear(format: &TextFormat) -> bool {
    format.font_bold == Some(true)
        || format.font_italic == Some(true)
        || format.font_underline == Some(true)
        || format.font_strikeout == Some(true)
        || format.anchor_href.is_some()
        || format
            .background_color
            .as_ref()
            .is_some_and(|color| color.alpha > 0)
}

pub fn query_clear_state(
    session: &NativeEditorSession,
    selection: NSRange,
) -> Result<SelectionState, EditorCodecError> {
    let text = session.text.to_addressable_text().map_err(model_error)?;
    let (start, end) = utf16_range(&text, selection)?;
    let active = if start == end {
        format_needs_clear(&session.effective_typing_format(start))
    } else {
        selection_needs_clear(session, start, end)
    };
    Ok(if active {
        SelectionState::Active
    } else {
        SelectionState::Inactive
    })
}

pub fn apply_inline_command(
    session: &mut NativeEditorSession,
    selection: NSRange,
    command: InlineCommand,
) -> Result<(), EditorCodecError> {
    let text = session.text.to_addressable_text().map_err(model_error)?;
    let (start, end) = utf16_range(&text, selection)?;
    if start == end {
        let current = session.effective_typing_format(start);
        let next = toggled_typing_format(&current, command);
        run_edit_command(session, |session| {
            let cursor = session.text.cursor_at(start);
            must_apply(cursor.merge_char_format(&next), "set typing format");
            session.typing_format = next.clone();
            session.typing_override = Some((start, next));
        });
        return Ok(());
    }
    if matches!(command, InlineCommand::Highlight) {
        let active = query_inline_state(session, selection, command)? == SelectionState::Active;
        run_edit_command(session, |session| {
            toggle_highlight_range(session, start, end);
            let cursor = session.text.cursor_at(start);
            cursor.set_position(end, MoveMode::KeepAnchor);
            must_apply(
                cursor.merge_char_format(&TextFormat {
                    background_color: Some(if active {
                        text_document::Color::rgba(0, 0, 0, 0)
                    } else {
                        text_document::Color::rgb(255, 235, 130)
                    }),
                    ..Default::default()
                }),
                "toggle highlight",
            );
        });
        return Ok(());
    }
    if matches!(command, InlineCommand::Clear) && !selection_needs_clear(session, start, end) {
        return Ok(());
    }
    let active = query_inline_state(session, selection, command)? == SelectionState::Active;
    run_edit_command(session, |session| {
        let cursor = session.text.cursor_at(start);
        cursor.set_position(end, MoveMode::KeepAnchor);
        if matches!(command, InlineCommand::Clear) {
            clear_highlight_range(session, start, end);
        }
        let format = match command {
            InlineCommand::Bold => TextFormat {
                font_bold: Some(!active),
                ..Default::default()
            },
            InlineCommand::Italic => TextFormat {
                font_italic: Some(!active),
                ..Default::default()
            },
            InlineCommand::Underline => TextFormat {
                font_underline: Some(!active),
                ..Default::default()
            },
            InlineCommand::Strikethrough => TextFormat {
                font_strikeout: Some(!active),
                ..Default::default()
            },
            InlineCommand::Highlight => unreachable!(),
            InlineCommand::Clear => TextFormat {
                font_bold: Some(false),
                font_italic: Some(false),
                font_underline: Some(false),
                font_strikeout: Some(false),
                background_color: Some(text_document::Color::rgba(0, 0, 0, 0)),
                clear_link: true,
                ..Default::default()
            },
        };
        must_apply(cursor.merge_char_format(&format), "apply inline format");
    });
    Ok(())
}

pub fn apply_block_command(
    session: &mut NativeEditorSession,
    selection: NSRange,
    command: BlockCommand,
) -> Result<(), EditorCodecError> {
    let text = session.text.to_addressable_text().map_err(model_error)?;
    let (start, end) = utf16_range(&text, selection)?;
    let toggle_active = list_command_is_active(session, start, end, command);
    if !toggle_active && block_command_is_noop(session, start, end, command) {
        return Ok(());
    }
    run_edit_command(session, |session| {
        let cursor = session.text.cursor_at(start);
        cursor.set_position(end, MoveMode::KeepAnchor);
        match command {
            BlockCommand::Paragraph => {
                must_apply(
                    cursor.set_block_format(&BlockFormat {
                        heading_level: Some(0),
                        marker: Some(MarkerType::NoMarker),
                        ..Default::default()
                    }),
                    "set paragraph format",
                );
                remove_lists_in_selection(session, start, end);
            }
            BlockCommand::Heading(level) => {
                let heading_level = match level {
                    HeadingLevel::One => 1,
                    HeadingLevel::Two => 2,
                    HeadingLevel::Three => 3,
                };
                must_apply(
                    cursor.set_block_format(&BlockFormat {
                        heading_level: Some(heading_level),
                        ..Default::default()
                    }),
                    "set heading format",
                );
            }
            BlockCommand::UnorderedList | BlockCommand::OrderedList | BlockCommand::Checklist => {
                if toggle_active {
                    remove_lists_in_selection(session, start, end);
                } else {
                    let style = match command {
                        BlockCommand::OrderedList => ListStyle::Decimal,
                        _ => ListStyle::Disc,
                    };
                    must_apply(cursor.create_list(style), "create list");
                    let marker = if matches!(command, BlockCommand::Checklist) {
                        MarkerType::Unchecked
                    } else {
                        MarkerType::NoMarker
                    };
                    must_apply(
                        cursor.set_block_format(&BlockFormat {
                            marker: Some(marker),
                            ..Default::default()
                        }),
                        "set list marker",
                    );
                }
            }
        }
    });
    Ok(())
}

fn remove_lists_in_selection(session: &NativeEditorSession, start: usize, end: usize) {
    let mut positions = Vec::new();
    for element in session.text.flow() {
        let FlowElement::Block(block) = element else {
            continue;
        };
        let snapshot = block.snapshot();
        let overlaps = if start == end {
            snapshot.position <= start && start <= snapshot.position + snapshot.length
        } else {
            snapshot.position < end && snapshot.position + snapshot.length >= start
        };
        if snapshot.list_info.is_some() && overlaps {
            positions.push(snapshot.position);
        }
    }
    for position in positions {
        let cursor = session.text.cursor_at(position);
        must_apply(
            cursor.remove_current_block_from_list(),
            "remove block from list",
        );
        must_apply(
            cursor.set_block_format(&BlockFormat {
                marker: Some(MarkerType::NoMarker),
                ..Default::default()
            }),
            "clear list marker",
        );
    }
}

pub fn apply_paragraph_command(
    session: &mut NativeEditorSession,
    selection: NSRange,
    command: ParagraphCommand,
) -> Result<(), EditorCodecError> {
    let text = session.text.to_addressable_text().map_err(model_error)?;
    let (start, end) = utf16_range(&text, selection)?;
    if paragraph_command_is_noop(session, start, end, command) {
        return Ok(());
    }
    let requested_format = match command {
        ParagraphCommand::Align(alignment) => BlockFormat {
            alignment: Some(td_alignment(alignment)),
            ..Default::default()
        },
        ParagraphCommand::IncreaseIndent | ParagraphCommand::DecreaseIndent => {
            let cursor = session.text.cursor_at(start);
            cursor.set_position(end, MoveMode::KeepAnchor);
            let current = cursor
                .block_format()
                .map_err(model_error)?
                .indent
                .unwrap_or(0);
            let indent = match command {
                ParagraphCommand::IncreaseIndent => current.saturating_add(1).min(8),
                ParagraphCommand::DecreaseIndent => current.saturating_sub(1),
                ParagraphCommand::Align(_) => unreachable!(),
            };
            BlockFormat {
                indent: Some(indent),
                ..Default::default()
            }
        }
    };
    run_edit_command(session, |session| {
        let cursor = session.text.cursor_at(start);
        cursor.set_position(end, MoveMode::KeepAnchor);
        must_apply(
            cursor.set_block_format(&requested_format),
            "set paragraph format",
        );
    });
    Ok(())
}

fn paragraph_command_is_noop(
    session: &NativeEditorSession,
    start: usize,
    end: usize,
    command: ParagraphCommand,
) -> bool {
    let mut found = false;
    for element in session.text.flow() {
        let FlowElement::Block(block) = element else {
            continue;
        };
        let snapshot = block.snapshot();
        let overlaps = if start == end {
            snapshot.position <= start && start <= snapshot.position + snapshot.length
        } else {
            snapshot.position < end && snapshot.position + snapshot.length > start
        };
        if !overlaps {
            continue;
        }
        found = true;
        let indent = snapshot.block_format.indent.unwrap_or(0);
        let matches = match command {
            ParagraphCommand::Align(alignment) => {
                html_alignment(snapshot.block_format.alignment) == alignment
            }
            ParagraphCommand::IncreaseIndent => indent >= 8,
            ParagraphCommand::DecreaseIndent => indent == 0,
        };
        if !matches {
            return false;
        }
    }
    found
}

pub fn toggle_checklist_at_utf16_location(
    session: &mut NativeEditorSession,
    location: usize,
) -> bool {
    let Ok(text) = session.text.to_addressable_text() else {
        return false;
    };
    let Ok((scalar, _)) = utf16_range(&text, NSRange::new(location, 0)) else {
        return false;
    };
    let cursor = session.text.cursor_at(scalar);
    let Ok(format) = cursor.block_format() else {
        return false;
    };
    let marker = match format.marker {
        Some(MarkerType::Checked) => MarkerType::Unchecked,
        Some(MarkerType::Unchecked) => MarkerType::Checked,
        _ => return false,
    };
    run_edit_command(session, |_session| {
        must_apply(
            cursor.set_block_format(&BlockFormat {
                marker: Some(marker),
                ..Default::default()
            }),
            "toggle checklist marker",
        );
    });
    true
}

pub fn query_inline_state(
    session: &NativeEditorSession,
    selection: NSRange,
    command: InlineCommand,
) -> Result<SelectionState, EditorCodecError> {
    let text = session.text.to_addressable_text().map_err(model_error)?;
    let (start, end) = utf16_range(&text, selection)?;
    if start == end {
        let format = session.effective_typing_format(start);
        let active = match command {
            InlineCommand::Bold => format.font_bold == Some(true),
            InlineCommand::Italic => format.font_italic == Some(true),
            InlineCommand::Underline => format.font_underline == Some(true),
            InlineCommand::Strikethrough => format.font_strikeout == Some(true),
            InlineCommand::Highlight => format
                .background_color
                .as_ref()
                .is_some_and(|color| color.alpha != 0),
            InlineCommand::Clear => false,
        };
        return Ok(if active {
            SelectionState::Active
        } else {
            SelectionState::Inactive
        });
    }
    if command == InlineCommand::Highlight {
        let values: Vec<bool> = (start..end)
            .map(|position| session.is_highlighted(position))
            .collect();
        if values.is_empty() || values.iter().all(|value| !*value) {
            return Ok(SelectionState::Inactive);
        }
        if values.iter().all(|value| *value) {
            return Ok(SelectionState::Active);
        }
        return Ok(SelectionState::Mixed);
    }
    let mut values = Vec::new();
    for element in session.text.flow() {
        let FlowElement::Block(block) = element else {
            continue;
        };
        let snapshot = block.snapshot();
        let block_start = snapshot.position;
        for fragment in snapshot.fragments {
            let FragmentContent::Text {
                offset,
                length,
                format,
                ..
            } = fragment
            else {
                continue;
            };
            let from = block_start + offset;
            let to = from + length;
            if from < end && to > start {
                let value = match command {
                    InlineCommand::Bold => format.font_bold == Some(true),
                    InlineCommand::Italic => format.font_italic == Some(true),
                    InlineCommand::Underline => format.font_underline == Some(true),
                    InlineCommand::Strikethrough => format.font_strikeout == Some(true),
                    InlineCommand::Highlight => format.background_color.is_some(),
                    InlineCommand::Clear => false,
                };
                values.push(value);
            }
        }
    }
    if values.is_empty() {
        return Ok(SelectionState::Inactive);
    }
    if values.iter().all(|value| *value) {
        Ok(SelectionState::Active)
    } else if values.iter().all(|value| !*value) {
        Ok(SelectionState::Inactive)
    } else {
        Ok(SelectionState::Mixed)
    }
}

pub fn query_link_selection(
    session: &NativeEditorSession,
    selection: NSRange,
) -> Result<LinkSelectionState, EditorCodecError> {
    let text = session.text.to_addressable_text().map_err(model_error)?;
    let (start, end) = utf16_range(&text, selection)?;
    if start == end {
        return Ok(LinkSelectionState {
            state: SelectionState::Inactive,
            has_linkable_text: false,
        });
    }
    let mut links = Vec::new();
    for element in session.text.flow() {
        let FlowElement::Block(block) = element else {
            continue;
        };
        let snapshot = block.snapshot();
        for fragment in snapshot.fragments {
            let FragmentContent::Text {
                text,
                offset,
                format,
                ..
            } = fragment
            else {
                continue;
            };
            for (index, character) in text.chars().enumerate() {
                if matches!(character, '\u{2028}' | '\u{000b}' | '\r') {
                    continue;
                }
                let position = snapshot.position + offset + index;
                if position >= start && position < end {
                    links.push(format.anchor_href.clone());
                }
            }
        }
    }
    if links.is_empty() || links.iter().all(Option::is_none) {
        return Ok(LinkSelectionState {
            state: SelectionState::Inactive,
            has_linkable_text: !links.is_empty(),
        });
    }
    let state = if links
        .iter()
        .all(|link| link.is_some() && link == links.first().unwrap())
    {
        SelectionState::Active
    } else {
        SelectionState::Mixed
    };
    Ok(LinkSelectionState {
        state,
        has_linkable_text: true,
    })
}

pub fn query_link_state(
    session: &NativeEditorSession,
    selection: NSRange,
) -> Result<SelectionState, EditorCodecError> {
    Ok(query_link_selection(session, selection)?.state)
}

fn style_for_text(format: &TextFormat, heading_level: Option<u8>) -> Retained<NSFont> {
    let size = match heading_level {
        Some(1) => 30.0,
        Some(2) => 24.0,
        Some(3) => 18.0,
        _ => 17.0,
    };
    let mut font = NSFont::systemFontOfSize(size);
    let descriptor = font.fontDescriptor();
    let mut traits = descriptor.symbolicTraits();
    if heading_level.is_some()
        || format.font_bold == Some(true)
        || format.font_weight.is_some_and(|weight| weight >= 600)
    {
        traits.insert(NSFontDescriptorSymbolicTraits::TraitBold);
    }
    if format.font_italic == Some(true) {
        traits.insert(NSFontDescriptorSymbolicTraits::TraitItalic);
    }
    if let Some(converted) =
        NSFont::fontWithDescriptor_size(&descriptor.fontDescriptorWithSymbolicTraits(traits), size)
    {
        font = converted;
    }
    font
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NativeListMarker {
    Ordered,
    Unordered,
    Checked,
    Unchecked,
}

fn list_marker_for_snapshot(snapshot: &text_document::BlockSnapshot) -> Option<NativeListMarker> {
    let list = snapshot.list_info.as_ref()?;
    Some(match snapshot.block_format.marker {
        Some(MarkerType::Checked) => NativeListMarker::Checked,
        Some(MarkerType::Unchecked) => NativeListMarker::Unchecked,
        _ if matches!(list.style, ListStyle::Decimal) => NativeListMarker::Ordered,
        _ => NativeListMarker::Unordered,
    })
}

fn new_native_list(marker_kind: NativeListMarker) -> Retained<NSTextList> {
    let marker = unsafe {
        match marker_kind {
            NativeListMarker::Checked => NSTextListMarkerCheck,
            NativeListMarker::Unchecked => NSTextListMarkerBox,
            NativeListMarker::Ordered => NSTextListMarkerDecimal,
            NativeListMarker::Unordered => NSTextListMarkerDisc,
        }
    };
    NSTextList::initWithMarkerFormat_options_startingItemNumber(
        NSTextList::alloc(),
        marker,
        NSTextListOptions::empty(),
        1,
    )
}

fn paragraph_style_for_snapshot(
    snapshot: &text_document::BlockSnapshot,
    list: Option<&NSTextList>,
    available_width: f64,
) -> Retained<NSMutableParagraphStyle> {
    let paragraph = NSMutableParagraphStyle::new();
    paragraph.setAlignment(match snapshot.block_format.alignment {
        Some(TdAlignment::Center) => NSTextAlignment::Center,
        Some(TdAlignment::Right) => NSTextAlignment::Right,
        Some(TdAlignment::Justify) => NSTextAlignment::Justified,
        _ => NSTextAlignment::Left,
    });
    paragraph.setLineSpacing(4.0);
    paragraph.setParagraphSpacing(6.0);
    paragraph.setHeadIndent(f64::from(snapshot.block_format.indent.unwrap_or(0)) * 24.0);
    if let Some(list) = list {
        let lists = objc2_foundation::NSArray::<NSTextList>::from_slice(&[list]);
        paragraph.setTextLists(&lists);
    }
    if snapshot
        .fragments
        .iter()
        .any(|fragment| matches!(fragment, FragmentContent::Image { .. }))
    {
        paragraph.setTailIndent(image_paragraph_tail_indent(available_width));
    }
    paragraph
}

fn apply_text_attributes(
    piece: &NSMutableAttributedString,
    format: &TextFormat,
    heading_level: Option<u8>,
    paragraph: &NSMutableParagraphStyle,
) {
    let range = NSRange::new(0, piece.string().length());
    if range.length == 0 {
        return;
    }
    let font = style_for_text(format, heading_level);
    unsafe {
        piece.addAttribute_value_range(NSFontAttributeName, &font, range);
        piece.addAttribute_value_range(NSParagraphStyleAttributeName, paragraph, range);
        if format.font_underline == Some(true) {
            let value = NSNumber::numberWithInteger(NSUnderlineStyle::Single.0);
            piece.addAttribute_value_range(NSUnderlineStyleAttributeName, &value, range);
        }
        if format.font_strikeout == Some(true) {
            let value = NSNumber::numberWithInteger(NSUnderlineStyle::Single.0);
            piece.addAttribute_value_range(NSStrikethroughStyleAttributeName, &value, range);
        }
        if let Some(color) = format.background_color {
            let value = NSColor::colorWithRed_green_blue_alpha(
                f64::from(color.red) / 255.0,
                f64::from(color.green) / 255.0,
                f64::from(color.blue) / 255.0,
                f64::from(color.alpha) / 255.0,
            );
            piece.addAttribute_value_range(NSBackgroundColorAttributeName, &value, range);
        }
        if let Some(href) = &format.anchor_href {
            let key = NSAttributedStringKey::from_str("NSLink");
            if let Some(value) = NSURL::initWithString(NSURL::alloc(), &NSString::from_str(href)) {
                piece.addAttribute_value_range(&key, &value, range);
            }
        }
    }
}

fn apply_highlight_overlay(
    piece: &NSMutableAttributedString,
    session: &NativeEditorSession,
    start: usize,
    text: &str,
) {
    let color = NSColor::systemYellowColor();
    let mut utf16_offset = 0usize;
    for (index, character) in text.chars().enumerate() {
        let utf16_length = character.len_utf16();
        if session.is_highlighted(start + index) {
            unsafe {
                piece.addAttribute_value_range(
                    NSBackgroundColorAttributeName,
                    &color,
                    NSRange::new(utf16_offset, utf16_length),
                );
            }
        }
        utf16_offset += utf16_length;
    }
}

fn downsampled_editor_image(bytes: &[u8], max_pixel_size: usize) -> Option<CFRetained<CGImage>> {
    if bytes.is_empty() || max_pixel_size == 0 {
        return None;
    }
    let data = CFData::from_bytes(bytes);
    let always: CFRetained<CFType> = CFBoolean::new(true).into();
    let transform: CFRetained<CFType> = CFBoolean::new(true).into();
    let max_size: CFRetained<CFType> = CFNumber::new_isize(max_pixel_size as isize).into();
    let keys: [&CFString; 3] = unsafe {
        [
            kCGImageSourceCreateThumbnailFromImageAlways,
            kCGImageSourceThumbnailMaxPixelSize,
            kCGImageSourceCreateThumbnailWithTransform,
        ]
    };
    let values: [&CFType; 3] = [always.as_ref(), max_size.as_ref(), transform.as_ref()];
    let options = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
    let options: &CFDictionary = unsafe { options.cast_unchecked() };
    let source = unsafe { CGImageSource::with_data(&data, Some(options)) }?;
    let image = unsafe { source.thumbnail_at_index(0, Some(options)) }?;
    let width = CGImage::width(Some(&image));
    let height = CGImage::height(Some(&image));
    (width > 0 && height > 0 && width <= max_pixel_size && height <= max_pixel_size)
        .then_some(image)
}

/// Build the bounded display representation used by NSTextAttachment.
///
/// The canonical resource bytes stay in the repository and are never replaced
/// by this projection.  Keeping the attachment on a bounded CGImage avoids
/// forcing AppKit to retain a full-resolution decoded bitmap for every image
/// in the editor.
pub fn editor_attachment_image(bytes: &[u8]) -> Option<Retained<NSImage>> {
    let image = downsampled_editor_image(bytes, INLINE_IMAGE_MAX_PIXEL_SIZE)?;
    let width = CGImage::width(Some(&image));
    let height = CGImage::height(Some(&image));
    Some(NSImage::initWithCGImage_size(
        NSImage::alloc(),
        &image,
        NSSize::new(width as f64, height as f64),
    ))
}

fn attachment_piece(
    resource: Option<&StoredResource>,
    name: &str,
    alt: &str,
    width: u32,
    height: u32,
    available_width: f64,
) -> Retained<NSMutableAttributedString> {
    let attachment = match resource {
        Some(resource) => {
            let attachment = NSTextAttachment::init(NSTextAttachment::alloc());
            if let Some(image) = editor_attachment_image(&resource.bytes) {
                attachment.setImage(Some(&image));
                let ratio = if image.size().width > 0.0 {
                    (available_width / image.size().width).min(1.0)
                } else {
                    1.0
                };
                attachment.setBounds(objc2_foundation::NSRect::new(
                    objc2_foundation::NSPoint::new(0.0, 0.0),
                    NSSize::new(image.size().width * ratio, image.size().height * ratio),
                ));
            }
            attachment
        }
        None => NSTextAttachment::init(NSTextAttachment::alloc()),
    };
    let attributed = NSAttributedString::attributedStringWithAttachment(&attachment);
    let piece = NSMutableAttributedString::from_attributed_nsstring(&attributed);
    let range = NSRange::new(0, 1);
    unsafe {
        piece.addAttribute_value_range(
            &NSString::from_str(RESOURCE_ID_KEY),
            &NSString::from_str(name),
            range,
        );
        piece.addAttribute_value_range(
            &NSString::from_str(RESOURCE_ALT_KEY),
            &NSString::from_str(alt),
            range,
        );
        piece.addAttribute_value_range(
            &NSString::from_str(RESOURCE_WIDTH_KEY),
            &NSNumber::numberWithUnsignedLongLong(width as u64),
            range,
        );
        piece.addAttribute_value_range(
            &NSString::from_str(RESOURCE_HEIGHT_KEY),
            &NSNumber::numberWithUnsignedLongLong(height as u64),
            range,
        );
        if resource.is_none() {
            piece.addAttribute_value_range(
                &NSString::from_str(MISSING_RESOURCE_KEY),
                &NSString::from_str("1"),
                range,
            );
        }
    }
    piece
}

pub fn render_session<F>(
    session: &NativeEditorSession,
    mut load: F,
    _width: f64,
) -> RenderedDocument
where
    F: FnMut(&str) -> Option<StoredResource>,
{
    let output = NSMutableAttributedString::from_nsstring(&NSString::from_str(""));
    let mut missing_resources = 0;
    let mut rendered_blocks = 0;
    let mut active_list: Option<(NativeListMarker, Retained<NSTextList>)> = None;
    let mut previous_paragraph: Option<Retained<NSMutableParagraphStyle>> = None;
    let mut pending_empty_block: Option<(usize, Retained<NSMutableParagraphStyle>)> = None;
    let mut empty_block_carriers = Vec::new();
    let mut attachments = Vec::new();
    for element in session.text.flow() {
        let FlowElement::Block(block) = element else {
            continue;
        };
        let snapshot = block.snapshot();
        let list = match list_marker_for_snapshot(&snapshot) {
            Some(marker_kind)
                if active_list
                    .as_ref()
                    .is_some_and(|(active_kind, _)| *active_kind == marker_kind) =>
            {
                active_list.as_ref().map(|(_, list)| list.clone())
            }
            Some(marker_kind) => {
                let list = new_native_list(marker_kind);
                active_list = Some((marker_kind, list.clone()));
                Some(list)
            }
            None => {
                active_list = None;
                None
            }
        };
        let paragraph = paragraph_style_for_snapshot(&snapshot, list.as_deref(), _width);
        if rendered_blocks > 0 {
            let separator_paragraph = pending_empty_block
                .take()
                .map(|(_, paragraph)| paragraph)
                .or_else(|| previous_paragraph.clone())
                .unwrap_or_else(|| paragraph.clone());
            let separator = NSMutableAttributedString::from_nsstring(&NSString::from_str("\n"));
            unsafe {
                separator.addAttribute_value_range(
                    NSParagraphStyleAttributeName,
                    &separator_paragraph,
                    NSRange::new(0, 1),
                );
            }
            output.appendAttributedString(&separator);
        }
        rendered_blocks += 1;
        if snapshot.fragments.is_empty() {
            pending_empty_block = Some((output.string().length(), paragraph.clone()));
        } else {
            pending_empty_block = None;
        }
        previous_paragraph = Some(paragraph.clone());
        let heading_level = snapshot
            .block_format
            .heading_level
            .filter(|level| *level > 0);
        for fragment in snapshot.fragments {
            match fragment {
                FragmentContent::Text {
                    text,
                    format,
                    offset,
                    ..
                } => {
                    let piece =
                        NSMutableAttributedString::from_nsstring(&NSString::from_str(&text));
                    apply_text_attributes(&piece, &format, heading_level, &paragraph);
                    apply_highlight_overlay(&piece, session, snapshot.position + offset, &text);
                    output.appendAttributedString(&piece);
                }
                FragmentContent::Image {
                    name,
                    alt,
                    width: image_width,
                    height: image_height,
                    ..
                } => {
                    attachments.push(RenderedAttachment {
                        addressable_offset: output.string().length(),
                        resource_id: name.clone(),
                    });
                    let resource = load(&name);
                    if resource.is_none() {
                        missing_resources += 1;
                    };
                    let piece = attachment_piece(
                        resource.as_ref(),
                        &name,
                        &alt,
                        image_width.max(1),
                        image_height.max(1),
                        _width.max(1.0),
                    );
                    unsafe {
                        piece.addAttribute_value_range(
                            NSParagraphStyleAttributeName,
                            &paragraph,
                            NSRange::new(0, 1),
                        );
                    }
                    output.appendAttributedString(&piece);
                }
                FragmentContent::FootnoteReference { marker, .. } => {
                    output.appendAttributedString(&NSMutableAttributedString::from_nsstring(
                        &NSString::from_str(&marker),
                    ));
                }
            }
        }
    }
    if let Some((addressable_offset, paragraph)) = pending_empty_block {
        empty_block_carriers.push(EmptyBlockCarrier {
            addressable_offset,
            paragraph,
        });
    }
    RenderedDocument {
        attributed: output,
        missing_resources,
        empty_block_carriers,
        attachments,
    }
}

fn valid_editor_link(value: &str) -> bool {
    if value.is_empty() || value.len() > 8 * 1024 || value.chars().any(char::is_control) {
        return false;
    }
    let target =
        strip_editor_prefix(value, "http://").or_else(|| strip_editor_prefix(value, "https://"));
    if let Some(target) = target {
        let authority = target.split(['/', '?', '#']).next().unwrap_or_default();
        let host_port = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        if host_port.is_empty() || host_port.chars().any(char::is_whitespace) {
            return false;
        }
        if host_port.starts_with('[') {
            let Some(close) = host_port.find(']') else {
                return false;
            };
            return close > 1
                && (host_port[close + 1..].is_empty()
                    || host_port[close + 1..]
                        .strip_prefix(':')
                        .is_some_and(|port| {
                            !port.is_empty()
                                && port.chars().all(|character| character.is_ascii_digit())
                        }));
        }
        let (host, port) = host_port
            .split_once(':')
            .map_or((host_port, None), |(host, port)| (host, Some(port)));
        return !host.is_empty()
            && port.is_none_or(|port| {
                !port.is_empty() && port.chars().all(|character| character.is_ascii_digit())
            });
    }
    let Some(target) = strip_editor_prefix(value, "mailto:") else {
        return false;
    };
    let mailbox = target.split(['?', '#']).next().unwrap_or_default();
    let Some((local, domain)) = mailbox.rsplit_once('@') else {
        return false;
    };
    !local.is_empty() && !domain.is_empty() && !mailbox.chars().any(char::is_whitespace)
}

fn strip_editor_prefix<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &value[prefix.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_app_kit::{
        NSBackgroundColorAttributeName, NSFontAttributeName, NSLayoutManager, NSParagraphStyle,
        NSStrikethroughStyleAttributeName, NSTextContainer, NSTextStorage,
        NSUnderlineStyleAttributeName,
    };
    use std::ptr::null_mut;

    fn note() -> Document {
        Document::from_blocks(vec![
            Block::Heading {
                level: HeadingLevel::Two,
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "标题😀".into(),
                    marks: Marks {
                        bold: true,
                        ..Default::default()
                    },
                }],
            },
            Block::List {
                kind: ListKind::Checklist,
                items: vec![ListItem {
                    checked: Some(true),
                    style: BlockStyle {
                        indent: 2,
                        ..Default::default()
                    },
                    inlines: vec![
                        Inline::Image {
                            resource_id: "res-1".into(),
                            alt: "图".into(),
                        },
                        Inline::Text {
                            text: " item".into(),
                            marks: Marks {
                                link: Some("https://example.com/a".into()),
                                ..Default::default()
                            },
                        },
                    ],
                }],
            },
        ])
    }

    #[test]
    fn explicit_adapter_preserves_semantics_without_html_exporter() {
        let session = session_from_document(&note()).unwrap();
        let round_trip = document_from_session(&session).unwrap();
        assert_eq!(round_trip, note());
        assert_eq!(
            session.text.to_addressable_text().unwrap(),
            "标题😀\n\u{fffc} item"
        );
    }

    #[test]
    fn red_block_image_insert_splits_paragraph_at_end_and_middle() {
        let mut at_end = session_from_document(&Document::from_blocks(vec![Block::Paragraph {
            style: BlockStyle::default(),
            inlines: vec![Inline::Text {
                text: "ab".into(),
                marks: Default::default(),
            }],
        }]))
        .unwrap();
        insert_image_block_anchor(
            &mut at_end,
            NSRange::new(2, 0),
            "0123456789abcdef0123456789abcdef",
            "A",
            1,
            1,
        )
        .unwrap();
        assert_eq!(
            document_from_session(&at_end).unwrap(),
            Document::from_blocks(vec![
                Block::Paragraph {
                    style: BlockStyle::default(),
                    inlines: vec![Inline::Text {
                        text: "ab".into(),
                        marks: Default::default(),
                    }],
                },
                Block::Paragraph {
                    style: BlockStyle::default(),
                    inlines: vec![Inline::Image {
                        resource_id: "0123456789abcdef0123456789abcdef".into(),
                        alt: "A".into(),
                    }],
                },
            ])
        );

        let mut in_middle = session_from_document(&Document::from_blocks(vec![Block::Paragraph {
            style: BlockStyle::default(),
            inlines: vec![Inline::Text {
                text: "ab".into(),
                marks: Default::default(),
            }],
        }]))
        .unwrap();
        insert_image_block_anchor(
            &mut in_middle,
            NSRange::new(1, 0),
            "0123456789abcdef0123456789abcdef",
            "A",
            1,
            1,
        )
        .unwrap();
        assert_eq!(
            document_from_session(&in_middle).unwrap(),
            Document::from_blocks(vec![
                Block::Paragraph {
                    style: BlockStyle::default(),
                    inlines: vec![Inline::Text {
                        text: "a".into(),
                        marks: Default::default(),
                    }],
                },
                Block::Paragraph {
                    style: BlockStyle::default(),
                    inlines: vec![Inline::Image {
                        resource_id: "0123456789abcdef0123456789abcdef".into(),
                        alt: "A".into(),
                    }],
                },
                Block::Paragraph {
                    style: BlockStyle::default(),
                    inlines: vec![Inline::Text {
                        text: "b".into(),
                        marks: Default::default(),
                    }],
                },
            ])
        );
    }

    #[test]
    fn red_block_image_insert_replaces_utf16_selection_and_undoes_as_one_command() {
        let document = Document::from_blocks(vec![Block::Paragraph {
            style: BlockStyle::default(),
            inlines: vec![Inline::Text {
                text: "A😀B尾".into(),
                marks: Default::default(),
            }],
        }]);
        let mut session = session_from_document(&document).unwrap();
        let revision_before = session.revision();
        insert_image_block_anchor(
            &mut session,
            NSRange::new(1, 3),
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "替换",
            1,
            1,
        )
        .unwrap();
        assert_eq!(
            document_from_session(&session).unwrap(),
            Document::from_blocks(vec![
                Block::Paragraph {
                    style: BlockStyle::default(),
                    inlines: vec![Inline::Text {
                        text: "A".into(),
                        marks: Default::default(),
                    }],
                },
                Block::Paragraph {
                    style: BlockStyle::default(),
                    inlines: vec![Inline::Image {
                        resource_id: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
                        alt: "替换".into(),
                    }],
                },
                Block::Paragraph {
                    style: BlockStyle::default(),
                    inlines: vec![Inline::Text {
                        text: "尾".into(),
                        marks: Default::default(),
                    }],
                },
            ])
        );
        assert_eq!(session.revision(), revision_before + 1);
        session.undo().unwrap();
        assert_eq!(document_from_session(&session).unwrap(), document);
    }

    #[test]
    fn red_block_image_insert_keeps_contiguous_order_after_reload() {
        let base = Document::from_blocks(vec![Block::Paragraph {
            style: BlockStyle::default(),
            inlines: vec![Inline::Text {
                text: "ab".into(),
                marks: Default::default(),
            }],
        }]);
        let mut live = session_from_document(&base).unwrap();
        insert_image_block_anchor(
            &mut live,
            NSRange::new(2, 0),
            "0123456789abcdef0123456789abcdef",
            "A",
            1,
            1,
        )
        .unwrap();
        let image_b_location = live.text.to_addressable_text().unwrap().chars().count();
        let mut candidate = session_from_document(&document_from_session(&live).unwrap()).unwrap();
        insert_image_block_anchor(
            &mut candidate,
            NSRange::new(image_b_location, 0),
            "fedcba9876543210fedcba9876543210",
            "B",
            1,
            1,
        )
        .unwrap();
        insert_image_block_anchor(
            &mut live,
            NSRange::new(image_b_location, 0),
            "fedcba9876543210fedcba9876543210",
            "B",
            1,
            1,
        )
        .unwrap();
        let persisted = document_from_session(&live).unwrap();
        assert_eq!(
            crate::html_body::serialize_html(&document_from_session(&candidate).unwrap()),
            crate::html_body::serialize_html(&persisted)
        );
        let reloaded = session_from_document(&persisted).unwrap();
        assert_eq!(document_from_session(&reloaded).unwrap(), persisted);
        assert_eq!(
            crate::html_body::resource_ids(&persisted),
            vec![
                "0123456789abcdef0123456789abcdef".to_string(),
                "fedcba9876543210fedcba9876543210".to_string(),
            ]
        );
    }

    #[test]
    fn rendered_image_anchor_is_emitted_once_even_when_inline_with_heading() {
        let document = Document::from_blocks(vec![Block::Heading {
            level: HeadingLevel::One,
            style: BlockStyle::default(),
            inlines: vec![
                Inline::Text {
                    text: "标题".into(),
                    marks: Marks::default(),
                },
                Inline::Image {
                    resource_id: "0123456789abcdef0123456789abcdef".into(),
                    alt: "图片".into(),
                },
            ],
        }]);
        let session = session_from_document(&document).unwrap();
        let rendered = render_session(&session, |_| None, 640.0);

        assert_eq!(
            rendered
                .attributed
                .string()
                .to_string()
                .chars()
                .filter(|character| *character == '\u{fffc}')
                .count(),
            1,
            "one semantic image must produce one native attachment"
        );
    }

    #[test]
    fn heading_image_is_a_full_line_before_following_text() {
        let document = Document::from_blocks(vec![Block::Heading {
            level: HeadingLevel::One,
            style: BlockStyle::default(),
            inlines: vec![
                Inline::Text {
                    text: "标题".into(),
                    marks: Marks::default(),
                },
                Inline::Image {
                    resource_id: "0123456789abcdef0123456789abcdef".into(),
                    alt: "图片".into(),
                },
                Inline::Text {
                    text: "后文".into(),
                    marks: Marks::default(),
                },
            ],
        }]);
        let session = session_from_document(&document).unwrap();
        let rendered = render_session(&session, |_| None, 640.0);
        let storage = NSTextStorage::new();
        let layout = NSLayoutManager::new();
        let container = NSTextContainer::initWithContainerSize(
            NSTextContainer::alloc(),
            NSSize::new(640.0, 1200.0),
        );
        storage.addLayoutManager(&layout);
        layout.addTextContainer(&container);
        container.setLineFragmentPadding(0.0);
        storage.setAttributedString(&rendered.attributed);
        layout.ensureLayoutForTextContainer(&container);

        let mut image_effective_range = NSRange::new(0, 0);
        let image_line = unsafe {
            layout.lineFragmentRectForGlyphAtIndex_effectiveRange(2, &mut image_effective_range)
        };
        let mut text_effective_range = NSRange::new(0, 0);
        let text_line = unsafe {
            layout.lineFragmentRectForGlyphAtIndex_effectiveRange(3, &mut text_effective_range)
        };
        let text_location = layout.locationForGlyphAtIndex(3);
        assert!(
            text_line.origin.y > image_line.origin.y,
            "heading image must not share a clipped heading line"
        );
        assert!(
            text_location.x <= 1.0,
            "text after heading image must restart at the left edge"
        );
    }

    #[test]
    fn consecutive_images_use_separate_full_lines() {
        let document = Document::from_blocks(vec![Block::Paragraph {
            style: BlockStyle::default(),
            inlines: vec![
                Inline::Text {
                    text: "前".into(),
                    marks: Marks::default(),
                },
                Inline::Image {
                    resource_id: "0123456789abcdef0123456789abcdef".into(),
                    alt: "一".into(),
                },
                Inline::Image {
                    resource_id: "fedcba9876543210fedcba9876543210".into(),
                    alt: "二".into(),
                },
                Inline::Text {
                    text: "后".into(),
                    marks: Marks::default(),
                },
            ],
        }]);
        let session = session_from_document(&document).unwrap();
        let rendered = render_session(&session, |_| None, 640.0);
        assert_eq!(
            rendered.attributed.string().to_string(),
            "前\n\u{fffc}\n\u{fffc}\n后"
        );

        let storage = NSTextStorage::new();
        let layout = NSLayoutManager::new();
        let container = NSTextContainer::initWithContainerSize(
            NSTextContainer::alloc(),
            NSSize::new(640.0, 1200.0),
        );
        storage.addLayoutManager(&layout);
        layout.addTextContainer(&container);
        container.setLineFragmentPadding(0.0);
        storage.setAttributedString(&rendered.attributed);
        layout.ensureLayoutForTextContainer(&container);
        let mut first_range = NSRange::new(0, 0);
        let first_line =
            unsafe { layout.lineFragmentRectForGlyphAtIndex_effectiveRange(2, &mut first_range) };
        let mut second_range = NSRange::new(0, 0);
        let second_line =
            unsafe { layout.lineFragmentRectForGlyphAtIndex_effectiveRange(4, &mut second_range) };
        let mut text_range = NSRange::new(0, 0);
        let text_line =
            unsafe { layout.lineFragmentRectForGlyphAtIndex_effectiveRange(6, &mut text_range) };
        assert!(second_line.origin.y > first_line.origin.y);
        assert!(text_line.origin.y > second_line.origin.y);
    }

    #[test]
    fn utf16_delta_rejects_surrogate_split_and_accepts_emoji() {
        let mut session = session_from_document(&Document::from_blocks(vec![Block::Paragraph {
            style: Default::default(),
            inlines: vec![Inline::Text {
                text: "A😀B".into(),
                marks: Default::default(),
            }],
        }]))
        .unwrap();
        assert_eq!(
            apply_committed_text_delta(&mut session, NSRange::new(2, 0), "x"),
            Err(EditorCodecError::InvalidUtf16Range)
        );
        apply_committed_text_delta(&mut session, NSRange::new(1, 2), "猫").unwrap();
        assert_eq!(session.text.to_addressable_text().unwrap(), "A猫B");
        apply_committed_text_delta(&mut session, NSRange::new(3, 0), "!").unwrap();
        assert_eq!(session.text.to_addressable_text().unwrap(), "A猫B!");
    }

    #[test]
    fn utf16_zero_offset_accepts_text_and_image_insertions() {
        let mut text_session =
            session_from_document(&Document::from_blocks(vec![Block::Paragraph {
                style: Default::default(),
                inlines: vec![Inline::Text {
                    text: "😀x".into(),
                    marks: Default::default(),
                }],
            }]))
            .unwrap();
        apply_committed_text_delta(&mut text_session, NSRange::new(0, 0), "前").unwrap();
        assert_eq!(text_session.text.to_addressable_text().unwrap(), "前😀x");

        let mut image_session =
            session_from_document(&Document::from_blocks(vec![Block::Paragraph {
                style: Default::default(),
                inlines: vec![Inline::Text {
                    text: "x".into(),
                    marks: Default::default(),
                }],
            }]))
            .unwrap();
        insert_image_anchor(
            &mut image_session,
            NSRange::new(0, 0),
            "image-at-zero",
            "图",
            1,
            1,
        )
        .unwrap();
        assert_eq!(
            image_session.text.to_addressable_text().unwrap(),
            "\u{fffc}x"
        );
    }

    #[test]
    fn no_op_and_failed_commands_do_not_consume_history_or_revision() {
        let mut session = session_from_document(&Document::from_blocks(vec![Block::Paragraph {
            style: Default::default(),
            inlines: vec![Inline::Text {
                text: "abc".into(),
                marks: Default::default(),
            }],
        }]))
        .unwrap();

        apply_inline_command(&mut session, NSRange::new(0, 3), InlineCommand::Highlight).unwrap();
        apply_inline_command(&mut session, NSRange::new(0, 3), InlineCommand::Clear).unwrap();
        let revision_after_clear = session.revision();
        apply_inline_command(&mut session, NSRange::new(0, 3), InlineCommand::Clear).unwrap();
        assert_eq!(session.revision(), revision_after_clear);
        session.undo().unwrap();
        assert_eq!(
            query_inline_state(&session, NSRange::new(0, 3), InlineCommand::Highlight).unwrap(),
            SelectionState::Active
        );

        let revision = session.revision();
        let can_undo = session.can_undo();
        let can_redo = session.can_redo();
        let failure = apply_link(
            &mut session,
            NSRange::new(0, 3),
            Some("javascript:alert(1)"),
        );
        assert_eq!(failure, Err(EditorCodecError::Unsupported));
        assert_eq!(session.revision(), revision);
        assert_eq!(session.can_undo(), can_undo);
        assert_eq!(session.can_redo(), can_redo);

        let before_text = session.text.to_addressable_text().unwrap();
        let partial =
            apply_block_command(&mut session, NSRange::new(99, 1), BlockCommand::Checklist);
        assert_eq!(partial, Err(EditorCodecError::InvalidUtf16Range));
        assert_eq!(session.text.to_addressable_text().unwrap(), before_text);
        assert_eq!(session.revision(), revision);
        assert_eq!(session.can_undo(), can_undo);
        assert_eq!(session.can_redo(), can_redo);
    }

    #[test]
    fn failed_command_preserves_redo_without_resurrecting_text() {
        let mut session = session_from_document(&Document::from_blocks(vec![Block::Paragraph {
            style: Default::default(),
            inlines: vec![Inline::Text {
                text: "a".into(),
                marks: Default::default(),
            }],
        }]))
        .unwrap();
        apply_committed_text_delta(&mut session, NSRange::new(1, 0), "b").unwrap();
        apply_committed_text_delta(&mut session, NSRange::new(2, 0), "c").unwrap();
        session.undo().unwrap();
        let before = session.text.to_addressable_text().unwrap();
        let revision = session.revision();
        let can_undo = session.can_undo();
        let can_redo = session.can_redo();
        let partial = apply_committed_text_delta(&mut session, NSRange::new(99, 0), "x");
        assert_eq!(partial, Err(EditorCodecError::InvalidUtf16Range));
        assert_eq!(session.text.to_addressable_text().unwrap(), before);
        assert_eq!(session.revision(), revision);
        assert_eq!(session.can_undo(), can_undo);
        assert_eq!(session.can_redo(), can_redo);
        session.redo().unwrap();
        assert_eq!(session.text.to_addressable_text().unwrap(), "abc");
    }

    #[test]
    fn empty_text_delta_is_not_a_visible_command() {
        let mut session = session_from_document(&Document::from_blocks(vec![Block::Paragraph {
            style: Default::default(),
            inlines: vec![Inline::Text {
                text: "abc".into(),
                marks: Default::default(),
            }],
        }]))
        .unwrap();
        apply_inline_command(&mut session, NSRange::new(0, 3), InlineCommand::Highlight).unwrap();
        let revision = session.revision();
        apply_committed_text_delta(&mut session, NSRange::new(0, 0), "").unwrap();
        assert_eq!(session.revision(), revision);
        session.undo().unwrap();
        assert_eq!(
            query_inline_state(&session, NSRange::new(0, 3), InlineCommand::Highlight).unwrap(),
            SelectionState::Inactive
        );
    }

    #[test]
    fn idempotent_format_commands_are_not_history_entries() {
        let linked = Document::from_blocks(vec![Block::Paragraph {
            style: Default::default(),
            inlines: vec![Inline::Text {
                text: "abc".into(),
                marks: Marks {
                    link: Some("https://example.com".into()),
                    ..Default::default()
                },
            }],
        }]);
        let mut session = session_from_document(&linked).unwrap();
        let revision = session.revision();
        assert!(!session.can_undo());
        apply_link(
            &mut session,
            NSRange::new(0, 3),
            Some("https://example.com"),
        )
        .unwrap();
        assert_eq!(session.revision(), revision);
        assert!(!session.can_undo());

        let heading = Document::from_blocks(vec![Block::Heading {
            level: HeadingLevel::Two,
            style: Default::default(),
            inlines: vec![Inline::Text {
                text: "heading".into(),
                marks: Default::default(),
            }],
        }]);
        let mut heading_session = session_from_document(&heading).unwrap();
        let revision = heading_session.revision();
        apply_block_command(
            &mut heading_session,
            NSRange::new(0, 7),
            BlockCommand::Heading(HeadingLevel::Two),
        )
        .unwrap();
        assert_eq!(heading_session.revision(), revision);
        assert!(!heading_session.can_undo());

        let mut aligned = session_from_document(&Document::from_blocks(vec![Block::Paragraph {
            style: Default::default(),
            inlines: vec![Inline::Text {
                text: "aligned".into(),
                marks: Default::default(),
            }],
        }]))
        .unwrap();
        let revision = aligned.revision();
        apply_paragraph_command(
            &mut aligned,
            NSRange::new(0, 7),
            ParagraphCommand::Align(Alignment::Left),
        )
        .unwrap();
        assert_eq!(aligned.revision(), revision);
        assert!(!aligned.can_undo());

        let mut bounded = session_from_document(&Document::from_blocks(vec![Block::Paragraph {
            style: BlockStyle {
                indent: 8,
                ..Default::default()
            },
            inlines: vec![Inline::Text {
                text: "bound".into(),
                marks: Default::default(),
            }],
        }]))
        .unwrap();
        let revision = bounded.revision();
        apply_paragraph_command(
            &mut bounded,
            NSRange::new(0, 5),
            ParagraphCommand::IncreaseIndent,
        )
        .unwrap();
        assert_eq!(bounded.revision(), revision);
        assert!(!bounded.can_undo());
    }

    #[test]
    fn repeated_same_link_does_not_evict_real_history_at_cap() {
        let document = Document::from_blocks(vec![Block::Paragraph {
            style: Default::default(),
            inlines: vec![Inline::Text {
                text: "a".into(),
                marks: Default::default(),
            }],
        }]);
        let mut session = session_from_document(&document).unwrap();
        apply_link(
            &mut session,
            NSRange::new(0, 1),
            Some("https://example.com"),
        )
        .unwrap();
        for position in 0..199 {
            let end = position + 1;
            apply_committed_text_delta(&mut session, NSRange::new(end, 0), "x").unwrap();
        }
        apply_link(
            &mut session,
            NSRange::new(0, 1),
            Some("https://example.com"),
        )
        .unwrap();
        for _ in 0..200 {
            session.undo().unwrap();
        }
        let restored = document_from_session(&session).unwrap();
        assert!(matches!(
            &restored.blocks[0],
            Block::Paragraph { inlines, .. }
                if inlines.iter().all(|inline| matches!(
                    inline,
                    Inline::Text { marks, .. } if marks.link.is_none()
                ))
        ));
    }

    #[test]
    fn list_item_indents_round_trip_independently() {
        let document = Document::from_blocks(vec![Block::List {
            kind: ListKind::Ordered,
            items: vec![
                ListItem {
                    checked: None,
                    style: BlockStyle::default(),
                    inlines: vec![Inline::Text {
                        text: "one".into(),
                        marks: Marks::default(),
                    }],
                },
                ListItem {
                    checked: None,
                    style: BlockStyle {
                        indent: 2,
                        ..Default::default()
                    },
                    inlines: vec![Inline::Text {
                        text: "nested".into(),
                        marks: Marks::default(),
                    }],
                },
                ListItem {
                    checked: None,
                    style: BlockStyle::default(),
                    inlines: vec![Inline::Text {
                        text: "two".into(),
                        marks: Marks::default(),
                    }],
                },
            ],
        }]);
        let session = session_from_document(&document).unwrap();
        assert_eq!(document_from_session(&session).unwrap(), document);
    }

    #[test]
    fn active_list_commands_toggle_back_to_paragraph_and_undo_redo() {
        for (kind, command) in [
            (ListKind::Unordered, BlockCommand::UnorderedList),
            (ListKind::Ordered, BlockCommand::OrderedList),
            (ListKind::Checklist, BlockCommand::Checklist),
        ] {
            let document = Document::from_blocks(vec![Block::List {
                kind,
                items: vec![ListItem {
                    checked: (kind == ListKind::Checklist).then_some(true),
                    style: Default::default(),
                    inlines: vec![Inline::Text {
                        text: "item".into(),
                        marks: Default::default(),
                    }],
                }],
            }]);
            let mut session = session_from_document(&document).unwrap();
            apply_block_command(&mut session, NSRange::new(0, 4), command).unwrap();
            assert!(matches!(
                document_from_session(&session).unwrap().blocks.as_slice(),
                [Block::Paragraph { .. }]
            ));
            assert!(session.can_undo());
            session.undo().unwrap();
            assert_eq!(document_from_session(&session).unwrap(), document);
            assert!(session.can_redo());
            session.redo().unwrap();
            assert!(matches!(
                document_from_session(&session).unwrap().blocks.as_slice(),
                [Block::Paragraph { .. }]
            ));
        }
    }

    #[test]
    fn formatted_selection_then_collapsed_list_toggle_exits_each_list_kind() {
        for command in [
            BlockCommand::UnorderedList,
            BlockCommand::OrderedList,
            BlockCommand::Checklist,
        ] {
            let mut session =
                session_from_document(&Document::from_blocks(vec![Block::Paragraph {
                    style: Default::default(),
                    inlines: vec![Inline::Text {
                        text: "abc".into(),
                        marks: Default::default(),
                    }],
                }]))
                .unwrap();
            for inline in [
                InlineCommand::Italic,
                InlineCommand::Underline,
                InlineCommand::Highlight,
                InlineCommand::Strikethrough,
            ] {
                apply_inline_command(&mut session, NSRange::new(0, 3), inline).unwrap();
            }
            apply_block_command(&mut session, NSRange::new(0, 0), command).unwrap();
            assert!(matches!(
                document_from_session(&session).unwrap().blocks.as_slice(),
                [Block::List { .. }]
            ));
            apply_block_command(&mut session, NSRange::new(0, 0), command).unwrap();
            assert!(matches!(
                document_from_session(&session).unwrap().blocks.as_slice(),
                [Block::Paragraph { .. }]
            ));
        }
    }

    #[test]
    fn switching_checklist_to_unordered_clears_checked_marker() {
        let document = Document::from_blocks(vec![Block::List {
            kind: ListKind::Checklist,
            items: vec![ListItem {
                checked: Some(true),
                style: Default::default(),
                inlines: vec![Inline::Text {
                    text: "item".into(),
                    marks: Default::default(),
                }],
            }],
        }]);
        let mut session = session_from_document(&document).unwrap();
        apply_block_command(
            &mut session,
            NSRange::new(0, 4),
            BlockCommand::UnorderedList,
        )
        .unwrap();
        assert!(matches!(
            document_from_session(&session).unwrap().blocks.as_slice(),
            [Block::List {
                kind: ListKind::Unordered,
                items,
            }] if items[0].checked.is_none()
        ));
    }

    #[test]
    fn link_and_checklist_commands_are_undoable_by_one_owner() {
        let mut session = session_from_document(&Document::from_blocks(vec![Block::Paragraph {
            style: Default::default(),
            inlines: vec![Inline::Text {
                text: "abc".into(),
                marks: Default::default(),
            }],
        }]))
        .unwrap();
        apply_link(
            &mut session,
            NSRange::new(0, 3),
            Some("https://example.com"),
        )
        .unwrap();
        assert!(session.can_undo());
        session.undo().unwrap();
        let doc = document_from_session(&session).unwrap();
        assert_eq!(crate::html_body::search_text(&doc), "abc");
    }

    #[test]
    fn commands_validate_links_and_toggle_marks_without_touching_text() {
        let mut session = session_from_document(&Document::from_blocks(vec![Block::Paragraph {
            style: Default::default(),
            inlines: vec![Inline::Text {
                text: "abc".into(),
                marks: Default::default(),
            }],
        }]))
        .unwrap();
        assert_eq!(
            apply_link(
                &mut session,
                NSRange::new(0, 3),
                Some("javascript:alert(1)")
            ),
            Err(EditorCodecError::Unsupported)
        );
        apply_inline_command(&mut session, NSRange::new(0, 3), InlineCommand::Bold).unwrap();
        assert_eq!(
            query_inline_state(&session, NSRange::new(0, 3), InlineCommand::Bold).unwrap(),
            SelectionState::Active
        );
        apply_inline_command(&mut session, NSRange::new(0, 3), InlineCommand::Bold).unwrap();
        assert_eq!(
            query_inline_state(&session, NSRange::new(0, 3), InlineCommand::Bold).unwrap(),
            SelectionState::Inactive
        );
        assert_eq!(session.text.to_addressable_text().unwrap(), "abc");
    }

    #[test]
    fn red_soft_break_is_not_linkable_and_linking_it_is_a_noop() {
        let document = Document::from_blocks(vec![Block::Paragraph {
            style: BlockStyle::default(),
            inlines: vec![
                Inline::Text {
                    text: "a".into(),
                    marks: Marks::default(),
                },
                Inline::SoftBreak,
                Inline::Text {
                    text: "b".into(),
                    marks: Marks::default(),
                },
            ],
        }]);
        let mut session = session_from_document(&document).unwrap();
        assert_eq!(
            query_link_selection(&session, NSRange::new(1, 1)).unwrap(),
            LinkSelectionState {
                state: SelectionState::Inactive,
                has_linkable_text: false,
            }
        );
        let revision = session.revision();
        let can_undo = session.can_undo();
        apply_link(
            &mut session,
            NSRange::new(1, 1),
            Some("https://example.com"),
        )
        .unwrap();
        assert_eq!(session.revision(), revision);
        assert_eq!(session.can_undo(), can_undo);

        let mixed = query_link_selection(&session, NSRange::new(0, 3)).unwrap();
        assert!(mixed.has_linkable_text);
        assert_eq!(mixed.state, SelectionState::Inactive);
    }

    #[test]
    fn collapsed_inline_format_is_persisted_for_the_next_insert() {
        let document = Document::from_blocks(vec![Block::Paragraph {
            style: Default::default(),
            inlines: vec![Inline::Text {
                text: "abc".into(),
                marks: Marks::default(),
            }],
        }]);
        let mut session = session_from_document(&document).unwrap();
        apply_inline_command(&mut session, NSRange::new(1, 0), InlineCommand::Bold).unwrap();
        assert_eq!(
            query_inline_state(&session, NSRange::new(1, 0), InlineCommand::Bold).unwrap(),
            SelectionState::Active
        );
        assert!(session.can_undo());
        session.undo().unwrap();
        assert_eq!(
            query_inline_state(&session, NSRange::new(1, 0), InlineCommand::Bold).unwrap(),
            SelectionState::Inactive
        );
        session.redo().unwrap();
        apply_committed_text_delta(&mut session, NSRange::new(1, 0), "X").unwrap();
        let persisted = document_from_session(&session).unwrap();
        assert!(matches!(
            &persisted.blocks[0],
            Block::Paragraph { inlines, .. }
                if inlines.iter().any(|inline| matches!(
                    inline,
                    Inline::Text { text, marks } if text == "X" && marks.bold
                ))
        ));
        let reloaded = session_from_document(&persisted).unwrap();
        assert_eq!(document_from_session(&reloaded).unwrap(), persisted);
    }

    #[test]
    fn collapsed_clear_format_is_persisted_for_the_next_insert() {
        let document = Document::from_blocks(vec![Block::Paragraph {
            style: Default::default(),
            inlines: vec![Inline::Text {
                text: "abc".into(),
                marks: Marks::default(),
            }],
        }]);
        let mut session = session_from_document(&document).unwrap();
        apply_inline_command(&mut session, NSRange::new(1, 0), InlineCommand::Bold).unwrap();
        apply_inline_command(&mut session, NSRange::new(1, 0), InlineCommand::Clear).unwrap();
        assert_eq!(
            query_inline_state(&session, NSRange::new(1, 0), InlineCommand::Bold).unwrap(),
            SelectionState::Inactive
        );
        apply_committed_text_delta(&mut session, NSRange::new(1, 0), "X").unwrap();
        let persisted = document_from_session(&session).unwrap();
        assert!(matches!(
            &persisted.blocks[0],
            Block::Paragraph { inlines, .. }
                if inlines.iter().any(|inline| matches!(
                    inline,
                    Inline::Text { text, marks } if text == "aXbc" && !marks.bold
                ))
        ));
        let reloaded = session_from_document(&persisted).unwrap();
        assert_eq!(document_from_session(&reloaded).unwrap(), persisted);
    }

    #[test]
    fn collapsed_caret_context_follows_semantic_boundaries_and_overrides() {
        let bold = Marks {
            bold: true,
            ..Default::default()
        };
        let document = Document::from_blocks(vec![
            Block::Paragraph {
                style: Default::default(),
                inlines: vec![Inline::Text {
                    text: "😀bold".into(),
                    marks: bold,
                }],
            },
            Block::Paragraph {
                style: Default::default(),
                inlines: vec![Inline::Text {
                    text: "plain".into(),
                    marks: Marks::default(),
                }],
            },
        ]);
        let mut session = session_from_document(&document).unwrap();
        assert_eq!(
            query_inline_state(&session, NSRange::new(0, 0), InlineCommand::Bold).unwrap(),
            SelectionState::Active
        );
        assert_eq!(
            query_inline_state(&session, NSRange::new(2, 0), InlineCommand::Bold).unwrap(),
            SelectionState::Active
        );
        assert_eq!(
            query_inline_state(&session, NSRange::new(7, 0), InlineCommand::Bold).unwrap(),
            SelectionState::Inactive
        );
        apply_inline_command(&mut session, NSRange::new(7, 0), InlineCommand::Bold).unwrap();
        assert_eq!(
            query_inline_state(&session, NSRange::new(7, 0), InlineCommand::Bold).unwrap(),
            SelectionState::Active
        );
        apply_committed_text_delta(&mut session, NSRange::new(7, 0), "X").unwrap();
        assert_eq!(
            query_inline_state(&session, NSRange::new(13, 0), InlineCommand::Bold).unwrap(),
            SelectionState::Inactive
        );
        session.undo().unwrap();
        assert_eq!(
            query_inline_state(&session, NSRange::new(7, 0), InlineCommand::Bold).unwrap(),
            SelectionState::Active
        );
        session.undo().unwrap();
        assert_eq!(
            query_inline_state(&session, NSRange::new(7, 0), InlineCommand::Bold).unwrap(),
            SelectionState::Inactive
        );
        session.redo().unwrap();
        assert_eq!(
            query_inline_state(&session, NSRange::new(7, 0), InlineCommand::Bold).unwrap(),
            SelectionState::Active
        );
    }

    #[test]
    fn renderer_underlying_text_matches_addressable_text_without_projection_prefixes() {
        let session = session_from_document(&note()).unwrap();
        let expected = session.text.to_addressable_text().unwrap();
        let rendered = render_session(&session, |_| None, 640.0);

        assert_eq!(rendered.attributed.string().to_string(), expected);
        assert_eq!(
            rendered.attributed.string().to_string(),
            "标题😀\n\u{fffc} item"
        );
        assert_eq!(rendered.missing_resources, 1);
    }

    #[test]
    fn renderer_separates_list_items_with_native_text_list_markers() {
        let document = Document::from_blocks(vec![Block::List {
            kind: ListKind::Unordered,
            items: vec![
                ListItem {
                    checked: None,
                    style: BlockStyle {
                        indent: 2,
                        ..BlockStyle::default()
                    },
                    inlines: vec![Inline::Text {
                        text: "one".into(),
                        marks: Marks::default(),
                    }],
                },
                ListItem {
                    checked: None,
                    style: BlockStyle::default(),
                    inlines: vec![Inline::Text {
                        text: "two".into(),
                        marks: Marks::default(),
                    }],
                },
            ],
        }]);
        let session = session_from_document(&document).unwrap();
        let rendered = render_session(&session, |_| None, 640.0);
        assert_eq!(
            rendered.attributed.string().to_string(),
            session.text.to_addressable_text().unwrap()
        );
        assert_eq!(rendered.attributed.string().to_string(), "one\ntwo");
        let source: &NSAttributedString = &rendered.attributed;
        let key = unsafe { NSParagraphStyleAttributeName };
        for location in [0, 4] {
            let value = unsafe {
                source
                    .attribute_atIndex_effectiveRange(key, location, null_mut())
                    .unwrap()
            };
            let style = value.downcast_ref::<NSParagraphStyle>().unwrap();
            assert_eq!(style.textLists().count(), 1);
        }
    }

    #[test]
    fn renderer_distinguishes_heading_levels() {
        let document = Document::from_blocks(vec![
            Block::Heading {
                level: HeadingLevel::One,
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "H1".into(),
                    marks: Marks::default(),
                }],
            },
            Block::Heading {
                level: HeadingLevel::Two,
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "H2".into(),
                    marks: Marks::default(),
                }],
            },
            Block::Heading {
                level: HeadingLevel::Three,
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "H3".into(),
                    marks: Marks::default(),
                }],
            },
        ]);
        let session = session_from_document(&document).unwrap();
        let rendered = render_session(&session, |_| None, 640.0);
        let source: &NSAttributedString = &rendered.attributed;

        let h1 = unsafe {
            source
                .attribute_atIndex_effectiveRange(NSFontAttributeName, 0, null_mut())
                .unwrap()
        };
        let h2 = unsafe {
            source
                .attribute_atIndex_effectiveRange(NSFontAttributeName, 3, null_mut())
                .unwrap()
        };
        let h3 = unsafe {
            source
                .attribute_atIndex_effectiveRange(NSFontAttributeName, 6, null_mut())
                .unwrap()
        };
        let h1 = h1.downcast_ref::<NSFont>().unwrap().pointSize();
        let h2 = h2.downcast_ref::<NSFont>().unwrap().pointSize();
        let h3 = h3.downcast_ref::<NSFont>().unwrap().pointSize();
        assert_eq!((h1, h2, h3), (30.0, 24.0, 18.0));
        assert!(h1 > h2 && h2 > h3);
    }

    #[test]
    fn renderer_projects_all_inline_marks() {
        let document = Document::from_blocks(vec![Block::Paragraph {
            style: BlockStyle::default(),
            inlines: vec![Inline::Text {
                text: "marks".into(),
                marks: Marks {
                    bold: true,
                    italic: true,
                    underline: true,
                    strikethrough: true,
                    highlight: true,
                    ..Marks::default()
                },
            }],
        }]);
        let session = session_from_document(&document).unwrap();
        let rendered = render_session(&session, |_| None, 640.0);
        let source: &NSAttributedString = &rendered.attributed;
        let marks_offset = 0;
        let mark_keys = unsafe {
            [
                ("font", NSFontAttributeName),
                ("underline", NSUnderlineStyleAttributeName),
                ("strike", NSStrikethroughStyleAttributeName),
                ("highlight", NSBackgroundColorAttributeName),
            ]
        };
        for (label, key) in mark_keys {
            assert!(
                unsafe {
                    source
                        .attribute_atIndex_effectiveRange(key, marks_offset, null_mut())
                        .is_some()
                },
                "missing {label} projection"
            );
        }
    }

    #[test]
    fn highlight_sidecar_round_trips_and_shares_text_document_undo_owner() {
        let document = Document::from_blocks(vec![Block::Paragraph {
            style: BlockStyle::default(),
            inlines: vec![Inline::Text {
                text: "marked".into(),
                marks: Marks {
                    highlight: true,
                    ..Marks::default()
                },
            }],
        }]);
        let mut session = session_from_document(&document).unwrap();
        assert_eq!(document_from_session(&session).unwrap(), document);
        apply_inline_command(&mut session, NSRange::new(0, 6), InlineCommand::Highlight).unwrap();
        assert!(!document_from_session(&session)
            .unwrap()
            .blocks
            .iter()
            .any(|block| matches!(block, Block::Paragraph { inlines, .. } if inlines.iter().any(|inline| matches!(inline, Inline::Text { marks, .. } if marks.highlight)))));
        session.undo().unwrap();
        assert_eq!(document_from_session(&session).unwrap(), document);
        session.redo().unwrap();
        assert!(!document_from_session(&session)
            .unwrap()
            .blocks
            .iter()
            .any(|block| matches!(block, Block::Paragraph { inlines, .. } if inlines.iter().any(|inline| matches!(inline, Inline::Text { marks, .. } if marks.highlight)))));
    }

    #[test]
    fn second_round_history_is_empty_on_load_and_one_record_per_mixed_command() {
        let document = Document::from_blocks(vec![Block::Paragraph {
            style: BlockStyle::default(),
            inlines: vec![Inline::Text {
                text: "abc".into(),
                marks: Marks::default(),
            }],
        }]);
        let mut session = session_from_document(&document).unwrap();
        assert!(
            !session.can_undo(),
            "load must not manufacture undo history"
        );

        apply_committed_text_delta(&mut session, NSRange::new(2, 1), "!").unwrap();
        apply_inline_command(&mut session, NSRange::new(0, 3), InlineCommand::Highlight).unwrap();
        apply_block_command(
            &mut session,
            NSRange::new(0, 3),
            BlockCommand::Heading(HeadingLevel::Two),
        )
        .unwrap();
        assert!(session.can_undo());

        session.undo().unwrap();
        assert!(matches!(
            document_from_session(&session).unwrap().blocks[0],
            Block::Paragraph { .. }
        ));
        session.undo().unwrap();
        assert!(!document_from_session(&session)
            .unwrap()
            .blocks
            .iter()
            .any(|block| matches!(block, Block::Paragraph { inlines, .. } if inlines.iter().any(|inline| matches!(inline, Inline::Text { marks, .. } if marks.highlight)))));
        session.undo().unwrap();
        assert_eq!(
            crate::html_body::search_text(&document_from_session(&session).unwrap()),
            "abc"
        );
        assert!(!session.can_undo());
        assert!(session.can_redo());
    }

    #[test]
    fn renderer_reuses_ordered_list_and_keeps_empty_list_carrier_without_text() {
        let ordered = Document::from_blocks(vec![Block::List {
            kind: ListKind::Ordered,
            items: vec![
                ListItem {
                    checked: None,
                    style: BlockStyle::default(),
                    inlines: vec![Inline::Text {
                        text: "one".into(),
                        marks: Marks::default(),
                    }],
                },
                ListItem {
                    checked: None,
                    style: BlockStyle::default(),
                    inlines: vec![Inline::Text {
                        text: "two".into(),
                        marks: Marks::default(),
                    }],
                },
            ],
        }]);
        let rendered = render_session(&session_from_document(&ordered).unwrap(), |_| None, 640.0);
        let source: &NSAttributedString = &rendered.attributed;
        let first = unsafe {
            source
                .attribute_atIndex_effectiveRange(NSParagraphStyleAttributeName, 0, null_mut())
                .unwrap()
                .downcast_ref::<NSParagraphStyle>()
                .unwrap()
                .textLists()
                .objectAtIndex(0)
        };
        let second = unsafe {
            source
                .attribute_atIndex_effectiveRange(NSParagraphStyleAttributeName, 4, null_mut())
                .unwrap()
                .downcast_ref::<NSParagraphStyle>()
                .unwrap()
                .textLists()
                .objectAtIndex(0)
        };
        assert_eq!(Retained::as_ptr(&first), Retained::as_ptr(&second));
        assert_eq!(first.markerForItemNumber(1).to_string(), "1");
        assert_eq!(first.markerForItemNumber(2).to_string(), "2");

        let empty = Document::from_blocks(vec![Block::List {
            kind: ListKind::Checklist,
            items: vec![ListItem {
                checked: Some(false),
                style: BlockStyle::default(),
                inlines: Vec::new(),
            }],
        }]);
        let empty_session = session_from_document(&empty).unwrap();
        let empty_rendered = render_session(&empty_session, |_| None, 640.0);
        assert_eq!(
            empty_rendered.attributed.string().to_string(),
            empty_session.text.to_addressable_text().unwrap()
        );
        assert_eq!(empty_rendered.attributed.string().length(), 0);
        assert_eq!(empty_rendered.empty_block_carriers.len(), 1);
        assert_eq!(
            empty_rendered.empty_block_carriers[0]
                .paragraph
                .textLists()
                .count(),
            1
        );
    }

    #[test]
    fn renderer_uses_newline_as_carrier_for_middle_empty_block() {
        let document = Document::from_blocks(vec![
            Block::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "a".into(),
                    marks: Marks::default(),
                }],
            },
            Block::Paragraph {
                style: BlockStyle {
                    indent: 2,
                    ..BlockStyle::default()
                },
                inlines: Vec::new(),
            },
            Block::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "b".into(),
                    marks: Marks::default(),
                }],
            },
        ]);
        let session = session_from_document(&document).unwrap();
        let rendered = render_session(&session, |_| None, 640.0);
        assert_eq!(rendered.attributed.string().to_string(), "a\n\nb");
        assert!(rendered.empty_block_carriers.is_empty());
        let source: &NSAttributedString = &rendered.attributed;
        let value = unsafe {
            source
                .attribute_atIndex_effectiveRange(NSParagraphStyleAttributeName, 2, null_mut())
                .unwrap()
        };
        let paragraph = value.downcast_ref::<NSParagraphStyle>().unwrap();
        assert_eq!(paragraph.headIndent(), 48.0);
    }

    #[test]
    fn renderer_keeps_empty_paragraph_and_heading_without_fake_text() {
        let document = Document::from_blocks(vec![
            Block::Paragraph {
                style: BlockStyle::default(),
                inlines: Vec::new(),
            },
            Block::Heading {
                level: HeadingLevel::One,
                style: BlockStyle::default(),
                inlines: Vec::new(),
            },
        ]);
        let session = session_from_document(&document).unwrap();
        let rendered = render_session(&session, |_| None, 640.0);
        assert_eq!(rendered.attributed.string().to_string(), "\n");
        assert_eq!(rendered.empty_block_carriers.len(), 1);
        assert_eq!(rendered.empty_block_carriers[0].addressable_offset, 1);
    }

    #[test]
    fn list_indent_commands_round_trip_through_list_format() {
        let document = Document::from_blocks(vec![Block::List {
            kind: ListKind::Unordered,
            items: vec![ListItem {
                checked: None,
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "item".into(),
                    marks: Marks::default(),
                }],
            }],
        }]);
        let mut session = session_from_document(&document).unwrap();
        apply_paragraph_command(
            &mut session,
            NSRange::new(0, 4),
            ParagraphCommand::IncreaseIndent,
        )
        .unwrap();
        let increased = document_from_session(&session).unwrap();
        let indent = match &increased.blocks[0] {
            Block::List { items, .. } => items[0].style.indent,
            _ => panic!("expected list"),
        };
        assert_eq!(indent, 1);
        let reloaded = session_from_document(&increased).unwrap();
        assert_eq!(document_from_session(&reloaded).unwrap(), increased);
        let mut reloaded = reloaded;
        apply_paragraph_command(
            &mut reloaded,
            NSRange::new(0, 4),
            ParagraphCommand::DecreaseIndent,
        )
        .unwrap();
        let decreased = document_from_session(&reloaded).unwrap();
        let indent = match &decreased.blocks[0] {
            Block::List { items, .. } => items[0].style.indent,
            _ => panic!("expected list"),
        };
        assert_eq!(indent, 0);
    }

    #[test]
    fn explicit_image_delete_updates_canonical_model_and_undo_restores_anchor() {
        let mut session = session_from_document(&note()).unwrap();
        delete_image_anchor(&mut session, NSRange::new(5, 1)).unwrap();
        assert_eq!(session.text.to_addressable_text().unwrap(), "标题😀\n item");
        assert!(
            !crate::html_body::resource_ids(&document_from_session(&session).unwrap())
                .contains(&"res-1".to_string())
        );
        session.undo().unwrap();
        assert_eq!(
            session.text.to_addressable_text().unwrap(),
            "标题😀\n\u{fffc} item"
        );
        assert!(matches!(
            &document_from_session(&session).unwrap().blocks[1],
            Block::List { items, .. }
                if items.iter().any(|item| item.inlines.iter().any(|inline| matches!(
                    inline,
                    Inline::Image { resource_id, .. } if resource_id == "res-1"
                )))
        ));
    }

    #[test]
    fn adjacent_image_deletion_uses_resource_identity_and_undo() {
        let document = Document::from_blocks(vec![Block::Paragraph {
            style: BlockStyle::default(),
            inlines: vec![
                Inline::Image {
                    resource_id: "image-a".into(),
                    alt: "A".into(),
                },
                Inline::Image {
                    resource_id: "image-b".into(),
                    alt: "B".into(),
                },
            ],
        }]);
        let mut first = session_from_document(&document).unwrap();
        assert_eq!(
            delete_image_anchor_if_identity(&mut first, NSRange::new(0, 1), Some("image-b")),
            Err(EditorCodecError::Unsupported)
        );
        delete_image_anchor_if_identity(&mut first, NSRange::new(0, 1), Some("image-a")).unwrap();
        assert_eq!(
            image_ids(&document_from_session(&first).unwrap()),
            vec!["image-b".to_string()]
        );
        first.undo().unwrap();
        assert_eq!(
            image_ids(&document_from_session(&first).unwrap()),
            vec!["image-a".to_string(), "image-b".to_string()]
        );

        let mut second = session_from_document(&document).unwrap();
        delete_image_anchor_if_identity(&mut second, NSRange::new(2, 1), Some("image-b")).unwrap();
        assert_eq!(
            image_ids(&document_from_session(&second).unwrap()),
            vec!["image-a".to_string()]
        );
        second.undo().unwrap();
        assert_eq!(
            image_ids(&document_from_session(&second).unwrap()),
            vec!["image-a".to_string(), "image-b".to_string()]
        );
    }

    fn image_ids(document: &Document) -> Vec<String> {
        document
            .blocks
            .iter()
            .flat_map(|block| match block {
                Block::Paragraph { inlines, .. } | Block::Heading { inlines, .. } => inlines,
                Block::List { items, .. } => &items[0].inlines,
            })
            .filter_map(|inline| match inline {
                Inline::Image { resource_id, .. } => Some(resource_id.clone()),
                _ => None,
            })
            .collect()
    }
}
