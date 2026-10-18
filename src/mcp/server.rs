use std::io::{self, BufRead, Write};

use crate::repository::Repository;

use super::prompts;
use super::protocol;
use super::resources;
use super::tools;

/// Run the MCP server: read JSON-RPC from stdin, write responses to stdout.
/// All logging goes to stderr. Reads use a read-only database connection;
/// explicitly enabled summary writes use a short-lived writable connection.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    // `Repository::init_read_only()` below deliberately skips migrations (see
    // its doc comment) so the long-lived server session never writes. But
    // that means a fresh install (no DB file yet) or a DB left on an older
    // schema version right after an upgrade would otherwise surface a raw
    // "no such table" error on the first tool call instead of the intended
    // empty-state message. Open-migrate-drop once up front to guarantee the
    // schema is current before the read-only connection is opened.
    //
    // This can fail (e.g. the DB was migrated to a newer schema by a
    // different Suvadu build than the one running as this MCP server). When
    // it does, we still complete the MCP handshake below and every
    // DB-touching call reports the error, instead of the process exiting
    // before the client ever gets a response — which surfaces to hosts as an
    // opaque "Connection closed" with no indication of what went wrong.
    let db_path = crate::db::get_db_path()?;
    let (repo, db_error) = init_repo(&db_path);
    let config = crate::config::load_config().unwrap_or_default();
    // Apply user risk-ignore suppressions so the assess_risk tool honors them.
    crate::risk::set_ignore_patterns(&config.agent.risk_ignore_patterns);
    crate::risk::set_extra_patterns(&config.agent.risk_extra_patterns);
    let server = Server {
        repo: repo.as_ref(),
        db_error: db_error.as_deref(),
        mcp: &config.mcp,
    };
    let stdin = io::stdin();
    let stdout = io::stdout();
    serve(&mut stdin.lock(), &mut stdout.lock(), &server)
}

/// Everything one server session needs to answer a message.
struct Server<'a> {
    repo: Option<&'a Repository>,
    db_error: Option<&'a str>,
    mcp: &'a crate::config::McpConfig,
}

/// Answer line-delimited JSON-RPC from `input` on `output` until `input`
/// ends. Only protocol messages are ever written to `output`.
///
/// Lines are read as bytes: a line that is not valid UTF-8 is that line's
/// parse error, not a reason to stop serving every later request.
fn serve(
    input: &mut impl BufRead,
    output: &mut impl Write,
    server: &Server<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut line = Vec::new();
    loop {
        line.clear();
        if input.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        if let Some(resp) = server.handle_line(&line) {
            writeln!(output, "{}", serde_json::to_string(&resp)?)?;
            output.flush()?;
        }
    }
}

/// JSON-RPC "Parse error": the line was not valid UTF-8 JSON.
const PARSE_ERROR: i64 = -32700;
/// JSON-RPC "Invalid Request": valid JSON, but not a request object.
const INVALID_REQUEST: i64 = -32600;

impl Server<'_> {
    /// The response to one input line, or `None` when it needs none.
    fn handle_line(&self, line: &[u8]) -> Option<serde_json::Value> {
        let Ok(text) = std::str::from_utf8(line) else {
            eprintln!("[suvadu-mcp] parse error: input line is not valid UTF-8");
            return Some(protocol::error_response(
                &serde_json::Value::Null,
                PARSE_ERROR,
                "Parse error",
            ));
        };
        if text.trim().is_empty() {
            return None;
        }
        match serde_json::from_str(text) {
            Ok(message) => self.handle_message(&message),
            Err(e) => {
                eprintln!("[suvadu-mcp] parse error: {e}");
                Some(protocol::error_response(
                    &serde_json::Value::Null,
                    PARSE_ERROR,
                    "Parse error",
                ))
            }
        }
    }

    fn handle_message(&self, message: &serde_json::Value) -> Option<serde_json::Value> {
        let null = serde_json::Value::Null;
        if message.is_array() {
            // MCP removed JSON-RPC batching in 2025-06-18. A batch gets one
            // error rather than silence, so the client is not left waiting.
            return Some(protocol::error_response(
                &null,
                INVALID_REQUEST,
                "Invalid Request: batch requests are not supported",
            ));
        }
        let Some(object) = message.as_object() else {
            return Some(protocol::error_response(
                &null,
                INVALID_REQUEST,
                "Invalid Request: expected a JSON-RPC object",
            ));
        };
        let Some(id) = object.get("id") else {
            // A notification is never answered, so nothing it names is run
            // either: a tools/call without an id would execute with no way
            // to report its result.
            return None;
        };
        let method = match object.get("method") {
            Some(serde_json::Value::String(method)) => method.as_str(),
            // A response to a request the server never sent: not answered.
            None if object.contains_key("result") || object.contains_key("error") => {
                return None;
            }
            _ => {
                let rid = if id.is_string() || id.is_number() {
                    id
                } else {
                    &null
                };
                return Some(protocol::error_response(
                    rid,
                    INVALID_REQUEST,
                    "Invalid Request: missing method",
                ));
            }
        };
        if !(id.is_string() || id.is_number()) {
            return Some(protocol::error_response(
                &null,
                INVALID_REQUEST,
                "Invalid Request: id must be a string or a number",
            ));
        }
        Some(self.handle_request(id, method, message))
    }

    /// Answer one request that has a valid id and method.
    fn handle_request(
        &self,
        rid: &serde_json::Value,
        method: &str,
        request: &serde_json::Value,
    ) -> serde_json::Value {
        match method {
            "initialize" => protocol::handle_initialize(rid, &request["params"]),
            "tools/list" => tools::list_tools(rid, self.mcp),
            "tools/call" => self.repo.map_or_else(
                || protocol::tool_error(rid, &db_unavailable_message(self.db_error)),
                |repo| handle_tool_call(repo, rid, request, self.mcp),
            ),
            "resources/list" => resources::list_resources(rid, self.mcp),
            "resources/templates/list" => resources::list_resource_templates(rid),
            "resources/read" => self.repo.map_or_else(
                || protocol::error_response(rid, -32000, &db_unavailable_message(self.db_error)),
                |repo| handle_resource_read(repo, rid, request, self.mcp),
            ),
            "prompts/list" => prompts::list_prompts(rid),
            "prompts/get" => prompts::get_prompt(rid, request),
            "ping" => protocol::handle_ping(rid),
            _ => protocol::error_response(rid, -32601, "Method not found"),
        }
    }
}

/// Migrate and open the read-only repository used by the long-lived MCP
/// session. Returns `(None, Some(reason))` instead of propagating the error
/// when the DB can't be initialized (e.g. schema newer than this build
/// supports), so `run` can still complete the MCP handshake and report the
/// reason on every DB-touching call.
fn init_repo(db_path: &std::path::PathBuf) -> (Option<Repository>, Option<String>) {
    match crate::db::init_db(db_path) {
        Ok(_) => match Repository::init_read_only(db_path) {
            Ok(repo) => (Some(repo), None),
            Err(e) => (None, Some(e.to_string())),
        },
        Err(e) => (None, Some(e.to_string())),
    }
}

/// Message returned for any DB-touching call when the server started without
/// a usable database connection (see [`init_repo`]).
fn db_unavailable_message(db_error: Option<&str>) -> String {
    let reason = db_error.unwrap_or("unknown error");
    format!("Suvadu database unavailable: {reason}")
}

fn handle_tool_call(
    repo: &Repository,
    id: &serde_json::Value,
    request: &serde_json::Value,
    mcp: &crate::config::McpConfig,
) -> serde_json::Value {
    let name = request["params"]["name"].as_str().unwrap_or("");
    let empty = serde_json::Value::Object(serde_json::Map::new());
    let args = request["params"].get("arguments").unwrap_or(&empty);

    let result = if name == "save_session_summary" {
        // Validate authorization and input before opening any writable connection.
        super::ai_sessions::summary_input(args, mcp).and_then(|_| {
            let writer = Repository::init().map_err(|e| e.to_string())?;
            tools::call_tool(&writer, name, args, mcp)
        })
    } else {
        tools::call_tool(repo, name, args, mcp)
    };
    match result {
        Ok(text) => tool_response(id, name, &text),
        Err(msg) => protocol::tool_error(id, &msg),
    }
}

/// Session tools already paginate records. Byte truncation would corrupt JSON
/// and discard pagination cursors or summary evidence.
fn tool_response(id: &serde_json::Value, name: &str, text: &str) -> serde_json::Value {
    if matches!(
        name,
        "list_agent_sessions" | "get_agent_session" | "save_session_summary"
    ) {
        serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "result": {"content": [{"type": "text", "text": text}], "isError": false}
        })
    } else {
        protocol::tool_result(id, text)
    }
}

fn handle_resource_read(
    repo: &Repository,
    id: &serde_json::Value,
    request: &serde_json::Value,
    mcp: &crate::config::McpConfig,
) -> serde_json::Value {
    let uri = request["params"]["uri"].as_str().unwrap_or("");
    match resources::read_resource(repo, uri, mcp) {
        Ok(result) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result
        }),
        Err(msg) => protocol::error_response(id, -32602, &msg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_handle_tool_call_unknown() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let req = json!({
            "params": {
                "name": "nonexistent",
                "arguments": {}
            }
        });
        let mcp = crate::config::McpConfig::default();
        let resp = handle_tool_call(&repo, &json!(1), &req, &mcp);
        assert_eq!(resp["result"]["isError"], true);
    }

    #[test]
    fn test_handle_tool_call_search() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let req = json!({
            "params": {
                "name": "search_commands",
                "arguments": {"query": "test"}
            }
        });
        let mcp = crate::config::McpConfig::default();
        let resp = handle_tool_call(&repo, &json!(1), &req, &mcp);
        assert_eq!(resp["result"]["isError"], false);
        assert!(resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("shown: 0 of 0"));
    }

    /// Feed `input` to a server session over `repo` and return every line it
    /// wrote, each parsed as JSON — so a stray non-protocol line fails here.
    fn exchange(repo: Option<&Repository>, input: &[u8]) -> Vec<serde_json::Value> {
        let mcp = crate::config::McpConfig::default();
        let server = Server {
            repo,
            db_error: None,
            mcp: &mcp,
        };
        let mut output = Vec::new();
        serve(&mut &input[..], &mut output, &server).unwrap();
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn serve_answers_requests_in_order_and_skips_blank_lines() {
        let responses = exchange(
            None,
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n\
              \n\
              {\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n\
              {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n",
        );
        assert_eq!(responses.len(), 2, "{responses:?}");
        assert_eq!(responses[0]["id"], 1);
        assert_eq!(responses[0]["result"]["serverInfo"]["name"], "suvadu-mcp");
        assert_eq!(responses[1]["id"], 2);
    }

    #[test]
    fn malformed_json_gets_a_parse_error_and_the_server_keeps_serving() {
        let responses = exchange(
            None,
            b"{ not valid json }}}\n{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n",
        );
        assert_eq!(responses.len(), 2, "{responses:?}");
        assert_eq!(responses[0]["error"]["code"], -32700);
        assert_eq!(responses[0]["error"]["message"], "Parse error");
        assert!(responses[0]["id"].is_null());
        assert_eq!(responses[1]["id"], 7);
    }

    fn initialize_line(version: Option<&str>) -> Vec<u8> {
        let params = version.map_or_else(
            || json!({}),
            |v| json!({"protocolVersion": v, "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}),
        );
        let mut line = serde_json::to_vec(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": params}),
        )
        .unwrap();
        line.push(b'\n');
        line
    }

    #[test]
    fn initialize_echoes_a_supported_protocol_version_the_client_asked_for() {
        for version in ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"] {
            let responses = exchange(None, &initialize_line(Some(version)));
            assert_eq!(
                responses[0]["result"]["protocolVersion"], version,
                "{responses:?}"
            );
        }
    }

    #[test]
    fn initialize_answers_the_latest_version_when_the_request_is_unsupported_or_missing() {
        for request in [
            initialize_line(Some("1999-01-01")),
            initialize_line(None),
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":5}}\n".to_vec(),
        ] {
            let responses = exchange(None, &request);
            assert_eq!(
                responses[0]["result"]["protocolVersion"], "2025-11-25",
                "{responses:?}"
            );
        }
    }

    fn tool_call_line(id: usize, name: &str, arguments: &serde_json::Value) -> Vec<u8> {
        let mut line = serde_json::to_vec(&json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }))
        .unwrap();
        line.push(b'\n');
        line
    }

    #[test]
    fn huge_dates_and_time_windows_do_not_take_the_server_down() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let far = "200000000000 days ago";
        let calls = [
            ("search_commands", json!({"query": "x", "after": far})),
            ("search_commands", json!({"query": "x", "before": far})),
            ("recent_commands", json!({"after": far})),
            ("get_prompts", json!({"after": far})),
            ("find_agent_session", json!({"after": far, "before": far})),
            ("get_stats", json!({"days": i64::MAX})),
            ("get_stats", json!({"days": i64::MIN})),
            ("what_changed", json!({"hours": i64::MAX})),
            ("what_changed", json!({"hours": i64::MIN})),
            ("what_failed", json!({"hours": i64::MAX})),
            ("learn_from_failures", json!({"days": i64::MAX})),
            ("project_context", json!({"days": i64::MAX})),
        ];
        let mut input = Vec::new();
        for (i, (name, args)) in calls.iter().enumerate() {
            input.extend(tool_call_line(i, name, args));
        }
        input.extend(b"{\"jsonrpc\":\"2.0\",\"id\":\"last\",\"method\":\"ping\"}\n");

        let responses = exchange(Some(&repo), &input);

        assert_eq!(responses.len(), calls.len() + 1, "{responses:?}");
        for (i, response) in responses.iter().take(calls.len()).enumerate() {
            assert_eq!(response["id"], i, "{response}");
            assert!(
                response["result"]["content"][0]["text"].is_string(),
                "{response}"
            );
        }
        assert_eq!(responses[calls.len()]["id"], "last");
    }

    #[test]
    fn a_batch_gets_one_invalid_request_error_instead_of_silence() {
        let responses = exchange(
            None,
            b"[{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"},{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}]\n[]\n",
        );
        assert_eq!(responses.len(), 2, "{responses:?}");
        for response in &responses {
            assert_eq!(response["error"]["code"], -32600, "{response}");
            assert!(response["id"].is_null(), "{response}");
        }
    }

    #[test]
    fn a_json_value_that_is_not_an_object_is_an_invalid_request() {
        let responses = exchange(None, b"42\n\"ping\"\nnull\ntrue\n");
        assert_eq!(responses.len(), 4, "{responses:?}");
        for response in &responses {
            assert_eq!(response["error"]["code"], -32600, "{response}");
            assert!(response["id"].is_null(), "{response}");
        }
    }

    #[test]
    fn a_request_without_a_method_is_an_invalid_request_not_an_unknown_method() {
        let responses = exchange(
            None,
            b"{\"jsonrpc\":\"2.0\",\"id\":4}\n{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":7}\n",
        );
        assert_eq!(responses.len(), 2, "{responses:?}");
        assert_eq!(responses[0]["error"]["code"], -32600);
        assert_eq!(responses[0]["id"], 4);
        assert_eq!(responses[1]["error"]["code"], -32600);
        assert_eq!(responses[1]["id"], 5);
    }

    #[test]
    fn a_request_id_that_is_not_a_string_or_number_is_an_invalid_request() {
        let responses = exchange(
            None,
            b"{\"jsonrpc\":\"2.0\",\"id\":{\"x\":1},\"method\":\"ping\"}\n",
        );
        assert_eq!(responses.len(), 1, "{responses:?}");
        assert_eq!(responses[0]["error"]["code"], -32600);
        assert!(responses[0]["id"].is_null());
    }

    #[test]
    fn notifications_never_get_a_response_and_never_run_a_tool() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let responses = exchange(
            Some(&repo),
            b"{\"jsonrpc\":\"2.0\",\"method\":\"tools/call\",\"params\":{\"name\":\"search_commands\",\"arguments\":{\"query\":\"x\"}}}\n\
              {\"jsonrpc\":\"2.0\",\"method\":\"ping\"}\n\
              {\"jsonrpc\":\"2.0\",\"method\":\"notifications/cancelled\",\"params\":{\"requestId\":1}}\n\
              {\"jsonrpc\":\"2.0\",\"method\":\"something/unknown\"}\n\
              {\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"ping\"}\n",
        );
        assert_eq!(responses.len(), 1, "{responses:?}");
        assert_eq!(responses[0]["id"], 9);
    }

    #[test]
    fn a_response_from_the_client_is_not_answered() {
        let responses = exchange(
            None,
            b"{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{}}\n{\"jsonrpc\":\"2.0\",\"id\":3,\"error\":{\"code\":1,\"message\":\"x\"}}\n",
        );
        assert!(responses.is_empty(), "{responses:?}");
    }

    #[test]
    fn invalid_utf8_is_a_parse_error_and_the_server_keeps_serving() {
        let responses = exchange(
            None,
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"\xff\xfe\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n",
        );
        assert_eq!(responses.len(), 2, "{responses:?}");
        assert_eq!(responses[0]["error"]["code"], -32700);
        assert!(responses[0]["id"].is_null());
        assert_eq!(responses[1]["id"], 2);
    }

    #[test]
    fn a_final_line_without_a_newline_is_still_answered() {
        let responses = exchange(None, b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}");
        assert_eq!(responses.len(), 1, "{responses:?}");
        assert_eq!(responses[0]["id"], 1);
    }
    #[test]
    fn session_json_keeps_pagination_after_large_payload() {
        let text = json!({"events": [{"text": "x".repeat(60_000)}], "next_offset": 20}).to_string();
        let response = tool_response(&json!(1), "get_agent_session", &text);
        let decoded: serde_json::Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(decoded["next_offset"], 20);
    }

    #[test]
    fn init_repo_reports_reason_instead_of_erroring_on_newer_schema() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        // Create a DB at the current schema, then bump its recorded version
        // past what this build supports -- simulating a DB last touched by a
        // newer Suvadu build (the scenario that used to kill mcp-serve
        // before the client ever got a response).
        {
            let conn = crate::db::init_db(&db_path).unwrap();
            conn.execute("UPDATE schema_version SET version = version + 1", [])
                .unwrap();
        }

        let (repo, db_error) = init_repo(&db_path);

        assert!(repo.is_none());
        let reason = db_error.expect("newer-schema DB should report a reason");
        assert!(reason.contains("newer than this version"), "{reason}");
    }

    #[test]
    fn init_repo_succeeds_on_a_current_schema_db() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");

        let (repo, db_error) = init_repo(&db_path);

        assert!(repo.is_some());
        assert!(db_error.is_none());
    }

    #[test]
    fn summary_server_rejects_disabled_write_before_opening_database() {
        let (_dir, repo) = crate::test_utils::test_repo();
        for mcp in [
            crate::config::McpConfig::default(),
            crate::config::McpConfig {
                allow_session_summaries: true,
                disabled_tools: vec!["save_session_summary".into()],
                ..crate::config::McpConfig::default()
            },
        ] {
            let response = handle_tool_call(
                &repo,
                &json!(1),
                &json!({"params": {"name": "save_session_summary", "arguments": {}}}),
                &mcp,
            );
            assert_eq!(response["result"]["isError"], true);
            assert!(response["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("disabled"));
        }
    }
}
