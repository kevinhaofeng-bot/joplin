use super::*;

const RECENT_NOTES_KEY: &str = "library-recent-note-ids";
const RECENT_NOTES_LIMIT: usize = 16;

fn recent_ids(connection: &Connection) -> Result<Vec<String>, LibraryError> {
    let value: Option<String> = connection.query_row(
        "SELECT value FROM settings WHERE key=?1", [RECENT_NOTES_KEY], |r| r.get(0),
    ).optional()?;
    let Some(value) = value else { return Ok(Vec::new()); };
    if value.len() > 4096 { return Err(LibraryError::InvalidSnapshot); }
    let ids: Vec<String> = serde_json::from_str(&value).map_err(|_| LibraryError::InvalidSnapshot)?;
    let mut seen = BTreeSet::new();
    Ok(ids.into_iter().filter(|id| NoteId::parse(id).is_ok() && seen.insert(id.clone()))
        .take(RECENT_NOTES_LIMIT).collect())
}

/// Local navigation preference, committed with selection and pane settings.
/// Reading/hovering a card never enters MRU; deleted and nonexistent IDs do not.
pub(super) fn record_selected_note(transaction: &Transaction<'_>, id: &NoteId, now: i64) -> Result<(), LibraryError> {
    let live: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM notes WHERE id=?1 AND deleted_time=0)", [id.as_str()], |r| r.get(0),
    )?;
    if !live { return Ok(()); }
    let mut ids = match recent_ids(transaction) {
        // A corrupt local preference must not brick the existing navigation
        // path. It is rebuilt from the next successfully selected live note.
        Err(LibraryError::InvalidSnapshot) => Vec::new(),
        result => result?,
    };
    ids.retain(|old| old != id.as_str());
    ids.insert(0, id.as_str().to_owned());
    ids.truncate(RECENT_NOTES_LIMIT);
    let value = serde_json::to_string(&ids).map_err(|_| LibraryError::InvalidSnapshot)?;
    transaction.execute(
        "INSERT INTO settings(key,value,updated_time) VALUES(?1,?2,?3)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_time=excluded.updated_time
         WHERE settings.value<>excluded.value",
        params![RECENT_NOTES_KEY, value, now],
    )?;
    Ok(())
}

impl LibraryRepository {
    /// At most sixteen metadata-only projections in actual selection order.
    /// Never joins note bodies, histories, resource bytes or merge state.
    pub fn list_recent_notes(&self, limit: usize) -> Result<Vec<NoteProjection>, LibraryError> {
        if limit == 0 { return Ok(Vec::new()); }
        let connection = self.connection.lock().expect("library mutex poisoned");
        let ids = recent_ids(&connection)?;
        let encoded = serde_json::to_string(&ids).map_err(|_| LibraryError::InvalidSnapshot)?;
        let mut statement = connection.prepare(
            "SELECT n.id,substr(n.title,1,120),substr(n.snippet,1,160),n.updated_time,
             n.deleted_time,n.notebook_id,
             COALESCE((SELECT n.selected_thumbnail_id WHERE EXISTS
               (SELECT 1 FROM note_resources nr JOIN resources r ON r.id=nr.resource_id
                WHERE nr.note_id=n.id AND nr.resource_id=n.selected_thumbnail_id
                  AND nr.is_associated=1 AND r.deleted_time=0 AND r.mime LIKE 'image/%')),
               (SELECT nr.resource_id FROM note_resources nr JOIN resources r ON r.id=nr.resource_id
                WHERE nr.note_id=n.id AND nr.is_associated=1 AND r.deleted_time=0
                  AND r.mime IN ('image/png','image/jpeg') ORDER BY nr.position,nr.resource_id LIMIT 1)),
             (SELECT count(*) FROM note_resources nr WHERE nr.note_id=n.id AND nr.is_associated=1)
             FROM json_each(?1) recent JOIN notes n ON n.id=recent.value
             WHERE n.deleted_time=0 ORDER BY CAST(recent.key AS INTEGER) LIMIT ?2",
        )?;
        let rows = statement.query_map(params![encoded, limit.min(RECENT_NOTES_LIMIT) as i64], row_to_projection)?;
        Ok(rows.collect::<Result<Vec<_>,_>>()?)
    }
}
