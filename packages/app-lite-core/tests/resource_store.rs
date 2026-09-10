use app_lite_core::{BlobHash, ResourceError, ResourceInput, ResourceStore};
use std::fs;
use tempfile::tempdir;

fn png_input(bytes: &[u8]) -> ResourceInput<'_> {
    ResourceInput {
        bytes,
        title: "evidence",
        mime: "image/png",
        file_extension: "png",
    }
}

#[test]
fn put_publishes_sha256_addressed_bytes_that_read_back() {
    // Catches publication without durable content-addressed storage or a read path that ignores integrity.
    let root = tempdir().unwrap();
    let store = ResourceStore::new(root.path()).unwrap();
    let stored = store.put(png_input(b"evidence bytes")).unwrap();

    assert_eq!(stored.sha256.as_str().len(), 64);
    assert_eq!(stored.size, 14);
    assert_eq!(store.read(&stored.sha256).unwrap(), b"evidence bytes");
}

#[test]
fn read_observer_reports_the_exact_blob_that_was_hydrated() {
    // Pairs with the library projection's zero-read assertion: deleting the
    // actual notification from ResourceStore::read must make this fail.
    let root = tempdir().unwrap();
    let store = ResourceStore::new(root.path()).unwrap();
    let stored = store.put(png_input(b"observed resource bytes")).unwrap();
    let reads = store.observe_reads();

    assert_eq!(
        store.read(&stored.sha256).unwrap(),
        b"observed resource bytes"
    );
    assert_eq!(reads.recv().unwrap(), stored.sha256);
}

#[test]
fn put_keeps_duplicate_blobs_addressed_once_and_readable() {
    // Catches duplicate input publishing divergent blobs or corrupting the first publication.
    let root = tempdir().unwrap();
    let store = ResourceStore::new(root.path()).unwrap();
    let first = store.put(png_input(b"same bytes")).unwrap();
    let second = store.put(png_input(b"same bytes")).unwrap();

    assert_ne!(first.id, second.id);
    assert_eq!(first.sha256, second.sha256);
    assert_eq!(store.read(&first.sha256).unwrap(), b"same bytes");
    assert_eq!(
        fs::read_dir(root.path().join("resources/blobs"))
            .unwrap()
            .count(),
        1
    );
}

#[cfg(unix)]
#[test]
fn store_refuses_symlinked_resource_roots_and_blob_leaves() {
    // Catches path traversal through a profile resource symlink or a digest-named blob symlink.
    use std::os::unix::fs::symlink;

    let root = tempdir().unwrap();
    let outside = tempdir().unwrap();
    symlink(outside.path(), root.path().join("resources")).unwrap();
    assert!(matches!(
        ResourceStore::new(root.path()),
        Err(ResourceError::Symlink)
    ));

    let root = tempdir().unwrap();
    let store = ResourceStore::new(root.path()).unwrap();
    let target = outside.path().join("outside");
    fs::write(&target, b"outside").unwrap();
    let digest = "a6e5f1b54d6ef6f3129d2777d51e0ad595ccc0d99d5d2b52b8c67a9dd4153383";
    symlink(&target, root.path().join("resources/blobs").join(digest)).unwrap();
    let digest = BlobHash::new(digest).unwrap();
    assert!(matches!(store.read(&digest), Err(ResourceError::Symlink)));
}

#[test]
fn interrupted_temporary_write_is_not_published_as_a_blob() {
    // Catches a recovery path treating a real leftover temporary name as committed resource data.
    let root = tempdir().unwrap();
    let store = ResourceStore::new(root.path()).unwrap();
    let blobs = root.path().join("resources/blobs");
    let temporary_hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let temporary_name = format!(".{temporary_hash}.0123456789abcdef0123456789abcdef.tmp");
    fs::write(blobs.join(&temporary_name), b"partial").unwrap();

    let temporary_hash = BlobHash::new(temporary_hash).unwrap();
    assert!(matches!(
        store.read(&temporary_hash),
        Err(ResourceError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound
    ));

    let stored = store.put(png_input(b"complete bytes")).unwrap();
    assert_eq!(store.read(&stored.sha256).unwrap(), b"complete bytes");
    assert!(blobs.join(temporary_name).is_file());
}

#[test]
fn read_and_duplicate_put_reject_a_tampered_published_blob() {
    // Catches removal of digest verification from read or deduplication.
    let root = tempdir().unwrap();
    let store = ResourceStore::new(root.path()).unwrap();
    let stored = store.put(png_input(b"integrity bytes")).unwrap();
    let path = root
        .path()
        .join("resources/blobs")
        .join(stored.sha256.as_str());
    fs::write(path, b"tampered").unwrap();

    assert!(matches!(
        store.read(&stored.sha256),
        Err(ResourceError::CorruptBlob)
    ));
    assert!(matches!(
        store.put(png_input(b"integrity bytes")),
        Err(ResourceError::CorruptBlob)
    ));
}

#[cfg(unix)]
#[test]
fn store_stays_bound_to_its_original_directory_after_resources_is_replaced() {
    // Catches a public resource result or read path that follows the replacement symlink.
    use std::fs::rename;
    use std::os::unix::fs::symlink;

    let root = tempdir().unwrap();
    let store = ResourceStore::new(root.path()).unwrap();
    let outside = tempdir().unwrap();
    let outside_resources = outside.path().join("resources");
    fs::create_dir(&outside_resources).unwrap();
    fs::create_dir(outside_resources.join("blobs")).unwrap();
    rename(
        root.path().join("resources"),
        root.path().join("resources-old"),
    )
    .unwrap();
    symlink(&outside_resources, root.path().join("resources")).unwrap();

    let stored = store.put(png_input(b"bound after replacement")).unwrap();
    assert_eq!(
        store.read(&stored.sha256).unwrap(),
        b"bound after replacement"
    );
    assert!(
        !outside_resources
            .join("blobs")
            .join(stored.sha256.as_str())
            .exists()
    );
}

#[test]
fn store_rejects_empty_and_oversized_resource_data() {
    // Catches resource limits being bypassed before filesystem publication.
    let root = tempdir().unwrap();
    let store = ResourceStore::new(root.path()).unwrap();
    assert!(matches!(
        store.put(png_input(b"")),
        Err(ResourceError::InvalidData)
    ));
    let too_large = vec![0; 10 * 1024 * 1024 + 1];
    assert!(matches!(
        store.put(png_input(&too_large)),
        Err(ResourceError::InvalidData)
    ));
}
