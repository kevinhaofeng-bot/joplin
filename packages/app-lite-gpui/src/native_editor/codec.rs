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
    Block, BlockContent, BlockKind, Document, DocumentError, Mark, NodeId, StyledRun, TextAlignment,
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
    let mut next_id = 1_u64;

    for (block_index, block) in document.blocks().iter().enumerate() {
        match block {
            CanonicalBlock::Paragraph { style, inlines } => {
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
            CanonicalBlock::List { kind, items } => {
                for item in items {
                    let kind = match kind {
                        ListKind::Unordered => BlockKind::BulletItem { depth: 0 },
                        ListKind::Ordered => BlockKind::OrderedItem { depth: 0 },
                        ListKind::Checklist => BlockKind::CheckItem {
                            depth: 0,
                            checked: item.checked.unwrap_or(false),
                        },
                    };
                    native.push(text_block(
                        next_node_id(&mut next_id),
                        kind,
                        &item.style,
                        &item.inlines,
                        block_index,
                    )?);
                }
            }
            CanonicalBlock::Quote { style, inlines } => {
                native.push(text_block(
                    next_node_id(&mut next_id),
                    BlockKind::Quote,
                    style,
                    inlines,
                    block_index,
                )?);
            }
            CanonicalBlock::Code { style, inlines } => {
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
    Document::from_blocks(native).map_err(CanonicalImportError::InvalidDocument)
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
    let mut pending_list: Option<(ListKind, Vec<app_lite_core::document::ListItem>)> = None;

    let flush_list =
        |output: &mut Vec<CanonicalBlock>,
         pending: &mut Option<(ListKind, Vec<app_lite_core::document::ListItem>)>| {
            if let Some((kind, items)) = pending.take() {
                output.push(CanonicalBlock::List { kind, items });
            }
        };

    for (block_index, block) in document.blocks().iter().enumerate() {
        let list_kind = match &block.kind {
            BlockKind::BulletItem { depth } => Some((ListKind::Unordered, *depth, None)),
            BlockKind::OrderedItem { depth } => Some((ListKind::Ordered, *depth, None)),
            BlockKind::CheckItem { depth, checked } => {
                Some((ListKind::Checklist, *depth, Some(*checked)))
            }
            _ => None,
        };
        if let Some((kind, depth, checked)) = list_kind {
            if depth != 0 {
                return Err(CanonicalExportError::UnsupportedListDepth { block_index, depth });
            }
            let (style, inlines) = export_text_block(block, block_index)?;
            match pending_list.as_mut() {
                Some((pending_kind, items)) if *pending_kind == kind => {
                    items.push(app_lite_core::document::ListItem {
                        checked,
                        style,
                        inlines,
                    });
                }
                _ => {
                    flush_list(&mut output, &mut pending_list);
                    pending_list = Some((
                        kind,
                        vec![app_lite_core::document::ListItem {
                            checked,
                            style,
                            inlines,
                        }],
                    ));
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
                let level = match level {
                    1 => HeadingLevel::One,
                    2 => HeadingLevel::Two,
                    3 => HeadingLevel::Three,
                    other => {
                        return Err(CanonicalExportError::UnsupportedBlockKind {
                            block_index,
                            kind: format!("Heading({other})"),
                        });
                    }
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
            BlockKind::BulletItem { .. }
            | BlockKind::OrderedItem { .. }
            | BlockKind::CheckItem { .. } => unreachable!("handled as a list item above"),
        }
    }
    flush_list(&mut output, &mut pending_list);
    Ok(CanonicalDocument::from_blocks(output))
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
        }
    }
    output
}

fn next_node_id(next_id: &mut u64) -> NodeId {
    let id = NodeId::new(*next_id);
    *next_id = next_id.saturating_add(1);
    id
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::One => 1,
        HeadingLevel::Two => 2,
        HeadingLevel::Three => 3,
    }
}

fn text_block(
    id: NodeId,
    kind: BlockKind,
    style: &BlockStyle,
    inlines: &[Inline],
    block_index: usize,
) -> Result<Block, CanonicalImportError> {
    if style.indent != 0 {
        return Err(CanonicalImportError::UnsupportedIndent {
            block_index,
            indent: style.indent,
        });
    }
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
            Inline::Image { .. } => {
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
            },
            CanonicalBlock::List {
                kind: ListKind::Ordered,
                items: vec![item("编号", None)],
            },
            CanonicalBlock::List {
                kind: ListKind::Checklist,
                items: vec![item("待办", Some(false))],
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
            }],
        }]);
        assert!(import_canonical(&image).is_err());
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
            },
        };
        let document = CanonicalDocument::from_blocks(vec![
            CanonicalBlock::Paragraph {
                style: BlockStyle::default(),
                inlines: vec![marked("段落"), Inline::SoftBreak, marked("换行")],
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
            },
            CanonicalBlock::List {
                kind: ListKind::Ordered,
                items: vec![ListItem {
                    checked: None,
                    style: BlockStyle::default(),
                    inlines: vec![marked("序号")],
                }],
            },
            CanonicalBlock::List {
                kind: ListKind::Checklist,
                items: vec![ListItem {
                    checked: Some(true),
                    style: BlockStyle::default(),
                    inlines: vec![marked("清单")],
                }],
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
