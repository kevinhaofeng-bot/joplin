use app_lite_core::{LibraryRoute, NoteId, NotebookId, ShortcutTarget, SortSpec, StackId, TagId};

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
        ExportCurrentNote,
        ImportLibrary,
        CancelLibraryImport,
        OpenImportedLibrary,
        BackupLibrary,
        RestoreLibrary,
        ExportLibraryReadable,
        RestoreLibraryReadable,
        CopyNote,
        NavigateLibraryBack,
        NavigateLibraryForward,
        SyncNow,
        OpenSyncSettings,
        ShowSyncFailures,
        InsertNoteTable,
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
    pub(crate) const fn setting_value(self) -> &'static str {
        match self {
            Self::Cards => "cards",
            Self::Snippets => "snippets",
            Self::Compact => "compact",
        }
    }

    pub(crate) fn from_setting_value(value: &str) -> Option<Self> {
        [Self::Cards, Self::Snippets, Self::Compact]
            .into_iter()
            .find(|mode| mode.setting_value() == value)
    }

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
    AddShortcuts(Vec<ShortcutTarget>),
    RemoveShortcuts(Vec<ShortcutTarget>),
    OpenShortcut(ShortcutTarget),
    OpenRecentNote(NoteId),
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
    SetNotebookStack {
        id: NotebookId,
        stack_id: Option<StackId>,
    },
    CreateStackForNotebook {
        id: NotebookId,
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
    /// Cmd-click: add/remove a note from the multi-selection. Move and tag
    /// actions on the "selected note" apply to every selected note.
    ToggleNoteInSelection(NoteId),
    /// Copy the selected note into its notebook and select the copy.
    CopySelectedNote,
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
    /// Exactly these notes, captured when permanent deletion was confirmed.
    PurgeNotes(Vec<NoteId>),
    PurgeSelected,
    ToggleSidebar,
    ToggleNoteList,
    SetListViewMode(ListViewMode),
    SetSort(NoteSort),
    ManualSync,
}
