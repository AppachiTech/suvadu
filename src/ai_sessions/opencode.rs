//! Incremental adapter for OpenCode's `GET /session/{id}/message` API
//! response (`Array<{info: Message, parts: Part[]}>`). Every call carries
//! the full session history -- the API has no cursor -- so incrementality
//! (skipping already-seen messages) is entirely handled here in Rust.
use super::AiEvent;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;

pub const ADAPTER_VERSION: u32 = 1;

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OpencodeState {
    pub seen_message_ids: HashSet<String>,
    pub next_seq: u64,
}

pub fn parse_messages(
    messages: &[Value],
    cwd: &str,
    state: &mut OpencodeState,
) -> Result<Vec<AiEvent>, String> {
    let mut events = Vec::new();
    for message in messages {
        let info = &message["info"];
        let id = required_string(info, "id")?.to_owned();
        if state.seen_message_ids.contains(&id) {
            continue;
        }
        let role = required_string(info, "role")?;
        let at = info["time"]["created"]
            .as_i64()
            .ok_or("missing info.time.created")?;
        let parts = message["parts"]
            .as_array()
            .ok_or("missing parts array")?;
        let text = joined_text(parts, &id);

        match role {
            "user" => {
                if let Some(text) = text {
                    let model = model_id(info, role);
                    events.push(next_event(
                        state,
                        "prompt",
                        "prompt",
                        at,
                        cwd,
                        Some(id.clone()),
                        model,
                        json!({"text": text}),
                    ));
                }
            }
            "assistant" => {
                let model = model_id(info, role);
                let turn_id = optional_string(info, "parentID");
                if let Some(text) = text {
                    events.push(next_event(
                        state,
                        "response",
                        "response",
                        at,
                        cwd,
                        turn_id.clone(),
                        model.clone(),
                        json!({"text": text}),
                    ));
                }
                if let Some(tokens) = info.get("tokens") {
                    let usage = usage_counters(tokens)?;
                    events.push(next_event(
                        state, "usage", "usage", at, cwd, turn_id, model,
                        json!({"total": usage}),
                    ));
                }
            }
            _ => {}
        }
        state.seen_message_ids.insert(id);
    }
    Ok(events)
}

/// Join every `text`-type part belonging to `message_id`, in array order.
/// Reasoning and tool parts are deliberately excluded.
fn joined_text(parts: &[Value], message_id: &str) -> Option<String> {
    let text = parts
        .iter()
        .filter(|part| {
            part["messageID"].as_str() == Some(message_id)
                && part["type"].as_str() == Some("text")
        })
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

/// `AssistantMessage` carries flat `modelID`/`providerID`; `UserMessage`
/// carries a nested `model: {providerID, modelID}` object. Different shapes
/// per role, per OpenCode's documented API types.
fn model_id(info: &Value, role: &str) -> Option<String> {
    if role == "assistant" {
        info["modelID"].as_str().map(str::to_owned)
    } else {
        info["model"]["modelID"].as_str().map(str::to_owned)
    }
}

fn usage_counters(tokens: &Value) -> Result<Value, String> {
    let input = counter(tokens.get("input"))?;
    let output = counter(tokens.get("output"))?;
    let cached_input = counter(tokens.get("cache").and_then(|c| c.get("read")))?;
    let cache_write_input = counter(tokens.get("cache").and_then(|c| c.get("write")))?;
    let total = input
        .checked_add(output)
        .ok_or("total token counter overflow")?;
    Ok(json!({
        "input_tokens": input,
        "cached_input_tokens": cached_input,
        "cache_write_input_tokens": cache_write_input,
        "output_tokens": output,
        "total_tokens": total
    }))
}

fn counter(value: Option<&Value>) -> Result<u64, String> {
    match value {
        None | Some(Value::Null) => Ok(0),
        Some(value) => value.as_u64().ok_or_else(|| "invalid token counter".into()),
    }
}

#[allow(clippy::too_many_arguments)]
fn next_event(
    state: &mut OpencodeState,
    suffix: &str,
    kind: &str,
    at: i64,
    cwd: &str,
    turn_id: Option<String>,
    model: Option<String>,
    data: Value,
) -> AiEvent {
    let seq = state.next_seq;
    state.next_seq += 1;
    AiEvent {
        id: format!("opencode-{seq}-{suffix}"),
        turn_id,
        kind: kind.into(),
        at,
        model,
        cwd: cwd.into(),
        data,
    }
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value[field]
        .as_str()
        .ok_or_else(|| format!("missing {field}"))
}

fn optional_string(value: &Value, field: &str) -> Option<String> {
    value[field].as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user_message(id: &str, text: &str, created: i64) -> Value {
        json!({
            "info": {
                "id": id, "sessionID": "ses_1", "role": "user",
                "time": {"created": created},
                "agent": "build", "model": {"providerID": "anthropic", "modelID": "claude-sonnet-5"}
            },
            "parts": [
                {"id": format!("{id}-p1"), "sessionID": "ses_1", "messageID": id, "type": "text", "text": text}
            ]
        })
    }

    fn assistant_message(id: &str, parent_id: &str, text: &str, created: i64) -> Value {
        json!({
            "info": {
                "id": id, "sessionID": "ses_1", "role": "assistant",
                "time": {"created": created, "completed": created + 500},
                "parentID": parent_id, "modelID": "claude-sonnet-5", "providerID": "anthropic",
                "mode": "build", "path": {"cwd": "/work", "root": "/work"}, "cost": 0.01,
                "tokens": {"input": 100, "output": 50, "reasoning": 0, "cache": {"read": 10, "write": 5}},
                "finish": "stop"
            },
            "parts": [
                {"id": format!("{id}-p1"), "sessionID": "ses_1", "messageID": id, "type": "reasoning", "text": "thinking", "time": {"start": created}},
                {"id": format!("{id}-p2"), "sessionID": "ses_1", "messageID": id, "type": "text", "text": text}
            ]
        })
    }

    #[test]
    fn extracts_prompt_and_response_text_from_matching_parts() {
        let messages = vec![
            user_message("msg_u1", "Fix the bug", 1_000),
            assistant_message("msg_a1", "msg_u1", "Fixed it.", 1_100),
        ];
        let mut state = OpencodeState::default();
        let events = parse_messages(&messages, "/work", &mut state).unwrap();

        assert_eq!(events.len(), 3); // prompt, response, usage
        assert_eq!(events[0].kind, "prompt");
        assert_eq!(events[0].data["text"], "Fix the bug");
        assert_eq!(events[0].turn_id.as_deref(), Some("msg_u1"));
        assert_eq!(events[1].kind, "response");
        assert_eq!(events[1].data["text"], "Fixed it.");
        assert_eq!(events[1].turn_id.as_deref(), Some("msg_u1")); // parentID groups the turn
        assert_eq!(events[1].model.as_deref(), Some("claude-sonnet-5"));
    }

    #[test]
    fn maps_token_usage_onto_the_shared_ai_event_usage_shape() {
        let messages = vec![assistant_message("msg_a1", "msg_u1", "Done.", 1_000)];
        let mut state = OpencodeState::default();
        let events = parse_messages(&messages, "/work", &mut state).unwrap();

        let usage = events.iter().find(|e| e.kind == "usage").unwrap();
        assert_eq!(usage.data["total"]["input_tokens"], 100);
        assert_eq!(usage.data["total"]["cached_input_tokens"], 10);
        assert_eq!(usage.data["total"]["cache_write_input_tokens"], 5);
        assert_eq!(usage.data["total"]["output_tokens"], 50);
        assert_eq!(usage.data["total"]["total_tokens"], 150);
    }

    #[test]
    fn reimporting_the_same_messages_produces_no_new_events() {
        let messages = vec![
            user_message("msg_u1", "Fix the bug", 1_000),
            assistant_message("msg_a1", "msg_u1", "Fixed it.", 1_100),
        ];
        let mut state = OpencodeState::default();
        parse_messages(&messages, "/work", &mut state).unwrap();
        let second_pass = parse_messages(&messages, "/work", &mut state).unwrap();
        assert!(second_pass.is_empty());
    }

    #[test]
    fn appending_a_new_turn_only_emits_events_for_the_new_messages() {
        let mut messages = vec![user_message("msg_u1", "Fix the bug", 1_000)];
        let mut state = OpencodeState::default();
        parse_messages(&messages, "/work", &mut state).unwrap();

        messages.push(assistant_message("msg_a1", "msg_u1", "Fixed it.", 1_100));
        let events = parse_messages(&messages, "/work", &mut state).unwrap();
        assert_eq!(events.len(), 2); // response + usage only, not the prompt again
    }

    #[test]
    fn a_message_with_no_text_part_produces_no_prompt_or_response_event() {
        let messages = vec![json!({
            "info": {"id": "msg_u1", "sessionID": "ses_1", "role": "user", "time": {"created": 1000}, "agent": "build", "model": {"providerID": "a", "modelID": "m"}},
            "parts": []
        })];
        let mut state = OpencodeState::default();
        let events = parse_messages(&messages, "/work", &mut state).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn tool_parts_and_unknown_extra_fields_are_ignored_without_error() {
        let messages = vec![json!({
            "info": {
                "id": "msg_a1", "sessionID": "ses_1", "role": "assistant",
                "time": {"created": 1000, "completed": 1500},
                "parentID": "msg_u1", "modelID": "claude-sonnet-5", "providerID": "anthropic",
                "mode": "build", "path": {"cwd": "/work", "root": "/work"}, "cost": 0.0,
                "tokens": {"input": 1, "output": 1, "reasoning": 0, "cache": {"read": 0, "write": 0}},
                "finish": "stop",
                "somethingFutureVersionsAdd": {"nested": true}
            },
            "parts": [
                {"id": "p1", "sessionID": "ses_1", "messageID": "msg_a1", "type": "tool", "callID": "c1", "tool": "bash", "state": {"status": "completed", "input": {"command": "echo hi"}, "output": "hi", "title": "bash", "metadata": {"exit": 0}}},
                {"id": "p2", "sessionID": "ses_1", "messageID": "msg_a1", "type": "text", "text": "Done."}
            ]
        })];
        let mut state = OpencodeState::default();
        let events = parse_messages(&messages, "/work", &mut state).unwrap();

        // Only response + usage; the tool part produced no event of its own,
        // and the extra unrecognized info field didn't cause an error.
        assert_eq!(events.len(), 2);
        assert!(events.iter().any(|e| e.kind == "response" && e.data["text"] == "Done."));
        assert!(events.iter().any(|e| e.kind == "usage"));
    }
}
