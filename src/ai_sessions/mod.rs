//! Agent-neutral session records and versioned native transcript adapters.
pub mod claude;
pub mod codex;
pub mod handoff;
pub mod opencode;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiEvent {
    pub id: String,
    pub turn_id: Option<String>,
    pub kind: String,
    pub at: i64,
    pub model: Option<String>,
    pub cwd: String,
    pub data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryInput {
    pub session_id: String,
    pub source_revision: String,
    pub text: String,
    /// Caller-declared authorship, not an authenticated identity.
    pub agent: String,
    pub model: String,
    pub source_ids: Vec<String>,
    /// Optional previous checkpoint being extended by this summary.
    #[serde(default)]
    pub base_summary_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CapturePolicy {
    pub enabled: bool,
    /// Session-wide environment pause, independent of project recording rules.
    pub paused: bool,
    pub redact: bool,
    pub extra_patterns: Vec<String>,
    pub exclusions: Vec<String>,
    pub max_chars: usize,
}

impl Default for CapturePolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            paused: false,
            redact: true,
            extra_patterns: Vec::new(),
            exclusions: Vec::new(),
            max_chars: 4000,
        }
    }
}
