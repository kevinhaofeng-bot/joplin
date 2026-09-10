use crate::{NoteId, NotebookId, ResourceId, TagId};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DeletionScope {
    #[default]
    Active,
    Trash,
    All,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListQuery {
    pub notebook_id: Option<NotebookId>,
    pub tag_id: Option<TagId>,
    pub deletion_scope: DeletionScope,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteProjection {
    pub id: NoteId,
    pub title_prefix: String,
    pub snippet: String,
    pub updated_time: i64,
    pub deleted_time: Option<i64>,
    pub notebook_id: NotebookId,
    pub selected_thumbnail_id: Option<ResourceId>,
    pub attachment_count: i64,
}
