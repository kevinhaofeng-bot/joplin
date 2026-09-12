use crate::{LibraryError, LibraryRepository};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

/// Drains at most one bounded worker batch.  A failed document leaves its job
/// untouched, so reopening the repository can retry without losing authority.
pub fn process_search_jobs(repository: &LibraryRepository) -> Result<usize, LibraryError> {
    if let Some(error) = repository.take_search_job_failure() {
        return Err(error);
    }
    let jobs = repository.take_search_jobs(100)?;
    let mut completed = 0;
    for job in jobs {
        let mut connection = repository.search_connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let source: Option<(String, String)> = transaction
            .query_row(
                "SELECT title, body_text FROM notes WHERE id=?1",
                [job.note_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        transaction.execute(
            "INSERT OR IGNORE INTO search_index_rows (note_id) VALUES (?1)",
            [job.note_id.as_str()],
        )?;
        let rowid: i64 = transaction.query_row(
            "SELECT fts_rowid FROM search_index_rows WHERE note_id=?1",
            [job.note_id.as_str()],
            |row| row.get(0),
        )?;
        transaction.execute("DELETE FROM search_unicode WHERE rowid=?1", [rowid])?;
        transaction.execute("DELETE FROM search_trigram WHERE rowid=?1", [rowid])?;
        if let Some((title, body)) = source {
            transaction.execute(
                "INSERT INTO search_unicode (rowid,note_id,title,body) VALUES (?1,?2,?3,?4)",
                params![rowid, job.note_id.as_str(), title, body],
            )?;
            transaction.execute(
                "INSERT INTO search_trigram (rowid,note_id,title,body) VALUES (?1,?2,?3,?4)",
                params![rowid, job.note_id.as_str(), title, body],
            )?;
        } else {
            transaction.execute(
                "DELETE FROM search_index_rows WHERE note_id=?1",
                [job.note_id.as_str()],
            )?;
        }
        transaction.execute(
            "DELETE FROM search_queue WHERE note_id=?1 AND updated_time=?2 AND reason=?3",
            params![job.note_id.as_str(), job.updated_time, job.reason],
        )?;
        transaction.commit()?;
        completed += 1;
    }
    Ok(completed)
}
