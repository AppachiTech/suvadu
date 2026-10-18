//! Incremental adapter for Codex native JSONL transcripts.
use super::AiEvent;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

pub const ADAPTER_VERSION: u32 = 1;

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CodexState {
    pub native_id: Option<String>,
    pub cwd: String,
    pub model: Option<String>,
    pub turn_id: Option<String>,
    pub parent_id: Option<String>,
    /// When this source's own session was created, in Unix seconds.
    pub created_at: Option<i64>,
    /// Inside history Codex copied from the parent into a forked rollout.
    pub replaying_parent: bool,
}

/// Parse complete records, leaving an unfinished tail for the next import.
/// State changes are committed only when every complete record is valid;
/// malformed lines are skipped as [`super::for_each_jsonl_record`] describes.
pub fn parse_chunk(
    bytes: &[u8],
    offset: u64,
    state: &mut CodexState,
) -> Result<super::ParsedChunk, String> {
    let mut next = state.clone();
    let mut events = Vec::new();
    let (consumed, malformed) =
        super::for_each_jsonl_record(bytes, offset, "Codex", |record, absolute| {
            if let Some(event) = parse_record(record, absolute, &mut next)? {
                events.push(event);
            }
            Ok(())
        })?;
    *state = next;
    Ok((events, consumed, malformed))
}

fn parse_record(
    record: &Value,
    offset: u64,
    state: &mut CodexState,
) -> Result<Option<AiEvent>, String> {
    let record_type = record["type"].as_str().unwrap_or_default();
    let payload = &record["payload"];
    if !matches!(
        record_type,
        "session_meta" | "turn_context" | "event_msg" | "response_item"
    ) {
        return Ok(None);
    }
    if !payload.is_object() {
        return Err("expected an object payload".into());
    }
    let subtype = payload["type"].as_str().unwrap_or_default();
    if record_type == "event_msg"
        && !matches!(
            subtype,
            "user_message"
                | "agent_message"
                | "token_count"
                | "task_started"
                | "task_complete"
                | "turn_aborted"
        )
    {
        return Ok(None);
    }
    if record_type == "response_item" && subtype != "message" {
        return Ok(None);
    }
    let timestamp = record["timestamp"].as_str().ok_or("missing timestamp")?;
    let at = chrono::DateTime::parse_from_rfc3339(timestamp)
        .map_err(|_| "invalid timestamp")?
        .timestamp_millis();
    if skip_replayed_parent(record_type, payload, state)? {
        return Ok(None);
    }
    if record_type != "session_meta" && state.native_id.is_none() {
        return Err("session metadata must precede events".into());
    }
    let (kind, data) = match record_type {
        "session_meta" => ("session_started", session_metadata(payload, at, state)?),
        "turn_context" => {
            update_context(payload, state)?;
            return Ok(None);
        }
        "event_msg" => {
            let Some(message) = event_message(payload, state)? else {
                return Ok(None);
            };
            message
        }
        "response_item" => {
            let Some(message) = response_item_message(payload)? else {
                return Ok(None);
            };
            message
        }
        _ => unreachable!(),
    };
    Ok(Some(AiEvent {
        id: format!("codex-{offset}"),
        turn_id: state.turn_id.clone(),
        kind: kind.into(),
        at,
        model: state.model.clone(),
        cwd: state.cwd.clone(),
        data,
    }))
}

/// A forked or subagent rollout starts with the child's own `session_meta`,
/// then replays the parent's rollout -- its `session_meta` first -- before
/// the child's first turn. The replayed records are the parent's activity,
/// already captured from the parent's own rollout, so none of them are
/// credited to the child. They carry fork-time record timestamps; the
/// replay ends at the first `task_started` that did not begin before the
/// child existed (its `started_at` keeps the turn's real start), or that
/// gives no start time to prove otherwise.
fn skip_replayed_parent(
    record_type: &str,
    payload: &Value,
    state: &mut CodexState,
) -> Result<bool, String> {
    if record_type == "session_meta" {
        let Some(current) = state.native_id.as_deref() else {
            return Ok(false);
        };
        let id = native_session_id(payload)?;
        if id == current {
            return Ok(state.replaying_parent);
        }
        if state.replaying_parent || state.parent_id.as_deref() == Some(id) {
            state.replaying_parent = true;
            return Ok(true);
        }
        return Err("native session ID changed within source".into());
    }
    if !state.replaying_parent {
        return Ok(false);
    }
    if record_type == "event_msg" && payload["type"].as_str() == Some("task_started") {
        let began_before_child = match (payload["started_at"].as_i64(), state.created_at) {
            (Some(started), Some(created)) => started < created,
            _ => false,
        };
        if !began_before_child {
            state.replaying_parent = false;
            return Ok(false);
        }
    }
    Ok(true)
}

fn native_session_id(payload: &Value) -> Result<&str, String> {
    let id = payload
        .get("id")
        .or_else(|| payload.get("session_id"))
        .and_then(Value::as_str)
        .ok_or("missing native session ID")?;
    validate_id(id)?;
    Ok(id)
}

fn event_message(
    payload: &Value,
    state: &mut CodexState,
) -> Result<Option<(&'static str, Value)>, String> {
    let subtype = payload["type"].as_str().unwrap_or_default();
    let message = match subtype {
        "user_message" => ("prompt", message(payload)?),
        "agent_message" => {
            if payload
                .get("phase")
                .is_some_and(|phase| !phase.is_null() && phase.as_str() != Some("final_answer"))
            {
                return Ok(None);
            }
            ("response", message(payload)?)
        }
        "token_count" => {
            let Some(info) = payload.get("info").filter(|info| !info.is_null()) else {
                return Ok(None);
            };
            if !info.is_object() {
                return Err("invalid usage info".into());
            }
            let mut data = json!({"total": counters(info.get("total_token_usage"))?});
            if let Some(last) = info.get("last_token_usage") {
                data["last"] = counters(Some(last))?;
            }
            ("usage", data)
        }
        "task_started" | "task_complete" | "turn_aborted" => {
            if let Some(turn) = optional_string(payload, "turn_id")? {
                state.turn_id = Some(turn);
            }
            let kind = match subtype {
                "task_started" => "turn_started",
                "task_complete" => "turn_completed",
                _ => "turn_interrupted",
            };
            let mut data = json!({});
            if let Some(duration) = payload.get("duration_ms") {
                if !duration.is_null() && duration.as_u64().is_none() {
                    return Err("invalid duration_ms".into());
                }
                data["duration_ms"] = duration.clone();
            }
            (kind, data)
        }
        _ => return Ok(None),
    };
    Ok(Some(message))
}

fn response_item_message(payload: &Value) -> Result<Option<(&'static str, Value)>, String> {
    let role = payload["role"].as_str().unwrap_or_default();
    let kind = match role {
        "user" if has_user_text_metadata(payload) => "prompt",
        "assistant" if payload["phase"].as_str() == Some("final_answer") => "response",
        _ => return Ok(None),
    };
    let content = payload["content"]
        .as_array()
        .ok_or("invalid message content: expected array")?;
    let text_kind = if kind == "prompt" {
        "input_text"
    } else {
        "output_text"
    };
    let text = content
        .iter()
        .filter(|item| item["type"].as_str() == Some(text_kind))
        .filter_map(|item| item["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        return Ok(None);
    }
    Ok(Some((kind, json!({"text":text}))))
}

fn has_user_text_metadata(payload: &Value) -> bool {
    payload["internal_chat_message_metadata_passthrough"]["content_item_kinds"]
        .as_array()
        .is_some_and(|kinds| kinds.iter().any(|kind| kind.as_str() == Some("user.text")))
}

fn session_metadata(payload: &Value, at: i64, state: &mut CodexState) -> Result<Value, String> {
    // A different ID never reaches here: `skip_replayed_parent` treats it
    // as the parent's replayed history or rejects it.
    let id = native_session_id(payload)?;
    if state.native_id.is_none() {
        let created = optional_string(payload, "timestamp")?
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(&value).ok())
            .map_or_else(|| at.div_euclid(1000), |value| value.timestamp());
        state.created_at = Some(created);
    }
    state.native_id = Some(id.to_owned());
    update_context(payload, state)?;
    if let Some(parent) = optional_string(payload, "parent_id")?
        .or(optional_string(payload, "forked_from_id")?)
        .or(optional_string(payload, "parent_thread_id")?)
    {
        validate_id(&parent)?;
        state.parent_id = Some(parent);
    }
    let mut metadata = json!({"native_id":id});
    for field in ["source", "model_provider", "cli_version"] {
        if let Some(value) = payload[field].as_str() {
            metadata[field] = json!(value);
        }
    }
    Ok(metadata)
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.len() > 128 || !crate::util::is_valid_session_id(id) {
        return Err("invalid native session ID".into());
    }
    Ok(())
}

fn optional_string(payload: &Value, field: &str) -> Result<Option<String>, String> {
    match payload.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!("invalid {field}: expected string")),
    }
}

fn update_context(payload: &Value, state: &mut CodexState) -> Result<(), String> {
    if let Some(cwd) = optional_string(payload, "cwd")? {
        state.cwd = cwd;
    }
    if let Some(model) = optional_string(payload, "model")? {
        state.model = Some(model);
    }
    if let Some(turn) = optional_string(payload, "turn_id")? {
        state.turn_id = Some(turn);
    }
    Ok(())
}

fn message(payload: &Value) -> Result<Value, String> {
    payload["message"]
        .as_str()
        .map(|text| json!({"text":text}))
        .ok_or_else(|| "missing message text".into())
}

fn counters(value: Option<&Value>) -> Result<Value, String> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(Value::Null);
    };
    let object = value
        .as_object()
        .ok_or("invalid token usage: expected object")?;
    let mut counters = Map::new();
    for field in [
        "input_tokens",
        "cached_input_tokens",
        "cache_write_input_tokens",
        "output_tokens",
        "reasoning_output_tokens",
        "total_tokens",
    ] {
        if let Some(counter) = object.get(field) {
            if !counter.is_null() && counter.as_u64().is_none() {
                return Err(format!("invalid token counter {field}"));
            }
            counters.insert(field.to_owned(), counter.clone());
        }
    }
    Ok(Value::Object(counters))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[allow(clippy::needless_pass_by_value)] // Inline fixture builders own their JSON values.
    fn record(kind: &str, payload: Value) -> String {
        format!(
            "{}\n",
            json!({"timestamp":"2026-09-12T10:00:00.123Z","type":kind,"payload":payload})
        )
    }

    fn meta() -> String {
        record(
            "session_meta",
            json!({"id":"native-123","cwd":"/work","base_instructions":{"text":"secret"},"source":"cli","cli_version":"0.100.0"}),
        )
    }

    fn initialized() -> CodexState {
        CodexState {
            native_id: Some("native-123".into()),
            cwd: "/work".into(),
            ..CodexState::default()
        }
    }

    #[test]
    fn standalone_prompt_uses_metadata_without_capturing_instructions() {
        let input = meta()
            + &record(
                "event_msg",
                json!({"type":"user_message","message":"Explain this repo"}),
            );
        let mut state = CodexState::default();
        let (events, consumed, _) = parse_chunk(input.as_bytes(), 0, &mut state).unwrap();
        assert_eq!(consumed, input.len());
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "session_started");
        assert!(!events[0].data.to_string().contains("secret"));
        assert_eq!(events[1].kind, "prompt");
        assert_eq!(events[1].data, json!({"text":"Explain this repo"}));
        assert_eq!(events[1].cwd, "/work");
        assert_eq!(events[1].at, 1_789_207_200_123);
        assert!(events[1].turn_id.is_none());
        assert_eq!(state.native_id.as_deref(), Some("native-123"));
    }

    #[test]
    fn current_response_items_capture_user_text_and_final_answer() {
        let input = record(
            "response_item",
            json!({
                "type":"message",
                "role":"user",
                "content":[
                    {"type":"input_text","text":"Review the change."},
                    {"type":"input_text","text":"Focus on privacy."}
                ],
                "internal_chat_message_metadata_passthrough":{
                    "content_item_kinds":["user.text"]
                }
            }),
        ) + &record(
            "response_item",
            json!({
                "type":"message",
                "role":"assistant",
                "phase":"final_answer",
                "content":[
                    {"type":"output_text","text":"The change is safe."},
                    {"type":"output_text","text":"No private context is stored."}
                ]
            }),
        );

        let (events, _, _) = parse_chunk(input.as_bytes(), 0, &mut initialized()).unwrap();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "prompt");
        assert_eq!(
            events[0].data,
            json!({"text":"Review the change.\nFocus on privacy."})
        );
        assert_eq!(events[1].kind, "response");
        assert_eq!(
            events[1].data,
            json!({"text":"The change is safe.\nNo private context is stored."})
        );
    }

    #[test]
    fn current_response_items_exclude_injected_context_and_non_final_output() {
        let input = record(
            "response_item",
            json!({
                "type":"message",
                "role":"user",
                "content":[{"type":"input_text","text":"<environment_context>secret</environment_context>"}],
                "internal_chat_message_metadata_passthrough":{
                    "content_item_kinds":["agents_md.instructions","environments.environment_context"]
                }
            }),
        ) + &record(
            "response_item",
            json!({
                "type":"message",
                "role":"developer",
                "content":[{"type":"input_text","text":"hook instructions"}]
            }),
        ) + &record(
            "response_item",
            json!({
                "type":"message",
                "role":"assistant",
                "phase":"commentary",
                "content":[{"type":"output_text","text":"internal progress"}]
            }),
        ) + &record(
            "event_msg",
            json!({
                "type":"item_completed",
                "item":{"type":"UserMessage","content":"duplicate"}
            }),
        );

        let (events, _, _) = parse_chunk(input.as_bytes(), 0, &mut initialized()).unwrap();

        assert!(events.is_empty());
    }

    #[test]
    fn only_final_or_legacy_agent_messages_are_captured() {
        let input = record(
            "event_msg",
            json!({"type":"agent_message","message":"thinking","phase":"commentary"}),
        ) + &record(
            "response_item",
            json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"answer"}]}),
        ) + &record(
            "event_msg",
            json!({"type":"agent_reasoning","text":"secret"}),
        ) + &record(
            "event_msg",
            json!({"type":"agent_message","message":"answer","phase":"final_answer"}),
        ) + &record(
            "event_msg",
            json!({"type":"agent_message","message":"legacy"}),
        ) + &record(
            "event_msg",
            json!({"type":"task_complete","turn_id":"t1","last_agent_message":"answer"}),
        );
        let (events, _, _) = parse_chunk(input.as_bytes(), 200, &mut initialized()).unwrap();
        assert_eq!(
            events.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
            ["response", "response", "turn_completed"]
        );
        assert_eq!(events[0].data, json!({"text":"answer"}));
        assert_eq!(events[1].data, json!({"text":"legacy"}));
        assert!(events[2].data.get("last_agent_message").is_none());
    }

    #[test]
    fn usage_preserves_missing_counters_and_replay_identity() {
        let input = record(
            "event_msg",
            json!({"type":"token_count","info":{"total_token_usage":{"input_tokens":50,"output_tokens":null,"total_tokens":50,"future_counter":99},"last_token_usage":{"cached_input_tokens":0},"model_context_window":1000}}),
        );
        let (events, _, _) = parse_chunk(input.as_bytes(), 432, &mut initialized()).unwrap();
        let (replay, _, _) = parse_chunk(input.as_bytes(), 432, &mut initialized()).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "usage");
        assert_eq!(events[0].id, "codex-432");
        assert_eq!(events[0].id, replay[0].id);
        assert_eq!(events[0].data["total"]["input_tokens"], 50);
        assert_eq!(events[0].data["last"], json!({"cached_input_tokens":0}));
        assert!(events[0].data["total"].get("cached_input_tokens").is_none());
        assert!(events[0].data["total"].get("future_counter").is_none());
        assert!(events[0].data["total"]["output_tokens"].is_null());
    }

    #[test]
    fn partial_json_waits_for_newline_and_resumes_at_byte_offset() {
        let first = meta();
        let prompt = record("event_msg", json!({"type":"user_message","message":"தமிழ்"}));
        let cut = prompt.len() - 4;
        let input = first.clone() + &prompt[..cut];
        let mut state = CodexState::default();
        let (events, consumed, _) = parse_chunk(input.as_bytes(), 0, &mut state).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(consumed, first.len());
        let (events, consumed, _) =
            parse_chunk(prompt.as_bytes(), first.len() as u64, &mut state).unwrap();
        assert_eq!(consumed, prompt.len());
        assert_eq!(events[0].id, format!("codex-{}", first.len()));
        assert_eq!(events[0].data["text"], "தமிழ்");
    }

    #[test]
    fn model_and_turn_context_follow_native_changes() {
        let input = record(
            "turn_context",
            json!({"turn_id":"t1","cwd":"/one","model":"model-a"}),
        ) + &record("event_msg", json!({"type":"task_started","turn_id":"t1"}))
            + &record("event_msg", json!({"type":"user_message","message":"one"}))
            + &record(
                "turn_context",
                json!({"turn_id":"t2","cwd":"/two","model":"model-b"}),
            )
            + &record(
                "event_msg",
                json!({"type":"agent_message","message":"two","phase":"final_answer"}),
            )
            + &record("event_msg", json!({"type":"turn_aborted","turn_id":"t2"}));
        let (events, _, _) = parse_chunk(input.as_bytes(), 0, &mut initialized()).unwrap();
        assert_eq!(events.len(), 4);
        assert_eq!(events[0].kind, "turn_started");
        assert_eq!(events[1].model.as_deref(), Some("model-a"));
        assert_eq!(events[2].model.as_deref(), Some("model-b"));
        assert_eq!(events[2].turn_id.as_deref(), Some("t2"));
        assert_eq!(events[2].cwd, "/two");
        assert_eq!(events[3].kind, "turn_interrupted");
    }

    #[test]
    fn invalid_timestamps_error() {
        let input = "{\"type\":\"event_msg\",\"timestamp\":\"bad\",\"payload\":{\"type\":\"user_message\",\"message\":\"hi\"}}\n";
        assert!(parse_chunk(input.as_bytes(), 0, &mut initialized()).is_err());
    }

    #[test]
    fn a_malformed_line_followed_by_complete_records_is_skipped_and_counted() {
        let prompt = record(
            "event_msg",
            json!({"type":"user_message","message":"after"}),
        );
        // A torn write leaves a fragment that the next append ends with its
        // own record and newline; the writer has moved on past it.
        let torn_write = format!("{}{prompt}", &prompt[..20]);
        for torn in [torn_write.as_str(), "null\n", "{\"payload\":{}}\n"] {
            let input = format!("{torn}{prompt}");
            let mut state = initialized();
            let (events, consumed, malformed) =
                parse_chunk(input.as_bytes(), 0, &mut state).unwrap();
            assert_eq!(consumed, input.len(), "{torn}");
            assert_eq!(malformed, 1, "{torn}");
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].data, json!({"text":"after"}));
            assert_eq!(events[0].id, format!("codex-{}", torn.len()));
        }
    }

    #[test]
    fn a_malformed_final_line_waits_for_a_later_record() {
        let prompt = record(
            "event_msg",
            json!({"type":"user_message","message":"before"}),
        );
        let input = format!("{prompt}{{broken}}\n");
        let mut state = initialized();
        let (events, consumed, malformed) = parse_chunk(input.as_bytes(), 0, &mut state).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(consumed, prompt.len());
        assert_eq!(malformed, 0);
    }

    #[test]
    fn oversized_records_error_even_when_incomplete() {
        let input = vec![b' '; 16 * 1024 * 1024 + 1];
        assert!(parse_chunk(&input, 0, &mut initialized()).is_err());
    }

    #[test]
    fn large_compaction_records_do_not_abort_the_import() {
        let large_snapshot = "x".repeat(1_100_000);
        let input = meta()
            + &record("compacted", json!({"replacement_history":large_snapshot}))
            + &record(
                "turn_context",
                json!({"turn_id":"t1","cwd":"/work","model":"model-a"}),
            )
            + &record(
                "event_msg",
                json!({"type":"token_count","info":{"total_token_usage":{"total_tokens":42}}}),
            );

        let mut state = CodexState::default();
        let (events, consumed, _) = parse_chunk(input.as_bytes(), 0, &mut state).unwrap();

        assert_eq!(consumed, input.len());
        assert_eq!(
            events.last().map(|event| event.kind.as_str()),
            Some("usage")
        );
        assert_eq!(
            events.last().and_then(|event| event.model.as_deref()),
            Some("model-a")
        );
        assert_eq!(
            events
                .last()
                .map(|event| &event.data["total"]["total_tokens"]),
            Some(&json!(42))
        );
    }

    #[test]
    fn large_image_bearing_prompt_captures_only_its_text() {
        let large_image = format!("data:image/png;base64,{}", "A".repeat(1_100_000));
        let input = record(
            "response_item",
            json!({
                "type":"message",
                "role":"user",
                "content":[
                    {"type":"input_text","text":"Describe this screenshot."},
                    {"type":"input_image","image_url":large_image}
                ],
                "internal_chat_message_metadata_passthrough":{
                    "content_item_kinds":["user.text", "user.image"]
                }
            }),
        );

        let (events, consumed, _) = parse_chunk(input.as_bytes(), 0, &mut initialized()).unwrap();

        assert_eq!(consumed, input.len());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "prompt");
        assert_eq!(events[0].data, json!({"text":"Describe this screenshot."}));
    }

    #[test]
    fn metadata_is_required_and_native_identity_cannot_change() {
        let prompt = record("event_msg", json!({"type":"user_message","message":"hi"}));
        assert!(parse_chunk(prompt.as_bytes(), 0, &mut CodexState::default()).is_err());
        for id in [
            "../../bad".to_owned(),
            "a".repeat(129),
            String::new(),
            "different".to_owned(),
        ] {
            let input = meta() + &record("session_meta", json!({"id":id,"cwd":"/work"}));
            assert!(parse_chunk(input.as_bytes(), 0, &mut CodexState::default()).is_err());
        }
    }

    #[test]
    fn invalid_known_token_counters_error_in_totals_or_last_usage() {
        for value in [json!(-1), json!(1.5), json!("30"), json!(true)] {
            for location in ["total_token_usage", "last_token_usage"] {
                let mut info = json!({"total_token_usage":{"input_tokens":30}});
                info[location] = json!({"input_tokens":value});
                let input = record("event_msg", json!({"type":"token_count","info":info}));
                assert!(parse_chunk(input.as_bytes(), 0, &mut initialized()).is_err());
            }
        }
    }

    /// A forked or subagent rollout as Codex writes it: the child's own
    /// metadata, then the parent's metadata and history replayed verbatim,
    /// then the child's first turn. Every replayed record carries the
    /// fork-time timestamp; only `task_started.started_at` keeps the time
    /// the parent's turn really began.
    fn forked_rollout() -> (String, String) {
        let replay = record(
            "session_meta",
            json!({"id":"parent-1","timestamp":"2026-09-10T08:00:00.000Z","cwd":"/parent","source":"vscode"}),
        ) + &record(
            "event_msg",
            json!({"type":"task_started","turn_id":"parent-turn-1","started_at":1_789_100_000}),
        ) + &record(
            "turn_context",
            json!({"turn_id":"parent-turn-1","cwd":"/parent","model":"parent-model"}),
        ) + &record(
            "response_item",
            json!({
                "type":"message","role":"user",
                "content":[{"type":"input_text","text":"parent prompt"}],
                "internal_chat_message_metadata_passthrough":{"content_item_kinds":["user.text"]}
            }),
        ) + &record(
            "event_msg",
            json!({"type":"user_message","message":"parent legacy prompt"}),
        ) + &record(
            "event_msg",
            json!({"type":"agent_message","message":"parent answer","phase":"final_answer"}),
        ) + &record(
            "event_msg",
            json!({"type":"token_count","info":{"total_token_usage":{"total_tokens":88_000_000}}}),
        ) + &record(
            "event_msg",
            json!({"type":"task_complete","turn_id":"parent-turn-1","started_at":1_789_100_000}),
        ) + &record(
            "event_msg",
            json!({"type":"task_started","turn_id":"parent-turn-2","started_at":1_789_200_000}),
        ) + &record(
            "turn_context",
            json!({"turn_id":"parent-turn-2","cwd":"/parent","model":"parent-model"}),
        ) + &record(
            "event_msg",
            json!({"type":"thread_settings_applied","thread_id":"child-1"}),
        );
        let child = record(
            "event_msg",
            json!({"type":"task_started","turn_id":"child-turn-1","started_at":1_789_207_200}),
        ) + &record(
            "turn_context",
            json!({"turn_id":"child-turn-1","cwd":"/work","model":"child-model"}),
        ) + &record(
            "event_msg",
            json!({"type":"agent_message","message":"child answer","phase":"final_answer"}),
        ) + &record(
            "event_msg",
            json!({"type":"token_count","info":{"total_token_usage":{"total_tokens":50}}}),
        ) + &record(
            "event_msg",
            json!({"type":"task_complete","turn_id":"child-turn-1"}),
        );
        let head = record(
            "session_meta",
            json!({
                "id":"child-1","session_id":"parent-1","forked_from_id":"parent-1",
                "parent_thread_id":"parent-1","timestamp":"2026-09-12T10:00:00.100Z",
                "cwd":"/work","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent-1","depth":1}}},
                "thread_source":"subagent"
            }),
        );
        (head + &replay, child)
    }

    #[test]
    fn forked_rollout_imports_as_the_child_without_the_parents_replayed_history() {
        let (head, child) = forked_rollout();
        let input = head + &child;
        let mut state = CodexState::default();
        let (events, consumed, _) = parse_chunk(input.as_bytes(), 0, &mut state).unwrap();

        assert_eq!(consumed, input.len());
        assert_eq!(state.native_id.as_deref(), Some("child-1"));
        assert_eq!(state.parent_id.as_deref(), Some("parent-1"));
        assert_eq!(
            events.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
            [
                "session_started",
                "turn_started",
                "response",
                "usage",
                "turn_completed"
            ]
        );
        assert_eq!(events[0].data["native_id"], "child-1");
        assert!(!events.iter().any(|e| e.data.to_string().contains("parent")));
        assert!(events.iter().all(|e| e.cwd == "/work"));
        assert_eq!(events[1].turn_id.as_deref(), Some("child-turn-1"));
        assert_eq!(events[2].model.as_deref(), Some("child-model"));
        assert_eq!(events[3].data["total"]["total_tokens"], 50);
    }

    #[test]
    fn a_fork_split_inside_the_replayed_history_resumes_without_crediting_it() {
        let (head, child) = forked_rollout();
        let mut state = CodexState::default();
        let (first, consumed, _) = parse_chunk(head.as_bytes(), 0, &mut state).unwrap();
        assert_eq!(consumed, head.len());
        assert_eq!(
            first.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
            ["session_started"]
        );
        // The checkpoint survives a round trip through its stored form.
        let mut state: CodexState =
            serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        let (events, _, _) = parse_chunk(child.as_bytes(), head.len() as u64, &mut state).unwrap();
        assert_eq!(events.len(), 4);
        assert_eq!(events[1].data, json!({"text":"child answer"}));
    }

    #[test]
    fn subagent_metadata_links_its_parent_thread() {
        let input = record(
            "session_meta",
            json!({"id":"child-2","cwd":"/work","parent_thread_id":"parent-1","thread_source":"subagent"}),
        );
        let mut state = CodexState::default();
        parse_chunk(input.as_bytes(), 0, &mut state).unwrap();
        assert_eq!(state.parent_id.as_deref(), Some("parent-1"));
    }

    #[test]
    fn checkpoints_written_before_fork_support_still_load() {
        let state: CodexState = serde_json::from_str(
            r#"{"native_id":"native-123","cwd":"/work","model":null,"turn_id":null,"parent_id":null}"#,
        )
        .unwrap();
        assert_eq!(state.native_id.as_deref(), Some("native-123"));
    }
}
