//! Shared Evernote-style editor commands.
//!
//! The catalogue is deliberately independent of the toolbar and More menu:
//! both surfaces enumerate this same donor-inspired command table and route
//! through [`CommandCatalogue::execute`].  The native editor remains the only
//! owner of selection, focus and transactions.

use std::fmt;

use super::core::EditorCore;
use super::model::{BlockKind, DocumentError, Mark, TextAlignment};
use super::transaction::Transaction;

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
    Highlight,
    BulletList,
    OrderedList,
    CheckList,
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
            EditorCommand::Redo => return history_state(editor.redo_depth()),
            EditorCommand::Bold
            | EditorCommand::Italic
            | EditorCommand::Underline
            | EditorCommand::Strike
            | EditorCommand::Highlight
            | EditorCommand::Link => {
                let mark = mark_for_command(command);
                let (any, all) = editor.selection_mark_state(&mark);
                let text_ranges = editor.selected_text_ranges();
                let text_only_selection = editor
                    .selected_block_indices()
                    .map(|(start, end)| {
                        editor
                            .document()
                            .blocks()
                            .iter_range(start..end.saturating_add(1))
                            .all(|block| block.content.as_text().is_some())
                    })
                    .unwrap_or(false);
                return CommandState {
                    enabled: text_only_selection && !text_ranges.is_empty(),
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
                let (count, text_only, type_matching, exact_matching) = editor
                    .document()
                    .blocks()
                    .iter_range(start..end.saturating_add(1))
                    .fold(
                        (0usize, true, 0usize, 0usize),
                        |(count, text_only, type_matching, exact_matching), block| {
                            (
                                count.saturating_add(1),
                                text_only && block.content.as_text().is_some(),
                                type_matching.saturating_add(usize::from(
                                    block_kind_matches_command(&block.kind, command),
                                )),
                                exact_matching.saturating_add(usize::from(
                                    block.kind == block_kind_for_command(command),
                                )),
                            )
                        },
                    );
                if count == 0 || !text_only {
                    return disabled();
                }
                return CommandState {
                    enabled: exact_matching != count,
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
            EditorCommand::Paragraph
            | EditorCommand::Heading1
            | EditorCommand::Heading2
            | EditorCommand::Heading3
            | EditorCommand::BulletList
            | EditorCommand::OrderedList
            | EditorCommand::CheckList => {
                editor.apply(Transaction::SetBlockKind {
                    selection,
                    kind: block_kind_for_command(command),
                })?;
            }
            EditorCommand::Bold
            | EditorCommand::Italic
            | EditorCommand::Underline
            | EditorCommand::Strike
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
        (_, CommandArgument::None) => Ok(CommandArgument::None),
        (_, CommandArgument::LinkUrl(_)) => Err(CommandError::ArgumentMismatch {
            command,
            expected: "CommandArgument::None",
        }),
        (_, CommandArgument::ImagePath(_)) => Err(CommandError::ArgumentMismatch {
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
}
