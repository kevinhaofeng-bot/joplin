use std::{
    io,
    path::{Component, Path, PathBuf},
};

pub const EXPECTED_PROFILE_DIRECTORY_NAME: &str = "com.kevinhao.joplin-lite";
pub const MAX_RESOURCE_BYTES: u64 = 100 * 1024 * 1024;
pub const MAX_JEX_BYTES: u64 = 8 * 1024 * 1024 * 1024;

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

pub fn validate_resource_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn validate_resource_file(path: &Path) -> io::Result<std::fs::Metadata> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_RESOURCE_BYTES
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resource file rejected",
        ));
    }
    Ok(metadata)
}

pub fn validate_jex_file(path: &Path) -> io::Result<std::fs::Metadata> {
    if !path.is_absolute()
        || path.to_string_lossy().contains('\0')
        || path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.eq_ignore_ascii_case("jex"))
            != Some(true)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "jex file rejected",
        ));
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_JEX_BYTES
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "jex file rejected",
        ));
    }
    Ok(metadata)
}

pub fn resource_path(
    paths: &ProfilePaths,
    id: &str,
    file_extension: Option<&str>,
) -> io::Result<PathBuf> {
    if !validate_resource_id(id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resource id rejected",
        ));
    }
    let extension = file_extension.unwrap_or("");
    if !extension.is_empty()
        && (extension.len() > 10 || !extension.bytes().all(|byte| byte.is_ascii_alphanumeric()))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resource extension rejected",
        ));
    }
    let root = std::fs::canonicalize(paths.root())?;
    let resource_metadata = std::fs::symlink_metadata(paths.resources())?;
    if resource_metadata.file_type().is_symlink() || !resource_metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resource directory rejected",
        ));
    }
    let resources = std::fs::canonicalize(paths.resources())?;
    if resources.parent() != Some(root.as_path()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resource directory rejected",
        ));
    }
    let suffix = if extension.is_empty() {
        String::new()
    } else {
        format!(".{extension}")
    };
    let candidate = paths.resources().join(format!("{id}{suffix}"));
    validate_resource_file(&candidate)?;
    let canonical = std::fs::canonicalize(candidate)?;
    if canonical.parent() != Some(resources.as_path()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resource path rejected",
        ));
    }
    Ok(canonical)
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
            if let Ok(metadata) = std::fs::symlink_metadata(path)
                && (metadata.file_type().is_symlink() || !metadata.is_dir())
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "managed profile path rejected",
                ));
            }
            std::fs::create_dir_all(path)?;
            let metadata = std::fs::symlink_metadata(path)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "managed profile path rejected",
                ));
            }
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

    #[test]
    fn resource_input_requires_a_regular_non_symlink_file_within_the_size_limit() {
        let temporary_directory = TemporaryDirectory::new();
        let file = temporary_directory.path().join("picture.png");
        fs::write(&file, b"image").unwrap();
        assert!(validate_resource_file(&file).is_ok());

        let link = temporary_directory.path().join("link.png");
        symlink(&file, &link).unwrap();
        assert!(validate_resource_file(&link).is_err());

        let directory = temporary_directory.path().join("directory");
        fs::create_dir(&directory).unwrap();
        assert!(validate_resource_file(&directory).is_err());

        let oversized = temporary_directory.path().join("oversized.bin");
        fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&oversized)
            .unwrap()
            .set_len(MAX_RESOURCE_BYTES + 1)
            .unwrap();
        assert!(validate_resource_file(&oversized).is_err());
    }

    #[test]
    fn jex_input_requires_absolute_regular_non_symlink_file_within_limit() {
        let temporary_directory = TemporaryDirectory::new();
        let file = temporary_directory.path().join("export.jex");
        fs::write(&file, b"fixture").unwrap();
        assert!(validate_jex_file(&file).is_ok());
        assert!(validate_jex_file(Path::new("relative.jex")).is_err());

        let link = temporary_directory.path().join("link.jex");
        symlink(&file, &link).unwrap();
        assert!(validate_jex_file(&link).is_err());

        let oversized = temporary_directory.path().join("oversized.jex");
        fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&oversized)
            .unwrap()
            .set_len(MAX_JEX_BYTES + 1)
            .unwrap();
        assert!(validate_jex_file(&oversized).is_err());
    }

    #[test]
    fn resource_ids_are_exact_lowercase_hex() {
        assert!(validate_resource_id(&"a".repeat(32)));
        assert!(!validate_resource_id(&"A".repeat(32)));
        assert!(!validate_resource_id(&"a".repeat(31)));
        assert!(!validate_resource_id(&format!("{}g", "a".repeat(31))));
    }

    #[test]
    fn resource_path_rejects_traversal_and_symlink_files() {
        let temporary_directory = TemporaryDirectory::new();
        let root = temporary_directory
            .path()
            .join(EXPECTED_PROFILE_DIRECTORY_NAME);
        let paths = ProfilePaths::try_from_app_data(root).unwrap();
        paths.ensure().unwrap();
        let id = "a".repeat(32);
        let resource = paths.resources().join(format!("{id}.png"));
        fs::write(&resource, b"png").unwrap();
        assert_eq!(
            resource_path(&paths, &id, Some("png")).unwrap(),
            fs::canonicalize(&resource).unwrap()
        );
        assert!(resource_path(&paths, &id, Some("../secret")).is_err());
        let link = paths.resources().join(format!("{id}.jpg"));
        symlink(&resource, &link).unwrap();
        assert!(resource_path(&paths, &id, Some("jpg")).is_err());
    }

    #[test]
    fn managed_resource_directory_symlink_is_rejected() {
        let temporary_directory = TemporaryDirectory::new();
        let root = temporary_directory
            .path()
            .join(EXPECTED_PROFILE_DIRECTORY_NAME);
        fs::create_dir_all(&root).unwrap();
        let outside = temporary_directory.path().join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, root.join("resources")).unwrap();
        let paths = ProfilePaths::try_from_app_data(root).unwrap();
        assert!(paths.ensure().is_err());
    }
}
