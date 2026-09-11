use crate::{CanonicalDocument, NoteId, ResourceId};
use serde::Deserialize;
use thiserror::Error;

/// Version one is the complete-document checkpoint wire format written by the
/// Task 4 v4 implementation. It had no writer token: v5 derives one from the
/// opaque journal row ID while migrating it.
pub const LEGACY_JOURNAL_SCHEMA_VERSION: u8 = 1;

// These are wire guards, not product note limits. They bound one hostile or
// corrupt crash checkpoint before it can make migration allocate unbounded
// memory. Normal documents remain governed by CanonicalDocument's parser
// limits and durable note storage.
const MAX_LEGACY_TITLE_BYTES: usize = 256 * 1024;
const MAX_LEGACY_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_LEGACY_RESOURCE_REFERENCES: usize = 100_000;
const MAX_LEGACY_WRITER_TOKEN_BYTES: usize = 96;

#[derive(Debug, Error)]
pub enum LegacyJournalPayloadError {
    #[error("legacy journal is not a complete version-one wire object")]
    Decode(#[from] serde_json::Error),
    #[error("legacy journal has an unsupported version")]
    Version,
    #[error("legacy journal note identity does not match its SQL row")]
    NoteIdentity,
    #[error("legacy journal expected revision is outside its valid range")]
    ExpectedRevision,
    #[error("legacy journal generation does not match its SQL row")]
    Generation,
    #[error("legacy journal title is outside its wire range")]
    TitleRange,
    #[error("legacy journal body is outside its wire range")]
    BodyRange,
    #[error("legacy journal body is not canonical HTML")]
    CanonicalBody,
    #[error("legacy journal resource list is outside its wire range")]
    ResourceCount,
    #[error("legacy journal contains an invalid resource ID")]
    ResourceId,
    #[error("legacy journal resources are not exactly this note's relation")]
    ResourceRelation,
    #[error("legacy journal body references resources outside its declared relation")]
    ResourceReferences,
    #[error("legacy journal cannot form a valid v5 writer identity")]
    WriterIdentity,
}

/// Strictly decoded, safe-to-rank v1 checkpoint. Its private fields ensure
/// schema migration and GPUI recovery cannot accidentally use an unvalidated
/// JSON field after parsing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyJournalPayload {
    expected_revision: i64,
    generation: i64,
    title: String,
    document: CanonicalDocument,
    resource_ids: Vec<ResourceId>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyJournalPayloadWire {
    version: u8,
    note_id: String,
    expected_revision: i64,
    generation: i64,
    title: String,
    body_html: String,
    resource_ids: Vec<String>,
}

impl LegacyJournalPayload {
    /// Decode the complete v1 wire object and validate it against the exact
    /// v4 SQL row and that note's current resource relation. `sql_note_id` is
    /// already typed, so an invalid raw SQL note ID must be rejected by the
    /// caller before reaching this boundary.
    pub fn parse_and_validate(
        delta_utf8: &str,
        sql_note_id: &NoteId,
        sql_generation: i64,
        associated_resource_ids: &[ResourceId],
    ) -> Result<Self, LegacyJournalPayloadError> {
        let wire: LegacyJournalPayloadWire = serde_json::from_str(delta_utf8)?;
        if wire.version != LEGACY_JOURNAL_SCHEMA_VERSION {
            return Err(LegacyJournalPayloadError::Version);
        }
        if NoteId::parse(&wire.note_id).ok().as_ref() != Some(sql_note_id) {
            return Err(LegacyJournalPayloadError::NoteIdentity);
        }
        if wire.expected_revision < 1 {
            return Err(LegacyJournalPayloadError::ExpectedRevision);
        }
        if sql_generation < 1 || wire.generation < 1 || wire.generation != sql_generation {
            return Err(LegacyJournalPayloadError::Generation);
        }
        if wire.title.len() > MAX_LEGACY_TITLE_BYTES {
            return Err(LegacyJournalPayloadError::TitleRange);
        }
        if wire.body_html.len() > MAX_LEGACY_BODY_BYTES {
            return Err(LegacyJournalPayloadError::BodyRange);
        }
        if wire.resource_ids.len() > MAX_LEGACY_RESOURCE_REFERENCES {
            return Err(LegacyJournalPayloadError::ResourceCount);
        }
        let resource_ids = wire
            .resource_ids
            .into_iter()
            .map(|raw| ResourceId::new(raw).map_err(|_| LegacyJournalPayloadError::ResourceId))
            .collect::<Result<Vec<_>, _>>()?;
        if resource_ids != associated_resource_ids {
            return Err(LegacyJournalPayloadError::ResourceRelation);
        }
        let document = CanonicalDocument::parse_html(&wire.body_html)
            .map_err(|_| LegacyJournalPayloadError::CanonicalBody)?;
        if document.to_canonical_html().as_str() != wire.body_html {
            return Err(LegacyJournalPayloadError::CanonicalBody);
        }
        if document.resource_ids() != resource_ids {
            return Err(LegacyJournalPayloadError::ResourceReferences);
        }
        Ok(Self {
            expected_revision: wire.expected_revision,
            generation: wire.generation,
            title: wire.title,
            document,
            resource_ids,
        })
    }

    pub fn expected_revision(&self) -> i64 {
        self.expected_revision
    }

    pub fn generation(&self) -> i64 {
        self.generation
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn document(&self) -> &CanonicalDocument {
        &self.document
    }

    pub fn resource_ids(&self) -> &[ResourceId] {
        &self.resource_ids
    }

    pub fn into_parts(self) -> (String, CanonicalDocument, Vec<ResourceId>) {
        (self.title, self.document, self.resource_ids)
    }
}

/// v1 did not persist a token. Its v5 lease owner must be deterministic and
/// bounded, and it must derive only from the v4 opaque journal row identity.
pub fn legacy_writer_token_for_journal_id(
    journal_id: &str,
) -> Result<String, LegacyJournalPayloadError> {
    // Journal IDs use the same opaque 32-lowercase-hex source as note IDs.
    // Parsing with NoteId preserves that wire rule without conflating the two
    // domain types in the public DTO.
    if NoteId::parse(journal_id).is_err() {
        return Err(LegacyJournalPayloadError::WriterIdentity);
    }
    let token = format!("legacy-v4-{journal_id}");
    if token.is_empty()
        || token.len() > MAX_LEGACY_WRITER_TOKEN_BYTES
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(LegacyJournalPayloadError::WriterIdentity);
    }
    Ok(token)
}
