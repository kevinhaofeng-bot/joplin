use std::{
    fs::File,
    io::{self, Cursor, Read, Write},
};

use app_lite_core::{JexScanError, MAX_RESOURCE_BYTES, scan_jex_archive};
use sha2::{Digest, Sha256};
use tar::{Builder, Header};
use tempfile::{NamedTempFile, TempPath};

const NOTE: &str = "11111111111111111111111111111111";
const FOLDER: &str = "22222222222222222222222222222222";
const RESOURCE: &str = "33333333333333333333333333333333";
const TAG: &str = "44444444444444444444444444444444";
const NOTE_TAG: &str = "55555555555555555555555555555555";

fn item(id: &str, item_type: u8, extra: &str) -> String {
    format!("id: {id}\ntype_: {item_type}\n{extra}")
}

fn append_bytes(builder: &mut Builder<File>, path: &str, bytes: &[u8]) {
    let mut header = Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, path, Cursor::new(bytes))
        .unwrap();
}

fn write_archive(entries: impl FnOnce(&mut Builder<File>)) -> TempPath {
    let archive = NamedTempFile::new().unwrap();
    let path = archive.into_temp_path();
    let file = File::create(&path).unwrap();
    let mut builder = Builder::new(file);
    entries(&mut builder);
    builder.finish().unwrap();
    path
}

fn write_unsafe_parent_path_archive() -> TempPath {
    let archive = NamedTempFile::new().unwrap();
    let path = archive.into_temp_path();
    let mut file = File::create(&path).unwrap();
    let contents = item(NOTE, 1, "");
    let mut header = [0u8; 512];
    header[.."../escape.md".len()].copy_from_slice(b"../escape.md");
    write_tar_octal(&mut header[100..108], 0o644);
    write_tar_octal(&mut header[124..136], contents.len() as u64);
    header[148..156].fill(b' ');
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    let checksum: u64 = header.iter().map(|byte| *byte as u64).sum();
    write_tar_octal(&mut header[148..156], checksum);
    file.write_all(&header).unwrap();
    file.write_all(contents.as_bytes()).unwrap();
    let padding = (512 - contents.len() % 512) % 512;
    file.write_all(&vec![0; padding]).unwrap();
    file.write_all(&[0; 1024]).unwrap();
    path
}

fn write_oversized_gnu_longname_header() -> TempPath {
    let archive = NamedTempFile::new().unwrap();
    let path = archive.into_temp_path();
    let mut file = File::create(&path).unwrap();
    let mut header = [0u8; 512];
    header[..14].copy_from_slice(b"././@LongLink\0");
    write_tar_octal(&mut header[100..108], 0o644);
    write_tar_octal(&mut header[124..136], 16 * 1024 * 1024);
    header[148..156].fill(b' ');
    header[156] = b'L';
    header[257..263].copy_from_slice(b"ustar\0");
    let checksum: u64 = header.iter().map(|byte| *byte as u64).sum();
    write_tar_octal(&mut header[148..156], checksum);
    file.write_all(&header).unwrap();
    file.write_all(&[0; 1024]).unwrap();
    path
}

fn write_tar_octal(field: &mut [u8], value: u64) {
    let encoded = format!("{:0width$o}\0", value, width = field.len() - 1);
    field.copy_from_slice(encoded.as_bytes());
}

#[test]
fn scans_complete_jex_without_reading_resource_into_item_memory() {
    let resource_bytes = b"a small resource";
    let archive = write_archive(|builder| {
        append_bytes(
            builder,
            &format!("{NOTE}.md"),
            format!(
                "Meeting\n\nBody\n\n{}",
                item(NOTE, 1, &format!("parent_id: {FOLDER}"))
            )
            .as_bytes(),
        );
        append_bytes(
            builder,
            &format!("{FOLDER}.md"),
            format!("Work\n\n{}", item(FOLDER, 2, "")).as_bytes(),
        );
        append_bytes(
            builder,
            &format!("{RESOURCE}.md"),
            format!(
                "receipt.png\n\n{}",
                item(RESOURCE, 4, "mime: image/png\nfile_extension: png")
            )
            .as_bytes(),
        );
        append_bytes(
            builder,
            &format!("{TAG}.md"),
            format!("Receipts\n\n{}", item(TAG, 5, "")).as_bytes(),
        );
        // Joplin's NoteTag serialization contains properties only: no title/body separator.
        append_bytes(
            builder,
            &format!("{NOTE_TAG}.md"),
            item(NOTE_TAG, 6, &format!("note_id: {NOTE}\ntag_id: {TAG}")).as_bytes(),
        );
        append_bytes(
            builder,
            &format!("resources/{RESOURCE}.png"),
            resource_bytes,
        );
    });

    let report = scan_jex_archive(&archive).unwrap();

    assert!(report.is_clean());
    assert_eq!(report.counts.notes, 1);
    assert_eq!(report.counts.folders, 1);
    assert_eq!(report.counts.resource_metadata, 1);
    assert_eq!(report.counts.tags, 1);
    assert_eq!(report.counts.note_tag_relations, 1);
    assert_eq!(report.counts.physical_resource_files, 1);
    assert_eq!(report.physical_resource_files[0].source_id, RESOURCE);
    assert_eq!(report.source_ids.notes, vec![NOTE]);
    assert_eq!(report.source_ids.note_tag_relations, vec![NOTE_TAG]);
    assert_eq!(
        report.resources[0].archive_path,
        format!("resources/{RESOURCE}.png")
    );
    assert_eq!(report.resources[0].byte_count, resource_bytes.len() as u64);
    assert_eq!(
        report.resources[0].sha256,
        format!("{:x}", Sha256::digest(resource_bytes))
    );
}

#[test]
fn reports_missing_resource_with_extensionless_joplin_filename() {
    let archive = write_archive(|builder| {
        append_bytes(
            builder,
            &format!("{RESOURCE}.md"),
            item(RESOURCE, 4, "mime: application/x-unknown").as_bytes(),
        );
    });

    let report = scan_jex_archive(&archive).unwrap();

    assert!(!report.is_clean());
    assert_eq!(
        report.missing_resource_files,
        vec![format!("resources/{RESOURCE}")]
    );
}

#[test]
fn uses_joplins_jpeg_fallback_not_a_nearby_mime_tables_first_suffix() {
    let archive = write_archive(|builder| {
        append_bytes(
            builder,
            &format!("{RESOURCE}.md"),
            item(RESOURCE, 4, "mime: image/jpeg").as_bytes(),
        );
        append_bytes(builder, &format!("resources/{RESOURCE}.jpg"), b"jpeg");
    });

    let report = scan_jex_archive(&archive).unwrap();

    assert!(report.is_clean());
    assert_eq!(
        report.resources[0].archive_path,
        format!("resources/{RESOURCE}.jpg")
    );
}

#[test]
fn rejects_duplicate_archive_paths() {
    let archive = write_archive(|builder| {
        append_bytes(builder, &format!("{NOTE}.md"), item(NOTE, 1, "").as_bytes());
        append_bytes(builder, &format!("{NOTE}.md"), item(NOTE, 1, "").as_bytes());
    });

    assert!(matches!(
        scan_jex_archive(&archive),
        Err(JexScanError::DuplicateArchivePath(path)) if path == format!("{NOTE}.md")
    ));
}

#[test]
fn reports_duplicate_source_ids_deterministically() {
    let duplicate_lower = "abcdefabcdefabcdefabcdefabcdefab";
    let duplicate_upper = duplicate_lower.to_ascii_uppercase();
    let archive = write_archive(|builder| {
        append_bytes(
            builder,
            &format!("{duplicate_lower}.md"),
            item(duplicate_lower, 1, "").as_bytes(),
        );
        // Joplin accepts upper-case hexadecimal IDs, so the two file names are
        // distinct tar paths but name the same source item identity.
        append_bytes(
            builder,
            &format!("{duplicate_upper}.md"),
            item(&duplicate_upper, 1, "").as_bytes(),
        );
    });

    let report = scan_jex_archive(&archive).unwrap();

    assert_eq!(report.duplicate_item_ids, vec![duplicate_upper]);
    assert!(!report.is_clean());
}

#[test]
fn accepts_the_safe_resources_directory_header() {
    let archive = write_archive(|builder| {
        builder.append_dir("resources", ".").unwrap();
        append_bytes(builder, &format!("{NOTE}.md"), item(NOTE, 1, "").as_bytes());
    });

    let report = scan_jex_archive(&archive).unwrap();

    assert_eq!(report.counts.notes, 1);
}

#[test]
fn rejects_parent_directory_tar_path() {
    let archive = write_unsafe_parent_path_archive();

    assert!(matches!(
        scan_jex_archive(&archive),
        Err(JexScanError::UnsafeArchivePath(path)) if path == "../escape.md"
    ));
}

#[test]
fn records_unsupported_and_encrypted_items_without_counting_them_as_supported() {
    let unsupported = "66666666666666666666666666666666";
    let encrypted = "77777777777777777777777777777777";
    let archive = write_archive(|builder| {
        append_bytes(
            builder,
            &format!("{unsupported}.md"),
            item(unsupported, 99, "").as_bytes(),
        );
        append_bytes(
            builder,
            &format!("{encrypted}.md"),
            item(encrypted, 1, "encryption_applied: 1").as_bytes(),
        );
    });

    let report = scan_jex_archive(&archive).unwrap();

    assert_eq!(report.unsupported_items.len(), 1);
    assert_eq!(report.unsupported_items[0].source_id, unsupported);
    assert_eq!(report.encrypted_item_ids, vec![encrypted]);
    assert_eq!(report.counts.notes, 0);
}

#[test]
fn reports_note_tag_whose_note_or_tag_is_absent_from_archive() {
    let archive = write_archive(|builder| {
        append_bytes(
            builder,
            &format!("{NOTE_TAG}.md"),
            item(NOTE_TAG, 6, &format!("note_id: {NOTE}\ntag_id: {TAG}")).as_bytes(),
        );
    });

    let report = scan_jex_archive(&archive).unwrap();

    assert_eq!(report.orphan_note_tag_relations, vec![NOTE_TAG]);
    assert!(!report.is_clean());
}

#[test]
fn reports_physical_resource_without_metadata_as_orphan() {
    let archive = write_archive(|builder| {
        append_bytes(builder, &format!("resources/{RESOURCE}.bin"), b"orphan");
    });

    let report = scan_jex_archive(&archive).unwrap();

    assert_eq!(
        report.orphan_physical_resource_files,
        vec![format!("resources/{RESOURCE}.bin")]
    );
    assert!(!report.is_clean());
}

#[test]
fn rejects_longname_extension_before_reading_its_declared_payload() {
    let archive = write_oversized_gnu_longname_header();
    let error = scan_jex_archive(&archive).unwrap_err();

    assert!(
        matches!(
            error,
            JexScanError::UnsafeArchiveEntry { ref kind, .. } if kind == "gnu-longname"
        ),
        "{error:?}"
    );
}

#[test]
fn streams_large_resource_and_reports_its_digest() {
    const BYTE_COUNT: usize = 9 * 1024 * 1024;
    let archive = write_archive(|builder| {
        append_bytes(
            builder,
            &format!("{RESOURCE}.md"),
            item(RESOURCE, 4, "file_extension: bin").as_bytes(),
        );
        let mut header = Header::new_gnu();
        header.set_size(BYTE_COUNT as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(
                &mut header,
                format!("resources/{RESOURCE}.bin"),
                RepeatingReader {
                    remaining: BYTE_COUNT,
                },
            )
            .unwrap();
    });

    let report = scan_jex_archive(&archive).unwrap();
    let mut hasher = Sha256::new();
    let chunk = [0x5a; 64 * 1024];
    for _ in 0..BYTE_COUNT / chunk.len() {
        hasher.update(chunk);
    }

    assert_eq!(report.resources[0].byte_count, BYTE_COUNT as u64);
    assert_eq!(
        report.resources[0].sha256,
        format!("{:x}", hasher.finalize())
    );
}

#[test]
fn records_resource_above_current_store_limit_as_compatibility_blocker() {
    let byte_count = MAX_RESOURCE_BYTES as usize + 1;
    let archive = write_archive(|builder| {
        let mut header = Header::new_gnu();
        header.set_size(byte_count as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(
                &mut header,
                format!("resources/{RESOURCE}.bin"),
                RepeatingReader {
                    remaining: byte_count,
                },
            )
            .unwrap();
    });

    let report = scan_jex_archive(&archive).unwrap();

    assert_eq!(report.store_compatibility_blockers.len(), 1);
    assert_eq!(
        report.store_compatibility_blockers[0].archive_path,
        format!("resources/{RESOURCE}.bin")
    );
    assert!(!report.is_clean());
}

#[test]
fn records_image_above_current_image_store_limit_as_compatibility_blocker() {
    let byte_count = 10 * 1024 * 1024 + 1;
    let archive = write_archive(|builder| {
        append_bytes(
            builder,
            &format!("{RESOURCE}.md"),
            item(RESOURCE, 4, "mime: image/png\nfile_extension: png").as_bytes(),
        );
        let mut header = Header::new_gnu();
        header.set_size(byte_count as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(
                &mut header,
                format!("resources/{RESOURCE}.png"),
                RepeatingReader {
                    remaining: byte_count,
                },
            )
            .unwrap();
    });

    let report = scan_jex_archive(&archive).unwrap();

    assert_eq!(report.store_compatibility_blockers.len(), 1);
    assert_eq!(
        report.store_compatibility_blockers[0].byte_count,
        byte_count as u64
    );
    assert!(
        report.store_compatibility_blockers[0]
            .reason
            .contains("10485760")
    );
}

#[test]
fn records_zero_byte_resource_as_current_store_compatibility_blocker() {
    let archive = write_archive(|builder| {
        append_bytes(
            builder,
            &format!("{RESOURCE}.md"),
            item(RESOURCE, 4, "file_extension: bin").as_bytes(),
        );
        append_bytes(builder, &format!("resources/{RESOURCE}.bin"), b"");
    });

    let report = scan_jex_archive(&archive).unwrap();

    assert_eq!(report.store_compatibility_blockers.len(), 1);
    assert!(
        report.store_compatibility_blockers[0]
            .reason
            .contains("zero-byte")
    );
    assert!(!report.is_clean());
}

struct RepeatingReader {
    remaining: usize,
}

impl Read for RepeatingReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = buffer.len().min(self.remaining);
        buffer[..count].fill(0x5a);
        self.remaining -= count;
        Ok(count)
    }
}
