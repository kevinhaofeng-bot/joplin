use app_lite_core::NoteId;

gpui::actions!(
    notes_library,
    [
        CreateNote,
        TrashSelected,
        ToggleSidebar,
        ToggleNoteList,
        CycleListViewMode,
        CycleSort,
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
    TitleAscending,
    TitleDescending,
}

impl NoteSort {
    pub const fn next(self) -> Self {
        match self {
            Self::UpdatedDescending => Self::TitleAscending,
            Self::TitleAscending => Self::TitleDescending,
            Self::TitleDescending => Self::UpdatedDescending,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppAction {
    CreateNote,
    SelectNote(NoteId),
    TrashNote(NoteId),
    TrashSelected,
    ToggleSidebar,
    ToggleNoteList,
    SetListViewMode(ListViewMode),
    SetSort(NoteSort),
}
