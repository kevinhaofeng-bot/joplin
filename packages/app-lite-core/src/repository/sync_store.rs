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

const CURSOR_NAME: &str = "server";
const CONFLICT_SUFFIX: &str = "（冲突副本）";

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncApplyReport {
    pub applied: usize,
    pub conflicts: usize,
    pub skipped: usize,
}

/// Why one remote change was not applied; recorded as a visible failure.
struct Skip(String);

impl LibraryRepository {
    pub(crate) fn sync_cursor(&self) -> Result<u64, LibraryError> {
        let connection = self.connection.lock().expect("library mutex poisoned");
        let cursor: Option<String> = connection
            .query_row(
                "SELECT cursor FROM sync_cursor WHERE name=?1",
                [CURSOR_NAME],
                |row| row.get(0),
            )
            .optional()?;
        cursor
            .map(|value| value.parse().map_err(|_| LibraryError::InvalidSnapshot))
            .unwrap_or(Ok(0))
    }

    /// Applies one pulled page and advances the cursor in the same
    /// transaction. Remote changes never produce outbox rows (no echo).
    pub(crate) fn sync_apply_page(
        &self,
        changes: &[app_lite_protocol::Change],
        next_cursor: u64,
    ) -> Result<SyncApplyReport, LibraryError> {
        let now = self.now();
        let mut report = SyncApplyReport::default();
        let mut touched_notes = Vec::new();
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for change in changes {
            let kind = change.entity.kind.as_str();
            let id = change.entity.id.as_str();
            if !app_lite_protocol::valid_id(id) {
                continue;
            }
            let known: i64 = transaction
                .query_row(
                    "SELECT server_revision FROM sync_entities WHERE entity_type=?1 AND entity_id=?2",
                    params![kind, id],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or(0);
            if known >= change.revision as i64 {
                continue;
            }
            let inflight_op: Option<String> = transaction
                .query_row(
                    "SELECT op_id FROM sync_inflight WHERE entity_type=?1 AND entity_id=?2",
                    params![kind, id],
                    |row| row.get(0),
                )
                .optional()?;
            if inflight_op.as_deref() == Some(change.op_id.as_str()) {
                // Our own op whose Accepted response was lost.
                accept_inflight(&transaction, &change.op_id, kind, id, change.revision)?;
                continue;
            }
            // Local work the server has not confirmed: queued changes, or an
            // entity that exists here but was never uploaded (a restored or
            // re-imported library).
            let pending = inflight_op.is_some()
                || transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sync_outbox WHERE entity_type=?1 AND entity_id=?2)",
                    params![kind, id],
                    |row| row.get::<_, i64>(0),
                )? != 0
                || (known == 0 && local_row_exists(&transaction, kind, id)?);
            if pending {
                // Remote wins the entity: a local note edit survives as its
                // conflict copy (made from the note row, not the outbox),
                // other kinds lose the edit. Cleared first so that applying
                // may queue new work (an attachment still used here).
                drop_pending(&transaction, kind, id)?;
            }
            let outcome = match kind {
                "note" => self.apply_remote_note(&transaction, id, change, pending, now),
                "notebook" => apply_remote_notebook(&transaction, id, change, now),
                "stack" | "tag" => apply_remote_container(&transaction, kind, id, change, now),
                "resource" => self.apply_remote_resource(&transaction, id, change, now),
                _ => Ok(Err(Skip(format!("{kind} changes are not supported")))),
            };
            match outcome? {
                Ok(conflict) => {
                    report.applied += 1;
                    if pending {
                        report.conflicts += usize::from(conflict || kind != "note");
                    }
                    if kind == "note" {
                        touched_notes.push(id.to_owned());
                    }
                }
                Err(Skip(reason)) => {
                    report.skipped += 1;
                    transaction.execute(
                        "INSERT INTO sync_failures(op_id,entity_type,entity_id,reason,updated_time) VALUES(?1,?2,?3,?4,?5)
                         ON CONFLICT(op_id) DO UPDATE SET reason=excluded.reason, updated_time=excluded.updated_time",
                        params![change.op_id, kind, id, reason, now],
                    )?;
                }
            }
            transaction.execute(
                "INSERT INTO sync_entities(entity_type,entity_id,server_revision) VALUES(?1,?2,?3)
                 ON CONFLICT(entity_type,entity_id) DO UPDATE SET server_revision=excluded.server_revision",
                params![kind, id, change.revision as i64],
            )?;
        }
        transaction.execute(
            "INSERT INTO sync_cursor(name,cursor,updated_time) VALUES(?1,?2,?3)
             ON CONFLICT(name) DO UPDATE SET cursor=excluded.cursor, updated_time=excluded.updated_time",
            params![CURSOR_NAME, next_cursor.to_string(), now],
        )?;
        transaction.commit()?;
        drop(connection);
        if report.applied > 0 {
            let mut events = vec![super::LibraryEvent::OrganizationChanged];
            for id in touched_notes {
                if let Ok(id) = NoteId::parse(&id) {
                    events.push(super::LibraryEvent::NoteProjectionChanged(id.clone()));
                    events.push(super::LibraryEvent::SearchProjectionQueued(id));
                }
            }
            self.publish(events);
        }
        Ok(report)
    }

    /// Ok(true) when a local edit was kept as a conflict copy.
    fn apply_remote_note(
        &self,
        transaction: &Transaction<'_>,
        id: &str,
        change: &app_lite_protocol::Change,
        pending: bool,
        now: i64,
    ) -> Result<Result<bool, Skip>, LibraryError> {
        let note_id = NoteId::parse(id).map_err(|_| LibraryError::InvalidId)?;
        let exists = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM notes WHERE id=?1)",
            [id],
            |row| row.get::<_, i64>(0),
        )? != 0;
        let remote = match (&change.deleted, &change.payload) {
            (false, Some(payload)) => match RemoteNote::parse(transaction, payload)? {
                Ok(note) => Some(note),
                Err(skip) => return Ok(Err(skip)),
            },
            (true, _) => None,
            (false, None) => return Ok(Err(Skip("note change without payload".into()))),
        };
        let copied = pending
            && exists
            && !remote
                .as_ref()
                .is_some_and(|note| note.matches_local(transaction, &note_id).unwrap_or(false));
        if copied {
            self.conflict_copy(transaction, &note_id, change.revision, now)?;
        }
        match remote {
            Some(note) => {
                note.write(transaction, &note_id, now)?;
            }
            None if exists => {
                transaction.execute(
                    "INSERT INTO tombstones (entity_type, entity_id, final_revision, deleted_time, purged_time)
                     SELECT 'note', id, revision + 1, CASE WHEN deleted_time = 0 THEN ?2 ELSE deleted_time END, ?2 FROM notes WHERE id = ?1",
                    params![id, now],
                )?;
                transaction.execute("DELETE FROM note_revisions WHERE note_id=?1", [id])?;
                transaction.execute("DELETE FROM notes WHERE id=?1", [id])?;
                super::queue_search(transaction, &note_id, now, "remote-purge")?;
            }
            None => {}
        }
        Ok(Ok(copied))
    }

    pub(crate) fn sync_has_blob(&self, sha256: &str) -> Result<bool, LibraryError> {
        let connection = self.connection.lock().expect("library mutex poisoned");
        Ok(connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM resource_blobs WHERE sha256=?1)",
            [sha256],
            |row| row.get::<_, i64>(0),
        )? != 0)
    }

    /// Writes downloaded bytes into the content-addressed store. Metadata
    /// follows only when the page applies, after `verify_staged_blob`.
    pub(crate) fn sync_store_blob<R: std::io::Read>(
        &self,
        reader: R,
        size: usize,
        title: &str,
        mime: &str,
        extension: &str,
    ) -> Result<crate::BlobHash, LibraryError> {
        Ok(self
            .resource_store
            .put_reader(reader, size, title, mime, extension)?
            .sha256)
    }

    fn apply_remote_resource(
        &self,
        transaction: &Transaction<'_>,
        id: &str,
        change: &app_lite_protocol::Change,
        now: i64,
    ) -> Result<Result<bool, Skip>, LibraryError> {
        if change.deleted {
            let used: i64 = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM note_resources WHERE resource_id=?1)
                     OR EXISTS(SELECT 1 FROM note_revisions WHERE instr(body_html, ':/' || ?1) > 0)",
                [id],
                |row| row.get(0),
            )?;
            if used != 0 {
                // Still shown here: keep it and put it back on the server.
                let resource = crate::ResourceId::new(id).map_err(|_| LibraryError::InvalidId)?;
                super::enqueue_sync(
                    transaction,
                    self.id_source.as_ref(),
                    &crate::EntityRef::Resource(resource),
                    1,
                    "restore",
                    now,
                )?;
                return Ok(Ok(true));
            }
            let sha: Option<String> = transaction
                .query_row("SELECT sha256 FROM resources WHERE id=?1", [id], |row| {
                    row.get(0)
                })
                .optional()?;
            if let Some(sha) = sha {
                transaction.execute(
                    "INSERT OR IGNORE INTO tombstones (entity_type, entity_id, final_revision, deleted_time, purged_time)
                     SELECT 'resource', id, revision + 1, ?2, ?2 FROM resources WHERE id=?1",
                    params![id, now],
                )?;
                transaction.execute("DELETE FROM resources WHERE id=?1", [id])?;
                let orphan = transaction.execute(
                    "DELETE FROM resource_blobs WHERE sha256=?1 AND NOT EXISTS(SELECT 1 FROM resources WHERE sha256=?1)",
                    [&sha],
                )?;
                if orphan != 0 {
                    transaction.execute(
                        "INSERT OR IGNORE INTO resource_gc_queue (sha256, created_time) VALUES (?1, ?2)",
                        params![sha, now],
                    )?;
                }
            }
            return Ok(Ok(false));
        }
        let Some(resource) = change.payload.as_ref().and_then(RemoteResourceRef::parse) else {
            return Ok(Err(Skip("attachment payload is incomplete".into())));
        };
        let existing: Option<String> = transaction
            .query_row("SELECT sha256 FROM resources WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .optional()?;
        if existing.is_some_and(|sha| sha != resource.sha256.as_str()) {
            return Ok(Err(Skip(
                "attachment content changed on another device".into(),
            )));
        }
        // The blob was downloaded before this page; inside this writer
        // transaction GC cannot race the check below.
        transaction.execute(
            "DELETE FROM resource_gc_queue WHERE sha256=?1",
            [resource.sha256.as_str()],
        )?;
        self.resource_store
            .verify_staged_blob(&crate::resource::ResourceBlob {
                sha256: resource.sha256.clone(),
                size: resource.size,
            })?;
        transaction.execute(
            "INSERT OR IGNORE INTO resource_blobs (sha256, size, mime, relative_path, created_time, revision) VALUES (?1, ?2, ?3, ?4, ?5, 1)",
            params![
                resource.sha256.as_str(),
                resource.size as i64,
                resource.mime,
                format!("resources/blobs/{}", resource.sha256.as_str()),
                now
            ],
        )?;
        transaction.execute(
            "DELETE FROM tombstones WHERE entity_type='resource' AND entity_id=?1",
            [id],
        )?;
        transaction.execute(
            "INSERT INTO resources (id, sha256, title, mime, file_extension, size, created_time, updated_time, deleted_time, revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, 0, 1)
             ON CONFLICT(id) DO UPDATE SET title=excluded.title, mime=excluded.mime,
               file_extension=excluded.file_extension, updated_time=excluded.updated_time,
               deleted_time=0, revision=resources.revision+1",
            params![
                id,
                resource.sha256.as_str(),
                resource.title,
                resource.mime,
                resource.file_extension,
                resource.size as i64,
                now
            ],
        )?;
        Ok(Ok(false))
    }

    /// The local version becomes a new note (uploaded like any new note) so
    /// that adopting the remote version loses nothing.
    fn conflict_copy(
        &self,
        transaction: &Transaction<'_>,
        original: &NoteId,
        remote_revision: u64,
        now: i64,
    ) -> Result<(), LibraryError> {
        let copy = NoteId::parse(self.allocate_id(transaction, "notes")?)
            .map_err(|_| LibraryError::InvalidId)?;
        let local_revision: i64 = transaction.query_row(
            "SELECT revision FROM notes WHERE id=?1",
            [original.as_str()],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO notes (id,title,body_html,body_text,snippet,notebook_id,created_time,updated_time,deleted_time,revision)
             SELECT ?2, title || ?3, body_html, body_text, snippet, notebook_id, ?4, ?4, 0, 1 FROM notes WHERE id=?1",
            params![original.as_str(), copy.as_str(), CONFLICT_SUFFIX, now],
        )?;
        transaction.execute(
            "INSERT INTO note_tags (note_id, tag_id, position) SELECT ?2, tag_id, position FROM note_tags WHERE note_id=?1",
            params![original.as_str(), copy.as_str()],
        )?;
        transaction.execute(
            "INSERT INTO note_resources (note_id, position, resource_id, is_associated)
             SELECT ?2, position, resource_id, 1 FROM note_resources WHERE note_id=?1 AND is_associated=1",
            params![original.as_str(), copy.as_str()],
        )?;
        transaction.execute(
            "INSERT INTO note_revisions (note_id, revision, title, body_html, body_text, created_time)
             SELECT id, revision, title, body_html, body_text, updated_time FROM notes WHERE id=?1",
            [copy.as_str()],
        )?;
        super::queue_search(transaction, &copy, now, "conflict-copy")?;
        super::queue_derived_text_for_note(transaction, &copy, now)?;
        super::enqueue_sync(
            transaction,
            self.id_source.as_ref(),
            &crate::EntityRef::Note(copy.clone()),
            1,
            "create",
            now,
        )?;
        transaction.execute(
            "INSERT INTO sync_conflicts (id, entity_id, local_revision, remote_revision, created_time) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![copy.as_str(), original.as_str(), local_revision, remote_revision as i64, now],
        )?;
        Ok(())
    }
}

/// A validated remote note body and its relations.
struct RemoteNote {
    title: String,
    body_html: String,
    body_text: String,
    notebook_id: String,
    tag_ids: Vec<String>,
    resource_ids: Vec<crate::ResourceId>,
    created_time: i64,
    updated_time: i64,
    deleted_time: i64,
}

impl RemoteNote {
    fn parse(
        transaction: &Transaction<'_>,
        payload: &Value,
    ) -> Result<Result<Self, Skip>, LibraryError> {
        let text = |key: &str| payload.get(key).and_then(Value::as_str);
        let number = |key: &str| payload.get(key).and_then(Value::as_i64);
        let ids = |key: &str| -> Option<Vec<String>> {
            payload
                .get(key)?
                .as_array()?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .filter(|id| app_lite_protocol::valid_id(id))
                        .map(str::to_owned)
                })
                .collect()
        };
        let (Some(title), Some(body_html), Some(notebook_id), Some(tag_ids), Some(resource_ids)) = (
            text("title"),
            text("body_html"),
            text("notebook_id"),
            ids("tag_ids"),
            ids("resource_ids"),
        ) else {
            return Ok(Err(Skip("note payload is incomplete".into())));
        };
        let Ok(document) = crate::CanonicalDocument::parse_html(body_html) else {
            return Ok(Err(Skip("note body is not valid HTML".into())));
        };
        if document.to_canonical_html().as_str() != body_html {
            return Ok(Err(Skip("note body is not canonical".into())));
        }
        let declared_resources = resource_ids;
        let resource_ids: Vec<crate::ResourceId> = document.resource_ids();
        if resource_ids
            .iter()
            .map(crate::ResourceId::as_str)
            .ne(declared_resources.iter().map(String::as_str))
        {
            return Ok(Err(Skip("note resources differ from its body".into())));
        }
        for resource in &resource_ids {
            let present: i64 = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM resources WHERE id=?1)",
                [resource.as_str()],
                |row| row.get(0),
            )?;
            if present == 0 {
                return Ok(Err(Skip(format!(
                    "attachment {} is not available",
                    resource.as_str()
                ))));
            }
        }
        let notebook_exists: i64 = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM notebooks WHERE id=?1 AND deleted_time=0)",
            [notebook_id],
            |row| row.get(0),
        )?;
        let notebook_id = if notebook_exists != 0 {
            notebook_id.to_owned()
        } else {
            transaction.query_row(
                "SELECT id FROM notebooks WHERE is_default=1 AND deleted_time=0 ORDER BY id LIMIT 1",
                [],
                |row| row.get(0),
            )?
        };
        let mut known_tags = Vec::new();
        for tag in tag_ids {
            let present: i64 = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM tags WHERE id=?1 AND deleted_time=0)",
                [&tag],
                |row| row.get(0),
            )?;
            if present != 0 {
                known_tags.push(tag);
            }
        }
        Ok(Ok(Self {
            title: title.chars().take(1024).collect(),
            body_html: body_html.to_owned(),
            body_text: document.search_text().as_str().to_owned(),
            notebook_id,
            tag_ids: known_tags,
            resource_ids,
            created_time: number("created_time").unwrap_or(0),
            updated_time: number("updated_time").unwrap_or(0),
            deleted_time: number("deleted_time").unwrap_or(0),
        }))
    }

    /// Same visible content as the local note: nothing to keep as a copy.
    fn matches_local(
        &self,
        transaction: &Transaction<'_>,
        id: &NoteId,
    ) -> Result<bool, LibraryError> {
        let local: Option<(String, String, String, i64)> = transaction
            .query_row(
                "SELECT title, body_html, notebook_id, deleted_time FROM notes WHERE id=?1",
                [id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((title, body_html, notebook_id, deleted_time)) = local else {
            return Ok(false);
        };
        let tags = column(
            transaction,
            "SELECT tag_id FROM note_tags WHERE note_id=?1 ORDER BY position, tag_id",
            id.as_str(),
        )?;
        Ok(title == self.title
            && body_html == self.body_html
            && notebook_id == self.notebook_id
            && (deleted_time != 0) == (self.deleted_time != 0)
            && tags == self.tag_ids)
    }

    fn write(
        &self,
        transaction: &Transaction<'_>,
        id: &NoteId,
        now: i64,
    ) -> Result<(), LibraryError> {
        let snippet = super::snippet(&self.body_text);
        // A remote edit may revive a note purged here (delete versus edit).
        transaction.execute(
            "DELETE FROM tombstones WHERE entity_type='note' AND entity_id=?1",
            [id.as_str()],
        )?;
        transaction.execute(
            "INSERT INTO notes (id,title,body_html,body_text,snippet,notebook_id,created_time,updated_time,deleted_time,revision)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,1)
             ON CONFLICT(id) DO UPDATE SET title=excluded.title, body_html=excluded.body_html,
               body_text=excluded.body_text, snippet=excluded.snippet, notebook_id=excluded.notebook_id,
               created_time=excluded.created_time, updated_time=excluded.updated_time,
               deleted_time=excluded.deleted_time, revision=notes.revision+1",
            params![
                id.as_str(),
                self.title,
                self.body_html,
                self.body_text,
                snippet,
                self.notebook_id,
                self.created_time,
                self.updated_time,
                self.deleted_time
            ],
        )?;
        transaction.execute(
            "INSERT OR REPLACE INTO note_revisions (note_id, revision, title, body_html, body_text, created_time)
             SELECT id, revision, title, body_html, body_text, ?2 FROM notes WHERE id=?1",
            params![id.as_str(), now],
        )?;
        transaction.execute("DELETE FROM note_tags WHERE note_id=?1", [id.as_str()])?;
        for (position, tag) in self.tag_ids.iter().enumerate() {
            transaction.execute(
                "INSERT INTO note_tags (note_id, tag_id, position) VALUES (?1, ?2, ?3)",
                params![id.as_str(), tag, position as i64],
            )?;
        }
        super::replace_note_resources(transaction, id, &self.resource_ids)?;
        transaction.execute(
            "UPDATE notes SET selected_thumbnail_id=NULL WHERE id=?1 AND selected_thumbnail_id IS NOT NULL
               AND NOT EXISTS(SELECT 1 FROM note_resources WHERE note_id=?1 AND resource_id=notes.selected_thumbnail_id AND is_associated=1)",
            [id.as_str()],
        )?;
        super::queue_search(transaction, id, now, "remote")?;
        super::queue_derived_text_for_note(transaction, id, now)?;
        Ok(())
    }
}

fn remote_title(change: &app_lite_protocol::Change) -> Result<String, Skip> {
    change
        .payload
        .as_ref()
        .and_then(|payload| payload.get("title"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty() && title.chars().count() <= 255)
        .map(str::to_owned)
        .ok_or_else(|| Skip("title is missing or invalid".into()))
}

fn apply_remote_container(
    transaction: &Transaction<'_>,
    kind: &str,
    id: &str,
    change: &app_lite_protocol::Change,
    now: i64,
) -> Result<Result<bool, Skip>, LibraryError> {
    let table = if kind == "stack" { "stacks" } else { "tags" };
    if change.deleted {
        transaction.execute(&format!("DELETE FROM {table} WHERE id=?1"), [id])?;
        return Ok(Ok(false));
    }
    let title = match remote_title(change) {
        Ok(title) => title,
        Err(skip) => return Ok(Err(skip)),
    };
    transaction.execute(
        &format!(
            "INSERT INTO {table} (id,title,revision,created_time,updated_time,deleted_time) VALUES (?1,?2,1,?3,?3,0)
             ON CONFLICT(id) DO UPDATE SET title=excluded.title, updated_time=excluded.updated_time,
               deleted_time=0, revision={table}.revision+1"
        ),
        params![id, title, now],
    )?;
    Ok(Ok(false))
}

fn apply_remote_notebook(
    transaction: &Transaction<'_>,
    id: &str,
    change: &app_lite_protocol::Change,
    now: i64,
) -> Result<Result<bool, Skip>, LibraryError> {
    let local_default: String = transaction.query_row(
        "SELECT id FROM notebooks WHERE is_default=1 AND deleted_time=0 ORDER BY id LIMIT 1",
        [],
        |row| row.get(0),
    )?;
    if change.deleted {
        if id != local_default {
            transaction.execute(
                "UPDATE notes SET notebook_id=?2 WHERE notebook_id=?1",
                params![id, local_default],
            )?;
            transaction.execute("DELETE FROM notebooks WHERE id=?1 AND is_default=0", [id])?;
        }
        return Ok(Ok(false));
    }
    let title = match remote_title(change) {
        Ok(title) => title,
        Err(skip) => return Ok(Err(skip)),
    };
    let payload = change
        .payload
        .as_ref()
        .expect("title came from the payload");
    let stack_id = payload
        .get("stack_id")
        .and_then(Value::as_str)
        .filter(|stack| {
            transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM stacks WHERE id=?1 AND deleted_time=0)",
                    [stack],
                    |row| row.get::<_, i64>(0),
                )
                .is_ok_and(|exists| exists != 0)
        });
    let remote_default = payload.get("is_default").and_then(Value::as_bool) == Some(true);
    if remote_default && id != local_default {
        // A fresh device joining a library: its untouched default notebook
        // becomes the library's, so both devices share one default.
        let untouched: i64 = transaction.query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM notes WHERE notebook_id=?1)
                AND NOT EXISTS(SELECT 1 FROM sync_entities WHERE entity_type='notebook' AND entity_id=?1)",
            [&local_default],
            |row| row.get(0),
        )?;
        if untouched != 0 {
            transaction.execute(
                "DELETE FROM sync_inflight WHERE entity_type='notebook' AND entity_id=?1",
                [&local_default],
            )?;
            transaction.execute(
                "DELETE FROM sync_outbox WHERE entity_type='notebook' AND entity_id=?1",
                [&local_default],
            )?;
            transaction.execute(
                "UPDATE notebooks SET id=?2, title=?3, stack_id=?4, updated_time=?5 WHERE id=?1",
                params![local_default, id, title, stack_id, now],
            )?;
            return Ok(Ok(false));
        }
    }
    transaction.execute(
        "INSERT INTO notebooks (id,title,stack_id,is_default,revision,created_time,updated_time,deleted_time)
         VALUES (?1,?2,?3,0,1,?4,?4,0)
         ON CONFLICT(id) DO UPDATE SET title=excluded.title, stack_id=excluded.stack_id,
           updated_time=excluded.updated_time, deleted_time=0, revision=notebooks.revision+1",
        params![id, title, stack_id, now],
    )?;
    Ok(Ok(false))
}

fn accept_inflight(
    transaction: &Transaction<'_>,
    op_id: &str,
    kind: &str,
    id: &str,
    revision: u64,
) -> Result<(), LibraryError> {
    let outbox: String = transaction.query_row(
        "SELECT outbox_ids_json FROM sync_inflight WHERE op_id=?1",
        [op_id],
        |row| row.get(0),
    )?;
    let outbox: Vec<String> =
        serde_json::from_str(&outbox).map_err(|_| LibraryError::InvalidSnapshot)?;
    delete_outbox(transaction, &outbox)?;
    transaction.execute("DELETE FROM sync_inflight WHERE op_id=?1", [op_id])?;
    transaction.execute("DELETE FROM sync_failures WHERE op_id=?1", [op_id])?;
    transaction.execute(
        "INSERT INTO sync_entities(entity_type,entity_id,server_revision) VALUES(?1,?2,?3)
         ON CONFLICT(entity_type,entity_id) DO UPDATE SET server_revision=excluded.server_revision",
        params![kind, id, revision as i64],
    )?;
    Ok(())
}

fn drop_pending(transaction: &Transaction<'_>, kind: &str, id: &str) -> Result<(), LibraryError> {
    transaction.execute(
        "DELETE FROM sync_failures WHERE op_id IN (SELECT op_id FROM sync_inflight WHERE entity_type=?1 AND entity_id=?2)",
        params![kind, id],
    )?;
    transaction.execute(
        "DELETE FROM sync_inflight WHERE entity_type=?1 AND entity_id=?2",
        params![kind, id],
    )?;
    transaction.execute(
        "DELETE FROM sync_outbox WHERE entity_type=?1 AND entity_id=?2",
        params![kind, id],
    )?;
    Ok(())
}

/// Validated attachment metadata from a remote payload.
pub struct RemoteResourceRef {
    pub(crate) title: String,
    pub(crate) mime: String,
    pub(crate) file_extension: String,
    pub(crate) size: usize,
    pub(crate) sha256: crate::BlobHash,
}

impl RemoteResourceRef {
    pub(crate) fn parse(payload: &Value) -> Option<Self> {
        let text = |key: &str| payload.get(key)?.as_str().map(str::to_owned);
        Some(Self {
            title: text("title")?,
            mime: text("mime")?,
            file_extension: text("file_extension")?,
            size: usize::try_from(payload.get("size")?.as_u64()?).ok()?,
            sha256: crate::BlobHash::new(text("sha256")?).ok()?,
        })
    }
}

fn local_row_exists(
    transaction: &Transaction<'_>,
    kind: &str,
    id: &str,
) -> Result<bool, LibraryError> {
    let table = match kind {
        "note" => "notes",
        "notebook" => "notebooks",
        "stack" => "stacks",
        "tag" => "tags",
        _ => "resources",
    };
    Ok(transaction.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id=?1)"),
        [id],
        |row| row.get::<_, i64>(0),
    )? != 0)
}

impl LibraryRepository {
    /// A restored library is a new device: it gets a new identity, never
    /// resends the old device's queued ops, and re-uploads the current state
    /// of anything that had unconfirmed changes (conflict copies keep them
    /// if the server moved on meanwhile).
    pub(crate) fn sync_reset_identity(&self) -> Result<(), LibraryError> {
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(
            "DELETE FROM sync_entities WHERE EXISTS(
                 SELECT 1 FROM sync_outbox o WHERE o.entity_type=sync_entities.entity_type AND o.entity_id=sync_entities.entity_id
                 UNION ALL
                 SELECT 1 FROM sync_inflight i WHERE i.entity_type=sync_entities.entity_type AND i.entity_id=sync_entities.entity_id);
             DELETE FROM sync_inflight;
             DELETE FROM sync_failures;
             DELETE FROM sync_outbox;
             DELETE FROM sync_cursor;",
        )?;
        transaction.execute("DELETE FROM settings WHERE key=?1", [DEVICE_ID_SETTING])?;
        transaction.commit()?;
        Ok(())
    }
}
