use crate::models::Entry;
use crate::repository;
use crate::util::{self, dirs_home, format_duration_ms, shorten_path};

pub struct ReplayParams<'a> {
    pub session: Option<&'a str>,
    pub after: Option<&'a str>,
    pub before: Option<&'a str>,
    pub tag: Option<&'a str>,
    pub exit_code: Option<i32>,
    pub executor: Option<&'a str>,
    pub here: bool,
    pub cwd: Option<&'a str>,
}

pub fn handle_replay(p: &ReplayParams) -> Result<(), Box<dyn std::error::Error>> {
    let repo = repository::Repository::init()?;

    let tag_id = p
        .tag
        .map(|t| repo.get_tag_id_by_name(t))
        .transpose()?
        .flatten();

    // Resolve --here flag
    let cwd_filter = if p.here {
        Some(std::env::current_dir()?.to_string_lossy().to_string())
    } else {
        p.cwd.map(String::from)
    };

    // Parse date filters
    let after_ms = p.after.and_then(|d| util::parse_date_input(d, false));
    let before_ms = p.before.and_then(|d| util::parse_date_input(d, true));

    let scope = ReplayScope {
        session: p.session,
        after: p.after,
        after_ms,
        before: p.before,
        before_ms,
        tag: p.tag,
        here: p.here,
        cwd: p.cwd,
    };
    let (session_filter, effective_after, label) = resolve_replay_scope(&scope);

    let entries = repo.get_replay_entries(
        session_filter.as_deref(),
        &repository::ReplayFilter {
            after: effective_after,
            before: before_ms,
            tag_id,
            exit_code: p.exit_code,
            executor: p.executor,
            cwd: cwd_filter.as_deref(),
        },
    )?;

    if entries.is_empty() {
        println!("\n  No commands found for: {label}");
        return Ok(());
    }

    // Header
    let total = entries.len();
    if util::color_enabled() {
        println!("\n\x1b[1m── Replay: {label} ─────────────────────────────────\x1b[0m");
    } else {
        println!("\n── Replay: {label} ─────────────────────────────────");
    }

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

    print_replay_entries(&entries);

    Ok(())
}

struct ReplayScope<'a> {
    session: Option<&'a str>,
    after: Option<&'a str>,
    after_ms: Option<i64>,
    before: Option<&'a str>,
    before_ms: Option<i64>,
    tag: Option<&'a str>,
    here: bool,
    cwd: Option<&'a str>,
}

/// Resolve the session filter, effective `after` timestamp, and display label
/// based on the combination of CLI flags and environment.
fn resolve_replay_scope(s: &ReplayScope) -> (Option<String>, Option<i64>, String) {
    if let Some(sid) = s.session {
        let label = format!("session {}", &sid[..sid.len().min(8)]);
        return (Some(sid.to_string()), s.after_ms, label);
    }

    if s.after_ms.is_some() || s.before_ms.is_some() || s.tag.is_some() || s.here || s.cwd.is_some()
    {
        // User specified time/filter flags — don't scope to session
        let parts: Vec<String> = [
            s.after.map(|d| format!("after {d}")),
            s.before.map(|d| format!("before {d}")),
            s.tag.map(|t| format!("tag:{t}")),
            if s.here {
                Some("current dir".into())
            } else {
                s.cwd.map(|d| format!("dir:{d}"))
            },
        ]
        .into_iter()
        .flatten()
        .collect();
        let label = if parts.is_empty() {
            "all time".into()
        } else {
            parts.join(", ")
        };
        return (None, s.after_ms, label);
    }

    if let Ok(sid) = std::env::var("SUVADU_SESSION_ID") {
        let label = format!("current session ({})", &sid[..sid.len().min(8)]);
        return (Some(sid), s.after_ms, label);
    }

    // No session env, no flags → last 24h
    let effective_after = Some(chrono::Utc::now().timestamp_millis() - 24 * 60 * 60 * 1000);
    (None, effective_after, "last 24 hours".into())
}

/// Print replay entries with status indicators and a footer summary.
fn print_replay_entries(entries: &[Entry]) {
    let home = dirs_home();
    let color = util::color_enabled();
    let total = entries.len();
    let mut success_count: usize = 0;
    let mut total_duration: i64 = 0;

    for entry in entries {
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

        if color {
            println!(
                " {time}  {dir:<20}  \x1b[{status_color}m{status:<4}\x1b[0m \x1b[2m{duration:>7}\x1b[0m  {cmd}",
                cmd = entry.command
            );
        } else {
            println!(
                " {time}  {dir:<20}  {status:<4} {duration:>7}  {cmd}",
                cmd = entry.command
            );
        }
    }

    // Footer summary
    let failed = total - success_count;
    #[allow(clippy::cast_possible_wrap)]
    let avg_duration = if total > 0 {
        total_duration / total as i64
    } else {
        0
    };
    let avg = format_duration_ms(avg_duration);
    if color {
        println!("\n\x1b[1m── {total} commands  │  {success_count} passed  │  {failed} failed  │  Avg {avg} ──\x1b[0m\n");
    } else {
        println!("\n── {total} commands  │  {success_count} passed  │  {failed} failed  │  Avg {avg} ──\n");
    }
}

#[cfg(test)]
mod tests {
    use crate::models::{Entry, Session};
    use crate::repository::{ReplayFilter, Repository};
    use crate::test_utils::test_repo;

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
        let (_dir, repo) = test_repo();
        seed_session(&repo, "sess-abc");
        seed_entry(&repo, "sess-abc", "git status", 0, 1_700_000_001_000);
        seed_entry(&repo, "sess-abc", "cargo build", 0, 1_700_000_002_000);
        seed_entry(&repo, "sess-abc", "cargo test", 1, 1_700_000_003_000);

        let entries = repo
            .get_replay_entries(Some("sess-abc"), &ReplayFilter::default())
            .unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].command, "git status");
        assert_eq!(entries[2].command, "cargo test");
    }

    #[test]
    fn test_replay_entries_exit_code_filter() {
        let (_dir, repo) = test_repo();
        seed_session(&repo, "sess-def");
        seed_entry(&repo, "sess-def", "ls", 0, 1_700_000_001_000);
        seed_entry(&repo, "sess-def", "bad", 1, 1_700_000_002_000);
        seed_entry(&repo, "sess-def", "ok", 0, 1_700_000_003_000);

        // Only failures
        let entries = repo
            .get_replay_entries(
                Some("sess-def"),
                &ReplayFilter {
                    exit_code: Some(1),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].command, "bad");
    }

    #[test]
    fn test_replay_entries_time_filter() {
        let (_dir, repo) = test_repo();
        seed_session(&repo, "sess-time");
        seed_entry(&repo, "sess-time", "early", 0, 1_700_000_000_000);
        seed_entry(&repo, "sess-time", "late", 0, 1_700_000_100_000);

        // Only after the first entry
        let entries = repo
            .get_replay_entries(
                None,
                &ReplayFilter {
                    after: Some(1_700_000_050_000),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].command, "late");
    }

    #[test]
    fn test_replay_empty_session() {
        let (_dir, repo) = test_repo();
        seed_session(&repo, "empty-sess");

        let entries = repo
            .get_replay_entries(Some("empty-sess"), &ReplayFilter::default())
            .unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_replay_entries_cwd_filter() {
        let (_dir, repo) = test_repo();
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
                &ReplayFilter {
                    cwd: Some("/home/user/project"),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].command, "make");
    }
}
