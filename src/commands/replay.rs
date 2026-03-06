use crate::repository;
use crate::util::{self, dirs_home, format_duration_ms, shorten_path};

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap
)]
pub fn handle_replay(
    session: Option<&str>,
    after: Option<&str>,
    before: Option<&str>,
    tag: Option<&str>,
    exit_code: Option<i32>,
    executor: Option<&str>,
    here: bool,
    cwd: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = repository::Repository::init()?;

    let tag_id = tag
        .map(|t| repo.get_tag_id_by_name(t))
        .transpose()?
        .flatten();

    // Resolve --here flag
    let cwd_filter = if here {
        Some(std::env::current_dir()?.to_string_lossy().to_string())
    } else {
        cwd.map(String::from)
    };

    // Parse date filters
    let after_ms = after.and_then(|d| util::parse_date_input(d, false));
    let before_ms = before.and_then(|d| util::parse_date_input(d, true));

    // Determine session filter: explicit --session, or fallback to env var, or fallback to last 24h
    let session_filter;
    let mut effective_after = after_ms;
    let label: String;

    if let Some(sid) = session {
        session_filter = Some(sid.to_string());
        label = format!("session {}", &sid[..sid.len().min(8)]);
    } else if after_ms.is_some() || before_ms.is_some() || tag.is_some() || here || cwd.is_some() {
        // User specified time/filter flags — don't scope to session
        session_filter = None;
        let parts: Vec<String> = [
            after.map(|d| format!("after {d}")),
            before.map(|d| format!("before {d}")),
            tag.map(|t| format!("tag:{t}")),
            if here {
                Some("current dir".into())
            } else {
                cwd.map(|d| format!("dir:{d}"))
            },
        ]
        .into_iter()
        .flatten()
        .collect();
        label = if parts.is_empty() {
            "all time".into()
        } else {
            parts.join(", ")
        };
    } else if let Ok(sid) = std::env::var("SUVADU_SESSION_ID") {
        session_filter = Some(sid.clone());
        label = format!("current session ({})", &sid[..sid.len().min(8)]);
    } else {
        // No session env, no flags → last 24h
        session_filter = None;
        effective_after = Some(chrono::Utc::now().timestamp_millis() - 24 * 60 * 60 * 1000);
        label = "last 24 hours".into();
    }

    let entries = repo.get_replay_entries(
        session_filter.as_deref(),
        effective_after,
        before_ms,
        tag_id,
        exit_code,
        executor,
        cwd_filter.as_deref(),
    )?;

    if entries.is_empty() {
        println!("\n  No commands found for: {label}");
        return Ok(());
    }

    let home = dirs_home();

    // Header
    let total = entries.len();
    println!("\n\x1b[1m── Replay: {label} ─────────────────────────────────\x1b[0m");

    // Session info if scoped to a session
    if let Some(ref sid) = session_filter {
        if let Ok(Some(sess)) = repo.get_session(sid) {
            let tag_info = if let Ok(Some(tname)) = repo.get_tag_by_session(sid) {
                format!("  │  Tag: {tname}")
            } else {
                String::new()
            };
            println!(
                "   Session {}  │  Host: {}{tag_info}  │  {} commands",
                &sess.id[..sess.id.len().min(8)],
                sess.hostname,
                total
            );
        }
    }
    println!();

    // Entries
    let mut success_count: usize = 0;
    let mut total_duration: i64 = 0;

    for entry in &entries {
        let time = chrono::DateTime::from_timestamp_millis(entry.started_at).map_or_else(
            || "??:??:??".into(),
            |dt| {
                dt.with_timezone(&chrono::Local)
                    .format("%H:%M:%S")
                    .to_string()
            },
        );

        let dir = shorten_path(&entry.cwd, &home);
        let duration = format_duration_ms(entry.duration_ms);

        let (status, status_color) = match entry.exit_code {
            Some(0) => {
                success_count += 1;
                ("\u{2713}".to_string(), "32") // green checkmark
            }
            Some(code) => (format!("\u{2717}{code}"), "31"), // red X + code
            None => ("\u{2022}".to_string(), "33"),          // yellow bullet
        };

        total_duration += entry.duration_ms;

        println!(
            " {time}  {dir:<20}  \x1b[{status_color}m{status:<4}\x1b[0m \x1b[2m{duration:>7}\x1b[0m  {cmd}",
            cmd = entry.command
        );
    }

    // Footer summary
    let failed = total - success_count;
    let avg_duration = if total > 0 {
        total_duration / total as i64
    } else {
        0
    };
    println!(
        "\n\x1b[1m── {} commands  │  {} passed  │  {} failed  │  Avg {} ──\x1b[0m\n",
        total,
        success_count,
        failed,
        format_duration_ms(avg_duration)
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::db;
    use crate::models::{Entry, Session};
    use crate::repository::Repository;

    fn test_repo() -> Repository {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let db_path = temp_dir.keep().join("test.db");
        let conn = db::init_db(&db_path).unwrap();
        Repository::new(conn)
    }

    fn seed_session(repo: &Repository, session_id: &str) {
        let session = Session {
            id: session_id.to_string(),
            hostname: "test-host".to_string(),
            created_at: 1_700_000_000_000,
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();
    }

    fn seed_entry(repo: &Repository, session_id: &str, cmd: &str, exit_code: i32, started_at: i64) {
        let entry = Entry::new(
            session_id.to_string(),
            cmd.to_string(),
            "/home/user".to_string(),
            Some(exit_code),
            started_at,
            started_at + 500,
        );
        repo.insert_entry(&entry).unwrap();
    }

    #[test]
    fn test_replay_entries_by_session() {
        let repo = test_repo();
        seed_session(&repo, "sess-abc");
        seed_entry(&repo, "sess-abc", "git status", 0, 1_700_000_001_000);
        seed_entry(&repo, "sess-abc", "cargo build", 0, 1_700_000_002_000);
        seed_entry(&repo, "sess-abc", "cargo test", 1, 1_700_000_003_000);

        let entries = repo
            .get_replay_entries(Some("sess-abc"), None, None, None, None, None, None)
            .unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].command, "git status");
        assert_eq!(entries[2].command, "cargo test");
    }

    #[test]
    fn test_replay_entries_exit_code_filter() {
        let repo = test_repo();
        seed_session(&repo, "sess-def");
        seed_entry(&repo, "sess-def", "ls", 0, 1_700_000_001_000);
        seed_entry(&repo, "sess-def", "bad", 1, 1_700_000_002_000);
        seed_entry(&repo, "sess-def", "ok", 0, 1_700_000_003_000);

        // Only failures
        let entries = repo
            .get_replay_entries(Some("sess-def"), None, None, None, Some(1), None, None)
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].command, "bad");
    }

    #[test]
    fn test_replay_entries_time_filter() {
        let repo = test_repo();
        seed_session(&repo, "sess-time");
        seed_entry(&repo, "sess-time", "early", 0, 1_700_000_000_000);
        seed_entry(&repo, "sess-time", "late", 0, 1_700_000_100_000);

        // Only after the first entry
        let entries = repo
            .get_replay_entries(None, Some(1_700_000_050_000), None, None, None, None, None)
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].command, "late");
    }

    #[test]
    fn test_replay_empty_session() {
        let repo = test_repo();
        seed_session(&repo, "empty-sess");

        let entries = repo
            .get_replay_entries(Some("empty-sess"), None, None, None, None, None, None)
            .unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_replay_entries_cwd_filter() {
        let repo = test_repo();
        seed_session(&repo, "sess-cwd");

        let mut e1 = Entry::new(
            "sess-cwd".to_string(),
            "make".to_string(),
            "/home/user/project".to_string(),
            Some(0),
            1_700_000_001_000,
            1_700_000_001_500,
        );
        repo.insert_entry(&e1).unwrap();

        e1.command = "ls".to_string();
        e1.cwd = "/tmp".to_string();
        e1.started_at = 1_700_000_002_000;
        e1.ended_at = 1_700_000_002_500;
        repo.insert_entry(&e1).unwrap();

        let entries = repo
            .get_replay_entries(
                None,
                None,
                None,
                None,
                None,
                None,
                Some("/home/user/project"),
            )
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].command, "make");
    }
}
