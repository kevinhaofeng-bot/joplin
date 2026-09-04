use std::{
    io,
    path::{Component, Path, PathBuf},
};

pub const EXPECTED_PROFILE_DIRECTORY_NAME: &str = "com.kevinhao.joplin-lite";

#[derive(Debug, thiserror::Error)]
pub enum ProfilePathError {
    #[error("the Joplin Lite profile directory name is not allowed")]
    UnexpectedProfileDirectory,
    #[error("the legacy Joplin desktop profile is not allowed")]
    LegacyProfile,
    #[error("the Joplin Lite profile root cannot be a symlink")]
    SymlinkRoot,
    #[error("could not inspect the Joplin Lite profile path")]
    Io(#[from] io::Error),
}

#[derive(Clone, Debug)]
pub struct ProfilePaths {
    root: PathBuf,
    database: PathBuf,
    resources: PathBuf,
    indexes: PathBuf,
    logs: PathBuf,
}

impl ProfilePaths {
    pub fn try_from_app_data(root: PathBuf) -> Result<Self, ProfilePathError> {
        validate_root(&root)?;

        Ok(Self {
            database: root.join("database.sqlite"),
            resources: root.join("resources"),
            indexes: root.join("indexes"),
            logs: root.join("logs"),
            root,
        })
    }

    pub fn from_app_data(root: PathBuf) -> Self {
        Self::try_from_app_data(root).expect("Joplin Lite received an unsafe profile path")
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        validate_root(&self.root).map_err(ProfilePathError::into_io_error)?;

        for path in [self.root(), self.resources(), self.indexes(), self.logs()] {
            std::fs::create_dir_all(path)?;
        }
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn database(&self) -> &Path {
        &self.database
    }

    pub fn resources(&self) -> &Path {
        &self.resources
    }

    pub fn indexes(&self) -> &Path {
        &self.indexes
    }

    pub fn logs(&self) -> &Path {
        &self.logs
    }
}

impl ProfilePathError {
    fn into_io_error(self) -> io::Error {
        match self {
            Self::Io(error) => error,
            error => io::Error::new(io::ErrorKind::InvalidInput, error),
        }
    }
}

fn validate_root(root: &Path) -> Result<(), ProfilePathError> {
    if root.file_name().and_then(|name| name.to_str()) != Some(EXPECTED_PROFILE_DIRECTORY_NAME) {
        return Err(ProfilePathError::UnexpectedProfileDirectory);
    }

    reject_legacy_component(root)?;

    match std::fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(ProfilePathError::SymlinkRoot);
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    if let Some(parent) = nearest_existing_parent(root.parent())? {
        reject_legacy_component(&std::fs::canonicalize(parent)?)?;
    }

    Ok(())
}

fn nearest_existing_parent(mut path: Option<&Path>) -> Result<Option<&Path>, ProfilePathError> {
    while let Some(candidate) = path {
        match std::fs::symlink_metadata(candidate) {
            Ok(_) => return Ok(Some(candidate)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => path = candidate.parent(),
            Err(error) => return Err(error.into()),
        }
    }

    Ok(None)
}

fn reject_legacy_component(path: &Path) -> Result<(), ProfilePathError> {
    if path.components().any(|component| {
        matches!(component, Component::Normal(name) if name.eq_ignore_ascii_case("joplin-desktop"))
    }) {
        return Err(ProfilePathError::LegacyProfile);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::symlink,
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
                "joplin-lite-profile-test-{}-{unique_id}",
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
    fn derives_every_path_below_the_app_data_directory() {
        let paths = ProfilePaths::from_app_data(PathBuf::from("/tmp/com.kevinhao.joplin-lite"));
        assert_eq!(
            paths.database(),
            Path::new("/tmp/com.kevinhao.joplin-lite/database.sqlite")
        );
        assert_eq!(
            paths.resources(),
            Path::new("/tmp/com.kevinhao.joplin-lite/resources")
        );
        assert_eq!(
            paths.indexes(),
            Path::new("/tmp/com.kevinhao.joplin-lite/indexes")
        );
        assert_eq!(
            paths.logs(),
            Path::new("/tmp/com.kevinhao.joplin-lite/logs")
        );
    }

    #[test]
    fn rejects_an_arbitrary_profile_directory_name() {
        let result = ProfilePaths::try_from_app_data(PathBuf::from("/tmp/anything-else"));
        assert!(result.is_err());
    }

    #[test]
    fn rejects_a_case_variant_of_the_legacy_profile_name() {
        let result = ProfilePaths::try_from_app_data(PathBuf::from("/tmp/JOPLIN-DESKTOP"));
        assert!(result.is_err());
    }

    #[test]
    fn rejects_a_profile_nested_below_the_legacy_directory() {
        let result = ProfilePaths::try_from_app_data(PathBuf::from(
            "/tmp/joplin-desktop/com.kevinhao.joplin-lite",
        ));
        assert!(result.is_err());
    }

    #[test]
    fn rejects_an_existing_profile_root_symlink() {
        let temporary_directory = TemporaryDirectory::new();
        let target = temporary_directory.path().join("target");
        let root = temporary_directory.path().join("com.kevinhao.joplin-lite");
        fs::create_dir_all(&target).unwrap();
        symlink(&target, &root).unwrap();

        assert!(ProfilePaths::try_from_app_data(root).is_err());
    }

    #[test]
    fn rejects_a_parent_symlink_that_resolves_below_a_legacy_directory() {
        let temporary_directory = TemporaryDirectory::new();
        let legacy_parent = temporary_directory.path().join("joplin-desktop");
        let alias = temporary_directory.path().join("alias");
        fs::create_dir_all(&legacy_parent).unwrap();
        symlink(&legacy_parent, &alias).unwrap();

        let root = alias.join("com.kevinhao.joplin-lite");
        assert!(ProfilePaths::try_from_app_data(root).is_err());
    }

    #[test]
    fn ensure_rejects_a_root_replaced_by_a_symlink_after_validation() {
        let temporary_directory = TemporaryDirectory::new();
        let root = temporary_directory.path().join("com.kevinhao.joplin-lite");
        let paths = ProfilePaths::try_from_app_data(root.clone()).unwrap();
        let target = temporary_directory.path().join("target");
        fs::create_dir_all(&target).unwrap();
        symlink(&target, &root).unwrap();

        assert!(paths.ensure().is_err());
    }
}
