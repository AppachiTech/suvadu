//! Bounded, selective reading of agent hook payloads.
//!
//! A hook payload carries the few fields Suvadu records -- the command,
//! its directory, the session -- next to whatever the tool produced, and a
//! `tool_response` can be megabytes of build output. Each hook names the
//! fields it uses and only those are kept; everything else is skipped as
//! it is parsed, so a large output costs a read but no copy.
//!
//! Input is read up to [`MAX_HOOK_PAYLOAD_BYTES`]. Beyond that the rest is
//! drained (so the agent never sees a broken pipe) and the fields that
//! were complete before the cut are still returned, marked truncated: the
//! agents send the command ahead of the tool's output, so it survives.
use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use std::fmt;
use std::io::Read;

/// Hard cap on the hook payload kept in memory.
pub const MAX_HOOK_PAYLOAD_BYTES: u64 = 16 * 1024 * 1024;

/// A field a hook uses. `inner` empty keeps the whole value; otherwise only
/// the named sub-fields of an object are kept, and a value of any other
/// type becomes null. A `tail` field keeps only the end of a string.
#[derive(Clone, Copy)]
pub struct Field {
    pub name: &'static str,
    pub inner: &'static [Self],
    pub tail: bool,
}

/// A field kept whole.
pub const fn whole(name: &'static str) -> Field {
    Field {
        name,
        inner: &[],
        tail: false,
    }
}

/// An object field of which only `inner` is kept.
pub const fn only(name: &'static str, inner: &'static [Field]) -> Field {
    Field {
        name,
        inner,
        tail: false,
    }
}

/// A string field of which only the last
/// [`crate::failure_output::WINDOW_BYTES`] are kept — a tool's output,
/// which can be megabytes. Read it with [`tail_text`]; a value of any other
/// type becomes null.
pub const fn tail(name: &'static str) -> Field {
    Field {
        name,
        inner: &[],
        tail: true,
    }
}

/// A [`tail`] field's text and how many bytes before it were dropped, or
/// `None` when the field was absent or not a string.
pub fn tail_text(value: &Value) -> Option<(&str, u64)> {
    match value {
        Value::String(text) => Some((text, 0)),
        Value::Object(cut) => Some((
            cut.get("tail")?.as_str()?,
            cut.get("dropped_bytes")?.as_u64()?,
        )),
        _ => None,
    }
}

/// A tool's output as one [`tail`] field held it, labelled `stream`.
/// `None` when the field was absent, not a string, or empty.
pub fn captured_stream(
    stream: &'static str,
    value: &Value,
) -> Option<crate::failure_output::Captured> {
    let (text, dropped) = tail_text(value).filter(|(text, _)| !text.trim().is_empty())?;
    Some(crate::failure_output::Captured::from_window(
        stream, text, dropped,
    ))
}

/// A tool's output from its separate stdout and stderr [`tail`] fields:
/// the one that has text, or both, stdout first (`stdout+stderr`). How
/// they interleaved is not reported, so it is not reconstructed.
pub fn captured_streams(stdout: &Value, stderr: &Value) -> Option<crate::failure_output::Captured> {
    match (
        captured_stream("stdout", stdout),
        captured_stream("stderr", stderr),
    ) {
        (Some(out), Some(err)) => {
            let text = format!("{}\n{}", out.text.trim_end_matches('\n'), err.text);
            Some(crate::failure_output::Captured::from_window(
                "stdout+stderr",
                &text,
                out.dropped_bytes + err.dropped_bytes,
            ))
        }
        (one, None) | (None, one) => one,
    }
}

/// The picked fields of a hook payload.
pub struct HookPayload {
    /// An object holding only the picked fields that were present, or
    /// `Value::Null` for an empty or non-object payload.
    pub event: Value,
    /// The payload exceeded [`MAX_HOOK_PAYLOAD_BYTES`]; `event` holds only
    /// the fields that were complete before the cut.
    pub truncated: bool,
}

impl HookPayload {
    /// Fail as an oversized payload when it was cut and `present` says
    /// the fields this hook needs did not survive the cut.
    pub fn require(&self, present: bool) -> Result<(), String> {
        if self.truncated && !present {
            return Err(format!(
                "Hook input exceeds {} MB",
                MAX_HOOK_PAYLOAD_BYTES / (1024 * 1024)
            ));
        }
        Ok(())
    }
}

/// Read a hook payload from `reader`, keeping only `fields`.
///
/// A payload within the cap must be valid JSON, as before. An empty
/// payload yields `None`.
pub fn read(reader: impl Read, fields: &'static [Field]) -> std::io::Result<Option<HookPayload>> {
    let mut reader = reader;
    let mut bytes = Vec::new();
    (&mut reader)
        .take(MAX_HOOK_PAYLOAD_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.is_empty() {
        return Ok(None);
    }
    let truncated = bytes.len() as u64 > MAX_HOOK_PAYLOAD_BYTES;
    if truncated {
        std::io::copy(&mut reader, &mut std::io::sink())?;
        bytes.truncate(usize::try_from(MAX_HOOK_PAYLOAD_BYTES).unwrap_or(usize::MAX));
    }
    let event = pick(&bytes, fields, truncated).map_err(std::io::Error::other)?;
    Ok(Some(HookPayload { event, truncated }))
}

/// Parse `bytes` keeping only `fields`. With `partial`, the input is a cut
/// prefix: parsing stops at the cut and the fields completed before it are
/// returned.
fn pick(bytes: &[u8], fields: &'static [Field], partial: bool) -> serde_json::Result<Value> {
    let mut out = Map::new();
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let result = (&mut deserializer).deserialize_any(TopLevel {
        fields,
        out: &mut out,
    });
    match result {
        Ok(is_object) => {
            deserializer.end()?;
            Ok(if is_object {
                Value::Object(out)
            } else {
                Value::Null
            })
        }
        Err(_) if partial => Ok(Value::Object(out)),
        Err(error) => Err(error),
    }
}

/// Collects picked top-level fields into `out` as they complete, so a
/// payload cut part-way still yields the fields before the cut.
struct TopLevel<'a> {
    fields: &'static [Field],
    out: &'a mut Map<String, Value>,
}

impl<'de> Visitor<'de> for TopLevel<'_> {
    /// Whether the payload was an object.
    type Value = bool;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a hook payload")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<bool, A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            if let Some(field) = self.fields.iter().find(|field| field.name == key) {
                let value = map.next_value_seed(Pick::of(field))?;
                self.out.insert(key, value);
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(true)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<bool, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(false)
    }

    fn visit_bool<E>(self, _: bool) -> Result<bool, E> {
        Ok(false)
    }
    fn visit_i64<E>(self, _: i64) -> Result<bool, E> {
        Ok(false)
    }
    fn visit_u64<E>(self, _: u64) -> Result<bool, E> {
        Ok(false)
    }
    fn visit_f64<E>(self, _: f64) -> Result<bool, E> {
        Ok(false)
    }
    fn visit_str<E>(self, _: &str) -> Result<bool, E> {
        Ok(false)
    }
    fn visit_unit<E>(self) -> Result<bool, E> {
        Ok(false)
    }
}

/// Deserializes one picked field's value.
struct Pick(&'static [Field]);

impl Pick {
    /// How `field`'s value is read: its tail, whole, or its sub-fields.
    const fn of(field: &Field) -> PickValue {
        if field.tail {
            PickValue::Tail
        } else {
            PickValue::Fields(Self(field.inner))
        }
    }
}

/// [`Pick`] for a field that may be a [`tail`].
enum PickValue {
    Fields(Pick),
    Tail,
}

impl<'de> DeserializeSeed<'de> for PickValue {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        match self {
            Self::Fields(pick) => pick.deserialize(deserializer),
            Self::Tail => deserializer.deserialize_any(TailVisitor),
        }
    }
}

impl<'de> DeserializeSeed<'de> for Pick {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        if self.0.is_empty() {
            return serde::Deserialize::deserialize(deserializer);
        }
        deserializer.deserialize_any(self)
    }
}

/// Keeps the end of a string value; anything else becomes null.
struct TailVisitor;

impl TailVisitor {
    fn keep(text: &str) -> Value {
        let mut start = text
            .len()
            .saturating_sub(crate::failure_output::WINDOW_BYTES);
        while !text.is_char_boundary(start) {
            start += 1;
        }
        if start == 0 {
            return Value::String(text.to_string());
        }
        let mut cut = Map::new();
        cut.insert("tail".into(), Value::String(text[start..].to_string()));
        cut.insert("dropped_bytes".into(), Value::from(start as u64));
        Value::Object(cut)
    }
}

impl<'de> Visitor<'de> for TailVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_str<E>(self, text: &str) -> Result<Value, E> {
        Ok(Self::keep(text))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Value::Null)
    }

    fn visit_bool<E>(self, _: bool) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_i64<E>(self, _: i64) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_u64<E>(self, _: u64) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_f64<E>(self, _: f64) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
}

impl<'de> Visitor<'de> for Pick {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut out = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if let Some(field) = self.0.iter().find(|field| field.name == key) {
                let value = map.next_value_seed(Self::of(field))?;
                out.insert(key, value);
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(Value::Object(out))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Value::Null)
    }

    fn visit_bool<E>(self, _: bool) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_i64<E>(self, _: i64) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_u64<E>(self, _: u64) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_f64<E>(self, _: f64) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_str<E>(self, _: &str) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const FIELDS: &[Field] = &[
        whole("session_id"),
        only("tool_input", &[whole("command")]),
        only("tool_response", &[whole("exit_code")]),
    ];

    fn read_bytes(bytes: &[u8]) -> HookPayload {
        read(bytes, FIELDS).unwrap().unwrap()
    }

    #[test]
    fn keeps_only_the_named_fields() {
        let payload = json!({
            "session_id":"s1", "transcript_path":"/t",
            "tool_input":{"command":"ls","description":"list"},
            "tool_response":{"stdout":"x".repeat(5 * 1024 * 1024),"exit_code":3}
        })
        .to_string();
        let payload = read_bytes(payload.as_bytes());
        assert!(!payload.truncated);
        assert_eq!(
            payload.event,
            json!({"session_id":"s1","tool_input":{"command":"ls"},"tool_response":{"exit_code":3}})
        );
    }

    #[test]
    fn a_picked_object_field_of_another_type_becomes_null() {
        let payload = read_bytes(br#"{"tool_response":"plain text output","tool_input":[1,2]}"#);
        assert_eq!(
            payload.event,
            json!({"tool_response":null,"tool_input":null})
        );
    }

    #[test]
    fn invalid_or_trailing_input_within_the_cap_still_errors() {
        for input in [&b"{\"session_id\":"[..], b"{} {}", b"{\"a\":}"] {
            assert!(read(input, FIELDS).is_err(), "{input:?}");
        }
        assert!(read(&b""[..], FIELDS).unwrap().is_none());
        assert_eq!(read_bytes(b"null").event, Value::Null);
    }

    const OUTPUT_FIELDS: &[Field] = &[
        whole("session_id"),
        only(
            "tool_response",
            &[whole("exit_code"), tail("stdout"), tail("stderr")],
        ),
    ];

    #[test]
    fn a_tail_field_keeps_only_the_end_of_a_long_string() {
        let window = crate::failure_output::WINDOW_BYTES;
        let stdout = format!("{}\nerror: the end", "x".repeat(3 * window));
        let payload = json!({
            "session_id":"s1",
            "tool_response":{"stdout":stdout,"stderr":"short","exit_code":2}
        })
        .to_string();
        let event = read(payload.as_bytes(), OUTPUT_FIELDS)
            .unwrap()
            .unwrap()
            .event;
        let (text, dropped) = tail_text(&event["tool_response"]["stdout"]).unwrap();
        assert_eq!(text.len(), window);
        assert!(text.ends_with("error: the end"));
        assert_eq!(dropped, (stdout.len() - window) as u64);
        assert_eq!(
            tail_text(&event["tool_response"]["stderr"]),
            Some(("short", 0))
        );
        assert_eq!(event["tool_response"]["exit_code"], 2);
    }

    #[test]
    fn a_tail_field_of_another_type_is_null_and_cuts_on_a_character() {
        let event = read(
            br#"{"tool_response":{"stdout":{"text":"x"},"stderr":[1]}}"#.as_slice(),
            OUTPUT_FIELDS,
        )
        .unwrap()
        .unwrap()
        .event;
        assert_eq!(event["tool_response"], json!({"stdout":null,"stderr":null}));
        assert_eq!(tail_text(&Value::Null), None);

        let window = crate::failure_output::WINDOW_BYTES;
        let payload = json!({"tool_response":{"stdout":"é".repeat(window)}}).to_string();
        let event = read(payload.as_bytes(), OUTPUT_FIELDS)
            .unwrap()
            .unwrap()
            .event;
        let (text, dropped) = tail_text(&event["tool_response"]["stdout"]).unwrap();
        assert!(text.chars().all(|c| c == 'é'));
        assert_eq!(text.len() as u64 + dropped, (2 * window) as u64);
    }

    #[test]
    fn a_payload_over_the_cap_keeps_the_fields_completed_before_the_cut() {
        let cap = usize::try_from(MAX_HOOK_PAYLOAD_BYTES).unwrap();
        let payload = format!(
            r#"{{"session_id":"s1","tool_input":{{"command":"cat big"}},"tool_response":{{"stdout":"{}"}}}}"#,
            "x".repeat(cap)
        );
        let payload = read_bytes(payload.as_bytes());
        assert!(payload.truncated);
        assert_eq!(
            payload.event,
            json!({"session_id":"s1","tool_input":{"command":"cat big"}})
        );
        assert!(payload.require(true).is_ok());
        assert!(payload
            .require(false)
            .unwrap_err()
            .contains("exceeds 16 MB"));
    }
}
