pub mod library;
pub mod profile;
pub mod runtime;
pub mod sync_sidecar;

use tauri::{Manager, http::Response};

use library::library_state_for_app_data;
use profile::{ProfilePaths, resource_path};
use runtime::{AppState, app_state_for_app_data, get_runtime_info};

fn resource_content_type(extension: &str) -> &'static str {
    match extension.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

fn resource_response(profile: Option<&ProfilePaths>, path: &str) -> Response<Vec<u8>> {
    let bad_request = || {
        Response::builder()
            .status(400)
            .header("Content-Type", "text/plain; charset=utf-8")
            .header("X-Content-Type-Options", "nosniff")
            .body(b"bad resource request".to_vec())
            .unwrap()
    };
    let not_found = || {
        Response::builder()
            .status(404)
            .header("Content-Type", "text/plain; charset=utf-8")
            .header("X-Content-Type-Options", "nosniff")
            .body(b"resource not found".to_vec())
            .unwrap()
    };
    let Some(profile) = profile else {
        return not_found();
    };
    let trimmed = path.strip_prefix('/').unwrap_or(path);
    if trimmed.is_empty() || trimmed.contains('/') || trimmed.contains('\\') {
        return bad_request();
    }
    let mut parts = trimmed.splitn(2, '.');
    let id = parts.next().unwrap_or_default();
    let extension = parts.next().unwrap_or_default();
    if extension.contains('.') {
        return bad_request();
    }
    let file = match resource_path(profile, id, (!extension.is_empty()).then_some(extension)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => return bad_request(),
        Err(_) => return not_found(),
    };
    let body = match std::fs::read(&file) {
        Ok(body) => body,
        Err(_) => return not_found(),
    };
    Response::builder()
        .status(200)
        .header("Content-Type", resource_content_type(extension))
        .header("X-Content-Type-Options", "nosniff")
        .body(body)
        .unwrap()
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .register_uri_scheme_protocol("joplin-resource", move |context, request| {
            let profile = context
                .app_handle()
                .path()
                .app_data_dir()
                .ok()
                .and_then(|root| ProfilePaths::try_from_app_data(root).ok());
            resource_response(profile.as_ref(), request.uri().path())
        })
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
            library::create_resource_from_path,
            library::list_note_resources,
            library::create_image_resource,
            library::open_resource,
            library::get_sync_config,
            library::configure_joplin_server,
            library::sync_now,
            library::shutdown_library
        ])
        .run(tauri::generate_context!())
        .expect("error while running Joplin Lite");
}
