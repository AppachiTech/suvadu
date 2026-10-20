//! Which repo skill files (`<root>/.suvadu/skills/*.md`) the user trusted,
//! and at what content. Only the hash is stored, never the file's text.

use crate::db::DbResult;
use rusqlite::params;
use std::collections::HashMap;

use super::Repository;

impl Repository {
    /// Record that the user trusts the repo skill file at `path` (relative
    /// to the project `root`) with content hash `hash`, replacing any
    /// earlier trust for that file.
    pub fn trust_repo_skill(&self, root: &str, path: &str, hash: &str) -> DbResult<()> {
        let now = chrono::Utc::now().timestamp_millis();
        self.conn.execute(
            "INSERT INTO skill_trust (root, path, content_hash, trusted_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(root, path) DO UPDATE SET content_hash = excluded.content_hash,
                                                   trusted_at = excluded.trusted_at",
            params![root, path, hash, now],
        )?;
        Ok(())
    }

    /// Trusted content hash per file path for one project root.
    pub fn repo_skill_trust(&self, root: &str) -> DbResult<HashMap<String, String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT path, content_hash FROM skill_trust WHERE root = ?1")?;
        let rows = stmt.query_map(params![root], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (path, hash) = row?;
            out.insert(path, hash);
        }
        Ok(out)
    }

    /// Forget the trust recorded for one file under `root`, or for every
    /// file under it when `path` is `None`. Returns how many were removed.
    pub fn untrust_repo_skills(&self, root: &str, path: Option<&str>) -> DbResult<usize> {
        let removed = match path {
            Some(path) => self.conn.execute(
                "DELETE FROM skill_trust WHERE root = ?1 AND path = ?2",
                params![root, path],
            )?,
            None => self
                .conn
                .execute("DELETE FROM skill_trust WHERE root = ?1", params![root])?,
        };
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use crate::test_utils::test_repo;

    #[test]
    fn trust_is_recorded_per_root_and_path_and_can_be_replaced_or_removed() {
        let (_dir, repo) = test_repo();
        repo.trust_repo_skill("/r", ".suvadu/skills/a.md", "h1")
            .unwrap();
        repo.trust_repo_skill("/r", ".suvadu/skills/b.md", "h2")
            .unwrap();
        repo.trust_repo_skill("/other", ".suvadu/skills/a.md", "h9")
            .unwrap();
        repo.trust_repo_skill("/r", ".suvadu/skills/a.md", "h1-new")
            .unwrap();

        let trusted = repo.repo_skill_trust("/r").unwrap();
        assert_eq!(trusted.len(), 2);
        assert_eq!(trusted[".suvadu/skills/a.md"], "h1-new");

        assert_eq!(
            repo.untrust_repo_skills("/r", Some(".suvadu/skills/a.md"))
                .unwrap(),
            1
        );
        assert_eq!(repo.untrust_repo_skills("/r", None).unwrap(), 1);
        assert!(repo.repo_skill_trust("/r").unwrap().is_empty());
        assert_eq!(repo.repo_skill_trust("/other").unwrap().len(), 1);
    }
}
