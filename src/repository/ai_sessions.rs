//! Persistent, agent-neutral session evidence. Native logs are read incrementally.
use super::Repository;
use crate::ai_sessions::{claude, codex, opencode, AiEvent, CapturePolicy, SummaryInput};
use crate::db::{DbError, DbResult};
use crate::models::{AiSummaryRecord, SessionKind, SessionSummary};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

fn hash_field(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value);
}

fn invalid(message: impl Into<String>) -> DbError {
    DbError::Validation(message.into())
}
fn decode<T: serde::de::DeserializeOwned>(text: &str) -> DbResult<T> {
    serde_json::from_str(text).map_err(|_| invalid("Invalid stored AI session data"))
}
fn page(limit: usize, offset: usize) -> DbResult<(i64, i64)> {
    if !(1..=100).contains(&limit) {
        return Err(invalid("limit must be between 1 and 100"));
    }
    Ok((
        i64::try_from(limit).map_err(|_| invalid("Invalid limit"))?,
        i64::try_from(offset).map_err(|_| invalid("Invalid offset"))?,
    ))
}
fn excluded(cwd: &str, dirs: &[String]) -> bool {
    dirs.iter().any(|dir| {
        let expanded = super::expand_tilde(dir);
        let root = expanded.trim_end_matches('/');
        !expanded.is_empty()
            && (cwd == root
                || cwd
                    .strip_prefix(root)
                    .is_some_and(|suffix| suffix.starts_with('/')))
    })
}

fn merge_ai_header(sessions: &mut HashMap<String, SessionSummary>, id: String, header: &Value) {
    let last_activity_at = header["last_activity_at"].as_i64().unwrap_or_default();
    let created_at = header["created_at"].as_i64().unwrap_or(last_activity_at);
    let first_activity_at = header["first_activity_at"].as_i64().unwrap_or(created_at);
    let models = header["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let model = header["model"].as_str().map(str::to_owned);
    let total_tokens = header["usage"]["total_tokens"].as_u64();
    let command_count = header["command_count"].as_i64().unwrap_or_default();
    let event_count = header["event_count"].as_i64().unwrap_or_default();
    let cwd = header["cwd"].as_str().map(str::to_owned);
    let agent = header["agent"].as_str().map(str::to_owned);
    let usage_complete = header["usage_complete"].as_bool().unwrap_or(false);
    sessions
        .entry(id.clone())
        .and_modify(|summary| {
            summary.kind = SessionKind::Ai;
            summary.cwd.clone_from(&cwd);
            summary.agent.clone_from(&agent);
            summary.model.clone_from(&model);
            summary.models.clone_from(&models);
            summary.total_tokens = total_tokens;
            summary.usage_complete = usage_complete;
            summary.event_count = event_count;
            summary.created_at = summary.created_at.min(created_at);
            summary.first_activity_at = summary.first_activity_at.min(first_activity_at);
            summary.last_activity_at = summary.last_activity_at.max(last_activity_at);
        })
        .or_insert_with(|| SessionSummary {
            id,
            kind: SessionKind::Ai,
            hostname: String::new(),
            cwd,
            agent,
            model,
            models,
            total_tokens,
            usage_complete,
            event_count,
            created_at,
            tag_name: None,
            cmd_count: command_count,
            success_count: 0,
            first_activity_at,
            last_activity_at,
        });
}

/// Return false when the user's exclusion patterns suppress this evidence.
fn sanitize_event(
    event: &mut AiEvent,
    policy: &CapturePolicy,
    cache: &mut HashMap<Vec<String>, Vec<crate::util::CompiledExclusion>>,
) -> bool {
    let Some(text) = event.data["text"].as_str() else {
        return true;
    };
    let patterns = cache
        .entry(policy.exclusions.clone())
        .or_insert_with(|| crate::util::compile_exclusions(&policy.exclusions));
    if crate::util::is_excluded_compiled(text, patterns) {
        return false;
    }
    // Hashed from the text as captured, before any redaction/truncation, so
    // `reconcile_agent_command_turns` can match a command's cached prompt to
    // this event by content identity even when the two were sanitized under
    // different directories' policies (e.g. a stricter nested-project
    // `.suvadu.toml` for the command than the session-wide import used) and
    // so no longer read as equal text.
    let text_hash = format!("sha256:{:x}", Sha256::digest(text.as_bytes()));
    let safe = if policy.redact {
        crate::redact::redact_secrets_with_extra(text, &policy.extra_patterns)
    } else {
        text.to_owned()
    };
    let truncated = safe.chars().count() > policy.max_chars;
    event.data = json!({
        "text": crate::util::truncate_str(&safe, policy.max_chars, "..."),
        "truncated": truncated,
        "text_hash": text_hash
    });
    true
}

/// Remember omitted source ranges without retaining their text.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct CaptureGaps {
    all_before: u64,
    dirs: std::collections::HashMap<String, u64>,
}
impl CaptureGaps {
    fn observe(&mut self, cwd: &str, policy: &CapturePolicy, end: u64) {
        if policy.paused {
            self.all_before = self.all_before.max(end);
        } else if !policy.enabled {
            self.dirs.insert(cwd.to_owned(), end);
        }
    }
    fn omits(&self, event: &AiEvent) -> bool {
        let offset = event_source_offset(&event.id).unwrap_or(0);
        offset < self.all_before || self.dirs.get(&event.cwd).is_some_and(|end| offset < *end)
    }
}

fn event_source_offset(id: &str) -> Option<u64> {
    id.strip_prefix("codex-")
        .and_then(|value| value.parse().ok())
        .or_else(|| {
            id.strip_prefix("claude-")
                .and_then(|value| value.split('-').next())
                .and_then(|value| value.parse().ok())
        })
        .or_else(|| {
            id.strip_prefix("opencode-")
                .and_then(|value| value.split('-').next())
                .and_then(|value| value.parse().ok())
        })
}

fn reject_non_advancing_oversized_record(
    bytes: &[u8],
    consumed: usize,
    offset: u64,
    source_len: u64,
) -> DbResult<()> {
    const MAX_CHUNK: usize = 16 * 1024 * 1024;
    if consumed == 0
        && bytes.len() == MAX_CHUNK
        && source_len > offset.saturating_add(bytes.len() as u64)
    {
        return Err(invalid("Native transcript record exceeds 16 MiB"));
    }
    Ok(())
}

/// Find the prompt a command belongs to: by content hash if the command's
/// cached context carries one (works even when this command's own directory
/// re-redacted `agent_prompt` more strictly than the session-wide imported
/// prompt event), else by exact text for older cached prompts or other
/// agents that never carried a hash.
fn matching_turn_id<'a>(
    prompts: &'a [(i64, String, String, Option<String>)],
    context: &HashMap<String, String>,
    started_at: i64,
) -> Option<&'a String> {
    if let Some(hash) = context.get("agent_prompt_hash") {
        return prompts
            .iter()
            .filter(|(at, _, _, text_hash)| {
                *at <= started_at && text_hash.as_deref() == Some(hash.as_str())
            })
            .max_by_key(|(at, _, _, _)| *at)
            .map(|(_, turn_id, _, _)| turn_id);
    }
    let prompt = context.get("agent_prompt")?;
    prompts
        .iter()
        .filter(|(at, _, text, _)| *at <= started_at && text == prompt)
        .max_by_key(|(at, _, _, _)| *at)
        .map(|(_, turn_id, _, _)| turn_id)
}

fn reconcile_agent_command_turns(tx: &rusqlite::Transaction<'_>, session_id: &str) -> DbResult<()> {
    let prompts = {
        let mut statement = tx.prepare(
            "SELECT data FROM ai_events WHERE session_id=?1 AND kind='prompt' ORDER BY rowid",
        )?;
        let rows = statement
            .query_map([session_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut prompts = Vec::new();
        for row in rows {
            let event = decode::<AiEvent>(&row)?;
            if let (Some(turn_id), Some(text)) = (event.turn_id, event.data["text"].as_str()) {
                let text_hash = event.data["text_hash"].as_str().map(str::to_owned);
                prompts.push((event.at, turn_id, text.to_owned(), text_hash));
            }
        }
        prompts
    };
    if prompts.is_empty() {
        return Ok(());
    }
    let commands = {
        let mut statement = tx.prepare(
            "SELECT id,started_at,context FROM entries WHERE session_id=?1 ORDER BY started_at,id",
        )?;
        let rows = statement
            .query_map([session_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    for (id, started_at, context) in commands {
        let Some(context) = context else { continue };
        let Ok(mut context) = serde_json::from_str::<HashMap<String, String>>(&context) else {
            continue;
        };
        if context.contains_key("agent_turn_id") || context.contains_key("codex_turn_id") {
            continue;
        }
        let Some(turn_id) = matching_turn_id(&prompts, &context, started_at) else {
            continue;
        };
        context.insert("agent_turn_id".into(), turn_id.clone());
        tx.execute(
            "UPDATE entries SET context=?2 WHERE id=?1",
            params![
                id,
                serde_json::to_string(&context)
                    .map_err(|_| invalid("Cannot encode command context"))?
            ],
        )?;
    }
    Ok(())
}

impl Repository {
    pub fn has_native_ai_session(&self, id: &str) -> DbResult<bool> {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM ai_sessions WHERE id=?1)",
                [id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Consume at most 16 MiB, atomically advancing only complete JSONL records.
    /// The policy is resolved for each record's working directory, including while paused.
    pub fn import_codex_session(
        &self,
        path: &Path,
        expected_native_id: Option<&str>,
        policy_for: impl Fn(&str) -> DbResult<CapturePolicy>,
    ) -> DbResult<Value> {
        const MAX_CHUNK: u64 = 16 * 1024 * 1024;
        let path = path.canonicalize()?;
        let mut file = std::fs::File::open(&path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(invalid("Transcript must be a regular file"));
        }
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            format!("{}:{}", metadata.dev(), metadata.ino())
        };
        #[cfg(not(unix))]
        let identity = format!("{:?}", metadata.created().ok());
        let path_text = path.to_string_lossy();
        let tx = self.conn.unchecked_transaction()?;
        let previous = tx.query_row(
            "SELECT byte_offset,state,identity,adapter_version,skipped,gaps FROM ai_sources WHERE path=?1",
            [path_text.as_ref()], |row| Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,u32>(3)?,row.get::<_,bool>(4)?,row.get::<_,String>(5)?))
        ).optional()?;
        let (offset, mut state, mut skipped, mut gaps) = if let Some((
            offset,
            state,
            old_identity,
            version,
            skipped,
            gaps,
        )) = previous
        {
            if identity != old_identity
                || i64::try_from(metadata.len()).map_err(|_| invalid("Transcript too large"))?
                    < offset
                || version != codex::ADAPTER_VERSION
            {
                return Err(invalid("Transcript was replaced, truncated, or uses a different adapter version; import was not advanced"));
            }
            (
                u64::try_from(offset).map_err(|_| invalid("Invalid checkpoint offset"))?,
                decode::<codex::CodexState>(&state)?,
                skipped,
                decode::<CaptureGaps>(&gaps)?,
            )
        } else {
            (
                0,
                codex::CodexState::default(),
                false,
                CaptureGaps::default(),
            )
        };
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        file.take(MAX_CHUNK).read_to_end(&mut bytes)?;
        let (events, consumed) = codex::parse_chunk(&bytes, offset, &mut state).map_err(invalid)?;
        reject_non_advancing_oversized_record(&bytes, consumed, offset, metadata.len())?;
        let native = state
            .native_id
            .as_deref()
            .ok_or_else(|| invalid("No complete Codex session metadata found"))?;
        if expected_native_id.is_some_and(|expected| expected != native) {
            return Err(invalid("Hook session ID does not match transcript"));
        }
        let id = format!("codex-{native}");
        let mut inserted = 0;
        let mut exclusion_cache = HashMap::new();
        let tail_policy = policy_for(&state.cwd)?;
        gaps.observe(&state.cwd, &tail_policy, metadata.len());
        skipped |= !tail_policy.enabled;
        for mut event in events {
            let policy = policy_for(&event.cwd)?;
            gaps.observe(&event.cwd, &policy, metadata.len());
            if !policy.enabled || gaps.omits(&event) {
                skipped = true;
                continue;
            }
            if !sanitize_event(&mut event, &policy, &mut exclusion_cache) {
                skipped = true;
                continue;
            }
            // Cumulative counters after a disabled interval include hidden activity.
            if skipped && event.kind == "usage" {
                continue;
            }
            tx.execute("INSERT OR IGNORE INTO ai_sessions(id,native_id,agent,cwd,parent_id,created_at,updated_at) VALUES (?1,?2,'openai-codex',?3,?4,?5,?5)", params![id,native,event.cwd,state.parent_id,event.at])?;
            inserted += tx.execute("INSERT OR IGNORE INTO ai_events(session_id,event_id,kind,cwd,data) VALUES (?1,?2,?3,?4,?5)", params![id,event.id,event.kind,event.cwd,serde_json::to_string(&event).map_err(|_| invalid("Cannot encode event"))?])?;
        }
        let advanced = consumed > 0;
        if advanced {
            tx.execute("UPDATE ai_sessions SET revision=revision+1,updated_at=?2,usage_complete=usage_complete AND ?3 WHERE id=?1", params![id,chrono::Utc::now().timestamp_millis(),!skipped])?;
        }
        tx.execute("INSERT INTO ai_sources(path,session_id,identity,byte_offset,state,adapter_version,skipped,gaps) VALUES (?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(path) DO UPDATE SET byte_offset=excluded.byte_offset,state=excluded.state,skipped=excluded.skipped,gaps=excluded.gaps", params![path_text.as_ref(),id,identity,i64::try_from(offset+consumed as u64).map_err(|_| invalid("Transcript too large"))?,serde_json::to_string(&state).map_err(|_| invalid("Cannot encode checkpoint"))?,codex::ADAPTER_VERSION,skipped,serde_json::to_string(&gaps).map_err(|_| invalid("Cannot encode capture gaps"))?])?;
        tx.commit()?;
        Ok(
            json!({"session_id":id,"imported_events":inserted,"byte_offset":offset+consumed as u64,"has_more":metadata.len()>offset+bytes.len() as u64,"incomplete_tail":consumed<bytes.len(),"coverage":"partial","usage_complete":!skipped}),
        )
    }

    /// Consume at most 16 MiB from a Claude Code transcript, advancing only
    /// complete JSONL records and retaining no tool output or attachments.
    pub fn import_claude_session(
        &self,
        path: &Path,
        expected_native_id: Option<&str>,
        policy_for: impl Fn(&str) -> DbResult<CapturePolicy>,
    ) -> DbResult<Value> {
        const MAX_CHUNK: u64 = 16 * 1024 * 1024;
        let path = path.canonicalize()?;
        let mut file = std::fs::File::open(&path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(invalid("Transcript must be a regular file"));
        }
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            format!("{}:{}", metadata.dev(), metadata.ino())
        };
        #[cfg(not(unix))]
        let identity = format!("{:?}", metadata.created().ok());
        let path_text = path.to_string_lossy();
        let tx = self.conn.unchecked_transaction()?;
        let previous = tx.query_row(
            "SELECT byte_offset,state,identity,adapter_version,skipped,gaps FROM ai_sources WHERE path=?1",
            [path_text.as_ref()], |row| Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,u32>(3)?,row.get::<_,bool>(4)?,row.get::<_,String>(5)?))
        ).optional()?;
        let (offset, mut state, mut skipped, mut gaps) = if let Some((
            offset,
            state,
            old_identity,
            version,
            skipped,
            gaps,
        )) = previous
        {
            if identity != old_identity
                || i64::try_from(metadata.len()).map_err(|_| invalid("Transcript too large"))?
                    < offset
                || version != claude::ADAPTER_VERSION
            {
                return Err(invalid("Transcript was replaced, truncated, or uses a different adapter version; import was not advanced"));
            }
            (
                u64::try_from(offset).map_err(|_| invalid("Invalid checkpoint offset"))?,
                decode::<claude::ClaudeState>(&state)?,
                skipped,
                decode::<CaptureGaps>(&gaps)?,
            )
        } else {
            (
                0,
                claude::ClaudeState::default(),
                false,
                CaptureGaps::default(),
            )
        };
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        file.take(MAX_CHUNK).read_to_end(&mut bytes)?;
        let (events, consumed) =
            claude::parse_chunk(&bytes, offset, &mut state).map_err(invalid)?;
        reject_non_advancing_oversized_record(&bytes, consumed, offset, metadata.len())?;
        let native = state
            .native_id
            .as_deref()
            .ok_or_else(|| invalid("No complete Claude session messages found"))?;
        if expected_native_id.is_some_and(|expected| expected != native) {
            return Err(invalid("Hook session ID does not match transcript"));
        }
        let id = format!("claude-{native}");
        let mut inserted = 0;
        let mut exclusion_cache = HashMap::new();
        let tail_policy = policy_for(&state.cwd)?;
        gaps.observe(&state.cwd, &tail_policy, metadata.len());
        skipped |= !tail_policy.enabled;
        for mut event in events {
            let policy = policy_for(&event.cwd)?;
            gaps.observe(&event.cwd, &policy, metadata.len());
            if !policy.enabled || gaps.omits(&event) {
                skipped = true;
                continue;
            }
            if !sanitize_event(&mut event, &policy, &mut exclusion_cache) {
                skipped = true;
                continue;
            }
            if skipped && event.kind == "usage" {
                continue;
            }
            tx.execute("INSERT OR IGNORE INTO ai_sessions(id,native_id,agent,cwd,parent_id,created_at,updated_at) VALUES (?1,?2,'claude-code',?3,NULL,?4,?4)", params![id,native,event.cwd,event.at])?;
            inserted += tx.execute("INSERT OR IGNORE INTO ai_events(session_id,event_id,kind,cwd,data) VALUES (?1,?2,?3,?4,?5)", params![id,event.id,event.kind,event.cwd,serde_json::to_string(&event).map_err(|_| invalid("Cannot encode event"))?])?;
        }
        reconcile_agent_command_turns(&tx, &id)?;
        let advanced = consumed > 0;
        if advanced {
            tx.execute("UPDATE ai_sessions SET revision=revision+1,updated_at=?2,usage_complete=usage_complete AND ?3 WHERE id=?1", params![id,chrono::Utc::now().timestamp_millis(),!skipped])?;
        }
        tx.execute("INSERT INTO ai_sources(path,session_id,identity,byte_offset,state,adapter_version,skipped,gaps) VALUES (?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(path) DO UPDATE SET byte_offset=excluded.byte_offset,state=excluded.state,skipped=excluded.skipped,gaps=excluded.gaps", params![path_text.as_ref(),id,identity,i64::try_from(offset+consumed as u64).map_err(|_| invalid("Transcript too large"))?,serde_json::to_string(&state).map_err(|_| invalid("Cannot encode checkpoint"))?,claude::ADAPTER_VERSION,skipped,serde_json::to_string(&gaps).map_err(|_| invalid("Cannot encode capture gaps"))?])?;
        tx.commit()?;
        Ok(
            json!({"session_id":id,"imported_events":inserted,"byte_offset":offset+consumed as u64,"has_more":metadata.len()>offset+bytes.len() as u64,"incomplete_tail":consumed<bytes.len(),"coverage":"partial","usage_complete":!skipped}),
        )
    }

    /// Import a full `OpenCode` session-message array. Unlike the file-backed
    /// adapters, there is no byte offset to checkpoint: every call hands in
    /// the complete history, and incrementality is tracked by message ID
    /// inside `OpencodeState`.
    pub fn import_opencode_session(
        &self,
        session_id: &str,
        cwd: &str,
        messages_json: &str,
        policy_for: impl Fn(&str) -> DbResult<CapturePolicy>,
    ) -> DbResult<Value> {
        let messages: Vec<Value> = serde_json::from_str(messages_json)
            .map_err(|error| invalid(format!("Invalid OpenCode message JSON: {error}")))?;
        let path = format!("opencode-session:{session_id}");
        let id = format!("opencode-{session_id}");
        let tx = self.conn.unchecked_transaction()?;
        let previous = tx
            .query_row(
                "SELECT state,adapter_version,skipped,gaps FROM ai_sources WHERE path=?1",
                [&path],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, u32>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        let (mut state, mut skipped, mut gaps) =
            if let Some((state, version, skipped, gaps)) = previous {
                if version != opencode::ADAPTER_VERSION {
                    return Err(invalid(
                    "OpenCode session uses a different adapter version; import was not advanced",
                ));
                }
                (
                    decode::<opencode::OpencodeState>(&state)?,
                    skipped,
                    decode::<CaptureGaps>(&gaps)?,
                )
            } else {
                (
                    opencode::OpencodeState::default(),
                    false,
                    CaptureGaps::default(),
                )
            };

        let events = opencode::parse_messages(&messages, cwd, &mut state).map_err(invalid)?;
        let mut inserted = 0;
        let mut exclusion_cache = HashMap::new();
        let tail_policy = policy_for(cwd)?;
        let end = state.next_seq;
        gaps.observe(cwd, &tail_policy, end);
        skipped |= !tail_policy.enabled;
        for mut event in events {
            let policy = policy_for(&event.cwd)?;
            gaps.observe(&event.cwd, &policy, end);
            if !policy.enabled || gaps.omits(&event) {
                skipped = true;
                continue;
            }
            if !sanitize_event(&mut event, &policy, &mut exclusion_cache) {
                skipped = true;
                continue;
            }
            if skipped && event.kind == "usage" {
                continue;
            }
            tx.execute(
                "INSERT OR IGNORE INTO ai_sessions(id,native_id,agent,cwd,parent_id,created_at,updated_at) VALUES (?1,?2,'opencode',?3,NULL,?4,?4)",
                params![id, session_id, event.cwd, event.at],
            )?;
            inserted += tx.execute(
                "INSERT OR IGNORE INTO ai_events(session_id,event_id,kind,cwd,data) VALUES (?1,?2,?3,?4,?5)",
                params![
                    id,
                    event.id,
                    event.kind,
                    event.cwd,
                    serde_json::to_string(&event).map_err(|_| invalid("Cannot encode event"))?
                ],
            )?;
        }
        reconcile_agent_command_turns(&tx, &id)?;
        if inserted > 0 {
            tx.execute(
                "UPDATE ai_sessions SET revision=revision+1,updated_at=?2,usage_complete=usage_complete AND ?3 WHERE id=?1",
                params![id, chrono::Utc::now().timestamp_millis(), !skipped],
            )?;
        }
        tx.execute(
            "INSERT INTO ai_sources(path,session_id,identity,byte_offset,state,adapter_version,skipped,gaps) VALUES (?1,?2,'opencode-api',0,?3,?4,?5,?6) ON CONFLICT(path) DO UPDATE SET state=excluded.state,skipped=excluded.skipped,gaps=excluded.gaps",
            params![
                path,
                id,
                serde_json::to_string(&state).map_err(|_| invalid("Cannot encode checkpoint"))?,
                opencode::ADAPTER_VERSION,
                skipped,
                serde_json::to_string(&gaps).map_err(|_| invalid("Cannot encode capture gaps"))?
            ],
        )?;
        tx.commit()?;
        Ok(json!({
            "session_id": id,
            "imported_events": inserted,
            "has_more": false,
            "coverage": "partial",
            "usage_complete": !skipped
        }))
    }

    fn ai_session_header(&self, id: &str, excluded_dirs: &[String]) -> DbResult<Value> {
        let row = self.conn.query_row("SELECT native_id,agent,cwd,parent_id,created_at,updated_at,revision,usage_complete FROM ai_sessions WHERE id=?1", [id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,i64>(4)?,r.get::<_,i64>(5)?,r.get::<_,i64>(6)?,r.get::<_,bool>(7)?))).optional()?.ok_or_else(|| invalid("Session not found or excluded"))?;
        let mut paths = self.conn.prepare("SELECT cwd FROM ai_events WHERE session_id=?1 UNION SELECT cwd FROM entries WHERE session_id=?1")?;
        if excluded(&row.2, excluded_dirs)
            || paths
                .query_map([id], |r| r.get::<_, String>(0))?
                .any(|p| p.map_or(true, |p| excluded(&p, excluded_dirs)))
        {
            return Err(invalid("Session not found or excluded"));
        }
        let (count, max_id): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*),COALESCE(MAX(id),0) FROM entries WHERE session_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let usage: Option<String> = if row.7 {
            self.conn.query_row("SELECT data FROM ai_events WHERE session_id=?1 AND kind='usage' ORDER BY rowid DESC LIMIT 1", [id], |r| r.get(0)).optional()?
        } else {
            None
        };
        let usage = usage
            .map(|data| decode::<AiEvent>(&data).map(|event| event.data["total"].clone()))
            .transpose()?
            .unwrap_or(Value::Null);
        let mut event_statement = self
            .conn
            .prepare("SELECT data FROM ai_events WHERE session_id=?1 ORDER BY rowid")?;
        let mut models = Vec::new();
        let mut latest_model = None;
        let mut event_count = 0_i64;
        let mut first_activity_at = None;
        let mut last_activity_at = None;
        for data in event_statement.query_map([id], |r| r.get::<_, String>(0))? {
            let event = decode::<AiEvent>(&data?)?;
            event_count += 1;
            first_activity_at =
                Some(first_activity_at.map_or(event.at, |current: i64| current.min(event.at)));
            last_activity_at =
                Some(last_activity_at.map_or(event.at, |current: i64| current.max(event.at)));
            if let Some(model) = event.model {
                latest_model = Some(model.clone());
                if !models.contains(&model) {
                    models.push(model);
                }
            }
        }
        let model = latest_model;
        Ok(
            json!({"id":id,"native_id":row.0,"agent":row.1,"cwd":row.2,"parent_id":row.3,"created_at":row.4,"updated_at":row.5,"first_activity_at":first_activity_at.unwrap_or(row.4),"last_activity_at":last_activity_at.unwrap_or(row.5),"revision":format!("e{}-c{count}-{max_id}",row.6),"model":model,"models":models,"usage":usage,"coverage":"partial","usage_complete":row.7,"event_count":event_count,"command_count":count,"coverage_note":"Captured native transcript events and locally recorded shell commands only; child sessions, non-shell tools and unavailable records are not combined."}),
        )
    }

    /// Fingerprint the exact ordered event/command prefix covered by a summary.
    /// Returning `None` means the requested prefix is no longer present.
    fn ai_session_prefix_hash(
        &self,
        id: &str,
        event_count: i64,
        command_count: i64,
    ) -> DbResult<Option<String>> {
        if event_count < 0 || command_count < 0 {
            return Ok(None);
        }
        let mut hash = Sha256::new();
        hash_field(&mut hash, id.as_bytes());
        hash_field(&mut hash, b"events");
        let mut seen_events = 0_i64;
        let mut statement = self.conn.prepare(
            "SELECT event_id,data FROM ai_events WHERE session_id=?1 ORDER BY rowid LIMIT ?2",
        )?;
        for row in statement.query_map(params![id, event_count], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            let (event_id, data) = row?;
            hash_field(&mut hash, event_id.as_bytes());
            hash_field(&mut hash, data.as_bytes());
            seen_events += 1;
        }
        if seen_events != event_count {
            return Ok(None);
        }

        hash_field(&mut hash, b"commands");
        let mut seen_commands = 0_i64;
        let mut statement = self.conn.prepare(
            "SELECT id,command,cwd,exit_code,started_at,ended_at,duration_ms,context
             FROM entries WHERE session_id=?1 ORDER BY id LIMIT ?2",
        )?;
        for row in statement.query_map(params![id, command_count], |row| {
            Ok(json!([
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i32>>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, Option<String>>(7)?,
            ]))
        })? {
            let data = row?.to_string();
            hash_field(&mut hash, data.as_bytes());
            seen_commands += 1;
        }
        if seen_commands != command_count {
            return Ok(None);
        }
        Ok(Some(format!("sha256:{:x}", hash.finalize())))
    }

    fn shell_session_summaries(
        &self,
        after: Option<i64>,
        tag_id: Option<i64>,
    ) -> DbResult<HashMap<String, SessionSummary>> {
        let mut statement = self.conn.prepare(
            "SELECT s.id,s.hostname,s.created_at,COALESCE(t.name,''),COUNT(e.id),
                    SUM(CASE WHEN e.exit_code=0 THEN 1 ELSE 0 END),MIN(e.started_at),
                    MAX(e.ended_at),MIN(e.cwd),
                    MAX(CASE WHEN e.executor_type IS NOT NULL AND e.executor_type NOT IN ('human','unknown') THEN 1 ELSE 0 END),
                    MAX(CASE WHEN e.executor_type IS NOT NULL AND e.executor_type NOT IN ('human','unknown') THEN e.executor END)
             FROM sessions s
             JOIN entries e ON e.session_id=s.id
             LEFT JOIN tags t ON t.id=s.tag_id
             WHERE (?1 IS NULL OR s.tag_id=?1)
             GROUP BY s.id
             HAVING (?2 IS NULL OR MAX(e.ended_at)>=?2)",
        )?;
        let rows = statement.query_map(params![tag_id, after], |row| {
            let tag: String = row.get(3)?;
            let has_agent = row.get::<_, i64>(9)? > 0;
            Ok(SessionSummary {
                id: row.get(0)?,
                kind: if has_agent {
                    SessionKind::Ai
                } else {
                    SessionKind::Human
                },
                hostname: row.get(1)?,
                cwd: row.get(8)?,
                agent: row.get(10)?,
                model: None,
                models: Vec::new(),
                total_tokens: None,
                usage_complete: false,
                event_count: 0,
                created_at: row.get(2)?,
                tag_name: (!tag.is_empty()).then_some(tag),
                cmd_count: row.get(4)?,
                success_count: row.get(5)?,
                first_activity_at: row.get(6)?,
                last_activity_at: row.get(7)?,
            })
        })?;
        let mut sessions = HashMap::new();
        for summary in rows {
            let summary = summary?;
            sessions.insert(summary.id.clone(), summary);
        }
        Ok(sessions)
    }

    /// List shell and captured AI sessions as one deduplicated, activity-sorted view.
    pub fn list_unified_sessions(
        &self,
        after: Option<i64>,
        tag_id: Option<i64>,
        limit: usize,
    ) -> DbResult<Vec<SessionSummary>> {
        let mut sessions = self.shell_session_summaries(after, tag_id)?;

        let mut ai_statement = self
            .conn
            .prepare("SELECT id FROM ai_sessions ORDER BY updated_at DESC,id")?;
        for id in ai_statement.query_map([], |row| row.get::<_, String>(0))? {
            let id = id?;
            let header = self.ai_session_header(&id, &[])?;
            let last_activity_at = header["last_activity_at"].as_i64().unwrap_or_default();
            if after.is_some_and(|start| last_activity_at < start) {
                continue;
            }
            if let Some(required_tag) = tag_id {
                let matches_tag: bool = self.conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1 AND tag_id=?2)",
                    params![id, required_tag],
                    |row| row.get(0),
                )?;
                if !matches_tag {
                    continue;
                }
            }
            merge_ai_header(&mut sessions, id, &header);
        }
        let mut sessions = sessions.into_values().collect::<Vec<_>>();
        sessions.sort_by(|left, right| {
            right
                .last_activity_at
                .cmp(&left.last_activity_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        sessions.truncate(limit);
        Ok(sessions)
    }

    /// Find a session prefix across both the shell and native AI stores.
    pub fn find_unified_sessions_by_prefix(&self, prefix: &str) -> DbResult<Vec<String>> {
        Ok(self
            .list_unified_sessions(None, None, usize::MAX)?
            .into_iter()
            .filter(|session| session.id.starts_with(prefix))
            .map(|session| session.id)
            .take(10)
            .collect())
    }

    pub fn list_ai_sessions(
        &self,
        limit: usize,
        offset: usize,
        excluded_dirs: &[String],
    ) -> DbResult<Value> {
        page(limit, offset)?;
        let snapshot = self.conn.unchecked_transaction()?;
        let mut statement = self
            .conn
            .prepare("SELECT id FROM ai_sessions ORDER BY updated_at DESC,id")?;
        let mut sessions = Vec::new();
        let mut visible = 0;
        for id in statement.query_map([], |r| r.get::<_, String>(0))? {
            let id = id?;
            let header = match self.ai_session_header(&id, excluded_dirs) {
                Ok(h) => h,
                Err(DbError::Validation(_)) => continue,
                Err(e) => return Err(e),
            };
            if visible >= offset {
                sessions.push(header);
            }
            visible += 1;
            if sessions.len() > limit {
                break;
            }
        }
        let more = sessions.len() > limit;
        sessions.truncate(limit);
        snapshot.commit()?;
        Ok(json!({"sessions":sessions,"next_offset":more.then_some(offset.saturating_add(limit))}))
    }

    pub fn get_ai_session(
        &self,
        id: &str,
        limit: usize,
        offset: usize,
        excluded_dirs: &[String],
    ) -> DbResult<Value> {
        self.get_ai_session_ranges(id, limit, offset, offset, excluded_dirs)
    }

    pub fn get_ai_session_ranges(
        &self,
        id: &str,
        limit: usize,
        event_offset: usize,
        command_offset: usize,
        excluded_dirs: &[String],
    ) -> DbResult<Value> {
        let (sql_limit, sql_event_offset) = page(limit, event_offset)?;
        let (_, sql_command_offset) = page(limit, command_offset)?;
        let snapshot = self.conn.unchecked_transaction()?;
        let session = self.ai_session_header(id, excluded_dirs)?;
        let mut statement = self.conn.prepare(
            "SELECT data FROM ai_events WHERE session_id=?1 ORDER BY rowid LIMIT ?2 OFFSET ?3",
        )?;
        let mut events = statement
            .query_map(params![id, sql_limit + 1, sql_event_offset], |r| {
                r.get::<_, String>(0)
            })?
            .map(|row| decode::<AiEvent>(&row?))
            .collect::<DbResult<Vec<_>>>()?;
        let mut statement = self.conn.prepare("SELECT id,command,cwd,exit_code,started_at,duration_ms,context FROM entries WHERE session_id=?1 ORDER BY id LIMIT ?2 OFFSET ?3")?;
        let mut commands = statement.query_map(params![id,sql_limit+1,sql_command_offset], |r| {
            let context: Option<String> = r.get(6)?;
            let context: Value = context.as_deref().and_then(|s| serde_json::from_str(s).ok()).unwrap_or(Value::Null);
            let turn_id = context
                .get("agent_turn_id")
                .or_else(|| context.get("codex_turn_id"))
                .cloned()
                .unwrap_or(Value::Null);
            Ok(json!({"id":format!("command-{}",r.get::<_,i64>(0)?),"command":r.get::<_,String>(1)?,"cwd":r.get::<_,String>(2)?,"exit_code":r.get::<_,Option<i32>>(3)?,"started_at":r.get::<_,i64>(4)?,"duration_ms":r.get::<_,i64>(5)?,"turn_id":turn_id}))
        })?.collect::<Result<Vec<_>,_>>()?;
        let more_events = events.len() > limit;
        let more_commands = commands.len() > limit;
        events.truncate(limit);
        commands.truncate(limit);
        let mut statement = self.conn.prepare("SELECT id,source_revision,text,agent,model,source_ids,source_event_count,source_command_count,source_prefix_hash,base_summary_id,created_at FROM ai_summaries WHERE session_id=?1 ORDER BY created_at DESC,rowid DESC LIMIT 5")?;
        let rows = statement
            .query_map([id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, String>(8)?,
                    r.get::<_, Option<String>>(9)?,
                    r.get::<_, i64>(10)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let current_event_count = session["event_count"].as_i64().unwrap_or_default();
        let current_command_count = session["command_count"].as_i64().unwrap_or_default();
        let current_revision = session["revision"].as_str().unwrap_or_default();
        let mut summaries = Vec::with_capacity(rows.len());
        for (
            summary_id,
            revision,
            text,
            agent,
            model,
            sources,
            source_event_count,
            source_command_count,
            source_prefix_hash,
            base_summary_id,
            created_at,
        ) in rows
        {
            let current = current_revision == revision;
            let grew = current_event_count >= source_event_count
                && current_command_count >= source_command_count
                && (current_event_count > source_event_count
                    || current_command_count > source_command_count);
            let prefix_matches = !source_prefix_hash.is_empty()
                && self
                    .ai_session_prefix_hash(id, source_event_count, source_command_count)?
                    .is_some_and(|hash| hash == source_prefix_hash);
            let has_new_activity = !current && grew && prefix_matches;
            let incremental_safe = current || has_new_activity;
            summaries.push(json!({
                "id":summary_id,"source_revision":revision,"text":text,"agent":agent,
                "model":model,"source_ids":serde_json::from_str::<Value>(&sources).unwrap_or(Value::Null),
                "source_event_count":source_event_count,"source_command_count":source_command_count,
                "base_summary_id":base_summary_id,"created_at":created_at,"generated":true,
                "current":current,"has_new_activity":has_new_activity,
                "incremental_safe":incremental_safe,"stale":!incremental_safe,
                "resume_event_offset":incremental_safe.then_some(source_event_count),
                "resume_command_offset":incremental_safe.then_some(source_command_count)
            }));
        }
        snapshot.commit()?;
        let next_event_offset = more_events.then_some(event_offset.saturating_add(limit));
        let next_command_offset = more_commands.then_some(command_offset.saturating_add(limit));
        let next_offset = (event_offset == command_offset && (more_events || more_commands))
            .then_some(event_offset.saturating_add(limit));
        Ok(
            json!({"session":session,"events":events,"commands":commands,"summaries":summaries,"next_offset":next_offset,"next_event_offset":next_event_offset,"next_command_offset":next_command_offset}),
        )
    }

    pub fn save_ai_summary(
        &self,
        input: &SummaryInput,
        excluded_dirs: &[String],
    ) -> DbResult<Value> {
        if input.text.trim().is_empty()
            || input.text.chars().count() > 16_000
            || input.text.len() > 64_000
            || input.agent.trim().is_empty()
            || input.agent.len() > 128
            || input.model.trim().is_empty()
            || input.model.len() > 256
            || input.source_ids.is_empty()
            || input.source_ids.len() > 1000
        {
            return Err(invalid(
                "Summary requires bounded text, agent, model, and 1–1000 evidence source IDs",
            ));
        }
        let tx = self.conn.unchecked_transaction()?;
        let session = self.ai_session_header(&input.session_id, excluded_dirs)?;
        if session["revision"] != input.source_revision {
            return Err(invalid(
                "Session revision changed; read all pages again before saving a summary",
            ));
        }
        let source_event_count = session["event_count"].as_i64().unwrap_or_default();
        let source_command_count = session["command_count"].as_i64().unwrap_or_default();
        let source_prefix_hash = self
            .ai_session_prefix_hash(&input.session_id, source_event_count, source_command_count)?
            .ok_or_else(|| invalid("Cannot fingerprint summary evidence"))?;
        if let Some(base_summary_id) = input.base_summary_id.as_deref() {
            let base = tx.query_row(
                "SELECT session_id,source_event_count,source_command_count,source_prefix_hash,source_ids FROM ai_summaries WHERE id=?1",
                [base_summary_id],
                |row| Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?,row.get::<_,i64>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?)),
            ).optional()?.ok_or_else(|| invalid("Base summary not found"))?;
            if base.0 != input.session_id
                || base.3.is_empty()
                || self.ai_session_prefix_hash(&input.session_id, base.1, base.2)? != Some(base.3)
            {
                return Err(invalid(
                    "Base summary no longer matches this session; rebuild from all evidence",
                ));
            }
            let base_sources = serde_json::from_str::<Vec<String>>(&base.4)
                .map_err(|_| invalid("Invalid stored summary evidence"))?;
            if base_sources
                .iter()
                .any(|source| !input.source_ids.contains(source))
            {
                return Err(invalid(
                    "Updated summary must retain the base summary's evidence IDs",
                ));
            }
        }
        for source in &input.source_ids {
            let exists: bool = if let Some(command) = source
                .strip_prefix("command-")
                .and_then(|s| s.parse::<i64>().ok())
            {
                tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM entries WHERE session_id=?1 AND id=?2)",
                    params![input.session_id, command],
                    |r| r.get(0),
                )?
            } else {
                tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM ai_events WHERE session_id=?1 AND event_id=?2)",
                    params![input.session_id, source],
                    |r| r.get(0),
                )?
            };
            if !exists {
                return Err(invalid("Summary references evidence outside this session"));
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        tx.execute("INSERT INTO ai_summaries(id,session_id,source_revision,text,agent,model,source_ids,source_event_count,source_command_count,source_prefix_hash,base_summary_id,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",params![id,input.session_id,input.source_revision,input.text,input.agent,input.model,serde_json::to_string(&input.source_ids).map_err(|_| invalid("Cannot encode sources"))?,source_event_count,source_command_count,source_prefix_hash,input.base_summary_id,chrono::Utc::now().timestamp_millis()])?;
        tx.commit()?;
        Ok(
            json!({"id":id,"session_id":input.session_id,"generated":true,"current":true,"stale":false,"source_event_count":source_event_count,"source_command_count":source_command_count,"base_summary_id":input.base_summary_id}),
        )
    }

    /// Saved summaries for a session, newest first, for the TUI summary
    /// panel. Unlike [`Self::get_ai_session`]'s `summaries` field, this
    /// doesn't compute checkpoint-resume eligibility -- it's just what a
    /// human viewer needs: the text, who/what wrote it, when, and whether
    /// it's still current.
    pub fn ai_summaries_for_session(&self, session_id: &str) -> DbResult<Vec<AiSummaryRecord>> {
        let current_revision = self.ai_session_header(session_id, &[])?["revision"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let mut statement = self.conn.prepare(
            "SELECT id,text,agent,model,source_revision,created_at FROM ai_summaries \
             WHERE session_id=?1 ORDER BY created_at DESC,rowid DESC",
        )?;
        let records = statement
            .query_map([session_id], |r| {
                let source_revision: String = r.get(4)?;
                Ok(AiSummaryRecord {
                    id: r.get(0)?,
                    text: r.get(1)?,
                    agent: r.get(2)?,
                    model: r.get(3)?,
                    current: source_revision == current_revision,
                    created_at: r.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(records)
    }

    /// Explicit deletion includes shell evidence and checkpoints, preventing retained summaries.
    pub fn delete_ai_session(&self, id: &str) -> DbResult<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_sessions WHERE id=?1 UNION SELECT 1 FROM ai_sources WHERE session_id=?1)",[id],|r|r.get(0))?;
        if !exists {
            return Err(invalid("AI session not found"));
        }
        let commands = tx.execute("DELETE FROM entries WHERE session_id=?1", [id])?;
        tx.execute("DELETE FROM ai_sources WHERE session_id=?1", [id])?;
        let sessions = tx.execute("DELETE FROM ai_sessions WHERE id=?1", [id])?;
        tx.commit()?;
        Ok(commands + sessions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Entry, Session, SessionKind};
    use std::io::Write;

    #[allow(clippy::format_collect)] // Small readable JSONL test fixture.
    fn records() -> String {
        [
            json!({"type":"session_meta","payload":{"id":"fixture","cwd":"/work/project"}}),
            json!({"type":"turn_context","payload":{"turn_id":"turn-1","cwd":"/work/project","model":"test-model"}}),
            json!({"type":"event_msg","payload":{"type":"user_message","message":"Fix the synthetic test"}}),
            json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":20,"reasoning_output_tokens":10,"total_tokens":120}}}}),
            json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":150,"cached_input_tokens":50,"output_tokens":30,"total_tokens":180}}}}),
            json!({"type":"event_msg","payload":{"type":"agent_message","phase":"final_answer","message":"The synthetic check passed."}}),
        ].into_iter().map(|mut v| {v["timestamp"]=json!("2026-09-12T12:00:00Z"); format!("{v}\n")}).collect()
    }
    fn import(repo: &Repository, path: &Path) -> Value {
        repo.import_codex_session(path, None, |_| Ok(CapturePolicy::default()))
            .unwrap()
    }
    fn append(path: &Path, text: &str) {
        std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    }
    fn prompt(text: &str) -> String {
        format!(
            "{}\n",
            json!({"timestamp":"2026-09-12T12:01:00Z","type":"event_msg","payload":{"type":"user_message","message":text}})
        )
    }

    fn mutate_event(repo: &Repository, event_id: &str) {
        let original: String = repo
            .conn
            .query_row(
                "SELECT data FROM ai_events WHERE session_id='codex-fixture' AND event_id=?1",
                [event_id],
                |row| row.get(0),
            )
            .unwrap();
        repo.conn
            .execute(
                "UPDATE ai_events SET data=?1 WHERE session_id='codex-fixture' AND event_id=?2",
                rusqlite::params![original.replace("synthetic", "changed"), event_id],
            )
            .unwrap();
        repo.conn
            .execute(
                "UPDATE ai_sessions SET revision=revision+1 WHERE id='codex-fixture'",
                [],
            )
            .unwrap();
    }

    fn insert_ai_fixture(
        repo: &Repository,
        id: &str,
        created_at: i64,
        updated_at: i64,
        models: &[&str],
        total_tokens: u64,
    ) {
        repo.conn
            .execute(
                "INSERT INTO ai_sessions(id,native_id,agent,cwd,created_at,updated_at) VALUES (?1,?1,'openai-codex','/work',?2,?3)",
                params![id, created_at, updated_at],
            )
            .unwrap();
        for (index, model) in models.iter().enumerate() {
            let event = AiEvent {
                id: format!("event-{index}"),
                turn_id: Some(format!("turn-{index}")),
                kind: if index + 1 == models.len() {
                    "usage".into()
                } else {
                    "prompt".into()
                },
                at: created_at + i64::try_from(index).unwrap(),
                model: Some((*model).into()),
                cwd: "/work".into(),
                data: if index + 1 == models.len() {
                    json!({"total":{"total_tokens":total_tokens}})
                } else {
                    json!({"text":"fixture"})
                },
            };
            repo.conn
                .execute(
                    "INSERT INTO ai_events(session_id,event_id,kind,cwd,data) VALUES (?1,?2,?3,'/work',?4)",
                    params![id, event.id, event.kind, serde_json::to_string(&event).unwrap()],
                )
                .unwrap();
        }
    }

    #[test]
    fn unified_sessions_merge_native_ai_and_classify_legacy_agents() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let mut human = Session::new("host-a".into(), 1_000);
        human.id = "human-1".into();
        repo.insert_session(&human).unwrap();
        repo.insert_entry(&Entry::new(
            human.id,
            "pwd".into(),
            "/human".into(),
            Some(0),
            1_000,
            1_100,
        ))
        .unwrap();

        let mut native_shell = Session::new("host-b".into(), 2_000);
        native_shell.id = "codex-native".into();
        repo.insert_session(&native_shell).unwrap();
        let mut native_command = Entry::new(
            native_shell.id,
            "cargo test".into(),
            "/work".into(),
            Some(0),
            2_100,
            2_200,
        );
        native_command.executor_type = Some("agent".into());
        native_command.executor = Some("openai-codex".into());
        repo.insert_entry(&native_command).unwrap();
        insert_ai_fixture(
            &repo,
            "codex-native",
            2_000,
            2_500,
            &["model-a", "model-b", "model-a"],
            321,
        );

        let mut legacy = Session::new("host-c".into(), 3_000);
        legacy.id = "claude-legacy".into();
        repo.insert_session(&legacy).unwrap();
        let mut legacy_command = Entry::new(
            legacy.id,
            "git status".into(),
            "/legacy".into(),
            Some(0),
            3_000,
            3_100,
        );
        legacy_command.executor_type = Some("agent".into());
        legacy_command.executor = Some("claude-code".into());
        repo.insert_entry(&legacy_command).unwrap();

        insert_ai_fixture(&repo, "codex-discussion", 4_000, 4_500, &["model-c"], 99);

        let sessions = repo.list_unified_sessions(None, None, 10).unwrap();

        assert_eq!(sessions.len(), 4);
        assert_eq!(sessions[0].id, "codex-discussion");
        assert_eq!(sessions[0].kind, SessionKind::Ai);
        assert_eq!(sessions[0].cmd_count, 0);
        let native = sessions.iter().find(|s| s.id == "codex-native").unwrap();
        assert_eq!(native.models, ["model-a", "model-b"]);
        assert_eq!(native.model.as_deref(), Some("model-a"));
        assert_eq!(native.total_tokens, Some(321));
        assert_eq!(native.cmd_count, 1);
        let legacy = sessions.iter().find(|s| s.id == "claude-legacy").unwrap();
        assert_eq!(legacy.kind, SessionKind::Ai);
        assert_eq!(legacy.agent.as_deref(), Some("claude-code"));
        assert_eq!(
            repo.list_unified_sessions(None, None, 1).unwrap()[0].id,
            "codex-discussion"
        );
        assert_eq!(
            repo.list_unified_sessions(Some(3_500), None, 10)
                .unwrap()
                .into_iter()
                .map(|session| session.id)
                .collect::<Vec<_>>(),
            vec!["codex-discussion"]
        );
        let tag_id = repo.create_tag("work", None).unwrap();
        repo.tag_session("codex-native", Some(tag_id)).unwrap();
        assert_eq!(
            repo.list_unified_sessions(None, Some(tag_id), 10)
                .unwrap()
                .into_iter()
                .map(|session| session.id)
                .collect::<Vec<_>>(),
            vec!["codex-native"]
        );
        assert_eq!(
            repo.find_unified_sessions_by_prefix("codex-").unwrap(),
            vec!["codex-discussion", "codex-native"]
        );
    }

    #[test]
    fn imports_are_idempotent_and_cumulative_usage_is_not_summed() {
        let (dir, repo) = crate::test_utils::test_repo();
        let path = dir.path().join("fixture.jsonl");
        std::fs::write(&path, records()).unwrap();
        assert_eq!(import(&repo, &path)["imported_events"], 5);
        let before = repo.get_ai_session("codex-fixture", 100, 0, &[]).unwrap();
        assert_eq!(before["session"]["model"], "test-model");
        assert_eq!(before["session"]["models"], json!(["test-model"]));
        assert_eq!(before["session"]["usage"]["total_tokens"], 180);
        assert_eq!(before["session"]["usage"]["cached_input_tokens"], 50);
        assert!(before["session"]["usage"]["reasoning_output_tokens"].is_null());
        assert_eq!(import(&repo, &path)["imported_events"], 0);
        assert_eq!(
            repo.get_ai_session("codex-fixture", 100, 0, &[]).unwrap(),
            before
        );
        let line = prompt("follow-up");
        let split = line.len() - 2;
        append(&path, &line[..split]);
        assert_eq!(import(&repo, &path)["incomplete_tail"], true);
        append(&path, &line[split..]);
        assert_eq!(import(&repo, &path)["imported_events"], 1);
        assert_eq!(
            repo.get_ai_session("codex-fixture", 100, 0, &[]).unwrap()["events"]
                .as_array()
                .unwrap()
                .len(),
            6
        );
    }

    #[test]
    fn oversized_record_fails_instead_of_returning_a_non_advancing_page() {
        let (dir, repo) = crate::test_utils::test_repo();
        let path = dir.path().join("oversized.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(records().lines().next().unwrap().as_bytes())
            .unwrap();
        file.write_all(b"\n").unwrap();
        file.write_all(&vec![b'x'; 16 * 1024 * 1024]).unwrap();
        file.write_all(b"\n").unwrap();

        let first = import(&repo, &path);
        assert_eq!(first["has_more"], true);
        let error = repo
            .import_codex_session(&path, None, |_| Ok(CapturePolicy::default()))
            .unwrap_err()
            .to_string();
        assert!(error.contains("exceeds 16 MiB"), "{error}");
    }

    #[test]
    fn malformed_transcript_and_wrong_identity_leave_no_partial_import() {
        let (dir, repo) = crate::test_utils::test_repo();
        let path = dir.path().join("fixture.jsonl");
        std::fs::write(&path, format!("{}invalid\n", records())).unwrap();
        assert!(repo
            .import_codex_session(&path, None, |_| Ok(CapturePolicy::default()))
            .is_err());
        assert_eq!(
            repo.list_ai_sessions(20, 0, &[]).unwrap()["sessions"],
            json!([])
        );
        std::fs::write(&path, records()).unwrap();
        assert!(repo
            .import_codex_session(&path, Some("another"), |_| Ok(CapturePolicy::default()))
            .is_err());
        import(&repo, &path);
        std::fs::write(&path, "").unwrap();
        assert!(repo
            .import_codex_session(&path, None, |_| Ok(CapturePolicy::default()))
            .is_err());
    }

    #[test]
    fn paused_capture_advances_without_revealing_content_or_later_totals() {
        let (dir, repo) = crate::test_utils::test_repo();
        let path = dir.path().join("fixture.jsonl");
        std::fs::write(&path, records()).unwrap();
        repo.import_codex_session(&path, None, |_| {
            Ok(CapturePolicy {
                enabled: false,
                paused: true,
                ..CapturePolicy::default()
            })
        })
        .unwrap();
        assert_eq!(
            repo.list_ai_sessions(20, 0, &[]).unwrap()["sessions"],
            json!([])
        );
        append(&path, &prompt("after resume"));
        import(&repo, &path);
        let session = repo.get_ai_session("codex-fixture", 100, 0, &[]).unwrap();
        assert_eq!(session["events"].as_array().unwrap().len(), 1);
        assert_eq!(session["events"][0]["data"]["text"], "after resume");
        assert_eq!(session["session"]["usage_complete"], false);
        assert!(session["session"]["usage"].is_null());
    }

    #[test]
    fn paused_torn_and_unread_records_are_not_backfilled_on_resume() {
        let (dir, repo) = crate::test_utils::test_repo();
        let path = dir.path().join("fixture.jsonl");
        std::fs::write(&path, records()).unwrap();
        import(&repo, &path);
        let hidden = prompt("hidden during pause");
        append(&path, &hidden[..hidden.len() - 2]);
        repo.import_codex_session(&path, None, |_| {
            Ok(CapturePolicy {
                enabled: false,
                paused: true,
                ..CapturePolicy::default()
            })
        })
        .unwrap();
        append(&path, &hidden[hidden.len() - 2..]);
        append(&path, &prompt("visible after pause"));
        import(&repo, &path);
        let data = repo
            .get_ai_session("codex-fixture", 100, 0, &[])
            .unwrap()
            .to_string();
        assert!(!data.contains("hidden during pause"));
        assert!(data.contains("visible after pause"));

        let (large_dir, large_repo) = crate::test_utils::test_repo();
        let large_path = large_dir.path().join("large.jsonl");
        let mut file = std::fs::File::create(&large_path).unwrap();
        file.write_all(records().as_bytes()).unwrap();
        let padding = json!({"type":"ignored","payload":"x".repeat(512*1024)}).to_string();
        for _ in 0..34 {
            writeln!(file, "{padding}").unwrap();
        }
        writeln!(file, "{}", json!({"timestamp":"2026-09-12T12:01:00Z","type":"turn_context","payload":{"cwd":"/another-project"}})).unwrap();
        file.write_all(prompt("unread while paused").as_bytes())
            .unwrap();
        let result = large_repo
            .import_codex_session(&large_path, None, |_| {
                Ok(CapturePolicy {
                    enabled: false,
                    paused: true,
                    ..CapturePolicy::default()
                })
            })
            .unwrap();
        assert_eq!(result["has_more"], true);
        import(&large_repo, &large_path);
        assert_eq!(
            large_repo.list_ai_sessions(20, 0, &[]).unwrap()["sessions"],
            json!([])
        );
        append(&large_path, &prompt("new visible request"));
        import(&large_repo, &large_path);
        let data = large_repo
            .get_ai_session("codex-fixture", 20, 0, &[])
            .unwrap()
            .to_string();
        assert!(!data.contains("unread while paused"));
        assert!(data.contains("new visible request"));
    }

    #[test]
    fn command_edits_invalidate_revision_and_excluded_commands_hide_the_session() {
        let (dir, repo) = crate::test_utils::test_repo();
        let path = dir.path().join("fixture.jsonl");
        std::fs::write(&path, records()).unwrap();
        import(&repo, &path);
        repo.conn
            .execute(
                "INSERT INTO sessions(id,hostname,created_at) VALUES ('codex-fixture','fixture',0)",
                [],
            )
            .unwrap();
        repo.conn.execute("INSERT INTO entries(session_id,command,cwd,started_at,ended_at,duration_ms) VALUES ('codex-fixture','git status','/work/project',0,0,0)",[]).unwrap();
        let before = repo.get_ai_session("codex-fixture", 20, 0, &[]).unwrap();
        repo.conn
            .execute(
                "UPDATE entries SET command='git diff' WHERE session_id='codex-fixture'",
                [],
            )
            .unwrap();
        let after = repo.get_ai_session("codex-fixture", 20, 0, &[]).unwrap();
        assert_ne!(before["session"]["revision"], after["session"]["revision"]);
        repo.conn
            .execute(
                "UPDATE entries SET cwd='/private-project' WHERE session_id='codex-fixture'",
                [],
            )
            .unwrap();
        assert!(repo
            .get_ai_session("codex-fixture", 20, 0, &["/private-project".into()])
            .is_err());
        assert_eq!(
            repo.list_ai_sessions(20, 0, &["/private-project".into()])
                .unwrap()["sessions"],
            json!([])
        );
    }

    #[test]
    fn disabled_project_does_not_suppress_other_projects_in_same_chunk() {
        for ends_private in [false, true] {
            let (dir, repo) = crate::test_utils::test_repo();
            let path = dir.path().join("mixed.jsonl");
            let context = |cwd: &str| {
                format!(
                    "{}\n",
                    json!({"timestamp":"2026-09-12T12:01:00Z","type":"turn_context","payload":{"cwd":cwd}})
                )
            };
            let mut text = records();
            text.push_str(&context("/private-project"));
            text.push_str(&prompt("private request"));
            if !ends_private {
                text.push_str(&context("/work/project"));
                text.push_str(&prompt("later public request"));
            }
            std::fs::write(&path, text).unwrap();
            repo.import_codex_session(&path, None, |cwd| {
                Ok(CapturePolicy {
                    enabled: cwd != "/private-project",
                    ..CapturePolicy::default()
                })
            })
            .unwrap();
            let data = repo
                .get_ai_session("codex-fixture", 100, 0, &[])
                .unwrap()
                .to_string();
            assert!(data.contains("Fix the synthetic test"));
            assert!(!data.contains("private request"));
            if !ends_private {
                assert!(data.contains("later public request"));
            }
        }
    }

    #[test]
    fn literal_exclusions_match_existing_shell_history_semantics() {
        let (dir, repo) = crate::test_utils::test_repo();
        let path = dir.path().join("fixture.jsonl");
        std::fs::write(&path, format!("{}{}", records(), prompt("remove *.log"))).unwrap();
        repo.import_codex_session(&path, None, |_| {
            Ok(CapturePolicy {
                exclusions: vec!["*.log".into()],
                ..CapturePolicy::default()
            })
        })
        .unwrap();
        let data = repo
            .get_ai_session("codex-fixture", 100, 0, &[])
            .unwrap()
            .to_string();
        assert!(data.contains("Fix the synthetic test"));
        assert!(!data.contains("remove *.log"));
    }

    #[test]
    fn text_is_redacted_truncated_and_project_exclusions_are_honored() {
        let (dir, repo) = crate::test_utils::test_repo();
        let path = dir.path().join("fixture.jsonl");
        std::fs::write(
            &path,
            format!(
                "{}{}",
                records(),
                prompt("synthetic-private-key then more text")
            ),
        )
        .unwrap();
        repo.import_codex_session(&path, None, |cwd| {
            assert_eq!(cwd, "/work/project");
            Ok(CapturePolicy {
                extra_patterns: vec!["synthetic-private-key".into()],
                exclusions: vec!["Fix the synthetic".into()],
                max_chars: 20,
                ..CapturePolicy::default()
            })
        })
        .unwrap();
        let session = repo.get_ai_session("codex-fixture", 100, 0, &[]).unwrap();
        let text = session.to_string();
        assert!(!text.contains("synthetic-private-key"));
        assert!(!text.contains("Fix the synthetic"));
        assert_eq!(
            session["events"].as_array().unwrap().last().unwrap()["data"]["truncated"],
            true
        );
        assert!(repo
            .get_ai_session("codex-fixture", 20, 0, &["/work".into()])
            .is_err());
        assert!(repo
            .get_ai_session("codex-fixture", 20, 0, &["/worker".into()])
            .is_ok());
    }

    #[test]
    fn summary_checkpoints_resume_after_append_and_invalidate_on_prefix_change() {
        let (dir, repo) = crate::test_utils::test_repo();
        let path = dir.path().join("fixture.jsonl");
        std::fs::write(&path, records()).unwrap();
        import(&repo, &path);
        let page = repo.get_ai_session("codex-fixture", 2, 0, &[]).unwrap();
        assert_eq!(page["next_offset"], 2);
        let mut input = SummaryInput {
            session_id: "codex-fixture".into(),
            source_revision: page["session"]["revision"].as_str().unwrap().into(),
            text: "The user asked for a synthetic fix.".into(),
            agent: "claude".into(),
            model: "fixture-writer".into(),
            source_ids: vec!["foreign-source".into()],
            base_summary_id: None,
        };
        assert!(repo.save_ai_summary(&input, &[]).is_err());
        input.source_ids = vec![page["events"][1]["id"].as_str().unwrap().into()];
        let saved = repo.save_ai_summary(&input, &[]).unwrap();
        let base_summary_id = saved["id"].as_str().unwrap().to_owned();
        let current = repo.get_ai_session("codex-fixture", 20, 0, &[]).unwrap();
        let summary = &current["summaries"][0];
        assert_eq!(summary["current"], true);
        assert_eq!(summary["stale"], false);
        assert_eq!(summary["incremental_safe"], true);
        assert_eq!(
            summary["source_event_count"],
            current["session"]["event_count"]
        );
        assert_eq!(
            summary["source_command_count"],
            current["session"]["command_count"]
        );

        append(&path, &prompt("new request"));
        import(&repo, &path);
        let appended = repo.get_ai_session("codex-fixture", 20, 0, &[]).unwrap();
        let summary = &appended["summaries"][0];
        assert_eq!(summary["current"], false);
        assert_eq!(summary["stale"], false);
        assert_eq!(summary["has_new_activity"], true);
        assert_eq!(summary["incremental_safe"], true);
        assert_eq!(
            summary["resume_event_offset"],
            current["session"]["event_count"]
        );
        assert_eq!(
            summary["resume_command_offset"],
            current["session"]["command_count"]
        );

        let newest_event = appended["events"].as_array().unwrap().last().unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        input.source_revision = appended["session"]["revision"].as_str().unwrap().to_owned();
        input.text = "The synthetic fix now includes a new request.".into();
        input.base_summary_id = Some(base_summary_id.clone());
        input.source_ids = vec![newest_event.clone()];
        assert!(repo.save_ai_summary(&input, &[]).is_err());
        input.source_ids = vec![
            page["events"][1]["id"].as_str().unwrap().into(),
            newest_event,
        ];
        let updated = repo.save_ai_summary(&input, &[]).unwrap();
        assert_eq!(updated["base_summary_id"], base_summary_id);
        let checkpoint = repo.get_ai_session("codex-fixture", 20, 0, &[]).unwrap();
        assert_eq!(checkpoint["summaries"][0]["current"], true);
        assert_eq!(
            checkpoint["summaries"][0]["base_summary_id"],
            base_summary_id
        );

        let event_id = page["events"][1]["id"].as_str().unwrap();
        mutate_event(&repo, event_id);
        let changed = repo.get_ai_session("codex-fixture", 20, 0, &[]).unwrap();
        assert_eq!(changed["summaries"][0]["stale"], true);
        assert_eq!(changed["summaries"][0]["incremental_safe"], false);

        assert!(repo.save_ai_summary(&input, &[]).is_err());
        repo.delete_ai_session("codex-fixture").unwrap();
        assert!(repo.get_ai_session("codex-fixture", 20, 0, &[]).is_err());
        let count: i64 = repo
            .conn
            .query_row("SELECT COUNT(*) FROM ai_summaries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn ai_schema_migrates_and_empty_sessions_are_queryable() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let result = repo.list_ai_sessions(20, 0, &[]).unwrap();
        assert_eq!(result["sessions"], serde_json::json!([]));
    }

    #[test]
    fn ai_summaries_for_session_orders_newest_first_and_flags_only_current_as_current() {
        let (dir, repo) = crate::test_utils::test_repo();
        let path = dir.path().join("fixture.jsonl");
        std::fs::write(&path, records()).unwrap();
        import(&repo, &path);

        assert_eq!(repo.ai_summaries_for_session("codex-fixture").unwrap(), []);

        let page = repo.get_ai_session("codex-fixture", 20, 0, &[]).unwrap();
        let first = SummaryInput {
            session_id: "codex-fixture".into(),
            source_revision: page["session"]["revision"].as_str().unwrap().into(),
            text: "First summary".into(),
            agent: "claude".into(),
            model: "fixture-writer".into(),
            source_ids: vec![page["events"][0]["id"].as_str().unwrap().into()],
            base_summary_id: None,
        };
        repo.save_ai_summary(&first, &[]).unwrap();

        append(&path, &prompt("a follow-up request"));
        import(&repo, &path);
        let appended = repo.get_ai_session("codex-fixture", 20, 0, &[]).unwrap();
        let second = SummaryInput {
            session_id: "codex-fixture".into(),
            source_revision: appended["session"]["revision"].as_str().unwrap().into(),
            text: "Second summary".into(),
            agent: "codex".into(),
            model: "fixture-writer-2".into(),
            source_ids: vec![appended["events"].as_array().unwrap().last().unwrap()["id"]
                .as_str()
                .unwrap()
                .into()],
            base_summary_id: None,
        };
        repo.save_ai_summary(&second, &[]).unwrap();

        let records = repo.ai_summaries_for_session("codex-fixture").unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].text, "Second summary");
        assert_eq!(records[0].agent, "codex");
        assert_eq!(records[0].model, "fixture-writer-2");
        assert!(records[0].current);
        assert_eq!(records[1].text, "First summary");
        assert!(!records[1].current);
    }

    #[test]
    fn import_opencode_session_stores_events_and_dedupes_on_reimport() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let messages = serde_json::json!([
            {
                "info": {
                    "id": "msg_u1", "sessionID": "ses_1", "role": "user",
                    "time": {"created": 1000},
                    "agent": "build", "model": {"providerID": "anthropic", "modelID": "claude-sonnet-5"}
                },
                "parts": [{"id": "msg_u1-p1", "sessionID": "ses_1", "messageID": "msg_u1", "type": "text", "text": "hi"}]
            },
            {
                "info": {
                    "id": "msg_a1", "sessionID": "ses_1", "role": "assistant",
                    "time": {"created": 1100, "completed": 1600},
                    "parentID": "msg_u1", "modelID": "claude-sonnet-5", "providerID": "anthropic",
                    "mode": "build", "path": {"cwd": "/work", "root": "/work"}, "cost": 0.0,
                    "tokens": {"input": 10, "output": 5, "reasoning": 0, "cache": {"read": 0, "write": 0}},
                    "finish": "stop"
                },
                "parts": [{"id": "msg_a1-p1", "sessionID": "ses_1", "messageID": "msg_a1", "type": "text", "text": "hello"}]
            }
        ])
        .to_string();

        let result = repo
            .import_opencode_session("ses_1", "/work", &messages, |_| {
                Ok(crate::ai_sessions::CapturePolicy::default())
            })
            .unwrap();
        assert_eq!(result["imported_events"], 3); // prompt, response, usage

        let session = repo.get_ai_session("opencode-ses_1", 20, 0, &[]).unwrap();
        assert_eq!(session["session"]["agent"], "opencode");
        assert_eq!(session["events"].as_array().unwrap().len(), 3);

        let second = repo
            .import_opencode_session("ses_1", "/work", &messages, |_| {
                Ok(crate::ai_sessions::CapturePolicy::default())
            })
            .unwrap();
        assert_eq!(second["imported_events"], 0);
    }

    #[test]
    fn import_opencode_session_only_imports_new_messages_on_incremental_growth() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let first_message = serde_json::json!({
            "info": {
                "id": "msg_u1", "sessionID": "ses_1", "role": "user",
                "time": {"created": 1000},
                "agent": "build", "model": {"providerID": "anthropic", "modelID": "claude-sonnet-5"}
            },
            "parts": [{"id": "msg_u1-p1", "sessionID": "ses_1", "messageID": "msg_u1", "type": "text", "text": "hi"}]
        });
        let mut messages = vec![first_message];

        let first_call = repo
            .import_opencode_session(
                "ses_1",
                "/work",
                &serde_json::Value::Array(messages.clone()).to_string(),
                |_| Ok(crate::ai_sessions::CapturePolicy::default()),
            )
            .unwrap();
        assert_eq!(first_call["imported_events"], 1); // just the prompt

        let second_message = serde_json::json!({
            "info": {
                "id": "msg_a1", "sessionID": "ses_1", "role": "assistant",
                "time": {"created": 1100, "completed": 1600},
                "parentID": "msg_u1", "modelID": "claude-sonnet-5", "providerID": "anthropic",
                "mode": "build", "path": {"cwd": "/work", "root": "/work"}, "cost": 0.0,
                "tokens": {"input": 10, "output": 5, "reasoning": 0, "cache": {"read": 0, "write": 0}},
                "finish": "stop"
            },
            "parts": [{"id": "msg_a1-p1", "sessionID": "ses_1", "messageID": "msg_a1", "type": "text", "text": "hello"}]
        });
        messages.push(second_message);

        let second_call = repo
            .import_opencode_session(
                "ses_1",
                "/work",
                &serde_json::Value::Array(messages).to_string(),
                |_| Ok(crate::ai_sessions::CapturePolicy::default()),
            )
            .unwrap();
        // Only the new assistant message's events (response + usage) are new;
        // the first message must not be re-imported.
        assert_eq!(second_call["imported_events"], 2);

        let session = repo.get_ai_session("opencode-ses_1", 20, 0, &[]).unwrap();
        assert_eq!(session["events"].as_array().unwrap().len(), 3); // 1 (first call) + 2 (second call)
    }

    #[test]
    fn import_opencode_session_rejects_adapter_version_mismatch() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let messages = serde_json::json!([{
            "info": {
                "id": "msg_u1", "sessionID": "ses_1", "role": "user",
                "time": {"created": 1000},
                "agent": "build", "model": {"providerID": "anthropic", "modelID": "claude-sonnet-5"}
            },
            "parts": [{"id": "msg_u1-p1", "sessionID": "ses_1", "messageID": "msg_u1", "type": "text", "text": "hi"}]
        }])
        .to_string();

        repo.import_opencode_session("ses_1", "/work", &messages, |_| {
            Ok(crate::ai_sessions::CapturePolicy::default())
        })
        .unwrap();

        repo.conn
            .execute(
                "UPDATE ai_sources SET adapter_version=9999 WHERE path='opencode-session:ses_1'",
                [],
            )
            .unwrap();

        assert!(repo
            .import_opencode_session("ses_1", "/work", &messages, |_| {
                Ok(crate::ai_sessions::CapturePolicy::default())
            })
            .is_err());
    }
}
