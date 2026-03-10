use std::io::Write;

use suvadu::db::init_db;
use suvadu::models::{Entry, Session};
use suvadu::repository::{QueryFilter, Repository};
use tempfile::TempDir;

/// Create a temp directory and an initialised repository, mirroring the
/// pattern used by the unit-test helper `test_utils::test_repo`.
fn test_repo() -> (TempDir, Repository) {
    let dir = TempDir::new().unwrap();
    let conn = init_db(&dir.path().join("test.db")).unwrap();
    (dir, Repository::new(conn))
}

/// Build a `Session` value.  `Session::new` is `#[cfg(test)]`-gated in
/// the library so it is unavailable to integration tests.  All fields
/// are `pub`, so we construct it directly.
fn make_session(hostname: &str, created_at: i64) -> Session {
    Session {
        id: uuid::Uuid::new_v4().to_string(),
        hostname: hostname.to_string(),
        created_at,
        tag_id: None,
    }
}

/// Count all entries in the database via `count_filtered` with an empty
/// filter (the zero-argument `count_entries` helper is `#[cfg(test)]`).
fn count_all_entries(repo: &Repository) -> i64 {
    repo.count_filtered(&QueryFilter::default()).unwrap()
}

/// Seed a session and N entries with sequential timestamps.
/// Returns the session and the list of entry IDs.
fn seed(repo: &Repository, hostname: &str, commands: &[&str], base_ts: i64) -> (Session, Vec<i64>) {
    let session = make_session(hostname, base_ts);
    repo.insert_session(&session).unwrap();
    let ids: Vec<i64> = commands
        .iter()
        .enumerate()
        .map(|(i, cmd)| {
            let ts = base_ts + (i as i64) * 1000;
            let entry = Entry::new(
                session.id.clone(),
                cmd.to_string(),
                "/project".to_string(),
                Some(0),
                ts,
                ts + 50,
            );
            repo.insert_entry(&entry).unwrap()
        })
        .collect();
    (session, ids)
}

// ──────────────────────────────────────────────────────────────────────
// 1. Add + search round-trip
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_add_search_roundtrip() {
    let (_tmp, repo) = test_repo();

    let session = make_session("test-host", 1000);
    repo.insert_session(&session).unwrap();

    let commands = ["ls -la", "git status", "cargo build"];
    for (i, cmd) in commands.iter().enumerate() {
        let entry = Entry::new(
            session.id.clone(),
            cmd.to_string(),
            "/home/user".to_string(),
            Some(0),
            2000 + (i as i64) * 100,
            2050 + (i as i64) * 100,
        );
        repo.insert_entry(&entry).unwrap();
    }

    // All three entries should be present.
    assert_eq!(count_all_entries(&repo), 3);

    // Search for "git" -- should find exactly one entry.
    let filter = suvadu::repository::QueryFilter {
        query: Some("git"),
        ..Default::default()
    };
    let results = repo.get_entries_filtered(100, 0, &filter).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].command, "git status");

    // Search for "cargo" -- should find exactly one entry.
    let filter = suvadu::repository::QueryFilter {
        query: Some("cargo"),
        ..Default::default()
    };
    let results = repo.get_entries_filtered(100, 0, &filter).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].command, "cargo build");

    // Non-matching query -- should return nothing.
    let filter = suvadu::repository::QueryFilter {
        query: Some("python"),
        ..Default::default()
    };
    let results = repo.get_entries_filtered(100, 0, &filter).unwrap();
    assert!(results.is_empty());
}

// ──────────────────────────────────────────────────────────────────────
// 2. Concurrent writes
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_concurrent_writes() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("concurrent.db");

    // Initialise the database once so the schema exists.
    let conn = init_db(&db_path).unwrap();
    drop(conn);

    let handles: Vec<_> = (0..4)
        .map(|thread_idx| {
            let path = db_path.clone();
            std::thread::spawn(move || {
                // Each thread opens its own connection.
                let conn = init_db(&path).unwrap();
                let repo = Repository::new(conn);

                let session = Session {
                    id: uuid::Uuid::new_v4().to_string(),
                    hostname: format!("host-{thread_idx}"),
                    created_at: 1000 + thread_idx,
                    tag_id: None,
                };
                repo.insert_session(&session).unwrap();

                for i in 0..25 {
                    let ts = 2000 + thread_idx * 1000 + i;
                    let entry = Entry::new(
                        session.id.clone(),
                        format!("cmd_{thread_idx}_{i}"),
                        "/tmp".to_string(),
                        Some(0),
                        ts,
                        ts + 10,
                    );
                    repo.insert_entry(&entry).unwrap();
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked");
    }

    // Open a fresh connection and verify all 100 entries landed.
    let conn = init_db(&db_path).unwrap();
    let repo = Repository::new(conn);
    assert_eq!(count_all_entries(&repo), 100);
}

// ──────────────────────────────────────────────────────────────────────
// 3. Export / import round-trip (JSONL)
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_export_import_roundtrip() {
    // -- Phase 1: populate a source database and export to JSONL -----------
    let (src_dir, src_repo) = test_repo();

    let session = make_session("export-host", 1000);
    src_repo.insert_session(&session).unwrap();

    let entry_count = 10;
    for i in 0..entry_count {
        let entry = Entry::new(
            session.id.clone(),
            format!("export_cmd_{i}"),
            "/tmp".to_string(),
            Some(0),
            3000 + i * 100,
            3050 + i * 100,
        );
        src_repo.insert_entry(&entry).unwrap();
    }

    assert_eq!(count_all_entries(&src_repo), entry_count);

    // Export by streaming entries to a JSONL file.
    let jsonl_path = src_dir.path().join("export.jsonl");
    {
        let mut file = std::fs::File::create(&jsonl_path).unwrap();
        src_repo
            .stream_export_entries(None, None, |entry| {
                let line = serde_json::to_string(&entry)?;
                writeln!(file, "{line}")?;
                Ok(())
            })
            .unwrap();
    }

    // -- Phase 2: import into a fresh database and verify counts ----------
    let (_dst_dir, dst_repo) = test_repo();

    // The import logic in import_export.rs talks to Repository::init() (uses
    // the default DB path).  For testing we replicate the core import loop
    // directly, which is what the CLI ultimately does.
    let import_session = make_session("import-host", 5000);
    dst_repo.insert_session(&import_session).unwrap();

    let contents = std::fs::read_to_string(&jsonl_path).unwrap();
    dst_repo.begin_transaction().unwrap();
    for line in contents.lines() {
        let entry: Entry = serde_json::from_str(line).unwrap();
        // Re-associate with the import session.
        let imported = Entry::new(
            import_session.id.clone(),
            entry.command,
            entry.cwd,
            entry.exit_code,
            entry.started_at,
            entry.ended_at,
        );
        dst_repo.insert_entry(&imported).unwrap();
    }
    dst_repo.commit().unwrap();

    assert_eq!(count_all_entries(&dst_repo), entry_count);
}

// ──────────────────────────────────────────────────────────────────────
// 4. GC cleans orphaned sessions (and notes)
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_gc_cleans_orphans() {
    let (_tmp, repo) = test_repo();

    // Create two sessions: one with entries and one without.
    let active_session = make_session("active-host", 1000);
    repo.insert_session(&active_session).unwrap();

    let orphan_session = make_session("orphan-host", 2000);
    repo.insert_session(&orphan_session).unwrap();

    // Add an entry only to the active session.
    let entry = Entry::new(
        active_session.id.clone(),
        "echo active".to_string(),
        "/tmp".to_string(),
        Some(0),
        3000,
        3010,
    );
    repo.insert_entry(&entry).unwrap();

    // Before GC: one orphaned session should exist.
    assert_eq!(repo.count_orphaned_sessions().unwrap(), 1);

    // Run GC.
    let deleted = repo.delete_orphaned_sessions().unwrap();
    assert_eq!(deleted, 1);

    // After GC: no orphaned sessions.
    assert_eq!(repo.count_orphaned_sessions().unwrap(), 0);

    // The active session should still be retrievable.
    assert!(repo.get_session(&active_session.id).unwrap().is_some());
    // The orphan should be gone.
    assert!(repo.get_session(&orphan_session.id).unwrap().is_none());
}

// ──────────────────────────────────────────────────────────────────────
// 5. Delete with pattern
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_delete_with_pattern() {
    let (_tmp, repo) = test_repo();

    let session = make_session("del-host", 1000);
    repo.insert_session(&session).unwrap();

    // Insert a mix of commands.
    let commands = [
        "git status",
        "git commit -m fix",
        "cargo build",
        "cargo test",
        "ls -la",
    ];
    for (i, cmd) in commands.iter().enumerate() {
        let entry = Entry::new(
            session.id.clone(),
            cmd.to_string(),
            "/project".to_string(),
            Some(0),
            4000 + (i as i64) * 100,
            4050 + (i as i64) * 100,
        );
        repo.insert_entry(&entry).unwrap();
    }

    assert_eq!(count_all_entries(&repo), 5);

    // Delete entries matching "git" (substring / LIKE match).
    let deleted = repo.delete_entries("git", false, None).unwrap();
    assert_eq!(deleted, 2);

    // Three entries should remain.
    assert_eq!(count_all_entries(&repo), 3);

    // Verify that the remaining commands are the non-git ones.
    let filter = suvadu::repository::QueryFilter {
        query: Some("git"),
        ..Default::default()
    };
    let remaining_git = repo.get_entries_filtered(100, 0, &filter).unwrap();
    assert!(
        remaining_git.is_empty(),
        "No git entries should remain after deletion"
    );

    // Verify a surviving entry is still there.
    let filter = suvadu::repository::QueryFilter {
        query: Some("cargo"),
        ..Default::default()
    };
    let remaining_cargo = repo.get_entries_filtered(100, 0, &filter).unwrap();
    assert_eq!(remaining_cargo.len(), 2);
}

// ──────────────────────────────────────────────────────────────────────
// 6. Foreign key cascade (session -> entries -> notes)
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_foreign_keys_cascade() {
    let (_tmp, repo) = test_repo();

    let session = make_session("cascade-host", 1000);
    repo.insert_session(&session).unwrap();

    // Insert an entry and attach a note.
    let entry = Entry::new(
        session.id.clone(),
        "echo cascade".to_string(),
        "/tmp".to_string(),
        Some(0),
        5000,
        5010,
    );
    let entry_id = repo.insert_entry(&entry).unwrap();
    repo.upsert_note(entry_id, "important note").unwrap();

    // Verify the note exists.
    let note = repo.get_note(entry_id).unwrap();
    assert!(note.is_some());
    assert_eq!(note.unwrap().content, "important note");

    // Delete the entry.  The notes table has
    // `ON DELETE CASCADE` on `entry_id`, so the note should vanish.
    repo.delete_entry(entry_id).unwrap();

    // Note should be gone via cascade.
    let note = repo.get_note(entry_id).unwrap();
    assert!(note.is_none(), "Note should be cascade-deleted with entry");

    // Entry count should be zero.
    assert_eq!(count_all_entries(&repo), 0);

    // Session is still present (entries FK does not cascade to sessions).
    assert!(repo.get_session(&session.id).unwrap().is_some());
}

// ──────────────────────────────────────────────────────────────────────
// 7. Tag lifecycle: create → assign → query → update
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_tag_lifecycle() {
    let (_tmp, repo) = test_repo();

    // Create two tags.
    let work_id = repo.create_tag("work", Some("work projects")).unwrap();
    let personal_id = repo
        .create_tag("personal", Some("personal projects"))
        .unwrap();
    assert_ne!(work_id, personal_id);

    // Create two sessions, tag one.
    let (s1, _) = seed(&repo, "host-1", &["git status"], 1_000_000);
    let (s2, _) = seed(&repo, "host-2", &["cargo build"], 2_000_000);
    repo.tag_session(&s1.id, Some(work_id)).unwrap();

    // Verify tag association.
    assert_eq!(
        repo.get_tag_by_session(&s1.id).unwrap().as_deref(),
        Some("work")
    );
    assert!(repo.get_tag_by_session(&s2.id).unwrap().is_none());

    // Look up by name.
    assert_eq!(repo.get_tag_id_by_name("work").unwrap(), Some(work_id));
    assert!(repo.get_tag_id_by_name("nonexistent").unwrap().is_none());

    // Update tag.
    repo.update_tag(work_id, "work-updated", Some("renamed"))
        .unwrap();
    assert_eq!(
        repo.get_tag_by_session(&s1.id).unwrap().as_deref(),
        Some("work-updated")
    );

    // List tags returns both.
    let tags = repo.get_tags().unwrap();
    assert_eq!(tags.len(), 2);
}

// ──────────────────────────────────────────────────────────────────────
// 8. Bookmark + note + alias CRUD together (cross-feature integration)
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_bookmark_note_alias_cross_feature() {
    let (_tmp, repo) = test_repo();
    let (_, ids) = seed(
        &repo,
        "host",
        &["docker compose up", "cargo test --release"],
        1_000_000,
    );

    // Bookmark a command.
    repo.add_bookmark("docker compose up", Some("start stack"))
        .unwrap();
    let bookmarks = repo.list_bookmarks().unwrap();
    assert_eq!(bookmarks.len(), 1);
    assert_eq!(bookmarks[0].label.as_deref(), Some("start stack"));

    // Annotate an entry.
    repo.upsert_note(ids[0], "important compose command")
        .unwrap();
    repo.upsert_note(ids[1], "release test").unwrap();
    let noted = repo.get_noted_entry_ids().unwrap();
    assert_eq!(noted.len(), 2);
    assert!(noted.contains(&ids[0]));
    assert!(noted.contains(&ids[1]));

    // Create aliases.
    repo.add_alias("dcu", "docker compose up").unwrap();
    repo.add_alias("ctr", "cargo test --release").unwrap();
    let aliases = repo.list_aliases().unwrap();
    assert_eq!(aliases.len(), 2);
    assert_eq!(aliases[0].name, "ctr"); // ordered by name
    assert_eq!(aliases[1].name, "dcu");

    // Upsert alias (update existing).
    repo.add_alias("dcu", "docker compose up -d").unwrap();
    let aliases = repo.list_aliases().unwrap();
    assert_eq!(aliases.len(), 2);
    let dcu = aliases.iter().find(|a| a.name == "dcu").unwrap();
    assert_eq!(dcu.command, "docker compose up -d");

    // Remove bookmark and alias.
    assert!(repo.remove_bookmark("docker compose up").unwrap());
    assert!(repo.remove_alias("dcu").unwrap());
    assert!(!repo.remove_alias("nonexistent").unwrap());
    assert_eq!(repo.list_bookmarks().unwrap().len(), 0);
    assert_eq!(repo.list_aliases().unwrap().len(), 1);

    // Notes survive bookmark/alias removal.
    let note = repo.get_note(ids[0]).unwrap().unwrap();
    assert_eq!(note.content, "important compose command");
}

// ──────────────────────────────────────────────────────────────────────
// 9. Session management: multi-session, list, entries-by-session
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_session_management() {
    let (_tmp, repo) = test_repo();

    // Create three sessions with entries at different times.
    let (s1, _) = seed(&repo, "laptop", &["git pull", "cargo build"], 1_000_000);
    let (s2, _) = seed(
        &repo,
        "desktop",
        &["npm install", "npm test", "npm run build"],
        2_000_000,
    );
    let (s3, _) = seed(&repo, "server", &["systemctl restart nginx"], 3_000_000);

    // Count entries per session using the count_filtered API.
    let s1_count = repo
        .count_filtered(&QueryFilter {
            query: Some(&s1.id),
            field: suvadu::models::SearchField::Session,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(s1_count, 2);
    let s2_count = repo
        .count_filtered(&QueryFilter {
            query: Some(&s2.id),
            field: suvadu::models::SearchField::Session,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(s2_count, 3);
    let s3_count = repo
        .count_filtered(&QueryFilter {
            query: Some(&s3.id),
            field: suvadu::models::SearchField::Session,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(s3_count, 1);

    // List sessions (most recent first, limited).
    let sessions = repo.list_sessions(None, None, 10).unwrap();
    assert_eq!(sessions.len(), 3);
    // Most recent session first.
    assert_eq!(sessions[0].hostname, "server");
    assert_eq!(sessions[0].cmd_count, 1);
    assert_eq!(sessions[1].hostname, "desktop");
    assert_eq!(sessions[1].cmd_count, 3);
    assert_eq!(sessions[2].hostname, "laptop");
    assert_eq!(sessions[2].cmd_count, 2);

    // Total entry count across all sessions.
    assert_eq!(count_all_entries(&repo), 6);
}

// ──────────────────────────────────────────────────────────────────────
// 10. Stats aggregation end-to-end
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_stats_aggregation() {
    let (_tmp, repo) = test_repo();

    let session = make_session("stats-host", 1_700_000_000_000);
    repo.insert_session(&session).unwrap();

    // Insert entries with varied commands, exit codes, and cwds.
    let data: Vec<(&str, &str, Option<i32>)> = vec![
        ("git status", "/project-a", Some(0)),
        ("git status", "/project-b", Some(0)),
        ("git status", "/project-c", Some(0)),
        ("cargo build", "/project-a", Some(0)),
        ("cargo build", "/project-a", Some(1)), // failure
        ("cargo test", "/project-a", Some(0)),
        ("npm install", "/project-b", Some(0)),
        ("npm install", "/project-b", Some(127)), // failure
    ];

    for (i, (cmd, cwd, exit_code)) in data.iter().enumerate() {
        let ts = 1_700_000_000_000 + (i as i64) * 60_000; // 1 minute apart
        let entry = Entry::new(
            session.id.clone(),
            cmd.to_string(),
            cwd.to_string(),
            *exit_code,
            ts,
            ts + 500,
        );
        repo.insert_entry(&entry).unwrap();
    }

    // Get stats (all time, top 5).
    let stats = repo.get_stats(None, 5, None).unwrap();

    assert_eq!(stats.total_commands, 8);
    assert_eq!(stats.unique_commands, 4); // git status, cargo build, cargo test, npm install
    assert_eq!(stats.success_count, 6);
    assert_eq!(stats.failure_count, 2);
    assert_eq!(stats.avg_duration_ms, 500);

    // Top commands are ordered by frequency.
    assert_eq!(stats.top_commands[0].0, "git status");
    assert_eq!(stats.top_commands[0].1, 3);

    // Top directories.
    assert_eq!(stats.top_directories[0].0, "/project-a");
    assert_eq!(stats.top_directories[0].1, 4);
}

// ──────────────────────────────────────────────────────────────────────
// 11. Unique (deduplicated) query mode
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_unique_entries() {
    let (_tmp, repo) = test_repo();
    let session = make_session("unique-host", 1_000_000);
    repo.insert_session(&session).unwrap();

    // Insert duplicates: "git status" 5 times, "cargo build" 3 times.
    for i in 0..5 {
        let ts = 1_000_000 + i * 1000;
        let entry = Entry::new(
            session.id.clone(),
            "git status".to_string(),
            "/project".to_string(),
            Some(0),
            ts,
            ts + 50,
        );
        repo.insert_entry(&entry).unwrap();
    }
    for i in 0..3 {
        let ts = 2_000_000 + i * 1000;
        let entry = Entry::new(
            session.id.clone(),
            "cargo build".to_string(),
            "/project".to_string(),
            Some(0),
            ts,
            ts + 50,
        );
        repo.insert_entry(&entry).unwrap();
    }

    // Non-unique: all 8 entries.
    assert_eq!(count_all_entries(&repo), 8);

    // Unique mode: only 2 distinct commands.
    let unique = repo
        .get_unique_entries_filtered(10, 0, &QueryFilter::default(), false)
        .unwrap();
    assert_eq!(unique.len(), 2);
}

// ──────────────────────────────────────────────────────────────────────
// 12. Filtered queries: date range, exit code, executor, cwd
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_filtered_queries() {
    let (_tmp, repo) = test_repo();
    let session = make_session("filter-host", 1_000_000);
    repo.insert_session(&session).unwrap();

    // Insert entries at different times, cwds, exit codes, and with executor info.
    let entries_data = [
        (
            "git status",
            "/project-a",
            Some(0),
            1_000_000_i64,
            None,
            None,
        ),
        ("git pull", "/project-a", Some(0), 2_000_000, None, None),
        ("npm test", "/project-b", Some(1), 3_000_000, None, None),
        (
            "cargo build",
            "/project-a",
            Some(0),
            4_000_000,
            Some("agent"),
            Some("claude-code"),
        ),
        (
            "cargo test",
            "/project-a",
            Some(0),
            5_000_000,
            Some("agent"),
            Some("cursor"),
        ),
    ];

    for (cmd, cwd, exit_code, ts, exec_type, executor) in &entries_data {
        let mut entry = Entry::new(
            session.id.clone(),
            cmd.to_string(),
            cwd.to_string(),
            *exit_code,
            *ts,
            ts + 100,
        );
        entry.executor_type = exec_type.map(String::from);
        entry.executor = executor.map(String::from);
        repo.insert_entry(&entry).unwrap();
    }

    // Filter by date range: only entries between 2M and 4M.
    let filter = QueryFilter {
        after: Some(2_000_000),
        before: Some(4_000_000),
        ..Default::default()
    };
    let results = repo.get_entries_filtered(100, 0, &filter).unwrap();
    assert_eq!(results.len(), 3); // git pull, npm test, cargo build

    // Filter by exit code: failures only.
    let filter = QueryFilter {
        exit_code: Some(1),
        ..Default::default()
    };
    let results = repo.get_entries_filtered(100, 0, &filter).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].command, "npm test");

    // Filter by cwd.
    let filter = QueryFilter {
        cwd: Some("/project-b"),
        ..Default::default()
    };
    let results = repo.get_entries_filtered(100, 0, &filter).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].command, "npm test");

    // Filter by executor.
    let filter = QueryFilter {
        executor: Some("claude"),
        ..Default::default()
    };
    let results = repo.get_entries_filtered(100, 0, &filter).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].command, "cargo build");
}

// ──────────────────────────────────────────────────────────────────────
// 13. Transaction guard: rollback on drop
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_transaction_rollback_on_drop() {
    let (_tmp, repo) = test_repo();
    let session = make_session("txn-host", 1_000_000);
    repo.insert_session(&session).unwrap();

    // Start a transaction, insert entries, then drop guard without commit.
    {
        let guard = repo.transaction().unwrap();
        for i in 0..5 {
            let entry = Entry::new(
                session.id.clone(),
                format!("txn_cmd_{i}"),
                "/tmp".to_string(),
                Some(0),
                1_000_000 + i * 100,
                1_000_050 + i * 100,
            );
            repo.insert_entry(&entry).unwrap();
        }
        // guard dropped here without commit() → rollback
        drop(guard);
    }

    // Entries should be rolled back.
    assert_eq!(count_all_entries(&repo), 0);

    // Now do the same but commit.
    {
        let guard = repo.transaction().unwrap();
        for i in 0..3 {
            let entry = Entry::new(
                session.id.clone(),
                format!("committed_cmd_{i}"),
                "/tmp".to_string(),
                Some(0),
                2_000_000 + i * 100,
                2_000_050 + i * 100,
            );
            repo.insert_entry(&entry).unwrap();
        }
        guard.commit().unwrap();
    }

    // These should persist.
    assert_eq!(count_all_entries(&repo), 3);
}

// ──────────────────────────────────────────────────────────────────────
// 14. Schema migration: re-opening an existing DB applies migrations
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_schema_migration_idempotent() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("migrate.db");

    // First open: creates schema.
    {
        let conn = init_db(&db_path).unwrap();
        let repo = Repository::new(conn);
        repo.add_alias("gst", "git status").unwrap();
        assert_eq!(repo.list_aliases().unwrap().len(), 1);
    }

    // Second open: re-running init_db on the same file should be idempotent.
    {
        let conn = init_db(&db_path).unwrap();
        let repo = Repository::new(conn);
        // Data from the first open should still be there.
        let aliases = repo.list_aliases().unwrap();
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0].name, "gst");
        // Can still insert.
        repo.add_alias("gco", "git checkout").unwrap();
        assert_eq!(repo.list_aliases().unwrap().len(), 2);
    }
}

// ──────────────────────────────────────────────────────────────────────
// 15. Export JSON + re-import preserves all fields
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_export_import_preserves_fields() {
    let (src_dir, src_repo) = test_repo();

    let session = make_session("field-host", 1_000_000);
    src_repo.insert_session(&session).unwrap();

    // Insert an entry with executor info and non-zero exit code.
    let mut entry = Entry::new(
        session.id.clone(),
        "npm test".to_string(),
        "/app".to_string(),
        Some(1),
        1_000_000,
        1_005_000,
    );
    entry.executor_type = Some("agent".to_string());
    entry.executor = Some("claude-code".to_string());
    let mut ctx = std::collections::HashMap::new();
    ctx.insert("prompt_id".to_string(), "abc-123".to_string());
    entry.context = Some(ctx);
    src_repo.insert_entry(&entry).unwrap();

    // Export to JSONL.
    let jsonl_path = src_dir.path().join("fields.jsonl");
    {
        let mut file = std::fs::File::create(&jsonl_path).unwrap();
        src_repo
            .stream_export_entries(None, None, |e| {
                writeln!(file, "{}", serde_json::to_string(&e)?)?;
                Ok(())
            })
            .unwrap();
    }

    // Parse the exported line and verify all fields survived serialization.
    let line = std::fs::read_to_string(&jsonl_path).unwrap();
    let parsed: Entry = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(parsed.command, "npm test");
    assert_eq!(parsed.cwd, "/app");
    assert_eq!(parsed.exit_code, Some(1));
    assert_eq!(parsed.duration_ms, 5_000);
    assert_eq!(parsed.executor_type.as_deref(), Some("agent"));
    assert_eq!(parsed.executor.as_deref(), Some("claude-code"));
    let ctx = parsed.context.as_ref().unwrap();
    assert_eq!(ctx.get("prompt_id").unwrap(), "abc-123");
}

// ──────────────────────────────────────────────────────────────────────
// 16. Regex delete + count consistency
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_regex_delete_and_count() {
    let (_tmp, repo) = test_repo();
    let (_, _) = seed(
        &repo,
        "regex-host",
        &[
            "git status",
            "git commit -m fix",
            "git push origin main",
            "cargo build",
            "cargo test",
            "ls -la",
        ],
        1_000_000,
    );

    // Count before delete.
    let count = repo.count_entries_by_pattern("^git", true, None).unwrap();
    assert_eq!(count, 3);

    // Regex delete: remove all "^git" entries.
    let deleted = repo.delete_entries("^git", true, None).unwrap();
    assert_eq!(deleted, 3);

    // Count after delete.
    assert_eq!(count_all_entries(&repo), 3);

    // Remaining entries should be cargo and ls.
    let remaining = repo
        .get_entries_filtered(100, 0, &QueryFilter::default())
        .unwrap();
    assert!(remaining.iter().all(|e| !e.command.starts_with("git")));
}

// ──────────────────────────────────────────────────────────────────────
// 17. Daily activity for stats heatmap
// ──────────────────────────────────────────────────────────────────────

#[test]
fn test_daily_activity() {
    let (_tmp, repo) = test_repo();

    // Use timestamps relative to "now" so the 30-day window always covers them.
    let now_ms = chrono::Utc::now().timestamp_millis();
    let day_ms = 86_400_000_i64;

    let session = make_session("daily-host", now_ms - 3 * day_ms);
    repo.insert_session(&session).unwrap();

    // Insert entries on 3 different "days" (today, yesterday, 2 days ago).
    for day in 0..3 {
        let count = (day + 1) * 2; // 2, 4, 6 entries per day
        let day_base = now_ms - (2 - day) * day_ms; // oldest first
        for i in 0..count {
            let ts = day_base + i * 60_000;
            let entry = Entry::new(
                session.id.clone(),
                format!("cmd_d{day}_e{i}"),
                "/tmp".to_string(),
                Some(0),
                ts,
                ts + 50,
            );
            repo.insert_entry(&entry).unwrap();
        }
    }

    // Get daily activity for last 30 days.
    let activity = repo.get_daily_activity(30, None).unwrap();

    // Should have 3 distinct days.
    assert_eq!(activity.len(), 3);

    // Counts should match what we inserted (2, 4, 6).
    let counts: Vec<i64> = activity.iter().map(|(_, _, c)| *c).collect();
    assert_eq!(counts, vec![2, 4, 6]);

    // Each tuple has (date_string, day_of_week 0-6, count).
    for (date, dow, _count) in &activity {
        assert!(!date.is_empty());
        assert!(*dow <= 6);
    }
}
