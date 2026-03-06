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
        .get_replay_entries(None, after_ms, None, None, None, executor, cwd)
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
    let ms_val = if ms > 1_000_000_000_000_000 {
        ms / 1000
    } else {
        ms
    };
    Local.timestamp_millis_opt(ms_val).single().map_or_else(
        || "??-?? ??:??".into(),
        |dt| dt.format("%m-%d %H:%M").to_string(),
    )
}

/// Full datetime for detail pane: "YYYY-MM-DD HH:MM:SS"
pub fn format_full_datetime(ms: i64) -> String {
    let ms_val = if ms > 1_000_000_000_000_000 {
        ms / 1000
    } else {
        ms
    };
    Local.timestamp_millis_opt(ms_val).single().map_or_else(
        || "????-??-?? ??:??:??".into(),
        |dt| dt.format("%Y-%m-%d %H:%M:%S").to_string(),
    )
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else if max > 3 {
        format!("{}...", &s[..max - 3])
    } else {
        s[..max].to_string()
    }
}
