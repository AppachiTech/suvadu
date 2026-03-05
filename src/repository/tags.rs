use crate::db::DbResult;
use rusqlite::params;

use super::Repository;

impl Repository {
    /// Create a new tag
    pub fn create_tag(&self, name: &str, description: Option<&str>) -> DbResult<i64> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM tags", [], |row| row.get(0))?;

        if count >= 20 {
            return Err(crate::db::DbError::Validation(
                "Maximum number of tags (20) reached".into(),
            ));
        }

        let name_lower = name.to_lowercase();

        self.conn.execute(
            "INSERT INTO tags (name, description) VALUES (?1, ?2)",
            params![name_lower, description],
        )?;

        Ok(self.conn.last_insert_rowid())
    }

    /// Get all tags
    pub fn get_tags(&self) -> DbResult<Vec<crate::models::Tag>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, description FROM tags ORDER BY name ASC")?;
        let tags = stmt
            .query_map([], |row| {
                Ok(crate::models::Tag {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(tags)
    }

    /// Update a tag
    pub fn update_tag(&self, id: i64, name: &str, description: Option<&str>) -> DbResult<()> {
        let name_lower = name.to_lowercase();
        self.conn.execute(
            "UPDATE tags SET name = ?1, description = ?2 WHERE id = ?3",
            params![name_lower, description, id],
        )?;
        Ok(())
    }

    /// Associate a session with a tag (or clear if `tag_id` is None)
    pub fn tag_session(&self, session_id: &str, tag_id: Option<i64>) -> DbResult<()> {
        self.conn.execute(
            "UPDATE sessions SET tag_id = ?1 WHERE id = ?2",
            params![tag_id, session_id],
        )?;
        Ok(())
    }

    /// Get the tag name associated with a session
    pub fn get_tag_by_session(&self, session_id: &str) -> DbResult<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.name
             FROM sessions s
             JOIN tags t ON s.tag_id = t.id
             WHERE s.id = ?1",
        )?;

        let mut rows = stmt.query(params![session_id])?;

        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }
}
