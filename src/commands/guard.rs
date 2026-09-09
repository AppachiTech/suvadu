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

/// Assess `command`'s risk. Returns `true` if it should be blocked (caller
/// is expected to `std::process::exit(2)` in that case — kept as a return
/// value rather than exiting here so this stays testable).
pub fn handle_guard(command: &str, block_at: Option<FailLevel>, verbose: bool) -> bool {
    let threshold = fail_level_to_risk_level(block_at.unwrap_or(DEFAULT_BLOCK_AT));

    let Some(assessment) = risk::assess_risk(command) else {
        if verbose {
            println!("suvadu: no risk detected");
        }
        return false;
    };

    let blocked = assessment.level >= threshold;

    if blocked {
        eprintln!(
            "suvadu: blocked — {} risk ({}): {}",
            assessment.level.label(),
            assessment.category,
            assessment.description
        );
        eprintln!("suvadu: run the command directly to bypass this check.");
    } else if verbose {
        println!(
            "suvadu: {} risk ({}): {}",
            assessment.level.label(),
            assessment.category,
            assessment.description
        );
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
}
