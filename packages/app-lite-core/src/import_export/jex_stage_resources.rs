//! C2c-3b resource intake and reopen checks for the owned JEX library stage.

use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
};

use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};

use crate::LibraryRepository;

use super::super::{
    JexPreparedSource, JexRawSourceItem, JexScannedResource, JexVerifiedResource, parse_item,
};
use super::{JexStageError, JexStagedResource};

struct ResourceFields {
    title: String,
    mime: String,
    extension: String,
}

fn unsupported(source_id: &str, reason: &'static str) -> JexStageError {
    JexStageError::UnsupportedResource {
        source_id: source_id.to_owned(),
        reason,
    }
}

fn parse_metadata(raw: &JexRawSourceItem) -> Result<(String, String, String), JexStageError> {
    let content = std::str::from_utf8(&raw.raw_bytes)
        .map_err(|_| unsupported(&raw.source_id, "resource metadata is not UTF-8"))?;
    let item = parse_item(&raw.archive_path, content)
        .map_err(|_| unsupported(&raw.source_id, "resource metadata is malformed"))?;
    if item.item_type != 4 || !item.id.eq_ignore_ascii_case(&raw.source_id) {
        return Err(unsupported(
            &raw.source_id,
            "resource metadata identity changed",
        ));
    }
    let separator = if content.contains("\r\n") {
        "\r\n\r\n"
    } else {
        "\n\n"
    };
    let title = content
        .split_once(separator)
        .map(|(title, _)| title.to_owned())
        .unwrap_or_default();
    let mime = item.properties.get("mime").cloned().unwrap_or_default();
    let extension = item
        .properties
        .get("file_extension")
        .cloned()
        .unwrap_or_default();
    Ok((title, mime, extension))
}

/// Make real exporter metadata storable without rejecting the library.
/// Pure in (raw metadata, byte count, first bytes) so verification can
/// replay it. Every change is reported.
fn normalize_metadata(
    raw: &JexRawSourceItem,
    byte_count: u64,
    prefix: &[u8],
) -> Result<(ResourceFields, Vec<&'static str>), JexStageError> {
    let (raw_title, raw_mime, raw_extension) = parse_metadata(raw)?;
    let mut changes = Vec::new();

    let mut title: String = raw_title
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .trim()
        .to_owned();
    if title.len() > 255 {
        let mut end = 255;
        while !title.is_char_boundary(end) {
            end -= 1;
        }
        title.truncate(end);
        title = title.trim_end().to_owned();
    }
    if title.is_empty() {
        title = format!("resource-{}", &raw.source_id[..raw.source_id.len().min(8)]);
    }
    if title != raw_title {
        changes.push("title");
    }

    let mut mime = raw_mime.trim().to_ascii_lowercase();
    if mime == "image/jpg" {
        mime = "image/jpeg".into();
    }
    if !valid_mime(&mime) {
        mime = "application/octet-stream".into();
    }
    // For formats with a reliable signature the bytes decide: a mislabeled
    // file gets its real type, unrecognizable bytes become a generic
    // attachment so they are never decoded as an image.
    let verifiable = matches!(
        mime.as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "application/pdf"
    );
    match sniff_mime(prefix) {
        Some(sniffed) if sniffed != mime && (verifiable || mime == "application/octet-stream") => {
            mime = sniffed.to_owned();
        }
        None if verifiable => mime = "application/octet-stream".into(),
        _ => {}
    }
    if mime.starts_with("image/") && byte_count > crate::MAX_IMAGE_BYTES as u64 {
        // Too large to decode inline; keep it as an ordinary attachment.
        mime = "application/octet-stream".into();
        changes.push("oversized image stored as attachment");
    }
    if mime != raw_mime {
        changes.push("mime");
    }

    let from_title = title
        .rsplit_once('.')
        .map(|(_, suffix)| suffix.to_ascii_lowercase());
    let extension = [
        Some(raw_extension.trim().to_ascii_lowercase()),
        from_title,
        extension_for_mime(&mime).map(str::to_owned),
    ]
    .into_iter()
    .flatten()
    .find(|candidate| valid_extension(candidate))
    .unwrap_or_else(|| "bin".into());
    if extension != raw_extension {
        changes.push("extension");
    }
    Ok((
        ResourceFields {
            title,
            mime,
            extension,
        },
        changes,
    ))
}

fn sniff_mime(prefix: &[u8]) -> Option<&'static str> {
    if prefix.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if prefix.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if prefix.starts_with(b"GIF87a") || prefix.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if prefix.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else {
        None
    }
}

fn extension_for_mime(mime: &str) -> Option<&'static str> {
    Some(match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        _ => return None,
    })
}

// Mirrors `resource.rs` store validation so normalized fields always store.
fn valid_mime(mime: &str) -> bool {
    let Some((kind, subtype)) = mime.split_once('/') else {
        return false;
    };
    !kind.is_empty()
        && !subtype.is_empty()
        && mime.len() <= 127
        && !subtype.contains('/')
        && mime.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(
                    byte,
                    b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-' | b'/'
                )
        })
}

fn valid_extension(extension: &str) -> bool {
    !extension.is_empty()
        && extension.len() <= 16
        && extension
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

pub(super) fn validate_source_item(raw: &JexRawSourceItem) -> Result<(), JexStageError> {
    parse_metadata(raw).map(|_| ())
}

fn read_prefix(file: &mut File) -> io::Result<Vec<u8>> {
    let mut prefix = [0_u8; 8];
    let mut filled = 0;
    while filled < prefix.len() {
        let count = file.read(&mut prefix[filled..])?;
        if count == 0 {
            break;
        }
        filled += count;
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(prefix[..filled].to_vec())
}

pub(super) fn verify_signature_prefix(
    source_id: &str,
    mime: &str,
    prefix: &[u8],
) -> Result<(), JexStageError> {
    let valid = match mime {
        "image/png" => prefix.len() >= 8 && prefix[..8] == *b"\x89PNG\r\n\x1a\n",
        "image/jpeg" => prefix.len() >= 3 && prefix[..3] == [0xff, 0xd8, 0xff],
        "application/pdf" => prefix.len() >= 5 && prefix[..5] == *b"%PDF-",
        _ => true,
    };
    if valid {
        Ok(())
    } else {
        Err(unsupported(
            source_id,
            "physical resource signature does not match MIME",
        ))
    }
}

pub(super) fn import_one(
    prepared: &JexPreparedSource,
    source: &JexScannedResource,
    repo: &LibraryRepository,
    audit: &Connection,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<(JexStagedResource, JexVerifiedResource, Vec<&'static str>), JexStageError> {
    let raw = prepared.raw_item(&source.source_id)?.ok_or_else(|| {
        JexStageError::Verification("verified resource metadata disappeared".into())
    })?;
    let (physical, mut file) = prepared
        .open_verified_resource(&source.source_id)?
        .ok_or_else(|| JexStageError::Verification("verified resource file disappeared".into()))?;
    if physical.archive_path != source.archive_path
        || physical.byte_count != source.byte_count
        || physical.sha256 != source.sha256
    {
        return Err(JexStageError::Verification(
            "resource source evidence differs".into(),
        ));
    }
    let prefix = read_prefix(&mut file)?;
    let (fields, changes) = normalize_metadata(&raw, source.byte_count, &prefix)?;
    let size = usize::try_from(source.byte_count)
        .map_err(|_| unsupported(&source.source_id, "resource exceeds addressable size"))?;
    let imported = repo.import_resource_reader(
        super::super::cancellable_read::CancellableRead::new(file, cancel),
        size,
        &fields.title,
        &fields.mime,
        &fields.extension,
    );
    super::check_cancel(cancel)?;
    let destination_id = imported?;
    let stored = repo
        .resource_metadata(&destination_id)?
        .ok_or_else(|| JexStageError::Verification("stored resource metadata missing".into()))?;
    if stored.sha256.as_str() != source.sha256 || stored.size != source.byte_count as i64 {
        return Err(JexStageError::Verification(
            "stored resource hash or size differs".into(),
        ));
    }
    audit.execute("INSERT INTO jex_stage_resource_audit
        (source_id,source_path,physical_path,resource_id,raw_metadata_bytes,raw_metadata_sha256,title,mime,file_extension,physical_sha256,physical_size)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)", params![
            source.source_id, raw.archive_path, source.archive_path, destination_id.as_str(), raw.raw_bytes,
            raw.raw_sha256, fields.title, fields.mime, fields.extension, source.sha256, source.byte_count as i64,
        ])?;
    let mapped = JexStagedResource {
        source_id: source.source_id.clone(),
        source_path: raw.archive_path,
        physical_path: source.archive_path.clone(),
        destination_id: destination_id.clone(),
        title: fields.title.clone(),
        mime: fields.mime.clone(),
        file_extension: fields.extension.clone(),
        raw_metadata_sha256: raw.raw_sha256,
        sha256: source.sha256.clone(),
        byte_count: source.byte_count,
    };
    Ok((
        mapped,
        JexVerifiedResource {
            destination_id,
            mime: fields.mime,
            filename: fields.title,
        },
        changes,
    ))
}

pub(super) fn verify_one(
    repo: &LibraryRepository,
    db: &Connection,
    mapped: &JexStagedResource,
) -> Result<(), JexStageError> {
    let (raw, raw_sha, source_path, physical_path, title, mime, extension, sha, size): (Vec<u8>, String, String, String, String, String, String, String, i64) = db.query_row(
        "SELECT raw_metadata_bytes,raw_metadata_sha256,source_path,physical_path,title,mime,file_extension,physical_sha256,physical_size
         FROM jex_stage_resource_audit WHERE source_id=?1 AND resource_id=?2",
        params![mapped.source_id, mapped.destination_id.as_str()],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?)),
    )?;
    if format!("{:x}", Sha256::digest(&raw)) != raw_sha
        || raw_sha != mapped.raw_metadata_sha256
        || source_path != mapped.source_path
        || physical_path != mapped.physical_path
        || title != mapped.title
        || mime != mapped.mime
        || extension != mapped.file_extension
        || sha != mapped.sha256
        || size != mapped.byte_count as i64
    {
        return Err(JexStageError::Verification(
            "resource source audit differs".into(),
        ));
    }
    let raw_item = JexRawSourceItem {
        archive_path: source_path,
        source_id: mapped.source_id.clone(),
        item_type: 4,
        byte_count: raw.len() as u64,
        raw_sha256: raw_sha,
        canonical_note_body_sha256: None,
        raw_bytes: raw,
    };
    let (stored, mut file) = repo
        .open_verified_resource_file(&mapped.destination_id)?
        .ok_or_else(|| JexStageError::Verification("reopened resource disappeared".into()))?;
    let prefix = read_prefix(&mut file)?;
    let (parsed, _) = normalize_metadata(&raw_item, size as u64, &prefix)?;
    if parsed.title != title || parsed.mime != mime || parsed.extension != extension {
        return Err(JexStageError::Verification(
            "resource metadata replay differs".into(),
        ));
    }
    let mut digest = Sha256::new();
    let copied = io::copy(&mut file, &mut digest)?;
    if stored.title != title
        || stored.mime != mime
        || stored.file_extension != extension
        || stored.sha256.as_str() != sha
        || stored.size != size
        || copied != mapped.byte_count
        || format!("{:x}", digest.finalize()) != sha
    {
        return Err(JexStageError::Verification(
            "reopened resource blob or metadata differs".into(),
        ));
    }
    Ok(())
}
