use std::io::{self, BufRead, Write};

use crate::repository::Repository;

use super::protocol;
use super::resources;
use super::tools;

/// Run the MCP server: read JSON-RPC from stdin, write responses to stdout.
/// All logging goes to stderr. The database is opened read-only.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::init_read_only()?;
    let config = crate::config::load_config().unwrap_or_default();
    // Apply user risk-ignore suppressions so the assess_risk tool honors them.
    crate::risk::set_ignore_patterns(&config.agent.risk_ignore_patterns);
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
                Some(handle_tool_call(&repo, rid, &request, mcp))
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
                Some(handle_resource_read(&repo, rid, &request, mcp))
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

fn handle_tool_call(
    repo: &Repository,
    id: &serde_json::Value,
    request: &serde_json::Value,
    mcp: &crate::config::McpConfig,
) -> serde_json::Value {
    let name = request["params"]["name"].as_str().unwrap_or("");
    let empty = serde_json::Value::Object(serde_json::Map::new());
    let args = request["params"].get("arguments").unwrap_or(&empty);

    match tools::call_tool(repo, name, args, mcp) {
        Ok(text) => protocol::tool_result(id, &text),
        Err(msg) => protocol::tool_error(id, &msg),
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

    fn make_request(method: &str, id: Option<i64>) -> String {
        let mut req = json!({"jsonrpc": "2.0", "method": method});
        if let Some(i) = id {
            req["id"] = json!(i);
        }
        serde_json::to_string(&req).unwrap()
    }

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
}
