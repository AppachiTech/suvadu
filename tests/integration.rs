use std::io::Write;

use suvadu::db::init_db;
use suvadu::models::{Entry, Session};
use suvadu::repository::Repository;
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
    repo.count_filtered(&suvadu::repository::QueryFilter::default())
        .unwrap()
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
