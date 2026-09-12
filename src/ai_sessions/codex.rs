//! Incremental adapter for Codex native JSONL transcripts.
use super::AiEvent;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

pub const ADAPTER_VERSION: u32 = 1;

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct CodexState {
    pub native_id: Option<String>,
    pub cwd: String,
    pub model: Option<String>,
    pub turn_id: Option<String>,
    pub parent_id: Option<String>,
}

/// Parse complete records, leaving an unfinished tail for the next import.
/// State changes are committed only when every complete record is valid.
pub fn parse_chunk(
    bytes: &[u8],
    offset: u64,
    state: &mut CodexState,
) -> Result<(Vec<AiEvent>, usize), String> {
    // Codex can embed image inputs and compaction snapshots in a single JSONL
    // record. Keep this aligned with the repository's bounded import chunk so
    // those records can be consumed without allowing unbounded allocation.
    const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
    let mut next = state.clone();
    let mut events = Vec::new();
    let mut consumed = 0;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        let complete = line.last() == Some(&b'\n');
        let content = if complete {
            &line[..line.len() - 1]
        } else {
            line
        };
        if content.len() > MAX_LINE_BYTES {
            return Err("Codex JSONL record exceeds 16 MiB".into());
        }
        if !complete {
            break;
        }
        let absolute = offset
            .checked_add(consumed as u64)
            .ok_or("Codex source offset overflow")?;
        let record: Value = serde_json::from_slice(content)
            .map_err(|error| format!("Invalid Codex JSON at byte {absolute}: {error}"))?;
        if !record.is_object() || !record["type"].is_string() {
            return Err(format!("Invalid Codex record at byte {absolute}"));
        }
        if let Some(event) = parse_record(&record, absolute, &mut next)
            .map_err(|error| format!("Codex record at byte {absolute}: {error}"))?
        {
            events.push(event);
        }
        consumed += line.len();
    }
    *state = next;
    Ok((events, consumed))
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
    if record_type != "session_meta" && state.native_id.is_none() {
        return Err("session metadata must precede events".into());
    }
    let (kind, data) = match record_type {
        "session_meta" => ("session_started", session_metadata(payload, state)?),
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

fn session_metadata(payload: &Value, state: &mut CodexState) -> Result<Value, String> {
    let id = payload
        .get("id")
        .or_else(|| payload.get("session_id"))
        .and_then(Value::as_str)
        .ok_or("missing native session ID")?;
    validate_id(id)?;
    if state
        .native_id
        .as_deref()
        .is_some_and(|current| current != id)
    {
        return Err("native session ID changed within source".into());
    }
    state.native_id = Some(id.to_owned());
    update_context(payload, state)?;
    if let Some(parent) =
        optional_string(payload, "parent_id")?.or(optional_string(payload, "forked_from_id")?)
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
        let (events, consumed) = parse_chunk(input.as_bytes(), 0, &mut state).unwrap();
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

        let (events, _) = parse_chunk(input.as_bytes(), 0, &mut initialized()).unwrap();

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

        let (events, _) = parse_chunk(input.as_bytes(), 0, &mut initialized()).unwrap();

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
        let (events, _) = parse_chunk(input.as_bytes(), 200, &mut initialized()).unwrap();
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
        let (events, _) = parse_chunk(input.as_bytes(), 432, &mut initialized()).unwrap();
        let (replay, _) = parse_chunk(input.as_bytes(), 432, &mut initialized()).unwrap();
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
        let (events, consumed) = parse_chunk(input.as_bytes(), 0, &mut state).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(consumed, first.len());
        let (events, consumed) =
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
        let (events, _) = parse_chunk(input.as_bytes(), 0, &mut initialized()).unwrap();
        assert_eq!(events.len(), 4);
        assert_eq!(events[0].kind, "turn_started");
        assert_eq!(events[1].model.as_deref(), Some("model-a"));
        assert_eq!(events[2].model.as_deref(), Some("model-b"));
        assert_eq!(events[2].turn_id.as_deref(), Some("t2"));
        assert_eq!(events[2].cwd, "/two");
        assert_eq!(events[3].kind, "turn_interrupted");
    }

    #[test]
    fn malformed_complete_lines_and_invalid_timestamps_error() {
        for input in ["{broken}\n".to_owned(), "null\n".to_owned(), "{\"type\":\"event_msg\",\"timestamp\":\"bad\",\"payload\":{\"type\":\"user_message\",\"message\":\"hi\"}}\n".to_owned()] {
            assert!(parse_chunk(input.as_bytes(), 0, &mut initialized()).is_err(), "{input}");
        }
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
        let (events, consumed) = parse_chunk(input.as_bytes(), 0, &mut state).unwrap();

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

        let (events, consumed) = parse_chunk(input.as_bytes(), 0, &mut initialized()).unwrap();

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
}
