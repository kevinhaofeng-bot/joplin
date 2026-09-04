use crate::profile::ProfilePaths;
use std::path::PathBuf;

pub const SAFE_RUNTIME_INITIALIZATION_ERROR: &str = "Joplin Lite 无法准备独立资料库";

pub enum AppState {
    Ready(ProfilePaths),
    Failed,
}

impl AppState {
    pub fn failed() -> Self {
        Self::Failed
    }
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

pub fn app_state_for_app_data(app_data: PathBuf) -> AppState {
    let Ok(profile_paths) = ProfilePaths::try_from_app_data(app_data) else {
        return AppState::Failed;
    };

    if profile_paths.ensure().is_err() {
        return AppState::Failed;
    }

    AppState::Ready(profile_paths)
}

pub fn runtime_info_for_state(state: &AppState) -> Result<RuntimeInfo, String> {
    match state {
        AppState::Ready(profile_paths) => Ok(runtime_info_for(profile_paths)),
        AppState::Failed => Err(SAFE_RUNTIME_INITIALIZATION_ERROR.into()),
    }
}

#[tauri::command]
pub fn get_runtime_info(state: tauri::State<'_, AppState>) -> Result<RuntimeInfo, String> {
    runtime_info_for_state(&state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicUsize, Ordering},
    };

    static TEMPORARY_DIRECTORY_COUNTER: AtomicUsize = AtomicUsize::new(0);

    struct TemporaryDirectory {
        path: PathBuf,
    }

    impl TemporaryDirectory {
        fn new() -> Self {
            let unique_id = TEMPORARY_DIRECTORY_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "joplin-lite-runtime-test-{}-{unique_id}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.path).unwrap();
        }
    }

    #[test]
    fn reports_the_same_safe_error_when_a_correct_profile_cannot_be_created() {
        let temporary_directory = TemporaryDirectory::new();
        let parent_file = temporary_directory.path().join("parent-file");
        fs::write(&parent_file, "not a directory").unwrap();
        let supplied_path = parent_file.join("com.kevinhao.joplin-lite");

        let state = app_state_for_app_data(supplied_path.clone());

        assert_eq!(
            runtime_info_for_state(&state).unwrap_err(),
            SAFE_RUNTIME_INITIALIZATION_ERROR
        );
        assert!(!SAFE_RUNTIME_INITIALIZATION_ERROR.contains(&supplied_path.display().to_string()));
    }

    #[test]
    fn reports_the_same_safe_error_for_invalid_app_data() {
        let supplied_path = PathBuf::from("/tmp/not-joplin-lite");

        let state = app_state_for_app_data(supplied_path.clone());

        assert_eq!(
            runtime_info_for_state(&state).unwrap_err(),
            SAFE_RUNTIME_INITIALIZATION_ERROR
        );
        assert!(!SAFE_RUNTIME_INITIALIZATION_ERROR.contains(&supplied_path.display().to_string()));
    }

    #[test]
    fn reports_runtime_info_for_a_ready_state() {
        let temporary_directory = TemporaryDirectory::new();
        let root = temporary_directory.path().join("com.kevinhao.joplin-lite");

        let state = app_state_for_app_data(root.clone());

        assert_eq!(
            runtime_info_for_state(&state).unwrap().profile_directory,
            root.display().to_string()
        );
    }

    #[test]
    fn reports_the_isolated_profile_without_touching_it() {
        let paths = ProfilePaths::from_app_data(PathBuf::from("/tmp/com.kevinhao.joplin-lite"));
        let info = runtime_info_for(&paths);
        assert_eq!(info.app_name, "Joplin Lite");
        assert_eq!(info.profile_directory, "/tmp/com.kevinhao.joplin-lite");
    }

    #[test]
    fn serializes_runtime_info_with_camel_case_json_keys() {
        let paths = ProfilePaths::from_app_data(PathBuf::from("/tmp/com.kevinhao.joplin-lite"));

        let json = serde_json::to_value(runtime_info_for(&paths)).unwrap();

        assert_eq!(
            json.get("appName"),
            Some(&Value::String("Joplin Lite".into()))
        );
        assert_eq!(
            json.get("profileDirectory"),
            Some(&Value::String("/tmp/com.kevinhao.joplin-lite".into()))
        );
        assert!(json.get("app_name").is_none());
        assert!(json.get("profile_directory").is_none());
    }
}
