//! Which library profile is active, and where imported libraries live.
//!
//! Imports never replace the base profile: each becomes a new directory
//! under `<base parent>/imported-libraries/`, and switching to one records
//! it in `<base parent>/active-library`. An explicit `JOPLIN_LITE_PROFILE`
//! still wins at startup.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const POINTER_FILE: &str = "active-library";
const IMPORTS_DIR: &str = "imported-libraries";

/// The base profile plus the one currently shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LibraryProfiles {
    pub(crate) base: PathBuf,
    pub(crate) active: PathBuf,
}

impl gpui::Global for LibraryProfiles {}

impl LibraryProfiles {
    pub(crate) fn imports_dir(&self) -> PathBuf {
        sibling(&self.base, IMPORTS_DIR)
    }
}

fn sibling(base: &Path, name: &str) -> PathBuf {
    base.parent().unwrap_or(base).join(name)
}

/// The recorded imported library if it is still a valid import of `base`,
/// otherwise `base` itself.
pub(crate) fn resolve_active(base: &Path) -> PathBuf {
    let Ok(recorded) = fs::read_to_string(sibling(base, POINTER_FILE)) else {
        return base.to_path_buf();
    };
    let recorded = PathBuf::from(recorded.trim_end_matches('\n'));
    let valid = recorded.is_absolute()
        && recorded.parent() == Some(sibling(base, IMPORTS_DIR).as_path())
        && recorded.join("library.sqlite").is_file();
    if valid { recorded } else { base.to_path_buf() }
}

/// Atomically record `active` as the library to open next time; recording
/// the base profile removes the pointer.
pub(crate) fn record_active(base: &Path, active: &Path) -> io::Result<()> {
    let pointer = sibling(base, POINTER_FILE);
    if active == base {
        return match fs::remove_file(&pointer) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
    }
    let temporary = sibling(base, ".active-library.tmp");
    let mut file = fs::File::create(&temporary)?;
    file.write_all(active.as_os_str().as_encoded_bytes())?;
    file.sync_all()?;
    fs::rename(&temporary, &pointer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(path: &Path) {
        fs::create_dir_all(path).unwrap();
        fs::write(path.join("library.sqlite"), b"").unwrap();
    }

    #[test]
    fn falls_back_to_base_without_a_valid_pointer() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("library");
        assert_eq!(resolve_active(&base), base);

        let outside = root.path().join("elsewhere");
        library(&outside);
        fs::write(
            root.path().join(POINTER_FILE),
            outside.as_os_str().as_encoded_bytes(),
        )
        .unwrap();
        assert_eq!(resolve_active(&base), base, "only imports of this base");

        let missing = root.path().join(IMPORTS_DIR).join("gone");
        fs::write(
            root.path().join(POINTER_FILE),
            missing.as_os_str().as_encoded_bytes(),
        )
        .unwrap();
        assert_eq!(resolve_active(&base), base, "deleted import falls back");
    }

    #[test]
    fn records_and_resolves_an_import_then_returns_to_base() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("library");
        let profiles = LibraryProfiles {
            base: base.clone(),
            active: base.clone(),
        };
        let imported = profiles.imports_dir().join("我的笔记-1");
        library(&imported);

        record_active(&base, &imported).unwrap();
        assert_eq!(resolve_active(&base), imported);

        record_active(&base, &base).unwrap();
        assert_eq!(resolve_active(&base), base);
        assert!(!root.path().join(POINTER_FILE).exists());
    }
}
