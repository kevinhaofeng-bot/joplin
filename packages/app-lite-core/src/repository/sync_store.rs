//! SQLite side of the client sync engine (docs/research/sync-client-design-v1.md §2).
//! The engine in `crate::sync` decides; these methods own every query.

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};

use super::{LibraryError, LibraryRepository, NoteId};

const DEVICE_ID_SETTING: &str = "sync.device_id";
/// Kinds in dependency order: containers before the notes that reference them.
const KINDS: [&str; 5] = ["stack", "notebook", "tag", "resource", "note"];

/// One entity's persisted upload, resent unchanged until it has a definite
/// outcome.
#[derive(Debug, Clone)]
pub struct SyncInflight {
    pub entity_type: String,
    pub entity_id: String,
    pub op_id: String,
    pub base_revision: i64,
    /// `app_lite_protocol::Action` as JSON.
    pub action_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncFailure {
    pub op_id: String,
    pub entity_type: String,
    pub entity_id: String,
    pub reason: String,
}

impl LibraryRepository {
    /// This profile's sync identity, created on first use. A restored or
    /// imported library gets a new one.
    pub fn sync_device_id(&self) -> Result<String, LibraryError> {
        let connection = self.connection.lock().expect("library mutex poisoned");
        let read = |connection: &rusqlite::Connection| {
            connection
                .query_row(
                    "SELECT value FROM settings WHERE key=?1",
                    [DEVICE_ID_SETTING],
                    |row| row.get::<_, String>(0),
                )
                .optional()
        };
        if let Some(id) = read(&connection)? {
            return Ok(id);
        }
        let id = self.id_source.next_id()?;
        if NoteId::parse(&id).is_err() {
            return Err(LibraryError::InvalidId);
        }
        connection.execute(
            "INSERT INTO settings(key,value,updated_time) VALUES(?1,?2,?3) ON CONFLICT(key) DO NOTHING",
            params![DEVICE_ID_SETTING, id, self.now()],
        )?;
        read(&connection)?.ok_or(LibraryError::InvalidSnapshot)
    }

    /// Creates in-flight ops for entities that changed (outbox) or were never
    /// uploaded, then returns every in-flight op, oldest first, up to `limit`.
    pub(crate) fn sync_prepare_inflight(
        &self,
        limit: usize,
    ) -> Result<Vec<SyncInflight>, LibraryError> {
        let now = self.now();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: i64 =
            transaction.query_row("SELECT count(*) FROM sync_inflight", [], |row| row.get(0))?;
        let mut room = limit.saturating_sub(existing as usize);
        for kind in KINDS {
            if room == 0 {
                break;
            }
            for (id, outbox_ids) in candidates(&transaction, kind, room)? {
                let base_revision: i64 = transaction
                    .query_row(
                        "SELECT server_revision FROM sync_entities WHERE entity_type=?1 AND entity_id=?2",
                        params![kind, id],
                        |row| row.get(0),
                    )
                    .optional()?
                    .unwrap_or(0);
                let action = match entity_payload(&transaction, kind, &id)? {
                    Some(payload) => json!({ "type": "put", "payload": payload }),
                    None if base_revision == 0 => {
                        // The server never saw it: there is nothing to delete.
                        delete_outbox(&transaction, &outbox_ids)?;
                        continue;
                    }
                    None => json!({ "type": "delete" }),
                };
                let op_id = self.id_source.next_id()?;
                if NoteId::parse(&op_id).is_err() {
                    return Err(LibraryError::InvalidId);
                }
                transaction.execute(
                    "INSERT INTO sync_inflight(entity_type,entity_id,op_id,base_revision,action_json,outbox_ids_json,created_time)
                     VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        kind,
                        id,
                        op_id,
                        base_revision,
                        action.to_string(),
                        Value::from(outbox_ids).to_string(),
                        now
                    ],
                )?;
                room -= 1;
            }
        }
        transaction.commit()?;
        let mut statement = connection.prepare(
            "SELECT entity_type,entity_id,op_id,base_revision,action_json FROM sync_inflight
             ORDER BY created_time, CASE entity_type WHEN 'stack' THEN 0 WHEN 'notebook' THEN 1
             WHEN 'tag' THEN 2 WHEN 'resource' THEN 3 ELSE 4 END, entity_id LIMIT ?1",
        )?;
        let rows = statement.query_map([limit as i64], |row| {
            Ok(SyncInflight {
                entity_type: row.get(0)?,
                entity_id: row.get(1)?,
                op_id: row.get(2)?,
                base_revision: row.get(3)?,
                action_json: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The server applied (or had already applied) this op.
    pub(crate) fn sync_record_accepted(
        &self,
        op_id: &str,
        server_revision: u64,
    ) -> Result<(), LibraryError> {
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some((kind, id, outbox_ids)) = transaction
            .query_row(
                "SELECT entity_type,entity_id,outbox_ids_json FROM sync_inflight WHERE op_id=?1",
                [op_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
        else {
            return Ok(());
        };
        let outbox_ids: Vec<String> =
            serde_json::from_str(&outbox_ids).map_err(|_| LibraryError::InvalidSnapshot)?;
        delete_outbox(&transaction, &outbox_ids)?;
        transaction.execute("DELETE FROM sync_inflight WHERE op_id=?1", [op_id])?;
        transaction.execute("DELETE FROM sync_failures WHERE op_id=?1", [op_id])?;
        transaction.execute(
            "INSERT INTO sync_entities(entity_type,entity_id,server_revision) VALUES(?1,?2,?3)
             ON CONFLICT(entity_type,entity_id) DO UPDATE SET server_revision=excluded.server_revision",
            params![kind, id, server_revision as i64],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Keeps the op and its outbox rows; the reason is shown to the person.
    pub(crate) fn sync_record_permanent(
        &self,
        op_id: &str,
        reason: &str,
    ) -> Result<(), LibraryError> {
        let connection = self.connection.lock().expect("library mutex poisoned");
        connection.execute(
            "INSERT INTO sync_failures(op_id,entity_type,entity_id,reason,updated_time)
             SELECT op_id,entity_type,entity_id,?2,?3 FROM sync_inflight WHERE op_id=?1
             ON CONFLICT(op_id) DO UPDATE SET reason=excluded.reason, updated_time=excluded.updated_time",
            params![op_id, reason, self.now()],
        )?;
        Ok(())
    }

    pub fn sync_failures(&self) -> Result<Vec<SyncFailure>, LibraryError> {
        let connection = self.connection.lock().expect("library mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT op_id,entity_type,entity_id,reason FROM sync_failures ORDER BY updated_time, op_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(SyncFailure {
                op_id: row.get(0)?,
                entity_type: row.get(1)?,
                entity_id: row.get(2)?,
                reason: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

/// Entities of `kind` with outbox rows, or that exist locally but were never
/// uploaded, and have no in-flight op yet. Oldest change first.
fn candidates(
    transaction: &Transaction<'_>,
    kind: &str,
    limit: usize,
) -> Result<Vec<(String, Vec<String>)>, LibraryError> {
    // A trashed note still uploads its state; other kinds are gone once deleted.
    let (table, live) = match kind {
        "stack" => ("stacks", "t.deleted_time=0"),
        "notebook" => ("notebooks", "t.deleted_time=0"),
        "tag" => ("tags", "t.deleted_time=0"),
        "resource" => ("resources", "t.deleted_time=0"),
        _ => ("notes", "1"),
    };
    let mut statement = transaction.prepare(&format!(
        "SELECT entity_id, json_group_array(outbox_id) FILTER (WHERE outbox_id IS NOT NULL), min(ordering)
         FROM (
             SELECT entity_id, id AS outbox_id, created_time AS ordering FROM sync_outbox WHERE entity_type=?1
             UNION ALL
             SELECT t.id, NULL, -1 FROM {table} t
             WHERE {live} AND NOT EXISTS (SELECT 1 FROM sync_entities e WHERE e.entity_type=?1 AND e.entity_id=t.id)
         ) AS pending
         WHERE NOT EXISTS (SELECT 1 FROM sync_inflight i WHERE i.entity_type=?1 AND i.entity_id=pending.entity_id)
         GROUP BY entity_id ORDER BY min(ordering), entity_id LIMIT ?2"
    ))?;
    let rows = statement.query_map(params![kind, limit as i64], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    rows.map(|row| {
        let (id, outbox) = row?;
        let outbox_ids = match outbox {
            Some(json) => serde_json::from_str(&json).map_err(|_| LibraryError::InvalidSnapshot)?,
            None => Vec::new(),
        };
        Ok((id, outbox_ids))
    })
    .collect()
}

fn delete_outbox(transaction: &Transaction<'_>, ids: &[String]) -> Result<(), LibraryError> {
    for id in ids {
        transaction.execute("DELETE FROM sync_outbox WHERE id=?1", [id])?;
    }
    Ok(())
}

/// Portable state of one live entity, or `None` when it no longer exists.
fn entity_payload(
    transaction: &Transaction<'_>,
    kind: &str,
    id: &str,
) -> Result<Option<Value>, LibraryError> {
    let payload = match kind {
        "note" => transaction
            .query_row(
                "SELECT title,body_html,notebook_id,created_time,updated_time,deleted_time FROM notes WHERE id=?1",
                [id],
                |row| {
                    Ok(json!({
                        "title": row.get::<_, String>(0)?,
                        "body_html": row.get::<_, String>(1)?,
                        "notebook_id": row.get::<_, String>(2)?,
                        "created_time": row.get::<_, i64>(3)?,
                        "updated_time": row.get::<_, i64>(4)?,
                        "deleted_time": row.get::<_, i64>(5)?,
                    }))
                },
            )
            .optional()?
            .map(|mut note| -> Result<Value, LibraryError> {
                note["tag_ids"] = Value::from(column(
                    transaction,
                    "SELECT tag_id FROM note_tags WHERE note_id=?1 ORDER BY position, tag_id",
                    id,
                )?);
                note["resource_ids"] = Value::from(column(
                    transaction,
                    "SELECT resource_id FROM note_resources WHERE note_id=?1 AND is_associated=1 ORDER BY position",
                    id,
                )?);
                Ok(note)
            })
            .transpose()?,
        "notebook" => transaction
            .query_row(
                "SELECT title,stack_id,is_default FROM notebooks WHERE id=?1 AND deleted_time=0",
                [id],
                |row| {
                    Ok(json!({
                        "title": row.get::<_, String>(0)?,
                        "stack_id": row.get::<_, Option<String>>(1)?,
                        "is_default": row.get::<_, i64>(2)? != 0,
                    }))
                },
            )
            .optional()?,
        "stack" | "tag" => transaction
            .query_row(
                &format!(
                    "SELECT title FROM {} WHERE id=?1 AND deleted_time=0",
                    if kind == "stack" { "stacks" } else { "tags" }
                ),
                [id],
                |row| Ok(json!({ "title": row.get::<_, String>(0)? })),
            )
            .optional()?,
        "resource" => transaction
            .query_row(
                "SELECT title,mime,file_extension,size,sha256 FROM resources WHERE id=?1 AND deleted_time=0",
                [id],
                |row| {
                    Ok(json!({
                        "title": row.get::<_, String>(0)?,
                        "mime": row.get::<_, String>(1)?,
                        "file_extension": row.get::<_, String>(2)?,
                        "size": row.get::<_, i64>(3)?,
                        "sha256": row.get::<_, String>(4)?,
                    }))
                },
            )
            .optional()?,
        _ => return Err(LibraryError::InvalidSnapshot),
    };
    Ok(payload)
}

fn column(transaction: &Transaction<'_>, sql: &str, id: &str) -> Result<Vec<String>, LibraryError> {
    let mut statement = transaction.prepare(sql)?;
    let rows = statement.query_map([id], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}
