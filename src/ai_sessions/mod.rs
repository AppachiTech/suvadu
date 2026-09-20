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

/// Privacy policy for summaries written *about* captured sessions.
///
/// Transcript ingestion receives a policy per working directory from its
/// caller, but a summary arrives over MCP from an agent, with no directory
/// to resolve and no chance for the tool handler to look one up. The
/// process installs the user's policy once at startup (as it already does
/// for risk patterns) and [`Repository::save_ai_summary`] applies it.
///
/// Unset means "no policy installed" — the summary is stored as written,
/// which is what the library's own tests and any embedding program get
/// unless they opt in.
static SUMMARY_POLICY: std::sync::OnceLock<CapturePolicy> = std::sync::OnceLock::new();

/// Install the summary policy for this process. A second call is a no-op.
pub fn set_summary_policy(policy: CapturePolicy) {
    let _ = SUMMARY_POLICY.set(policy);
}

/// The installed summary policy, if any.
pub fn summary_policy() -> Option<&'static CapturePolicy> {
    SUMMARY_POLICY.get()
}
