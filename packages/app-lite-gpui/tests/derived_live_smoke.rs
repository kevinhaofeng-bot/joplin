#![cfg(target_os = "macos")]

use app_lite_core::document::Block;
use app_lite_core::{
    CanonicalDocument, CreateNote, DerivedTextStatus, LibraryRepository, SearchQuery,
};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Owns the ordinary GUI process for the whole smoke assertion. Dropping the
/// test scope always terminates and reaps it, including panic/timeout paths.
struct RunningApp(Option<Child>);

impl RunningApp {
    fn start(profile: &std::path::Path) -> Self {
        let child = Command::new(smoke_executable())
            .env("JOPLIN_LITE_PROFILE", profile)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start ordinary velotype app process");
        Self(Some(child))
    }

    fn finish(&mut self) -> Output {
        let mut child = self.0.take().expect("app process is still owned");
        let _ = child.kill();
        child.wait_with_output().expect("reap app process")
    }

    fn exit_context(&mut self) -> Option<String> {
        let exited = self.0.as_mut()?.try_wait().ok().flatten().is_some();
        if !exited {
            return None;
        }
        let output = self.finish();
        Some(format!(
            "ordinary app exited {:?}; stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ))
    }

    fn pid(&self) -> u32 {
        self.0.as_ref().expect("app process is still owned").id()
    }
}

fn smoke_executable() -> PathBuf {
    let executable = std::env::var_os("JOPLIN_LITE_LIVE_SMOKE_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_velotype")));
    assert!(
        executable.is_absolute(),
        "JOPLIN_LITE_LIVE_SMOKE_BIN must be an absolute executable path: {}",
        executable.display()
    );
    assert!(
        executable.is_file(),
        "live smoke executable does not exist: {}",
        executable.display()
    );
    executable
}

impl Drop for RunningApp {
    fn drop(&mut self) {
        if self.0.is_some() {
            let _ = self.finish();
        }
    }
}

fn rss_kib(pid: u32) -> Option<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    std::str::from_utf8(&output.stdout)
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn direct_child_peak_rss_kib(parent: u32) -> Option<u64> {
    let output = Command::new("pgrep")
        .args(["-P", &parent.to_string()])
        .output()
        .ok()?;
    std::str::from_utf8(&output.stdout)
        .ok()?
        .lines()
        .filter_map(|pid| pid.parse().ok())
        .filter_map(rss_kib)
        .max()
}

/// Manual macOS acceptance smoke for the real default app startup path.
///
/// It never uses a personal profile: the app receives a newly-created absolute
/// tempdir via `JOPLIN_LITE_PROFILE`. Run explicitly on a logged-in macOS GUI
/// host with `cargo test --test derived_live_smoke -- --ignored`. Set the
/// test-only `JOPLIN_LITE_LIVE_SMOKE_BIN` to an absolute binary path to smoke
/// a release build; it otherwise defaults to `CARGO_BIN_EXE_velotype`.
#[test]
#[ignore = "manual macOS WindowServer smoke"]
fn ordinary_app_indexes_a_selectable_pdf_into_english_and_chinese_search() {
    let profile = tempfile::tempdir().expect("temporary smoke profile");
    let database = profile.path().join("library.sqlite");
    let (resource, note) = {
        let repository = LibraryRepository::open(&database).expect("create isolated library");
        let resource = repository
            .import_resource(
                include_bytes!("resources/extractor-fixture.pdf"),
                "smoke-fixture.pdf",
                "application/pdf",
                "pdf",
            )
            .expect("import checked-in selectable PDF");
        let note = repository
            .create_note(CreateNote {
                title: "derived text smoke owner".into(),
                notebook_id: None,
                document: CanonicalDocument::from_blocks(vec![Block::Attachment {
                    resource_id: resource.clone(),
                    filename: "smoke-fixture.pdf".into(),
                    media_type: "application/pdf".into(),
                }]),
            })
            .expect("associate PDF without search target words in note metadata");
        (resource, note)
    };

    let mut app = RunningApp::start(profile.path());
    let observer = LibraryRepository::open(&database).expect("open independent smoke observer");
    let deadline = Instant::now() + Duration::from_secs(25);
    let mut child_peak_rss_kib = None;
    loop {
        if let Some(context) = app.exit_context() {
            panic!("{context}");
        }
        child_peak_rss_kib = child_peak_rss_kib.max(direct_child_peak_rss_kib(app.pid()));
        let status = observer
            .derived_text_status(&resource)
            .expect("read derived text status");
        if matches!(status, Some(DerivedTextStatus::Indexed { .. })) {
            for term in ["English", "中文可选文字检索"] {
                let hits = observer
                    .search(SearchQuery::parse(term))
                    .expect("search derived text");
                assert_eq!(hits.len(), 1, "{term}");
                assert_eq!(hits[0].note.id, note.id, "{term}");
                assert_eq!(hits[0].matched_resource, Some(resource.clone()), "{term}");
            }
            // Informational only: parent RSS is sampled after indexing and child
            // RSS is the greatest 10 ms observation, not a strict peak or gate.
            eprintln!(
                "D3b live smoke RSS sample: parent after indexing={} KiB; greatest observed child RSS={child_peak_rss_kib:?} KiB",
                rss_kib(app.pid()).unwrap_or_default()
            );
            return;
        }
        if Instant::now() >= deadline {
            let output = app.finish();
            panic!(
                "ordinary app did not index the fixture within 25s; last status: {status:?}; exit {:?}; stderr: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        // The fixture's one-shot child is short-lived; sample more often only
        // for this ignored acceptance test so the RSS line can include it.
        std::thread::sleep(Duration::from_millis(10));
    }
}
