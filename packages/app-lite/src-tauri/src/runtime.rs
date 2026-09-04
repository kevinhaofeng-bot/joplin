use crate::profile::ProfilePaths;

pub struct AppState {
    pub profile_paths: ProfilePaths,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInfo {
    pub app_name: String,
    pub profile_directory: String,
}

pub fn runtime_info_for(paths: &ProfilePaths) -> RuntimeInfo {
    RuntimeInfo {
        app_name: "Joplin Lite".into(),
        profile_directory: paths.root().display().to_string(),
    }
}

#[tauri::command]
pub fn get_runtime_info(state: tauri::State<'_, AppState>) -> RuntimeInfo {
    runtime_info_for(&state.profile_paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn reports_the_isolated_profile_without_touching_it() {
        let paths = ProfilePaths::from_app_data(PathBuf::from("/tmp/com.kevinhao.joplin-lite"));
        let info = runtime_info_for(&paths);
        assert_eq!(info.app_name, "Joplin Lite");
        assert_eq!(info.profile_directory, "/tmp/com.kevinhao.joplin-lite");
    }
}
