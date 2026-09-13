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
    let mcp = &config.mcp;
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let request: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[suvadu-mcp] parse error: {e}");
                let resp =
                    protocol::error_response(&serde_json::Value::Null, -32700, "Parse error");
                writeln!(stdout, "{}", serde_json::to_string(&resp)?)?;
                stdout.flush()?;
                continue;
            }
        };

        let method = request["method"].as_str().unwrap_or("");
        let id = request.get("id").cloned();

        // Notifications (no id) don't get a response
        let is_notification = id.is_none();

        let response = match method {
            "initialize" => {
                let rid = id.as_ref().unwrap_or(&serde_json::Value::Null);
                Some(protocol::handle_initialize(rid))
            }
            "notifications/initialized" => None,
            "tools/list" => {
                let rid = id.as_ref().unwrap_or(&serde_json::Value::Null);
                Some(tools::list_tools(rid, mcp))
            }
            "tools/call" => {
                let rid = id.as_ref().unwrap_or(&serde_json::Value::Null);
                Some(repo.as_ref().map_or_else(
                    || protocol::tool_error(rid, &db_unavailable_message(db_error.as_deref())),
                    |repo| handle_tool_call(repo, rid, &request, mcp),
                ))
            }
            "resources/list" => {
                let rid = id.as_ref().unwrap_or(&serde_json::Value::Null);
                Some(resources::list_resources(rid, mcp))
            }
            "resources/templates/list" => {
                let rid = id.as_ref().unwrap_or(&serde_json::Value::Null);
                Some(resources::list_resource_templates(rid))
            }
            "resources/read" => {
                let rid = id.as_ref().unwrap_or(&serde_json::Value::Null);
                Some(repo.as_ref().map_or_else(
                    || {
                        protocol::error_response(
                            rid,
                            -32000,
                            &db_unavailable_message(db_error.as_deref()),
                        )
                    },
                    |repo| handle_resource_read(repo, rid, &request, mcp),
                ))
            }
            "prompts/list" => {
                let rid = id.as_ref().unwrap_or(&serde_json::Value::Null);
                Some(prompts::list_prompts(rid))
            }
            "prompts/get" => {
                let rid = id.as_ref().unwrap_or(&serde_json::Value::Null);
                Some(prompts::get_prompt(rid, &request))
            }
            "ping" => {
                let rid = id.as_ref().unwrap_or(&serde_json::Value::Null);
                Some(protocol::handle_ping(rid))
            }
            _ => {
                if is_notification {
                    // Unknown notifications are silently ignored per spec
                    None
                } else {
                    let rid = id.as_ref().unwrap_or(&serde_json::Value::Null);
                    Some(protocol::error_response(rid, -32601, "Method not found"))
                }
            }
        };

        if let Some(resp) = response {
            writeln!(stdout, "{}", serde_json::to_string(&resp)?)?;
            stdout.flush()?;
        }
    }

    Ok(())
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
            .contains("No commands found"));
    }

    #[test]
    fn test_malformed_json_returns_parse_error() {
        // Simulate what the server does when it receives malformed JSON:
        // serde_json::from_str fails and we produce an error_response.
        let bad_input = "{ not valid json }}}";
        let parse_result: Result<serde_json::Value, _> = serde_json::from_str(bad_input);
        assert!(parse_result.is_err());

        let resp = protocol::error_response(&serde_json::Value::Null, -32700, "Parse error");
        assert_eq!(resp["error"]["code"], -32700);
        assert_eq!(resp["error"]["message"], "Parse error");
        assert!(resp["id"].is_null());
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
