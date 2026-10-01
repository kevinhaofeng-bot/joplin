//! Shared Evernote-style editor commands.
//!
//! The catalogue is deliberately independent of the toolbar and More menu:
//! both surfaces enumerate this same donor-inspired command table and route
//! through [`CommandCatalogue::execute`].  The native editor remains the only
//! owner of selection, focus and transactions.

use std::fmt;

use super::core::{EditorCore, TextColorState};
use super::model::{
    Affinity, Block, BlockContent, BlockKind, DocPoint, Document, DocumentError, InlineGroup, Mark,
    NodeId, Selection, TextAlignment,
};
use super::transaction::{Transaction, TransactionBatch};

/// Commands visible in the native Evernote-order editor strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EditorCommand {
    InsertImage,
    Undo,
    Redo,
    Paragraph,
    Heading1,
    Heading2,
    Heading3,
    Bold,
    Italic,
    Underline,
    Strike,
    Superscript,
    Subscript,
    TextColor,
    Highlight,
    BulletList,
    OrderedList,
    CheckList,
    Quote,
    Link,
    AlignLeft,
    AlignCenter,
    AlignRight,
    IndentList,
    OutdentList,
}

/// Optional data supplied when a command needs more than the current
/// selection.  Link is intentionally the only command that accepts an
/// argument in this spike.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandArgument {
    None,
    LinkUrl(String),
    ImagePath(std::path::PathBuf),
    /// A colour for `TextColor`; `None` returns text to the default colour.
    TextColor(Option<app_lite_core::TextColor>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToggleState {
    Off,
    On,
    Mixed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandState {
    pub enabled: bool,
    pub toggle: ToggleState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandDescriptor {
    pub command: EditorCommand,
    pub label: &'static str,
    pub label_zh: &'static str,
    pub icon_path: Option<&'static str>,
    pub group: u8,
    pub primary: bool,
}

/// Structured failures from command argument validation or the document
/// transaction layer. Validation is completed before `EditorCore` is touched,
/// so an argument error cannot consume history or partially mutate content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandError {
    ArgumentMismatch {
        command: EditorCommand,
        expected: &'static str,
    },
    EmptyLinkUrl,
    InvalidLinkUrl,
    Document(DocumentError),
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ArgumentMismatch { command, expected } => {
                write!(f, "command {command:?} requires {expected}")
            }
            Self::EmptyLinkUrl => f.write_str("link URL cannot be empty"),
            Self::InvalidLinkUrl => f.write_str("link URL is invalid"),
            Self::Document(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for CommandError {}

impl From<DocumentError> for CommandError {
    fn from(error: DocumentError) -> Self {
        Self::Document(error)
    }
}

const COMMANDS: &[CommandDescriptor] = &[
    CommandDescriptor {
        command: EditorCommand::InsertImage,
        label: "Insert image",
        label_zh: "插入图片",
        icon_path: Some("icon/editor/image.svg"),
        group: 0,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::Undo,
        label: "Undo",
        label_zh: "撤销",
        icon_path: Some("icon/editor/undo.svg"),
        group: 0,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::Redo,
        label: "Redo",
        label_zh: "重做",
        icon_path: Some("icon/editor/redo.svg"),
        group: 0,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::Paragraph,
        label: "Paragraph",
        label_zh: "正文",
        icon_path: None,
        group: 1,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::Heading1,
        label: "Heading 1",
        label_zh: "标题 1",
        icon_path: None,
        group: 1,
        primary: false,
    },
    CommandDescriptor {
        command: EditorCommand::Heading2,
        label: "Heading 2",
        label_zh: "标题 2",
        icon_path: None,
        group: 1,
        primary: false,
    },
    CommandDescriptor {
        command: EditorCommand::Heading3,
        label: "Heading 3",
        label_zh: "标题 3",
        icon_path: None,
        group: 1,
        primary: false,
    },
    CommandDescriptor {
        command: EditorCommand::Bold,
        label: "Bold",
        label_zh: "粗体",
        icon_path: Some("icon/editor/bold.svg"),
        group: 2,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::Italic,
        label: "Italic",
        label_zh: "斜体",
        icon_path: Some("icon/editor/italic.svg"),
        group: 2,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::Underline,
        label: "Underline",
        label_zh: "下划线",
        icon_path: Some("icon/editor/underline.svg"),
        group: 2,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::Strike,
        label: "Strike",
        label_zh: "删除线",
        icon_path: Some("icon/editor/strike.svg"),
        group: 2,
        primary: false,
    },
    // Evernote `FormattingBar.superscript` / `.subscript`; keymap Ctrl-Cmd-=
    // and Ctrl-Cmd-- (common-editor textformatter/keymap.ts).
    CommandDescriptor {
        command: EditorCommand::Superscript,
        label: "Superscript",
        label_zh: "上标",
        icon_path: None,
        group: 2,
        primary: false,
    },
    CommandDescriptor {
        command: EditorCommand::Subscript,
        label: "Subscript",
        label_zh: "下标",
        icon_path: None,
        group: 2,
        primary: false,
    },
    // Evernote `FormattingBar.fontColor`.
    CommandDescriptor {
        command: EditorCommand::TextColor,
        label: "Font color",
        label_zh: "字体颜色",
        icon_path: None,
        group: 2,
        primary: false,
    },
    // Evernote's quoteblock command (common-editor quoteblock/commands).
    CommandDescriptor {
        command: EditorCommand::Quote,
        label: "Quote",
        label_zh: "引用",
        icon_path: None,
        group: 3,
        primary: false,
    },
    CommandDescriptor {
        command: EditorCommand::Highlight,
        label: "Highlight",
        label_zh: "高亮",
        icon_path: Some("icon/editor/highlight.svg"),
        group: 2,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::BulletList,
        label: "Bulleted list",
        label_zh: "项目符号列表",
        icon_path: Some("icon/editor/bulleted-list.svg"),
        group: 3,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::OrderedList,
        label: "Numbered list",
        label_zh: "编号列表",
        icon_path: Some("icon/editor/ordered-list.svg"),
        group: 3,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::CheckList,
        label: "Checklist",
        label_zh: "待办事项",
        icon_path: Some("icon/editor/checklist.svg"),
        group: 3,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::Link,
        label: "Link",
        label_zh: "链接",
        icon_path: Some("icon/editor/link.svg"),
        group: 4,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::AlignLeft,
        label: "Align left",
        label_zh: "左对齐",
        icon_path: Some("icon/editor/align-left.svg"),
        group: 5,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::AlignCenter,
        label: "Align center",
        label_zh: "居中",
        icon_path: Some("icon/editor/align-center.svg"),
        group: 5,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::AlignRight,
        label: "Align right",
        label_zh: "右对齐",
        icon_path: Some("icon/editor/align-right.svg"),
        group: 5,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::IndentList,
        label: "Indent list",
        label_zh: "增加缩进",
        icon_path: Some("icon/editor/indent.svg"),
        group: 6,
        primary: false,
    },
    CommandDescriptor {
        command: EditorCommand::OutdentList,
        label: "Outdent list",
        label_zh: "减少缩进",
        icon_path: Some("icon/editor/outdent.svg"),
        group: 6,
        primary: false,
    },
];

#[derive(Clone, Copy, Debug, Default)]
pub struct CommandCatalogue;

impl CommandCatalogue {
    pub const fn new() -> Self {
        Self
    }

    pub fn descriptors(&self) -> &'static [CommandDescriptor] {
        COMMANDS
    }

    pub fn primary_descriptors(&self) -> Vec<&'static CommandDescriptor> {
        COMMANDS
            .iter()
            .filter(|descriptor| descriptor.primary)
            .collect()
    }

    pub fn more_descriptors(&self) -> Vec<&'static CommandDescriptor> {
        COMMANDS
            .iter()
            .filter(|descriptor| !descriptor.primary)
            .collect()
    }

    /// Derive the toolbar state from the real editor selection/document.
    pub fn state(&self, command: EditorCommand, editor: &EditorCore) -> CommandState {
        // Command execution is also protected by EditorCore, but a read-only
        // Task 3 surface must not advertise any durable-edit operation that
        // Task 4 has not installed yet.
        if editor.is_read_only() {
            return disabled();
        }
        match command {
            EditorCommand::InsertImage => CommandState {
                enabled: editor.selected_block_indices().is_some(),
                toggle: ToggleState::Off,
            },
            EditorCommand::Undo => return history_state(editor.undo_depth()),
            // quoteblock/commands queryCommandValue: on when any selected
            // block is inside a quote; then it unwraps, else it wraps what
            // a quote can hold.
            EditorCommand::Quote => {
                let Some((start, end)) = editor.selected_block_indices() else {
                    return disabled();
                };
                let in_quote = (start..=end).any(|index| editor.document().block_in_quote(index));
                return CommandState {
                    enabled: in_quote || editor.document().can_wrap_in_quote(editor.selection()),
                    toggle: if in_quote {
                        ToggleState::On
                    } else {
                        ToggleState::Off
                    },
                };
            }
            EditorCommand::Redo => return history_state(editor.redo_depth()),
            // Evernote forecolor.ts: selected text, or the pending style at a
            // collapsed caret. "On" means a colour other than the default.
            EditorCommand::TextColor => {
                return CommandState {
                    enabled: !editor.selected_text_ranges().is_empty() || editor.caret_in_text(),
                    toggle: match editor.selection_text_color() {
                        TextColorState::Default => ToggleState::Off,
                        TextColorState::Mixed => ToggleState::Mixed,
                        TextColorState::Color(_) => ToggleState::On,
                    },
                };
            }
            EditorCommand::Bold
            | EditorCommand::Italic
            | EditorCommand::Underline
            | EditorCommand::Strike
            | EditorCommand::Superscript
            | EditorCommand::Subscript
            | EditorCommand::Highlight
            | EditorCommand::Link => {
                let mark = mark_for_command(command);
                let (any, all) = editor.selection_mark_state(&mark);
                // Evernote marks the text of a mixed selection and leaves its
                // resources alone (ProseMirror `addMark` skips atoms).
                // Evernote `boolformat.ts`: a collapsed caret toggles the
                // pending style. A link needs selected text.
                return CommandState {
                    enabled: !editor.selected_text_ranges().is_empty()
                        || (command != EditorCommand::Link && editor.caret_in_text()),
                    toggle: toggle_state(any, all),
                };
            }
            EditorCommand::Paragraph
            | EditorCommand::Heading1
            | EditorCommand::Heading2
            | EditorCommand::Heading3
            | EditorCommand::BulletList
            | EditorCommand::OrderedList
            | EditorCommand::CheckList => {
                let Some((start, end)) = editor.selected_block_indices() else {
                    return disabled();
                };
                if start > end {
                    return disabled();
                }
                let is_list = matches!(
                    command,
                    EditorCommand::BulletList
                        | EditorCommand::OrderedList
                        | EditorCommand::CheckList
                );
                // Only a list takes in a standalone resource; a paragraph or
                // heading style skips it, as ProseMirror `setBlockType` does.
                let (count, type_matching, exact_matching) =
                    selected_items(editor.document(), start, end)
                        .iter()
                        .filter_map(|item| match item {
                            SelectedItem::Text { kind, .. }
                            | SelectedItem::Grouped { kind, .. } => Some(Some(kind)),
                            SelectedItem::Standalone { .. } => is_list.then_some(None),
                        })
                        .fold(
                            (0usize, 0usize, 0usize),
                            |(count, type_matching, exact_matching), kind| {
                                (
                                    count.saturating_add(1),
                                    type_matching.saturating_add(usize::from(kind.is_some_and(
                                        |kind| block_kind_matches_command(kind, command),
                                    ))),
                                    exact_matching.saturating_add(usize::from(kind.is_some_and(
                                        |kind| *kind == block_kind_for_command(command),
                                    ))),
                                )
                            },
                        );
                if count == 0 {
                    return disabled();
                }
                // A list command is never a no-op: it converts, or toggles
                // an all-matching list back to paragraphs.
                return CommandState {
                    enabled: is_list || exact_matching != count,
                    toggle: toggle_state(type_matching > 0, type_matching == count),
                };
            }
            EditorCommand::AlignLeft | EditorCommand::AlignCenter | EditorCommand::AlignRight => {
                let Some((start, end)) = editor.selected_block_indices() else {
                    return disabled();
                };
                if start > end {
                    return disabled();
                }
                let expected = alignment_for_command(command);
                let (count, text_only, matching) = editor
                    .document()
                    .blocks()
                    .iter_range(start..end.saturating_add(1))
                    .fold(
                        (0usize, true, 0usize),
                        |(count, text_only, matching), block| {
                            (
                                count.saturating_add(1),
                                text_only && block.content.as_text().is_some(),
                                matching.saturating_add(usize::from(block.alignment == expected)),
                            )
                        },
                    );
                if count == 0 || !text_only {
                    return disabled();
                }
                return CommandState {
                    enabled: matching != count,
                    toggle: toggle_state(matching > 0, matching == count),
                };
            }
            EditorCommand::IndentList | EditorCommand::OutdentList => {
                let Some((start, end)) = editor.selected_block_indices() else {
                    return disabled();
                };
                if start > end {
                    return disabled();
                }
                let mut blocks = editor
                    .document()
                    .blocks()
                    .iter_range(start..end.saturating_add(1));
                // List-depth transactions are intentionally atomic across the
                // whole block selection.  A mixed selection must therefore be
                // disabled whenever one item cannot take the same operation;
                // reporting enabled for only the applicable subset would make
                // a toolbar click deterministically return a transaction error.
                let enabled = blocks.all(|block| {
                    list_depth(&block.kind).is_some_and(|depth| match command {
                        EditorCommand::IndentList => depth < super::model::MAX_LIST_DEPTH,
                        EditorCommand::OutdentList => depth > 0,
                        _ => false,
                    })
                });
                return CommandState {
                    enabled,
                    toggle: ToggleState::Off,
                };
            }
        }
    }

    /// Execute one command through `EditorCore`, which in turn routes all
    /// changes through the Task 3 transaction/history engine.
    pub fn execute(
        &self,
        command: EditorCommand,
        argument: CommandArgument,
        editor: &mut EditorCore,
    ) -> Result<(), CommandError> {
        let argument = validate_argument(command, argument)?;
        let selection = editor.selection();
        match command {
            EditorCommand::InsertImage => {
                let CommandArgument::ImagePath(path) = argument else {
                    unreachable!("validate_argument checked InsertImage's argument");
                };
                editor.insert_image_path(&path)?;
            }
            EditorCommand::Undo => editor.undo()?,
            EditorCommand::Redo => editor.redo()?,
            EditorCommand::BulletList | EditorCommand::OrderedList | EditorCommand::CheckList => {
                apply_list_command(command, editor)?;
            }
            EditorCommand::Quote => {
                let quote = self.state(EditorCommand::Quote, editor).toggle != ToggleState::On;
                editor.apply(Transaction::SetQuote { selection, quote })?;
            }
            EditorCommand::Paragraph
            | EditorCommand::Heading1
            | EditorCommand::Heading2
            | EditorCommand::Heading3 => {
                editor.apply(Transaction::SetBlockKind {
                    selection,
                    kind: block_kind_for_command(command),
                })?;
            }
            EditorCommand::Bold
            | EditorCommand::Italic
            | EditorCommand::Underline
            | EditorCommand::Strike
            | EditorCommand::Superscript
            | EditorCommand::Subscript
            | EditorCommand::Highlight => {
                editor.apply(Transaction::ToggleMark {
                    selection,
                    mark: mark_for_command(command),
                })?;
            }
            EditorCommand::Link => {
                let CommandArgument::LinkUrl(url) = argument else {
                    unreachable!("validate_argument checked Link's argument");
                };
                editor.apply(Transaction::SetLink {
                    selection,
                    url: Some(url),
                })?;
            }
            EditorCommand::AlignLeft | EditorCommand::AlignCenter | EditorCommand::AlignRight => {
                editor.apply(Transaction::SetAlignment {
                    selection,
                    alignment: alignment_for_command(command),
                })?;
            }
            EditorCommand::IndentList => {
                editor.apply(Transaction::IndentList { selection })?;
            }
            EditorCommand::OutdentList => {
                editor.apply(Transaction::OutdentList { selection })?;
            }
            EditorCommand::TextColor => {
                let CommandArgument::TextColor(color) = argument else {
                    unreachable!("validate_argument checked TextColor's argument");
                };
                editor.apply(Transaction::SetTextColor { selection, color })?;
            }
        }
        Ok(())
    }
}

fn validate_argument(
    command: EditorCommand,
    argument: CommandArgument,
) -> Result<CommandArgument, CommandError> {
    match (command, argument) {
        (EditorCommand::InsertImage, CommandArgument::ImagePath(path)) => {
            Ok(CommandArgument::ImagePath(path))
        }
        (EditorCommand::InsertImage, _) => Err(CommandError::ArgumentMismatch {
            command,
            expected: "CommandArgument::ImagePath",
        }),
        (EditorCommand::Link, CommandArgument::LinkUrl(url)) if !url.trim().is_empty() => {
            let url = url.trim();
            let parsed = url::Url::parse(url).map_err(|_| CommandError::InvalidLinkUrl)?;
            if matches!(parsed.scheme(), "http" | "https") && parsed.has_host() {
                Ok(CommandArgument::LinkUrl(url.to_owned()))
            } else {
                Err(CommandError::InvalidLinkUrl)
            }
        }
        (EditorCommand::Link, CommandArgument::LinkUrl(_)) => Err(CommandError::EmptyLinkUrl),
        (EditorCommand::Link, CommandArgument::None) => Err(CommandError::ArgumentMismatch {
            command,
            expected: "CommandArgument::LinkUrl",
        }),
        (EditorCommand::TextColor, CommandArgument::TextColor(color)) => {
            Ok(CommandArgument::TextColor(color))
        }
        (EditorCommand::TextColor, _) => Err(CommandError::ArgumentMismatch {
            command,
            expected: "CommandArgument::TextColor",
        }),
        (_, CommandArgument::None) => Ok(CommandArgument::None),
        (_, CommandArgument::LinkUrl(_)) => Err(CommandError::ArgumentMismatch {
            command,
            expected: "CommandArgument::None",
        }),
        (_, CommandArgument::ImagePath(_)) => Err(CommandError::ArgumentMismatch {
            command,
            expected: "CommandArgument::None",
        }),
        (_, CommandArgument::TextColor(_)) => Err(CommandError::ArgumentMismatch {
            command,
            expected: "CommandArgument::None",
        }),
    }
}

fn history_state(depth: usize) -> CommandState {
    CommandState {
        enabled: depth > 0,
        toggle: if depth > 0 {
            ToggleState::On
        } else {
            ToggleState::Off
        },
    }
}

fn disabled() -> CommandState {
    CommandState {
        enabled: false,
        toggle: ToggleState::Off,
    }
}

fn toggle_state(any: bool, all: bool) -> ToggleState {
    if all {
        ToggleState::On
    } else if any {
        ToggleState::Mixed
    } else {
        ToggleState::Off
    }
}

fn block_kind_for_command(command: EditorCommand) -> BlockKind {
    match command {
        EditorCommand::Paragraph => BlockKind::Paragraph,
        EditorCommand::Heading1 => BlockKind::Heading { level: 1 },
        EditorCommand::Heading2 => BlockKind::Heading { level: 2 },
        EditorCommand::Heading3 => BlockKind::Heading { level: 3 },
        EditorCommand::BulletList => BlockKind::BulletItem { depth: 0 },
        EditorCommand::OrderedList => BlockKind::OrderedItem { depth: 0 },
        EditorCommand::CheckList => BlockKind::CheckItem {
            depth: 0,
            checked: false,
        },
        _ => unreachable!("{command:?} is not a block-kind command"),
    }
}

fn block_kind_matches_command(kind: &BlockKind, command: EditorCommand) -> bool {
    match (kind, command) {
        (BlockKind::Paragraph, EditorCommand::Paragraph) => true,
        (BlockKind::Heading { level: 1 }, EditorCommand::Heading1) => true,
        (BlockKind::Heading { level: 2 }, EditorCommand::Heading2) => true,
        (BlockKind::Heading { level: 3 }, EditorCommand::Heading3) => true,
        (BlockKind::BulletItem { .. }, EditorCommand::BulletList) => true,
        (BlockKind::OrderedItem { .. }, EditorCommand::OrderedList) => true,
        (BlockKind::CheckItem { .. }, EditorCommand::CheckList) => true,
        _ => false,
    }
}

fn mark_for_command(command: EditorCommand) -> Mark {
    match command {
        EditorCommand::Bold => Mark::Bold,
        EditorCommand::Italic => Mark::Italic,
        EditorCommand::Underline => Mark::Underline,
        EditorCommand::Strike => Mark::Strike,
        EditorCommand::Highlight => Mark::Highlight,
        EditorCommand::Superscript => Mark::Superscript,
        EditorCommand::Subscript => Mark::Subscript,
        EditorCommand::Link => Mark::Link(String::new()),
        _ => unreachable!("{command:?} is not a mark command"),
    }
}

fn alignment_for_command(command: EditorCommand) -> TextAlignment {
    match command {
        EditorCommand::AlignLeft => TextAlignment::Left,
        EditorCommand::AlignCenter => TextAlignment::Center,
        EditorCommand::AlignRight => TextAlignment::Right,
        _ => unreachable!("{command:?} is not an alignment command"),
    }
}

/// Evernote `list/list.ts::insertListAtSelection`: when every selected block
/// is already this list type the list is removed (`removeList`); otherwise
/// each block converts, and existing list items keep their nesting
/// (`insertOrToggleList` uses `setNodeMarkup`). One undo entry.
///
/// `insertOrToggleList` also wraps a selected resource in its own list item
/// (`applyIndent(tr, pos, listType, checked, false)`). Here that item is the
/// shape a saved `<li><img></li>` opens as: an empty list row grouped with the
/// resource. Removing the list unwraps such an item back to the resource.
fn apply_list_command(command: EditorCommand, editor: &mut EditorCore) -> Result<(), CommandError> {
    let Some((start, end)) = editor.selected_block_indices() else {
        return Ok(());
    };
    let document = editor.document();
    let items = selected_items(document, start, end);
    let remove = !items.is_empty()
        && items.iter().all(|item| match item {
            SelectedItem::Text { kind, .. } | SelectedItem::Grouped { kind, .. } => {
                block_kind_matches_command(kind, command)
            }
            SelectedItem::Standalone { .. } => false,
        });
    let unwrapped: Vec<_> = if remove {
        document
            .inline_groups()
            .iter()
            .filter(|group| {
                let mut members = group.members.iter().filter_map(|id| document.block(*id));
                group.members.iter().any(|id| {
                    document
                        .node_index(*id)
                        .is_ok_and(|index| (start..=end).contains(&index))
                }) && members.all(|block| block.content.as_text().is_none_or(str::is_empty))
            })
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    // A selected resource retypes its item through the item's text row.
    let mut rows = Vec::new();
    for item in &items {
        let row = match item {
            SelectedItem::Text { id, kind, len } => Some((*id, kind.clone(), *len)),
            SelectedItem::Grouped {
                kind,
                row: Some((id, len)),
            } => Some((*id, kind.clone(), *len)),
            _ => None,
        };
        if let Some(row) = row
            && !rows.iter().any(|(id, _, _)| *id == row.0)
        {
            rows.push(row);
        }
    }
    let mut transactions: Vec<_> = rows
        .iter()
        .filter_map(|(id, kind, len)| {
            if unwrapped.iter().any(|group| group.members.contains(id)) {
                return None;
            }
            let target = if remove {
                BlockKind::Paragraph
            } else {
                list_target(command, kind)
            };
            (target != *kind).then(|| Transaction::SetBlockKind {
                selection: Selection::new(DocPoint::new(*id, 0), DocPoint::new(*id, *len)),
                kind: target,
            })
        })
        .collect();
    let mut after = editor.selection();
    if remove {
        let mut removed_rows = Vec::new();
        for group in &unwrapped {
            let Some(resource) = group.members.iter().copied().find(|id| {
                document
                    .block(*id)
                    .is_some_and(|block| block.content.as_text().is_none())
            }) else {
                continue;
            };
            transactions.push(Transaction::RestoreInlineGroups {
                remove: group.members.first().copied().into_iter().collect(),
                groups: Vec::new(),
            });
            for id in &group.members {
                if document
                    .block(*id)
                    .is_some_and(|block| block.content.as_text().is_some())
                {
                    removed_rows.push((document.node_index(*id)?, *id, resource));
                }
            }
        }
        removed_rows.sort_by(|left, right| right.0.cmp(&left.0));
        for (index, id, resource) in removed_rows {
            transactions.push(Transaction::RestoreBlocks {
                index,
                remove_count: 1,
                blocks: Vec::new(),
            });
            for point in [&mut after.anchor, &mut after.head] {
                if point.node_id == id {
                    *point = DocPoint::with_affinity(resource, 0, Affinity::Before);
                }
            }
        }
    } else {
        let standalone: Vec<_> = items
            .iter()
            .filter_map(|item| match item {
                SelectedItem::Standalone { id, index } => Some((*index, *id)),
                _ => None,
            })
            .collect();
        let rows = document.unused_node_ids(standalone.len())?;
        let kind = list_target(command, &BlockKind::Paragraph);
        for ((index, resource), row) in standalone.into_iter().zip(rows).rev() {
            transactions.push(Transaction::RestoreBlocks {
                index,
                remove_count: 0,
                blocks: vec![Block {
                    id: row,
                    kind: kind.clone(),
                    content: BlockContent::text(""),
                    alignment: TextAlignment::Left,
                    quoted: false,
                    quote_start: false,
                    revision: 0,
                }],
            });
            transactions.push(Transaction::RestoreInlineGroups {
                remove: Vec::new(),
                groups: vec![InlineGroup {
                    kind: kind.clone(),
                    members: vec![row, resource],
                }],
            });
        }
    }
    if !transactions.is_empty() {
        editor.apply_batch_then_select(TransactionBatch(transactions), after)?;
    }
    Ok(())
}

fn list_target(command: EditorCommand, kind: &BlockKind) -> BlockKind {
    let depth = list_depth(kind).unwrap_or(0);
    match command {
        EditorCommand::BulletList => BlockKind::BulletItem { depth },
        EditorCommand::OrderedList => BlockKind::OrderedItem { depth },
        _ => BlockKind::CheckItem {
            depth,
            checked: match kind {
                BlockKind::CheckItem { checked, .. } => *checked,
                _ => false,
            },
        },
    }
}

/// What a block/list command sees in the selected block range. A grouped
/// resource takes its item's style; a table is neither text nor a resource
/// Evernote lists wrap here.
enum SelectedItem {
    Text {
        id: NodeId,
        kind: BlockKind,
        len: usize,
    },
    /// `row` is the item's text row, which carries its list type.
    Grouped {
        kind: BlockKind,
        row: Option<(NodeId, usize)>,
    },
    Standalone {
        id: NodeId,
        index: usize,
    },
}

fn selected_items(document: &Document, start: usize, end: usize) -> Vec<SelectedItem> {
    document
        .blocks()
        .iter_range(start..end.saturating_add(1))
        .enumerate()
        .filter_map(|(offset, block)| {
            if let Some(text) = block.content.as_text() {
                return Some(SelectedItem::Text {
                    id: block.id,
                    kind: block.kind.clone(),
                    len: text.len(),
                });
            }
            if !matches!(
                block.content,
                BlockContent::Image { .. } | BlockContent::Attachment { .. }
            ) {
                return None;
            }
            Some(
                match document
                    .inline_groups()
                    .iter()
                    .find(|group| group.members.contains(&block.id))
                {
                    Some(group) => SelectedItem::Grouped {
                        kind: group.kind.clone(),
                        row: group.members.iter().find_map(|id| {
                            let text = document.block(*id)?.content.as_text()?;
                            Some((*id, text.len()))
                        }),
                    },
                    None => SelectedItem::Standalone {
                        id: block.id,
                        index: start + offset,
                    },
                },
            )
        })
        .collect()
}

fn list_depth(kind: &BlockKind) -> Option<u8> {
    match kind {
        BlockKind::BulletItem { depth }
        | BlockKind::OrderedItem { depth }
        | BlockKind::CheckItem { depth, .. } => Some(*depth),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{CommandArgument, CommandCatalogue, CommandError, EditorCommand};
    use crate::native_editor::core::EditorCore;
    use crate::native_editor::images::ClipboardPayload;
    use crate::native_editor::model::{Document, DocumentError};
    use gpui::AppContext;

    #[gpui::test]
    fn read_only_catalogue_disables_every_mutating_command(cx: &mut gpui::TestAppContext) {
        // A future toolbar must not advertise an operation that the Task 4
        // read-only core will reject. This fails if the access capability is
        // ignored while deriving command state.
        let editor =
            cx.new(|cx| EditorCore::new_read_only(Document::from_paragraph("只读正文"), cx));
        editor.update(cx, |editor, _| editor.select_all());
        let catalogue = CommandCatalogue::new();

        editor.read_with(cx, |editor, _| {
            for descriptor in catalogue.descriptors() {
                assert!(
                    !catalogue.state(descriptor.command, editor).enabled,
                    "{command:?} must be disabled for a read-only document",
                    command = descriptor.command,
                );
            }
        });
    }

    #[gpui::test]
    fn read_only_catalogue_execution_cannot_mutate_document_or_history(
        cx: &mut gpui::TestAppContext,
    ) {
        // This catches a bypass where a future menu calls `execute` directly
        // instead of checking the disabled toolbar state first.
        let editor =
            cx.new(|cx| EditorCore::new_read_only(Document::from_paragraph("只读正文"), cx));
        editor.update(cx, |editor, _| editor.select_all());
        let before = editor.read_with(cx, |editor, _| {
            (
                editor.document().semantic_snapshot(),
                editor.selection(),
                editor.undo_depth(),
                editor.redo_depth(),
            )
        });
        let catalogue = CommandCatalogue::new();

        editor.update(cx, |editor, _| {
            for descriptor in catalogue.descriptors() {
                let argument = match descriptor.command {
                    EditorCommand::InsertImage => {
                        CommandArgument::ImagePath("/tmp/read-only-drop.png".into())
                    }
                    EditorCommand::Link => {
                        CommandArgument::LinkUrl("https://example.com/read-only".into())
                    }
                    EditorCommand::TextColor => {
                        CommandArgument::TextColor(app_lite_core::TextColor::parse("red"))
                    }
                    _ => CommandArgument::None,
                };
                assert_eq!(
                    catalogue.execute(descriptor.command, argument, editor),
                    Err(CommandError::Document(DocumentError::ReadOnly)),
                    "{command:?} must stop at the read-only core gate",
                    command = descriptor.command,
                );
            }
        });

        let after = editor.read_with(cx, |editor, _| {
            (
                editor.document().semantic_snapshot(),
                editor.selection(),
                editor.undo_depth(),
                editor.redo_depth(),
            )
        });
        assert_eq!(after, before);
    }

    #[gpui::test]
    fn insert_image_command_commits_a_real_structural_image_and_undo_restores_text(
        cx: &mut gpui::TestAppContext,
    ) {
        let path = std::env::temp_dir().join(format!(
            "joplin-lite-command-image-{}.png",
            uuid::Uuid::new_v4()
        ));
        let bytes = ClipboardPayload::fixture_with_png_and_text("ignored")
            .images
            .into_iter()
            .next()
            .expect("fixture PNG")
            .bytes;
        std::fs::write(&path, bytes).expect("fixture path");
        let mut editor = EditorCore::for_test("前后", cx);
        editor.set_caret_utf8("前".len());
        let selection_before = editor.selection();
        let history_before = editor.undo_depth();

        CommandCatalogue::new()
            .execute(
                EditorCommand::InsertImage,
                CommandArgument::ImagePath(path.clone()),
                &mut editor,
            )
            .expect("typed image command must use EditorCore insertion");

        assert_eq!(editor.copy_all_plain_text(), "前\n\u{fffc}\n后");
        assert_ne!(editor.selection(), selection_before);
        assert_eq!(editor.undo_depth(), history_before + 1);
        editor.undo().expect("catalogue insertion must be undoable");
        assert_eq!(editor.copy_all_plain_text(), "前后");
        let _ = std::fs::remove_file(path);
    }

    #[gpui::test]
    fn insert_image_command_rejects_unsupported_and_mismatched_arguments_without_history(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut editor = EditorCore::for_test("前后", cx);
        editor.set_caret_utf8("前".len());
        let baseline = (
            editor.copy_all_plain_text(),
            editor.selection(),
            editor.undo_depth(),
        );
        let catalogue = CommandCatalogue::new();

        assert!(matches!(
            catalogue.execute(
                EditorCommand::InsertImage,
                CommandArgument::None,
                &mut editor,
            ),
            Err(CommandError::ArgumentMismatch { .. })
        ));
        let unsupported = std::env::temp_dir().join(format!(
            "joplin-lite-command-image-{}.txt",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&unsupported, "not an image").expect("unsupported fixture");
        assert!(matches!(
            catalogue.execute(
                EditorCommand::InsertImage,
                CommandArgument::ImagePath(unsupported.clone()),
                &mut editor,
            ),
            Err(CommandError::Document(_))
        ));
        assert_eq!(
            (
                editor.copy_all_plain_text(),
                editor.selection(),
                editor.undo_depth()
            ),
            baseline
        );
        let _ = std::fs::remove_file(unsupported);
    }

    const IMAGE: &str = r#"<img src=":/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" alt="图">"#;
    const ATTACHMENT: &str = r#"<a data-joplin-lite-inline-attachment="true" href=":/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" data-filename="报告.pdf" data-media-type="application/pdf">报告.pdf</a>"#;

    fn canonical(html: &str) -> String {
        app_lite_core::document::CanonicalDocument::parse_html(html)
            .unwrap()
            .to_canonical_html()
            .as_str()
            .to_owned()
    }

    fn saved(editor: &EditorCore) -> String {
        crate::native_editor::codec::export_canonical(editor.document())
            .unwrap()
            .to_canonical_html()
            .as_str()
            .to_owned()
    }

    fn opened(html: &str, cx: &mut gpui::TestAppContext) -> EditorCore {
        let document = app_lite_core::document::CanonicalDocument::parse_html(html).unwrap();
        EditorCore::from_document(
            crate::native_editor::codec::import_canonical(&document).unwrap(),
            cx,
        )
    }

    #[gpui::test]
    fn a_list_takes_in_selected_resources_saves_reopens_undoes_and_toggles_back(
        cx: &mut gpui::TestAppContext,
    ) {
        // Evernote `list.ts::insertOrToggleList` wraps a selected resource in
        // its own list item (`applyIndent(..., false)`) beside the paragraphs.
        let source = format!("<p>甲</p><p>{IMAGE}</p><p>{ATTACHMENT}</p><p>乙</p>");
        for (command, list) in [
            (EditorCommand::BulletList, "ul"),
            (EditorCommand::OrderedList, "ol"),
        ] {
            let mut editor = opened(&source, cx);
            let original = editor.document().semantic_snapshot();
            editor.select_all();
            let catalogue = CommandCatalogue::new();
            assert!(catalogue.state(command, &editor).enabled);
            catalogue
                .execute(command, CommandArgument::None, &mut editor)
                .unwrap();
            let expected = canonical(&format!(
                "<{list}><li>甲</li><li>{IMAGE}</li><li>{ATTACHMENT}</li><li>乙</li></{list}>"
            ));
            assert_eq!(saved(&editor), expected);
            assert_eq!(editor.undo_depth(), 1, "one command is one undo step");
            if command == EditorCommand::OrderedList {
                let numbers =
                    crate::native_editor::layout::ordered_number_summary(editor.document());
                let mut shown: Vec<_> = numbers.values().copied().collect();
                shown.sort();
                assert_eq!(shown, vec![1, 2, 3, 4], "each resource item takes a number");
            }
            let wrapped = editor.document().semantic_snapshot();

            let mut reopened = opened(&expected, cx);
            assert_eq!(
                saved(&reopened),
                expected,
                "a reopened list saves unchanged"
            );
            reopened.select_all();
            assert_eq!(
                catalogue.state(command, &reopened).toggle,
                super::ToggleState::On
            );
            catalogue
                .execute(command, CommandArgument::None, &mut reopened)
                .unwrap();
            assert_eq!(saved(&reopened), saved(&opened(&source, cx)));

            editor.undo().unwrap();
            assert_eq!(editor.document().semantic_snapshot(), original);
            editor.redo().unwrap();
            assert_eq!(editor.document().semantic_snapshot(), wrapped);
            editor.select_all();
            catalogue
                .execute(command, CommandArgument::None, &mut editor)
                .unwrap();
            assert_eq!(editor.document().semantic_snapshot(), original);
            assert!(
                editor
                    .document()
                    .block(editor.selection().head.node_id)
                    .is_some()
            );
            editor.undo().unwrap();
            assert_eq!(editor.document().semantic_snapshot(), wrapped);
        }
    }

    #[gpui::test]
    fn a_selected_list_resource_alone_changes_its_own_item_type(cx: &mut gpui::TestAppContext) {
        // `insertOrToggleList` retypes existing list items with `setNodeMarkup`,
        // whichever child of the item the selection covers.
        for resource in [IMAGE, ATTACHMENT] {
            let mut editor = opened(&format!("<ul><li>甲</li><li>{resource}</li></ul>"), cx);
            let original = editor.document().semantic_snapshot();
            let atom = editor
                .document()
                .blocks()
                .iter()
                .find(|block| block.content.as_text().is_none())
                .unwrap()
                .id;
            let catalogue = CommandCatalogue::new();
            for (command, list) in [
                (EditorCommand::OrderedList, "<ol><li>{r}</li></ol>"),
                (
                    EditorCommand::CheckList,
                    r#"<ul data-type="checklist"><li data-checked="false">{r}</li></ul>"#,
                ),
            ] {
                editor.set_selection_for_test(crate::native_editor::model::Selection::new(
                    crate::native_editor::model::DocPoint::with_affinity(
                        atom,
                        0,
                        crate::native_editor::model::Affinity::Before,
                    ),
                    crate::native_editor::model::DocPoint::with_affinity(
                        atom,
                        0,
                        crate::native_editor::model::Affinity::After,
                    ),
                ));
                assert!(catalogue.state(command, &editor).enabled);
                let depth = editor.undo_depth();
                catalogue
                    .execute(command, CommandArgument::None, &mut editor)
                    .unwrap();
                assert_eq!(editor.undo_depth(), depth + 1, "{command:?} is one step");
                let expected = saved(&opened(
                    &format!("<ul><li>甲</li></ul>{}", list.replace("{r}", resource)),
                    cx,
                ));
                assert_eq!(saved(&editor), expected, "{command:?}");
                assert_eq!(
                    saved(&opened(&expected, cx)),
                    expected,
                    "reopened {command:?}"
                );
            }
            editor.undo().unwrap();
            editor.undo().unwrap();
            assert_eq!(editor.document().semantic_snapshot(), original);
        }
    }

    #[gpui::test]
    fn text_styles_over_a_mixed_selection_change_only_its_text(cx: &mut gpui::TestAppContext) {
        // ProseMirror `addMark` and `setBlockType`, which Evernote's editor
        // commands use, apply to text and skip resource nodes.
        let source = format!("<p>甲</p><p>{IMAGE}</p><p>乙</p>");
        for (command, expected) in [
            (
                EditorCommand::Bold,
                format!("<p><strong>甲</strong></p><p>{IMAGE}</p><p><strong>乙</strong></p>"),
            ),
            (
                EditorCommand::Heading1,
                format!("<h1>甲</h1><p>{IMAGE}</p><h1>乙</h1>"),
            ),
        ] {
            let mut editor = opened(&source, cx);
            let original = editor.document().semantic_snapshot();
            editor.select_all();
            let catalogue = CommandCatalogue::new();
            assert!(catalogue.state(command, &editor).enabled, "{command:?}");
            catalogue
                .execute(command, CommandArgument::None, &mut editor)
                .unwrap();
            assert_eq!(saved(&editor), saved(&opened(&expected, cx)), "{command:?}");
            editor.undo().unwrap();
            assert_eq!(
                editor.document().semantic_snapshot(),
                original,
                "{command:?}"
            );
        }
    }

    #[gpui::test]
    fn a_resource_alone_takes_a_list_but_no_text_style(cx: &mut gpui::TestAppContext) {
        let mut editor = opened(&format!("<p>{IMAGE}</p>"), cx);
        editor.select_all();
        let catalogue = CommandCatalogue::new();
        for command in [
            EditorCommand::Bold,
            EditorCommand::Heading1,
            EditorCommand::Paragraph,
        ] {
            assert!(!catalogue.state(command, &editor).enabled, "{command:?}");
        }
        catalogue
            .execute(EditorCommand::CheckList, CommandArgument::None, &mut editor)
            .unwrap();
        let saved_list = saved(&editor);
        assert_eq!(saved(&opened(&saved_list, cx)), saved_list);
        assert!(saved_list.contains("<img "));
        assert_eq!(
            catalogue.state(EditorCommand::CheckList, &editor).toggle,
            super::ToggleState::On
        );
    }
}
