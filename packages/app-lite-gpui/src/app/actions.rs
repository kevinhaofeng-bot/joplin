use app_lite_core::NoteId;

gpui::actions!(notes_library, [CreateNote, ToggleSidebar, ToggleNoteList]);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppAction {
    CreateNote,
    SelectNote(NoteId),
    TrashNote(NoteId),
    ToggleSidebar,
    ToggleNoteList,
}
