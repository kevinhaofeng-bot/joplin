use std::{
    fs::OpenOptions,
    os::{fd::AsFd, unix::fs::OpenOptionsExt},
    path::PathBuf,
};

use crate::profile::ProfilePaths;

use super::protocol::{SidecarError, SidecarErrorKind};

pub const LEASE_FILE_NAME: &str = ".com.kevinhao.joplin-lite.canonical.lock";

#[derive(Debug)]
pub struct ProfileLease {
    file: std::fs::File,
}

impl ProfileLease {
    pub fn acquire(paths: &ProfilePaths) -> Result<Self, SidecarError> {
        let lock_path = lease_path(paths);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&lock_path)
            .map_err(|_| SidecarError::new(SidecarErrorKind::ProfileLockRequired, "lease open"))?;
        let metadata = rustix::fs::fstat(file.as_fd())
            .map_err(|_| SidecarError::new(SidecarErrorKind::ProfileLockRequired, "lease stat"))?;
        if rustix::fs::FileType::from_raw_mode(metadata.st_mode)
            != rustix::fs::FileType::RegularFile
            || rustix::fs::Mode::from_raw_mode(metadata.st_mode).bits() != 0o600
        {
            return Err(SidecarError::new(
                SidecarErrorKind::ProfileLockRequired,
                "lease type",
            ));
        }
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            let errno = std::io::Error::last_os_error().raw_os_error();
            if errno == Some(libc::EWOULDBLOCK) || errno == Some(libc::EAGAIN) {
                return Err(SidecarError::new(
                    SidecarErrorKind::ProfileInUse,
                    "lease busy",
                ));
            }
            return Err(SidecarError::new(
                SidecarErrorKind::ProfileLockRequired,
                "lease flock",
            ));
        }
        Ok(Self { file })
    }

    pub fn raw_fd(&self) -> std::os::unix::io::RawFd {
        self.file.as_raw_fd()
    }
}

pub fn lease_path(paths: &ProfilePaths) -> PathBuf {
    paths
        .root()
        .parent()
        .expect("validated profile has a parent")
        .join(LEASE_FILE_NAME)
}

use std::os::unix::io::AsRawFd;

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::symlink,
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn profile() -> (PathBuf, ProfilePaths) {
        let root = std::env::temp_dir().join(format!(
            "joplin-lease-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let profile = root.join(crate::profile::EXPECTED_PROFILE_DIRECTORY_NAME);
        let paths = ProfilePaths::try_from_app_data(profile).unwrap();
        paths.ensure().unwrap();
        (root, paths)
    }

    #[test]
    fn first_lease_excludes_second_until_drop() {
        let (root, paths) = profile();
        let first = ProfileLease::acquire(&paths).expect("first lease");
        let second = ProfileLease::acquire(&paths).unwrap_err();
        assert_eq!(second.kind(), SidecarErrorKind::ProfileInUse);
        drop(first);
        ProfileLease::acquire(&paths).expect("released lease");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn symlink_lease_is_rejected_without_following() {
        let (root, paths) = profile();
        let lock = lease_path(&paths);
        ProfileLease::acquire(&paths).expect("create lease");
        fs::remove_file(&lock).unwrap();
        let target = root.join("outside-lock");
        fs::write(&target, "fixture").unwrap();
        symlink(&target, &lock).unwrap();
        let error = ProfileLease::acquire(&paths).unwrap_err();
        assert_eq!(error.kind(), SidecarErrorKind::ProfileLockRequired);
        assert_eq!(fs::read_to_string(target).unwrap(), "fixture");
        fs::remove_dir_all(root).unwrap();
    }
}
