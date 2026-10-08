use super::*;
use crate::{LibraryShortcut, ShortcutTarget};

impl LibraryRepository {
    /// Local navigation preferences, not note copies or acknowledged sync ops.
    /// The immediate transaction serializes membership/order with other writers.
    pub fn add_shortcuts(&self, targets: &[ShortcutTarget]) -> Result<(), LibraryError> {
        if targets.is_empty() {
            return Ok(());
        }
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut next_position: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(position), -1) FROM shortcuts",
            [],
            |row| row.get(0),
        )?;
        let now = self.now();
        let mut changed = false;
        for target in targets.iter().collect::<BTreeSet<_>>() {
            // Validation precedes duplicate detection: a trashed note is never
            // advertised as a successful add, even if an old membership exists.
            let (_, id, table) = target_columns(target);
            let exists: bool = transaction.query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id=?1 AND deleted_time=0)"),
                [id],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(LibraryError::NotFound);
            }
        }
        // Keep the user's input order; the set above is only validation.
        for target in targets {
            let (kind, id, _) = target_columns(target);
            let exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM shortcuts WHERE entity_type=?1 AND entity_id=?2)",
                params![kind, id],
                |row| row.get(0),
            )?;
            if exists {
                continue;
            }
            next_position = next_position
                .checked_add(1)
                .ok_or(LibraryError::InvalidSnapshot)?;
            self.insert_with_unique_id(&transaction, "shortcuts", |shortcut_id| {
                transaction.execute(
                    "INSERT INTO shortcuts(id,entity_type,entity_id,position,created_time) VALUES(?1,?2,?3,?4,?5)",
                    params![shortcut_id, kind, id, next_position, now],
                )
            })?;
            changed = true;
        }
        transaction.commit()?;
        drop(connection);
        if changed {
            self.publish(vec![LibraryEvent::OrganizationChanged]);
        }
        Ok(())
    }

    /// Removing a shortcut never deletes the target or its content. This also
    /// permits removing membership whose target has since been soft-deleted.
    pub fn remove_shortcuts(&self, targets: &[ShortcutTarget]) -> Result<(), LibraryError> {
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut changed = false;
        for target in targets {
            let (kind, id, _) = target_columns(target);
            changed |= transaction.execute(
                "DELETE FROM shortcuts WHERE entity_type=?1 AND entity_id=?2",
                params![kind, id],
            )? != 0;
        }
        transaction.commit()?;
        drop(connection);
        if changed {
            self.publish(vec![LibraryEvent::OrganizationChanged]);
        }
        Ok(())
    }

    pub fn list_shortcuts(&self) -> Result<Vec<LibraryShortcut>, LibraryError> {
        let connection = self.connection.lock().expect("library mutex poisoned");
        list_shortcuts_on(&connection)
    }
}

fn target_columns(target: &ShortcutTarget) -> (&'static str, &str, &'static str) {
    match target {
        ShortcutTarget::Note(id) => ("note", id.as_str(), "notes"),
        ShortcutTarget::Notebook(id) => ("notebook", id.as_str(), "notebooks"),
        ShortcutTarget::Stack(id) => ("stack", id.as_str(), "stacks"),
        ShortcutTarget::Tag(id) => ("tag", id.as_str(), "tags"),
    }
}

/// One SQL snapshot resolves live titles, retaining identity and original
/// membership order. No body, snippet, resource or merge-state is hydrated.
pub(super) fn list_shortcuts_on(
    connection: &Connection,
) -> Result<Vec<LibraryShortcut>, LibraryError> {
    let mut statement = connection.prepare(
        "SELECT s.id,s.entity_type,s.entity_id,s.position,
                CASE s.entity_type WHEN 'note' THEN n.title WHEN 'notebook' THEN b.title
                  WHEN 'stack' THEN g.title WHEN 'tag' THEN t.title END AS shortcut_title
         FROM shortcuts s
         LEFT JOIN notes n ON s.entity_type='note' AND n.id=s.entity_id AND n.deleted_time=0
         LEFT JOIN notebooks b ON s.entity_type='notebook' AND b.id=s.entity_id AND b.deleted_time=0
         LEFT JOIN stacks g ON s.entity_type='stack' AND g.id=s.entity_id AND g.deleted_time=0
         LEFT JOIN tags t ON s.entity_type='tag' AND t.id=s.entity_id AND t.deleted_time=0
         WHERE shortcut_title IS NOT NULL ORDER BY s.position,s.id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    rows.map(|row| {
        let (id, kind, target_id, position, title) = row?;
        let target = match kind.as_str() {
            "note" => {
                ShortcutTarget::Note(NoteId::parse(target_id).map_err(|_| LibraryError::InvalidId)?)
            }
            "notebook" => ShortcutTarget::Notebook(
                NotebookId::parse(target_id).map_err(|_| LibraryError::InvalidId)?,
            ),
            "stack" => ShortcutTarget::Stack(
                StackId::parse(target_id).map_err(|_| LibraryError::InvalidId)?,
            ),
            "tag" => {
                ShortcutTarget::Tag(TagId::parse(target_id).map_err(|_| LibraryError::InvalidId)?)
            }
            _ => return Err(LibraryError::InvalidId),
        };
        Ok(LibraryShortcut {
            id,
            target,
            title,
            position,
        })
    })
    .collect()
}
