use app_lite_core::{LibraryRoute, NoteId, NotebookId, SortSpec, StackId, TagId};

gpui::actions!(
    notes_library,
    [
        CreateNote,
        TrashSelected,
        ToggleSidebar,
        ToggleNoteList,
        CycleListViewMode,
        CycleSort,
        SyncCurrent,
    ]
);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ListViewMode {
    #[default]
    Cards,
    Snippets,
    Compact,
}

impl ListViewMode {
    pub const fn next(self) -> Self {
        match self {
            Self::Cards => Self::Snippets,
            Self::Snippets => Self::Compact,
            Self::Compact => Self::Cards,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NoteSort {
    #[default]
    UpdatedDescending,
    DeletedDescending,
    TitleAscending,
    TitleDescending,
}

impl NoteSort {
    pub const fn next(self) -> Self {
        match self {
            Self::UpdatedDescending => Self::TitleAscending,
            Self::TitleAscending => Self::TitleDescending,
            Self::TitleDescending => Self::UpdatedDescending,
            Self::DeletedDescending => Self::TitleAscending,
        }
    }

    pub(crate) const fn sort_spec(self) -> SortSpec {
        match self {
            Self::UpdatedDescending => SortSpec::UPDATED_DESCENDING,
            Self::DeletedDescending => SortSpec::DELETED_DESCENDING,
            Self::TitleAscending => SortSpec::title_ascending(),
            Self::TitleDescending => SortSpec::title_descending(),
        }
    }

    pub(crate) fn from_sort_spec(sort: SortSpec) -> Self {
        match sort {
            SortSpec::DELETED_DESCENDING => Self::DeletedDescending,
            SortSpec::UPDATED_DESCENDING => Self::UpdatedDescending,
            sort if sort == SortSpec::title_ascending() => Self::TitleAscending,
            _ => Self::TitleDescending,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppAction {
    CreateNote,
    CreateStack {
        title: String,
    },
    CreateNotebook {
        title: String,
        stack_id: Option<StackId>,
    },
    CreateTag {
        title: String,
    },
    RenameStack {
        id: StackId,
        title: String,
    },
    RenameNotebook {
        id: NotebookId,
        title: String,
    },
    RenameTag {
        id: TagId,
        title: String,
    },
    DeleteStack(StackId),
    DeleteNotebook(NotebookId),
    DeleteTag(TagId),
    MoveSelectedNote(NotebookId),
    SetSelectedNoteTags(Vec<TagId>),
    AddTagToSelectedNote(TagId),
    RemoveTagFromSelectedNote(TagId),
    SelectNote(NoteId),
    NavigateTo {
        route: LibraryRoute,
        selected_note_id: Option<NoteId>,
    },
    NavigateBack,
    NavigateForward,
    TrashNote(NoteId),
    TrashSelected,
    RestoreNote(NoteId),
    RestoreSelected,
    PurgeNote(NoteId),
    PurgeSelected,
    ToggleSidebar,
    ToggleNoteList,
    SetListViewMode(ListViewMode),
    SetSort(NoteSort),
    ManualSync,
}
