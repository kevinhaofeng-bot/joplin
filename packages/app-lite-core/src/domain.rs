use crate::{CanonicalDocument, ResourceId};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredResource {
    pub id: ResourceId,
    pub sha256: String,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateNote {
    pub title: String,
    pub notebook_id: Option<NotebookId>,
    pub document: CanonicalDocument,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveNote {
    pub id: NoteId,
    pub title: String,
    pub document: CanonicalDocument,
    pub resource_ids: Vec<ResourceId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditJournalEntry {
    pub note_id: NoteId,
    pub generation: i64,
    pub delta_utf8: String,
}
