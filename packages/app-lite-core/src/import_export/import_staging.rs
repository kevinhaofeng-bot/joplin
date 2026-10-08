//! Import-only scratch recovery. Restore ownership remains a separate contract.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::Path,
};

const MARKER: &str = ".joplin-lite-import-lease";
const OWNER: &[u8] = b"joplin-lite-owned-import-stage-v1\n";

fn valid_name(name: &str) -> bool {
    ["jex-source-", "jex-stage-", "enex-stage-"]
        .iter()
        .any(|prefix| {
            name.strip_prefix(prefix).is_some_and(|suffix| {
                !suffix.is_empty()
                    && suffix.len() <= 64
                    && suffix.bytes().all(|b| b.is_ascii_alphanumeric())
            })
        })
}

fn options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options
}

/// Claim before writing any SQLite/blob bytes; hold through publication/Drop.
pub(super) fn claim(stage: &Path) -> io::Result<File> {
    let name = stage
        .file_name()
        .and_then(|v| v.to_str())
        .filter(|name| valid_name(name))
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "invalid import scratch name")
        })?;
    if !fs::symlink_metadata(stage)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "import scratch is not a directory",
        ));
    }
    let mut lease = options().create_new(true).open(stage.join(MARKER))?;
    lease.lock()?;
    lease.write_all(OWNER)?;
    lease.write_all(name.as_bytes())?;
    lease.write_all(b"\n")?;
    lease.sync_all()?;
    Ok(lease)
}

pub(super) fn finish(published: &Path) {
    let _ = fs::remove_file(published.join(MARKER));
}

/// Reclaim only owned, unlocked import stages inside this exact scratch parent.
pub fn cleanup_abandoned_import_staging(parent: &Path) -> io::Result<usize> {
    match fs::symlink_metadata(parent) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "import scratch parent is not a directory",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    }
    let mut removed = 0;
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|name| valid_name(name)) else {
            continue;
        };
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let stage = entry.path();
        let marker = stage.join(MARKER);
        // A crash between rename and finish must never make published data
        // collectible, even with a scratch-like destination name.
        let expected = [OWNER, name.as_bytes(), b"\n"].concat();
        let Ok(metadata) = fs::symlink_metadata(&marker) else {
            continue;
        };
        if !metadata.is_file() || metadata.len() != expected.len() as u64 {
            continue;
        }
        let Ok(mut lease) = options().open(&marker) else {
            continue;
        };
        if lease.try_lock().is_err() || !lease.metadata()?.is_file() {
            continue;
        }
        let mut owner = Vec::with_capacity(expected.len() + 1);
        (&mut lease)
            .take(expected.len() as u64 + 1)
            .read_to_end(&mut owner)?;
        if owner != expected {
            continue;
        }
        fs::remove_dir_all(&stage)?;
        removed += 1;
        drop(lease);
    }
    Ok(removed)
}
