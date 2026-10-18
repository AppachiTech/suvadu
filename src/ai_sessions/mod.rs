//! Agent-neutral session records and versioned native transcript adapters.
pub mod claude;
pub mod codex;
pub mod handoff;
pub mod opencode;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What a native transcript adapter took from one chunk: the events, the
/// bytes it consumed, and how many malformed lines it skipped on the way.
pub type ParsedChunk = (Vec<AiEvent>, usize, usize);

/// Walk the complete JSONL records of `bytes`, handing each object record
/// and its absolute source offset to `handle`.
///
/// A final line without its newline may still be being written, so it is
/// left for the next import. A complete line that is not a JSON object
/// with a string `type` -- what a torn write leaves once the next record
/// is appended after it -- is skipped and counted, but only once a later
/// complete line shows the writer has moved past it; until then it is
/// left unconsumed like an unfinished line. Returns the bytes consumed
/// and the number of malformed lines skipped.
fn for_each_jsonl_record(
    bytes: &[u8],
    offset: u64,
    agent: &str,
    mut handle: impl FnMut(&Value, u64) -> Result<(), String>,
) -> Result<(usize, usize), String> {
    // Codex can embed image inputs and compaction snapshots in a single JSONL
    // record. Keep this aligned with the repository's bounded import chunk so
    // those records can be consumed without allowing unbounded allocation.
    const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
    let mut consumed = 0;
    let mut malformed = 0;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        let complete = line.last() == Some(&b'\n');
        let content = if complete {
            &line[..line.len() - 1]
        } else {
            line
        };
        if content.len() > MAX_LINE_BYTES {
            return Err(format!("{agent} JSONL record exceeds 16 MiB"));
        }
        if !complete {
            break;
        }
        let absolute = offset
            .checked_add(consumed as u64)
            .ok_or_else(|| format!("{agent} source offset overflow"))?;
        let record = serde_json::from_slice::<Value>(content)
            .ok()
            .filter(|record| record.is_object() && record["type"].is_string());
        let Some(record) = record else {
            let later_record = bytes[consumed + line.len()..].contains(&b'\n');
            if !later_record {
                break;
            }
            malformed += 1;
            consumed += line.len();
            continue;
        };
        handle(&record, absolute)
            .map_err(|error| format!("{agent} record at byte {absolute}: {error}"))?;
        consumed += line.len();
    }
    Ok((consumed, malformed))
}

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
