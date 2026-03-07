mod dashboard;
mod stats;

pub use dashboard::run_agent_ui;
pub use stats::run_agent_stats_ui;

use std::collections::HashMap;

use chrono::{Local, TimeZone};

use crate::models::Entry;
use crate::repository::Repository;
use crate::risk;
use crate::risk::RiskLevel;

// ── Period selector ──────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Period {
    Today,
    Days7,
    Days30,
    AllTime,
}

impl Period {
    pub(super) fn after_ms(self) -> Option<i64> {
        let now = chrono::Utc::now().timestamp_millis();
        match self {
            Self::Today => {
                let start = Local::now()
                    .date_naive()
                    .and_hms_opt(0, 0, 0)
                    .and_then(|dt| {
                        Local
                            .from_local_datetime(&dt)
                            .single()
                            .map(|d| d.timestamp_millis())
                    });
                start.or(Some(now - 24 * 60 * 60 * 1000))
            }
            Self::Days7 => Some(now - 7 * 24 * 60 * 60 * 1000),
            Self::Days30 => Some(now - 30 * 24 * 60 * 60 * 1000),
            Self::AllTime => None,
        }
    }

    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::Days7 => "7d",
            Self::Days30 => "30d",
            Self::AllTime => "All",
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────

pub fn load_entries(
    repo: &Repository,
    after_ms: Option<i64>,
    executor: Option<&str>,
    cwd: Option<&str>,
) -> Vec<Entry> {
    let all = repo
        .get_replay_entries(
            None,
            &crate::repository::ReplayFilter {
                after: after_ms,
                executor,
                cwd,
                ..Default::default()
            },
        )
        .unwrap_or_default();

    if executor.is_some() {
        all
    } else {
        all.into_iter().filter(Entry::is_agent).collect()
    }
}

pub fn compute_risk_levels(entries: &[Entry]) -> Vec<RiskLevel> {
    entries
        .iter()
        .map(|e| risk::risk_level(&e.command))
        .collect()
}

pub fn compute_agent_counts(entries: &[Entry]) -> Vec<(String, usize)> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for e in entries {
        let name = e.executor.as_deref().unwrap_or("unknown");
        *counts.entry(name.to_string()).or_default() += 1;
    }
    let mut sorted: Vec<_> = counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    sorted
}

/// Short time for table columns: "MM-DD HH:MM"
pub fn format_datetime(ms: i64) -> String {
    let ms_val = crate::util::normalize_display_ms(ms);
    Local.timestamp_millis_opt(ms_val).single().map_or_else(
        || "??-?? ??:??".into(),
        |dt| dt.format("%m-%d %H:%M").to_string(),
    )
}

/// Full datetime for detail pane: "YYYY-MM-DD HH:MM:SS"
pub fn format_full_datetime(ms: i64) -> String {
    let ms_val = crate::util::normalize_display_ms(ms);
    Local.timestamp_millis_opt(ms_val).single().map_or_else(
        || "????-??-?? ??:??:??".into(),
        |dt| dt.format("%Y-%m-%d %H:%M:%S").to_string(),
    )
}

pub fn truncate(s: &str, max: usize) -> String {
    crate::util::truncate_str(s, max, "...")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Entry;

    fn make_entry(executor: Option<&str>, executor_type: Option<&str>) -> Entry {
        Entry {
            id: None,
            session_id: "s1".to_string(),
            command: "test".to_string(),
            cwd: "/tmp".to_string(),
            exit_code: Some(0),
            started_at: 1_700_000_000_000,
            ended_at: 1_700_000_001_000,
            duration_ms: 1000,
            context: None,
            tag_name: None,
            tag_id: None,
            executor_type: executor_type.map(String::from),
            executor: executor.map(String::from),
        }
    }

    #[test]
    fn compute_agent_counts_empty() {
        let result = compute_agent_counts(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn compute_agent_counts_single_agent() {
        let entries = vec![
            make_entry(Some("claude"), Some("ai")),
            make_entry(Some("claude"), Some("ai")),
        ];
        let counts = compute_agent_counts(&entries);
        assert_eq!(counts.len(), 1);
        assert_eq!(counts[0], ("claude".to_string(), 2));
    }

    #[test]
    fn compute_agent_counts_multiple_agents() {
        let entries = vec![
            make_entry(Some("claude"), Some("ai")),
            make_entry(Some("copilot"), Some("ai")),
            make_entry(Some("claude"), Some("ai")),
        ];
        let counts = compute_agent_counts(&entries);
        assert_eq!(counts[0], ("claude".to_string(), 2));
        assert_eq!(counts[1], ("copilot".to_string(), 1));
    }

    #[test]
    fn compute_agent_counts_unknown_executor() {
        let entries = vec![make_entry(None, Some("ai"))];
        let counts = compute_agent_counts(&entries);
        assert_eq!(counts[0].0, "unknown");
    }

    #[test]
    fn format_datetime_valid_ms() {
        let result = format_datetime(1_700_000_000_000);
        // Should produce a valid date string (not the fallback)
        assert!(!result.contains("??"));
        assert!(result.contains("-"));
        assert!(result.contains(":"));
    }

    #[test]
    fn format_datetime_microsecond_input() {
        // If ms > 1_000_000_000_000_000, it divides by 1000
        let result = format_datetime(1_700_000_000_000_000);
        assert!(!result.contains("??"));
    }

    #[test]
    fn format_full_datetime_valid_ms() {
        let result = format_full_datetime(1_700_000_000_000);
        assert!(!result.contains("??"));
        assert!(result.contains("-"));
        assert!(result.contains(":"));
        // Should be in YYYY-MM-DD HH:MM:SS format
        assert_eq!(result.len(), 19);
    }

    #[test]
    fn format_full_datetime_microsecond_input() {
        let result = format_full_datetime(1_700_000_000_000_000);
        assert!(!result.contains("??"));
        assert_eq!(result.len(), 19);
    }

    #[test]
    fn period_after_ms_all_time_is_none() {
        assert!(Period::AllTime.after_ms().is_none());
    }

    #[test]
    fn period_after_ms_days7_is_some() {
        let result = Period::Days7.after_ms();
        assert!(result.is_some());
        let now = chrono::Utc::now().timestamp_millis();
        let diff = now - result.unwrap();
        // Should be approximately 7 days in ms (with some tolerance)
        let seven_days_ms = 7 * 24 * 60 * 60 * 1000;
        assert!((diff - seven_days_ms).abs() < 1000);
    }

    #[test]
    fn period_after_ms_days30_is_some() {
        let result = Period::Days30.after_ms();
        assert!(result.is_some());
    }

    #[test]
    fn period_after_ms_today_is_some() {
        let result = Period::Today.after_ms();
        assert!(result.is_some());
        // Should be within the last 24 hours
        let now = chrono::Utc::now().timestamp_millis();
        let diff = now - result.unwrap();
        assert!(diff <= 24 * 60 * 60 * 1000 + 1000);
    }

    #[test]
    fn period_labels() {
        assert_eq!(Period::Today.label(), "Today");
        assert_eq!(Period::Days7.label(), "7d");
        assert_eq!(Period::Days30.label(), "30d");
        assert_eq!(Period::AllTime.label(), "All");
    }
}
