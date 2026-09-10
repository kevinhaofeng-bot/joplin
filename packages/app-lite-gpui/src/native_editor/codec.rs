//! One-way import of the durable canonical document snapshot into the native
//! editor model.  Task 4 owns the inverse codec and durable saving.

use app_lite_core::CanonicalDocument;
use app_lite_core::document::{
    Alignment, Block as CanonicalBlock, BlockStyle, HeadingLevel, Inline, ListKind, Marks,
};
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
    UnsupportedIndent { block_index: usize, indent: u8 },
    UnsupportedInlineImage { block_index: usize },
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
            Self::InvalidDocument(error) => write!(f, "无法构造原生文档：{error}"),
        }
    }
}

impl std::error::Error for CanonicalImportError {}

/// Imports the durable HTML-derived document into the native block model.
/// This is intentionally one way: the library is read-only until Task 4 adds
/// an inverse codec and save coordinator.
pub fn import_canonical(document: &CanonicalDocument) -> Result<Document, CanonicalImportError> {
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
        }
    }

    if native.is_empty() {
        return Ok(Document::new());
    }
    Document::from_blocks(native).map_err(CanonicalImportError::InvalidDocument)
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
    output
}

#[cfg(test)]
mod tests {
    use super::import_canonical;
    use crate::native_editor::model::{BlockContent, BlockKind, Mark, TextAlignment};
    use app_lite_core::CanonicalDocument;
    use app_lite_core::document::{
        Alignment, Block as CanonicalBlock, BlockStyle, HeadingLevel, Inline, ListItem, ListKind,
        Marks,
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
}
