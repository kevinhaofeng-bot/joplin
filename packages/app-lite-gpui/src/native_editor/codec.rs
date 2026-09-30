//! Deterministic bridge between the durable canonical document snapshot and
//! the native editor model.

use app_lite_core::document::{
    Alignment, Block as CanonicalBlock, BlockStyle, HeadingLevel, ImagePresentation, Inline,
    ListKind, Marks,
};
use app_lite_core::{CanonicalDocument, ResourceId};
use smallvec::SmallVec;
use std::fmt;

use super::model::{
    Block, BlockContent, BlockKind, Document, DocumentError, InlineGroup, MAX_LIST_DEPTH, Mark,
    NodeId, StyledRun, TableContent, TextAlignment,
};

/// The library shell deliberately imports only the canonical subset that can
/// be displayed faithfully before Task 4 owns a bidirectional save codec.
///
/// Returning a typed error is intentional: using `body_text` as a fallback
/// would silently destroy formatting and make a later save irreversible.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CanonicalImportError {
    UnsupportedIndent {
        block_index: usize,
        indent: u8,
    },
    UnsupportedInlineImage {
        block_index: usize,
    },
    MissingResource {
        block_index: usize,
        resource_id: String,
    },
    InvalidDocument(DocumentError),
}

impl fmt::Display for CanonicalImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedIndent {
                block_index,
                indent,
            } => write!(
                f,
                "第 {} 个段落使用了尚未支持的缩进级别 {}",
                block_index + 1,
                indent
            ),
            Self::UnsupportedInlineImage { block_index } => write!(
                f,
                "第 {} 个段落包含图片；图片将在下一阶段显示",
                block_index + 1
            ),
            Self::MissingResource {
                block_index,
                resource_id,
            } => write!(
                f,
                "第 {} 个资源块引用的资源 {resource_id} 不属于当前笔记",
                block_index + 1
            ),
            Self::InvalidDocument(error) => write!(f, "无法构造原生文档：{error}"),
        }
    }
}

impl std::error::Error for CanonicalImportError {}

/// Export errors are deliberately typed and fail closed. Saving a document
/// whose native structure cannot be represented by the canonical Task 4
/// subset is worse than leaving it dirty with a visible error: flattening it
/// would make a later recovery impossible.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CanonicalExportError {
    UnsupportedBlockKind {
        block_index: usize,
        kind: String,
    },
    UnsupportedListDepth {
        block_index: usize,
        depth: u8,
    },
    InvalidTextContent {
        block_index: usize,
    },
    MissingResource {
        block_index: usize,
        resource_id: String,
    },
}

impl fmt::Display for CanonicalExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedBlockKind { block_index, kind } => write!(
                f,
                "第 {} 个原生块类型 {kind} 尚不能安全保存",
                block_index + 1
            ),
            Self::UnsupportedListDepth { block_index, depth } => write!(
                f,
                "第 {} 个列表项使用了尚未支持的嵌套级别 {depth}",
                block_index + 1
            ),
            Self::InvalidTextContent { block_index } => {
                write!(f, "第 {} 个文本块内容无效", block_index + 1)
            }
            Self::MissingResource {
                block_index,
                resource_id,
            } => write!(
                f,
                "第 {} 个资源块引用的资源 {resource_id} 不属于当前笔记",
                block_index + 1
            ),
        }
    }
}

impl std::error::Error for CanonicalExportError {}

/// Imports the durable HTML-derived document into the native block model.
/// This is intentionally one way: the library is read-only until Task 4 adds
/// an inverse codec and save coordinator.
pub fn import_canonical(document: &CanonicalDocument) -> Result<Document, CanonicalImportError> {
    import_canonical_with_resources(document, &document.resource_ids())
}

/// Strict library-session importer.  Resource insertion belongs to Task 5;
/// Task 4 may only display and preserve a resource already related to this
/// note.  The explicit relation list prevents a malformed HTML body from
/// manufacturing a reference during an otherwise ordinary text save.
pub fn import_canonical_with_resources(
    document: &CanonicalDocument,
    available_resources: &[ResourceId],
) -> Result<Document, CanonicalImportError> {
    let mut native = Vec::new();
    let mut inline_groups = Vec::new();
    let mut list_starts = std::collections::BTreeMap::new();
    let mut next_id = 1_u64;

    for (block_index, block) in document.blocks().iter().enumerate() {
        match block {
            CanonicalBlock::Paragraph { style, inlines } => {
                if has_inline_atom(inlines) {
                    // The native document has image atoms at block boundaries.
                    // Split only this legacy flow, preserving text order/marks,
                    // repeated images and alt text. The persisted source isn't
                    // changed just by opening; a later edit saves this explicit
                    // structural representation.
                    let mut text = Vec::new();
                    for inline in inlines {
                        if let Inline::Image {
                            resource_id,
                            alt,
                            display_width,
                            link,
                        } = inline
                        {
                            ensure_resource(resource_id, available_resources, block_index)?;
                            if !text.is_empty() {
                                native.push(text_block(
                                    next_node_id(&mut next_id),
                                    BlockKind::Paragraph,
                                    style,
                                    &text,
                                    block_index,
                                )?);
                                text.clear();
                            }
                            if style.indent != 0 {
                                return Err(CanonicalImportError::UnsupportedIndent {
                                    block_index,
                                    indent: style.indent,
                                });
                            }
                            native.push(Block {
                                id: next_node_id(&mut next_id),
                                kind: BlockKind::Image,
                                content: BlockContent::Image {
                                    resource_id: resource_id.as_str().to_owned(),
                                    alt: alt.clone(),
                                    natural_size_known: false,
                                    natural_size: (1024, 768),
                                    display_width: *display_width,
                                    link: link.clone(),
                                },
                                alignment: match style.alignment {
                                    Alignment::Left => TextAlignment::Left,
                                    Alignment::Center => TextAlignment::Center,
                                    Alignment::Right => TextAlignment::Right,
                                },
                                revision: 0,
                            });
                        } else if let Inline::Attachment {
                            resource_id,
                            filename,
                            media_type,
                        } = inline
                        {
                            ensure_resource(resource_id, available_resources, block_index)?;
                            if !text.is_empty() {
                                native.push(text_block(
                                    next_node_id(&mut next_id),
                                    BlockKind::Paragraph,
                                    style,
                                    &text,
                                    block_index,
                                )?);
                                text.clear();
                            }
                            native.push(attachment_block(
                                next_node_id(&mut next_id),
                                resource_id,
                                filename,
                                media_type,
                            ));
                        } else {
                            text.push(inline.clone());
                        }
                    }
                    if !text.is_empty() {
                        native.push(text_block(
                            next_node_id(&mut next_id),
                            BlockKind::Paragraph,
                            style,
                            &text,
                            block_index,
                        )?);
                    }
                    continue;
                }
                native.push(text_block(
                    next_node_id(&mut next_id),
                    BlockKind::Paragraph,
                    style,
                    inlines,
                    block_index,
                )?);
            }
            CanonicalBlock::Heading {
                level,
                style,
                inlines,
            } => {
                if has_inline_image(inlines) {
                    push_inline_group(
                        &mut native,
                        &mut inline_groups,
                        &mut next_id,
                        BlockKind::Heading {
                            level: heading_level(*level),
                        },
                        style,
                        inlines,
                        available_resources,
                        block_index,
                    )?;
                    continue;
                }
                native.push(text_block(
                    next_node_id(&mut next_id),
                    BlockKind::Heading {
                        level: heading_level(*level),
                    },
                    style,
                    inlines,
                    block_index,
                )?);
            }
            CanonicalBlock::List { kind, items, start } => {
                let first = native.len();
                for item in items {
                    let kind = match kind {
                        ListKind::Unordered => BlockKind::BulletItem { depth: 0 },
                        ListKind::Ordered => BlockKind::OrderedItem { depth: 0 },
                        ListKind::Checklist => BlockKind::CheckItem {
                            depth: 0,
                            checked: item.checked.unwrap_or(false),
                        },
                    };
                    if has_inline_image(&item.inlines) {
                        push_inline_group(
                            &mut native,
                            &mut inline_groups,
                            &mut next_id,
                            kind,
                            &item.style,
                            &item.inlines,
                            available_resources,
                            block_index,
                        )?;
                        continue;
                    }
                    native.push(text_block(
                        next_node_id(&mut next_id),
                        kind,
                        &item.style,
                        &item.inlines,
                        block_index,
                    )?);
                }
                if let (Some(start), Some(first)) = (start, native.get(first)) {
                    list_starts.insert(first.id, *start);
                }
            }
            CanonicalBlock::Quote { style, inlines } => {
                if has_inline_image(inlines) {
                    push_inline_group(
                        &mut native,
                        &mut inline_groups,
                        &mut next_id,
                        BlockKind::Quote,
                        style,
                        inlines,
                        available_resources,
                        block_index,
                    )?;
                    continue;
                }
                native.push(text_block(
                    next_node_id(&mut next_id),
                    BlockKind::Quote,
                    style,
                    inlines,
                    block_index,
                )?);
            }
            CanonicalBlock::Code { style, inlines } => {
                if has_inline_image(inlines) {
                    push_inline_group(
                        &mut native,
                        &mut inline_groups,
                        &mut next_id,
                        BlockKind::Code,
                        style,
                        inlines,
                        available_resources,
                        block_index,
                    )?;
                    continue;
                }
                native.push(text_block(
                    next_node_id(&mut next_id),
                    BlockKind::Code,
                    style,
                    inlines,
                    block_index,
                )?);
            }
            CanonicalBlock::Image {
                resource_id,
                alt,
                presentation,
                link,
            } => {
                ensure_resource(resource_id, available_resources, block_index)?;
                native.push(Block {
                    id: next_node_id(&mut next_id),
                    kind: BlockKind::Image,
                    content: BlockContent::Image {
                        resource_id: resource_id.as_str().to_owned(),
                        alt: alt.clone(),
                        // Legacy HTML has no dimensions. It remains readable
                        // and is refreshed by the resource hydrator, while a
                        // persisted image uses its own first-frame extent.
                        natural_size_known: presentation.natural_size.is_some(),
                        natural_size: presentation.natural_size.unwrap_or((1024, 768)),
                        display_width: presentation.display_width,
                        link: link.clone(),
                    },
                    alignment: TextAlignment::Left,
                    revision: 0,
                });
            }
            CanonicalBlock::Attachment {
                resource_id,
                filename,
                media_type,
            } => {
                ensure_resource(resource_id, available_resources, block_index)?;
                native.push(Block {
                    id: next_node_id(&mut next_id),
                    kind: BlockKind::Attachment,
                    content: BlockContent::Attachment {
                        resource_id: resource_id.as_str().to_owned(),
                        filename: filename.clone(),
                        media_type: media_type.clone(),
                    },
                    alignment: TextAlignment::Left,
                    revision: 0,
                });
            }
            CanonicalBlock::Table { rows, header } => {
                for resource_id in
                    CanonicalDocument::from_blocks(vec![block.clone()]).resource_ids()
                {
                    ensure_resource(&resource_id, available_resources, block_index)?;
                }
                native.push(Block {
                    id: next_node_id(&mut next_id),
                    kind: BlockKind::Table,
                    content: BlockContent::Table(std::sync::Arc::new(TableContent {
                        header: *header,
                        rows: rows
                            .iter()
                            .map(|row| {
                                row.cells
                                    .iter()
                                    .map(|cell| table_cell_text(&cell.inlines))
                                    .collect()
                            })
                            .collect(),
                        canonical: block.clone(),
                    })),
                    alignment: TextAlignment::Left,
                    revision: 0,
                });
            }
            CanonicalBlock::Divider => native.push(Block {
                id: next_node_id(&mut next_id),
                kind: BlockKind::Divider,
                content: BlockContent::Empty,
                alignment: TextAlignment::Left,
                revision: 0,
            }),
        }
    }

    if native.is_empty() {
        return Ok(Document::new());
    }
    let mut document =
        Document::from_blocks(native).map_err(CanonicalImportError::InvalidDocument)?;
    document.set_inline_groups(inline_groups);
    document.set_list_starts(list_starts);
    Ok(document)
}

fn has_inline_image(inlines: &[Inline]) -> bool {
    has_inline_atom(inlines)
}

/// Images and file cards are block atoms natively; a parent holding either
/// is imported as a semantic group of text and atom members.
fn has_inline_atom(inlines: &[Inline]) -> bool {
    inlines
        .iter()
        .any(|inline| matches!(inline, Inline::Image { .. } | Inline::Attachment { .. }))
}

fn attachment_block(
    id: NodeId,
    resource_id: &ResourceId,
    filename: &str,
    media_type: &str,
) -> Block {
    Block {
        id,
        kind: BlockKind::Attachment,
        content: BlockContent::Attachment {
            resource_id: resource_id.as_str().to_owned(),
            filename: filename.to_owned(),
            media_type: media_type.to_owned(),
        },
        alignment: TextAlignment::Left,
        revision: 0,
    }
}

fn push_inline_group(
    native: &mut Vec<Block>,
    groups: &mut Vec<InlineGroup>,
    next_id: &mut u64,
    kind: BlockKind,
    style: &BlockStyle,
    inlines: &[Inline],
    available_resources: &[ResourceId],
    block_index: usize,
) -> Result<(), CanonicalImportError> {
    let mut members = Vec::new();
    let mut text = Vec::new();
    for inline in inlines {
        match inline {
            Inline::Image {
                resource_id,
                alt,
                display_width,
                link,
            } => {
                ensure_resource(resource_id, available_resources, block_index)?;
                let id = next_node_id(next_id);
                members.push(id);
                native.push(text_block(id, kind.clone(), style, &text, block_index)?);
                text.clear();
                let id = next_node_id(next_id);
                members.push(id);
                native.push(Block {
                    id,
                    kind: BlockKind::Image,
                    content: BlockContent::Image {
                        resource_id: resource_id.as_str().to_owned(),
                        alt: alt.clone(),
                        natural_size_known: false,
                        natural_size: (1024, 768),
                        display_width: *display_width,
                        link: link.clone(),
                    },
                    alignment: TextAlignment::Left,
                    revision: 0,
                });
            }
            Inline::Attachment {
                resource_id,
                filename,
                media_type,
            } => {
                ensure_resource(resource_id, available_resources, block_index)?;
                let id = next_node_id(next_id);
                members.push(id);
                native.push(text_block(id, kind.clone(), style, &text, block_index)?);
                text.clear();
                let id = next_node_id(next_id);
                members.push(id);
                native.push(attachment_block(id, resource_id, filename, media_type));
            }
            _ => text.push(inline.clone()),
        }
    }
    if !text.is_empty() {
        let id = next_node_id(next_id);
        members.push(id);
        native.push(text_block(id, kind.clone(), style, &text, block_index)?);
    }
    groups.push(InlineGroup { kind, members });
    Ok(())
}

/// Exports the semantic native document without ever consulting `body_text`.
/// This is intentionally the inverse of `import_canonical` for the current
/// editable subset; unsupported structural nodes return a visible save error
/// until their dedicated product phase owns them.
pub fn export_canonical(document: &Document) -> Result<CanonicalDocument, CanonicalExportError> {
    export_canonical_with_resources(document, None)
}

/// Strict exporter used by a note session.  `None` keeps the pure codec
/// useful to native-editor tests; a real session always supplies its durable
/// note-resource relation and therefore fails closed before SQLite writes.
pub fn export_canonical_with_resources(
    document: &Document,
    available_resources: Option<&[ResourceId]>,
) -> Result<CanonicalDocument, CanonicalExportError> {
    let mut output = Vec::new();
    let mut pending_list: Option<PendingList> = None;

    let flush_list = |output: &mut Vec<CanonicalBlock>, pending: &mut Option<PendingList>| {
        if let Some((kind, items, start)) = pending.take() {
            output.push(CanonicalBlock::List { kind, items, start });
        }
    };

    for (block_index, block) in document.blocks().iter().enumerate() {
        if let Some(group) = document
            .inline_groups()
            .iter()
            .find(|group| group.members.contains(&block.id))
        {
            let active = document
                .blocks()
                .iter()
                .enumerate()
                .filter(|(_, candidate)| group.members.contains(&candidate.id))
                .collect::<Vec<_>>();
            if active
                .first()
                .is_none_or(|(index, _)| *index != block_index)
            {
                continue;
            }
            if active.windows(2).any(|pair| pair[1].0 != pair[0].0 + 1) {
                return Err(CanonicalExportError::InvalidTextContent { block_index });
            }
            let mut kind = group.kind.clone();
            let mut style = None;
            let mut inlines = Vec::new();
            for (index, member) in active {
                match &member.content {
                    BlockContent::Text { .. } => {
                        let (segment_style, segment) = export_text_block(member, index)?;
                        if let Some(existing) = &style {
                            if existing != &segment_style {
                                return Err(CanonicalExportError::InvalidTextContent {
                                    block_index,
                                });
                            }
                        } else {
                            kind = member.kind.clone();
                            style = Some(segment_style);
                        }
                        inlines.extend(segment);
                    }
                    BlockContent::Image {
                        resource_id,
                        alt,
                        display_width,
                        link,
                        ..
                    } => {
                        inlines.push(Inline::Image {
                            resource_id: parse_allowed_resource(
                                resource_id,
                                available_resources,
                                index,
                            )?,
                            alt: alt.clone(),
                            display_width: *display_width,
                            link: link.clone(),
                        });
                    }
                    BlockContent::Attachment {
                        resource_id,
                        filename,
                        media_type,
                    } => {
                        inlines.push(Inline::Attachment {
                            resource_id: parse_allowed_resource(
                                resource_id,
                                available_resources,
                                index,
                            )?,
                            filename: filename.clone(),
                            media_type: media_type.clone(),
                        });
                    }
                    _ => return Err(CanonicalExportError::InvalidTextContent { block_index }),
                }
            }
            let mut style = style.unwrap_or_default();
            let list = match kind {
                BlockKind::BulletItem { depth } => Some((ListKind::Unordered, depth, None)),
                BlockKind::OrderedItem { depth } => Some((ListKind::Ordered, depth, None)),
                BlockKind::CheckItem { depth, checked } => {
                    Some((ListKind::Checklist, depth, Some(checked)))
                }
                _ => None,
            };
            if let Some((list_kind, depth, checked)) = list {
                style.indent = list_indent(depth, block_index)?;
                let item = app_lite_core::document::ListItem {
                    checked,
                    style,
                    inlines,
                };
                let start = document.list_start(block.id);
                match pending_list.as_mut() {
                    Some((pending_kind, items, _))
                        if *pending_kind == list_kind && start.is_none() =>
                    {
                        items.push(item)
                    }
                    _ => {
                        flush_list(&mut output, &mut pending_list);
                        pending_list = Some((list_kind, vec![item], start));
                    }
                }
            } else {
                flush_list(&mut output, &mut pending_list);
                output.push(match kind {
                    BlockKind::Heading { level } if canonical_heading_level(level).is_some() => {
                        CanonicalBlock::Heading {
                            level: canonical_heading_level(level).expect("checked by the guard"),
                            style,
                            inlines,
                        }
                    }
                    BlockKind::Quote => CanonicalBlock::Quote { style, inlines },
                    BlockKind::Code => CanonicalBlock::Code { style, inlines },
                    BlockKind::Paragraph => CanonicalBlock::Paragraph { style, inlines },
                    _ => {
                        return Err(CanonicalExportError::UnsupportedBlockKind {
                            block_index,
                            kind: format!("{kind:?}"),
                        });
                    }
                });
            }
            continue;
        }
        let list_kind = match &block.kind {
            BlockKind::BulletItem { depth } => Some((ListKind::Unordered, *depth, None)),
            BlockKind::OrderedItem { depth } => Some((ListKind::Ordered, *depth, None)),
            BlockKind::CheckItem { depth, checked } => {
                Some((ListKind::Checklist, *depth, Some(*checked)))
            }
            _ => None,
        };
        if let Some((kind, depth, checked)) = list_kind {
            let (mut style, inlines) = export_text_block(block, block_index)?;
            style.indent = list_indent(depth, block_index)?;
            let item = app_lite_core::document::ListItem {
                checked,
                style,
                inlines,
            };
            let start = document.list_start(block.id);
            match pending_list.as_mut() {
                Some((pending_kind, items, _)) if *pending_kind == kind && start.is_none() => {
                    items.push(item);
                }
                _ => {
                    flush_list(&mut output, &mut pending_list);
                    pending_list = Some((kind, vec![item], start));
                }
            }
            continue;
        }
        flush_list(&mut output, &mut pending_list);
        match &block.kind {
            BlockKind::Paragraph => {
                let (style, inlines) = export_text_block(block, block_index)?;
                output.push(CanonicalBlock::Paragraph { style, inlines });
            }
            BlockKind::Heading { level } => {
                let (style, inlines) = export_text_block(block, block_index)?;
                let Some(level) = canonical_heading_level(*level) else {
                    return Err(CanonicalExportError::UnsupportedBlockKind {
                        block_index,
                        kind: format!("Heading({level})"),
                    });
                };
                output.push(CanonicalBlock::Heading {
                    level,
                    style,
                    inlines,
                });
            }
            BlockKind::Quote => {
                let (style, inlines) = export_text_block(block, block_index)?;
                output.push(CanonicalBlock::Quote { style, inlines });
            }
            BlockKind::Code => {
                let (style, inlines) = export_text_block(block, block_index)?;
                output.push(CanonicalBlock::Code { style, inlines });
            }
            BlockKind::Image => {
                let BlockContent::Image {
                    resource_id,
                    alt,
                    natural_size_known,
                    natural_size,
                    display_width,
                    link,
                } = &block.content
                else {
                    return Err(CanonicalExportError::InvalidTextContent { block_index });
                };
                let resource_id =
                    parse_allowed_resource(resource_id, available_resources, block_index)?;
                output.push(CanonicalBlock::Image {
                    resource_id,
                    alt: alt.clone(),
                    presentation: ImagePresentation {
                        natural_size: (*natural_size_known).then_some(*natural_size),
                        display_width: *display_width,
                    },
                    link: link.clone(),
                });
            }
            BlockKind::Attachment => {
                let BlockContent::Attachment {
                    resource_id,
                    filename,
                    media_type,
                } = &block.content
                else {
                    return Err(CanonicalExportError::InvalidTextContent { block_index });
                };
                output.push(CanonicalBlock::Attachment {
                    resource_id: parse_allowed_resource(
                        resource_id,
                        available_resources,
                        block_index,
                    )?,
                    filename: filename.clone(),
                    media_type: media_type.clone(),
                });
            }
            BlockKind::Divider => {
                if !matches!(block.content, BlockContent::Empty) {
                    return Err(CanonicalExportError::InvalidTextContent { block_index });
                }
                output.push(CanonicalBlock::Divider);
            }
            BlockKind::Table => {
                let BlockContent::Table(table) = &block.content else {
                    return Err(CanonicalExportError::InvalidTextContent { block_index });
                };
                for resource_id in
                    CanonicalDocument::from_blocks(vec![table.canonical.clone()]).resource_ids()
                {
                    parse_allowed_resource(resource_id.as_str(), available_resources, block_index)?;
                }
                output.push(table.canonical.clone());
            }
            BlockKind::BulletItem { .. }
            | BlockKind::OrderedItem { .. }
            | BlockKind::CheckItem { .. } => unreachable!("handled as a list item above"),
        }
    }
    flush_list(&mut output, &mut pending_list);
    Ok(CanonicalDocument::from_blocks(output))
}

/// Display text of a read-only table cell: soft breaks become lines and
/// images/cards show their label.
pub(crate) fn table_cell_text(inlines: &[Inline]) -> String {
    let mut text = String::new();
    for inline in inlines {
        match inline {
            Inline::Text { text: run, .. } => text.push_str(run),
            Inline::SoftBreak => text.push('\n'),
            Inline::Image { alt, .. } => {
                text.push_str(if alt.is_empty() { "[图片]" } else { alt })
            }
            Inline::Attachment { filename, .. } => text.push_str(filename),
        }
    }
    text
}

fn ensure_resource(
    resource_id: &ResourceId,
    available_resources: &[ResourceId],
    block_index: usize,
) -> Result<(), CanonicalImportError> {
    if available_resources.iter().any(|id| id == resource_id) {
        Ok(())
    } else {
        Err(CanonicalImportError::MissingResource {
            block_index,
            resource_id: resource_id.as_str().to_owned(),
        })
    }
}

fn parse_allowed_resource(
    raw: &str,
    available_resources: Option<&[ResourceId]>,
    block_index: usize,
) -> Result<ResourceId, CanonicalExportError> {
    let resource_id = ResourceId::new(raw).map_err(|_| CanonicalExportError::MissingResource {
        block_index,
        resource_id: raw.to_owned(),
    })?;
    if let Some(available_resources) = available_resources
        && !available_resources.iter().any(|id| id == &resource_id)
    {
        return Err(CanonicalExportError::MissingResource {
            block_index,
            resource_id: raw.to_owned(),
        });
    }
    Ok(resource_id)
}

fn export_text_block(
    block: &Block,
    block_index: usize,
) -> Result<(BlockStyle, Vec<Inline>), CanonicalExportError> {
    let BlockContent::Text { text, styles } = &block.content else {
        return Err(CanonicalExportError::InvalidTextContent { block_index });
    };
    Ok((
        BlockStyle {
            alignment: match block.alignment {
                TextAlignment::Left => Alignment::Left,
                TextAlignment::Center => Alignment::Center,
                TextAlignment::Right => Alignment::Right,
            },
            indent: 0,
        },
        export_inlines(text, styles, block_index)?,
    ))
}

fn export_inlines(
    text: &str,
    styles: &[StyledRun],
    block_index: usize,
) -> Result<Vec<Inline>, CanonicalExportError> {
    let mut boundaries = vec![0, text.len()];
    for style in styles {
        boundaries.push(style.range.start);
        boundaries.push(style.range.end);
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut output = Vec::new();
    for pair in boundaries.windows(2) {
        let start = pair[0];
        let end = pair[1];
        if start == end {
            continue;
        }
        let segment = text
            .get(start..end)
            .ok_or(CanonicalExportError::InvalidTextContent { block_index })?;
        let mut native_marks = smallvec::SmallVec::<[Mark; 4]>::new();
        for style in styles {
            if style.range.start <= start && style.range.end >= end {
                native_marks.extend(style.marks.iter().cloned());
            }
        }
        let marks = canonical_marks(&native_marks);
        let mut pieces = segment.split('\n').peekable();
        while let Some(piece) = pieces.next() {
            if !piece.is_empty() {
                output.push(Inline::Text {
                    text: piece.to_owned(),
                    marks: marks.clone(),
                });
            }
            if pieces.peek().is_some() {
                output.push(Inline::SoftBreak);
            }
        }
    }
    Ok(output)
}

fn canonical_marks(marks: &[Mark]) -> Marks {
    let mut output = Marks::default();
    for mark in marks {
        match mark {
            Mark::Bold => output.bold = true,
            Mark::Italic => output.italic = true,
            Mark::Underline => output.underline = true,
            Mark::Strike => output.strikethrough = true,
            Mark::Highlight => output.highlight = true,
            Mark::Link(url) => output.link = Some(url.clone()),
            Mark::InlineCode => output.inline_code = true,
            Mark::Superscript => output.script = Some(app_lite_core::Script::Superscript),
            Mark::Subscript => output.script = Some(app_lite_core::Script::Subscript),
            Mark::Color(color) => output.color = Some(*color),
        }
    }
    output
}

/// A canonical list being assembled from consecutive native items, with its
/// `<ol start>` taken from the first item's recorded start.
type PendingList = (
    ListKind,
    Vec<app_lite_core::document::ListItem>,
    Option<u32>,
);

fn list_indent(depth: u8, block_index: usize) -> Result<u8, CanonicalExportError> {
    if depth > MAX_LIST_DEPTH {
        return Err(CanonicalExportError::UnsupportedListDepth { block_index, depth });
    }
    Ok(depth)
}

fn next_node_id(next_id: &mut u64) -> NodeId {
    let id = NodeId::new(*next_id);
    *next_id = next_id.saturating_add(1);
    id
}

fn canonical_heading_level(level: u8) -> Option<HeadingLevel> {
    Some(match level {
        1 => HeadingLevel::One,
        2 => HeadingLevel::Two,
        3 => HeadingLevel::Three,
        4 => HeadingLevel::Four,
        5 => HeadingLevel::Five,
        6 => HeadingLevel::Six,
        _ => return None,
    })
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::One => 1,
        HeadingLevel::Two => 2,
        HeadingLevel::Three => 3,
        HeadingLevel::Four => 4,
        HeadingLevel::Five => 5,
        HeadingLevel::Six => 6,
    }
}

fn text_block(
    id: NodeId,
    kind: BlockKind,
    style: &BlockStyle,
    inlines: &[Inline],
    block_index: usize,
) -> Result<Block, CanonicalImportError> {
    let kind = match kind {
        BlockKind::BulletItem { .. } => BlockKind::BulletItem {
            depth: style.indent,
        },
        BlockKind::OrderedItem { .. } => BlockKind::OrderedItem {
            depth: style.indent,
        },
        BlockKind::CheckItem { checked, .. } => BlockKind::CheckItem {
            depth: style.indent,
            checked,
        },
        _ if style.indent != 0 => {
            return Err(CanonicalImportError::UnsupportedIndent {
                block_index,
                indent: style.indent,
            });
        }
        kind => kind,
    };
    let (text, styles) = import_inlines(inlines, block_index)?;
    Ok(Block {
        id,
        kind,
        content: BlockContent::Text { text, styles },
        alignment: match style.alignment {
            Alignment::Left => TextAlignment::Left,
            Alignment::Center => TextAlignment::Center,
            Alignment::Right => TextAlignment::Right,
        },
        revision: 0,
    })
}

fn import_inlines(
    inlines: &[Inline],
    block_index: usize,
) -> Result<(String, SmallVec<[StyledRun; 4]>), CanonicalImportError> {
    let mut text = String::new();
    let mut styles = SmallVec::new();
    for inline in inlines {
        match inline {
            Inline::Text { text: value, marks } => {
                let start = text.len();
                text.push_str(value);
                let marks = native_marks(marks);
                if start != text.len() && !marks.is_empty() {
                    styles.push(StyledRun::new(start..text.len(), marks));
                }
            }
            Inline::SoftBreak => text.push('\n'),
            Inline::Image { .. } | Inline::Attachment { .. } => {
                return Err(CanonicalImportError::UnsupportedInlineImage { block_index });
            }
        }
    }
    Ok((text, styles))
}

fn native_marks(marks: &Marks) -> SmallVec<[Mark; 4]> {
    let mut output = SmallVec::new();
    if marks.bold {
        output.push(Mark::Bold);
    }
    if marks.italic {
        output.push(Mark::Italic);
    }
    if marks.underline {
        output.push(Mark::Underline);
    }
    if marks.strikethrough {
        output.push(Mark::Strike);
    }
    if marks.highlight {
        output.push(Mark::Highlight);
    }
    if let Some(link) = &marks.link {
        output.push(Mark::Link(link.clone()));
    }
    if marks.inline_code {
        output.push(Mark::InlineCode);
    }
    match marks.script {
        Some(app_lite_core::Script::Superscript) => output.push(Mark::Superscript),
        Some(app_lite_core::Script::Subscript) => output.push(Mark::Subscript),
        None => {}
    }
    if let Some(color) = marks.color {
        output.push(Mark::Color(color));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{
        CanonicalExportError, CanonicalImportError, export_canonical_with_resources,
        import_canonical, import_canonical_with_resources,
    };
    use crate::native_editor::model::{BlockContent, BlockKind, Mark, TextAlignment};
    use app_lite_core::CanonicalDocument;
    use app_lite_core::document::{
        Alignment, Block as CanonicalBlock, BlockStyle, HeadingLevel, ImagePresentation, Inline,
        ListItem, ListKind, Marks,
    };

    #[test]
    fn imports_rich_canonical_blocks_without_flattening_styles_or_soft_breaks() {
        // This would fail if the importer silently fell back to body_text, lost
        // UTF-8 ranges, or treated canonical list items as ordinary paragraphs.
        let document = CanonicalDocument::from_blocks(vec![
            CanonicalBlock::Paragraph {
                style: BlockStyle {
                    alignment: Alignment::Center,
                    indent: 0,
                },
                inlines: vec![
                    Inline::Text {
                        text: "中文".into(),
                        marks: Marks {
                            bold: true,
                            link: Some("https://example.com/笔记".into()),
                            ..Marks::default()
                        },
                    },
                    Inline::SoftBreak,
                    Inline::Text {
                        text: "下一行".into(),
                        marks: Marks {
                            italic: true,
                            underline: true,
                            strikethrough: true,
                            highlight: true,
                            ..Marks::default()
                        },
                    },
                ],
            },
            CanonicalBlock::Heading {
                level: HeadingLevel::Two,
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "标题".into(),
                    marks: Marks::default(),
                }],
            },
            CanonicalBlock::List {
                kind: ListKind::Checklist,
                items: vec![ListItem {
                    checked: Some(true),
                    style: BlockStyle {
                        alignment: Alignment::Right,
                        indent: 0,
                    },
                    inlines: vec![Inline::Text {
                        text: "完成".into(),
                        marks: Marks::default(),
                    }],
                }],
                start: None,
            },
        ]);

        let imported = import_canonical(&document).expect("supported canonical document");

        assert_eq!(imported.block_count(), 3);
        assert_eq!(
            imported.block_kinds(),
            vec![
                BlockKind::Paragraph,
                BlockKind::Heading { level: 2 },
                BlockKind::CheckItem {
                    depth: 0,
                    checked: true,
                },
            ]
        );
        assert_eq!(imported.blocks()[0].alignment, TextAlignment::Center);
        assert_eq!(imported.blocks()[2].alignment, TextAlignment::Right);
        let BlockContent::Text { text, styles } = &imported.blocks()[0].content else {
            panic!("paragraph must remain text");
        };
        assert_eq!(text, "中文\n下一行");
        assert!(styles.iter().any(|style| {
            style.range == (0.."中文".len())
                && style.marks.contains(&Mark::Bold)
                && style
                    .marks
                    .contains(&Mark::Link("https://example.com/笔记".into()))
        }));
        assert!(styles.iter().any(|style| {
            style.range == ("中文\n".len()..text.len())
                && style.marks.contains(&Mark::Italic)
                && style.marks.contains(&Mark::Underline)
                && style.marks.contains(&Mark::Strike)
                && style.marks.contains(&Mark::Highlight)
        }));
    }

    #[test]
    fn imports_every_supported_heading_and_list_variant() {
        // Keep the Task 3 one-way bridge honest for the complete supported
        // block set; a body-text fallback could not satisfy these kinds.
        let item = |text: &str, checked: Option<bool>| ListItem {
            checked,
            style: BlockStyle::default(),
            inlines: vec![Inline::Text {
                text: text.into(),
                marks: Marks::default(),
            }],
        };
        let document = CanonicalDocument::from_blocks(vec![
            CanonicalBlock::Heading {
                level: HeadingLevel::One,
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "一级".into(),
                    marks: Marks::default(),
                }],
            },
            CanonicalBlock::Heading {
                level: HeadingLevel::Two,
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "二级".into(),
                    marks: Marks::default(),
                }],
            },
            CanonicalBlock::Heading {
                level: HeadingLevel::Three,
                style: BlockStyle::default(),
                inlines: vec![Inline::Text {
                    text: "三级".into(),
                    marks: Marks::default(),
                }],
            },
            CanonicalBlock::List {
                kind: ListKind::Unordered,
                items: vec![item("项目", None)],
                start: None,
            },
            CanonicalBlock::List {
                kind: ListKind::Ordered,
                items: vec![item("编号", None)],
                start: None,
            },
            CanonicalBlock::List {
                kind: ListKind::Checklist,
                items: vec![item("待办", Some(false))],
                start: None,
            },
        ]);

        let imported = import_canonical(&document).expect("all Task 3 variants import");
        assert_eq!(
            imported.block_kinds(),
            vec![
                BlockKind::Heading { level: 1 },
                BlockKind::Heading { level: 2 },
                BlockKind::Heading { level: 3 },
                BlockKind::BulletItem { depth: 0 },
                BlockKind::OrderedItem { depth: 0 },
                BlockKind::CheckItem {
                    depth: 0,
                    checked: false,
                },
            ]
        );
    }

    #[test]
    fn rejects_unrepresentable_canonical_content_instead_of_flattening_it() {
        let indented = CanonicalDocument::from_blocks(vec![CanonicalBlock::Paragraph {
            style: BlockStyle {
                alignment: Alignment::Left,
                indent: 1,
            },
            inlines: vec![Inline::Text {
                text: "不能丢失缩进".into(),
                marks: Marks::default(),
            }],
        }]);
        assert!(import_canonical(&indented).is_err());

        let image = CanonicalDocument::from_blocks(vec![CanonicalBlock::Paragraph {
            style: BlockStyle::default(),
            inlines: vec![Inline::Image {
                resource_id: app_lite_core::ResourceId::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                    .expect("valid resource id"),
                alt: "尚未支持的图片".into(),
                display_width: None,
                link: None,
            }],
        }]);
        let imported = import_canonical(&image).expect("legacy inline image must open");
        assert!(matches!(imported.blocks()[0].kind, BlockKind::Image));
    }

    #[test]
    fn legacy_image_between_marked_text_opens_in_order_and_exports_without_loss() {
        let canonical = CanonicalDocument::parse_html(
            "<p><strong>图前</strong><img src=\":/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" alt=\"图\"><em>图后</em></p>"
        ).unwrap();
        let native = import_canonical(&canonical).expect("imported JEX image must open");
        assert_eq!(native.blocks().len(), 3);
        assert!(matches!(native.blocks()[1].kind, BlockKind::Image));
        let exported = super::export_canonical(&native).unwrap();
        assert_eq!(exported.resource_ids(), canonical.resource_ids());
        let html = exported.to_canonical_html();
        assert!(html.as_str().contains("<strong>图前</strong>"));
        assert!(html.as_str().contains("<em>图后</em>"));
        assert!(html.as_str().find("图前").unwrap() < html.as_str().find("<img ").unwrap());
        assert!(html.as_str().find("<img ").unwrap() < html.as_str().find("图后").unwrap());
        assert!(import_canonical_with_resources(&canonical, &[]).is_err());
    }

    #[test]
    fn images_inside_heading_quote_and_ordered_item_preserve_their_parent_structure() {
        let resource = app_lite_core::ResourceId::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let image = Inline::Image {
            resource_id: resource.clone(),
            alt: "图".into(),
            display_width: None,
            link: None,
        };
        let text = |value: &str| Inline::Text {
            text: value.into(),
            marks: Marks::default(),
        };
        let style = BlockStyle::default();
        let canonical = CanonicalDocument::from_blocks(vec![
            CanonicalBlock::Heading {
                level: HeadingLevel::Two,
                style: style.clone(),
                inlines: vec![text("标题前"), image.clone(), text("标题后")],
            },
            CanonicalBlock::Quote {
                style: style.clone(),
                inlines: vec![text("引用前"), image.clone(), text("引用后")],
            },
            CanonicalBlock::List {
                kind: ListKind::Ordered,
                items: vec![
                    ListItem {
                        checked: None,
                        style: style.clone(),
                        inlines: vec![text("第一项")],
                    },
                    ListItem {
                        checked: None,
                        style,
                        inlines: vec![text("图前"), image, text("图后")],
                    },
                ],
                start: None,
            },
        ]);
        let native = import_canonical(&canonical).expect("inline images must load");
        assert_eq!(
            native
                .blocks()
                .iter()
                .filter(|block| matches!(block.kind, BlockKind::Image))
                .count(),
            3
        );
        let numbers = crate::native_editor::layout::ordered_number_summary(&native);
        let ordered = native
            .blocks()
            .iter()
            .filter(|block| matches!(block.kind, BlockKind::OrderedItem { .. }))
            .collect::<Vec<_>>();
        assert_eq!(numbers.get(&ordered[0].id), Some(&1));
        assert_eq!(numbers.get(&ordered[1].id), Some(&2));
        assert_eq!(
            numbers.get(&ordered[2].id),
            None,
            "continuation must not add a list number"
        );
        assert_eq!(super::export_canonical(&native).unwrap(), canonical);
    }

    #[test]
    fn grouped_list_image_survives_text_edits_image_delete_and_undo() {
        use crate::native_editor::history::History;
        use crate::native_editor::model::{Affinity, DocPoint, Selection};
        use crate::native_editor::transaction::Transaction;
        let canonical = CanonicalDocument::parse_html(
            "<ol><li>图前<img src=\":/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" alt=\"照片\">图后</li><li>下一项</li></ol>"
        ).unwrap();
        let mut document = import_canonical(&canonical).unwrap();
        let before = document.blocks()[0].id;
        let image = document.blocks()[1].id;
        let after = document.blocks()[2].id;
        let mut history = History::new(32, 4 * 1024 * 1024);
        history
            .apply(
                &mut document,
                Transaction::InsertText {
                    selection: Selection::caret(DocPoint::new(before, "图".len())),
                    text: "新".into(),
                },
            )
            .unwrap();
        history
            .apply(
                &mut document,
                Transaction::InsertText {
                    selection: Selection::caret(DocPoint::new(after, "图".len())),
                    text: "增".into(),
                },
            )
            .unwrap();
        let exported = super::export_canonical(&document).unwrap();
        let html = exported.to_canonical_html();
        assert!(html.as_str().contains("图新前"));
        assert!(html.as_str().contains("图增后"));
        assert_eq!(
            crate::native_editor::layout::ordered_number_summary(&document)
                .get(&document.blocks()[3].id),
            Some(&2)
        );
        let before_delete = document.semantic_snapshot();
        history
            .apply(
                &mut document,
                Transaction::DeleteRange {
                    selection: Selection::new(
                        DocPoint::with_affinity(image, 0, Affinity::Before),
                        DocPoint::with_affinity(image, 0, Affinity::After),
                    ),
                },
            )
            .unwrap();
        let without_image = super::export_canonical(&document).unwrap();
        assert!(without_image.resource_ids().is_empty());
        assert!(
            without_image
                .to_canonical_html()
                .as_str()
                .contains("图新前图增后")
        );
        history.undo(&mut document).unwrap();
        assert_eq!(document.semantic_snapshot(), before_delete);
        assert_eq!(
            super::export_canonical(&document).unwrap().resource_ids(),
            canonical.resource_ids()
        );
    }

    #[test]
    fn grouped_list_image_uses_full_image_row_inside_list_content_column() {
        let canonical = CanonicalDocument::parse_html(
            "<ol><li>前<img src=\":/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\">后</li><li>下一项</li></ol>",
        )
        .unwrap();
        let document = import_canonical(&canonical).unwrap();
        let image_id = document
            .blocks()
            .iter()
            .find(|block| matches!(block.kind, BlockKind::Image))
            .unwrap()
            .id;
        let mut layout = crate::native_editor::layout::LayoutRegistry::new();
        layout.layout_document(&document, 0.0, 1_000.0, 680.0);
        let image = layout
            .visible()
            .iter()
            .find(|block| block.node_id == image_id)
            .unwrap();
        assert!(f32::from(image.bounds.left()) >= 22.0);
        assert!(f32::from(image.bounds.size.width) > 300.0);
        assert!(f32::from(image.bounds.size.height) > 200.0);
    }

    #[test]
    fn grouped_inline_image_resize_saves_and_reopens_with_its_width() {
        use crate::native_editor::transaction::Transaction;
        let canonical = CanonicalDocument::parse_html(
            "<h2>前<img src=\":/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\">后</h2>",
        )
        .unwrap();
        let mut document = import_canonical(&canonical).unwrap();
        let image_id = document
            .blocks()
            .iter()
            .find(|block| matches!(block.kind, BlockKind::Image))
            .unwrap()
            .id;
        document
            .apply(Transaction::SetImageDisplayWidth {
                node_id: image_id,
                display_width: Some(320),
            })
            .unwrap();
        let exported = super::export_canonical(&document).expect("width is storable");
        let html = exported.to_canonical_html();
        assert!(html.as_str().starts_with("<h2>前<img"), "{}", html.as_str());
        let reopened = import_canonical(&exported).unwrap();
        let width = reopened
            .blocks()
            .iter()
            .find_map(|block| match &block.content {
                BlockContent::Image { display_width, .. } => Some(*display_width),
                _ => None,
            });
        assert_eq!(width, Some(Some(320)));
        assert_eq!(super::export_canonical(&reopened).unwrap(), exported);
    }

    #[test]
    fn tables_open_as_read_only_atoms_and_save_unchanged() {
        let html = "<p>前</p><table data-joplin-lite-table=\"true\"><tbody><tr><th>名称</th><th>说明</th></tr><tr><td><strong>甲</strong></td><td>一行<br>二行</td></tr><tr><td><a href=\"https://example.com/\">链接</a></td><td><img src=\":/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" alt=\"图\"></td></tr></tbody></table><p>后</p>";
        let canonical = CanonicalDocument::parse_html(html).unwrap();
        let resources = canonical.resource_ids();
        let document = import_canonical_with_resources(&canonical, &resources).unwrap();
        let table = &document.blocks()[1];
        assert_eq!(table.kind, BlockKind::Table);
        let BlockContent::Table(content) = &table.content else {
            panic!("{:?}", table.content);
        };
        assert!(content.header);
        assert_eq!(
            content.rows,
            vec![
                vec!["名称".to_owned(), "说明".to_owned()],
                vec!["甲".to_owned(), "一行\n二行".to_owned()],
                vec!["链接".to_owned(), "图".to_owned()],
            ]
        );
        assert_eq!(
            export_canonical_with_resources(&document, Some(&resources)).unwrap(),
            canonical
        );
        assert!(matches!(
            export_canonical_with_resources(&document, Some(&[])),
            Err(CanonicalExportError::MissingResource { .. })
        ));
        assert!(matches!(
            import_canonical_with_resources(&canonical, &[]),
            Err(CanonicalImportError::MissingResource { .. })
        ));
        let mut layout = crate::native_editor::layout::LayoutRegistry::new();
        layout.layout_document(&document, 0.0, 1_000.0, 680.0);
        let bounds = layout
            .visible()
            .iter()
            .find(|block| block.node_id == table.id)
            .unwrap()
            .bounds;
        // Rows of 1, 2 and 1 display lines.
        let expected = 4.0 * crate::native_editor::layout::TABLE_LINE_HEIGHT
            + 3.0 * crate::native_editor::layout::TABLE_ROW_PADDING;
        assert!((f32::from(bounds.size.height) - expected).abs() < 0.5);
    }

    #[test]
    fn ordered_list_start_numbers_the_editor_and_saves_unchanged() {
        let canonical = CanonicalDocument::parse_html(
            "<ol start=\"3\"><li>三</li><li>四</li></ol><p>间隔</p><ol start=\"10\"><li>图<img src=\":/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" alt=\"\">后</li><li>十一</li></ol>",
        )
        .unwrap();
        let document = import_canonical(&canonical).unwrap();
        assert_eq!(super::export_canonical(&document).unwrap(), canonical);
        let numbers = crate::native_editor::layout::ordered_number_summary(&document);
        let numbered: Vec<_> = document
            .blocks()
            .iter()
            .filter_map(|block| numbers.get(&block.id).copied())
            .collect();
        assert_eq!(numbered, vec![3, 4, 10, 11]);
        let mut layout = crate::native_editor::layout::LayoutRegistry::new();
        layout.layout_document(&document, 0.0, 1_000.0, 680.0);
        let painted: Vec<_> = document
            .blocks()
            .iter()
            .filter_map(|block| layout.ordered_number(block.id))
            .collect();
        assert_eq!(painted, vec![3, 4, 10, 11]);
    }

    #[test]
    fn h4_to_h6_open_as_native_headings_and_save_unchanged() {
        let canonical =
            CanonicalDocument::parse_html("<h4>四</h4><h5>五</h5><h6>六<img src=\":/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" alt=\"\"></h6>")
                .unwrap();
        let document = import_canonical(&canonical).unwrap();
        assert!(matches!(
            document.blocks()[0].kind,
            BlockKind::Heading { level: 4 }
        ));
        assert!(matches!(
            document.blocks()[1].kind,
            BlockKind::Heading { level: 5 }
        ));
        assert!(matches!(
            document.blocks()[2].kind,
            BlockKind::Heading { level: 6 }
        ));
        assert_eq!(super::export_canonical(&document).unwrap(), canonical);
    }

    #[test]
    fn linked_images_keep_their_link_through_open_resize_and_save() {
        use crate::native_editor::transaction::Transaction;
        let html = "<ul><li>图前<a href=\"https://example.com/a\"><img src=\":/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" alt=\"\"></a>图后</li></ul><a href=\"https://example.com/b\"><img data-joplin-lite-block-image=\"true\" src=\":/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" alt=\"\"></a>";
        let canonical = CanonicalDocument::parse_html(html).unwrap();
        let mut document = import_canonical(&canonical).unwrap();
        assert_eq!(super::export_canonical(&document).unwrap(), canonical);
        let images: Vec<_> = document
            .blocks()
            .iter()
            .filter(|block| matches!(block.kind, BlockKind::Image))
            .map(|block| block.id)
            .collect();
        for node_id in images {
            document
                .apply(Transaction::SetImageDisplayWidth {
                    node_id,
                    display_width: Some(200),
                })
                .unwrap();
        }
        let html = super::export_canonical(&document)
            .unwrap()
            .to_canonical_html();
        assert!(
            html.as_str()
                .contains("<a href=\"https://example.com/a\"><img src=")
                && html.as_str().contains(
                    "<a href=\"https://example.com/b\"><img data-joplin-lite-block-image"
                ),
            "{}",
            html.as_str()
        );
    }

    #[test]
    fn attachment_inserted_inside_an_image_list_item_saves_in_place_and_undoes() {
        use crate::native_editor::history::History;
        use crate::native_editor::model::{DocPoint, Selection};
        use crate::native_editor::transaction::Transaction;
        let canonical = CanonicalDocument::parse_html(
            "<ul><li>图前<img src=\":/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\">图后</li></ul>",
        )
        .unwrap();
        let mut document = import_canonical(&canonical).unwrap();
        let before = document.clone();
        let tail = document
            .blocks()
            .iter()
            .filter(|block| block.content.as_text().is_some())
            .last()
            .unwrap()
            .id;
        let caret = Selection::caret(DocPoint::new(tail, "图".len()));
        let mut history = History::new(100, 1 << 20);
        history
            .apply_with_selection(
                &mut document,
                caret,
                Transaction::InsertAttachment {
                    selection: caret,
                    resource_id: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
                    filename: "合同.pdf".into(),
                    media_type: "application/pdf".into(),
                },
            )
            .expect("attachments may live inside a grouped list item");
        let exported = super::export_canonical(&document).expect("storable");
        let html = exported.to_canonical_html();
        assert!(
            html.as_str().starts_with("<ul><li>图前<img")
                && html
                    .as_str()
                    .contains("图<a data-joplin-lite-inline-attachment=\"true\"")
                && html.as_str().ends_with("后</li></ul>"),
            "{}",
            html.as_str()
        );
        let reopened = import_canonical(&exported).unwrap();
        assert_eq!(super::export_canonical(&reopened).unwrap(), exported);
        history.undo_with_outcome(&mut document).unwrap();
        assert_eq!(
            super::export_canonical(&document).unwrap(),
            super::export_canonical(&before).unwrap()
        );
    }

    #[test]
    fn indenting_a_list_image_continuation_moves_its_whole_item_and_image_column() {
        use crate::native_editor::model::{DocPoint, Selection};
        use crate::native_editor::transaction::Transaction;
        let canonical = CanonicalDocument::parse_html(
            "<ol><li>首项</li><li>图前<img src=\":/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\">图后</li><li>尾项</li></ol>"
        ).unwrap();
        let mut document = import_canonical(&canonical).unwrap();
        let first = document.blocks()[1].id;
        let image = document.blocks()[2].id;
        let continuation = document.blocks()[3].id;
        let tail = document.blocks()[4].id;
        document
            .apply(Transaction::IndentList {
                selection: Selection::caret(DocPoint::new(continuation, 0)),
            })
            .unwrap();
        assert!(matches!(
            document.block(first).unwrap().kind,
            BlockKind::OrderedItem { depth: 1 }
        ));
        assert!(matches!(
            document.block(continuation).unwrap().kind,
            BlockKind::OrderedItem { depth: 1 }
        ));
        let mut layout = crate::native_editor::layout::LayoutRegistry::new();
        layout.layout_document(&document, 0.0, 1_000.0, 680.0);
        let image_bounds = layout
            .visible()
            .iter()
            .find(|block| block.node_id == image)
            .unwrap()
            .bounds;
        assert!(f32::from(image_bounds.left()) >= 42.0);
        let numbers = crate::native_editor::layout::ordered_number_summary(&document);
        assert_eq!(numbers.get(&tail), Some(&2));
    }

    #[test]
    fn indented_list_items_save_as_canonical_indent_and_reopen_at_their_depth() {
        use crate::native_editor::model::{DocPoint, Selection};
        use crate::native_editor::transaction::Transaction;
        let canonical =
            CanonicalDocument::parse_html("<ul><li>外层</li><li>内层</li></ul>").unwrap();
        let mut document = import_canonical(&canonical).unwrap();
        let inner = document.blocks()[1].id;
        document
            .apply(Transaction::IndentList {
                selection: Selection::caret(DocPoint::new(inner, 0)),
            })
            .unwrap();
        let exported = super::export_canonical(&document).unwrap();
        assert_eq!(
            exported.to_canonical_html().as_str(),
            "<ul><li>外层</li><li data-indent=\"1\">内层</li></ul>"
        );
        let reopened = import_canonical(&exported).unwrap();
        assert!(matches!(
            reopened.blocks()[1].kind,
            BlockKind::BulletItem { depth: 1 }
        ));
        assert_eq!(super::export_canonical(&reopened).unwrap(), exported);
    }

    #[test]
    fn indented_grouped_list_item_with_an_image_round_trips_its_depth() {
        let html = "<ol><li>首项</li><li data-indent=\"2\">图前<img src=\":/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\">图后</li></ol>";
        let canonical = CanonicalDocument::parse_html(html).unwrap();
        let document = import_canonical(&canonical).unwrap();
        assert!(matches!(
            document.blocks()[1].kind,
            BlockKind::OrderedItem { depth: 2 }
        ));
        assert_eq!(super::export_canonical(&document).unwrap(), canonical);
    }

    #[test]
    fn indented_paragraphs_remain_an_explicit_import_error() {
        let canonical = CanonicalDocument::parse_html("<p data-indent=\"1\">段落</p>").unwrap();
        assert!(matches!(
            import_canonical(&canonical),
            Err(CanonicalImportError::UnsupportedIndent { indent: 1, .. })
        ));
    }

    #[test]
    fn enter_after_grouped_list_image_creates_a_second_list_item() {
        use crate::native_editor::history::History;
        use crate::native_editor::model::DocPoint;
        use crate::native_editor::transaction::Transaction;
        let canonical = CanonicalDocument::parse_html(
            "<ol><li>前<img src=\":/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\">后续</li></ol>",
        )
        .unwrap();
        let mut document = import_canonical(&canonical).unwrap();
        let trailing = document.blocks()[2].id;
        let mut history = History::new(32, 4 * 1024 * 1024);
        history
            .apply(
                &mut document,
                Transaction::SplitBlock {
                    at: DocPoint::new(trailing, "后".len()),
                },
            )
            .unwrap();
        let exported = super::export_canonical(&document).unwrap();
        let [
            CanonicalBlock::List {
                kind: ListKind::Ordered,
                items,
                ..
            },
        ] = exported.blocks()
        else {
            panic!("Enter must retain an ordered list");
        };
        assert_eq!(items.len(), 2);
        assert!(
            items[0]
                .inlines
                .iter()
                .any(|inline| matches!(inline, Inline::Image { .. }))
        );
        assert_eq!(
            items[1].inlines,
            vec![Inline::Text {
                text: "续".into(),
                marks: Marks::default()
            }]
        );
        let numbers = crate::native_editor::layout::ordered_number_summary(&document);
        assert_eq!(numbers.get(&document.blocks()[3].id), Some(&2));
        history.undo(&mut document).unwrap();
        assert_eq!(super::export_canonical(&document).unwrap(), canonical);
    }

    #[test]
    fn merging_two_image_list_items_exports_each_resource_exactly_once() {
        use crate::native_editor::transaction::Transaction;
        let canonical = CanonicalDocument::parse_html(
            "<ol><li>A<img src=\":/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\">B</li><li>C<img src=\":/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\">D</li></ol>"
        ).unwrap();
        let mut document = import_canonical(&canonical).unwrap();
        let left = document.blocks()[2].id;
        let right = document.blocks()[3].id;
        document
            .apply(Transaction::MergeBlocks { left, right })
            .unwrap();
        let exported = super::export_canonical(&document).unwrap();
        let [
            CanonicalBlock::List {
                kind: ListKind::Ordered,
                items,
                ..
            },
        ] = exported.blocks()
        else {
            panic!("merged image items must remain an ordered list");
        };
        assert_eq!(items.len(), 1);
        assert_eq!(
            exported
                .resource_ids()
                .iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>(),
            [
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            ]
        );
    }

    #[test]
    #[ignore = "read-only real-copy audit; requires JOPLIN_LITE_AUDIT_DATABASE"]
    fn imported_real_copy_opens_and_round_trips_resources_in_native_editor() {
        use app_lite_core::{JexVerifiedResource, ResourceId, convert_jex_note_body_or_degrade};
        use std::collections::BTreeMap;
        let path = std::env::var("JOPLIN_LITE_AUDIT_DATABASE").expect("explicit isolated database");
        let db =
            rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        let mut resources = BTreeMap::new();
        let mut statement = db.prepare("SELECT a.source_id,r.id,r.mime,r.title FROM jex_stage_resource_audit a JOIN resources r ON r.id=a.resource_id").unwrap();
        for row in statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .unwrap()
        {
            let (source, id, mime, filename) = row.unwrap();
            resources.insert(
                source,
                JexVerifiedResource {
                    destination_id: ResourceId::new(id).unwrap(),
                    mime,
                    filename,
                },
            );
        }
        let mut statement = db.prepare("SELECT source_id,source_path,markup_language,raw_body_bytes FROM jex_stage_note_audit").unwrap();
        let mut failures = BTreeMap::<String, usize>::new();
        let mut total = 0;
        for row in statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Vec<u8>>(3)?,
                ))
            })
            .unwrap()
        {
            let (id, path, markup, bytes) = row.unwrap();
            total += 1;
            let converted = match convert_jex_note_body_or_degrade(
                &id,
                &path,
                markup,
                std::str::from_utf8(&bytes).unwrap(),
                &resources,
            ) {
                Ok((value, _)) => value,
                Err(error) => {
                    *failures
                        .entry(format!("conversion: {:?}", error.kind))
                        .or_default() += 1;
                    continue;
                }
            };
            match import_canonical(&converted.document) {
                Ok(native) => match super::export_canonical(&native) {
                    Ok(exported) => {
                        if exported.resource_ids() != converted.document.resource_ids() {
                            *failures.entry("resource order changed".into()).or_default() += 1;
                        }
                        let visible =
                            |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
                        if visible(exported.search_text().as_str())
                            != visible(&converted.search_text)
                        {
                            *failures
                                .entry("visible non-whitespace text changed".into())
                                .or_default() += 1;
                        }
                    }
                    Err(_) => *failures.entry("export rejected".into()).or_default() += 1,
                },
                Err(error) => {
                    let category = if let super::CanonicalImportError::UnsupportedInlineImage {
                        block_index,
                    } = &error
                    {
                        match &converted.document.blocks()[*block_index] {
                            CanonicalBlock::List { .. } => "list image",
                            CanonicalBlock::Heading { .. } => "heading image",
                            CanonicalBlock::Quote { .. } => "quote image",
                            _ => "other image",
                        }
                        .to_owned()
                    } else {
                        format!("native: {error}")
                    };
                    *failures.entry(category).or_default() += 1;
                }
            }
        }
        eprintln!("real-copy notes={total}, failure_categories={failures:?}");
        assert!(total > 0);
        assert!(
            failures.is_empty(),
            "real-copy native roundtrip failed; see count-only categories"
        );
    }

    #[test]
    fn task_four_codec_round_trips_every_structural_block_and_mark() {
        // This is deliberately a semantic round trip rather than a text
        // assertion: deleting any one branch in either codec direction loses
        // a block/mark or causes the generated canonical HTML to drift.
        let resource_id = app_lite_core::ResourceId::new("0123456789abcdef0123456789abcdef")
            .expect("valid resource id");
        let marked = |text: &str| Inline::Text {
            text: text.into(),
            marks: Marks {
                bold: true,
                italic: true,
                underline: true,
                strikethrough: true,
                highlight: true,
                inline_code: true,
                link: Some("https://example.test/格式".into()),
                script: Some(app_lite_core::Script::Superscript),
                color: Some(app_lite_core::TextColor::new([0xfc, 0x12, 0x33])),
            },
        };
        let lowered = |text: &str| Inline::Text {
            text: text.into(),
            marks: Marks {
                script: Some(app_lite_core::Script::Subscript),
                ..Marks::default()
            },
        };
        let document = CanonicalDocument::from_blocks(vec![
            CanonicalBlock::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![
                    marked("段落"),
                    Inline::SoftBreak,
                    marked("换行"),
                    lowered("下标"),
                ],
            },
            CanonicalBlock::Heading {
                level: HeadingLevel::Three,
                style: BlockStyle {
                    alignment: Alignment::Center,
                    indent: 0,
                },
                inlines: vec![marked("标题")],
            },
            CanonicalBlock::List {
                kind: ListKind::Unordered,
                items: vec![ListItem {
                    checked: None,
                    style: BlockStyle::default(),
                    inlines: vec![marked("项目")],
                }],
                start: None,
            },
            CanonicalBlock::List {
                kind: ListKind::Ordered,
                items: vec![ListItem {
                    checked: None,
                    style: BlockStyle::default(),
                    inlines: vec![marked("序号")],
                }],
                start: None,
            },
            CanonicalBlock::List {
                kind: ListKind::Checklist,
                items: vec![ListItem {
                    checked: Some(true),
                    style: BlockStyle::default(),
                    inlines: vec![marked("清单")],
                }],
                start: None,
            },
            CanonicalBlock::Quote {
                style: BlockStyle::default(),
                inlines: vec![marked("引用")],
            },
            CanonicalBlock::Code {
                style: BlockStyle::default(),
                inlines: vec![marked("let 中文 = true;")],
            },
            CanonicalBlock::Image {
                resource_id: resource_id.clone(),
                alt: "截图.png".into(),
                presentation: ImagePresentation::default(),
                link: None,
            },
            CanonicalBlock::Attachment {
                resource_id: resource_id.clone(),
                filename: "证据.pdf".into(),
                media_type: "application/pdf".into(),
            },
            CanonicalBlock::Divider,
        ]);

        let imported = import_canonical_with_resources(&document, &[resource_id.clone()])
            .expect("all Task 4 canonical forms import");
        assert_eq!(
            imported.block_kinds(),
            vec![
                BlockKind::Paragraph,
                BlockKind::Heading { level: 3 },
                BlockKind::BulletItem { depth: 0 },
                BlockKind::OrderedItem { depth: 0 },
                BlockKind::CheckItem {
                    depth: 0,
                    checked: true,
                },
                BlockKind::Quote,
                BlockKind::Code,
                BlockKind::Image,
                BlockKind::Attachment,
                BlockKind::Divider,
            ]
        );
        let exported = export_canonical_with_resources(&imported, Some(&[resource_id.clone()]))
            .expect("all Task 4 native forms export");
        assert_eq!(exported, document);
        assert_eq!(
            CanonicalDocument::parse_html(exported.to_canonical_html().as_str())
                .expect("generated canonical html parses"),
            document
        );
    }

    #[test]
    fn block_image_presentation_round_trips_without_a_one_pixel_placeholder() {
        // This fails if the canonical bridge reverts to its former fixed
        // (1, 1) import placeholder, drops a user display width, or omits
        // either value while serializing the durable note snapshot.
        let resource_id = app_lite_core::ResourceId::new("0123456789abcdef0123456789abcdef")
            .expect("valid resource id");
        let canonical = CanonicalDocument::from_blocks(vec![CanonicalBlock::Image {
            resource_id: resource_id.clone(),
            alt: "首帧稳定.png".into(),
            presentation: ImagePresentation {
                natural_size: Some((4032, 3024)),
                display_width: Some(960),
            },
            link: None,
        }]);

        let imported = import_canonical_with_resources(&canonical, &[resource_id.clone()])
            .expect("durable image imports");
        assert!(matches!(
            &imported.blocks()[0].content,
            BlockContent::Image {
                natural_size: (4032, 3024),
                display_width: Some(960),
                ..
            }
        ));
        assert_eq!(
            export_canonical_with_resources(&imported, Some(&[resource_id]))
                .expect("durable image exports"),
            canonical
        );
    }

    #[test]
    fn structural_resources_fail_closed_without_an_existing_note_relation() {
        let resource_id = app_lite_core::ResourceId::new("0123456789abcdef0123456789abcdef")
            .expect("valid resource id");
        let document = CanonicalDocument::from_blocks(vec![CanonicalBlock::Image {
            resource_id: resource_id.clone(),
            alt: "不能偷偷导入".into(),
            presentation: ImagePresentation::default(),
            link: None,
        }]);
        assert!(matches!(
            import_canonical_with_resources(&document, &[]),
            Err(CanonicalImportError::MissingResource { .. })
        ));
        let imported = import_canonical(&document).expect("pure codec can inspect the shape");
        assert!(matches!(
            export_canonical_with_resources(&imported, Some(&[])),
            Err(CanonicalExportError::MissingResource { .. })
        ));
    }
}
