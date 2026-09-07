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
    NSAttachmentAttributeName, NSAttributedStringAttachmentConveniences, NSFont,
    NSFontAttributeName, NSImage, NSMutableParagraphStyle, NSParagraphStyleAttributeName,
    NSTextAlignment, NSTextAttachment,
};
use objc2_foundation::{
    NSAttributedString, NSAttributedStringKey, NSData, NSMutableAttributedString, NSNumber,
    NSRange, NSSize, NSString, NSURL,
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
const PROJECTION_PREFIX_KEY: &str = "com.kevinhao.joplin-lite.projection-prefix";

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

/// The one owner of the live edit history.  AppKit's text storage is a
/// projection and must not register a second undo manager for these edits.
pub struct NativeEditorSession {
    _backend: text_document::DocumentBackend,
    pub(crate) text: TextDocument,
    pub(crate) revision: u64,
    image_dimensions: HashMap<String, (u32, u32)>,
}

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

    pub fn undo(&mut self) -> Result<(), EditorCodecError> {
        self.text.undo().map_err(model_error)?;
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    pub fn redo(&mut self) -> Result<(), EditorCodecError> {
        self.text.redo().map_err(model_error)?;
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }
}

pub struct RenderedDocument {
    pub attributed: Retained<NSMutableAttributedString>,
    pub missing_resources: usize,
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
                result.push(logical_block(block.clone()));
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

    let mut image_dimensions = HashMap::new();
    let mut document_offset = 0usize;
    for logical in &blocks {
        let block_len = logical.text.chars().count();
        let cursor = model.cursor_at(document_offset);
        cursor.set_position(document_offset + block_len, MoveMode::KeepAnchor);
        apply_block_style(&cursor, &logical.block)?;
        apply_list_style(&model, &cursor, &logical.block)?;

        let mut local_offset = 0usize;
        let inlines = block_inlines(&logical.block);
        for inline in inlines {
            let scalar_len = match inline {
                Inline::Text { text, marks } => {
                    let len = text.chars().count();
                    if len > 0 {
                        let range = model.cursor_at(document_offset + local_offset);
                        range.set_position(
                            document_offset + local_offset + len,
                            MoveMode::KeepAnchor,
                        );
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
            let image = model.cursor_at(document_offset + *position);
            // TextDocument requires positive dimensions for an image anchor;
            // actual pixels stay in ResourceStore and are never copied here.
            image
                .insert_image(resource_id, alt, 1, 1)
                .map_err(model_error)?;
            image_dimensions
                .entry(resource_id.clone())
                .or_insert((1, 1));
        }
        document_offset += block_len + logical.images.len() + 1;
    }

    Ok(NativeEditorSession {
        _backend: backend,
        text: model,
        revision: 0,
        image_dimensions,
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

fn apply_list_style(
    _model: &TextDocument,
    cursor: &TextCursor,
    block: &Block,
) -> Result<(), EditorCodecError> {
    let Some(kind) = (match block {
        Block::List { kind, .. } => Some(*kind),
        _ => None,
    }) else {
        return Ok(());
    };
    let style = match kind {
        ListKind::Ordered => ListStyle::Decimal,
        ListKind::Unordered | ListKind::Checklist => ListStyle::Disc,
    };
    cursor.create_list(style).map_err(model_error)?;
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
    // The list operation owns the list; explicitly set the indent afterwards
    // so nested list items remain distinguishable in the live model.
    let indent = match block {
        Block::List { items, .. } => items[0].style.indent,
        _ => 0,
    };
    cursor
        .set_current_list_format(&text_document::ListFormat {
            indent: Some(indent),
            ..Default::default()
        })
        .map_err(model_error)?;
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
        highlight: format.background_color.is_some(),
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
                FragmentContent::Text { text, format, .. } => {
                    for character in text.chars() {
                        if character == '\u{2028}' || character == '\u{000b}' {
                            inlines.push(Inline::SoftBreak);
                        } else if character != '\r' {
                            append_text(
                                &mut inlines,
                                &character.to_string(),
                                &marks_from_format(&format),
                            );
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
                .list_info
                .as_ref()
                .map(|info| info.indent)
                .or(snapshot.block_format.indent)
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
    if range.location == 0 {
        start_scalar = Some(0);
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
    if end == offset {
        end_scalar = Some(text.chars().count());
    }
    match (start_scalar, end_scalar) {
        (Some(start), Some(end)) => Ok((start, end)),
        _ => Err(EditorCodecError::InvalidUtf16Range),
    }
}

pub fn apply_committed_text_delta(
    session: &mut NativeEditorSession,
    range: NSRange,
    replacement: &str,
) -> Result<(), EditorCodecError> {
    if replacement.contains('\0') {
        return Err(EditorCodecError::InvalidReplacement);
    }
    let text = session.text.to_addressable_text().map_err(model_error)?;
    let (start, end) = utf16_range(&text, range)?;
    let cursor = session.text.cursor_at(start);
    cursor.set_position(end, MoveMode::KeepAnchor);
    cursor.insert_text(replacement).map_err(model_error)?;
    session.revision = session.revision.wrapping_add(1);
    Ok(())
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
    let cursor = session.text.cursor_at(start);
    cursor.set_position(end, MoveMode::KeepAnchor);
    cursor
        .insert_image(resource_id, alt, width.max(1), height.max(1))
        .map_err(model_error)?;
    session
        .image_dimensions
        .insert(resource_id.to_owned(), (width.max(1), height.max(1)));
    session.revision = session.revision.wrapping_add(1);
    Ok(())
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
    cursor.merge_char_format(&format).map_err(model_error)?;
    session.revision = session.revision.wrapping_add(1);
    Ok(())
}

pub fn apply_inline_command(
    session: &mut NativeEditorSession,
    selection: NSRange,
    command: InlineCommand,
) -> Result<(), EditorCodecError> {
    let text = session.text.to_addressable_text().map_err(model_error)?;
    let (start, end) = utf16_range(&text, selection)?;
    let cursor = session.text.cursor_at(start);
    cursor.set_position(end, MoveMode::KeepAnchor);
    let active = query_inline_state(session, selection, command)? == SelectionState::Active;
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
        InlineCommand::Highlight => TextFormat {
            background_color: Some(if active {
                text_document::Color::rgba(0, 0, 0, 0)
            } else {
                text_document::Color::rgb(255, 235, 130)
            }),
            ..Default::default()
        },
        InlineCommand::Clear => TextFormat {
            font_bold: Some(false),
            font_italic: Some(false),
            font_underline: Some(false),
            font_strikeout: Some(false),
            clear_link: true,
            ..Default::default()
        },
    };
    cursor.merge_char_format(&format).map_err(model_error)?;
    session.revision = session.revision.wrapping_add(1);
    Ok(())
}

pub fn apply_block_command(
    session: &mut NativeEditorSession,
    selection: NSRange,
    command: BlockCommand,
) -> Result<(), EditorCodecError> {
    let text = session.text.to_addressable_text().map_err(model_error)?;
    let (start, end) = utf16_range(&text, selection)?;
    let cursor = session.text.cursor_at(start);
    cursor.set_position(end, MoveMode::KeepAnchor);
    match command {
        BlockCommand::Paragraph => {
            cursor
                .set_block_format(&BlockFormat {
                    heading_level: Some(0),
                    marker: Some(MarkerType::NoMarker),
                    ..Default::default()
                })
                .map_err(model_error)?;
            remove_lists_in_selection(session, start, end)?;
        }
        BlockCommand::Heading(level) => {
            let heading_level = match level {
                HeadingLevel::One => 1,
                HeadingLevel::Two => 2,
                HeadingLevel::Three => 3,
            };
            cursor
                .set_block_format(&BlockFormat {
                    heading_level: Some(heading_level),
                    ..Default::default()
                })
                .map_err(model_error)?;
        }
        BlockCommand::UnorderedList | BlockCommand::OrderedList | BlockCommand::Checklist => {
            let style = match command {
                BlockCommand::OrderedList => ListStyle::Decimal,
                _ => ListStyle::Disc,
            };
            cursor.create_list(style).map_err(model_error)?;
            if matches!(command, BlockCommand::Checklist) {
                cursor
                    .set_block_format(&BlockFormat {
                        marker: Some(MarkerType::Unchecked),
                        ..Default::default()
                    })
                    .map_err(model_error)?;
            }
        }
    }
    session.revision = session.revision.wrapping_add(1);
    Ok(())
}

fn remove_lists_in_selection(
    session: &NativeEditorSession,
    start: usize,
    end: usize,
) -> Result<(), EditorCodecError> {
    let mut positions = Vec::new();
    for element in session.text.flow() {
        let FlowElement::Block(block) = element else {
            continue;
        };
        let snapshot = block.snapshot();
        if let Some(_list) = snapshot.list_info
            && snapshot.position < end
            && snapshot.position + snapshot.length >= start
        {
            positions.push(snapshot.position);
        }
    }
    for position in positions {
        let cursor = session.text.cursor_at(position);
        cursor
            .remove_current_block_from_list()
            .map_err(model_error)
            .ok();
    }
    Ok(())
}

pub fn apply_paragraph_command(
    session: &mut NativeEditorSession,
    selection: NSRange,
    command: ParagraphCommand,
) -> Result<(), EditorCodecError> {
    let text = session.text.to_addressable_text().map_err(model_error)?;
    let (start, end) = utf16_range(&text, selection)?;
    let cursor = session.text.cursor_at(start);
    cursor.set_position(end, MoveMode::KeepAnchor);
    match command {
        ParagraphCommand::Align(alignment) => cursor.set_block_format(&BlockFormat {
            alignment: Some(td_alignment(alignment)),
            ..Default::default()
        }),
        ParagraphCommand::IncreaseIndent => {
            let current = cursor
                .block_format()
                .map_err(model_error)?
                .indent
                .unwrap_or(0);
            cursor.set_block_format(&BlockFormat {
                indent: Some(current.saturating_add(1).min(8)),
                ..Default::default()
            })
        }
        ParagraphCommand::DecreaseIndent => {
            let current = cursor
                .block_format()
                .map_err(model_error)?
                .indent
                .unwrap_or(0);
            cursor.set_block_format(&BlockFormat {
                indent: Some(current.saturating_sub(1)),
                ..Default::default()
            })
        }
    }
    .map_err(model_error)?;
    session.revision = session.revision.wrapping_add(1);
    Ok(())
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
    if cursor
        .set_block_format(&BlockFormat {
            marker: Some(marker),
            ..Default::default()
        })
        .is_ok()
    {
        session.revision = session.revision.wrapping_add(1);
        true
    } else {
        false
    }
}

pub fn query_inline_state(
    session: &NativeEditorSession,
    selection: NSRange,
    command: InlineCommand,
) -> Result<SelectionState, EditorCodecError> {
    let text = session.text.to_addressable_text().map_err(model_error)?;
    let (start, end) = utf16_range(&text, selection)?;
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

fn style_for_block(_block_format: &text_document::BlockFormat, heading: bool) -> Retained<NSFont> {
    let size = if heading { 22.0 } else { 17.0 };
    NSFont::systemFontOfSize(size)
}

pub fn render_session<F>(session: &NativeEditorSession, mut load: F, width: f64) -> RenderedDocument
where
    F: FnMut(&str) -> Option<StoredResource>,
{
    let output = NSMutableAttributedString::from_nsstring(&NSString::from_str(""));
    let mut missing_resources = 0;
    let _ = width;
    for element in session.text.flow() {
        let FlowElement::Block(block) = element else {
            continue;
        };
        let snapshot = block.snapshot();
        let heading = snapshot
            .block_format
            .heading_level
            .is_some_and(|level| level > 0);
        if let Some(list) = snapshot.list_info.as_ref() {
            let marker = match snapshot.block_format.marker {
                Some(MarkerType::Checked) => "☑".to_owned(),
                Some(MarkerType::Unchecked) => "☐".to_owned(),
                _ => list.marker.clone(),
            };
            let prefix = NSMutableAttributedString::from_nsstring(&NSString::from_str(&format!(
                "{marker} "
            )));
            let prefix_key = NSString::from_str(PROJECTION_PREFIX_KEY);
            let marker_value = NSString::from_str("1");
            unsafe {
                prefix.addAttribute_value_range(
                    &prefix_key,
                    &marker_value,
                    NSRange::new(0, prefix.string().length()),
                );
            }
            output.appendAttributedString(&prefix);
        }
        for fragment in snapshot.fragments {
            match fragment {
                FragmentContent::Text { text, format, .. } => {
                    let piece =
                        NSMutableAttributedString::from_nsstring(&NSString::from_str(&text));
                    let range = NSRange::new(0, piece.string().length());
                    let font = style_for_block(&format_to_block(&format), heading);
                    let paragraph = NSMutableParagraphStyle::new();
                    paragraph.setAlignment(match snapshot.block_format.alignment {
                        Some(TdAlignment::Center) => NSTextAlignment::Center,
                        Some(TdAlignment::Right) => NSTextAlignment::Right,
                        Some(TdAlignment::Justify) => NSTextAlignment::Justified,
                        _ => NSTextAlignment::Left,
                    });
                    paragraph
                        .setHeadIndent(f64::from(snapshot.block_format.indent.unwrap_or(0)) * 24.0);
                    unsafe {
                        piece.addAttribute_value_range(NSFontAttributeName, &font, range);
                        piece.addAttribute_value_range(
                            NSParagraphStyleAttributeName,
                            &paragraph,
                            range,
                        );
                    }
                    if let Some(href) = format.anchor_href {
                        let key = NSAttributedStringKey::from_str("NSLink");
                        let value =
                            NSURL::initWithString(NSURL::alloc(), &NSString::from_str(&href));
                        if let Some(value) = value {
                            unsafe {
                                piece.addAttribute_value_range(&key, &value, range);
                            }
                        }
                    }
                    output.appendAttributedString(&piece);
                }
                FragmentContent::Image {
                    name,
                    alt,
                    width: image_width,
                    height: image_height,
                    ..
                } => {
                    let Some(resource) = load(&name) else {
                        missing_resources += 1;
                        let text = if alt.is_empty() {
                            "[图片]".to_owned()
                        } else {
                            format!("[图片：{alt}]")
                        };
                        let placeholder =
                            NSMutableAttributedString::from_nsstring(&NSString::from_str(&text));
                        let marker_key = NSString::from_str(MISSING_RESOURCE_KEY);
                        let marker = NSString::from_str("1");
                        let id_key = NSString::from_str(RESOURCE_ID_KEY);
                        let alt_key = NSString::from_str(RESOURCE_ALT_KEY);
                        unsafe {
                            placeholder.addAttribute_value_range(
                                &marker_key,
                                &marker,
                                NSRange::new(0, placeholder.string().length()),
                            );
                            placeholder.addAttribute_value_range(
                                &id_key,
                                &NSString::from_str(&name),
                                NSRange::new(0, placeholder.string().length()),
                            );
                            placeholder.addAttribute_value_range(
                                &alt_key,
                                &NSString::from_str(&alt),
                                NSRange::new(0, placeholder.string().length()),
                            );
                        }
                        output.appendAttributedString(&placeholder);
                        continue;
                    };
                    let data = NSData::with_bytes(&resource.bytes);
                    let attachment = NSTextAttachment::initWithData_ofType(
                        NSTextAttachment::alloc(),
                        Some(&data),
                        None,
                    );
                    if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
                        attachment.setImage(Some(&image));
                        let ratio = if image.size().width > 0.0 {
                            (width / image.size().width).min(1.0)
                        } else {
                            1.0
                        };
                        attachment.setBounds(objc2_foundation::NSRect::new(
                            objc2_foundation::NSPoint::new(0.0, 0.0),
                            NSSize::new(image.size().width * ratio, image.size().height * ratio),
                        ));
                    }
                    let attributed =
                        NSAttributedString::attributedStringWithAttachment(&attachment);
                    let piece = NSMutableAttributedString::from_attributed_nsstring(&attributed);
                    unsafe {
                        let id_key = NSString::from_str(RESOURCE_ID_KEY);
                        let alt_key = NSString::from_str(RESOURCE_ALT_KEY);
                        let width_key = NSString::from_str(RESOURCE_WIDTH_KEY);
                        let height_key = NSString::from_str(RESOURCE_HEIGHT_KEY);
                        piece.addAttribute_value_range(
                            &id_key,
                            &NSString::from_str(&name),
                            NSRange::new(0, 1),
                        );
                        piece.addAttribute_value_range(
                            &alt_key,
                            &NSString::from_str(&alt),
                            NSRange::new(0, 1),
                        );
                        piece.addAttribute_value_range(
                            &width_key,
                            &NSNumber::numberWithUnsignedLongLong(image_width as u64),
                            NSRange::new(0, 1),
                        );
                        piece.addAttribute_value_range(
                            &height_key,
                            &NSNumber::numberWithUnsignedLongLong(image_height as u64),
                            NSRange::new(0, 1),
                        );
                        let _ = NSAttachmentAttributeName;
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
        output.appendAttributedString(&NSMutableAttributedString::from_nsstring(
            &NSString::from_str("\n"),
        ));
    }
    RenderedDocument {
        attributed: output,
        missing_resources,
    }
}

fn format_to_block(_format: &TextFormat) -> text_document::BlockFormat {
    text_document::BlockFormat::default()
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
}
