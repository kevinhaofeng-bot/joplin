//! Crash recovery for *owned* restore scratch directories, never libraries.
//!
//! Independent native enhancement: an OS file lock, not a PID or age guess,
//! distinguishes an in-progress restore from one whose process has exited.
//! Unmarked legacy directories and published libraries are left untouched.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::Path,
};

const LEASE: &str = ".joplin-lite-restore-lease";
const OWNER: &[u8] = b"joplin-lite-owned-restore-stage-v1\n";

fn lease_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options
}

/// Hold the returned handle until publication has completed. The marker is
/// written only after acquiring the lock; a scanner ignores empty markers.
pub(super) fn claim(stage: &Path) -> io::Result<File> {
    let mut lease = lease_options().create_new(true).open(stage.join(LEASE))?;
    lease.lock()?;
    lease.write_all(OWNER)?;
    let name = stage
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "restore stage name is not UTF-8",
            )
        })?;
    lease.write_all(name.as_bytes())?;
    lease.write_all(b"\n")?;
    lease.sync_all()?;
    Ok(lease)
}

pub(super) fn finish(published: &Path) {
    // Publication already succeeded. Failure to remove an inert marker must
    // not turn it into a failed restore, and cleanup never scans published dirs.
    let _ = fs::remove_file(published.join(LEASE));
}

/// Reclaim abandoned stages inside this exact imports directory. Does not
/// create it, follow directory/marker symlinks, touch unmarked old stages,
/// or acquire a lock held by a live restore (including in another process).
pub fn cleanup_abandoned_restore_staging(parent: &Path) -> io::Result<usize> {
    match fs::symlink_metadata(parent) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "restore parent is not a regular directory",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut removed = 0;
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(suffix) = name
            .to_str()
            .and_then(|name| name.strip_prefix(".restore-"))
        else {
            continue;
        };
        if suffix.is_empty()
            || suffix.len() > 64
            || !suffix.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            continue;
        }
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let stage = entry.path();
        let marker = stage.join(LEASE);
        // Bind ownership to the ORIGINAL scratch name. A process may die
        // between the publishing rename and marker removal; even a published
        // destination beginning with .restore- must then be left alone.
        let expected_owner = [OWNER, name.to_str().unwrap().as_bytes(), b"\n"].concat();
        let Ok(marker_metadata) = fs::symlink_metadata(&marker) else {
            continue;
        };
        if !marker_metadata.is_file() || marker_metadata.len() != expected_owner.len() as u64 {
            continue;
        }
        let Ok(mut lease) = lease_options().open(&marker) else {
            continue;
        };
        if lease.try_lock().is_err() {
            continue;
        }
        if !lease.metadata()?.is_file() {
            continue;
        }
        let mut owner = Vec::with_capacity(expected_owner.len() + 1);
        (&mut lease)
            .take(expected_owner.len() as u64 + 1)
            .read_to_end(&mut owner)?;
        if owner != expected_owner {
            continue;
        }
        fs::remove_dir_all(&stage)?;
        removed += 1;
        // Keep the OS lock held through removal, not just the initial check.
        drop(lease);
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn crash_child_holds_a_real_restore_lease() {
        let Some(root) = std::env::var_os("JOPLIN_LITE_RESTORE_LEASE_TEST_ROOT") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let stage = tempfile::Builder::new()
            .prefix(".restore-")
            .tempdir_in(&root)
            .unwrap();
        let _lease = claim(stage.path()).unwrap();
        fs::write(
            stage.path().join("library.sqlite"),
            b"partial copy before process kill",
        )
        .unwrap();
        fs::write(
            root.join("ready.tmp"),
            stage.path().as_os_str().as_encoded_bytes(),
        )
        .unwrap();
        // The parent observes existence. Publish the complete path atomically,
        // never an empty file between create/truncate and write.
        fs::rename(root.join("ready.tmp"), root.join("ready")).unwrap();
        loop {
            std::thread::park();
        }
    }

    #[test]
    fn actual_killed_process_releases_lease_and_restart_cleanup_removes_only_its_stage() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join(".restore-unmarked");
        fs::create_dir(&old).unwrap();
        fs::write(old.join("library.sqlite"), b"must keep unknown data").unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "import_export::restore_staging::tests::crash_child_holds_a_real_restore_lease",
            ])
            .env("JOPLIN_LITE_RESTORE_LEASE_TEST_ROOT", root.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !root.path().join("ready").exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "child exited before holding a lease"
            );
            if std::time::Instant::now() > deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("child never reached actual locked partial copy");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let stage =
            std::path::PathBuf::from(fs::read_to_string(root.path().join("ready")).unwrap());
        let live_removed = cleanup_abandoned_restore_staging(root.path()).unwrap();
        let live_bytes = fs::read(stage.join("library.sqlite")).unwrap();
        child.kill().unwrap();
        assert!(!child.wait().unwrap().success());
        assert_eq!(live_removed, 0, "OS lock must protect another live process");
        assert_eq!(live_bytes, b"partial copy before process kill");
        assert_eq!(cleanup_abandoned_restore_staging(root.path()).unwrap(), 1);
        assert!(!stage.exists());
        assert_eq!(
            fs::read(old.join("library.sqlite")).unwrap(),
            b"must keep unknown data"
        );
    }

    #[test]
    fn cleanup_keeps_a_published_library_even_with_a_restore_prefixed_destination() {
        let root = tempfile::tempdir().unwrap();
        let stage = tempfile::Builder::new()
            .prefix(".restore-")
            .tempdir_in(root.path())
            .unwrap();
        let lease = claim(stage.path()).unwrap();
        fs::write(
            stage.path().join("library.sqlite"),
            b"published library, not scratch",
        )
        .unwrap();
        let destination = root.path().join(".restore-ZYX987");
        fs::rename(stage.keep(), &destination).unwrap();
        // Simulate process exit after publish but before finish removes the
        // marker. Prefix matching alone must NEVER destroy the published data.
        drop(lease);
        assert_eq!(cleanup_abandoned_restore_staging(root.path()).unwrap(), 0);
        assert_eq!(
            fs::read(destination.join("library.sqlite")).unwrap(),
            b"published library, not scratch"
        );
    }

    #[test]
    fn cleanup_skips_wrong_markers_and_unrelated_directories() {
        let root = tempfile::tempdir().unwrap();
        for (name, marker) in [
            (".restore-wrong", b"not an owner".as_slice()),
            ("published", OWNER),
            (".restore-", OWNER),
            (".restore-invalid.name", OWNER),
        ] {
            let dir = root.path().join(name);
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join(LEASE), marker).unwrap();
            fs::write(dir.join("library.sqlite"), b"not disposable").unwrap();
        }
        assert_eq!(cleanup_abandoned_restore_staging(root.path()).unwrap(), 0);
        for name in [
            ".restore-wrong",
            "published",
            ".restore-",
            ".restore-invalid.name",
        ] {
            assert_eq!(
                fs::read(root.path().join(name).join("library.sqlite")).unwrap(),
                b"not disposable"
            );
        }
        assert_eq!(
            cleanup_abandoned_restore_staging(&root.path().join("absent")).unwrap(),
            0
        );
        assert!(!root.path().join("absent").exists());
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_never_follows_stage_marker_or_parent_symlinks() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        fs::write(external.path().join(LEASE), OWNER).unwrap();
        fs::write(external.path().join("library.sqlite"), b"external original").unwrap();
        symlink(external.path(), root.path().join(".restore-linked")).unwrap();
        let stage = root.path().join(".restore-markerlink");
        fs::create_dir(&stage).unwrap();
        fs::write(stage.join("library.sqlite"), b"unowned data").unwrap();
        symlink(external.path().join(LEASE), stage.join(LEASE)).unwrap();
        assert_eq!(cleanup_abandoned_restore_staging(root.path()).unwrap(), 0);
        symlink(external.path(), root.path().join("parent-link")).unwrap();
        assert!(cleanup_abandoned_restore_staging(&root.path().join("parent-link")).is_err());
        assert_eq!(
            fs::read(external.path().join("library.sqlite")).unwrap(),
            b"external original"
        );
        assert_eq!(
            fs::read(stage.join("library.sqlite")).unwrap(),
            b"unowned data"
        );
    }
}
