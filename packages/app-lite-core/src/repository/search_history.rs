use super::*;

const HISTORY_LIMIT: usize = 128;
const QUERY_BYTE_LIMIT: usize = 4096;

/// Local search preferences. No note bodies, resource bytes or sync operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentSearch {
    pub query: String,
    pub last_used_time: i64,
    pub use_count: i64,
}

impl LibraryRepository {
    pub fn record_search(&self, query: &str) -> Result<(), LibraryError> {
        let query = query.trim();
        if query.is_empty() { return Ok(()); }
        if query.len() > QUERY_BYTE_LIMIT { return Err(LibraryError::InvalidSnapshot); }
        let mut connection = self.connection.lock().expect("library mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO search_history(query,last_used_time,use_count) VALUES(?1,?2,1)
             ON CONFLICT(query) DO UPDATE SET last_used_time=excluded.last_used_time,
             use_count=CASE WHEN search_history.use_count<9223372036854775807
             THEN search_history.use_count+1 ELSE search_history.use_count END",
            params![query, self.now()],
        )?;
        transaction.execute(
            "DELETE FROM search_history WHERE query NOT IN
             (SELECT query FROM search_history ORDER BY last_used_time DESC,query ASC LIMIT 128)",
            [],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn list_recent_searches(&self, prefix: &str, limit: usize) -> Result<Vec<RecentSearch>, LibraryError> {
        if limit == 0 { return Ok(Vec::new()); }
        if prefix.len() > QUERY_BYTE_LIMIT { return Err(LibraryError::InvalidSnapshot); }
        // Query text is literal, not a user-supplied LIKE pattern.
        let pattern = format!("{}%", prefix.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        let connection = self.connection.lock().expect("library mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT query,last_used_time,use_count FROM search_history
             WHERE query LIKE ?1 ESCAPE '\\' COLLATE NOCASE
             ORDER BY last_used_time DESC,query ASC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![pattern, limit.min(HISTORY_LIMIT) as i64], |row| {
            Ok(RecentSearch { query: row.get(0)?, last_used_time: row.get(1)?, use_count: row.get(2)? })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Remove the query and strict prefixes left by older suggestion writers.
    /// SQLite length/substr count Unicode characters; never slice UTF-8 bytes.
    pub fn delete_search_history(&self, query: &str) -> Result<(), LibraryError> {
        if query.is_empty() { return Ok(()); }
        if query.len() > QUERY_BYTE_LIMIT { return Err(LibraryError::InvalidSnapshot); }
        let connection = self.connection.lock().expect("library mutex poisoned");
        connection.execute(
            "DELETE FROM search_history WHERE query=?1 OR
             (length(query)<length(?1) AND substr(?1,1,length(query))=query)", [query],
        )?;
        Ok(())
    }

    pub fn clear_search_history(&self) -> Result<(), LibraryError> {
        let connection = self.connection.lock().expect("library mutex poisoned");
        connection.execute("DELETE FROM search_history", [])?;
        Ok(())
    }
}
