//! Incremental adapter for `OpenCode`'s `GET /session/{id}/message` API
//! response (`Array<{info: Message, parts: Part[]}>`). Every call carries
//! the full session history -- the API has no cursor -- so incrementality
//! (skipping already-seen messages) is entirely handled here in Rust.
use super::AiEvent;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;

/// v2: usage is now accumulated across assistant messages into running
/// totals (see `OpencodeState`'s token fields) instead of each `usage`
/// event carrying only that one message's own counters -- `OpenCode`
/// reports token usage per model response, not as a session-wide running
/// total, so
/// a multi-turn session's header/timeline previously showed only its last
/// turn's tokens. Bumped so old on-disk checkpoints (whose running totals
/// would otherwise silently start from zero) are rejected rather than
/// silently continuing with an incomplete count.
pub const ADAPTER_VERSION: u32 = 2;

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpencodeState {
    pub seen_message_ids: HashSet<String>,
    pub next_seq: u64,
    /// Running totals across every assistant message seen so far, since
    /// `OpenCode`'s own `tokens` field on each message is that message's own
    /// usage, not a session-wide cumulative count.
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_write_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
}

pub fn parse_messages(
    messages: &[Value],
    cwd: &str,
    state: &mut OpencodeState,
) -> Result<Vec<AiEvent>, String> {
    let mut next = state.clone();
    let mut events = Vec::new();
    for message in messages {
        let info = &message["info"];
        let id = required_string(info, "id")?.to_owned();
        if next.seen_message_ids.contains(&id) {
            continue;
        }
        let role = required_string(info, "role")?;
        let at = info["time"]["created"]
            .as_i64()
            .ok_or("missing info.time.created")?;
        let parts = message["parts"].as_array().ok_or("missing parts array")?;
        let text = joined_text(parts, &id);

        match role {
            "user" => {
                if let Some(text) = text {
                    let model = model_id(info, role);
                    events.push(next_event(
                        &mut next,
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
                // OpenCode's own message API leaves `time.completed` unset
                // (and `finish`/`error` null) on a message that's still
                // streaming or was interrupted mid-turn -- e.g. when
                // dispose() imports a session mid-turn on process exit.
                // Defer the whole message (don't mark it seen) until a
                // later call sees it complete, or its response/usage would
                // be captured from partial data and then never revisited,
                // since the same message id would already be in
                // seen_message_ids by the time it actually finishes.
                if info["time"]["completed"].is_null() {
                    continue;
                }
                let model = model_id(info, role);
                let turn_id = optional_string(info, "parentID");
                if let Some(text) = text {
                    events.push(next_event(
                        &mut next,
                        "response",
                        "response",
                        at,
                        cwd,
                        turn_id.clone(),
                        model.clone(),
                        json!({"text": text}),
                    ));
                }
                if let Some(tokens) = info.get("tokens").filter(|v| !v.is_null()) {
                    let last = usage_counters(tokens)?;
                    // OpenCode reports each message's own usage, not a
                    // session-wide running total (unlike Claude/Codex,
                    // whose native transcripts already report cumulative
                    // counts) -- accumulate into the session's running
                    // totals here so the emitted "total" matches what
                    // every other adapter's usage event means.
                    next.input_tokens = checked_add(next.input_tokens, last.input, "input")?;
                    next.cached_input_tokens =
                        checked_add(next.cached_input_tokens, last.cached_input, "cached input")?;
                    next.cache_write_input_tokens = checked_add(
                        next.cache_write_input_tokens,
                        last.cache_write_input,
                        "cache write input",
                    )?;
                    next.output_tokens = checked_add(next.output_tokens, last.output, "output")?;
                    next.reasoning_output_tokens = checked_add(
                        next.reasoning_output_tokens,
                        last.reasoning,
                        "reasoning output",
                    )?;
                    let usage_data = json!({
                        "last": last.to_json()?,
                        "total": folded_usage_json(
                            next.input_tokens,
                            next.cached_input_tokens,
                            next.cache_write_input_tokens,
                            next.output_tokens,
                            next.reasoning_output_tokens,
                        )?,
                    });
                    events.push(next_event(
                        &mut next, "usage", "usage", at, cwd, turn_id, model, usage_data,
                    ));
                }
            }
            _ => {}
        }
        next.seen_message_ids.insert(id);
    }
    *state = next;
    Ok(events)
}

/// Join every `text`-type part belonging to `message_id`, in array order.
/// Reasoning and tool parts are deliberately excluded. A part missing
/// `messageID` is still included (tolerated, not required) since the API's
/// response shape already scopes `parts` to their own message.
fn joined_text(parts: &[Value], message_id: &str) -> Option<String> {
    let text = parts
        .iter()
        .filter(|part| {
            part["type"].as_str() == Some("text")
                && part["messageID"].as_str().is_none_or(|id| id == message_id)
        })
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

/// `AssistantMessage` carries flat `modelID`/`providerID`; `UserMessage`
/// carries a nested `model: {providerID, modelID}` object. Different shapes
/// per role, per `OpenCode`'s documented API types.
fn model_id(info: &Value, role: &str) -> Option<String> {
    if role == "assistant" {
        info["modelID"].as_str().map(str::to_owned)
    } else {
        info["model"]["modelID"].as_str().map(str::to_owned)
    }
}

/// One message's own raw token counters, straight off the wire.
#[derive(Default)]
struct Usage {
    input: u64,
    cached_input: u64,
    cache_write_input: u64,
    output: u64,
    reasoning: u64,
}
impl Usage {
    fn to_json(&self) -> Result<Value, String> {
        folded_usage_json(
            self.input,
            self.cached_input,
            self.cache_write_input,
            self.output,
            self.reasoning,
        )
    }
}

fn usage_counters(tokens: &Value) -> Result<Usage, String> {
    Ok(Usage {
        input: counter(tokens.get("input"))?,
        output: counter(tokens.get("output"))?,
        reasoning: counter(tokens.get("reasoning"))?,
        cached_input: counter(tokens.get("cache").and_then(|c| c.get("read")))?,
        cache_write_input: counter(tokens.get("cache").and_then(|c| c.get("write")))?,
    })
}

/// Folds `cached_input`/`cache_write_input` into `input_tokens` (matching
/// how a single message's own usage was already shaped before totals were
/// tracked) while keeping each counter visible separately, for both a
/// single message's `last` usage and the session's cumulative `total`.
fn folded_usage_json(
    input: u64,
    cached_input: u64,
    cache_write_input: u64,
    output: u64,
    reasoning: u64,
) -> Result<Value, String> {
    let input_total = input
        .checked_add(cached_input)
        .and_then(|value| value.checked_add(cache_write_input))
        .ok_or("input token counter overflow")?;
    let total = input_total
        .checked_add(output)
        .ok_or("total token counter overflow")?;
    Ok(json!({
        "input_tokens": input_total,
        "cached_input_tokens": cached_input,
        "cache_write_input_tokens": cache_write_input,
        "output_tokens": output,
        "reasoning_output_tokens": reasoning,
        "total_tokens": total
    }))
}

fn counter(value: Option<&Value>) -> Result<u64, String> {
    match value {
        None | Some(Value::Null) => Ok(0),
        Some(value) => value.as_u64().ok_or_else(|| "invalid token counter".into()),
    }
}

fn checked_add(a: u64, b: u64, label: &str) -> Result<u64, String> {
    a.checked_add(b)
        .ok_or_else(|| format!("{label} token counter overflow"))
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
        assert_eq!(events[0].model.as_deref(), Some("claude-sonnet-5"));
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
        assert_eq!(usage.data["total"]["input_tokens"], 115);
        assert_eq!(usage.data["total"]["cached_input_tokens"], 10);
        assert_eq!(usage.data["total"]["cache_write_input_tokens"], 5);
        assert_eq!(usage.data["total"]["output_tokens"], 50);
        assert_eq!(usage.data["total"]["reasoning_output_tokens"], 0);
        assert_eq!(usage.data["total"]["total_tokens"], 165);
    }

    #[test]
    fn two_turn_session_accumulates_usage_into_a_running_total() {
        // OpenCode's `tokens` field is that one model response's own usage,
        // not a session-wide running total, so a second turn's usage event
        // must add onto the first turn's counts rather than replace them --
        // otherwise anything reading only the latest usage event (the
        // session header, the TUI timeline) would report just the last
        // turn's tokens for the whole session.
        let messages = vec![
            user_message("msg_u1", "Fix the bug", 1_000),
            assistant_message("msg_a1", "msg_u1", "Fixed it.", 1_100),
            user_message("msg_u2", "Now add a test", 1_200),
            assistant_message("msg_a2", "msg_u2", "Added.", 1_300),
        ];
        let mut state = OpencodeState::default();
        let events = parse_messages(&messages, "/work", &mut state).unwrap();

        let usage_events: Vec<_> = events.iter().filter(|e| e.kind == "usage").collect();
        assert_eq!(usage_events.len(), 2);

        // Each message's own delta is 115 input (100 + 10 cache-read + 5
        // cache-write) / 50 output / 165 total (see the single-turn test).
        assert_eq!(usage_events[0].data["last"]["total_tokens"], 165);
        assert_eq!(usage_events[0].data["total"]["total_tokens"], 165);

        assert_eq!(usage_events[1].data["last"]["total_tokens"], 165);
        assert_eq!(usage_events[1].data["total"]["input_tokens"], 230);
        assert_eq!(usage_events[1].data["total"]["output_tokens"], 100);
        assert_eq!(usage_events[1].data["total"]["cached_input_tokens"], 20);
        assert_eq!(
            usage_events[1].data["total"]["cache_write_input_tokens"],
            10
        );
        assert_eq!(usage_events[1].data["total"]["total_tokens"], 330);

        // The running total must also survive across separate incremental
        // import calls (a real session is imported repeatedly on every
        // session.idle, not all at once).
        let mut state2 = OpencodeState::default();
        let first_call = parse_messages(&messages[..2], "/work", &mut state2).unwrap();
        assert_eq!(
            first_call.iter().find(|e| e.kind == "usage").unwrap().data["total"]["total_tokens"],
            165
        );
        let second_call = parse_messages(&messages, "/work", &mut state2).unwrap();
        assert_eq!(second_call.len(), 3); // prompt, response, usage for msg_u2/msg_a2 only
        assert_eq!(
            second_call.iter().find(|e| e.kind == "usage").unwrap().data["total"]["total_tokens"],
            330
        );
    }

    #[test]
    fn an_assistant_message_still_streaming_is_deferred_until_it_completes() {
        // dispose() (or any import racing a live turn) can see an assistant
        // message with no `time.completed` yet -- interrupted mid-stream,
        // per OpenCode's own handling of a killed process. If this were
        // marked seen from partial data, the same message id would be
        // silently skipped forever once it actually finished.
        let incomplete = json!({
            "info": {
                "id": "msg_a1", "sessionID": "ses_1", "role": "assistant",
                "time": {"created": 1_100},
                "parentID": "msg_u1", "modelID": "claude-sonnet-5", "providerID": "anthropic",
                "mode": "build", "path": {"cwd": "/work", "root": "/work"}, "cost": 0.0,
                "finish": null
            },
            "parts": [
                {"id": "msg_a1-p1", "sessionID": "ses_1", "messageID": "msg_a1", "type": "text", "text": "Partial resp"}
            ]
        });
        let mut state = OpencodeState::default();

        let mid_stream = parse_messages(
            &[user_message("msg_u1", "Fix the bug", 1_000), incomplete],
            "/work",
            &mut state,
        )
        .unwrap();
        // Only the prompt is captured; the incomplete assistant message
        // produces nothing yet and must not be marked seen.
        assert_eq!(mid_stream.len(), 1);
        assert_eq!(mid_stream[0].kind, "prompt");
        assert!(!state.seen_message_ids.contains("msg_a1"));

        // The turn later finishes -- OpenCode reuses the same message id,
        // now with `time.completed`, real text, and token usage.
        let completed = vec![
            user_message("msg_u1", "Fix the bug", 1_000),
            assistant_message("msg_a1", "msg_u1", "Fixed it.", 1_100),
        ];
        let resumed = parse_messages(&completed, "/work", &mut state).unwrap();
        assert_eq!(resumed.len(), 2); // response + usage, not re-emitting the prompt
        assert!(resumed
            .iter()
            .any(|e| e.kind == "response" && e.data["text"] == "Fixed it."));
        assert!(resumed.iter().any(|e| e.kind == "usage"));
        assert!(state.seen_message_ids.contains("msg_a1"));
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
    fn joined_text_tolerates_parts_missing_message_id() {
        let messages = vec![json!({
            "info": {
                "id": "msg_a1", "sessionID": "ses_1", "role": "assistant",
                "time": {"created": 1000, "completed": 1500},
                "parentID": "msg_u1", "modelID": "m", "providerID": "p",
                "mode": "build", "path": {"cwd": "/work", "root": "/work"}, "cost": 0.0,
                "tokens": {"input": 1, "output": 1, "reasoning": 0, "cache": {"read": 0, "write": 0}},
                "finish": "stop"
            },
            "parts": [{"id": "p1", "sessionID": "ses_1", "type": "text", "text": "Done."}]
        })];
        let mut state = OpencodeState::default();
        let events = parse_messages(&messages, "/work", &mut state).unwrap();
        let response = events.iter().find(|e| e.kind == "response").unwrap();
        assert_eq!(response.data["text"], "Done.");
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
        assert!(events
            .iter()
            .any(|e| e.kind == "response" && e.data["text"] == "Done."));
        assert!(events.iter().any(|e| e.kind == "usage"));
    }

    #[test]
    fn state_is_unchanged_on_parse_error() {
        let messages = vec![
            user_message("msg_u1", "Fix the bug", 1_000),
            json!({
                "info": {
                    "id": "msg_bad", "sessionID": "ses_1",
                    "time": {"created": 1_100}
                    // missing "role" field
                },
                "parts": []
            }),
        ];
        let mut state = OpencodeState::default();
        let state_before = state.clone();

        let result = parse_messages(&messages, "/work", &mut state);
        assert!(result.is_err());
        assert_eq!(state, state_before);
    }
}
