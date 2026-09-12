use crate::{BlobHash, CanonicalDocument, ResourceId};

macro_rules! opaque_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd)]
        pub struct $name(pub(crate) String);

        impl $name {
            pub fn parse(value: impl AsRef<str>) -> Result<Self, &'static str> {
                let value = value.as_ref();
                if value.len() == 32
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
                {
                    Ok(Self(value.to_owned()))
                } else {
                    Err("opaque IDs must be 32 lowercase hexadecimal characters")
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

opaque_id!(NoteId);
opaque_id!(NotebookId);
opaque_id!(StackId);
opaque_id!(TagId);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityRef {
    Note(NoteId),
    Notebook(NotebookId),
    Stack(StackId),
    Tag(TagId),
    Resource(ResourceId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notebook {
    pub id: NotebookId,
    pub title: String,
    pub stack_id: Option<StackId>,
    pub revision: i64,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stack {
    pub id: StackId,
    pub title: String,
    pub revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub id: TagId,
    pub title: String,
    pub revision: i64,
}

/// The lightweight, typed input for a library navigation tree.
///
/// This is deliberately separate from `NoteProjection`: expanding a sidebar
/// must never query a note body, snippet, thumbnail, or resource blob merely
/// to learn which durable Notebook/Stack/Tag identities can be navigated to.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LibraryNavigationIndex {
    pub notebooks: Vec<Notebook>,
    pub stacks: Vec<Stack>,
    pub tags: Vec<Tag>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredResource {
    pub id: ResourceId,
    pub sha256: BlobHash,
    pub title: String,
    pub mime: String,
    pub file_extension: String,
    pub size: i64,
    pub revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub id: NoteId,
    pub title: String,
    pub body_html: String,
    pub body_text: String,
    pub snippet: String,
    pub notebook_id: NotebookId,
    pub resource_ids: Vec<ResourceId>,
    pub tag_ids: Vec<TagId>,
    pub created_time: i64,
    pub updated_time: i64,
    pub deleted_time: Option<i64>,
    pub revision: i64,
}

/// The narrow post-organization shape for an already-mounted note. It keeps
/// route membership and tag controls current without loading canonical HTML,
/// search text, merge state, or resource bytes just to repaint a sidebar/menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteOrganizationState {
    pub id: NoteId,
    pub notebook_id: NotebookId,
    pub tag_ids: Vec<TagId>,
    pub updated_time: i64,
    pub deleted_time: Option<i64>,
    pub revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateNote {
    pub title: String,
    pub notebook_id: Option<NotebookId>,
    pub document: CanonicalDocument,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveNote {
    pub id: NoteId,
    pub expected_revision: i64,
    pub title: String,
    pub document: CanonicalDocument,
    pub resource_ids: Vec<ResourceId>,
    pub selected_thumbnail_id: Option<ResourceId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssociateResource {
    pub snapshot: SaveNote,
}

/// The exact durable journal row a retained session is allowed to compact.
///
/// A writer token alone identifies a session but not a particular checkpoint:
/// the same session may publish a newer generation while an older worker is
/// still queued. SQLite allocates `sequence` atomically with the checkpoint,
/// so the pair is a lease over one concrete row rather than a broad writer
/// permission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalOwnership {
    pub writer_token: String,
    pub sequence: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditJournalEntry {
    pub note_id: NoteId,
    /// Durable revision the delta was computed against. A later snapshot must
    /// reject an old writer rather than replaying it over a newer note.
    pub expected_revision: i64,
    /// Opaque retained-session identity. Local generations deliberately start
    /// from one for every window, so they are not a database ordering key.
    pub writer_token: String,
    /// Assigned by SQLite under the append transaction; callers never choose
    /// it and recovery orders only by this monotonic value.
    pub sequence: i64,
    pub generation: i64,
    pub delta_utf8: String,
}

impl EditJournalEntry {
    pub fn ownership(&self) -> JournalOwnership {
        JournalOwnership {
            writer_token: self.writer_token.clone(),
            sequence: self.sequence,
        }
    }
}

/// Durable Task 7 hand-off.  Delete jobs intentionally remain addressable
/// after their note row has been purged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchJob {
    pub note_id: NoteId,
    pub updated_time: i64,
    pub reason: String,
}

/// A bounded hand-off to the future platform extractor.  D3a never opens the
/// blob: the worker receives its identity and must later use the repository's
/// verified descriptor boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedTextJob {
    pub resource_id: ResourceId,
    pub sha256: BlobHash,
    pub extractor_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DerivedTextFailure {
    Unsupported,
    Unavailable,
    Parse,
    Locked,
    NoSelectableText,
    TooLarge,
    Timeout,
    Failed,
}

impl DerivedTextFailure {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::Unavailable => "unavailable",
            Self::Parse => "parse",
            Self::Locked => "locked",
            Self::NoSelectableText => "no-selectable-text",
            Self::TooLarge => "too-large",
            Self::Timeout => "timeout",
            Self::Failed => "failed",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "unsupported" => Some(Self::Unsupported),
            "unavailable" => Some(Self::Unavailable),
            "parse" => Some(Self::Parse),
            "locked" => Some(Self::Locked),
            "no-selectable-text" => Some(Self::NoSelectableText),
            "too-large" => Some(Self::TooLarge),
            "timeout" => Some(Self::Timeout),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DerivedTextStatus {
    Pending {
        attempts: i64,
    },
    Indexed {
        attempts: i64,
    },
    Failed {
        failure: DerivedTextFailure,
        attempts: i64,
    },
}
