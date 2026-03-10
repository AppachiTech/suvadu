use crate::db::DbResult;
use crate::models::Note;
use rusqlite::params;

use super::Repository;

impl Repository {
    /// Upsert a note for a history entry
    pub fn upsert_note(&self, entry_id: i64, note: &str) -> DbResult<()> {
        let now = chrono::Utc::now().timestamp_millis();
        self.conn.execute(
            "INSERT INTO notes (entry_id, note, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(entry_id) DO UPDATE SET note = excluded.note, updated_at = excluded.updated_at",
            params![entry_id, note, now],
        )?;
        Ok(())
    }

    /// Get a note for a history entry
    pub fn get_note(&self, entry_id: i64) -> DbResult<Option<Note>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, entry_id, note, created_at, updated_at FROM notes WHERE entry_id = ?1",
        )?;
        let mut rows = stmt.query_map(params![entry_id], |row| {
            Ok(Note {
                id: row.get(0)?,
                entry_id: row.get(1)?,
                content: row.get(2)?,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
            })
        })?;
        match rows.next() {
            Some(Ok(note)) => Ok(Some(note)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Delete a note for a history entry
    pub fn delete_note(&self, entry_id: i64) -> DbResult<bool> {
        let count = self
            .conn
            .execute("DELETE FROM notes WHERE entry_id = ?1", params![entry_id])?;
        Ok(count > 0)
    }

    /// Get all entry IDs that have notes
    pub fn get_noted_entry_ids(&self) -> DbResult<std::collections::HashSet<i64>> {
        let mut stmt = self.conn.prepare("SELECT entry_id FROM notes")?;
        let rows = stmt.query_map([], |row| row.get::<_, i64>(0))?;
        let mut set = std::collections::HashSet::new();
        for row in rows {
            set.insert(row?);
        }
        Ok(set)
    }
}
