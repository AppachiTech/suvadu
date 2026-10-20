//! Adapter for Cursor's agent hooks. Cursor pushes one event per hook run
//! -- `beforeSubmitPrompt`, `afterAgentResponse`, `stop` -- instead of
//! leaving a transcript to read, so there is no byte offset to checkpoint:
//! each delivery becomes at most one event, and a delivery already stored
//! is recognised by its fingerprint and stored once.
//!
//! Cursor's hooks carry no token counts, so this adapter never emits a
//! `usage` event and its sessions say usage is unavailable rather than
//! zero.
use super::AiEvent;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

pub const ADAPTER_VERSION: u32 = 1;

/// The `ai_sessions.agent` value of a captured Cursor conversation.
pub const AGENT: &str = "cursor";

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CursorState {
    /// Fingerprints of the deliveries already stored, so a hook that runs
    /// twice for the same event stores it once.
    pub seen: HashSet<String>,
    /// The next event sequence number; also the end of the source range a
    /// capture gap covers.
    pub next_seq: u64,
}

/// The event one Cursor hook payload describes, with a placeholder id
/// until [`assign_id`] numbers it. `None` for a hook this adapter does not
/// read, or one that carries nothing to store (an empty prompt or
/// response, a `stop` without a status).
pub fn event_from_payload(payload: &Value, cwd: &str, at: i64) -> Option<AiEvent> {
    let (kind, data) = match payload["hook_event_name"].as_str()? {
        "beforeSubmitPrompt" => ("prompt", json!({"text": non_empty(&payload["prompt"])?})),
        "afterAgentResponse" => ("response", json!({"text": non_empty(&payload["text"])?})),
        "stop" => match payload["status"].as_str()? {
            "completed" => ("turn_completed", json!({})),
            status if !status.is_empty() => ("turn_interrupted", json!({"status": status})),
            _ => return None,
        },
        _ => return None,
    };
    let turn_id = payload["generation_id"]
        .as_str()
        .filter(|id| crate::util::is_valid_session_id(id))
        .map(str::to_owned);
    let model = ["model_id", "model"]
        .iter()
        .find_map(|field| payload[*field].as_str().filter(|model| !model.is_empty()))
        .map(str::to_owned);
    Some(AiEvent {
        id: String::new(),
        turn_id,
        kind: kind.into(),
        at,
        model,
        cwd: cwd.into(),
        data,
    })
}

fn non_empty(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| !text.trim().is_empty())
}

/// What makes two deliveries the same event: kind, turn and the stored
/// (already sanitized) data. Taken after redaction, so the checkpoint never
/// holds a digest of text the database does not also hold.
pub fn fingerprint(event: &AiEvent) -> String {
    let mut hash = Sha256::new();
    for part in [
        event.kind.as_bytes(),
        event.turn_id.as_deref().unwrap_or_default().as_bytes(),
        event.data.to_string().as_bytes(),
    ] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    format!("{:x}", hash.finalize())[..16].to_owned()
}

/// Number `event` with the next sequence number and return that number.
pub fn assign_id(state: &mut CursorState, event: &mut AiEvent) -> u64 {
    let seq = state.next_seq;
    state.next_seq += 1;
    event.id = format!("cursor-{seq}-{}", event.kind);
    seq
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(event: &str, extra: &Value) -> Value {
        let mut value = json!({
            "hook_event_name": event,
            "conversation_id": "conv-1",
            "generation_id": "gen-1",
            "model": "claude-sonnet-5",
            "workspace_roots": ["/work"]
        });
        for (key, field) in extra.as_object().unwrap() {
            value[key] = field.clone();
        }
        value
    }

    #[test]
    fn a_prompt_becomes_a_prompt_event_in_its_generation() {
        let event = event_from_payload(
            &payload("beforeSubmitPrompt", &json!({"prompt": "Fix the bug"})),
            "/work",
            1_000,
        )
        .unwrap();
        assert_eq!(event.kind, "prompt");
        assert_eq!(event.data["text"], "Fix the bug");
        assert_eq!(event.turn_id.as_deref(), Some("gen-1"));
        assert_eq!(event.model.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(event.cwd, "/work");
        assert_eq!(event.at, 1_000);
    }

    #[test]
    fn a_response_becomes_a_response_event_and_prefers_model_id() {
        let event = event_from_payload(
            &payload(
                "afterAgentResponse",
                &json!({"text": "Fixed it.", "model_id": "claude-sonnet-5-thinking"}),
            ),
            "/work",
            2_000,
        )
        .unwrap();
        assert_eq!(event.kind, "response");
        assert_eq!(event.data["text"], "Fixed it.");
        assert_eq!(event.model.as_deref(), Some("claude-sonnet-5-thinking"));
    }

    #[test]
    fn stop_records_how_the_turn_ended() {
        let completed =
            event_from_payload(&payload("stop", &json!({"status": "completed"})), "/w", 1).unwrap();
        assert_eq!(completed.kind, "turn_completed");
        let aborted =
            event_from_payload(&payload("stop", &json!({"status": "aborted"})), "/w", 1).unwrap();
        assert_eq!(aborted.kind, "turn_interrupted");
        assert_eq!(aborted.data["status"], "aborted");
        assert!(event_from_payload(&payload("stop", &json!({})), "/w", 1).is_none());
    }

    #[test]
    fn missing_or_empty_fields_produce_no_event_and_no_invented_values() {
        assert!(event_from_payload(&payload("beforeSubmitPrompt", &json!({})), "/w", 1).is_none());
        assert!(event_from_payload(
            &payload("afterAgentResponse", &json!({"text": "  "})),
            "/w",
            1
        )
        .is_none());
        assert!(event_from_payload(
            &payload("afterAgentThought", &json!({"text": "hmm"})),
            "/w",
            1
        )
        .is_none());
        assert!(event_from_payload(&json!({"prompt": "no event name"}), "/w", 1).is_none());

        let bare = event_from_payload(
            &json!({"hook_event_name": "afterAgentResponse", "text": "ok", "generation_id": "../x"}),
            "/w",
            1,
        )
        .unwrap();
        assert_eq!(bare.turn_id, None, "an unsafe generation id is dropped");
        assert_eq!(bare.model, None);
    }

    #[test]
    fn fingerprints_separate_turns_and_kinds_but_match_a_redelivery() {
        let prompt = |generation: &str, text: &str| {
            event_from_payload(
                &payload(
                    "beforeSubmitPrompt",
                    &json!({"prompt": text, "generation_id": generation}),
                ),
                "/w",
                1,
            )
            .unwrap()
        };
        let mut later = prompt("gen-1", "continue");
        later.at = 99;
        assert_eq!(
            fingerprint(&prompt("gen-1", "continue")),
            fingerprint(&later)
        );
        assert_ne!(
            fingerprint(&prompt("gen-1", "continue")),
            fingerprint(&prompt("gen-2", "continue"))
        );
        assert_ne!(
            fingerprint(&prompt("gen-1", "continue")),
            fingerprint(&prompt("gen-1", "stop"))
        );
    }

    #[test]
    fn ids_are_numbered_in_delivery_order() {
        let mut state = CursorState::default();
        let mut event =
            event_from_payload(&payload("stop", &json!({"status": "completed"})), "/w", 1).unwrap();
        assert_eq!(assign_id(&mut state, &mut event), 0);
        assert_eq!(event.id, "cursor-0-turn_completed");
        assert_eq!(assign_id(&mut state, &mut event), 1);
        assert_eq!(state.next_seq, 2);
    }
}
