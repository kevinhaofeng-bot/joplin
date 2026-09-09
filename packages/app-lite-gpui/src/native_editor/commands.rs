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
    Document(DocumentError),
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ArgumentMismatch { command, expected } => {
                write!(f, "command {command:?} requires {expected}")
            }
            Self::EmptyLinkUrl => f.write_str("link URL cannot be empty"),
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
        command: EditorCommand::Undo,
        label: "Undo",
        group: 0,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::Redo,
        label: "Redo",
        group: 0,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::Paragraph,
        label: "Paragraph",
        group: 1,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::Heading1,
        label: "Heading 1",
        group: 1,
        primary: false,
    },
    CommandDescriptor {
        command: EditorCommand::Heading2,
        label: "Heading 2",
        group: 1,
        primary: false,
    },
    CommandDescriptor {
        command: EditorCommand::Heading3,
        label: "Heading 3",
        group: 1,
        primary: false,
    },
    CommandDescriptor {
        command: EditorCommand::Bold,
        label: "Bold",
        group: 2,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::Italic,
        label: "Italic",
        group: 2,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::Underline,
        label: "Underline",
        group: 2,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::Strike,
        label: "Strike",
        group: 2,
        primary: false,
    },
    CommandDescriptor {
        command: EditorCommand::Highlight,
        label: "Highlight",
        group: 2,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::BulletList,
        label: "Bulleted list",
        group: 3,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::OrderedList,
        label: "Numbered list",
        group: 3,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::CheckList,
        label: "Checklist",
        group: 3,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::Link,
        label: "Link",
        group: 4,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::AlignLeft,
        label: "Align left",
        group: 5,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::AlignCenter,
        label: "Align center",
        group: 5,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::AlignRight,
        label: "Align right",
        group: 5,
        primary: true,
    },
    CommandDescriptor {
        command: EditorCommand::IndentList,
        label: "Indent list",
        group: 6,
        primary: false,
    },
    CommandDescriptor {
        command: EditorCommand::OutdentList,
        label: "Outdent list",
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
        match command {
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
                        editor.document().blocks()[start..=end]
                            .iter()
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
                let blocks = &editor.document().blocks()[start..=end];
                if blocks.is_empty() || blocks.iter().any(|block| block.content.as_text().is_none())
                {
                    return disabled();
                }
                let type_matching = blocks
                    .iter()
                    .filter(|block| block_kind_matches_command(&block.kind, command))
                    .count();
                let exact = block_kind_for_command(command);
                let exact_matching = blocks.iter().filter(|block| block.kind == exact).count();
                return CommandState {
                    enabled: exact_matching != blocks.len(),
                    toggle: toggle_state(type_matching > 0, type_matching == blocks.len()),
                };
            }
            EditorCommand::AlignLeft | EditorCommand::AlignCenter | EditorCommand::AlignRight => {
                let Some((start, end)) = editor.selected_block_indices() else {
                    return disabled();
                };
                let blocks = &editor.document().blocks()[start..=end];
                if blocks.is_empty() || blocks.iter().any(|block| block.content.as_text().is_none())
                {
                    return disabled();
                }
                let expected = alignment_for_command(command);
                let matching = blocks
                    .iter()
                    .filter(|block| block.alignment == expected)
                    .count();
                return CommandState {
                    enabled: matching != blocks.len(),
                    toggle: toggle_state(matching > 0, matching == blocks.len()),
                };
            }
            EditorCommand::IndentList | EditorCommand::OutdentList => {
                let Some((start, end)) = editor.selected_block_indices() else {
                    return disabled();
                };
                let blocks = &editor.document().blocks()[start..=end];
                let mut applicable = 0usize;
                let mut enabled = false;
                for block in blocks {
                    let depth = list_depth(&block.kind);
                    if let Some(depth) = depth {
                        applicable += 1;
                        enabled |= match command {
                            EditorCommand::IndentList => depth < super::model::MAX_LIST_DEPTH,
                            EditorCommand::OutdentList => depth > 0,
                            _ => false,
                        };
                    }
                }
                return CommandState {
                    enabled,
                    toggle: if applicable > 0 {
                        ToggleState::Off
                    } else {
                        ToggleState::Off
                    },
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
        (EditorCommand::Link, CommandArgument::LinkUrl(url)) if !url.trim().is_empty() => {
            Ok(CommandArgument::LinkUrl(url))
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
