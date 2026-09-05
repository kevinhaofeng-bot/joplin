pub mod library;
pub mod profile;
pub mod runtime;
pub mod sync_sidecar;

use tauri::Manager;

use library::library_state_for_app_data;
use runtime::{AppState, app_state_for_app_data, get_runtime_info};

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let app_data = app.path().app_data_dir();
            let runtime_state = app_data
                .as_ref()
                .map(|path| app_state_for_app_data(path.clone()))
                .unwrap_or_else(|_| AppState::failed());
            let library_state = app_data
                .map(library_state_for_app_data)
                .unwrap_or_else(|_| library::LibraryState::unavailable());
            app.manage(runtime_state);
            app.manage(library_state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_runtime_info,
            library::open_library,
            library::retry_library,
            library::profile_status,
            library::list_folders,
            library::create_folder,
            library::update_folder,
            library::trash_folder,
            library::list_tags,
            library::create_tag,
            library::update_tag,
            library::delete_tag,
            library::list_notes,
            library::get_note,
            library::create_note,
            library::update_note,
            library::trash_note,
            library::set_note_tags,
            library::shutdown_library
        ])
        .run(tauri::generate_context!())
        .expect("error while running Joplin Lite");
}
