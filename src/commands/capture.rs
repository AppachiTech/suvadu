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
///
/// Counts are split by **provenance** and by **shell session** because the
/// three questions are different: what is stored, what a live hook was seen
/// writing, and whether the shell being diagnosed is the one writing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaptureFacts {
    /// A `suv init <shell>` line was found in the rc file of the shell being
    /// diagnosed.
    pub hook_configured: bool,
    /// `SUVADU_SESSION_ID` is exported in this shell. Deliberately *not* an
    /// input to the verdict: it proves the hook ran once in this shell, not
    /// that any command reached the database.
    pub session_env_present: bool,
    /// Non-agent records that no importer wrote — the only rows a live hook
    /// could have produced.
    pub live_records: i64,
    /// Age of the newest live record, in seconds.
    pub newest_live_record_age_secs: Option<i64>,
    /// Live records belonging to the shell session being diagnosed.
    pub session_records: i64,
    /// Age of the newest live record from that session, in seconds.
    pub newest_session_record_age_secs: Option<i64>,
    /// Non-agent records an importer wrote. Searchable history — and it may
    /// legitimately be dated "now" — but never evidence of capture.
    pub imported_records: i64,
}

/// What can honestly be claimed about capture, strongest evidence first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureEvidence {
    /// A live record arrived recently — the hook is demonstrably working.
    /// `same_session` is `true` when it came from the shell being diagnosed.
    RecentRecord { age_secs: i64, same_session: bool },
    /// A live record arrived recently, but from a different shell session,
    /// and the shell being diagnosed has no hook in its rc file. Some shell
    /// is being captured; this one is not known to be.
    RecordFromAnotherShell { age_secs: i64 },
    /// Live records exist but all are old: the hook was observed working
    /// before, not since.
    EarlierRecord { age_secs: i64 },
    /// Nothing was captured; the stored shell history all came from an
    /// import. Stored history is not evidence of capture.
    ImportedHistoryOnly { records: i64 },
    /// Configuration is in place; nothing has been captured through it yet.
    ConfigurationPresent,
    /// No configuration and no records.
    NotVerified,
}

impl CaptureEvidence {
    pub fn headline(self) -> String {
        match self {
            Self::RecentRecord {
                age_secs,
                same_session: true,
            } => format!(
                "most recent record received {} ago, from this shell session",
                format_age(age_secs)
            ),
            Self::RecentRecord {
                age_secs,
                same_session: false,
            } => format!(
                "most recent record received {} ago, from another shell session",
                format_age(age_secs)
            ),
            Self::RecordFromAnotherShell { age_secs } => format!(
                "a record arrived {} ago, but from another shell \u{2014} this shell has no hook installed, so its own capture is not verified",
                format_age(age_secs)
            ),
            Self::EarlierRecord { age_secs } => format!(
                "hook observed earlier (newest record {} old) \u{2014} not verified since",
                format_age(age_secs)
            ),
            Self::ImportedHistoryOnly { records } => format!(
                "not yet verified \u{2014} all {records} stored shell record(s) came from an import, which is stored history, not capture"
            ),
            Self::ConfigurationPresent => {
                "configuration present, capture not yet verified".to_string()
            }
            Self::NotVerified => "not yet verified \u{2014} no captured command found".to_string(),
        }
    }

    /// `true` only when a live-recorded row proves capture is working now.
    pub const fn is_proven(self) -> bool {
        matches!(self, Self::RecentRecord { .. })
    }
}

/// Decide what the stored evidence supports.
///
/// Configuration alone, the session environment variable alone, and stored
/// history alone never reach `RecentRecord`. Only a row that no importer
/// could have written, recent enough to be about *now*, counts — and when
/// it came from a different shell session, the shell being diagnosed must
/// at least have the hook installed before its own capture is claimed.
pub const fn capture_evidence(facts: &CaptureFacts) -> CaptureEvidence {
    if let Some(age) = facts.newest_session_record_age_secs {
        if facts.session_records > 0 && age <= RECENT_RECORD_WINDOW_SECS {
            return CaptureEvidence::RecentRecord {
                age_secs: age,
                same_session: true,
            };
        }
    }
    if let Some(age) = facts.newest_live_record_age_secs {
        if facts.live_records > 0 {
            if age > RECENT_RECORD_WINDOW_SECS {
                return CaptureEvidence::EarlierRecord { age_secs: age };
            }
            return if facts.hook_configured {
                CaptureEvidence::RecentRecord {
                    age_secs: age,
                    same_session: false,
                }
            } else {
                CaptureEvidence::RecordFromAnotherShell { age_secs: age }
            };
        }
    }
    if facts.hook_configured {
        CaptureEvidence::ConfigurationPresent
    } else if facts.imported_records > 0 {
        CaptureEvidence::ImportedHistoryOnly {
            records: facts.imported_records,
        }
    } else {
        CaptureEvidence::NotVerified
    }
}

/// Caveats to print alongside the verdict.
pub fn evidence_notes(facts: &CaptureFacts) -> Vec<String> {
    let mut notes = Vec::new();
    let evidence = capture_evidence(facts);
    if facts.session_env_present && !evidence.is_proven() {
        notes.push(
            "SUVADU_SESSION_ID is set in this shell, but a session variable is not proof that any command was stored."
                .to_string(),
        );
    }
    // Said whenever imported rows exist and capture is unproven, including
    // the case where the hook *is* configured: the reason the diagnostics
    // are not green is precisely that the only stored rows were imported.
    if facts.imported_records > 0 && !evidence.is_proven() {
        notes.push(format!(
            "{} stored shell record(s) came from an import (suv import). They are searchable history, not evidence that a hook captured anything \u{2014} an imported row can carry any timestamp, including now.",
            facts.imported_records
        ));
    }
    notes
}

/// What to do next when capture is not verified, in the order to do it.
///
/// An import-only user is the case this exists for: refusing to go green
/// without saying why, or what would make it green, is not an improvement
/// on claiming capture that never happened.
pub fn capture_fixes(facts: &CaptureFacts, shell_name: &str) -> Vec<String> {
    if capture_evidence(facts).is_proven() {
        return Vec::new();
    }
    let mut fixes = Vec::new();
    if !facts.hook_configured {
        if let Some(rc) = rc_file_for_shell(shell_name) {
            fixes.push(format!(
                "suv init {shell_name} >> ~/{rc}   \u{2014} install the capture hook, then start a new shell"
            ));
        } else {
            fixes.push(format!(
                "suvadu has no shell hook for {shell_name}; shell capture needs bash or zsh"
            ));
        }
    }
    if facts.imported_records > 0 && facts.live_records == 0 {
        fixes.push(
            "imported history stays searchable either way \u{2014} only a command run through the hook can verify capture"
                .to_string(),
        );
    }
    fixes
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

/// The shell session id the current shell exported, if any.
pub fn current_session_id() -> Option<String> {
    std::env::var("SUVADU_SESSION_ID")
        .ok()
        .filter(|id| !id.is_empty())
}

/// Age in whole seconds of a millisecond timestamp, never negative.
pub const fn age_secs(now_ms: i64, at_ms: i64) -> i64 {
    let delta = now_ms - at_ms;
    if delta < 0 {
        0
    } else {
        delta / 1000
    }
}

/// Turn the database's provenance-split counts into the facts the verdict
/// is made from. Shared by `suv status` and `suv doctor` so the two can
/// never disagree about what the same database supports.
pub fn facts_from_records(
    hook_configured: bool,
    session_env_present: bool,
    stats: &crate::repository::CaptureRecordStats,
    now_ms: i64,
) -> CaptureFacts {
    CaptureFacts {
        hook_configured,
        session_env_present,
        live_records: stats.live_records,
        newest_live_record_age_secs: stats.newest_live_started_at.map(|at| age_secs(now_ms, at)),
        session_records: stats.session_records,
        newest_session_record_age_secs: stats
            .newest_session_started_at
            .map(|at| age_secs(now_ms, at)),
        imported_records: stats.imported_records,
    }
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
        CaptureFacts::default()
    }

    /// A live record of the given age, in the shell session being diagnosed.
    fn captured_here(age_secs: i64) -> CaptureFacts {
        CaptureFacts {
            live_records: 1,
            newest_live_record_age_secs: Some(age_secs),
            session_records: 1,
            newest_session_record_age_secs: Some(age_secs),
            ..clean()
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
            live_records: 12,
            newest_live_record_age_secs: Some(90),
            session_records: 12,
            newest_session_record_age_secs: Some(90),
            ..clean()
        };
        assert_eq!(
            capture_evidence(&facts),
            CaptureEvidence::RecentRecord {
                age_secs: 90,
                same_session: true
            }
        );
        assert!(capture_evidence(&facts).is_proven());
    }

    #[test]
    fn an_old_record_is_reported_as_observed_earlier_not_as_live_capture() {
        let age = RECENT_RECORD_WINDOW_SECS + 60;
        let facts = CaptureFacts {
            hook_configured: false,
            live_records: 3,
            newest_live_record_age_secs: Some(age),
            ..clean()
        };
        assert_eq!(
            capture_evidence(&facts),
            CaptureEvidence::EarlierRecord { age_secs: age }
        );
        assert!(!capture_evidence(&facts).is_proven());
    }

    /// R09: one imported row dated *now* used to read as a green capture
    /// check. Stored history is not evidence of capture, whatever its
    /// timestamp says.
    #[test]
    fn imported_history_alone_is_never_capture_evidence() {
        let facts = CaptureFacts {
            imported_records: 1,
            ..clean()
        };
        assert_eq!(
            capture_evidence(&facts),
            CaptureEvidence::ImportedHistoryOnly { records: 1 }
        );
        assert!(!capture_evidence(&facts).is_proven());
        assert!(
            capture_evidence(&facts).headline().contains("import"),
            "the verdict must name the reason: {}",
            capture_evidence(&facts).headline()
        );
    }

    /// Having installed the hook does not turn imported rows into proof
    /// either — but it does change what the user is told to do next.
    #[test]
    fn imported_history_with_the_hook_installed_is_still_only_configuration() {
        let facts = CaptureFacts {
            hook_configured: true,
            imported_records: 400,
            ..clean()
        };
        assert_eq!(
            capture_evidence(&facts),
            CaptureEvidence::ConfigurationPresent
        );
        assert!(!capture_evidence(&facts).is_proven());
        assert!(
            evidence_notes(&facts)
                .iter()
                .any(|note| note.contains("400 stored shell record(s) came from an import")),
            "the imported rows must be explained: {:?}",
            evidence_notes(&facts)
        );
        assert!(
            capture_fixes(&facts, "zsh")
                .iter()
                .all(|fix| !fix.contains("suv init")),
            "the hook is already installed; do not tell the user to install it again"
        );
    }

    /// An import-only user must be told the two things that get them to
    /// green, not merely refused.
    #[test]
    fn an_import_only_setup_is_told_how_to_become_verified() {
        let facts = CaptureFacts {
            imported_records: 12,
            ..clean()
        };
        let fixes = capture_fixes(&facts, "zsh").join("\n");
        assert!(
            fixes.contains("suv init zsh >> ~/.zshrc"),
            "the hook install must be spelled out: {fixes}"
        );
        assert!(
            fixes.contains("imported history stays searchable"),
            "the import must not be made to look like a mistake: {fixes}"
        );
        // And the existing end-to-end sequence stays the way it is resolved.
        assert!(verification_steps()[0].contains(VERIFY_MARKER));
    }

    #[test]
    fn a_shell_with_no_hook_of_its_own_has_no_install_advice_it_can_follow() {
        let facts = CaptureFacts {
            imported_records: 1,
            ..clean()
        };
        let fixes = capture_fixes(&facts, "fish").join("\n");
        assert!(
            fixes.contains("no shell hook for fish"),
            "an unsupported shell must be told so rather than given a broken command: {fixes}"
        );
    }

    /// A live record from another shell session, while this shell has no
    /// hook at all, is what `suv doctor` used to pass as capture evidence
    /// on the same screen as "~/.zshrc not found".
    #[test]
    fn a_record_from_another_shell_does_not_verify_an_unhooked_shell() {
        let facts = CaptureFacts {
            hook_configured: false,
            live_records: 5,
            newest_live_record_age_secs: Some(30),
            session_records: 0,
            newest_session_record_age_secs: None,
            ..clean()
        };
        assert_eq!(
            capture_evidence(&facts),
            CaptureEvidence::RecordFromAnotherShell { age_secs: 30 }
        );
        assert!(!capture_evidence(&facts).is_proven());
        assert!(
            capture_fixes(&facts, "zsh")
                .iter()
                .any(|fix| fix.contains("suv init zsh")),
            "this shell must be told to install its own hook"
        );
    }

    /// The same record in a shell that *is* hooked is fine: a terminal that
    /// has not typed anything yet is not a broken setup.
    #[test]
    fn a_record_from_another_session_counts_when_this_shell_is_hooked() {
        let facts = CaptureFacts {
            hook_configured: true,
            live_records: 5,
            newest_live_record_age_secs: Some(30),
            ..clean()
        };
        assert_eq!(
            capture_evidence(&facts),
            CaptureEvidence::RecentRecord {
                age_secs: 30,
                same_session: false
            }
        );
        assert!(capture_evidence(&facts).is_proven());
        assert!(capture_fixes(&facts, "zsh").is_empty());
    }

    /// The transition R09 asks for: unverified with imported rows, then a
    /// genuine capture arrives in this session and the verdict flips.
    #[test]
    fn a_genuine_capture_turns_an_import_only_setup_verified() {
        let before = CaptureFacts {
            imported_records: 3,
            ..clean()
        };
        assert!(!capture_evidence(&before).is_proven());

        let after = CaptureFacts {
            hook_configured: true,
            session_env_present: true,
            live_records: 1,
            newest_live_record_age_secs: Some(2),
            session_records: 1,
            newest_session_record_age_secs: Some(2),
            ..before
        };
        assert_eq!(
            capture_evidence(&after),
            CaptureEvidence::RecentRecord {
                age_secs: 2,
                same_session: true
            }
        );
        assert!(capture_evidence(&after).is_proven());
        assert!(
            evidence_notes(&after).is_empty(),
            "a proven setup needs no caveats: {:?}",
            evidence_notes(&after)
        );
        assert!(capture_fixes(&after, "zsh").is_empty());
    }

    /// Imported rows must not drag a proven verdict backwards either: a
    /// user who imports their old history and then runs a command is
    /// captured, and the age reported is the live row's, not the import's.
    #[test]
    fn imported_rows_never_supply_the_age_of_the_proof() {
        let facts = CaptureFacts {
            hook_configured: true,
            imported_records: 10_000,
            ..captured_here(45)
        };
        assert_eq!(
            capture_evidence(&facts),
            CaptureEvidence::RecentRecord {
                age_secs: 45,
                same_session: true
            }
        );
    }

    #[test]
    fn record_ages_are_measured_from_the_stored_timestamp() {
        let stats = crate::repository::CaptureRecordStats {
            live_records: 2,
            newest_live_started_at: Some(9_000),
            session_records: 1,
            newest_session_started_at: Some(4_000),
            imported_records: 7,
            newest_record: None,
        };
        let facts = facts_from_records(true, true, &stats, 10_000);
        assert_eq!(facts.newest_live_record_age_secs, Some(1));
        assert_eq!(facts.newest_session_record_age_secs, Some(6));
        assert_eq!(facts.imported_records, 7);
        // A clock that jumped backwards must not produce a negative age.
        assert_eq!(age_secs(0, 5_000), 0);
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
        assert!(CaptureEvidence::RecentRecord {
            age_secs: 30,
            same_session: true
        }
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
