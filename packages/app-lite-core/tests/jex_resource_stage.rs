use std::{
    fs::{self, File},
    io::Cursor,
};

use app_lite_core::{JexPrepareError, JexStageError, LibraryRepository, stage_jex_file};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use tar::{Builder, Header};
use tempfile::{NamedTempFile, TempPath, tempdir};

const MD: &str = "11111111111111111111111111111111";
const HTML: &str = "22222222222222222222222222222222";
const IMAGE_A: &str = "33333333333333333333333333333333";
const IMAGE_B: &str = "44444444444444444444444444444444";
const PDF: &str = "55555555555555555555555555555555";
const UNUSED: &str = "66666666666666666666666666666666";

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

fn note(id: &str, title: &str, body: &str, markup: i64) -> String {
    format!(
        "{title}\n\n{body}\n\nid: {id}\ntype_: 1\nparent_id: \nmarkup_language: {markup}\ncreated_time: 2023-11-14T22:13:21.000Z\nupdated_time: 2023-11-14T22:13:22.000Z\nuser_created_time: 2023-11-14T22:13:23.000Z\nuser_updated_time: 2023-11-14T22:13:24.000Z\n"
    )
}

fn metadata(id: &str, title: &str, mime: &str, extension: &str) -> String {
    format!("{title}\n\nid: {id}\ntype_: 4\nmime: {mime}\nfile_extension: {extension}\n")
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 == 1 { 0xedb8_8320 } else { 0 };
        }
    }
    !crc
}

fn large_png() -> Vec<u8> {
    // Valid 1x1 PNG plus a large ancillary tEXt chunk before IEND.
    let mut png = STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/eZkAAAAASUVORK5CYII=").unwrap();
    let iend = png.split_off(png.len() - 12);
    let mut chunk = b"tEXtComment\0".to_vec();
    chunk.extend(std::iter::repeat_n(b'x', 17_000));
    png.extend_from_slice(&((chunk.len() - 4) as u32).to_be_bytes());
    png.extend_from_slice(&chunk);
    png.extend_from_slice(&crc32(&chunk).to_be_bytes());
    png.extend_from_slice(&iend);
    png
}

fn listing(path: &std::path::Path) -> Vec<std::ffi::OsString> {
    let mut entries = fs::read_dir(path)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

#[test]
fn resource_bearing_markdown_and_html_reopen_with_distinct_ids_repeated_order_and_thumbnail() {
    // Mutations caught: scan-only result, whole-resource omission, hash-based
    // ID collapse, duplicate occurrence collapse, wrong thumbnail, or no reopen.
    let png = large_png();
    assert!(png.len() > 16 * 1024);
    let pdf = b"%PDF-1.4\n1 0 obj<</Type/Catalog>>endobj\n%%EOF\n";
    let md_body = format!("图前![图](:/{IMAGE_A})中![图](:/{IMAGE_A})图后\n\n[附件.pdf](:/{PDF})");
    let html_body = format!("<p>网页前<img src=\":/{IMAGE_B}\" alt=\"图\"/>网页后</p>");
    let md = note(MD, "图文", &md_body, 1);
    let html = note(HTML, "网页", &html_body, 2);
    let a = metadata(IMAGE_A, "图.png", "image/png", "png");
    let b = metadata(IMAGE_B, "副本.png", "image/png", "png");
    let pdf_meta = metadata(PDF, "附件.pdf", "application/pdf", "pdf");
    let unused_meta = metadata(UNUSED, "未引用.txt", "text/plain", "txt");
    let unused_bytes = b"orphan source resource";
    let source = archive(|tar| {
        append(tar, &format!("resources/{PDF}.pdf"), pdf);
        append(tar, &format!("{HTML}.md"), html.as_bytes());
        append(tar, &format!("{IMAGE_B}.md"), b.as_bytes());
        append(tar, &format!("resources/{IMAGE_A}.png"), &png);
        append(tar, &format!("{PDF}.md"), pdf_meta.as_bytes());
        append(tar, &format!("{MD}.md"), md.as_bytes());
        append(tar, &format!("resources/{IMAGE_B}.png"), &png);
        append(tar, &format!("{IMAGE_A}.md"), a.as_bytes());
        append(tar, &format!("resources/{UNUSED}.txt"), unused_bytes);
        append(tar, &format!("{UNUSED}.md"), unused_meta.as_bytes());
    });
    let parent = tempdir().unwrap();
    fs::write(parent.path().join("sentinel.bin"), b"keep").unwrap();
    let staged = stage_jex_file(&source, parent.path()).unwrap();
    let profile = staged.profile_path().to_path_buf();
    let report = staged.report();
    assert_eq!(report.preflight_counts.notes, 2);
    assert_eq!(report.preflight_counts.resource_metadata, 4);
    assert_eq!(report.resources.len(), 4);
    assert_eq!(report.verified_sync_outbox_rows, 0);
    assert!(report.search_index_drained);
    let get_resource = |source_id: &str| {
        report
            .resources
            .iter()
            .find(|r| r.source_id == source_id)
            .unwrap()
    };
    let image_a = get_resource(IMAGE_A);
    let image_b = get_resource(IMAGE_B);
    let pdf_map = get_resource(PDF);
    assert_ne!(image_a.destination_id, image_b.destination_id);
    assert_eq!(image_a.sha256, image_b.sha256);
    assert_eq!(image_a.sha256, format!("{:x}", Sha256::digest(&png)));
    assert_eq!(pdf_map.sha256, format!("{:x}", Sha256::digest(pdf)));

    let db = Connection::open(profile.join("library.sqlite")).unwrap();
    let repo = LibraryRepository::open(profile.join("library.sqlite")).unwrap();
    assert_eq!(
        db.query_row::<String, _, _>("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap(),
        "ok"
    );
    assert_eq!(
        db.query_row::<i64, _, _>("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
            .get(0))
            .unwrap(),
        0
    );
    for (table, count) in [
        ("resources", 4),
        ("resource_blobs", 3),
        ("note_resources", 4),
        ("jex_stage_resource_audit", 4),
        ("sync_outbox", 0),
        ("search_unicode", 2),
        ("search_trigram", 2),
    ] {
        assert_eq!(
            db.query_row::<i64, _, _>(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap(),
            count,
            "{table}"
        );
    }
    for (source_id, raw, expected_title, expected_mime, expected_extension, bytes) in [
        (
            IMAGE_A,
            a.as_bytes(),
            "图.png",
            "image/png",
            "png",
            png.as_slice(),
        ),
        (
            IMAGE_B,
            b.as_bytes(),
            "副本.png",
            "image/png",
            "png",
            png.as_slice(),
        ),
        (
            PDF,
            pdf_meta.as_bytes(),
            "附件.pdf",
            "application/pdf",
            "pdf",
            pdf.as_slice(),
        ),
        (
            UNUSED,
            unused_meta.as_bytes(),
            "未引用.txt",
            "text/plain",
            "txt",
            unused_bytes.as_slice(),
        ),
    ] {
        let mapped = get_resource(source_id);
        assert_eq!(mapped.source_path, format!("{source_id}.md"));
        assert_eq!(
            mapped.physical_path,
            format!("resources/{source_id}.{expected_extension}")
        );
        assert_eq!(
            mapped.raw_metadata_sha256,
            format!("{:x}", Sha256::digest(raw))
        );
        assert_eq!(mapped.byte_count, bytes.len() as u64);
        let (stored, mut file) = repo
            .open_verified_resource_file(&mapped.destination_id)
            .unwrap()
            .unwrap();
        let mut digest = Sha256::new();
        let size = std::io::copy(&mut file, &mut digest).unwrap();
        assert_eq!(
            (
                stored.title.as_str(),
                stored.mime.as_str(),
                stored.file_extension.as_str()
            ),
            (expected_title, expected_mime, expected_extension)
        );
        assert_eq!(size, bytes.len() as u64);
        assert_eq!(
            format!("{:x}", digest.finalize()),
            format!("{:x}", Sha256::digest(bytes))
        );
        let audit: (Vec<u8>, String, String, String, String, i64) = db.query_row(
            "SELECT raw_metadata_bytes,title,mime,file_extension,physical_sha256,physical_size FROM jex_stage_resource_audit WHERE source_id=?1 AND resource_id=?2",
            rusqlite::params![source_id, mapped.destination_id.as_str()],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
        ).unwrap();
        assert_eq!(audit.0, raw);
        assert_eq!(
            (audit.1.as_str(), audit.2.as_str(), audit.3.as_str()),
            (expected_title, expected_mime, expected_extension)
        );
        assert_eq!(audit.4, mapped.sha256);
        assert_eq!(audit.5, bytes.len() as i64);
    }
    let md_note = report.notes.iter().find(|n| n.source_id == MD).unwrap();
    let html_note = report.notes.iter().find(|n| n.source_id == HTML).unwrap();
    let md_loaded = repo.load_note(&md_note.destination_id).unwrap().unwrap();
    let html_loaded = repo.load_note(&html_note.destination_id).unwrap().unwrap();
    assert_eq!(
        md_loaded.body_html,
        format!(
            "<p>图前<img src=\":/{}\" alt=\"图\">中<img src=\":/{}\" alt=\"图\">图后</p><a data-joplin-lite-block-attachment=\"true\" href=\":/{}\" data-resource-id=\"{}\" data-filename=\"附件.pdf\" data-media-type=\"application/pdf\">附件.pdf</a>",
            image_a.destination_id.as_str(),
            image_a.destination_id.as_str(),
            pdf_map.destination_id.as_str(),
            pdf_map.destination_id.as_str()
        )
    );
    assert_eq!(md_loaded.body_text, "图前图中图图后\n附件.pdf");
    assert_eq!(
        md_loaded.resource_ids,
        vec![
            image_a.destination_id.clone(),
            image_a.destination_id.clone(),
            pdf_map.destination_id.clone()
        ]
    );
    assert_eq!(
        html_loaded.body_html,
        format!(
            "<p>网页前<img src=\":/{}\" alt=\"图\">网页后</p>",
            image_b.destination_id.as_str()
        )
    );
    assert_eq!(html_loaded.body_text, "网页前图网页后");
    assert_eq!(
        html_loaded.resource_ids,
        vec![image_b.destination_id.clone()]
    );
    for (note_id, thumbnail) in [
        (md_loaded.id.as_str(), image_a.destination_id.as_str()),
        (html_loaded.id.as_str(), image_b.destination_id.as_str()),
    ] {
        let selected: String = db
            .query_row(
                "SELECT selected_thumbnail_id FROM notes WHERE id=?1",
                [note_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(selected, thumbnail);
    }
    for (note_id, ids) in [
        (
            md_loaded.id.as_str(),
            vec![
                image_a.destination_id.as_str(),
                image_a.destination_id.as_str(),
                pdf_map.destination_id.as_str(),
            ],
        ),
        (
            html_loaded.id.as_str(),
            vec![image_b.destination_id.as_str()],
        ),
    ] {
        let mut statement = db
            .prepare("SELECT position,resource_id,is_associated FROM note_resources WHERE note_id=?1 ORDER BY position")
            .unwrap();
        let persisted = statement
            .query_map([note_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let expected = ids
            .into_iter()
            .enumerate()
            .map(|(position, id)| (position as i64, id.to_owned(), 1))
            .collect::<Vec<_>>();
        assert_eq!(persisted, expected);
    }
    for note in [&md_loaded, &html_loaded] {
        for table in ["search_unicode", "search_trigram"] {
            let body: String = db
                .query_row(
                    &format!("SELECT body FROM {table} WHERE note_id=?1"),
                    [note.id.as_str()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(body, note.body_text);
        }
    }
    assert!(!repo.has_pending_search_jobs().unwrap());
    drop(repo);
    drop(db);
    drop(staged);
    assert!(!profile.exists());
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
fn missing_file_and_ambiguous_name_refuse_without_touching_sibling() {
    // Mutations caught: inventing a resource name or overlooking a source
    // file that the accepted C2c-1 spool refuses before staging.
    let parent = tempdir().unwrap();
    fs::write(parent.path().join("sentinel.bin"), b"keep").unwrap();
    let body = format!("图![图](:/{IMAGE_A})");
    let good_note = note(MD, "笔记", &body, 1);
    let png = large_png();
    let missing = archive(|tar| {
        append(tar, &format!("{MD}.md"), good_note.as_bytes());
        append(
            tar,
            &format!("{IMAGE_A}.md"),
            metadata(IMAGE_A, "图.png", "image/png", "png").as_bytes(),
        );
    });
    assert!(matches!(
        stage_jex_file(&missing, parent.path()),
        Err(JexStageError::Prepare(
            JexPrepareError::PreflightBlocked { .. }
        ))
    ));
    assert_eq!(
        listing(parent.path()),
        vec![std::ffi::OsString::from("sentinel.bin")]
    );
    let mismatched = archive(|tar| {
        append(tar, &format!("{MD}.md"), good_note.as_bytes());
        append(
            tar,
            &format!("{IMAGE_A}.md"),
            metadata(IMAGE_A, "图.pdf", "image/png", "png").as_bytes(),
        );
        append(tar, &format!("resources/{IMAGE_A}.png"), &png);
    });
    assert!(matches!(
        stage_jex_file(&mismatched, parent.path()),
        Err(JexStageError::UnsupportedResource { .. })
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
fn failed_second_note_after_resource_and_first_note_cleans_owned_profile() {
    // Mutation caught: leaking the staged resource or first note when the
    // later Markdown converter rejects a fidelity-risk structure.
    let png = large_png();
    let first = note(MD, "先成功", &format!("图![图](:/{IMAGE_A})"), 1);
    let second = note(HTML, "后失败", "|A|B|\n|-|-|\n|1|2|", 1);
    let source = archive(|tar| {
        append(tar, &format!("{MD}.md"), first.as_bytes());
        append(tar, &format!("{HTML}.md"), second.as_bytes());
        append(
            tar,
            &format!("{IMAGE_A}.md"),
            metadata(IMAGE_A, "图.png", "image/png", "png").as_bytes(),
        );
        append(tar, &format!("resources/{IMAGE_A}.png"), &png);
    });
    let parent = tempdir().unwrap();
    fs::write(parent.path().join("sentinel.bin"), b"keep").unwrap();
    assert!(matches!(
        stage_jex_file(&source, parent.path()),
        Err(JexStageError::Fidelity(_))
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
