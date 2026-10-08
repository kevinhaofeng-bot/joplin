//! Regression for killed imports leaking partial copies. Test real producers.
use app_lite_core::{
    cleanup_abandoned_import_staging, prepare_jex_source_archive, publish_staged_library_with_hook,
    stage_enex_file, stage_jex_file,
};
use std::{
    fs::{self, File, OpenOptions},
    io::{Cursor, Write},
    path::Path,
    sync::atomic::AtomicBool,
};
use tempfile::{NamedTempFile, tempdir};

const MARKER: &str = ".joplin-lite-import-lease";

fn assert_live_lease(stage: &Path) {
    let name = stage.file_name().unwrap().to_str().unwrap();
    assert_eq!(
        fs::read(stage.join(MARKER))
            .expect("every import scratch must have a durable ownership lease"),
        format!("joplin-lite-owned-import-stage-v1\n{name}\n").as_bytes()
    );
    let marker = OpenOptions::new()
        .read(true)
        .write(true)
        .open(stage.join(MARKER))
        .unwrap();
    assert!(
        marker.try_lock().is_err(),
        "live import must exclude another cleaner"
    );
}

fn enex() -> NamedTempFile {
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(b"<en-export><note><title>lease fixture</title><content><![CDATA[<en-note><div>preserve me</div></en-note>]]></content></note></en-export>").unwrap();
    file
}

fn jex() -> NamedTempFile {
    let file = NamedTempFile::new().unwrap();
    let mut archive = tar::Builder::new(File::create(file.path()).unwrap());
    let bytes = b"lease fixture\n\npreserve me\n\nid: 11111111111111111111111111111111\ntype_: 1\nparent_id: \nmarkup_language: 1\ncreated_time: 2023-11-14T22:13:21.000Z\nupdated_time: 2023-11-14T22:13:22.000Z\nuser_created_time: 2023-11-14T22:13:23.000Z\nuser_updated_time: 2023-11-14T22:13:24.000Z\n";
    let mut header = tar::Header::new_gnu();
    header.set_mode(0o644);
    header.set_size(bytes.len() as u64);
    header.set_cksum();
    archive
        .append_data(
            &mut header,
            "11111111111111111111111111111111.md",
            Cursor::new(bytes),
        )
        .unwrap();
    archive.finish().unwrap();
    file
}

#[test]
fn actual_enex_stage_keeps_lease_through_publish_then_removes_marker() {
    let root = tempdir().unwrap();
    let stage = stage_enex_file(enex().path(), root.path()).unwrap();
    let path = stage.profile_path().to_path_buf();
    assert_live_lease(&path);
    let destination = root.path().join("published-enex");
    publish_staged_library_with_hook(stage, &destination, &AtomicBool::new(false), || {
        assert_live_lease(&path);
        assert_eq!(cleanup_abandoned_import_staging(root.path()).unwrap(), 0);
        Ok(())
    })
    .unwrap();
    assert!(!path.exists());
    assert!(!destination.join(MARKER).exists());
    assert!(destination.join("library.sqlite").is_file());
}

#[test]
fn crash_child_holds_an_actual_import_lease() {
    let Some(root) = std::env::var_os("JOPLIN_LITE_IMPORT_CRASH_TEST_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let stage = stage_enex_file(enex().path(), &root).unwrap();
    assert_live_lease(stage.profile_path());
    fs::write(
        root.join("ready.tmp"),
        stage.profile_path().as_os_str().as_encoded_bytes(),
    )
    .unwrap();
    fs::rename(root.join("ready.tmp"), root.join("ready")).unwrap();
    loop {
        std::thread::park();
    }
}

#[test]
fn killed_real_import_is_reclaimed_but_live_process_is_preserved() {
    use std::process::{Command, Stdio};
    let root = tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_child_holds_an_actual_import_lease"])
        .env("JOPLIN_LITE_IMPORT_CRASH_TEST_ROOT", root.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !root.path().join("ready").exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "child must actually hold the lease"
        );
        if std::time::Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("child never claimed its stage");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let stage = std::path::PathBuf::from(fs::read_to_string(root.path().join("ready")).unwrap());
    let live_result = cleanup_abandoned_import_staging(root.path()).unwrap();
    let still_live = stage.join("library.sqlite").is_file();
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    assert_eq!(live_result, 0);
    assert!(still_live);
    assert_eq!(cleanup_abandoned_import_staging(root.path()).unwrap(), 1);
    assert!(!stage.exists());
}

#[cfg(unix)]
#[test]
fn cleanup_never_follows_scratch_parent_stage_or_marker_symlinks() {
    use std::os::unix::fs::symlink;
    let root = tempdir().unwrap();
    let external = tempdir().unwrap();
    fs::write(external.path().join("keep"), b"external original").unwrap();
    fs::write(
        external.path().join(MARKER),
        b"joplin-lite-owned-import-stage-v1\nenex-stage-LINK123\n",
    )
    .unwrap();
    symlink(external.path(), root.path().join("enex-stage-LINK123")).unwrap();
    let stage = root.path().join("enex-stage-MARK123");
    fs::create_dir(&stage).unwrap();
    // Exact matching ownership content: this must be rejected because it is
    // a symlink, not merely because its target has another stage's name.
    fs::write(
        external.path().join("linked-marker"),
        b"joplin-lite-owned-import-stage-v1\nenex-stage-MARK123\n",
    )
    .unwrap();
    symlink(external.path().join("linked-marker"), stage.join(MARKER)).unwrap();
    fs::write(stage.join("keep"), b"unknown original").unwrap();
    assert_eq!(cleanup_abandoned_import_staging(root.path()).unwrap(), 0);
    symlink(external.path(), root.path().join("parent-link")).unwrap();
    assert!(cleanup_abandoned_import_staging(&root.path().join("parent-link")).is_err());
    assert_eq!(fs::read(stage.join("keep")).unwrap(), b"unknown original");
    assert_eq!(
        fs::read(external.path().join("keep")).unwrap(),
        b"external original"
    );
}

#[test]
fn actual_jex_source_and_profile_have_live_owned_leases() {
    let root = tempdir().unwrap();
    let source = jex();
    let prepared = prepare_jex_source_archive(source.path(), root.path()).unwrap();
    let source_stage = prepared.staging_path().to_path_buf();
    assert_live_lease(&source_stage);
    drop(prepared);
    assert!(
        !source_stage.exists(),
        "normal Drop must still clean its source scratch"
    );
    let stage = stage_jex_file(source.path(), root.path()).unwrap();
    let path = stage.profile_path().to_path_buf();
    assert_live_lease(&path);
    let destination = root.path().join("published-jex");
    publish_staged_library_with_hook(stage, &destination, &AtomicBool::new(false), || {
        assert_live_lease(&path);
        Ok(())
    })
    .unwrap();
    assert!(!destination.join(MARKER).exists());
}

#[test]
fn cleanup_reclaims_only_valid_unlocked_import_scratch() {
    let root = tempdir().unwrap();
    let dead = root.path().join("jex-source-ABC123");
    fs::create_dir(&dead).unwrap();
    fs::write(
        dead.join(MARKER),
        b"joplin-lite-owned-import-stage-v1\njex-source-ABC123\n",
    )
    .unwrap();
    fs::write(dead.join("jex-source.sqlite"), b"dead partial copy").unwrap();
    for (name, marker) in [
        ("jex-source-unknown", b"".as_slice()),
        ("enex-stage-wrong", b"not an owner".as_slice()),
        (
            "jex-stage-PUB999",
            b"joplin-lite-owned-import-stage-v1\njex-stage-OLD123\n".as_slice(),
        ),
        (
            "published",
            b"joplin-lite-owned-import-stage-v1\npublished\n".as_slice(),
        ),
    ] {
        let path = root.path().join(name);
        fs::create_dir(&path).unwrap();
        fs::write(path.join(MARKER), marker).unwrap();
        fs::write(path.join("keep"), b"not disposable").unwrap();
    }
    assert_eq!(cleanup_abandoned_import_staging(root.path()).unwrap(), 1);
    assert!(!dead.exists());
    for name in [
        "jex-source-unknown",
        "enex-stage-wrong",
        "jex-stage-PUB999",
        "published",
    ] {
        assert_eq!(
            fs::read(root.path().join(name).join("keep")).unwrap(),
            b"not disposable"
        );
    }
}
