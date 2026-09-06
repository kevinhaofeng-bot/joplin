use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ResourceError {
    #[error("resource storage I/O error")]
    Io(#[from] std::io::Error),
    #[error("invalid resource data")]
    InvalidData,
    #[error("resource path is unsafe")]
    UnsafePath,
    #[error("resource path is a symbolic link")]
    Symlink,
    #[error("resource blob is corrupt")]
    CorruptBlob,
}

#[derive(Clone, Copy)]
pub struct ResourceImport<'a> {
    pub bytes: &'a [u8],
    pub title: &'a str,
    pub mime: &'a str,
    pub file_extension: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceBlob {
    pub id: String,
    pub sha256: String,
    pub size: usize,
    pub path: PathBuf,
}

pub struct ResourceStore {
    profile_root: PathBuf,
    resource_root: PathBuf,
}

impl ResourceStore {
    pub fn new(profile_root: PathBuf) -> Result<Self, ResourceError> {
        ensure_directory(&profile_root)?;
        let profile_root = fs::canonicalize(profile_root)?;
        let resource_root = profile_root.join("resources");
        match fs::symlink_metadata(&resource_root) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(ResourceError::Symlink);
                }
                if !metadata.is_dir() {
                    return Err(ResourceError::UnsafePath);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&resource_root)?;
            }
            Err(error) => return Err(error.into()),
        }
        ensure_directory(&resource_root)?;
        Ok(Self {
            profile_root,
            resource_root,
        })
    }

    pub fn put(&self, input: ResourceImport<'_>) -> Result<ResourceBlob, ResourceError> {
        validate_import(&input)?;
        ensure_directory(&self.profile_root)?;
        ensure_directory(&self.resource_root)?;

        let digest = Sha256::digest(input.bytes);
        let sha256 = hex_digest(&digest);
        let path = self.resource_root.join(&sha256);
        persist_blob(&path, input.bytes, &sha256)?;
        Ok(ResourceBlob {
            id: crate::core::new_id(),
            sha256,
            size: input.bytes.len(),
            path,
        })
    }

    pub(crate) fn read_blob(&self, sha256: &str) -> Result<Vec<u8>, ResourceError> {
        if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ResourceError::InvalidData);
        }
        let path = self.resource_root.join(sha256);
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(ResourceError::Symlink);
        }
        if !metadata.is_file() {
            return Err(ResourceError::UnsafePath);
        }
        let mut bytes = Vec::new();
        File::open(path)?.read_to_end(&mut bytes)?;
        if hex_digest(&Sha256::digest(&bytes)) != sha256 {
            return Err(ResourceError::CorruptBlob);
        }
        Ok(bytes)
    }

    pub(crate) fn path_for(&self, sha256: &str) -> PathBuf {
        self.resource_root.join(sha256)
    }
}

fn validate_import(input: &ResourceImport<'_>) -> Result<(), ResourceError> {
    if input.bytes.is_empty() || input.bytes.len() > MAX_IMAGE_BYTES {
        return Err(ResourceError::InvalidData);
    }
    if !matches!(input.mime, "image/png" | "image/jpeg") {
        return Err(ResourceError::InvalidData);
    }
    if !matches!(input.file_extension, "png" | "jpg" | "jpeg")
        || input.file_extension.contains('/')
        || input.file_extension.contains('\\')
        || input.file_extension == "."
        || input.file_extension == ".."
    {
        return Err(ResourceError::UnsafePath);
    }
    Ok(())
}

fn persist_blob(path: &Path, bytes: &[u8], sha256: &str) -> Result<(), ResourceError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(ResourceError::Symlink);
            }
            if !metadata.is_file() {
                return Err(ResourceError::UnsafePath);
            }
            let mut existing = Vec::new();
            File::open(path)?.read_to_end(&mut existing)?;
            if hex_digest(&Sha256::digest(&existing)) != sha256 {
                return Err(ResourceError::CorruptBlob);
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let temp_name = format!(".{sha256}.{}.tmp", crate::core::new_id());
            let temp_path = path
                .parent()
                .ok_or(ResourceError::UnsafePath)?
                .join(temp_name);
            let mut temp = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)?;
            if let Err(error) = temp.write_all(bytes).and_then(|_| temp.sync_all()) {
                let _ = fs::remove_file(&temp_path);
                return Err(error.into());
            }
            match fs::rename(&temp_path, path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let _ = fs::remove_file(&temp_path);
                    persist_blob(path, bytes, sha256)
                }
                Err(error) => {
                    let _ = fs::remove_file(&temp_path);
                    Err(error.into())
                }
            }
        }
        Err(error) => Err(error.into()),
    }
}

fn ensure_directory(path: &Path) -> Result<(), ResourceError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ResourceError::UnsafePath
        } else {
            ResourceError::Io(error)
        }
    })?;
    if metadata.file_type().is_symlink() {
        return Err(ResourceError::Symlink);
    }
    if !metadata.is_dir() {
        return Err(ResourceError::UnsafePath);
    }
    Ok(())
}

fn hex_digest(digest: &[u8]) -> String {
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{MAX_IMAGE_BYTES, ResourceError, ResourceImport, ResourceStore};
    use tempfile::tempdir;

    #[test]
    fn put_rejects_empty_and_over_limit_data() {
        let store = ResourceStore::new(tempdir().unwrap().path().to_path_buf()).unwrap();
        assert!(matches!(
            store.put(ResourceImport {
                bytes: &[],
                title: "empty",
                mime: "image/png",
                file_extension: "png",
            }),
            Err(ResourceError::InvalidData)
        ));
        let oversized = vec![0u8; MAX_IMAGE_BYTES + 1];
        assert!(matches!(
            store.put(ResourceImport {
                bytes: &oversized,
                title: "large",
                mime: "image/png",
                file_extension: "png",
            }),
            Err(ResourceError::InvalidData)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_resource_root_and_blob_are_rejected() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let target = tempdir().unwrap();
        symlink(target.path(), dir.path().join("resources")).unwrap();
        assert!(matches!(
            ResourceStore::new(dir.path().to_path_buf()),
            Err(ResourceError::Symlink)
        ));

        let dir = tempdir().unwrap();
        let store = ResourceStore::new(dir.path().to_path_buf()).unwrap();
        let blob_target_dir = tempdir().unwrap();
        let blob_target = blob_target_dir.path().join("outside");
        std::fs::write(&blob_target, b"outside").unwrap();
        let digest = "a".repeat(64);
        symlink(&blob_target, dir.path().join("resources").join(&digest)).unwrap();
        let bytes = b"resource";
        let result = store.put(ResourceImport {
            bytes,
            title: "blob",
            mime: "image/png",
            file_extension: "png",
        });
        assert!(result.is_ok());
        assert!(matches!(
            store.read_blob(&digest),
            Err(ResourceError::Symlink) | Err(ResourceError::CorruptBlob)
        ));
    }
}
