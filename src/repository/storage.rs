//! Where the disk goes: storage usage by category, and what retention rule
//! governs each one.

use std::path::Path;

use crate::db::DbResult;

use super::Repository;

/// Category names, in report order.
const COMMANDS: &str = "Commands";
const SESSIONS: &str = "Sessions";
const SUMMARIES: &str = "Summaries";
const SKILLS: &str = "Skills";
const OTHER: &str = "Other";

/// One reportable storage category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryUsage {
    pub name: &'static str,
    pub rows: i64,
    pub bytes: u64,
    /// How this category's data goes away, in one line.
    pub retention: &'static str,
}

/// Backup files sitting next to the database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupUsage {
    pub files: usize,
    pub bytes: u64,
}

/// A whole-database storage picture.
#[derive(Debug, Clone)]
pub struct StorageUsage {
    pub categories: Vec<CategoryUsage>,
    pub database_bytes: u64,
    /// Pages freed by deletes that the file still holds onto.
    pub reclaimable_bytes: u64,
    pub backups: BackupUsage,
}

/// Which category a table (or one of its indexes) belongs to.
///
/// Tags, notes and bookmarks annotate commands and disappear with them, so
/// they are reported as part of Commands rather than as categories a user
/// would have to reason about separately.
fn category_of(table: &str) -> &'static str {
    match table {
        "entries" | "notes" | "bookmarks" | "aliases" | "tags" => COMMANDS,
        "sessions" | "ai_sessions" | "ai_events" | "ai_sources" => SESSIONS,
        "ai_summaries" => SUMMARIES,
        "skills" => SKILLS,
        // The FTS5 index and its shadow tables are the cost of searching
        // commands, so they are charged to commands.
        other if other.starts_with("entries_fts") => COMMANDS,
        _ => OTHER,
    }
}

/// What makes each category go away. Kept next to the numbers so the report
/// never shows a size without saying how a user reduces it.
fn retention_of(category: &str) -> &'static str {
    match category {
        COMMANDS => {
            "kept until you delete them (suv delete <pattern>); a pre-delete backup keeps a copy"
        }
        SESSIONS => {
            "shell sessions follow their commands; agent sessions go with suv agent delete-session"
        }
        SUMMARIES => "deleted with their agent session; new commands only mark them stale",
        SKILLS => "kept until suv skills remove; a sync rewrites agent-side copies, not this row",
        _ => "tags, schema bookkeeping and SQLite's own overhead",
    }
}

impl StorageUsage {
    /// Human-readable report lines, one category per line, followed by the
    /// file-level totals and the retention caveats.
    pub fn report_lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = self
            .categories
            .iter()
            .map(|c| {
                // "Other" is overhead, not records: a row count there would
                // invite the question of which rows it means.
                let rows = if c.name == OTHER {
                    String::new()
                } else {
                    format!("{} rows", crate::util::format_count(c.rows))
                };
                format!(
                    "{:<11} {:>9}  {:>12}  {}",
                    c.name,
                    crate::util::human_bytes(c.bytes),
                    rows,
                    c.retention
                )
            })
            .collect();
        lines.push(format!(
            "{:<11} {:>9}  {} reclaimable by VACUUM (deleted rows keep their pages until then)",
            "Database",
            crate::util::human_bytes(self.database_bytes),
            crate::util::human_bytes(self.reclaimable_bytes),
        ));
        lines.push(format!(
            "{:<11} {:>9}  {} file(s), never pruned automatically \u{2014} a backup taken before a delete still holds the deleted commands",
            "Backups",
            crate::util::human_bytes(self.backups.bytes),
            self.backups.files,
        ));
        lines
    }
}

/// Total size and file count of `*.db` backups in `dir`. A missing or
/// unreadable directory reports zero rather than failing the whole report.
fn backup_usage(dir: &Path) -> BackupUsage {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return BackupUsage::default();
    };
    let mut usage = BackupUsage::default();
    for entry in entries.flatten() {
        if entry.path().extension().is_some_and(|ext| ext == "db") {
            if let Ok(meta) = entry.metadata() {
                usage.files += 1;
                usage.bytes += meta.len();
            }
        }
    }
    usage
}

impl Repository {
    /// Storage usage by category, plus the backup directory if one is given.
    ///
    /// Byte counts come from `dbstat`, so they are the pages each table and
    /// its indexes actually occupy — not a guess from row text lengths — and
    /// they exclude pages already freed by deletes, which are reported
    /// separately as `reclaimable_bytes`.
    pub fn storage_usage(&self, backup_dir: Option<&Path>) -> DbResult<StorageUsage> {
        let page_size: u64 = self
            .conn
            .query_row("PRAGMA page_size", [], |row| row.get::<_, i64>(0))?
            .unsigned_abs();
        let page_count: u64 = self
            .conn
            .query_row("PRAGMA page_count", [], |row| row.get::<_, i64>(0))?
            .unsigned_abs();
        let freelist: u64 = self
            .conn
            .query_row("PRAGMA freelist_count", [], |row| row.get::<_, i64>(0))?
            .unsigned_abs();

        // dbstat reports one row per table or index; sqlite_master maps an
        // index back to the table it belongs to, so an index is charged to
        // the data it indexes.
        let mut bytes: std::collections::HashMap<&'static str, u64> =
            std::collections::HashMap::new();
        let mut stmt = self.conn.prepare(
            "SELECT COALESCE(m.tbl_name, d.name) AS owner, SUM(d.pgsize)
             FROM dbstat d LEFT JOIN sqlite_master m ON m.name = d.name
             GROUP BY owner",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (owner, size) = row?;
            *bytes.entry(category_of(&owner)).or_default() += size.unsigned_abs();
        }

        let count =
            |sql: &str| -> DbResult<i64> { Ok(self.conn.query_row(sql, [], |row| row.get(0))?) };
        let rows_by_category = [
            (COMMANDS, count("SELECT COUNT(*) FROM entries")?),
            (
                SESSIONS,
                count("SELECT COUNT(*) FROM sessions")?
                    + count("SELECT COUNT(*) FROM ai_sessions")?,
            ),
            (SUMMARIES, count("SELECT COUNT(*) FROM ai_summaries")?),
            (SKILLS, count("SELECT COUNT(*) FROM skills")?),
            (OTHER, 0),
        ];

        let categories = rows_by_category
            .into_iter()
            .map(|(name, rows)| CategoryUsage {
                name,
                rows,
                bytes: bytes.get(name).copied().unwrap_or_default(),
                retention: retention_of(name),
            })
            .collect();

        Ok(StorageUsage {
            categories,
            database_bytes: page_size * page_count,
            reclaimable_bytes: page_size * freelist,
            backups: backup_dir.map(backup_usage).unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Entry, Session};
    use crate::test_utils::test_repo;

    fn seed(repo: &Repository) {
        let session = Session::new("host".to_string(), 1000);
        repo.insert_session(&session).unwrap();
        for i in 0..200 {
            repo.insert_entry(&Entry::new(
                session.id.clone(),
                format!("cargo test --offline case-{i}"),
                "/work/project".to_string(),
                Some(0),
                1000 + i,
                1100 + i,
            ))
            .unwrap();
        }
        repo.upsert_skill(&crate::models::NewSkill {
            name: "deploy".to_string(),
            description: "how to deploy".to_string(),
            body: "run the deploy script".to_string(),
            triggers: vec![],
            scope: "global".to_string(),
            source: "human".to_string(),
            status: "active".to_string(),
        })
        .unwrap();
    }

    fn category<'a>(usage: &'a StorageUsage, name: &str) -> &'a CategoryUsage {
        usage
            .categories
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("missing category {name}"))
    }

    #[test]
    fn usage_is_reported_per_category_with_rows_and_bytes() {
        let (_dir, repo) = test_repo();
        seed(&repo);
        let usage = repo.storage_usage(None).unwrap();

        let commands = category(&usage, "Commands");
        assert_eq!(commands.rows, 200);
        assert!(commands.bytes > 0, "commands should use disk");

        let skills = category(&usage, "Skills");
        assert_eq!(skills.rows, 1);

        // Sessions and summaries are reported even when empty, so a user can
        // see that a category exists and holds nothing.
        assert_eq!(category(&usage, "Sessions").rows, 1);
        assert_eq!(category(&usage, "Summaries").rows, 0);

        // Every category carries the retention rule that governs it.
        assert!(usage.categories.iter().all(|c| !c.retention.is_empty()));

        // Categories account for the file: their total plus overhead cannot
        // exceed the database, and commands must dominate this fixture.
        let total: u64 = usage.categories.iter().map(|c| c.bytes).sum();
        assert!(
            total <= usage.database_bytes,
            "{total} > {}",
            usage.database_bytes
        );
        assert!(commands.bytes > skills.bytes);
    }

    #[test]
    fn deleting_history_frees_pages_inside_the_file_without_shrinking_it() {
        let (_dir, repo) = test_repo();
        seed(&repo);
        let before = repo.storage_usage(None).unwrap();

        let deleted = repo.delete_entries("case-", false, None).unwrap();
        assert_eq!(deleted, 200);
        let after = repo.storage_usage(None).unwrap();

        assert_eq!(category(&after, "Commands").rows, 0);
        assert_eq!(
            after.database_bytes, before.database_bytes,
            "a delete must not be reported as reclaimed disk space"
        );
        assert!(
            after.reclaimable_bytes > 0,
            "deleted pages stay in the file until VACUUM"
        );
    }

    #[test]
    fn backup_files_are_counted_from_the_backup_directory() {
        let (dir, repo) = test_repo();
        seed(&repo);
        let backups = dir.path().join("backups");
        std::fs::create_dir_all(&backups).unwrap();
        repo.backup_to(&backups.join("history-1.db")).unwrap();
        repo.backup_to(&backups.join("predelete-2.db")).unwrap();
        std::fs::write(backups.join("notes.txt"), b"not a backup").unwrap();

        let usage = repo.storage_usage(Some(&backups)).unwrap();
        assert_eq!(usage.backups.files, 2);
        assert!(usage.backups.bytes > 0);

        // A missing backup directory is simply zero, not an error.
        let missing = repo.storage_usage(Some(&dir.path().join("nope"))).unwrap();
        assert_eq!(missing.backups, BackupUsage::default());
    }

    #[test]
    fn the_report_names_every_category_and_states_what_a_backup_keeps() {
        let (_dir, repo) = test_repo();
        seed(&repo);
        let usage = repo.storage_usage(None).unwrap();
        let report = usage.report_lines().join("\n");

        for name in ["Commands", "Sessions", "Summaries", "Skills", "Backups"] {
            assert!(report.contains(name), "report is missing {name}:\n{report}");
        }
        assert!(report.to_lowercase().contains("vacuum"));
        assert!(
            report.to_lowercase().contains("backup"),
            "the report must say what deleting does not touch"
        );
    }
}
