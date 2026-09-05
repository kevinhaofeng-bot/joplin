pub mod profile;
pub mod runtime;
pub mod sync_sidecar;

use tauri::Manager;

use runtime::{AppState, app_state_for_app_data, get_runtime_info};

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let state = app
                .path()
                .app_data_dir()
                .map(app_state_for_app_data)
                .unwrap_or_else(|_| AppState::failed());
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_runtime_info])
        .run(tauri::generate_context!())
        .expect("error while running Joplin Lite");
}
