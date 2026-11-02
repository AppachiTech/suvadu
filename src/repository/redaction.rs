//! Masking what is already stored.
//!
//! Redaction runs before a row is written, so a rule added later never
//! reaches the rows recorded before it. This finds every stored text the
//! rules would change and rewrites it, for `suv redact`.
//!
//! The list here is the list of places redaction writes to. Bookmarks, notes
//! and aliases are not on it: redaction never touched them, the person wrote
//! them, and a masked alias or bookmark stops working. They are only counted.
//!
//! A rewritten row is not yet gone from the disk. FTS5 marks an old term
//! deleted without removing it, so [`Repository::apply_redaction`] rebuilds
//! the search indexes. A shorter row leaves the old one's start in freed
//! space, and stale copies sit in the unused space of every page that split as
//! a row was recorded, where `secure_delete` does not reach because this write
//! never freed them. Only rewriting the file clears those, so the caller then
//! runs [`Repository::compact_after_masking`] and
//! [`Repository::checkpoint_wal`].
//!
//! A run can stop between the two, and the next scan would find nothing to
//! change. The masked rows therefore commit with a note that the file still
//! owes its compaction, which `compact_after_masking` clears.

use rusqlite::{params, Connection, OpenFlags};
use serde_json::Value;

use crate::db::DbResult;

use super::Repository;

/// Store names, in report order.
pub const COMMANDS: &str = "Commands";
pub const COMMAND_CONTEXT: &str = "Command context";
pub const FAILURE_OUTPUT: &str = "Failure output";
pub const AGENT_EVENTS: &str = "Agent prompts and replies";
pub const SUMMARIES: &str = "Agent summaries";
pub const GIT_SNAPSHOTS: &str = "Git snapshots";

/// `PRAGMA user_version` of a file whose masked rows are waiting for it to be
/// compacted. The field sits in the database header, so it commits with the
/// rows and exists in a backup of any schema. Nothing else uses it: Suvadu's
/// own version lives in `schema_version`.
const COMPACTION_DUE: i64 = 1;

/// How many characters a preview keeps on each side of a change.
const PREVIEW_CONTEXT_CHARS: usize = 40;

/// Masks one stored text, given the directory it was recorded in (when the
/// row knows it), so that a project's own rules reach that project's rows.
pub type Mask<'a> = dyn FnMut(&str, Option<&str>) -> String + 'a;

/// How captured text sits inside a column.
#[derive(Clone, Copy)]
enum Format {
    /// The column is the text.
    Text,
    /// A JSON object; these pointers hold captured text. Everything else in
    /// it is an id or a hash, which the rules have no business reading.
    Json(&'static [&'static str]),
    /// A JSON array of objects, each with these pointers.
    JsonList(&'static [&'static str]),
}

/// One column redaction writes to.
struct Store {
    label: &'static str,
    table: &'static str,
    column: &'static str,
    /// Selects `rowid`, the column, and the directory the row was recorded in.
    rows: &'static str,
    format: Format,
}

const STORES: [Store; 7] = [
    Store {
        label: COMMANDS,
        table: "entries",
        column: "command",
        rows: "SELECT rowid, command, cwd FROM entries",
        format: Format::Text,
    },
    Store {
        label: COMMAND_CONTEXT,
        table: "entries",
        column: "context",
        rows: "SELECT rowid, context, cwd FROM entries WHERE context IS NOT NULL",
        format: Format::Json(&["/agent_prompt", "/git_branch"]),
    },
    Store {
        label: FAILURE_OUTPUT,
        table: "entry_output",
        column: "tail",
        rows: "SELECT o.rowid, o.tail, e.cwd FROM entry_output o \
               LEFT JOIN entries e ON e.id = o.entry_id",
        format: Format::Text,
    },
    Store {
        label: AGENT_EVENTS,
        table: "ai_events",
        column: "data",
        rows: "SELECT rowid, data, cwd FROM ai_events",
        format: Format::Json(&["/data/text"]),
    },
    Store {
        label: SUMMARIES,
        table: "ai_summaries",
        column: "text",
        rows: "SELECT s.rowid, s.text, a.cwd FROM ai_summaries s \
               LEFT JOIN ai_sessions a ON a.id = s.session_id",
        format: Format::Text,
    },
    Store {
        label: GIT_SNAPSHOTS,
        table: "ai_snapshots",
        column: "branch",
        rows: "SELECT rowid, branch, cwd FROM ai_snapshots WHERE branch IS NOT NULL",
        format: Format::Text,
    },
    Store {
        label: GIT_SNAPSHOTS,
        table: "ai_snapshots",
        column: "paths",
        rows: "SELECT rowid, paths, cwd FROM ai_snapshots",
        format: Format::JsonList(&["/path", "/from"]),
    },
];

/// The first text in a value that changes, before and after, as a person
/// reads it. Not the stored form: re-encoding a JSON column sorts its keys, so
/// comparing the stored text would find a difference at its first key.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Shown {
    before: String,
    after: String,
}

/// What the rules make of one stored value.
enum Outcome {
    Unchanged,
    Masked {
        stored: String,
        shown: Shown,
    },
    /// Not the JSON this column should hold. Left alone: rewriting text
    /// that can't be read could only damage it.
    Unreadable,
}

impl Format {
    fn apply(self, stored: &str, cwd: Option<&str>, mask: &mut Mask<'_>) -> Outcome {
        match self {
            Self::Text => {
                let masked = mask(stored, cwd);
                if masked == stored {
                    return Outcome::Unchanged;
                }
                Outcome::Masked {
                    stored: masked.clone(),
                    shown: Shown {
                        before: stored.to_owned(),
                        after: masked,
                    },
                }
            }
            Self::Json(pointers) => {
                let Ok(mut value) = serde_json::from_str::<Value>(stored) else {
                    return Outcome::Unreadable;
                };
                let shown = mask_fields(&mut value, pointers, cwd, mask);
                encoded(&value, shown)
            }
            Self::JsonList(pointers) => {
                let Ok(mut value) = serde_json::from_str::<Value>(stored) else {
                    return Outcome::Unreadable;
                };
                let Some(items) = value.as_array_mut() else {
                    return Outcome::Unreadable;
                };
                let mut shown = None;
                for item in items {
                    let changed = mask_fields(item, pointers, cwd, mask);
                    shown = shown.or(changed);
                }
                encoded(&value, shown)
            }
        }
    }
}

fn encoded(value: &Value, shown: Option<Shown>) -> Outcome {
    let Some(shown) = shown else {
        return Outcome::Unchanged;
    };
    serde_json::to_string(value).map_or(Outcome::Unreadable, |stored| Outcome::Masked {
        stored,
        shown,
    })
}

/// Mask the strings at `pointers` inside `value`. The first that changed.
fn mask_fields(
    value: &mut Value,
    pointers: &[&str],
    cwd: Option<&str>,
    mask: &mut Mask<'_>,
) -> Option<Shown> {
    let mut shown = None;
    for pointer in pointers {
        if let Some(field) = value.pointer_mut(pointer) {
            if let Some(text) = field.as_str() {
                let masked = mask(text, cwd);
                if masked != text {
                    let change = Shown {
                        before: text.to_owned(),
                        after: masked.clone(),
                    };
                    *field = Value::String(masked);
                    shown = shown.or(Some(change));
                }
            }
        }
    }
    shown
}

/// One stored value the rules would change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub store: &'static str,
    table: &'static str,
    column: &'static str,
    rowid: i64,
    before: String,
    after: String,
    shown: Shown,
}

impl Change {
    /// One line around the first place this row changes, after masking. Safe
    /// to print: it only ever reads the masked text, and carries no control
    /// characters, which an old row could otherwise replay at the terminal.
    pub fn preview(&self) -> String {
        let Shown { before, after } = &self.shown;
        let mut start = before
            .bytes()
            .zip(after.bytes())
            .position(|(old, new)| old != new)
            .unwrap_or(0);
        while !after.is_char_boundary(start) {
            start -= 1;
        }

        let (head, tail) = after.split_at(start);
        let kept_head: String = {
            let mut chars: Vec<char> = head.chars().rev().take(PREVIEW_CONTEXT_CHARS).collect();
            chars.reverse();
            chars.into_iter().collect()
        };
        let tail_chars = crate::redact::REDACTED.chars().count() + PREVIEW_CONTEXT_CHARS;
        let kept_tail: String = tail.chars().take(tail_chars).collect();

        let window = format!(
            "{}{kept_head}{kept_tail}{}",
            if kept_head.len() < head.len() {
                "…"
            } else {
                ""
            },
            if kept_tail.len() < tail.len() {
                "…"
            } else {
                ""
            },
        );
        window
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .filter(|c| !c.is_control())
            .collect()
    }
}

/// Everything a scan found.
#[derive(Debug, Default)]
pub struct RedactionPlan {
    pub changes: Vec<Change>,
    /// Rows left alone because they could not be read.
    pub unreadable: usize,
    /// An earlier run masked this file and stopped before compacting it.
    pub compaction_due: bool,
}

/// Rows of the person's own making that the rules match. Reported, never
/// rewritten.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct KeptSecrets {
    pub bookmarks: usize,
    pub notes: usize,
    pub aliases: usize,
}

impl Repository {
    /// Every stored text `mask` would change. Reads only.
    pub fn find_unmasked(&self, mask: &mut Mask<'_>) -> DbResult<RedactionPlan> {
        let due: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let mut plan = RedactionPlan {
            compaction_due: due == COMPACTION_DUE,
            ..RedactionPlan::default()
        };
        for store in &STORES {
            // A backup from an older schema has fewer of these.
            if !self.has_table(store.table)? {
                continue;
            }
            let mut statement = self.conn.prepare(store.rows)?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let Ok(stored) = row.get_ref(1)?.as_str() else {
                    plan.unreadable += 1;
                    continue;
                };
                let cwd: Option<String> = row.get(2)?;
                match store.format.apply(stored, cwd.as_deref(), mask) {
                    Outcome::Unchanged => {}
                    Outcome::Unreadable => plan.unreadable += 1,
                    Outcome::Masked {
                        stored: after,
                        shown,
                    } => plan.changes.push(Change {
                        store: store.label,
                        table: store.table,
                        column: store.column,
                        rowid: row.get(0)?,
                        before: stored.to_owned(),
                        after,
                        shown,
                    }),
                }
            }
        }
        Ok(plan)
    }

    /// Bookmarks, notes and aliases `mask` would change.
    pub fn count_kept_secrets(&self, mask: &mut Mask<'_>) -> DbResult<KeptSecrets> {
        Ok(KeptSecrets {
            bookmarks: self.count_matching("SELECT command FROM bookmarks", mask)?,
            notes: self.count_matching("SELECT note FROM notes", mask)?,
            aliases: self.count_matching("SELECT command FROM aliases", mask)?,
        })
    }

    fn count_matching(&self, sql: &str, mask: &mut Mask<'_>) -> DbResult<usize> {
        let mut statement = self.conn.prepare(sql)?;
        let mut rows = statement.query([])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            if let Ok(text) = row.get_ref(0)?.as_str() {
                if mask(text, None) != text {
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    /// Write a plan, then rebuild the search indexes so that no old term
    /// survives in them. Returns how many rows changed. The file then owes
    /// a [`Self::compact_after_masking`].
    ///
    /// A row is only written if it still reads as it did when scanned, so
    /// nothing changed since then is lost; the next scan picks it up.
    pub fn apply_redaction(&self, plan: &RedactionPlan) -> DbResult<usize> {
        if plan.changes.is_empty() {
            return Ok(0);
        }
        let tx = self.transaction()?;
        let mut masked = 0;
        for change in &plan.changes {
            let (table, column) = (change.table, change.column);
            masked += self
                .conn
                .prepare_cached(&format!(
                    "UPDATE {table} SET {column} = ?1 WHERE rowid = ?2 AND {column} = ?3"
                ))?
                .execute(params![change.after, change.rowid, change.before])?;
        }
        self.rebuild_search_index()?;
        self.conn
            .pragma_update(None, "user_version", COMPACTION_DUE)?;
        tx.commit()?;
        Ok(masked)
    }

    /// Rewrite the file without anything its rows no longer hold, and record
    /// that nothing is owed.
    pub fn compact_after_masking(&self) -> DbResult<()> {
        self.vacuum()?;
        self.conn.pragma_update(None, "user_version", 0)?;
        Ok(())
    }

    /// Move the write-ahead log into the database and empty it. `false` when
    /// another connection was mid-read, which leaves the log's contents on
    /// disk until a later checkpoint.
    pub fn checkpoint_wal(&self) -> DbResult<bool> {
        let busy: i64 = self
            .conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
        Ok(busy == 0)
    }

    /// Open a backup to write to it. No migrations run: it keeps the schema
    /// it was taken under, so that it restores to what it was.
    pub fn open_backup(path: &std::path::Path) -> DbResult<Self> {
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        Ok(Self::new(Connection::open_with_flags(path, flags)?))
    }

    /// Whether this database has `table`.
    pub fn has_table(&self, table: &str) -> DbResult<bool> {
        Ok(self
            .conn
            .prepare_cached("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")?
            .exists([table])?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::test_repo;
    use serde_json::json;
    use std::collections::HashMap;

    const TOKEN: &str = "ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789";
    const OTHER_TOKEN: &str = "ghp_zZyYxXwWvVuUtTsSrRqQpPoOnNmMlLkKjJiI0987";

    fn mask(text: &str, _cwd: Option<&str>) -> String {
        crate::redact::redact_secrets(text)
    }

    fn command(repo: &Repository, id: i64, text: &str, context: Option<&str>) {
        repo.raw_execute_for_test(
            "INSERT OR IGNORE INTO sessions (id, hostname, created_at) VALUES ('s', 'h', 1)",
        )
        .unwrap();
        repo.conn
            .execute(
                "INSERT INTO entries (id, session_id, command, cwd, started_at, ended_at, \
                 duration_ms, context) VALUES (?1, 's', ?2, '/project', 1, 2, 1, ?3)",
                params![id, text, context],
            )
            .unwrap();
    }

    fn agent_session(repo: &Repository) {
        repo.raw_execute_for_test(
            "INSERT INTO ai_sessions (id, native_id, agent, cwd, created_at, updated_at) \
             VALUES ('agent', 'n', 'claude-code', '/project', 1, 1)",
        )
        .unwrap();
    }

    fn event_json(text: &str) -> String {
        json!({
            "id": "e1", "turn_id": "t1", "kind": "prompt", "at": 1, "model": null,
            "cwd": "/project",
            "data": {"text": text, "truncated": false, "text_hash": "hmac-sha256:abc"}
        })
        .to_string()
    }

    fn event(repo: &Repository, text: &str) {
        repo.conn
            .execute(
                "INSERT INTO ai_events (session_id, event_id, kind, cwd, data) \
                 VALUES ('agent', 'e1', 'prompt', '/project', ?1)",
                [event_json(text)],
            )
            .unwrap();
    }

    fn summary(repo: &Repository, text: &str) {
        repo.conn
            .execute(
                "INSERT INTO ai_summaries (id, session_id, source_revision, text, agent, model, \
                 source_ids, created_at) VALUES ('sum', 'agent', '0', ?1, 'a', 'm', '[]', 1)",
                [text],
            )
            .unwrap();
    }

    fn snapshot(repo: &Repository, branch: &str, paths: &str) {
        repo.conn
            .execute(
                "INSERT INTO ai_snapshots (session_id, kind, at, cwd, repo_root, branch, paths, \
                 path_count) VALUES ('agent', 'start', 1, '/project', '/project', ?1, ?2, 1)",
                params![branch, paths],
            )
            .unwrap();
    }

    /// One row holding `TOKEN` in every place redaction writes to.
    fn seed_every_store(repo: &Repository) {
        let secret = format!("password={TOKEN}");
        let context = json!({
            "agent_prompt": secret,
            "git_branch": format!("fix/{TOKEN}"),
            "agent_turn_id": "turn-1",
            "agent_prompt_hash": "hmac-sha256:abc",
        })
        .to_string();
        command(repo, 1, &format!("deploy {secret}"), Some(&context));
        repo.conn
            .execute(
                "INSERT INTO entry_output (entry_id, stream, tail) VALUES (1, 'stderr', ?1)",
                [&secret],
            )
            .unwrap();
        agent_session(repo);
        event(repo, &secret);
        summary(repo, &secret);
        let paths = json!([
            {"status": "R.", "path": format!("{TOKEN}.txt"), "from": format!("old-{TOKEN}.txt")},
            {"status": ".M", "path": "src/main.rs"}
        ])
        .to_string();
        snapshot(repo, &format!("fix/{TOKEN}"), &paths);
    }

    fn text_change(before: String, after: &str) -> Change {
        Change {
            store: COMMANDS,
            table: "entries",
            column: "command",
            rowid: 7,
            before: before.clone(),
            after: after.to_string(),
            shown: Shown {
                before,
                after: after.to_string(),
            },
        }
    }

    fn text_of(repo: &Repository, sql: &str) -> String {
        repo.conn.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    fn matches_in(repo: &Repository, index: &str, query: &str) -> i64 {
        repo.conn
            .query_row(
                &format!("SELECT count(*) FROM {index} WHERE {index} MATCH ?1"),
                [format!("\"{query}\"")],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn a_secret_is_found_wherever_redaction_writes_and_nothing_changes_yet() {
        let (_dir, repo) = test_repo();
        seed_every_store(&repo);

        let plan = repo.find_unmasked(&mut mask).unwrap();

        let labels: Vec<&str> = plan.changes.iter().map(|c| c.store).collect();
        assert_eq!(
            labels,
            [
                COMMANDS,
                COMMAND_CONTEXT,
                FAILURE_OUTPUT,
                AGENT_EVENTS,
                SUMMARIES,
                GIT_SNAPSHOTS,
                GIT_SNAPSHOTS
            ]
        );
        assert!(text_of(&repo, "SELECT command FROM entries").contains(TOKEN));
    }

    #[test]
    fn a_row_without_a_secret_is_not_planned() {
        let (_dir, repo) = test_repo();
        command(&repo, 1, "git status", Some(r#"{"git_branch":"main"}"#));
        agent_session(&repo);
        event(&repo, "what does this do?");

        let plan = repo.find_unmasked(&mut mask).unwrap();

        assert!(plan.changes.is_empty(), "{:?}", plan.changes);
    }

    #[test]
    fn applying_a_plan_masks_every_store() {
        let (_dir, repo) = test_repo();
        seed_every_store(&repo);

        let plan = repo.find_unmasked(&mut mask).unwrap();
        assert_eq!(repo.apply_redaction(&plan).unwrap(), 7);

        for sql in [
            "SELECT command FROM entries",
            "SELECT context FROM entries",
            "SELECT tail FROM entry_output",
            "SELECT data FROM ai_events",
            "SELECT text FROM ai_summaries",
            "SELECT branch FROM ai_snapshots",
            "SELECT paths FROM ai_snapshots",
        ] {
            let stored = text_of(&repo, sql);
            assert!(!stored.contains(TOKEN), "{sql}: {stored}");
            assert!(stored.contains("***REDACTED***"), "{sql}: {stored}");
        }
    }

    #[test]
    fn masked_json_rows_keep_their_shape_and_their_other_fields() {
        let (_dir, repo) = test_repo();
        seed_every_store(&repo);

        let plan = repo.find_unmasked(&mut mask).unwrap();
        repo.apply_redaction(&plan).unwrap();

        let context: HashMap<String, String> =
            serde_json::from_str(&text_of(&repo, "SELECT context FROM entries")).unwrap();
        assert_eq!(context["agent_turn_id"], "turn-1");
        assert_eq!(context["agent_prompt_hash"], "hmac-sha256:abc");

        let event: crate::ai_sessions::AiEvent =
            serde_json::from_str(&text_of(&repo, "SELECT data FROM ai_events")).unwrap();
        assert_eq!(event.turn_id.as_deref(), Some("t1"));
        assert_eq!(event.data["text_hash"], "hmac-sha256:abc");
        assert_eq!(event.data["truncated"], false);

        let paths: Vec<crate::ai_sessions::git_snapshot::ChangedPath> =
            serde_json::from_str(&text_of(&repo, "SELECT paths FROM ai_snapshots")).unwrap();
        assert_eq!(paths[0].status, "R.");
        assert_eq!(paths[0].path, "***REDACTED***.txt");
        assert_eq!(paths[0].from.as_deref(), Some("old-***REDACTED***.txt"));
        assert_eq!(paths[1].path, "src/main.rs");
    }

    #[test]
    fn a_second_scan_finds_nothing() {
        let (_dir, repo) = test_repo();
        seed_every_store(&repo);

        let plan = repo.find_unmasked(&mut mask).unwrap();
        repo.apply_redaction(&plan).unwrap();

        let again = repo.find_unmasked(&mut mask).unwrap();
        assert!(again.changes.is_empty(), "{:?}", again.changes);
    }

    #[test]
    fn neither_search_index_finds_the_secret_afterwards_and_both_stay_consistent() {
        let (_dir, repo) = test_repo();
        command(&repo, 1, &format!("deploy password={TOKEN}"), None);
        assert_eq!(matches_in(&repo, "entries_fts", TOKEN), 1);
        let normalized = TOKEN.replace('_', "").to_lowercase();
        assert_eq!(matches_in(&repo, "entries_norm_fts", &normalized), 1);

        let plan = repo.find_unmasked(&mut mask).unwrap();
        repo.apply_redaction(&plan).unwrap();

        assert_eq!(matches_in(&repo, "entries_fts", TOKEN), 0);
        assert_eq!(matches_in(&repo, "entries_norm_fts", &normalized), 0);
        assert_eq!(matches_in(&repo, "entries_fts", "REDACTED"), 1);
        assert!(crate::db::search_index_is_consistent(&repo.conn).unwrap());
    }

    #[test]
    fn a_row_edited_since_the_scan_keeps_the_edit() {
        let (_dir, repo) = test_repo();
        command(&repo, 1, &format!("deploy password={TOKEN}"), None);
        let plan = repo.find_unmasked(&mut mask).unwrap();

        repo.raw_execute_for_test(&format!(
            "UPDATE entries SET command = 'deploy password={OTHER_TOKEN}'"
        ))
        .unwrap();

        assert_eq!(repo.apply_redaction(&plan).unwrap(), 0);
        assert!(text_of(&repo, "SELECT command FROM entries").contains(OTHER_TOKEN));
        // The next run sees the edited row.
        let again = repo.find_unmasked(&mut mask).unwrap();
        assert_eq!(again.changes.len(), 1);
    }

    #[test]
    fn json_that_cannot_be_read_is_counted_and_left_as_it_is() {
        let (_dir, repo) = test_repo();
        let broken = format!("not json password={TOKEN}");
        command(&repo, 1, "git status", Some(&broken));

        let plan = repo.find_unmasked(&mut mask).unwrap();

        assert!(plan.changes.is_empty(), "{:?}", plan.changes);
        assert_eq!(plan.unreadable, 1);
        assert_eq!(text_of(&repo, "SELECT context FROM entries"), broken);
    }

    #[test]
    fn text_that_is_not_valid_utf8_is_counted_and_does_not_stop_the_scan() {
        let (_dir, repo) = test_repo();
        command(&repo, 1, "placeholder", None);
        command(&repo, 2, &format!("deploy password={TOKEN}"), None);
        repo.raw_execute_for_test(
            "UPDATE entries SET command = CAST(x'ff41' AS TEXT) WHERE id = 1",
        )
        .unwrap();

        let plan = repo.find_unmasked(&mut mask).unwrap();

        assert_eq!(plan.unreadable, 1);
        assert_eq!(plan.changes.len(), 1);
    }

    #[test]
    fn each_row_is_masked_under_its_own_directory() {
        let (_dir, repo) = test_repo();
        command(&repo, 1, "ticket CORP-1234", None);
        command(&repo, 2, "ticket CORP-1234", None);
        repo.raw_execute_for_test("UPDATE entries SET cwd = '/elsewhere' WHERE id = 2")
            .unwrap();

        let plan = repo
            .find_unmasked(&mut |text, cwd| {
                if cwd == Some("/project") {
                    text.replace("CORP-1234", "***REDACTED***")
                } else {
                    text.to_string()
                }
            })
            .unwrap();

        let ids: Vec<i64> = plan.changes.iter().map(|c| c.rowid).collect();
        assert_eq!(ids, [1]);
    }

    #[test]
    fn a_database_from_an_older_schema_is_masked_as_far_as_it_goes() {
        let dir = tempfile::TempDir::new().unwrap();
        let conn = Connection::open(dir.path().join("old.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE entries (id INTEGER PRIMARY KEY AUTOINCREMENT, command TEXT NOT NULL,
                 cwd TEXT NOT NULL, context TEXT);
             CREATE VIRTUAL TABLE entries_fts USING fts5(command, content='entries',
                 content_rowid='id', tokenize='trigram');
             CREATE TRIGGER entries_fts_ai AFTER INSERT ON entries BEGIN
                 INSERT INTO entries_fts(rowid, command) VALUES (new.id, new.command);
             END;
             CREATE TRIGGER entries_fts_au AFTER UPDATE ON entries BEGIN
                 INSERT INTO entries_fts(entries_fts, rowid, command)
                     VALUES ('delete', old.id, old.command);
                 INSERT INTO entries_fts(rowid, command) VALUES (new.id, new.command);
             END;",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO entries (command, cwd) VALUES (?1, '/project')",
            [format!("deploy password={TOKEN}")],
        )
        .unwrap();
        let repo = Repository::new(conn);

        let plan = repo.find_unmasked(&mut mask).unwrap();
        assert_eq!(repo.apply_redaction(&plan).unwrap(), 1);

        assert_eq!(matches_in(&repo, "entries_fts", TOKEN), 0);
        assert!(!text_of(&repo, "SELECT command FROM entries").contains(TOKEN));
    }

    #[test]
    fn bookmarks_notes_and_aliases_are_counted_and_never_planned() {
        let (_dir, repo) = test_repo();
        let secret = format!("deploy password={TOKEN}");
        command(&repo, 1, "git status", None);
        repo.add_bookmark(&secret, Some("deploy")).unwrap();
        repo.upsert_note(1, &secret).unwrap();
        repo.add_alias("ship", &secret).unwrap();
        repo.add_alias("gs", "git status").unwrap();

        let plan = repo.find_unmasked(&mut mask).unwrap();
        let kept = repo.count_kept_secrets(&mut mask).unwrap();

        assert!(plan.changes.is_empty(), "{:?}", plan.changes);
        assert_eq!(
            kept,
            KeptSecrets {
                bookmarks: 1,
                notes: 1,
                aliases: 1
            }
        );
    }

    #[test]
    fn a_preview_of_a_json_row_shows_the_text_that_changed_not_the_json() {
        let (_dir, repo) = test_repo();
        command(
            &repo,
            1,
            "git status",
            Some(
                &json!({"z_last": "x", "agent_prompt": format!("use\n password={TOKEN}")})
                    .to_string(),
            ),
        );
        agent_session(&repo);
        // Written in struct order: re-encoding sorts the keys, so comparing
        // the stored JSON finds a difference at its first key.
        repo.conn
            .execute(
                "INSERT INTO ai_events (session_id, event_id, kind, cwd, data) \
                 VALUES ('agent', 'e1', 'prompt', '/project', ?1)",
                [format!(
                    r#"{{"id":"e1","turn_id":null,"kind":"prompt","at":1,"model":null,"cwd":"/project","data":{{"text":"password={TOKEN}"}}}}"#
                )],
            )
            .unwrap();

        let plan = repo.find_unmasked(&mut mask).unwrap();

        let previews: Vec<String> = plan.changes.iter().map(Change::preview).collect();
        assert_eq!(
            previews,
            ["use password=***REDACTED***", "password=***REDACTED***"]
        );
    }

    #[test]
    fn a_preview_shows_the_change_in_context_and_never_the_secret() {
        let change = text_change(
            format!("cd /srv && deploy --env prod\n password={TOKEN} --force"),
            "cd /srv && deploy --env prod\n password=***REDACTED*** --force",
        );

        assert_eq!(
            change.preview(),
            "cd /srv && deploy --env prod password=***REDACTED*** --force"
        );
    }

    #[test]
    fn a_preview_is_cut_around_the_change_and_points_at_the_first_new_mask() {
        let padding = "x".repeat(200);
        let change = text_change(
            format!("password=***REDACTED*** {padding} token={TOKEN} {padding}"),
            &format!("password=***REDACTED*** {padding} token=***REDACTED*** {padding}"),
        );

        let preview = change.preview();

        assert!(preview.starts_with('…'), "{preview}");
        assert!(preview.ends_with('…'), "{preview}");
        assert!(preview.contains("token=***REDACTED*** x"), "{preview}");
        assert!(!preview.contains("password"), "{preview}");
    }

    #[test]
    fn a_preview_cannot_drive_the_terminal() {
        let change = text_change(
            format!("\u{1b}[2Jpassword={TOKEN}"),
            "\u{1b}[2Jpassword=***REDACTED***",
        );

        assert_eq!(change.preview(), "[2Jpassword=***REDACTED***");
    }

    #[test]
    fn a_preview_handles_text_where_a_character_starts_the_same_way() {
        // `é` and `è` share their first byte, so the first differing byte
        // falls inside a character.
        let change = text_change("café".to_string(), "cafè");

        assert_eq!(change.preview(), "cafè");
    }

    /// A run can stop at any moment after it commits: Ctrl-C, a closed
    /// terminal, a closed pipe. The rows are masked by then, so the next scan
    /// finds nothing to change, and only the file still owes its compaction:
    /// stale copies of a row sit in the unused space of pages that split as
    /// it was recorded, where nothing but a `VACUUM` reaches them. The clean
    /// rows and the long value are what make `SQLite` leave those copies.
    #[test]
    fn a_run_stopped_before_compacting_is_finished_by_the_next_one() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.db");
        let value: String = TOKEN.repeat(8);
        let start = &value[..40];
        let holds_it = |path: &std::path::Path| {
            std::fs::read(path)
                .unwrap()
                .windows(start.len())
                .any(|window| window == start.as_bytes())
        };
        {
            let repo = Repository::new(crate::db::init_db(&path).unwrap());
            for id in 0..30 {
                if id % 2 == 0 {
                    command(&repo, id, &format!("password={value}"), None);
                } else {
                    command(&repo, id, &"x".repeat(400), None);
                }
            }
            let plan = repo.find_unmasked(&mut mask).unwrap();
            assert!(!plan.compaction_due);
            assert_eq!(repo.apply_redaction(&plan).unwrap(), 15);
        }
        assert!(holds_it(&path), "the fixture must leave the text behind");

        let repo = Repository::new(crate::db::init_db(&path).unwrap());
        let plan = repo.find_unmasked(&mut mask).unwrap();
        assert!(plan.changes.is_empty(), "{:?}", plan.changes);
        assert!(plan.compaction_due);

        repo.compact_after_masking().unwrap();
        assert!(!repo.find_unmasked(&mut mask).unwrap().compaction_due);
        drop(repo);

        assert!(!holds_it(&path));
    }

    #[test]
    fn an_empty_plan_writes_nothing_and_owes_nothing() {
        let (_dir, repo) = test_repo();
        command(&repo, 1, "git status", None);

        let plan = repo.find_unmasked(&mut mask).unwrap();

        assert_eq!(repo.apply_redaction(&plan).unwrap(), 0);
        assert!(!repo.find_unmasked(&mut mask).unwrap().compaction_due);
    }

    #[test]
    fn checkpointing_empties_the_write_ahead_log() {
        let (dir, repo) = test_repo();
        seed_every_store(&repo);
        let plan = repo.find_unmasked(&mut mask).unwrap();
        repo.apply_redaction(&plan).unwrap();

        repo.compact_after_masking().unwrap();
        assert!(repo.checkpoint_wal().unwrap());

        let wal = dir.path().join("test.db-wal");
        assert_eq!(std::fs::metadata(wal).map_or(0, |m| m.len()), 0);
    }
}
