//! Resolve the one-shot picker without sharing the parent app's bundle identity.
use std::path::{Path, PathBuf};

pub fn picker_executable(current: &Path) -> Result<PathBuf, String> {
    let Some(macos) = current
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "MacOS"))
    else {
        return Ok(current.to_path_buf());
    };
    let Some(contents) = macos
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "Contents"))
    else {
        return Ok(current.to_path_buf());
    };
    if !contents
        .parent()
        .is_some_and(|path| path.extension().is_some_and(|extension| extension == "app"))
    {
        return Ok(current.to_path_buf());
    }
    let helper = contents.join("Helpers/Joplin Lite Picker.app/Contents/MacOS/joplin-lite-picker");
    let metadata = std::fs::metadata(&helper)
        .map_err(|error| format!("picker helper unavailable: {error}"))?;
    if !metadata.is_file() {
        return Err("picker helper is not a file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err("picker helper is not executable".into());
        }
    }
    Ok(helper)
}

pub fn is_helper_executable(current: &Path) -> bool {
    current
        .file_name()
        .is_some_and(|name| name == "joplin-lite-picker")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "joplin-picker-path-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn app(&self) -> PathBuf {
            self.0
                .join("中文 有空格/Joplin Lite.app/Contents/MacOS/joplin-lite")
        }
        fn helper(&self) -> PathBuf {
            self.0.join("中文 有空格/Joplin Lite.app/Contents/Helpers/Joplin Lite Picker.app/Contents/MacOS/joplin-lite-picker")
        }
        fn write(&self, path: &Path, executable: bool) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"fixture").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    path,
                    std::fs::Permissions::from_mode(if executable { 0o700 } else { 0o600 }),
                )
                .unwrap();
            }
            #[cfg(not(unix))]
            let _ = executable;
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn packaged_picker_uses_separate_helper_not_parent_bundle() {
        let fixture = Fixture::new();
        fixture.write(&fixture.app(), true);
        fixture.write(&fixture.helper(), true);
        assert_eq!(picker_executable(&fixture.app()).unwrap(), fixture.helper());
    }

    #[test]
    fn packaged_missing_helper_fails_instead_of_reusing_parent_identity() {
        let fixture = Fixture::new();
        fixture.write(&fixture.app(), true);
        assert!(picker_executable(&fixture.app()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn packaged_nonexecutable_helper_is_rejected_before_spawn() {
        let fixture = Fixture::new();
        fixture.write(&fixture.helper(), false);
        assert!(picker_executable(&fixture.app()).is_err());
    }

    #[test]
    fn packaged_directory_at_helper_path_is_rejected() {
        let fixture = Fixture::new();
        std::fs::create_dir_all(fixture.helper()).unwrap();
        assert!(picker_executable(&fixture.app()).is_err());
    }

    #[test]
    fn development_binary_keeps_existing_cli_worker() {
        assert_eq!(
            picker_executable(Path::new("/target/release/velotype")).unwrap(),
            Path::new("/target/release/velotype")
        );
        assert_eq!(
            picker_executable(Path::new("/tmp/not.app/velotype")).unwrap(),
            Path::new("/tmp/not.app/velotype")
        );
    }

    #[test]
    fn helper_identity_is_detected_for_pre_profile_startup_guard() {
        assert!(is_helper_executable(Path::new(
            "/中文/Joplin Lite Picker.app/Contents/MacOS/joplin-lite-picker"
        )));
        assert!(!is_helper_executable(Path::new(
            "/中文/Joplin Lite.app/Contents/MacOS/joplin-lite"
        )));
    }
}
