use crate::{NoteId, NotebookId, ResourceId, StackId, TagId};
use rusqlite::types::Value;
use std::collections::BTreeSet;
use thiserror::Error;

/// The current library location. A route owns filtering semantics; callers
/// never infer a route from a sidebar label or a transient card index.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum LibraryRoute {
    AllNotes,
    Notebook(NotebookId),
    Stack(StackId),
    /// Tags are canonicalized into a sorted, duplicate-free AND set.
    Tags(BTreeSet<TagId>),
    Trash,
}

impl Default for LibraryRoute {
    fn default() -> Self {
        Self::AllNotes
    }
}

impl LibraryRoute {
    pub fn tags(tag_ids: Vec<TagId>) -> Result<Self, ListQueryError> {
        let tag_ids = tag_ids.into_iter().collect::<BTreeSet<_>>();
        if tag_ids.is_empty() {
            return Err(ListQueryError::EmptyTagIntersection);
        }
        Ok(Self::Tags(tag_ids))
    }

    pub const fn default_sort(&self) -> SortSpec {
        match self {
            Self::Trash => SortSpec::DELETED_DESCENDING,
            Self::AllNotes | Self::Notebook(_) | Self::Stack(_) | Self::Tags(_) => {
                SortSpec::UPDATED_DESCENDING
            }
        }
    }
}

/// A sortable lightweight note-list column. These are deliberately limited to
/// indexed card metadata: canonical documents and resource bytes are not valid
/// list-query inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortField {
    Updated,
    Deleted,
    Title,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortSpec {
    field: SortField,
    direction: SortDirection,
}

impl SortSpec {
    pub const UPDATED_DESCENDING: Self = Self {
        field: SortField::Updated,
        direction: SortDirection::Descending,
    };
    pub const DELETED_DESCENDING: Self = Self {
        field: SortField::Deleted,
        direction: SortDirection::Descending,
    };

    pub const fn new(field: SortField, direction: SortDirection) -> Self {
        Self { field, direction }
    }

    pub const fn title_ascending() -> Self {
        Self::new(SortField::Title, SortDirection::Ascending)
    }

    pub const fn title_descending() -> Self {
        Self::new(SortField::Title, SortDirection::Descending)
    }

    pub const fn field(self) -> SortField {
        self.field
    }

    pub const fn direction(self) -> SortDirection {
        self.direction
    }
}

/// A SQL-bounded view over the stable `NoteProjection` shape. `for_route` is
/// retained for the existing virtual-list bootstrap path; new list consumers
/// should use `paged` so a viewport never accidentally compiles an unbounded
/// result set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListQuery {
    route: LibraryRoute,
    sort: SortSpec,
    offset: usize,
    limit: Option<usize>,
}

impl Default for ListQuery {
    fn default() -> Self {
        Self::for_route(LibraryRoute::AllNotes)
    }
}

impl ListQuery {
    pub const MAX_PAGE_SIZE: usize = 500;

    pub fn for_route(route: LibraryRoute) -> Self {
        Self {
            sort: route.default_sort(),
            route,
            offset: 0,
            limit: None,
        }
    }

    pub fn paged(
        route: LibraryRoute,
        sort: SortSpec,
        offset: usize,
        limit: usize,
    ) -> Result<Self, ListQueryError> {
        if limit == 0 || limit > Self::MAX_PAGE_SIZE {
            return Err(ListQueryError::PageLimitOutOfRange {
                limit,
                maximum: Self::MAX_PAGE_SIZE,
            });
        }
        Ok(Self {
            route,
            sort,
            offset,
            limit: Some(limit),
        })
    }

    pub const fn route(&self) -> &LibraryRoute {
        &self.route
    }

    pub const fn sort(&self) -> SortSpec {
        self.sort
    }

    pub const fn offset(&self) -> usize {
        self.offset
    }

    pub const fn limit(&self) -> Option<usize> {
        self.limit
    }

    pub fn with_sort(mut self, sort: SortSpec) -> Self {
        self.sort = sort;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ListQueryError {
    #[error("a tag route must contain at least one tag")]
    EmptyTagIntersection,
    #[error("note-list page limit {limit} is outside 1..={maximum}")]
    PageLimitOutOfRange { limit: usize, maximum: usize },
    #[error("note-list offset or limit exceeds SQLite's signed range")]
    SqlIntegerOverflow,
}

/// The repository executes this compiled shape directly. It is intentionally
/// private to core: external callers select a typed route/sort/page rather
/// than assembling SQL or injecting projection columns.
pub(crate) struct CompiledNoteListQuery {
    pub(crate) sql: String,
    pub(crate) params: Vec<Value>,
}

pub(crate) fn compile_note_list_query(
    query: &ListQuery,
) -> Result<CompiledNoteListQuery, ListQueryError> {
    let mut predicates = Vec::<String>::new();
    let mut params = Vec::<Value>::new();

    match query.route() {
        LibraryRoute::AllNotes => predicates.push("n.deleted_time = 0".into()),
        LibraryRoute::Notebook(id) => {
            predicates.push("n.deleted_time = 0".into());
            predicates.push("n.notebook_id = ?".into());
            params.push(Value::Text(id.as_str().to_owned()));
        }
        LibraryRoute::Stack(id) => {
            predicates.push("n.deleted_time = 0".into());
            predicates.push(
                "n.notebook_id IN (SELECT nb.id FROM notebooks nb JOIN stacks s ON s.id = nb.stack_id WHERE nb.stack_id = ? AND nb.deleted_time = 0 AND s.deleted_time = 0)".into(),
            );
            params.push(Value::Text(id.as_str().to_owned()));
        }
        LibraryRoute::Tags(tag_ids) => {
            if tag_ids.is_empty() {
                return Err(ListQueryError::EmptyTagIntersection);
            }
            predicates.push("n.deleted_time = 0".into());
            let placeholders = std::iter::repeat_n("?", tag_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            predicates.push(format!(
                "n.id IN (SELECT nt.note_id FROM note_tags nt JOIN tags t ON t.id = nt.tag_id WHERE nt.tag_id IN ({placeholders}) AND t.deleted_time = 0 GROUP BY nt.note_id HAVING count(DISTINCT nt.tag_id) = ?)"
            ));
            params.extend(tag_ids.iter().map(|id| Value::Text(id.as_str().to_owned())));
            params.push(Value::Integer(
                i64::try_from(tag_ids.len()).map_err(|_| ListQueryError::SqlIntegerOverflow)?,
            ));
        }
        LibraryRoute::Trash => predicates.push("n.deleted_time <> 0".into()),
    }

    let order = match (query.sort().field(), query.sort().direction()) {
        (SortField::Title, SortDirection::Ascending) => "n.title COLLATE NOCASE ASC, n.id ASC",
        (SortField::Title, SortDirection::Descending) => "n.title COLLATE NOCASE DESC, n.id ASC",
        (SortField::Updated, SortDirection::Ascending) => "n.updated_time ASC, n.id ASC",
        (SortField::Updated, SortDirection::Descending) => "n.updated_time DESC, n.id ASC",
        (SortField::Deleted, SortDirection::Ascending) => "n.deleted_time ASC, n.id ASC",
        (SortField::Deleted, SortDirection::Descending) => "n.deleted_time DESC, n.id ASC",
    };
    let limit = query
        .limit()
        .map(|value| i64::try_from(value).map_err(|_| ListQueryError::SqlIntegerOverflow))
        .transpose()?
        .unwrap_or(-1);
    let offset = i64::try_from(query.offset()).map_err(|_| ListQueryError::SqlIntegerOverflow)?;
    params.push(Value::Integer(limit));
    params.push(Value::Integer(offset));

    Ok(CompiledNoteListQuery {
        sql: format!(
            "SELECT n.id, substr(n.title, 1, 120), substr(n.snippet, 1, 160), n.updated_time,\n                    n.deleted_time, n.notebook_id,\n                    COALESCE((SELECT n.selected_thumbnail_id WHERE EXISTS (SELECT 1 FROM note_resources snr JOIN resources sr ON sr.id = snr.resource_id WHERE snr.note_id=n.id AND snr.resource_id=n.selected_thumbnail_id AND snr.is_associated=1 AND sr.deleted_time=0 AND sr.mime LIKE 'image/%')),\n                    (SELECT nr.resource_id FROM note_resources nr JOIN resources r ON r.id = nr.resource_id\n                     WHERE nr.note_id = n.id AND nr.is_associated = 1 AND r.deleted_time = 0\n                       AND r.mime IN ('image/png', 'image/jpeg') ORDER BY nr.position, nr.resource_id LIMIT 1)),\n                    (SELECT count(*) FROM note_resources nr WHERE nr.note_id = n.id AND nr.is_associated = 1)\n             FROM notes n\n             WHERE {}\n             ORDER BY {order}\n             LIMIT ? OFFSET ?",
            predicates.join(" AND ")
        ),
        params,
    })
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
