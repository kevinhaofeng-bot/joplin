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

fn parse_metadata(raw: &JexRawSourceItem) -> Result<ResourceFields, JexStageError> {
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
    let (title, _) = content
        .split_once(separator)
        .ok_or_else(|| unsupported(&raw.source_id, "resource title is missing"))?;
    if title.is_empty()
        || title != title.trim()
        || title.len() > 255
        || title.contains(['/', '\\', ':', '\n', '\r'])
        || title
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err(unsupported(
            &raw.source_id,
            "resource title is not a safe filename",
        ));
    }
    let mime = item
        .properties
        .get("mime")
        .ok_or_else(|| unsupported(&raw.source_id, "resource MIME is missing"))?;
    let extension = item
        .properties
        .get("file_extension")
        .ok_or_else(|| unsupported(&raw.source_id, "resource extension is missing"))?;
    let expected_extensions: &[&str] = match mime.as_str() {
        "image/png" => &["png"],
        "image/jpeg" => &["jpg", "jpeg"],
        "application/pdf" => &["pdf"],
        "text/plain" => &["txt"],
        "application/octet-stream" => &["bin"],
        _ => {
            return Err(unsupported(
                &raw.source_id,
                "resource MIME is outside the verified 3b subset",
            ));
        }
    };
    if !expected_extensions.contains(&extension.as_str())
        || !title
            .rsplit_once('.')
            .is_some_and(|(_, suffix)| suffix.eq_ignore_ascii_case(extension))
    {
        return Err(unsupported(
            &raw.source_id,
            "resource title, MIME and extension disagree",
        ));
    }
    Ok(ResourceFields {
        title: title.to_owned(),
        mime: mime.clone(),
        extension: extension.clone(),
    })
}

pub(super) fn validate_source_item(raw: &JexRawSourceItem) -> Result<(), JexStageError> {
    parse_metadata(raw).map(|_| ())
}

fn verify_signature(source_id: &str, mime: &str, file: &mut File) -> Result<(), JexStageError> {
    let mut prefix = [0_u8; 8];
    let count = file.read(&mut prefix)?;
    let valid = match mime {
        "image/png" => count >= 8 && prefix == *b"\x89PNG\r\n\x1a\n",
        "image/jpeg" => count >= 3 && prefix[..3] == [0xff, 0xd8, 0xff],
        "application/pdf" => count >= 5 && prefix[..5] == *b"%PDF-",
        _ => true,
    };
    file.seek(SeekFrom::Start(0))?;
    if valid {
        Ok(())
    } else {
        Err(unsupported(
            source_id,
            "physical resource signature does not match MIME",
        ))
    }
}

pub(super) fn validate_source_resource(
    prepared: &JexPreparedSource,
    source: &JexScannedResource,
) -> Result<(), JexStageError> {
    let raw = prepared.raw_item(&source.source_id)?.ok_or_else(|| {
        JexStageError::Verification("verified resource metadata disappeared".into())
    })?;
    let fields = parse_metadata(&raw)?;
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
    verify_signature(&source.source_id, &fields.mime, &mut file)
}

pub(super) fn import_one(
    prepared: &JexPreparedSource,
    source: &JexScannedResource,
    repo: &LibraryRepository,
    audit: &Connection,
) -> Result<(JexStagedResource, JexVerifiedResource), JexStageError> {
    let raw = prepared.raw_item(&source.source_id)?.ok_or_else(|| {
        JexStageError::Verification("verified resource metadata disappeared".into())
    })?;
    let fields = parse_metadata(&raw)?;
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
    verify_signature(&source.source_id, &fields.mime, &mut file)?;
    let size = usize::try_from(source.byte_count)
        .map_err(|_| unsupported(&source.source_id, "resource exceeds addressable size"))?;
    let destination_id =
        repo.import_resource_reader(file, size, &fields.title, &fields.mime, &fields.extension)?;
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
    let parsed = parse_metadata(&raw_item)?;
    if parsed.title != title || parsed.mime != mime || parsed.extension != extension {
        return Err(JexStageError::Verification(
            "resource metadata replay differs".into(),
        ));
    }
    let (stored, mut file) = repo
        .open_verified_resource_file(&mapped.destination_id)?
        .ok_or_else(|| JexStageError::Verification("reopened resource disappeared".into()))?;
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
