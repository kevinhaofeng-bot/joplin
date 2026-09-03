use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ProfilePathError {
    #[error("the legacy Joplin desktop profile is not allowed")]
    LegacyProfile,
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
        if root.file_name().and_then(|name| name.to_str()) == Some("joplin-desktop") {
            return Err(ProfilePathError::LegacyProfile);
        }

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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn rejects_the_legacy_desktop_profile_name() {
        let result = ProfilePaths::try_from_app_data(PathBuf::from("/tmp/joplin-desktop"));
        assert!(matches!(result, Err(ProfilePathError::LegacyProfile)));
    }
}
