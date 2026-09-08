use crate::db::{DbError, DbResult};
use crate::models::{NewSkill, Skill};
use rusqlite::params;

use super::Repository;

/// Map a `skills` row to a `Skill`. Column order matches `SKILL_COLUMNS`.
fn skill_from_row(row: &rusqlite::Row) -> rusqlite::Result<Skill> {
    let triggers_json: Option<String> = row.get(4)?;
    let triggers = triggers_json
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    Ok(Skill {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        body: row.get(3)?,
        triggers,
        scope: row.get(5)?,
        source: row.get(6)?,
        status: row.get(7)?,
        version: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

const SKILL_COLUMNS: &str =
    "id, name, description, body, triggers, scope, source, status, version, created_at, updated_at";

impl Repository {
    /// Create a new skill. Fails with a `UNIQUE` constraint violation if a
    /// skill with the same `(name, scope)` already exists — callers should
    /// use [`Repository::update_skill`] to edit an existing one.
    pub fn create_skill(&self, new: &NewSkill) -> DbResult<Skill> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().timestamp_millis();
        let triggers_json = serde_json::to_string(&new.triggers).unwrap_or_else(|_| "[]".into());

        self.conn.execute(
            "INSERT INTO skills (id, name, description, body, triggers, scope, source, status, version, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9, ?9)",
            params![
                id,
                new.name,
                new.description,
                new.body,
                triggers_json,
                new.scope,
                new.source,
                new.status,
                now
            ],
        )?;

        Ok(Skill {
            id,
            name: new.name.clone(),
            description: new.description.clone(),
            body: new.body.clone(),
            triggers: new.triggers.clone(),
            scope: new.scope.clone(),
            source: new.source.clone(),
            status: new.status.clone(),
            version: 1,
            created_at: now,
            updated_at: now,
        })
    }

    /// Partially update an existing skill's content. `None` fields are left
    /// unchanged. Bumps `version` and `updated_at`. Returns `None` if no
    /// skill matches `(name, scope)`.
    pub fn update_skill(
        &self,
        name: &str,
        scope: &str,
        description: Option<&str>,
        body: Option<&str>,
        triggers: Option<&[String]>,
    ) -> DbResult<Option<Skill>> {
        let Some(existing) = self.get_skill(name, scope)? else {
            return Ok(None);
        };

        let new_description = description.unwrap_or(&existing.description);
        let new_body = body.unwrap_or(&existing.body);
        let new_triggers = triggers.unwrap_or(&existing.triggers);
        let triggers_json = serde_json::to_string(new_triggers).unwrap_or_else(|_| "[]".into());
        let now = chrono::Utc::now().timestamp_millis();

        self.conn.execute(
            "UPDATE skills SET description = ?1, body = ?2, triggers = ?3, version = version + 1, updated_at = ?4
             WHERE name = ?5 AND scope = ?6",
            params![new_description, new_body, triggers_json, now, name, scope],
        )?;

        self.get_skill(name, scope)
    }

    /// Set a skill's `status` (e.g. approving or archiving it). Returns
    /// `None` if no skill matches `(name, scope)`.
    pub fn set_skill_status(
        &self,
        name: &str,
        scope: &str,
        status: &str,
    ) -> DbResult<Option<Skill>> {
        let now = chrono::Utc::now().timestamp_millis();
        let changed = self.conn.execute(
            "UPDATE skills SET status = ?1, updated_at = ?2 WHERE name = ?3 AND scope = ?4",
            params![status, now, name, scope],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        self.get_skill(name, scope)
    }

    /// Exact `(name, scope)` lookup.
    pub fn get_skill(&self, name: &str, scope: &str) -> DbResult<Option<Skill>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {SKILL_COLUMNS} FROM skills WHERE name = ?1 AND scope = ?2"
        ))?;
        let mut rows = stmt.query(params![name, scope])?;
        rows.next()?
            .map(|r| skill_from_row(r))
            .transpose()
            .map_err(DbError::from)
    }

    /// Resolve a skill by name without requiring an exact scope. Tries, in
    /// order: `scope_hint` (if given), the global scope, then any scope
    /// (most recently updated first). Only considers `active` skills.
    pub fn find_skill(&self, name: &str, scope_hint: Option<&str>) -> DbResult<Option<Skill>> {
        if let Some(scope) = scope_hint {
            if let Some(s) = self.get_skill(name, scope)? {
                if s.status == crate::models::SKILL_STATUS_ACTIVE {
                    return Ok(Some(s));
                }
            }
        }
        if scope_hint != Some(crate::models::SKILL_SCOPE_GLOBAL) {
            if let Some(s) = self.get_skill(name, crate::models::SKILL_SCOPE_GLOBAL)? {
                if s.status == crate::models::SKILL_STATUS_ACTIVE {
                    return Ok(Some(s));
                }
            }
        }
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {SKILL_COLUMNS} FROM skills WHERE name = ?1 AND status = ?2 ORDER BY updated_at DESC LIMIT 1"
        ))?;
        let mut rows = stmt.query(params![name, crate::models::SKILL_STATUS_ACTIVE])?;
        rows.next()?
            .map(|r| skill_from_row(r))
            .transpose()
            .map_err(DbError::from)
    }

    /// List skills, optionally filtered by scope and/or status.
    /// `status_filter = None` returns skills in every status.
    pub fn list_skills(
        &self,
        scope_filter: Option<&str>,
        status_filter: Option<&str>,
    ) -> DbResult<Vec<Skill>> {
        let mut clauses = Vec::new();
        let mut sql_params: Vec<&dyn rusqlite::ToSql> = Vec::new();
        if let Some(scope) = &scope_filter {
            clauses.push("scope = ?");
            sql_params.push(scope);
        }
        if let Some(status) = &status_filter {
            clauses.push("status = ?");
            sql_params.push(status);
        }
        let where_clause = if clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", clauses.join(" AND "))
        };
        let sql = format!(
            "SELECT {SKILL_COLUMNS} FROM skills{where_clause} ORDER BY scope ASC, name ASC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let skills = stmt
            .query_map(sql_params.as_slice(), skill_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(skills)
    }

    /// Substring search across name/description/triggers among `active` skills.
    pub fn search_skills(&self, query: &str, scope_filter: Option<&str>) -> DbResult<Vec<Skill>> {
        let pattern = format!(
            "%{}%",
            query
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let mut clauses = vec![
            "status = ?".to_string(),
            "(name LIKE ? ESCAPE '\\' OR description LIKE ? ESCAPE '\\' OR triggers LIKE ? ESCAPE '\\')"
                .to_string(),
        ];
        let mut sql_params: Vec<&dyn rusqlite::ToSql> = vec![
            &crate::models::SKILL_STATUS_ACTIVE,
            &pattern,
            &pattern,
            &pattern,
        ];
        if let Some(scope) = &scope_filter {
            clauses.push("scope = ?".to_string());
            sql_params.push(scope);
        }
        let sql = format!(
            "SELECT {SKILL_COLUMNS} FROM skills WHERE {} ORDER BY name ASC",
            clauses.join(" AND ")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(sql_params.as_slice(), skill_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Delete a skill by `(name, scope)`. Returns `true` if a row was removed.
    pub fn delete_skill(&self, name: &str, scope: &str) -> DbResult<bool> {
        let count = self.conn.execute(
            "DELETE FROM skills WHERE name = ?1 AND scope = ?2",
            params![name, scope],
        )?;
        Ok(count > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        SKILL_SCOPE_GLOBAL, SKILL_SOURCE_HUMAN, SKILL_STATUS_ACTIVE, SKILL_STATUS_PENDING,
    };
    use crate::test_utils::test_repo;

    fn new_skill(name: &str, scope: &str, status: &str) -> NewSkill {
        NewSkill {
            name: name.to_string(),
            description: format!("{name} description"),
            body: format!("# {name}\n\nDo the thing."),
            triggers: vec!["deploy".into(), "release".into()],
            scope: scope.to_string(),
            source: SKILL_SOURCE_HUMAN.to_string(),
            status: status.to_string(),
        }
    }

    #[test]
    fn create_and_get_skill_roundtrip() {
        let (_dir, repo) = test_repo();
        let created = repo
            .create_skill(&new_skill(
                "deploy-checklist",
                SKILL_SCOPE_GLOBAL,
                SKILL_STATUS_ACTIVE,
            ))
            .unwrap();
        assert_eq!(created.version, 1);
        assert!(!created.id.is_empty());

        let fetched = repo
            .get_skill("deploy-checklist", SKILL_SCOPE_GLOBAL)
            .unwrap()
            .unwrap();
        assert_eq!(fetched.name, "deploy-checklist");
        assert_eq!(fetched.body, "# deploy-checklist\n\nDo the thing.");
        assert_eq!(fetched.triggers, vec!["deploy", "release"]);
        assert_eq!(fetched.status, SKILL_STATUS_ACTIVE);
    }

    #[test]
    fn create_duplicate_name_scope_fails() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&new_skill("dup", SKILL_SCOPE_GLOBAL, SKILL_STATUS_ACTIVE))
            .unwrap();
        let result = repo.create_skill(&new_skill("dup", SKILL_SCOPE_GLOBAL, SKILL_STATUS_ACTIVE));
        assert!(result.is_err());
    }

    #[test]
    fn same_name_different_scope_is_allowed() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&new_skill("dup", SKILL_SCOPE_GLOBAL, SKILL_STATUS_ACTIVE))
            .unwrap();
        let ok = repo.create_skill(&new_skill("dup", "/tmp/project", SKILL_STATUS_ACTIVE));
        assert!(ok.is_ok());
    }

    #[test]
    fn update_skill_bumps_version_and_preserves_unset_fields() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&new_skill("s1", SKILL_SCOPE_GLOBAL, SKILL_STATUS_ACTIVE))
            .unwrap();

        let updated = repo
            .update_skill("s1", SKILL_SCOPE_GLOBAL, None, Some("new body"), None)
            .unwrap()
            .unwrap();
        assert_eq!(updated.version, 2);
        assert_eq!(updated.body, "new body");
        assert_eq!(updated.description, "s1 description"); // unchanged
        assert_eq!(updated.triggers, vec!["deploy", "release"]); // unchanged
    }

    #[test]
    fn update_nonexistent_skill_returns_none() {
        let (_dir, repo) = test_repo();
        let result = repo
            .update_skill("missing", SKILL_SCOPE_GLOBAL, Some("d"), None, None)
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn set_skill_status_transitions() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&new_skill(
            "pending-one",
            SKILL_SCOPE_GLOBAL,
            SKILL_STATUS_PENDING,
        ))
        .unwrap();

        let approved = repo
            .set_skill_status("pending-one", SKILL_SCOPE_GLOBAL, SKILL_STATUS_ACTIVE)
            .unwrap()
            .unwrap();
        assert_eq!(approved.status, SKILL_STATUS_ACTIVE);
        assert_eq!(approved.version, 1); // status change is not a content edit
    }

    #[test]
    fn find_skill_prefers_scope_hint_then_global_then_any() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&new_skill("greet", SKILL_SCOPE_GLOBAL, SKILL_STATUS_ACTIVE))
            .unwrap();
        repo.create_skill(&new_skill("greet", "/proj/a", SKILL_STATUS_ACTIVE))
            .unwrap();

        // Exact scope hint wins.
        let found = repo.find_skill("greet", Some("/proj/a")).unwrap().unwrap();
        assert_eq!(found.scope, "/proj/a");

        // No hint (or a scope with no match) falls back to global.
        let found = repo.find_skill("greet", None).unwrap().unwrap();
        assert_eq!(found.scope, SKILL_SCOPE_GLOBAL);
        let found = repo
            .find_skill("greet", Some("/proj/other"))
            .unwrap()
            .unwrap();
        assert_eq!(found.scope, SKILL_SCOPE_GLOBAL);
    }

    #[test]
    fn find_skill_ignores_non_active() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&new_skill(
            "draft",
            SKILL_SCOPE_GLOBAL,
            SKILL_STATUS_PENDING,
        ))
        .unwrap();
        assert!(repo.find_skill("draft", None).unwrap().is_none());
    }

    #[test]
    fn list_skills_filters_by_scope_and_status() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&new_skill("a", SKILL_SCOPE_GLOBAL, SKILL_STATUS_ACTIVE))
            .unwrap();
        repo.create_skill(&new_skill("b", "/proj", SKILL_STATUS_ACTIVE))
            .unwrap();
        repo.create_skill(&new_skill("c", SKILL_SCOPE_GLOBAL, SKILL_STATUS_PENDING))
            .unwrap();

        assert_eq!(repo.list_skills(None, None).unwrap().len(), 3);
        assert_eq!(
            repo.list_skills(Some(SKILL_SCOPE_GLOBAL), None)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            repo.list_skills(None, Some(SKILL_STATUS_ACTIVE))
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            repo.list_skills(Some(SKILL_SCOPE_GLOBAL), Some(SKILL_STATUS_PENDING))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn search_skills_matches_name_description_and_triggers() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&NewSkill {
            triggers: vec!["release".into()],
            ..new_skill("deploy-checklist", SKILL_SCOPE_GLOBAL, SKILL_STATUS_ACTIVE)
        })
        .unwrap();
        repo.create_skill(&NewSkill {
            triggers: vec!["unit-testing".into()],
            ..new_skill("write-tests", SKILL_SCOPE_GLOBAL, SKILL_STATUS_ACTIVE)
        })
        .unwrap();

        let by_name = repo.search_skills("deploy", None).unwrap();
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].name, "deploy-checklist");

        let by_trigger = repo.search_skills("unit-testing", None).unwrap();
        assert_eq!(by_trigger.len(), 1);
        assert_eq!(by_trigger[0].name, "write-tests");
    }

    #[test]
    fn search_skills_excludes_pending() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&new_skill(
            "draft-skill",
            SKILL_SCOPE_GLOBAL,
            SKILL_STATUS_PENDING,
        ))
        .unwrap();
        assert!(repo.search_skills("draft", None).unwrap().is_empty());
    }

    #[test]
    fn delete_skill_removes_row() {
        let (_dir, repo) = test_repo();
        repo.create_skill(&new_skill("temp", SKILL_SCOPE_GLOBAL, SKILL_STATUS_ACTIVE))
            .unwrap();
        assert!(repo.delete_skill("temp", SKILL_SCOPE_GLOBAL).unwrap());
        assert!(repo
            .get_skill("temp", SKILL_SCOPE_GLOBAL)
            .unwrap()
            .is_none());
        assert!(!repo.delete_skill("temp", SKILL_SCOPE_GLOBAL).unwrap());
    }
}
