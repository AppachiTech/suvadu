//! Incremental adapter for Claude Code native JSONL transcripts.
use super::AiEvent;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const ADAPTER_VERSION: u32 = 1;

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClaudeState {
    pub native_id: Option<String>,
    pub cwd: String,
    pub model: Option<String>,
    pub turn_id: Option<String>,
    pub session_started: bool,
    pub last_usage_message_id: Option<String>,
    pub last_response_block_id: Option<String>,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_write_input_tokens: u64,
    pub output_tokens: u64,
}

/// Parse complete records, leaving an unfinished tail for the next import.
/// State changes are committed only when every complete record is valid;
/// malformed lines are skipped as [`super::for_each_jsonl_record`] describes.
pub fn parse_chunk(
    bytes: &[u8],
    offset: u64,
    state: &mut ClaudeState,
) -> Result<super::ParsedChunk, String> {
    let mut next = state.clone();
    let mut events = Vec::new();
    let (consumed, malformed) =
        super::for_each_jsonl_record(bytes, offset, "Claude", |record, absolute| {
            parse_record(record, absolute, &mut next, &mut events)
        })?;
    *state = next;
    Ok((events, consumed, malformed))
}

fn parse_record(
    record: &Value,
    offset: u64,
    state: &mut ClaudeState,
    events: &mut Vec<AiEvent>,
) -> Result<(), String> {
    let record_type = record["type"].as_str().unwrap_or_default();
    if !matches!(record_type, "user" | "assistant") {
        return Ok(());
    }
    let native_id = required_string(record, "sessionId")?;
    validate_id(native_id)?;
    if state
        .native_id
        .as_deref()
        .is_some_and(|current| current != native_id)
    {
        return Err("native session ID changed within source".into());
    }
    state.native_id = Some(native_id.to_owned());
    if let Some(cwd) = optional_string(record, "cwd")? {
        state.cwd = cwd;
    }
    let at = timestamp(record)?;
    if !state.session_started {
        state.session_started = true;
        let mut data = json!({"native_id":native_id});
        for field in ["entrypoint", "version"] {
            if let Some(value) = record[field].as_str() {
                data[field] = json!(value);
            }
        }
        events.push(event(offset, "session", "session_started", at, state, data));
    }

    match record_type {
        "user" => parse_user(record, offset, at, state, events),
        "assistant" => parse_assistant(record, offset, at, state, events),
        _ => unreachable!(),
    }
}

fn parse_user(
    record: &Value,
    offset: u64,
    at: i64,
    state: &mut ClaudeState,
    events: &mut Vec<AiEvent>,
) -> Result<(), String> {
    if record.get("toolUseResult").is_some()
        || record.get("sourceToolAssistantUUID").is_some()
        || record["isMeta"].as_bool() == Some(true)
        || record["promptSource"].as_str() == Some("system")
        || record["origin"]["kind"].as_str() == Some("task-notification")
    {
        return Ok(());
    }
    let message = record["message"]
        .as_object()
        .ok_or("invalid user message")?;
    if message.get("role").and_then(Value::as_str) != Some("user") {
        return Ok(());
    }
    let Some(text) = text_content(message.get("content"), true)? else {
        return Ok(());
    };
    state.turn_id = optional_string(record, "promptId")?.or(optional_string(record, "uuid")?);
    events.push(event(
        offset,
        "prompt",
        "prompt",
        at,
        state,
        json!({"text":text}),
    ));
    Ok(())
}

fn parse_assistant(
    record: &Value,
    offset: u64,
    at: i64,
    state: &mut ClaudeState,
    events: &mut Vec<AiEvent>,
) -> Result<(), String> {
    let message = record["message"]
        .as_object()
        .ok_or("invalid assistant message")?;
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return Ok(());
    }
    let synthetic = message.get("model").and_then(Value::as_str) == Some("<synthetic>");
    if let Some(model) = message
        .get("model")
        .and_then(Value::as_str)
        .filter(|_| !synthetic)
    {
        state.model = Some(model.to_owned());
    }
    let message_id = message
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| record["requestId"].as_str());
    if let (Some(id), Some(usage)) = (message_id, message.get("usage").filter(|_| !synthetic)) {
        if state.last_usage_message_id.as_deref() != Some(id) {
            let last = usage_counters(usage)?;
            state.input_tokens = checked_add(state.input_tokens, last.input, "input")?;
            state.cached_input_tokens =
                checked_add(state.cached_input_tokens, last.cached_input, "cached input")?;
            state.cache_write_input_tokens = checked_add(
                state.cache_write_input_tokens,
                last.cache_write_input,
                "cache write input",
            )?;
            state.output_tokens = checked_add(state.output_tokens, last.output, "output")?;
            state.last_usage_message_id = Some(id.to_owned());
            let total_tokens = checked_add(state.input_tokens, state.output_tokens, "total")?;
            events.push(event(
                offset,
                "usage",
                "usage",
                at,
                state,
                json!({
                    "last": last.to_json()?,
                    "total": {
                        "input_tokens":state.input_tokens,
                        "cached_input_tokens":state.cached_input_tokens,
                        "cache_write_input_tokens":state.cache_write_input_tokens,
                        "output_tokens":state.output_tokens,
                        "total_tokens":total_tokens
                    }
                }),
            ));
        }
    }
    if let Some(text) = text_content(message.get("content"), false)? {
        let block_id = message_id
            .map(|id| {
                format!(
                    "{id}:{}",
                    record["apiBlockIndex"]
                        .as_u64()
                        .map_or_else(|| "single".into(), |index| index.to_string())
                )
            })
            .or_else(|| record["uuid"].as_str().map(str::to_owned));
        if block_id.as_deref() != state.last_response_block_id.as_deref() {
            state.last_response_block_id = block_id;
            events.push(event(
                offset,
                "response",
                "response",
                at,
                state,
                json!({"text":text}),
            ));
        }
    }
    Ok(())
}

#[derive(Default)]
struct Usage {
    input: u64,
    cached_input: u64,
    cache_write_input: u64,
    output: u64,
}
impl Usage {
    fn to_json(&self) -> Result<Value, String> {
        Ok(json!({
            "input_tokens":self.input,
            "cached_input_tokens":self.cached_input,
            "cache_write_input_tokens":self.cache_write_input,
            "output_tokens":self.output,
            "total_tokens":checked_add(self.input, self.output, "total")?
        }))
    }
}

fn usage_counters(value: &Value) -> Result<Usage, String> {
    let object = value.as_object().ok_or("invalid token usage")?;
    let raw_input = counter(object.get("input_tokens"), "input_tokens")?;
    let cached_input = counter(
        object.get("cache_read_input_tokens"),
        "cache_read_input_tokens",
    )?;
    let cache_write_input = counter(
        object.get("cache_creation_input_tokens"),
        "cache_creation_input_tokens",
    )?;
    let input = raw_input
        .checked_add(cached_input)
        .and_then(|value| value.checked_add(cache_write_input))
        .ok_or("input token counter overflow")?;
    Ok(Usage {
        input,
        cached_input,
        cache_write_input,
        output: counter(object.get("output_tokens"), "output_tokens")?,
    })
}

fn counter(value: Option<&Value>, name: &str) -> Result<u64, String> {
    match value {
        None | Some(Value::Null) => Ok(0),
        Some(value) => value
            .as_u64()
            .ok_or_else(|| format!("invalid token counter {name}")),
    }
}

fn checked_add(current: u64, value: u64, name: &str) -> Result<u64, String> {
    current
        .checked_add(value)
        .ok_or_else(|| format!("{name} token counter overflow"))
}

fn text_content(value: Option<&Value>, allow_string: bool) -> Result<Option<String>, String> {
    let Some(value) = value else { return Ok(None) };
    let text = if allow_string {
        if let Some(text) = value.as_str() {
            text.to_owned()
        } else {
            text_blocks(value)?
        }
    } else {
        text_blocks(value)?
    };
    Ok((!text.is_empty()).then_some(text))
}

fn text_blocks(value: &Value) -> Result<String, String> {
    let items = value.as_array().ok_or("invalid message content")?;
    Ok(items
        .iter()
        .filter(|item| item["type"].as_str() == Some("text"))
        .filter_map(|item| item["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n"))
}

fn event(
    offset: u64,
    suffix: &str,
    kind: &str,
    at: i64,
    state: &ClaudeState,
    data: Value,
) -> AiEvent {
    AiEvent {
        id: format!("claude-{offset}-{suffix}"),
        turn_id: state.turn_id.clone(),
        kind: kind.into(),
        at,
        model: state.model.clone(),
        cwd: state.cwd.clone(),
        data,
    }
}

fn timestamp(record: &Value) -> Result<i64, String> {
    let timestamp = required_string(record, "timestamp")?;
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|value| value.timestamp_millis())
        .map_err(|_| "invalid timestamp".into())
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.len() > 128 || !crate::util::is_valid_session_id(id) {
        return Err("invalid native session ID".into());
    }
    Ok(())
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value[field]
        .as_str()
        .ok_or_else(|| format!("missing {field}"))
}

fn optional_string(value: &Value, field: &str) -> Result<Option<String>, String> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!("invalid {field}: expected string")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(text: &str) -> String {
        format!(
            "{}\n",
            json!({
                "type":"user","sessionId":"native-1","cwd":"/work",
                "timestamp":"2026-09-12T10:00:00Z","uuid":"u1",
                "message":{"role":"user","content":text}
            })
        )
    }

    #[test]
    fn a_complete_transcript_imports_its_prompts() {
        let input = prompt("one") + &prompt("two");
        let mut state = ClaudeState::default();
        let (events, consumed, malformed) = parse_chunk(input.as_bytes(), 0, &mut state).unwrap();
        assert_eq!(consumed, input.len());
        assert_eq!(malformed, 0);
        assert_eq!(
            events.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
            ["session_started", "prompt", "prompt"]
        );
    }

    #[test]
    fn an_incomplete_final_line_waits_for_the_writer() {
        let first = prompt("one");
        let second = prompt("two");
        let input = first.clone() + &second[..second.len() - 5];
        let (events, consumed, malformed) =
            parse_chunk(input.as_bytes(), 0, &mut ClaudeState::default()).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(consumed, first.len());
        assert_eq!(malformed, 0);
    }

    #[test]
    fn a_torn_line_followed_by_complete_records_is_skipped_and_counted() {
        let one = prompt("one");
        let torn = format!("{}{}", &one[..30], prompt("two"));
        let input = one.clone() + &torn + &prompt("three");
        let mut state = ClaudeState::default();
        let (events, consumed, malformed) = parse_chunk(input.as_bytes(), 0, &mut state).unwrap();
        assert_eq!(consumed, input.len());
        assert_eq!(malformed, 1);
        let texts = events
            .iter()
            .filter_map(|e| e.data["text"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(texts, ["one", "three"]);
        assert_eq!(
            events.last().unwrap().id,
            format!("claude-{}-prompt", one.len() + torn.len())
        );
    }

    #[test]
    fn a_malformed_final_line_waits_for_a_later_record() {
        let one = prompt("one");
        let input = one.clone() + "{\"type\":\"user\",\"sessionId\n";
        let (events, consumed, malformed) =
            parse_chunk(input.as_bytes(), 0, &mut ClaudeState::default()).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(consumed, one.len());
        assert_eq!(malformed, 0);
    }

    #[test]
    fn invalid_records_that_parse_as_json_still_error() {
        let input = prompt("one").replace("2026-09-12T10:00:00Z", "yesterday");
        assert!(parse_chunk(input.as_bytes(), 0, &mut ClaudeState::default()).is_err());
    }
}
