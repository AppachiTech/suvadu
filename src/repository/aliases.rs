use crate::db::DbResult;
use crate::models::Alias;
use rusqlite::params;

use super::Repository;

impl Repository {
    /// Add or update an alias. On name conflict, updates the command.
    pub fn add_alias(&self, name: &str, command: &str) -> DbResult<i64> {
        let now = chrono::Utc::now().timestamp_millis();
        self.conn.execute(
            "INSERT INTO aliases (name, command, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(name) DO UPDATE SET command = excluded.command",
            params![name, command, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Remove an alias by name.
    pub fn remove_alias(&self, name: &str) -> DbResult<bool> {
        let count = self
            .conn
            .execute("DELETE FROM aliases WHERE name = ?1", params![name])?;
        Ok(count > 0)
    }

    /// List all aliases ordered by name.
    pub fn list_aliases(&self) -> DbResult<Vec<Alias>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, command, created_at FROM aliases ORDER BY name")?;
        let rows = stmt.query_map([], |row| {
            Ok(Alias {
                id: row.get(0)?,
                name: row.get(1)?,
                command: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;
        let mut aliases = Vec::new();
        for row in rows {
            aliases.push(row?);
        }
        Ok(aliases)
    }
}
