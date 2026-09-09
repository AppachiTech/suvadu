//! MCP `prompts` capability: reusable, parameterized request templates a
//! client can surface directly to a human (often as slash commands),
//! independent of whether the model decides to reach for a tool on its own.
//! Each prompt just expands to a canned user-role message naming the
//! suvadu tool(s) to call — the actual work still happens through the
//! existing `tools/call` path in `tools.rs`.

use serde_json::{json, Value};

/// Build the response for `prompts/list`.
pub fn list_prompts(id: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "prompts": [
                {
                    "name": "project_briefing",
                    "description": "Get oriented in this project: common commands, recent failures, and agent activity before making changes."
                },
                {
                    "name": "check_recent_failures",
                    "description": "Summarize recent command failures in this project and whether agents fail more than humans.",
                    "arguments": [
                        {
                            "name": "days",
                            "description": "How many days back to look (default: 7)",
                            "required": false
                        }
                    ]
                },
                {
                    "name": "assess_command_risk",
                    "description": "Assess the risk of a shell command before running it.",
                    "arguments": [
                        {
                            "name": "command",
                            "description": "The command to assess",
                            "required": true
                        }
                    ]
                }
            ]
        }
    })
}

/// Build the response for `prompts/get`. `request` is the full incoming
/// JSON-RPC request so this can read `params.name`/`params.arguments`
/// itself, matching how `handle_tool_call` reads `tools/call`'s params.
pub fn get_prompt(id: &Value, request: &Value) -> Value {
    let name = request["params"]["name"].as_str().unwrap_or("");
    let args = &request["params"]["arguments"];

    let text = match name {
        "project_briefing" => {
            "Use the suvadu MCP server's `project_context` and `learn_from_failures` tools to \
             give me a briefing on this project before I start working: common commands, \
             build/test/lint patterns, recent failures, and agent activity."
                .to_string()
        }
        "check_recent_failures" => {
            let days = args["days"].as_str().unwrap_or("7");
            format!(
                "Use the suvadu MCP server's `learn_from_failures` tool (last {days} days) to \
                 summarize recurring command failures in this project and whether agents fail \
                 more than humans."
            )
        }
        "assess_command_risk" => {
            let command = args["command"].as_str().unwrap_or("");
            format!(
                "Use the suvadu MCP server's `assess_risk` tool to assess this command before I \
                 run it: `{command}`"
            )
        }
        _ => {
            return json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32602,
                    "message": format!("Unknown prompt: {name}")
                }
            });
        }
    };

    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "messages": [
                {
                    "role": "user",
                    "content": { "type": "text", "text": text }
                }
            ]
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get_request(name: &str, arguments: &Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "prompts/get",
            "params": { "name": name, "arguments": arguments }
        })
    }

    #[test]
    fn list_prompts_returns_exactly_the_three_expected_names() {
        let resp = list_prompts(&json!(1));
        let names: Vec<&str> = resp["result"]["prompts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec![
                "project_briefing",
                "check_recent_failures",
                "assess_command_risk"
            ]
        );
    }

    #[test]
    fn list_prompts_marks_command_as_required_for_assess_command_risk() {
        let resp = list_prompts(&json!(1));
        let prompts = resp["result"]["prompts"].as_array().unwrap();
        let assess = prompts
            .iter()
            .find(|p| p["name"] == "assess_command_risk")
            .unwrap();
        assert_eq!(assess["arguments"][0]["name"], "command");
        assert_eq!(assess["arguments"][0]["required"], true);
    }

    #[test]
    fn get_project_briefing_mentions_both_underlying_tools() {
        let req = get_request("project_briefing", &json!({}));
        let resp = get_prompt(&json!(1), &req);
        let text = resp["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap();
        assert!(text.contains("project_context"));
        assert!(text.contains("learn_from_failures"));
    }

    #[test]
    fn get_check_recent_failures_interpolates_days() {
        let req = get_request("check_recent_failures", &json!({ "days": "30" }));
        let resp = get_prompt(&json!(1), &req);
        let text = resp["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap();
        assert!(text.contains("30 days"));
    }

    #[test]
    fn get_check_recent_failures_defaults_days_to_seven() {
        let req = get_request("check_recent_failures", &json!({}));
        let resp = get_prompt(&json!(1), &req);
        let text = resp["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap();
        assert!(text.contains("7 days"));
    }

    #[test]
    fn get_assess_command_risk_interpolates_command() {
        let req = get_request("assess_command_risk", &json!({ "command": "rm -rf /" }));
        let resp = get_prompt(&json!(1), &req);
        let text = resp["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap();
        assert!(text.contains("rm -rf /"));
    }

    #[test]
    fn get_unknown_prompt_returns_error_not_panic() {
        let req = get_request("nonexistent_prompt", &json!({}));
        let resp = get_prompt(&json!(1), &req);
        assert_eq!(resp["error"]["code"], -32602);
        assert!(resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("nonexistent_prompt"));
    }
}
