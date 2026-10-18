use serde_json::{json, Value};

/// The newest MCP protocol version we speak, answered whenever the client
/// asks for one we do not.
pub const PROTOCOL_VERSION: &str = "2025-11-25";

/// Every MCP protocol version this server can answer in. Everything it
/// sends — text tool results with `isError`, resources, resource
/// templates, prompts, `instructions` — exists unchanged in all of them,
/// and it sends nothing a later version added (`structuredContent`,
/// `outputSchema`, titles, icons), so one set of responses serves each.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] =
    &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// Maximum number of bytes in a tool result before truncation. Paged tools
/// keep under it themselves, so this cut is a last resort.
const MAX_OUTPUT_CHARS: usize = super::conventions::MAX_RESPONSE_BYTES;

/// The protocol version to answer `initialize` with: the client's own when
/// we support it, otherwise our latest, which the client may then reject.
pub fn negotiate_version(params: &Value) -> &'static str {
    params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .and_then(|asked| {
            SUPPORTED_PROTOCOL_VERSIONS
                .iter()
                .copied()
                .find(|&v| v == asked)
        })
        .unwrap_or(PROTOCOL_VERSION)
}

/// Build the response for `initialize`, given the request's `params`.
pub fn handle_initialize(id: &Value, params: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": negotiate_version(params),
            "capabilities": {
                "tools": { "listChanged": false },
                "resources": { "subscribe": false, "listChanged": false },
                "prompts": { "listChanged": false }
            },
            "serverInfo": {
                "name": "suvadu-mcp",
                "version": env!("CARGO_PKG_VERSION")
            },
            "instructions": "Suvadu shell history server: local, structured shell history and AI-agent activity for this machine. \
    Before starting work in an unfamiliar project, call `project_context` and `learn_from_failures` to avoid repeating known-bad approaches. \
    Before running a command that looks destructive or unfamiliar, call `assess_risk` first. \
    Check `list_skills`/`search_skills` for existing shared instructions before writing a new checklist from scratch. \
    When the user asks to summarize this/current agent session, call `resolve_current_agent_session`, read the returned session pages, and call `save_session_summary` only when the user explicitly asks to save. \
    All data is local; nothing leaves this machine."
        }
    })
}

/// Build the response for `ping`.
pub fn handle_ping(id: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {}
    })
}

/// Build a JSON-RPC error response.
pub fn error_response(id: &Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

/// Build a successful tool call result with text content.
/// Output exceeding [`MAX_OUTPUT_CHARS`] is truncated with a suffix.
pub fn tool_result(id: &Value, text: &str) -> Value {
    let truncated;
    let output = if text.len() > MAX_OUTPUT_CHARS {
        // Snap the byte cap down to a UTF-8 char boundary so multibyte content
        // (emoji / CJK / accents) landing on the boundary can't panic the slice.
        let mut end = MAX_OUTPUT_CHARS;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        truncated = format!("{}... (output truncated)", &text[..end]);
        &truncated
    } else {
        text
    };
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{ "type": "text", "text": output }],
            "isError": false
        }
    })
}

/// Build a tool call error result (tool executed but failed).
pub fn tool_error(id: &Value, text: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{ "type": "text", "text": text }],
            "isError": true
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initialize_response() {
        let resp = handle_initialize(&json!(1), &json!({}));
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["serverInfo"]["name"], "suvadu-mcp");
        assert!(resp["result"]["capabilities"]["tools"].is_object());
        assert!(resp["result"]["capabilities"]["prompts"].is_object());
    }

    #[test]
    fn test_initialize_instructions_are_directive_not_just_descriptive() {
        // Guards against the instructions field regressing into a generic
        // "here's what this server is" blurb — it should tell a connecting
        // client *when* to proactively call the highest-value tools.
        let resp = handle_initialize(&json!(1), &json!({}));
        let instructions = resp["result"]["instructions"].as_str().unwrap();
        assert!(instructions.contains("project_context"));
        assert!(instructions.contains("learn_from_failures"));
        assert!(instructions.contains("assess_risk"));
        assert!(instructions.contains("list_skills") || instructions.contains("search_skills"));
        assert!(instructions.contains("resolve_current_agent_session"));
        assert!(instructions.contains("save_session_summary"));
    }

    #[test]
    fn test_ping_response() {
        let resp = handle_ping(&json!(42));
        assert_eq!(resp["id"], 42);
        assert!(resp["result"].is_object());
    }

    #[test]
    fn test_error_response() {
        let resp = error_response(&json!(1), -32601, "Method not found");
        assert_eq!(resp["error"]["code"], -32601);
        assert_eq!(resp["error"]["message"], "Method not found");
    }

    #[test]
    fn test_tool_result() {
        let resp = tool_result(&json!(5), "hello");
        assert_eq!(resp["result"]["content"][0]["type"], "text");
        assert_eq!(resp["result"]["content"][0]["text"], "hello");
        assert_eq!(resp["result"]["isError"], false);
    }

    #[test]
    fn test_tool_error() {
        let resp = tool_error(&json!(5), "something went wrong");
        assert_eq!(resp["result"]["isError"], true);
    }

    #[test]
    fn test_tool_result_truncates_long_output() {
        let long_text = "x".repeat(MAX_OUTPUT_CHARS + 1000);
        let resp = tool_result(&json!(1), &long_text);
        let output = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            output.len() <= MAX_OUTPUT_CHARS + 30,
            "output should be capped near MAX_OUTPUT_CHARS"
        );
        assert!(
            output.ends_with("... (output truncated)"),
            "truncated output should end with suffix"
        );
    }

    #[test]
    fn test_tool_result_truncates_multibyte_without_panic() {
        // A multibyte char (😀 = 4 bytes) straddling the byte cap must not panic
        // the slice; truncation snaps down to a char boundary.
        let long_text = "😀".repeat(MAX_OUTPUT_CHARS); // far exceeds the byte cap
        let resp = tool_result(&json!(1), &long_text);
        let output = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(output.ends_with("... (output truncated)"));
        // The body before the suffix must be valid UTF-8 made of whole emoji.
        let body = output.trim_end_matches("... (output truncated)");
        assert!(body.chars().all(|c| c == '😀'));
    }

    #[test]
    fn test_tool_result_no_truncation_under_limit() {
        let short_text = "hello world";
        let resp = tool_result(&json!(1), short_text);
        let output = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert_eq!(output, short_text);
    }
}
