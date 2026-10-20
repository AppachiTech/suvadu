//! Explicit session import and agent-neutral local session access.
use crate::{ai_sessions::CapturePolicy, config};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn capture_policy(cfg: config::Config, paused: bool) -> CapturePolicy {
    CapturePolicy {
        enabled: cfg.enabled && !paused,
        paused,
        redact: cfg.redaction.enabled,
        extra_patterns: cfg.redaction.extra_patterns,
        exclusions: cfg.exclusions,
        max_chars: cfg.agent.prompt_capture_max_chars,
    }
}

/// The capture policy for records from `cwd`. A config file that does not
/// load falls back exactly as recording a command does -- the global config,
/// or the defaults with redaction on -- and is noted for `suv doctor`, so a
/// typo in a `.suvadu.toml` cannot fail every import and lose the session.
pub fn capture_policy_for_dir(cwd: &str, paused: bool) -> CapturePolicy {
    capture_policy(
        config::capture_config_for_dir(std::path::Path::new(cwd)),
        paused,
    )
}

/// Take a git snapshot of `cwd` for the agent session `session_id` (the id
/// its commands are recorded under: `claude-<id>`, `codex-<id>`,
/// `opencode-<id>`), at a session's first prompt (`Start`, kept once) or a
/// turn end (`End`, replacing the last one).
///
/// Called from agent hooks, so it never fails them: paused capture, a
/// project with recording or `agent.record_git_snapshots` off, a directory
/// outside a repository, a missing or slow `git`, all quietly record
/// nothing, and a database error is only reported on stderr. A `Start` the
/// session already has costs one lookup and no `git` process. A turn end
/// whose hook names no directory uses the one of the first prompt.
pub fn record_git_snapshot(
    session_id: &str,
    cwd: Option<&str>,
    kind: crate::ai_sessions::git_snapshot::SnapshotKind,
) {
    if config::is_paused() {
        return;
    }
    if let Err(error) = try_record_git_snapshot(session_id, cwd, kind) {
        eprintln!("suvadu: git snapshot: {error}");
    }
}

fn try_record_git_snapshot(
    session_id: &str,
    cwd: Option<&str>,
    kind: crate::ai_sessions::git_snapshot::SnapshotKind,
) -> Result<()> {
    use crate::ai_sessions::git_snapshot::{self, GitSnapshot, SnapshotKind};
    let cwd = cwd.filter(|cwd| !cwd.is_empty());
    // Settled before the database is opened, so a project that turned
    // snapshots off costs nothing more.
    if let Some(cwd) = cwd {
        let cfg = config::capture_config_for_dir(std::path::Path::new(cwd));
        if !cfg.enabled || !cfg.agent.record_git_snapshots {
            return Ok(());
        }
    }
    let repo = crate::repository::Repository::init()?;
    if kind == SnapshotKind::Start && repo.has_git_snapshot(session_id, SnapshotKind::Start)? {
        return Ok(());
    }
    let cwd = match cwd {
        Some(cwd) => cwd.to_string(),
        None => match repo.git_snapshots(session_id)?.0 {
            Some(start) => start.cwd,
            None => return Ok(()),
        },
    };
    let cfg = config::capture_config_for_dir(std::path::Path::new(&cwd));
    if !cfg.enabled || !cfg.agent.record_git_snapshots {
        return Ok(());
    }
    let Some(status) = git_snapshot::observe(std::path::Path::new(&cwd), git_snapshot::GIT_TIMEOUT)
    else {
        return Ok(());
    };
    let snapshot = GitSnapshot::from_status(
        session_id,
        kind,
        chrono::Utc::now().timestamp_millis(),
        &cwd,
        status,
        &capture_policy(cfg, false),
    );
    repo.save_git_snapshot(&snapshot)?;
    Ok(())
}

pub fn import_native(path: &std::path::Path, expected: Option<&str>) -> Result<serde_json::Value> {
    let paused = config::is_paused();
    let repo = crate::repository::Repository::init()?;
    let policies =
        std::cell::RefCell::new(std::collections::HashMap::<String, CapturePolicy>::new());
    Ok(repo.import_codex_session(path, expected, |cwd| {
        if let Some(policy) = policies.borrow().get(cwd) {
            return Ok(policy.clone());
        }
        let policy = capture_policy_for_dir(cwd, paused);
        policies.borrow_mut().insert(cwd.to_owned(), policy.clone());
        Ok(policy)
    })?)
}

pub fn import_native_claude(
    path: &std::path::Path,
    expected: Option<&str>,
) -> Result<serde_json::Value> {
    let paused = config::is_paused();
    let repo = crate::repository::Repository::init()?;
    let policies =
        std::cell::RefCell::new(std::collections::HashMap::<String, CapturePolicy>::new());
    Ok(repo.import_claude_session(path, expected, |cwd| {
        if let Some(policy) = policies.borrow().get(cwd) {
            return Ok(policy.clone());
        }
        let policy = capture_policy_for_dir(cwd, paused);
        policies.borrow_mut().insert(cwd.to_owned(), policy.clone());
        Ok(policy)
    })?)
}

pub fn import_native_opencode(
    session_id: &str,
    cwd: &str,
    messages_json: &str,
) -> Result<serde_json::Value> {
    let paused = config::is_paused();
    let repo = crate::repository::Repository::init()?;
    let policy = capture_policy_for_dir(cwd, paused);
    Ok(repo.import_opencode_session(session_id, cwd, messages_json, |_| Ok(policy.clone()))?)
}

pub fn import(path: &std::path::Path) -> Result<()> {
    let agent = detect_native_agent(path)?;
    loop {
        let result = match agent {
            NativeAgent::Codex => import_native(path, None)?,
            NativeAgent::Claude => import_native_claude(path, None)?,
        };
        println!("{}", serde_json::to_string_pretty(&result)?);
        if result["has_more"] != true {
            break;
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum NativeAgent {
    Codex,
    Claude,
}

fn detect_native_agent(path: &std::path::Path) -> Result<NativeAgent> {
    use std::io::{BufRead, Read};

    const MAX_SCAN: u64 = 16 * 1024 * 1024;
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file.take(MAX_SCAN + 1));
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        if line.len() as u64 > MAX_SCAN {
            return Err("Native transcript record exceeds 16 MiB".into());
        }
        let record: serde_json::Value = serde_json::from_slice(&line)?;
        if record.get("payload").is_some()
            && matches!(
                record["type"].as_str(),
                Some("session_meta" | "turn_context" | "event_msg" | "response_item")
            )
        {
            return Ok(NativeAgent::Codex);
        }
        if record
            .get("sessionId")
            .and_then(serde_json::Value::as_str)
            .is_some()
        {
            return Ok(NativeAgent::Claude);
        }
    }
    Err("Could not identify transcript as Codex or Claude Code JSONL within 16 MiB".into())
}

pub fn list(limit: usize, offset: usize) -> Result<()> {
    let repo = crate::repository::Repository::init()?;
    let result = repo.list_ai_sessions(limit, offset, &[])?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    if offset == 0 && result["sessions"].as_array().is_some_and(Vec::is_empty) {
        eprintln!("No captured AI sessions yet.\n  {}\n  {}\n  Finish a fresh local agent turn, then run `suv agent sessions` again. If still empty, inspect the agent's hook/debug log for errors or a missing transcript path.", crate::upgrade_notice::CODEX_SETUP, crate::upgrade_notice::CLAUDE_SETUP);
    }
    Ok(())
}
pub fn get(id: &str, limit: usize, offset: usize) -> Result<()> {
    let repo = crate::repository::Repository::init()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&repo.get_ai_session(id, limit, offset, &[])?)?
    );
    Ok(())
}
pub fn delete(id: &str) -> Result<()> {
    let repo = crate::repository::Repository::init()?;
    // Say what is about to go before it goes: a session id on its own gives
    // no sense of how much evidence hangs off it.
    let preview = repo.ai_session_deletion_preview(id)?;
    println!(
        "Deleting session {id}: {} captured event(s), {} recorded command(s), {} saved summary/summaries, {} git snapshot(s).",
        preview.events, preview.commands, preview.summaries, preview.snapshots
    );
    let count = repo.delete_ai_session(id)?;
    println!("Deleted session {id} and {count} session/command records. Native agent transcripts are unchanged.");
    println!(
        "This is not a secure erase: the rows keep their pages in history.db until a VACUUM \
         reuses them, and any backup in the backups directory still contains this session."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_policy_preserves_project_privacy_settings_and_pause() {
        let mut cfg = config::Config::default();
        cfg.redaction.enabled = false;
        cfg.redaction.extra_patterns = vec!["private-token".into()];
        cfg.exclusions = vec!["password".into()];
        cfg.agent.prompt_capture_max_chars = 25;
        let policy = capture_policy(cfg.clone(), false);
        assert!(policy.enabled);
        assert!(!policy.redact);
        assert_eq!(policy.extra_patterns, ["private-token"]);
        assert_eq!(policy.exclusions, ["password"]);
        assert_eq!(policy.max_chars, 25);
        assert!(!capture_policy(cfg.clone(), true).enabled);
        cfg.enabled = false;
        assert!(!capture_policy(cfg, false).enabled);
    }
}
