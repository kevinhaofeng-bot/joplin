use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::{Mutex, OnceLock};
use thiserror::Error;

pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResourceId(String);

impl ResourceId {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ResourceError> {
        let value = value.as_ref();
        if value.len() == 32
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            Ok(Self(value.to_owned()))
        } else {
            Err(ResourceError::InvalidData)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlobHash(String);

impl BlobHash {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ResourceError> {
        let value = value.as_ref();
        if value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            Ok(Self(value.to_owned()))
        } else {
            Err(ResourceError::InvalidData)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[cfg(test)]
    fn from_bytes(bytes: &[u8]) -> Self {
        Self(hex_digest(&Sha256::digest(bytes)))
    }
}

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
    #[error("resource ID entropy failed")]
    Entropy(#[source] getrandom::Error),
}

#[derive(Clone, Copy)]
pub struct ResourceInput<'a> {
    pub bytes: &'a [u8],
    pub title: &'a str,
    pub mime: &'a str,
    pub file_extension: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceBlob {
    pub id: ResourceId,
    pub sha256: BlobHash,
    pub size: usize,
}

pub struct ResourceStore {
    blobs_dir: DirFd,
    // A preflight can create either directory independently. Keep distinct
    // descriptor-relative cleanup ownership so a failed migration never
    // removes a caller's pre-existing resources directory.
    cleanup_resources: Option<DirFd>,
    cleanup_profile: Option<DirFd>,
}

/// A profile directory held open by descriptor. SQLite still needs a pathname,
/// so repository open verifies this identity before and after each pathname
/// boundary; resource children are always opened relative to this descriptor.
pub(crate) struct ProfileDir {
    fd: DirFd,
    path: std::path::PathBuf,
    device: u64,
    inode: u64,
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
    pub fn new(profile_root: impl AsRef<Path>) -> Result<Self, ResourceError> {
        ensure_directory(profile_root.as_ref())?;
        let profile_root = fs::canonicalize(profile_root)?;
        let profile_dir = open_dir_path(&profile_root)?;
        let resources_dir = open_or_create_dir(profile_dir.0, "resources")?;
        let blobs_dir = open_or_create_dir(resources_dir.0, "blobs")?;
        Ok(Self {
            blobs_dir,
            cleanup_resources: None,
            cleanup_profile: None,
        })
    }

    pub(crate) fn from_profile_dir(profile: &ProfileDir) -> Result<Self, ResourceError> {
        let created_root = !child_exists(profile.fd.0, "resources")?;
        let resources_dir = open_or_create_dir(profile.fd.0, "resources")?;
        let created_blobs = !child_exists(resources_dir.0, "blobs")?;
        let blobs_dir = match open_or_create_dir(resources_dir.0, "blobs") {
            Ok(dir) => dir,
            Err(error) => {
                if created_root {
                    let _ = unlink_dir_at(profile.fd.0, "resources");
                }
                return Err(error);
            }
        };
        let cleanup_resources = if created_blobs {
            Some(duplicate_dir_fd(resources_dir.0)?)
        } else {
            None
        };
        let cleanup_profile = if created_root {
            Some(duplicate_dir_fd(profile.fd.0)?)
        } else {
            None
        };
        Ok(Self {
            blobs_dir,
            cleanup_resources,
            cleanup_profile,
        })
    }

    pub(crate) fn mark_published(&mut self) {
        self.cleanup_resources = None;
        self.cleanup_profile = None;
    }

    pub fn put(&self, input: ResourceInput<'_>) -> Result<ResourceBlob, ResourceError> {
        validate_import(&input)?;

        let digest = Sha256::digest(input.bytes);
        let sha256 = BlobHash::new(hex_digest(&digest)).expect("SHA-256 digest is valid");
        persist_blob(self.blobs_dir.0, input.bytes, sha256.as_str())?;
        Ok(ResourceBlob {
            id: ResourceId::new(new_resource_id()?).expect("generated resource id is valid"),
            sha256,
            size: input.bytes.len(),
        })
    }

    pub fn read(&self, sha256: &BlobHash) -> Result<Vec<u8>, ResourceError> {
        self.read_blob(sha256.as_str())
    }

    fn read_blob(&self, sha256: &str) -> Result<Vec<u8>, ResourceError> {
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
}

impl Drop for ResourceStore {
    fn drop(&mut self) {
        if let Some(resources) = self.cleanup_resources.take() {
            let _ = unlink_dir_at(resources.0, "blobs");
        }
        let Some(profile) = self.cleanup_profile.take() else {
            return;
        };
        let _ = unlink_dir_at(profile.0, "resources");
    }
}

impl ProfileDir {
    pub(crate) fn open(path: &Path) -> Result<Self, ResourceError> {
        // Bind the caller's lexical directory before doing any metadata or
        // canonical-path work.  `open(..., O_DIRECTORY|O_NOFOLLOW)` is the
        // authority; any replacement while deriving a display path is caught
        // by comparing the pathname back to this descriptor identity.
        let fd = open_dir_path(path)?;
        let (device, inode) = fd_identity(fd.0)?;
        let path = fs::canonicalize(path)?;
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.dev() != device
            || metadata.ino() != inode
        {
            return Err(ResourceError::UnsafePath);
        }
        Ok(Self {
            fd,
            path,
            device,
            inode,
        })
    }

    pub(crate) fn verify_path_identity(&self) -> Result<bool, ResourceError> {
        let metadata = fs::symlink_metadata(&self.path)?;
        Ok(metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode)
    }

    pub(crate) fn database_path(&self, name: &std::ffi::OsStr) -> Result<PathBuf, ResourceError> {
        let c_name =
            std::ffi::CString::new(name.as_bytes()).map_err(|_| ResourceError::UnsafePath)?;
        if child_is_symlink(self.fd.0, &c_name) {
            return Err(ResourceError::Symlink);
        }
        Ok(self.path.join(name))
    }

    /// Resolve the live display path of the already-bound descriptor.  This
    /// is only used to recover SQLite journal mode after a detected rename;
    /// SQLite itself is never opened through `/dev/fd`.
    pub(crate) fn current_database_path(
        &self,
        name: &std::ffi::OsStr,
    ) -> Result<PathBuf, ResourceError> {
        let c_name =
            std::ffi::CString::new(name.as_bytes()).map_err(|_| ResourceError::UnsafePath)?;
        if child_is_symlink(self.fd.0, &c_name) {
            return Err(ResourceError::Symlink);
        }
        Ok(fd_display_path(self.fd.0)?.join(name))
    }
}

fn fd_display_path(fd: RawFd) -> Result<PathBuf, ResourceError> {
    #[cfg(target_os = "macos")]
    {
        let mut buffer = [0_i8; libc::PATH_MAX as usize];
        if unsafe { libc::fcntl(fd, libc::F_GETPATH, buffer.as_mut_ptr()) } < 0 {
            return Err(io_error());
        }
        let path = unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) }
            .to_str()
            .map_err(|_| ResourceError::UnsafePath)?;
        return Ok(PathBuf::from(path));
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::fs::read_link(format!("/proc/self/fd/{fd}")).map_err(Into::into)
    }
}

fn validate_import(input: &ResourceInput<'_>) -> Result<(), ResourceError> {
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

    let temp_name = format!(".{sha256}.{}.tmp", new_resource_id()?);
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
    if let Err(error) = test_publish_hook(dir_fd, sha256) {
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

fn fd_identity(fd: RawFd) -> Result<(u64, u64), ResourceError> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } < 0 {
        return Err(io_error());
    }
    let stat = unsafe { stat.assume_init() };
    Ok((stat.st_dev as u64, stat.st_ino as u64))
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
        let child = DirFd(fd);
        fsync_fd(parent_fd)?;
        return Ok(child);
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
    let child = DirFd(fd);
    fsync_fd(parent_fd)?;
    Ok(child)
}

fn child_exists(parent_fd: RawFd, name: &str) -> Result<bool, ResourceError> {
    let name = std::ffi::CString::new(name).map_err(|_| ResourceError::UnsafePath)?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        libc::fstatat(
            parent_fd,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::NotFound {
        Ok(false)
    } else {
        Err(error.into())
    }
}

fn duplicate_dir_fd(fd: RawFd) -> Result<DirFd, ResourceError> {
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        Err(io_error())
    } else {
        Ok(DirFd(duplicate))
    }
}

fn unlink_dir_at(parent_fd: RawFd, name: &str) -> std::io::Result<()> {
    let name = std::ffi::CString::new(name)
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    if unsafe { libc::unlinkat(parent_fd, name.as_ptr(), libc::AT_REMOVEDIR) } < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
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
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::renameatx_np(
            old_dir,
            old.as_ptr(),
            new_dir,
            new.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(not(target_os = "macos"))]
    let result = unsafe { libc::linkat(old_dir, old.as_ptr(), new_dir, new.as_ptr(), 0) };
    if result < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        #[cfg(not(target_os = "macos"))]
        unlink_at(old_dir, old_name)?;
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

fn new_resource_id() -> Result<String, ResourceError> {
    let mut bytes = [0_u8; 16];
    getrandom::getrandom(&mut bytes).map_err(ResourceError::Entropy)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
enum PublishHook {
    Fail,
    PublishExisting(Vec<u8>),
}

#[cfg(test)]
struct PendingPublishHook {
    sha256: BlobHash,
    hook: PublishHook,
}

#[cfg(test)]
static PUBLISH_HOOK: OnceLock<Mutex<Vec<PendingPublishHook>>> = OnceLock::new();

#[cfg(test)]
fn publish_hook() -> &'static Mutex<Vec<PendingPublishHook>> {
    PUBLISH_HOOK.get_or_init(|| Mutex::new(Vec::new()))
}

#[cfg(test)]
fn fail_next_publish_after_temp_sync(sha256: BlobHash) {
    publish_hook().lock().unwrap().push(PendingPublishHook {
        sha256,
        hook: PublishHook::Fail,
    });
}

#[cfg(test)]
fn publish_existing_target_after_temp_sync(sha256: BlobHash, bytes: &[u8]) {
    publish_hook().lock().unwrap().push(PendingPublishHook {
        sha256,
        hook: PublishHook::PublishExisting(bytes.to_vec()),
    });
}

#[cfg(test)]
fn test_publish_hook(dir_fd: RawFd, sha256: &str) -> std::io::Result<()> {
    let mut pending = publish_hook().lock().unwrap();
    let Some(position) = pending
        .iter()
        .position(|candidate| candidate.sha256.as_str() == sha256)
    else {
        return Ok(());
    };
    let hook = pending.remove(position).hook;
    drop(pending);
    match hook {
        PublishHook::Fail => Err(std::io::Error::other("test publish interruption")),
        PublishHook::PublishExisting(bytes) => {
            let name = std::ffi::CString::new(sha256)
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
            let fd = unsafe {
                libc::openat(
                    dir_fd,
                    name.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mut file = unsafe { File::from_raw_fd(fd) };
            file.write_all(&bytes)?;
            file.sync_all()
        }
    }
}

#[cfg(not(test))]
fn test_publish_hook(_dir_fd: RawFd, _sha256: &str) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        BlobHash, MAX_IMAGE_BYTES, ResourceError, ResourceInput, ResourceStore,
        fail_next_publish_after_temp_sync, publish_existing_target_after_temp_sync,
    };
    use sha2::{Digest, Sha256};
    use tempfile::tempdir;

    #[test]
    fn after_temp_sync_failure_does_not_publish_or_poison_a_later_put() {
        // Catches direct-final writes and failure paths that leave a published target behind.
        let dir = tempdir().unwrap();
        let store = ResourceStore::new(dir.path()).unwrap();
        let bytes = b"recover after publish failure";
        let hash = BlobHash::from_bytes(bytes);

        fail_next_publish_after_temp_sync(hash.clone());
        assert!(matches!(
            store.put(ResourceInput {
                bytes,
                title: "failure",
                mime: "image/png",
                file_extension: "png",
            }),
            Err(ResourceError::Io(_))
        ));
        assert!(
            !dir.path()
                .join("resources/blobs")
                .join(hash.as_str())
                .exists()
        );
        assert_eq!(
            store
                .put(ResourceInput {
                    bytes,
                    title: "recovered",
                    mime: "image/png",
                    file_extension: "png",
                })
                .unwrap()
                .sha256,
            hash
        );
    }

    #[test]
    fn concurrent_existing_target_after_temp_sync_is_not_replaced() {
        // Catches overwrite-on-rename if another publisher creates the final name after temp sync.
        let dir = tempdir().unwrap();
        let store = ResourceStore::new(dir.path()).unwrap();
        let bytes = b"target race";
        let hash = BlobHash::from_bytes(bytes);

        publish_existing_target_after_temp_sync(hash.clone(), b"racing publisher");
        assert!(matches!(
            store.put(ResourceInput {
                bytes,
                title: "race",
                mime: "image/png",
                file_extension: "png",
            }),
            Err(ResourceError::CorruptBlob)
        ));
        assert_eq!(
            std::fs::read(dir.path().join("resources/blobs").join(hash.as_str())).unwrap(),
            b"racing publisher"
        );
    }

    #[test]
    fn put_rejects_empty_and_over_limit_data() {
        let dir = tempdir().unwrap();
        let store = ResourceStore::new(dir.path().to_path_buf()).unwrap();
        assert!(matches!(
            store.put(ResourceInput {
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
                .put(ResourceInput {
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
            store.put(ResourceInput {
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
        let result = store.put(ResourceInput {
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
            .put(ResourceInput {
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
        std::fs::write(outside_blobs.join(blob.sha256.as_str()), b"outside bytes").unwrap();
        let original = dir.path().join("resources");
        rename(&original, dir.path().join("resources-old")).unwrap();
        symlink(&outside_resources, &original).unwrap();
        assert_eq!(store.read_blob(blob.sha256.as_str()).unwrap(), bytes);
        assert_eq!(
            Sha256::digest(store.read_blob(blob.sha256.as_str()).unwrap()),
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
            store.put(ResourceInput {
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
