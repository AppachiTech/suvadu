//! Honest reporting of whether Suvadu is actually capturing history.
//!
//! Shared by `suv status` and `suv doctor`. Everything here is pure: callers
//! gather the facts (config, rc files, database) and this module decides what
//! may honestly be claimed. Configuration is never treated as evidence of
//! capture, and a session environment variable is never treated as proof.

/// A record captured within this many seconds counts as live proof that the
/// hook is currently working. Anything older only proves it worked once.
pub const RECENT_RECORD_WINDOW_SECS: i64 = 24 * 60 * 60;

/// The harmless command the documented verification sequence runs.
pub const VERIFY_MARKER: &str = "suvadu-capture-check";

/// Whether recording is *allowed* right now. This is configuration state
/// only — it is never evidence that anything was captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingState {
    Enabled,
    /// `enabled = false` in the config file (`suv disable`).
    DisabledInConfig,
    /// `SUVADU_PAUSED` is set in this shell (`eval $(suv pause)`).
    PausedInShell,
    DisabledAndPaused,
}

impl RecordingState {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Enabled => "enabled in config",
            Self::DisabledInConfig => "disabled in config",
            Self::PausedInShell => "paused in this shell",
            Self::DisabledAndPaused => "disabled in config and paused in this shell",
        }
    }

    pub const fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

/// Classify configuration state. Disabled-in-config and paused-in-this-shell
/// are different problems with different repairs, so they stay distinct.
pub const fn recording_state(config_enabled: bool, paused_in_shell: bool) -> RecordingState {
    match (config_enabled, paused_in_shell) {
        (true, false) => RecordingState::Enabled,
        (true, true) => RecordingState::PausedInShell,
        (false, false) => RecordingState::DisabledInConfig,
        (false, true) => RecordingState::DisabledAndPaused,
    }
}

/// Copy-pasteable repairs for a state that is not recording.
///
/// `suv enable` only flips `enabled` in the config file — it does **not**
/// clear `SUVADU_PAUSED`, which `eval $(suv pause)` exported into the current
/// shell — so a paused shell is told to unset the variable instead.
pub fn recording_fixes(state: RecordingState) -> Vec<String> {
    let enable =
        || "suv enable                 \u{2014} re-enable recording in the config file".to_string();
    let unpause = || {
        "unset SUVADU_PAUSED        \u{2014} clears the pause `eval $(suv pause)` set in this shell (re-enabling in config does not clear it)"
            .to_string()
    };
    match state {
        RecordingState::Enabled => vec![],
        RecordingState::DisabledInConfig => vec![enable()],
        RecordingState::PausedInShell => vec![unpause()],
        RecordingState::DisabledAndPaused => vec![enable(), unpause()],
    }
}

/// Observed facts about shell capture, gathered by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaptureFacts {
    /// A `suv init <shell>` line was found in the shell's rc file.
    pub hook_configured: bool,
    /// `SUVADU_SESSION_ID` is exported in this shell. Deliberately *not* an
    /// input to the verdict: it proves the hook ran once in this shell, not
    /// that any command reached the database.
    pub session_env_present: bool,
    /// Number of stored non-agent (shell) records.
    pub shell_records: i64,
    /// Age of the newest stored shell record, in seconds.
    pub newest_shell_record_age_secs: Option<i64>,
}

/// What can honestly be claimed about capture, strongest evidence first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureEvidence {
    /// A shell record arrived recently — the hook is demonstrably working.
    RecentRecord { age_secs: i64 },
    /// Records exist but all are old: the hook was observed working before.
    EarlierRecord { age_secs: i64 },
    /// Configuration is in place; nothing has been captured through it yet.
    ConfigurationPresent,
    /// No configuration and no records.
    NotVerified,
}

impl CaptureEvidence {
    pub fn headline(self) -> String {
        match self {
            Self::RecentRecord { age_secs } => {
                format!("most recent record received {} ago", format_age(age_secs))
            }
            Self::EarlierRecord { age_secs } => format!(
                "hook observed earlier (newest record {} old) \u{2014} not verified since",
                format_age(age_secs)
            ),
            Self::ConfigurationPresent => {
                "configuration present, capture not yet verified".to_string()
            }
            Self::NotVerified => "not yet verified \u{2014} no captured command found".to_string(),
        }
    }

    /// `true` only when a stored record proves capture is working now.
    pub const fn is_proven(self) -> bool {
        matches!(self, Self::RecentRecord { .. })
    }
}

/// Decide what the stored evidence supports. Configuration alone, and the
/// session environment variable alone, never reach `RecentRecord`.
pub const fn capture_evidence(facts: &CaptureFacts) -> CaptureEvidence {
    match facts.newest_shell_record_age_secs {
        Some(age) if facts.shell_records > 0 && age <= RECENT_RECORD_WINDOW_SECS => {
            CaptureEvidence::RecentRecord { age_secs: age }
        }
        Some(age) if facts.shell_records > 0 => CaptureEvidence::EarlierRecord { age_secs: age },
        _ if facts.hook_configured => CaptureEvidence::ConfigurationPresent,
        _ => CaptureEvidence::NotVerified,
    }
}

/// Caveats to print alongside the verdict.
pub fn evidence_notes(facts: &CaptureFacts) -> Vec<String> {
    let mut notes = Vec::new();
    if facts.session_env_present && !capture_evidence(facts).is_proven() {
        notes.push(
            "SUVADU_SESSION_ID is set in this shell, but a session variable is not proof that any command was stored."
                .to_string(),
        );
    }
    notes
}

/// Format an age in seconds as a coarse human string ("3 minutes").
pub fn format_age(secs: i64) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    let (value, unit) = if secs >= DAY {
        (secs / DAY, "day")
    } else if secs >= HOUR {
        (secs / HOUR, "hour")
    } else if secs >= MINUTE {
        (secs / MINUTE, "minute")
    } else {
        (secs.max(0), "second")
    };
    if value == 1 {
        format!("1 {unit}")
    } else {
        format!("{value} {unit}s")
    }
}

/// The rc file `suv init <shell>` writes to, for the shells Suvadu supports.
pub const fn rc_file_for_shell(shell_name: &str) -> Option<&'static str> {
    match shell_name.as_bytes() {
        b"zsh" => Some(".zshrc"),
        b"bash" => Some(".bashrc"),
        _ => None,
    }
}

/// `true` if an rc file's contents install Suvadu's shell hooks.
pub fn rc_hook_configured(rc_contents: &str) -> bool {
    rc_contents.contains("suv init")
}

/// The current shell's short name, from `$SHELL`.
pub fn current_shell_name() -> String {
    std::env::var("SHELL")
        .unwrap_or_default()
        .rsplit('/')
        .next()
        .unwrap_or("unknown")
        .to_string()
}

/// Read `$HOME/<rc file>` for the current shell and report whether Suvadu's
/// hooks are installed there. Read-only; never writes to the user's rc files.
pub fn shell_hook_configured() -> bool {
    let (Ok(home), Some(rc)) = (
        std::env::var("HOME"),
        rc_file_for_shell(&current_shell_name()),
    ) else {
        return false;
    };
    std::fs::read_to_string(std::path::Path::new(&home).join(rc))
        .is_ok_and(|contents| rc_hook_configured(&contents))
}

/// The exact sequence that proves capture end to end.
pub fn verification_steps() -> Vec<String> {
    vec![
        format!("1. In the shell you want recorded, run:  echo {VERIFY_MARKER}"),
        format!("2. Search for it:                        suv get {VERIFY_MARKER}"),
        "3. Confirm the stored record:            suv history --limit 3".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean() -> CaptureFacts {
        CaptureFacts {
            hook_configured: false,
            session_env_present: false,
            shell_records: 0,
            newest_shell_record_age_secs: None,
        }
    }

    #[test]
    fn ages_are_formatted_in_human_units() {
        assert_eq!(format_age(5), "5 seconds");
        assert_eq!(format_age(90), "1 minute");
        assert_eq!(format_age(7200), "2 hours");
        assert_eq!(format_age(60 * 60 * 24 * 3), "3 days");
    }

    #[test]
    fn rc_files_are_known_for_supported_shells_only() {
        assert_eq!(rc_file_for_shell("zsh"), Some(".zshrc"));
        assert_eq!(rc_file_for_shell("bash"), Some(".bashrc"));
        assert_eq!(rc_file_for_shell("fish"), None);
    }

    #[test]
    fn rc_hook_is_detected_by_the_suv_init_line() {
        assert!(rc_hook_configured("eval \"$(suv init zsh)\"\n"));
        assert!(!rc_hook_configured("# suvadu is not set up here\n"));
    }

    #[test]
    fn clean_setup_is_not_verified() {
        assert_eq!(capture_evidence(&clean()), CaptureEvidence::NotVerified);
        assert!(!capture_evidence(&clean()).is_proven());
    }

    #[test]
    fn session_env_var_alone_is_not_proof() {
        let facts = CaptureFacts {
            session_env_present: true,
            ..clean()
        };
        assert_eq!(capture_evidence(&facts), CaptureEvidence::NotVerified);
        assert!(!capture_evidence(&facts).is_proven());
        assert!(
            evidence_notes(&facts)
                .iter()
                .any(|n| n.contains("not proof")),
            "the session variable must be called out as insufficient: {:?}",
            evidence_notes(&facts)
        );
    }

    #[test]
    fn rc_hook_without_any_record_is_configuration_only() {
        let facts = CaptureFacts {
            hook_configured: true,
            session_env_present: true,
            ..clean()
        };
        assert_eq!(
            capture_evidence(&facts),
            CaptureEvidence::ConfigurationPresent
        );
        assert!(!capture_evidence(&facts).is_proven());
    }

    #[test]
    fn a_recent_record_is_proof_of_capture() {
        let facts = CaptureFacts {
            hook_configured: true,
            shell_records: 12,
            newest_shell_record_age_secs: Some(90),
            ..clean()
        };
        assert_eq!(
            capture_evidence(&facts),
            CaptureEvidence::RecentRecord { age_secs: 90 }
        );
        assert!(capture_evidence(&facts).is_proven());
    }

    #[test]
    fn an_old_record_is_reported_as_observed_earlier_not_as_live_capture() {
        let age = RECENT_RECORD_WINDOW_SECS + 60;
        let facts = CaptureFacts {
            hook_configured: false,
            shell_records: 3,
            newest_shell_record_age_secs: Some(age),
            ..clean()
        };
        assert_eq!(
            capture_evidence(&facts),
            CaptureEvidence::EarlierRecord { age_secs: age }
        );
        assert!(!capture_evidence(&facts).is_proven());
    }

    #[test]
    fn recording_state_separates_disabled_config_from_a_paused_shell() {
        assert_eq!(recording_state(true, false), RecordingState::Enabled);
        assert_eq!(
            recording_state(false, false),
            RecordingState::DisabledInConfig
        );
        assert_eq!(recording_state(true, true), RecordingState::PausedInShell);
        assert_eq!(
            recording_state(false, true),
            RecordingState::DisabledAndPaused
        );
    }

    #[test]
    fn pausing_is_repaired_by_unsetting_the_env_var_not_by_suv_enable() {
        let fixes = recording_fixes(RecordingState::PausedInShell);
        let joined = fixes.join("\n");
        assert!(
            joined.contains("unset SUVADU_PAUSED"),
            "paused shells must be told to unset the variable: {joined}"
        );
        assert!(
            !joined.contains("suv enable"),
            "`suv enable` does not clear SUVADU_PAUSED, so it must not be the advice: {joined}"
        );
    }

    #[test]
    fn disabled_config_is_repaired_by_suv_enable() {
        let joined = recording_fixes(RecordingState::DisabledInConfig).join("\n");
        assert!(joined.contains("suv enable"), "{joined}");
        assert!(!joined.contains("unset SUVADU_PAUSED"), "{joined}");
    }

    #[test]
    fn disabled_and_paused_reports_both_repairs() {
        let joined = recording_fixes(RecordingState::DisabledAndPaused).join("\n");
        assert!(joined.contains("suv enable"), "{joined}");
        assert!(joined.contains("unset SUVADU_PAUSED"), "{joined}");
    }

    #[test]
    fn enabled_recording_needs_no_repair() {
        assert!(recording_fixes(RecordingState::Enabled).is_empty());
    }

    #[test]
    fn verification_sequence_runs_searches_then_confirms() {
        let steps = verification_steps();
        assert_eq!(steps.len(), 3, "{steps:?}");
        assert!(
            steps[0].contains(&format!("echo {VERIFY_MARKER}")),
            "step 1 must run a harmless command: {}",
            steps[0]
        );
        assert!(
            steps[1].contains(&format!("suv get {VERIFY_MARKER}")),
            "step 2 must search for it: {}",
            steps[1]
        );
        assert!(
            steps[2].contains("suv history"),
            "step 3 must confirm the stored record: {}",
            steps[2]
        );
    }

    #[test]
    fn recording_state_labels_never_claim_capture() {
        for state in [
            RecordingState::Enabled,
            RecordingState::DisabledInConfig,
            RecordingState::PausedInShell,
            RecordingState::DisabledAndPaused,
        ] {
            let label = state.label();
            assert!(
                !label.to_lowercase().contains("is being recorded"),
                "configuration must not claim capture: {label}"
            );
        }
        assert_eq!(RecordingState::Enabled.label(), "enabled in config");
    }

    #[test]
    fn evidence_headlines_describe_the_proof_they_have() {
        assert!(CaptureEvidence::RecentRecord { age_secs: 30 }
            .headline()
            .contains("most recent record received"));
        assert!(CaptureEvidence::EarlierRecord { age_secs: 900_000 }
            .headline()
            .contains("hook observed"));
        assert!(CaptureEvidence::ConfigurationPresent
            .headline()
            .contains("configuration present"));
        assert!(CaptureEvidence::NotVerified
            .headline()
            .contains("not yet verified"));
    }
}
