use crate::db::DbResult;
use crate::models::Bookmark;
use rusqlite::params;

use super::Repository;

impl Repository {
    /// Add or update a bookmark. Preserves `created_at` if the command is already bookmarked.
    pub fn add_bookmark(&self, command: &str, label: Option<&str>) -> DbResult<i64> {
        let now = chrono::Utc::now().timestamp_millis();
        self.conn.execute(
            "INSERT INTO bookmarks (command, label, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(command) DO UPDATE SET label = excluded.label",
            params![command, label, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Remove a bookmark by command text
    pub fn remove_bookmark(&self, command: &str) -> DbResult<bool> {
        let count = self
            .conn
            .execute("DELETE FROM bookmarks WHERE command = ?1", params![command])?;
        Ok(count > 0)
    }

    /// List all bookmarks ordered by most recent first
    pub fn list_bookmarks(&self) -> DbResult<Vec<Bookmark>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, command, label, created_at FROM bookmarks ORDER BY id DESC")?;
        let rows = stmt.query_map([], |row| {
            Ok(Bookmark {
                id: row.get(0)?,
                command: row.get(1)?,
                label: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;
        let mut bookmarks = Vec::new();
        for row in rows {
            bookmarks.push(row?);
        }
        Ok(bookmarks)
    }

    /// Get all bookmarked command strings as a set
    pub fn get_bookmarked_commands(&self) -> DbResult<std::collections::HashSet<String>> {
        let mut stmt = self.conn.prepare("SELECT command FROM bookmarks")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut set = std::collections::HashSet::new();
        for row in rows {
            set.insert(row?);
        }
        Ok(set)
    }
}
