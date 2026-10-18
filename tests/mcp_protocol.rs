//! `suv mcp-serve` over a real stdin/stdout pipe: every input line that
//! needs an answer gets exactly one, stdout carries nothing but JSON-RPC
//! messages, and no input — malformed, non-UTF-8, batched, or asking for a
//! date or window too large to represent — ends the session early.
//!
//! Runs the real binary against a private `$HOME` and data directory, so
//! nothing here can touch the developer's own history.
use std::io::Write as _;
use std::process::{Command, Stdio};

fn serve(input: &[u8]) -> (Vec<serde_json::Value>, std::process::ExitStatus) {
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_suv"))
        .arg("mcp-serve")
        .current_dir(home.path())
        .env("HOME", home.path())
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env_remove("SUVADU_PAUSED")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(input).unwrap();
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8(out.stdout).expect("stdout must be UTF-8");
    let messages = stdout
        .lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("non-protocol line on stdout ({e}): {line:?}"))
        })
        .collect();
    (messages, out.status)
}

fn line(value: &serde_json::Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(value).unwrap();
    bytes.push(b'\n');
    bytes
}

#[test]
fn hostile_input_never_ends_the_session_or_pollutes_stdout() {
    let far = "200000000000 days ago";
    let mut input = Vec::new();
    input.extend(line(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}
    })));
    input.extend(line(
        &serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    ));
    input.extend(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"\xff\"}\n");
    input.extend(b"[{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"ping\"}]\n");
    input.extend(b"not json\n");
    for (id, name, args) in [
        (
            4,
            "search_commands",
            serde_json::json!({"query": "x", "after": far}),
        ),
        (5, "find_agent_session", serde_json::json!({"before": far})),
        (6, "get_stats", serde_json::json!({"days": i64::MAX})),
        (7, "what_changed", serde_json::json!({"hours": i64::MIN})),
    ] {
        input.extend(line(&serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": name, "arguments": args}
        })));
    }
    input.extend(line(&serde_json::json!({
        "jsonrpc": "2.0", "method": "tools/call",
        "params": {"name": "recent_commands", "arguments": {}}
    })));
    input.extend(line(
        &serde_json::json!({"jsonrpc": "2.0", "id": "end", "method": "ping"}),
    ));

    let (messages, status) = serve(&input);

    assert!(status.success(), "{status}");
    let ids: Vec<_> = messages.iter().map(|m| m["id"].clone()).collect();
    assert_eq!(
        ids,
        serde_json::json!([1, null, null, null, 4, 5, 6, 7, "end"])
            .as_array()
            .unwrap()
            .clone(),
        "{messages:?}"
    );
    assert_eq!(messages[0]["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(messages[1]["error"]["code"], -32700);
    assert_eq!(messages[2]["error"]["code"], -32600);
    assert_eq!(messages[3]["error"]["code"], -32700);
    // An unrepresentable date is a tool error naming the accepted formats.
    assert_eq!(messages[4]["result"]["isError"], true);
    assert!(messages[4]["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("YYYY-MM-DD"));
    assert_eq!(messages[5]["result"]["isError"], true);
    assert_eq!(messages[6]["result"]["isError"], false);
    assert_eq!(messages[7]["result"]["isError"], false);
}
