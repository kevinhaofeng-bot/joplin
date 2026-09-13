use std::{
    fs::{self, File},
    io::Cursor,
    sync::{Arc, atomic::AtomicBool},
};

use app_lite_core::{
    JexFieldDisposition, JexPrepareError, JexQualificationBlockerKind, JexQualificationError,
    qualify_jex_archive, qualify_jex_archive_with_cancel,
};
use tar::{Builder, Header};
use tempfile::{NamedTempFile, TempPath, tempdir};

const CLEAN: &str = "11111111111111111111111111111111";
const URL_ORDER: &str = "22222222222222222222222222222222";
const TODO: &str = "33333333333333333333333333333333";
const BAD_BODY: &str = "44444444444444444444444444444444";
const TAG: &str = "55555555555555555555555555555555";
const REL: &str = "66666666666666666666666666666666";
const RESOURCE: &str = "77777777777777777777777777777777";
const ROOT_A: &str = "88888888888888888888888888888888";
const ROOT_B: &str = "99999999999999999999999999999999";
const CHILD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BAD_TIME: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const GIF_RESOURCE: &str = "cccccccccccccccccccccccccccccccc";
const MISSING_TARGET: &str = "dddddddddddddddddddddddddddddddd";

fn append(tar: &mut Builder<File>, path: &str, bytes: &[u8]) {
    let mut header = Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, path, Cursor::new(bytes))
        .unwrap();
}

fn archive(entries: impl FnOnce(&mut Builder<File>)) -> TempPath {
    let path = NamedTempFile::new().unwrap().into_temp_path();
    let mut tar = Builder::new(File::create(&path).unwrap());
    entries(&mut tar);
    tar.finish().unwrap();
    path
}

fn exporter_note(id: &str, body: &str, overrides: &[(&str, &str)]) -> String {
    // FieldNames projection from JoplinDatabase + BaseItem.serialize: every
    // default is emitted, not omitted like the earlier minimal stage fixtures.
    let mut props = vec![
        ("id", id),
        ("parent_id", ""),
        ("created_time", "2023-11-14T22:13:21.000Z"),
        ("updated_time", "2023-11-14T22:13:22.000Z"),
        ("user_created_time", "2023-11-14T22:13:23.000Z"),
        ("user_updated_time", "2023-11-14T22:13:24.000Z"),
        ("markup_language", "1"),
        ("is_conflict", "0"),
        ("latitude", "0"),
        ("longitude", "0"),
        ("altitude", "0"),
        ("author", ""),
        ("source_url", ""),
        ("is_todo", "0"),
        ("todo_due", "0"),
        ("todo_completed", "0"),
        ("source", ""),
        ("source_application", ""),
        ("application_data", ""),
        ("order", "0"),
        ("deleted_time", "0"),
        ("encryption_applied", "0"),
        ("encryption_cipher_text", ""),
        ("master_key_id", ""),
        ("share_id", ""),
        ("is_shared", "0"),
        ("is_locked", "0"),
        ("extracted_resource_ids", ""),
        ("conflict_original_id", ""),
        ("user_data", ""),
        ("type_", "1"),
    ];
    for (key, value) in overrides {
        props
            .iter_mut()
            .find(|(candidate, _)| candidate == key)
            .unwrap()
            .1 = value;
    }
    let props = props
        .into_iter()
        .map(|(key, value)| format!("{key}: {value}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("导出笔记\n\n{body}\n\n{props}")
}

fn exporter_tag() -> String {
    format!(
        "要务\n\nid: {TAG}\ncreated_time: 2023-11-14T22:13:21.000Z\nupdated_time: 2023-11-14T22:13:22.000Z\nuser_created_time: \nuser_updated_time: \nparent_id: \nencryption_applied: 0\nencryption_cipher_text: \nis_shared: 0\nuser_data: \ntype_: 5"
    )
}

fn exporter_relation() -> String {
    format!(
        "id: {REL}\nnote_id: {CLEAN}\ntag_id: {TAG}\ncreated_time: 2023-11-14T22:13:21.000Z\nupdated_time: 2023-11-14T22:13:22.000Z\nuser_created_time: \nuser_updated_time: \nencryption_applied: 0\nencryption_cipher_text: \nis_shared: 0\ntype_: 6"
    )
}

fn exporter_folder(id: &str, parent_id: &str, title: &str) -> String {
    format!(
        "{title}\n\nid: {id}\nparent_id: {parent_id}\ncreated_time: 2023-11-14T22:13:21.000Z\nupdated_time: 2023-11-14T22:13:22.000Z\nuser_created_time: \nuser_updated_time: \ndeleted_time: 0\nencryption_applied: 0\nencryption_cipher_text: \nicon: \nis_shared: 0\nmaster_key_id: \nshare_id: \nuser_data: \ntype_: 2"
    )
}

fn fixture(reverse: bool) -> TempPath {
    let mut entries = vec![
        (format!("{CLEAN}.md"), exporter_note(CLEAN, "普通正文", &[])),
        (
            format!("{URL_ORDER}.md"),
            exporter_note(
                URL_ORDER,
                "稍后阅读",
                &[
                    ("source_url", "https://example.org/article"),
                    ("order", "42"),
                ],
            ),
        ),
        (
            format!("{TODO}.md"),
            exporter_note(
                TODO,
                "任务",
                &[("is_todo", "1"), ("todo_due", "1700000000000")],
            ),
        ),
        (
            format!("{BAD_BODY}.md"),
            exporter_note(BAD_BODY, "|A|B|\n|-|-|\n|1|2|", &[]),
        ),
        (
            format!("{BAD_TIME}.md"),
            exporter_note(BAD_TIME, "时间", &[("updated_time", "invalid-date")]),
        ),
        (format!("{TAG}.md"), exporter_tag()),
        (format!("{REL}.md"), exporter_relation()),
    ];
    if reverse {
        entries.reverse();
    }
    archive(|tar| {
        for (path, bytes) in entries {
            append(tar, &path, bytes.as_bytes());
        }
    })
}

fn listing(path: &std::path::Path) -> Vec<std::ffi::OsString> {
    let mut names = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn exporter_defaults_and_later_semantic_blockers_are_aggregated_independent_of_tar_order() {
    // Mutation caught: stopping at the first strict-stage metadata error, or
    // treating source raw audit as preserved todo/source URL/order behavior.
    let parent = tempdir().unwrap();
    fs::write(parent.path().join("sentinel.bin"), b"keep").unwrap();
    let mut reports = Vec::new();
    for reverse in [false, true] {
        let source = fixture(reverse);
        let original = fs::read(&source).unwrap();
        let report = qualify_jex_archive(&source, parent.path()).unwrap();
        assert_eq!(report.counts.notes, 5);
        assert_eq!(report.counts.tags, 1);
        assert_eq!(report.counts.note_tag_relations, 1);
        assert!(!report.ready_for_current_stage);
        let semantics = report
            .category(JexQualificationBlockerKind::UnmappedSemanticField)
            .unwrap();
        assert_eq!(semantics.item_count, 2);
        assert!(semantics.samples.iter().any(|s| s.source_id == URL_ORDER
            && s.source_path == format!("{URL_ORDER}.md")
            && s.field.as_deref() == Some("source_url")));
        assert!(
            semantics
                .samples
                .iter()
                .any(|s| s.source_id == TODO && s.field.as_deref() == Some("is_todo"))
        );
        assert_eq!(
            report
                .category(JexQualificationBlockerKind::BodyFidelity)
                .unwrap()
                .item_count,
            1
        );
        assert_eq!(
            report
                .category(JexQualificationBlockerKind::InvalidSourceTimestamp)
                .unwrap()
                .item_count,
            1
        );
        assert!(
            report
                .category(JexQualificationBlockerKind::InvalidSourceTimestamp)
                .unwrap()
                .samples
                .iter()
                .any(|s| s.source_id == BAD_TIME
                    && s.source_path == format!("{BAD_TIME}.md")
                    && s.field.as_deref() == Some("updated_time"))
        );
        assert!(
            report
                .category(JexQualificationBlockerKind::ExporterFieldGap)
                .unwrap()
                .item_count
                >= 5
        );
        assert_eq!(fs::read(&source).unwrap(), original);
        assert_eq!(
            listing(parent.path()),
            vec![std::ffi::OsString::from("sentinel.bin")]
        );
        reports.push(report);
    }
    assert_eq!(reports[0], reports[1]);
    assert_eq!(
        fs::read(parent.path().join("sentinel.bin")).unwrap(),
        b"keep"
    );
}

#[test]
fn mixed_root_and_duplicate_sibling_title_are_separate_source_located_blockers() {
    let source = archive(|tar| {
        append(
            tar,
            &format!("{ROOT_A}.md"),
            exporter_folder(ROOT_A, "", "项目").as_bytes(),
        );
        append(
            tar,
            &format!("{ROOT_B}.md"),
            exporter_folder(ROOT_B, "", "项目").as_bytes(),
        );
        append(
            tar,
            &format!("{CHILD}.md"),
            exporter_folder(CHILD, ROOT_A, "子本").as_bytes(),
        );
        append(
            tar,
            &format!("{CLEAN}.md"),
            exporter_note(CLEAN, "正文", &[("parent_id", ROOT_A)]).as_bytes(),
        );
    });
    let parent = tempdir().unwrap();
    let report = qualify_jex_archive(&source, parent.path()).unwrap();
    let hierarchy = report
        .category(JexQualificationBlockerKind::FolderHierarchy)
        .unwrap();
    assert!(
        hierarchy
            .samples
            .iter()
            .any(|s| s.source_id == ROOT_A && s.reason.contains("both own notes"))
    );
    assert!(
        hierarchy
            .samples
            .iter()
            .any(|s| s.source_id == ROOT_B && s.reason.contains("duplicate sibling"))
    );
    assert!(listing(parent.path()).is_empty());
}

#[test]
fn resource_signature_mismatch_is_reported_without_creating_a_library_profile() {
    // Mutation caught: the source spool hash alone being mistaken for a
    // stage-compatible PDF while the 3b importer would reject its signature.
    let resource = format!(
        "附件.pdf\n\nid: {RESOURCE}\ntype_: 4\nmime: application/pdf\nfile_extension: pdf\ncreated_time: 2023-11-14T22:13:21.000Z\nupdated_time: 2023-11-14T22:13:22.000Z\n"
    );
    let source = archive(|tar| {
        append(
            tar,
            &format!("{CLEAN}.md"),
            exporter_note(CLEAN, &format!("[附件.pdf](:/{RESOURCE})"), &[]).as_bytes(),
        );
        append(tar, &format!("{RESOURCE}.md"), resource.as_bytes());
        append(tar, &format!("resources/{RESOURCE}.pdf"), b"not-a-pdf");
    });
    let parent = tempdir().unwrap();
    fs::write(parent.path().join("sentinel.bin"), b"keep").unwrap();
    let report = qualify_jex_archive(&source, parent.path()).unwrap();
    assert_eq!(report.counts.resource_metadata, 1);
    assert!(
        report
            .category(JexQualificationBlockerKind::StageValidation)
            .unwrap()
            .samples
            .iter()
            .any(|sample| sample.source_id == RESOURCE
                && sample.source_path == format!("{RESOURCE}.md"))
    );
    assert_eq!(
        listing(parent.path()),
        vec![std::ffi::OsString::from("sentinel.bin")]
    );
}

#[test]
fn mixed_source_newlines_are_not_hidden_by_exporter_field_gaps() {
    let item =
        exporter_note(CLEAN, "可见正文", &[]).replacen("导出笔记\n\n", "导出笔记\r\n\r\n", 1);
    let source = archive(|tar| append(tar, &format!("{CLEAN}.md"), item.as_bytes()));
    let parent = tempdir().unwrap();
    let report = qualify_jex_archive(&source, parent.path()).unwrap();
    assert!(
        report
            .category(JexQualificationBlockerKind::StageValidation)
            .unwrap()
            .samples
            .iter()
            .any(|sample| sample.source_id == CLEAN && sample.reason.contains("newline"))
    );
    assert!(listing(parent.path()).is_empty());
}

#[test]
fn corrupt_archive_and_cancellation_leave_only_sentinel() {
    let parent = tempdir().unwrap();
    fs::write(parent.path().join("sentinel.bin"), b"keep").unwrap();
    let source = archive(|tar| {
        append(
            tar,
            &format!("{CLEAN}.md"),
            exporter_note(CLEAN, "正文", &[]).as_bytes(),
        )
    });
    let cancelled =
        qualify_jex_archive_with_cancel(&source, parent.path(), Arc::new(AtomicBool::new(true)));
    assert!(matches!(
        cancelled,
        Err(JexQualificationError::Prepare(JexPrepareError::Cancelled))
    ));
    let corrupt = NamedTempFile::new().unwrap();
    fs::write(corrupt.path(), b"not a tar archive").unwrap();
    assert!(matches!(
        qualify_jex_archive(corrupt.path(), parent.path()),
        Err(JexQualificationError::Prepare(JexPrepareError::Scan(_)))
    ));
    assert_eq!(
        listing(parent.path()),
        vec![std::ffi::OsString::from("sentinel.bin")]
    );
    assert_eq!(
        fs::read(parent.path().join("sentinel.bin")).unwrap(),
        b"keep"
    );
}

#[test]
fn semantic_findings_count_all_items_but_bound_source_samples() {
    let source = archive(|tar| {
        for number in 100..112 {
            let id = format!("{number:032x}");
            append(
                tar,
                &format!("{id}.md"),
                exporter_note(&id, "正文", &[("source_url", "https://example.org")]).as_bytes(),
            );
        }
    });
    let parent = tempdir().unwrap();
    let report = qualify_jex_archive(&source, parent.path()).unwrap();
    let semantic = report
        .category(JexQualificationBlockerKind::UnmappedSemanticField)
        .unwrap();
    assert_eq!(semantic.item_count, 12);
    assert_eq!(semantic.finding_count, 12);
    assert_eq!(semantic.samples.len(), 8);
    assert!(listing(parent.path()).is_empty());
}

#[test]
fn preflight_semantic_gate_has_classified_counts_without_opening_spool() {
    let source = archive(|tar| {
        append(
            tar,
            &format!("{CLEAN}.md"),
            exporter_note(CLEAN, "加密", &[("encryption_applied", "1")]).as_bytes(),
        );
        append(
            tar,
            &format!("{BAD_BODY}.md"),
            format!("id: {BAD_BODY}\ntype_: 7\n").as_bytes(),
        );
    });
    let parent = tempdir().unwrap();
    fs::write(parent.path().join("sentinel.bin"), b"keep").unwrap();
    let report = qualify_jex_archive(&source, parent.path()).unwrap();
    assert!(report.semantic_scan_completed);
    assert!(!report.ready_for_current_stage);
    assert_eq!(report.metadata_item_type_counts, vec![(1, 1), (7, 1)]);
    assert_eq!(report.unclassifiable_item_type_counts, vec![(1, 1), (7, 1)]);
    assert_eq!(
        report
            .category(JexQualificationBlockerKind::PreflightEncrypted)
            .unwrap()
            .item_count,
        1
    );
    assert_eq!(
        report
            .category(JexQualificationBlockerKind::PreflightUnsupportedType)
            .unwrap()
            .item_count,
        1
    );
    assert!(
        report
            .category(JexQualificationBlockerKind::PreflightEncrypted)
            .unwrap()
            .samples
            .iter()
            .any(|sample| sample.source_id == CLEAN && sample.source_path == format!("{CLEAN}.md"))
    );
    assert_eq!(
        listing(parent.path()),
        vec![std::ffi::OsString::from("sentinel.bin")]
    );
}

#[test]
fn resource_mime_distribution_and_nonzero_schema_default_are_distinct_from_semantics() {
    let pdf = format!(
        "附件.pdf\n\nid: {RESOURCE}\ntype_: 4\nmime: application/pdf\nfile_extension: pdf\nsize: -1\nocr_driver_id: 1\ncreated_time: 2023-11-14T22:13:21.000Z\nupdated_time: 2023-11-14T22:13:22.000Z\n"
    );
    let gif = format!(
        "动画.gif\n\nid: {GIF_RESOURCE}\ntype_: 4\nmime: image/gif\nfile_extension: gif\ncreated_time: 2023-11-14T22:13:21.000Z\nupdated_time: 2023-11-14T22:13:22.000Z\n"
    );
    let source = archive(|tar| {
        append(tar, &format!("{RESOURCE}.md"), pdf.as_bytes());
        append(tar, &format!("resources/{RESOURCE}.pdf"), b"%PDF-1.4\n");
        append(tar, &format!("{GIF_RESOURCE}.md"), gif.as_bytes());
        append(tar, &format!("resources/{GIF_RESOURCE}.gif"), b"GIF89a");
    });
    let parent = tempdir().unwrap();
    let report = qualify_jex_archive(&source, parent.path()).unwrap();
    assert!(report.semantic_scan_completed);
    assert_eq!(
        report.resource_mime_counts,
        vec![
            ("application/pdf".to_owned(), 1),
            ("image/gif".to_owned(), 1)
        ]
    );
    assert!(report.fields.iter().any(|field| field.item_type == 4
        && field.field == "ocr_driver_id"
        && field.disposition == JexFieldDisposition::SourceAuditOnly
        && field.nondefault_count == 0));
    assert!(
        report
            .category(JexQualificationBlockerKind::StageValidation)
            .unwrap()
            .samples
            .iter()
            .any(|sample| sample.source_id == GIF_RESOURCE)
    );
    assert!(
        report
            .category(JexQualificationBlockerKind::UnmappedSemanticField)
            .is_none()
    );
}

#[test]
fn clean_archive_without_notes_is_not_reported_ready_for_the_strict_stage() {
    // Mutation caught: computing readiness from categories.is_empty alone,
    // while stage_jex_archive rejects every source with zero supported notes.
    let resource = format!(
        "附件.pdf\n\nid: {RESOURCE}\ntype_: 4\nmime: application/pdf\nfile_extension: pdf\ncreated_time: 2023-11-14T22:13:21.000Z\nupdated_time: 2023-11-14T22:13:22.000Z\n"
    );
    let empty = archive(|_| {});
    let resource_only = archive(|tar| {
        append(tar, &format!("{RESOURCE}.md"), resource.as_bytes());
        append(tar, &format!("resources/{RESOURCE}.pdf"), b"%PDF-1.4\n");
    });
    let parent = tempdir().unwrap();
    fs::write(parent.path().join("sentinel.bin"), b"keep").unwrap();
    for source in [&empty, &resource_only] {
        let report = qualify_jex_archive(source, parent.path()).unwrap();
        assert_eq!(report.counts.notes, 0);
        assert!(report.semantic_scan_completed);
        assert!(!report.ready_for_current_stage);
        let blocker = report
            .category(JexQualificationBlockerKind::StageValidation)
            .unwrap();
        assert_eq!(blocker.item_count, 1);
        assert_eq!(blocker.finding_count, 1);
        assert_eq!(blocker.samples[0].source_path, "<archive>");
        assert!(blocker.samples[0].reason.contains("no supported notes"));
        assert_eq!(
            listing(parent.path()),
            vec![std::ffi::OsString::from("sentinel.bin")]
        );
    }
    assert_eq!(
        fs::read(parent.path().join("sentinel.bin")).unwrap(),
        b"keep"
    );
}

fn nonclean_fixture(reverse: bool) -> TempPath {
    let resource_metadata = format!(
        "图.png\n\nid: {RESOURCE}\ntype_: 4\nmime: image/png\nfile_extension: png\nsize: -1\ncreated_time: 2023-11-14T22:13:21.000Z\nupdated_time: 2023-11-14T22:13:22.000Z\n"
    );
    let mut png = vec![0_u8; 10 * 1024 * 1024 + 1];
    png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut entries = vec![
        (
            format!("{ROOT_A}.md"),
            exporter_folder(ROOT_A, "", "项目").into_bytes(),
        ),
        (
            format!("{CHILD}.md"),
            exporter_folder(CHILD, ROOT_A, "子本").into_bytes(),
        ),
        (
            format!("{CLEAN}.md"),
            exporter_note(
                CLEAN,
                &format!("[缺失](:/{MISSING_TARGET})\n\n![图](:/{RESOURCE})"),
                &[("parent_id", ROOT_A)],
            )
            .into_bytes(),
        ),
        (
            format!("{URL_ORDER}.md"),
            exporter_note(
                URL_ORDER,
                "来源",
                &[("source_url", "https://example.org"), ("order", "9")],
            )
            .into_bytes(),
        ),
        (
            format!("{TODO}.md"),
            exporter_note(
                TODO,
                "任务",
                &[("is_todo", "1"), ("todo_due", "1700000000000")],
            )
            .into_bytes(),
        ),
        (format!("{RESOURCE}.md"), resource_metadata.into_bytes()),
        (format!("resources/{RESOURCE}.png"), png),
    ];
    if reverse {
        entries.reverse();
    }
    archive(|tar| {
        for (path, bytes) in entries {
            append(tar, &path, &bytes);
        }
    })
}

#[test]
fn nonclean_jex_still_aggregates_later_semantics_without_creating_a_profile() {
    // Mutation caught: returning the preflight-only report immediately after
    // an unresolved internal link or a resource over the current Store cap.
    let source = nonclean_fixture(false);
    let original = fs::read(&source).unwrap();
    let parent = tempdir().unwrap();
    fs::write(parent.path().join("sentinel.bin"), b"keep").unwrap();
    let report = qualify_jex_archive(&source, parent.path()).unwrap();
    assert_eq!(report.counts.notes, 3);
    assert!(report.semantic_scan_completed);
    assert!(!report.ready_for_current_stage);
    assert_eq!(
        report
            .category(JexQualificationBlockerKind::PreflightIntegrity)
            .unwrap()
            .item_count,
        1
    );
    assert!(
        report
            .category(JexQualificationBlockerKind::PreflightIntegrity)
            .unwrap()
            .samples
            .iter()
            .any(|sample| sample.source_id == CLEAN
                && sample.related_source_id.as_deref() == Some(MISSING_TARGET))
    );
    assert_eq!(
        report
            .category(JexQualificationBlockerKind::PreflightStoreLimit)
            .unwrap()
            .item_count,
        1
    );
    assert_eq!(
        report
            .category(JexQualificationBlockerKind::UnmappedSemanticField)
            .unwrap()
            .item_count,
        2
    );
    assert!(
        report
            .category(JexQualificationBlockerKind::UnmappedSemanticField)
            .unwrap()
            .samples
            .iter()
            .any(|sample| sample.source_id == URL_ORDER
                && sample.field.as_deref() == Some("source_url"))
    );
    assert!(
        report
            .category(JexQualificationBlockerKind::UnmappedSemanticField)
            .unwrap()
            .samples
            .iter()
            .any(|sample| sample.source_id == TODO && sample.field.as_deref() == Some("is_todo"))
    );
    assert!(
        report
            .category(JexQualificationBlockerKind::FolderHierarchy)
            .unwrap()
            .samples
            .iter()
            .any(|sample| sample.source_id == ROOT_A && sample.reason.contains("both own notes"))
    );
    assert!(
        report
            .category(JexQualificationBlockerKind::ExporterFieldGap)
            .is_some()
    );
    assert_eq!(fs::read(&source).unwrap(), original);
    assert_eq!(
        listing(parent.path()),
        vec![std::ffi::OsString::from("sentinel.bin")]
    );
    assert_eq!(
        fs::read(parent.path().join("sentinel.bin")).unwrap(),
        b"keep"
    );
    let reversed = nonclean_fixture(true);
    let reversed_report = qualify_jex_archive(&reversed, parent.path()).unwrap();
    assert_eq!(report, reversed_report);
    assert_eq!(
        listing(parent.path()),
        vec![std::ffi::OsString::from("sentinel.bin")]
    );
}

#[test]
fn unknown_exporter_fields_have_bounded_report_memory() {
    // Mutation caught: retaining every attacker-controlled property name in
    // the report rather than the single unknown-field aggregate bucket.
    let mut note = exporter_note(CLEAN, "正文", &[]);
    for index in 0..500 {
        note.push_str(&format!("\n{}{:04}: value", "x".repeat(4000), index));
    }
    let source = archive(|tar| append(tar, &format!("{CLEAN}.md"), note.as_bytes()));
    let parent = tempdir().unwrap();
    let report = qualify_jex_archive(&source, parent.path()).unwrap();
    assert!(report.semantic_scan_completed);
    let unknown = report
        .category(JexQualificationBlockerKind::UnknownField)
        .unwrap();
    assert_eq!(unknown.item_count, 1);
    assert_eq!(unknown.finding_count, 500);
    assert_eq!(unknown.samples.len(), 8);
    assert!(unknown.samples.iter().all(|sample| sample.field.is_none()));
    assert!(report.fields.len() < 50);
    assert!(
        report
            .fields
            .iter()
            .any(|field| field.field == "<unknown>" && field.occurrence_count == 500)
    );
    assert!(listing(parent.path()).is_empty());
}
