use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
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
    blobs_root: PathBuf,
    blobs_dir: DirFd,
}

struct DirFd(RawFd);

impl Drop for DirFd {
    fn drop(&mut self) {
        // This descriptor is owned exclusively by the store.
        unsafe {
            libc::close(self.0);
        }
    }
}

impl ResourceStore {
    pub fn new(profile_root: PathBuf) -> Result<Self, ResourceError> {
        ensure_directory(&profile_root)?;
        let profile_root = fs::canonicalize(profile_root)?;
        let profile_dir = open_dir_path(&profile_root)?;
        let resources_dir = open_or_create_dir(profile_dir.0, "resources")?;
        let blobs_dir = open_or_create_dir(resources_dir.0, "blobs")?;
        let blobs_root = profile_root.join("resources").join("blobs");
        Ok(Self {
            blobs_root,
            blobs_dir,
        })
    }

    pub fn put(&self, input: ResourceImport<'_>) -> Result<ResourceBlob, ResourceError> {
        validate_import(&input)?;

        let digest = Sha256::digest(input.bytes);
        let sha256 = hex_digest(&digest);
        persist_blob(self.blobs_dir.0, input.bytes, &sha256)?;
        Ok(ResourceBlob {
            id: crate::core::new_id(),
            sha256,
            size: input.bytes.len(),
            path: self.path_for(&hex_digest(&digest)),
        })
    }

    pub(crate) fn read_blob(&self, sha256: &str) -> Result<Vec<u8>, ResourceError> {
        if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ResourceError::InvalidData);
        }
        let Some(mut file) = open_blob(self.blobs_dir.0, sha256)? else {
            return Err(ResourceError::Io(std::io::Error::from(
                std::io::ErrorKind::NotFound,
            )));
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        if hex_digest(&Sha256::digest(&bytes)) != sha256 {
            return Err(ResourceError::CorruptBlob);
        }
        Ok(bytes)
    }

    pub(crate) fn path_for(&self, sha256: &str) -> PathBuf {
        self.blobs_root.join(sha256)
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

fn persist_blob(dir_fd: RawFd, bytes: &[u8], sha256: &str) -> Result<(), ResourceError> {
    if let Some(mut existing) = open_blob(dir_fd, sha256)? {
        let mut current = Vec::new();
        existing.read_to_end(&mut current)?;
        if hex_digest(&Sha256::digest(&current)) != sha256 {
            return Err(ResourceError::CorruptBlob);
        }
        existing.sync_all()?;
        fsync_fd(dir_fd)?;
        return Ok(());
    }

    let temp_name = format!(".{sha256}.{}.tmp", crate::core::new_id());
    let temp_fd = unsafe {
        let name =
            std::ffi::CString::new(temp_name.as_bytes()).map_err(|_| ResourceError::UnsafePath)?;
        libc::openat(
            dir_fd,
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if temp_fd < 0 {
        return Err(io_error());
    }
    let mut temp = unsafe { File::from_raw_fd(temp_fd) };
    let result = temp.write_all(bytes).and_then(|_| temp.sync_all());
    if let Err(error) = result {
        drop(temp);
        let _ = unlink_at(dir_fd, &temp_name);
        return Err(error.into());
    }
    drop(temp);
    let rename_result = rename_at(dir_fd, &temp_name, dir_fd, sha256);
    if let Err(error) = rename_result {
        let _ = unlink_at(dir_fd, &temp_name);
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            return persist_blob(dir_fd, bytes, sha256);
        }
        return Err(error.into());
    }
    fsync_fd(dir_fd)?;
    Ok(())
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

fn open_dir_path(path: &Path) -> Result<DirFd, ResourceError> {
    let bytes = path.as_os_str().as_bytes();
    let c_path = std::ffi::CString::new(bytes).map_err(|_| ResourceError::UnsafePath)?;
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io_error());
    }
    Ok(DirFd(fd))
}

fn open_or_create_dir(parent_fd: RawFd, name: &str) -> Result<DirFd, ResourceError> {
    let c_name = std::ffi::CString::new(name).map_err(|_| ResourceError::UnsafePath)?;
    let fd = unsafe {
        libc::openat(
            parent_fd,
            c_name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd >= 0 {
        fsync_fd(parent_fd)?;
        return Ok(DirFd(fd));
    }
    let first_error = std::io::Error::last_os_error();
    if first_error.kind() != std::io::ErrorKind::NotFound {
        return Err(if child_is_symlink(parent_fd, &c_name) {
            ResourceError::Symlink
        } else {
            ResourceError::Io(first_error)
        });
    }
    let mkdir_result = unsafe { libc::mkdirat(parent_fd, c_name.as_ptr(), 0o700) };
    if mkdir_result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error.into());
        }
    } else {
        fsync_fd(parent_fd)?;
    }
    let fd = unsafe {
        libc::openat(
            parent_fd,
            c_name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(if child_is_symlink(parent_fd, &c_name) {
            ResourceError::Symlink
        } else {
            io_error()
        });
    }
    Ok(DirFd(fd))
}

fn open_blob(dir_fd: RawFd, name: &str) -> Result<Option<File>, ResourceError> {
    let c_name = std::ffi::CString::new(name).map_err(|_| ResourceError::UnsafePath)?;
    let fd = unsafe {
        libc::openat(
            dir_fd,
            c_name.as_ptr(),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        if child_is_symlink(dir_fd, &c_name) {
            return Err(ResourceError::Symlink);
        }
        return Err(error.into());
    }
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } < 0 {
        let error = io_error();
        unsafe { libc::close(fd) };
        return Err(error);
    }
    let stat = unsafe { stat.assume_init() };
    if (stat.st_mode & libc::S_IFMT) != libc::S_IFREG {
        unsafe { libc::close(fd) };
        return Err(ResourceError::UnsafePath);
    }
    Ok(Some(unsafe { File::from_raw_fd(fd) }))
}

fn child_is_symlink(parent_fd: RawFd, name: &std::ffi::CStr) -> bool {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            parent_fd,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } < 0
    {
        return false;
    }
    let stat = unsafe { stat.assume_init() };
    (stat.st_mode & libc::S_IFMT) == libc::S_IFLNK
}

fn rename_at(
    old_dir: RawFd,
    old_name: &str,
    new_dir: RawFd,
    new_name: &str,
) -> std::io::Result<()> {
    let old = std::ffi::CString::new(old_name)
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let new = std::ffi::CString::new(new_name)
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    if unsafe { libc::renameat(old_dir, old.as_ptr(), new_dir, new.as_ptr()) } < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn unlink_at(dir_fd: RawFd, name: &str) -> std::io::Result<()> {
    let name = std::ffi::CString::new(name)
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    if unsafe { libc::unlinkat(dir_fd, name.as_ptr(), 0) } < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn fsync_fd(fd: RawFd) -> Result<(), ResourceError> {
    if unsafe { libc::fsync(fd) } < 0 {
        Err(io_error())
    } else {
        Ok(())
    }
}

fn io_error() -> ResourceError {
    ResourceError::Io(std::io::Error::last_os_error())
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
    use sha2::{Digest, Sha256};
    use tempfile::tempdir;

    #[test]
    fn put_rejects_empty_and_over_limit_data() {
        let dir = tempdir().unwrap();
        let store = ResourceStore::new(dir.path().to_path_buf()).unwrap();
        assert!(matches!(
            store.put(ResourceImport {
                bytes: &[],
                title: "empty",
                mime: "image/png",
                file_extension: "png",
            }),
            Err(ResourceError::InvalidData)
        ));
        let exact = vec![7u8; MAX_IMAGE_BYTES];
        assert_eq!(
            store
                .put(ResourceImport {
                    bytes: &exact,
                    title: "exact",
                    mime: "image/png",
                    file_extension: "png",
                })
                .unwrap()
                .size,
            MAX_IMAGE_BYTES
        );
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
        let result = ResourceStore::new(dir.path().to_path_buf());
        let error = result.err().expect("symlinked root must fail");
        assert!(matches!(error, ResourceError::Symlink));

        let dir = tempdir().unwrap();
        let store = ResourceStore::new(dir.path().to_path_buf()).unwrap();
        let blob_target_dir = tempdir().unwrap();
        let blob_target = blob_target_dir.path().join("outside");
        std::fs::write(&blob_target, b"outside").unwrap();
        let bytes = b"resource";
        let digest = super::hex_digest(&Sha256::digest(bytes));
        symlink(
            &blob_target,
            dir.path().join("resources").join("blobs").join(&digest),
        )
        .unwrap();
        let result = store.put(ResourceImport {
            bytes,
            title: "blob",
            mime: "image/png",
            file_extension: "png",
        });
        assert!(matches!(result, Err(ResourceError::Symlink)));
    }

    #[cfg(unix)]
    #[test]
    fn initialized_directory_handles_do_not_follow_a_replaced_resources_symlink() {
        use sha2::{Digest, Sha256};
        use std::fs::rename;
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let store = ResourceStore::new(dir.path().to_path_buf()).unwrap();
        let bytes = b"bound bytes";
        let blob = store
            .put(ResourceImport {
                bytes,
                title: "bound",
                mime: "image/png",
                file_extension: "png",
            })
            .unwrap();
        let outside = tempdir().unwrap();
        let outside_resources = outside.path().join("resources");
        let outside_blobs = outside_resources.join("blobs");
        std::fs::create_dir(&outside_resources).unwrap();
        std::fs::create_dir(&outside_blobs).unwrap();
        std::fs::write(outside_blobs.join(&blob.sha256), b"outside bytes").unwrap();
        let original = dir.path().join("resources");
        rename(&original, dir.path().join("resources-old")).unwrap();
        symlink(&outside_resources, &original).unwrap();
        assert_eq!(store.read_blob(&blob.sha256).unwrap(), bytes);
        assert_eq!(
            Sha256::digest(store.read_blob(&blob.sha256).unwrap()),
            Sha256::digest(bytes)
        );
    }

    #[cfg(unix)]
    #[test]
    fn fifo_digest_leaf_is_rejected_without_blocking() {
        use sha2::{Digest, Sha256};
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempdir().unwrap();
        let store = ResourceStore::new(dir.path().to_path_buf()).unwrap();
        let bytes = b"fifo bytes";
        let digest = super::hex_digest(&Sha256::digest(bytes));
        let path = dir.path().join("resources").join("blobs").join(&digest);
        let path = CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert!(matches!(
            store.put(ResourceImport {
                bytes,
                title: "fifo",
                mime: "image/png",
                file_extension: "png",
            }),
            Err(ResourceError::UnsafePath)
        ));
        assert!(matches!(
            store.read_blob(&digest),
            Err(ResourceError::UnsafePath)
        ));
    }
}
