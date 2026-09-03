pub mod profile;
pub mod runtime;

use tauri::Manager;

use runtime::{AppState, get_runtime_info};

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let app_data = app.path().app_data_dir()?;
            let profile_paths = profile::ProfilePaths::try_from_app_data(app_data)?;
            profile_paths.ensure()?;
            app.manage(AppState { profile_paths });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_runtime_info])
        .run(tauri::generate_context!())
        .expect("error while running Joplin Lite");
}
