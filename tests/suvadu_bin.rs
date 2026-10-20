//! The `suvadu` binary runs the `suv` installed beside it, so it must behave
//! exactly like `suv`: same output, same exit status, and an MCP server that
//! answers over stdio.
use std::io::Write;
use std::process::{Command, Stdio};

fn run(bin: &str, args: &[&str]) -> std::process::Output {
    Command::new(bin)
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

#[test]
fn suvadu_prints_what_suv_prints() {
    let suv = run(env!("CARGO_BIN_EXE_suv"), &["--version"]);
    let suvadu = run(env!("CARGO_BIN_EXE_suvadu"), &["--version"]);
    assert!(suvadu.status.success());
    assert_eq!(suvadu.stdout, suv.stdout);
}

#[test]
fn suvadu_keeps_suvs_exit_status() {
    let suv = run(env!("CARGO_BIN_EXE_suv"), &["no-such-command"]);
    let suvadu = run(env!("CARGO_BIN_EXE_suvadu"), &["no-such-command"]);
    assert!(!suv.status.success());
    assert_eq!(suvadu.status.code(), suv.status.code());
}

#[test]
fn suvadu_mcp_serve_answers_initialize_over_stdio() {
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_suvadu"))
        .arg("mcp-serve")
        .env("HOME", home.path())
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}
"#,
        )
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let first = String::from_utf8_lossy(&out.stdout);
    let reply: serde_json::Value =
        serde_json::from_str(first.lines().next().expect("a reply line")).unwrap();
    assert_eq!(reply["id"], 1);
    assert!(reply["result"]["capabilities"].is_object(), "{reply}");
}
