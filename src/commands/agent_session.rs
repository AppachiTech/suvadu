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

pub fn import_native(path: &std::path::Path, expected: Option<&str>) -> Result<serde_json::Value> {
    let paused = config::is_paused();
    let repo = crate::repository::Repository::init()?;
    let policies =
        std::cell::RefCell::new(std::collections::HashMap::<String, CapturePolicy>::new());
    Ok(repo.import_codex_session(path, expected, |cwd| {
        if let Some(policy) = policies.borrow().get(cwd) {
            return Ok(policy.clone());
        }
        let policy = config::load_config_for_dir(std::path::Path::new(cwd))
            .map(|cfg| capture_policy(cfg, paused))
            .map_err(|error| crate::db::DbError::Validation(error.to_string()))?;
        policies.borrow_mut().insert(cwd.to_owned(), policy.clone());
        Ok(policy)
    })?)
}

pub fn import(path: &std::path::Path) -> Result<()> {
    loop {
        let result = import_native(path, None)?;
        println!("{}", serde_json::to_string_pretty(&result)?);
        if result["has_more"] != true {
            break;
        }
    }
    Ok(())
}

pub fn list(limit: usize, offset: usize) -> Result<()> {
    let repo = crate::repository::Repository::init()?;
    let result = repo.list_ai_sessions(limit, offset, &[])?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    if offset == 0 && result["sessions"].as_array().is_some_and(Vec::is_empty) {
        eprintln!("No captured AI sessions yet. {}\n  Finish a fresh local Codex turn, then run `suv agent sessions` again. If still empty, inspect the Codex Output log for hook errors or a missing transcript path.", crate::upgrade_notice::CODEX_SETUP);
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
    let count = repo.delete_ai_session(id)?;
    println!("Deleted session {id} and {count} session/command records. Native agent transcripts are unchanged.");
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
