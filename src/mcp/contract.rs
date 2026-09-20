//! PROD-13 contract fixtures: the response conventions every MCP tool and
//! resource must satisfy, exercised end-to-end through `call_tool` /
//! `read_resource` against tempfile databases only.
//!
//! These tests are deliberately written against the *convention*
//! (`super::conventions`), not against one tool's wording, so a new tool
//! that forgets the trailer, invents a second timestamp format or quietly
//! drops records fails here rather than in a user's agent transcript.

use serde_json::{json, Value};

use super::conventions as conv;
use crate::config::McpConfig;
use crate::models::{Entry, Session};
use crate::repository::Repository;

const OPEN_DIR: &str = "/work/open";
const SECRET_DIR: &str = "/work/secret";
const SHELL_SESSION: &str = "shell-1";
const AGENT_SESSION: &str = "claude-agent-1";

fn mcp() -> McpConfig {
    McpConfig {
        allow_session_summaries: true,
        allow_skill_proposals: true,
        ..McpConfig::default()
    }
}

fn session(repo: &Repository, id: &str) {
    repo.insert_session(&Session {
        id: id.to_string(),
        hostname: "fixture".into(),
        created_at: 1_700_000_000_000,
        tag_id: None,
    })
    .unwrap();
}

#[allow(clippy::too_many_arguments)]
fn entry(
    repo: &Repository,
    session_id: &str,
    command: &str,
    cwd: &str,
    exit: Option<i32>,
    at: i64,
    executor: Option<(&str, &str)>,
    prompt: Option<&str>,
) {
    let context = prompt.map(|p| {
        let mut map = std::collections::HashMap::new();
        map.insert("agent_prompt".to_string(), p.to_string());
        map
    });
    repo.insert_entry(&Entry {
        id: None,
        session_id: session_id.to_string(),
        command: command.to_string(),
        cwd: cwd.to_string(),
        exit_code: exit,
        started_at: at,
        ended_at: at + 1_500,
        duration_ms: 1_500,
        context,
        tag_name: None,
        tag_id: None,
        executor_type: executor.map(|(t, _)| t.to_string()),
        executor: executor.map(|(_, n)| n.to_string()),
    })
    .unwrap();
}

/// A repository with a human shell session, an agent session, a session
/// whose every command is in an excluded directory, one record with no
/// metadata at all, and one shared skill.
fn seeded() -> (tempfile::TempDir, Repository) {
    let (dir, repo) = crate::test_utils::test_repo();
    let now = chrono::Utc::now().timestamp_millis();

    session(&repo, SHELL_SESSION);
    entry(
        &repo,
        SHELL_SESSION,
        "cargo build --release",
        OPEN_DIR,
        Some(0),
        now - 60_000,
        Some(("human", "zsh")),
        None,
    );
    entry(
        &repo,
        SHELL_SESSION,
        "rm -rf ./target",
        OPEN_DIR,
        Some(1),
        now - 50_000,
        Some(("human", "zsh")),
        None,
    );
    // No exit code, no executor, no prompt: every optional field missing.
    entry(
        &repo,
        SHELL_SESSION,
        "vim src/lib.rs",
        OPEN_DIR,
        None,
        now - 40_000,
        None,
        None,
    );

    session(&repo, AGENT_SESSION);
    for (i, (command, exit)) in [
        ("cargo test parser", Some(101)),
        ("cargo test parser", Some(101)),
        ("cargo test parser", Some(101)),
        ("git commit -m fix", Some(0)),
    ]
    .into_iter()
    .enumerate()
    {
        entry(
            &repo,
            AGENT_SESSION,
            command,
            OPEN_DIR,
            exit,
            now - 30_000 + i64::try_from(i).unwrap_or(0) * 1_000,
            Some(("agent", "claude-code")),
            Some("Fix the flaky parser test"),
        );
    }

    session(&repo, "shell-secret");
    entry(
        &repo,
        "shell-secret",
        "cat topsecret.env",
        SECRET_DIR,
        Some(0),
        now - 20_000,
        Some(("human", "zsh")),
        None,
    );

    repo.create_skill(&crate::models::NewSkill {
        name: "release".into(),
        description: "How to cut a release".into(),
        body: "1. bump version\n2. tag\n".into(),
        triggers: vec!["release".into()],
        scope: crate::models::SKILL_SCOPE_GLOBAL.to_string(),
        source: "human".into(),
        status: crate::models::SKILL_STATUS_ACTIVE.to_string(),
    })
    .unwrap();

    (dir, repo)
}

/// Representative arguments for every catalog tool, so the conventions can
/// be asserted over the whole surface rather than the tools someone
/// remembered to check. Windows are set wide enough that the seeded
/// fixture is always inside them.
#[allow(clippy::match_same_arms)]
fn tool_args(name: &str) -> Option<Value> {
    Some(match name {
        "search_commands" => json!({"query": "cargo"}),
        "recent_commands" => json!({}),
        "command_status" => json!({"command": "cargo"}),
        "get_prompts" => json!({}),
        "session_history" => json!({"session_id": SHELL_SESSION}),
        "get_stats" => json!({"days": 3650}),
        "list_sessions" => json!({}),
        "what_changed" => json!({"hours": 87_600}),
        "what_failed" => json!({"hours": 87_600}),
        "suggest_next" => json!({}),
        "assess_risk" => json!({"command": "rm -rf /"}),
        "find_agent_session" => json!({}),
        "replay_agent_session" => json!({"session_id": AGENT_SESSION}),
        "learn_from_failures" => json!({"days": 3650}),
        "project_context" => json!({"days": 3650}),
        "list_skills" => json!({}),
        "get_skill" => json!({"name": "release"}),
        "search_skills" => json!({"query": "release"}),
        // Writes and JSON-shaped session tools are covered separately.
        _ => return None,
    })
}

/// Everything above the trailer, i.e. the rows themselves.
fn body(text: &str) -> &str {
    text.rsplit_once("\n---\n").map_or(text, |(body, _)| body)
}

/// Tools that answer with JSON rather than prose.
const JSON_TOOLS: [&str; 3] = [
    "list_agent_sessions",
    "get_agent_session",
    "resolve_current_agent_session",
];

fn text_responses(repo: &Repository, mcp: &McpConfig) -> Vec<(&'static str, String)> {
    super::catalog::TOOLS
        .iter()
        .filter_map(|entry| {
            let args = tool_args(entry.name)?;
            let text = super::tools::call_tool(repo, entry.name, &args, mcp)
                .unwrap_or_else(|e| panic!("{} failed: {e}", entry.name));
            Some((entry.name, text))
        })
        .collect()
}

// ── 1. One convention, applied everywhere ───────────────────

#[test]
fn every_text_tool_response_carries_the_standard_trailer() {
    let (_dir, repo) = seeded();
    for (name, text) in text_responses(&repo, &mcp()) {
        let trailer = conv::parse_trailer(&text)
            .unwrap_or_else(|| panic!("{name} response has no conventions trailer:\n{text}"));
        assert!(
            trailer.contains_key("shown"),
            "{name} trailer has no shown: line\n{text}"
        );
        assert!(
            trailer.contains_key("next_offset"),
            "{name} trailer has no next_offset: line\n{text}"
        );
        assert!(
            trailer.contains_key("provenance"),
            "{name} trailer has no provenance: line\n{text}"
        );
    }
}

/// Resources serve the same records as the tools do, so they answer in
/// the same shape. A caller that learned the trailer from one surface
/// must not have to learn a second one for the other.
fn resource_responses(repo: &Repository, mcp: &McpConfig) -> Vec<(String, String)> {
    super::catalog::RESOURCES
        .iter()
        .map(super::catalog::ResourceEntry::uri)
        .chain(std::iter::once(format!(
            "suvadu://history/session/{SHELL_SESSION}"
        )))
        .map(|uri| {
            let value = super::resources::read_resource(repo, &uri, mcp)
                .unwrap_or_else(|e| panic!("{uri} failed: {e}"));
            let text = value["contents"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            (uri, text)
        })
        .collect()
}

#[test]
fn every_resource_answers_in_the_same_shape_as_the_tools() {
    let (_dir, repo) = seeded();
    for (uri, text) in resource_responses(&repo, &mcp()) {
        let trailer = conv::parse_trailer(&text)
            .unwrap_or_else(|| panic!("{uri} has no conventions trailer:\n{text}"));
        for key in ["shown", "next_offset", "provenance"] {
            assert!(
                trailer.contains_key(key),
                "{uri} trailer has no {key}: line\n{text}"
            );
        }
        assert!(
            !conv::has_legacy_timestamp(&text),
            "{uri} still prints a space-separated local timestamp:\n{text}"
        );
        assert!(
            !text.contains("[?]") && !text.contains("exit -1"),
            "{uri} uses a placeholder other than `unknown`:\n{text}"
        );
    }
}

#[test]
fn timestamps_are_rfc3339_with_an_explicit_offset() {
    let (_dir, repo) = seeded();
    for (name, text) in text_responses(&repo, &mcp()) {
        assert!(
            !conv::has_legacy_timestamp(&text),
            "{name} still prints a space-separated local timestamp:\n{text}"
        );
    }
}

#[test]
fn unknown_values_are_always_the_word_unknown() {
    let (_dir, repo) = seeded();
    // `vim src/lib.rs` was recorded with no exit code and no executor.
    let text = super::tools::call_tool(
        &repo,
        "search_commands",
        &json!({"query": "vim", "detail": true}),
        &mcp(),
    )
    .unwrap();
    assert!(text.contains("exit unknown"), "{text}");
    assert!(text.contains("executor unknown"), "{text}");
    for (name, text) in text_responses(&repo, &mcp()) {
        assert!(
            !text.contains("[?]") && !text.contains("exit -1"),
            "{name} uses a placeholder other than `unknown`:\n{text}"
        );
    }
}

#[test]
fn every_list_tool_paginates_with_offset_and_terminates() {
    let (_dir, repo) = seeded();
    let mcp = mcp();
    for name in ["search_commands", "recent_commands", "session_history"] {
        let mut args = tool_args(name).unwrap();
        args["limit"] = json!(1);
        let mut seen = Vec::new();
        let mut offset = Some(0_u64);
        let mut pages = 0;
        while let Some(current) = offset {
            args["offset"] = json!(current);
            let text = super::tools::call_tool(&repo, name, &args, &mcp).unwrap();
            seen.extend(conv::command_ids(&text));
            offset = conv::parse_trailer(&text)
                .and_then(|t| t.get("next_offset").cloned())
                .and_then(|value| value.parse::<u64>().ok());
            pages += 1;
            assert!(pages < 20, "{name} pagination did not terminate");
        }
        assert!(pages > 1, "{name} never offered a second page");
        let unique = seen.iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), seen.len(), "{name} repeated a record");
    }
}

// ── 2. Concise defaults, stable IDs, opt-in detail ──────────

#[test]
fn command_rows_carry_stable_ids_and_detail_is_opt_in() {
    let (_dir, repo) = seeded();
    let mcp = mcp();
    let concise =
        super::tools::call_tool(&repo, "search_commands", &json!({"query": "cargo"}), &mcp)
            .unwrap();
    let ids = conv::command_ids(&concise);
    assert!(!ids.is_empty(), "no stable command IDs in:\n{concise}");
    assert!(
        !body(&concise).contains("duration"),
        "the concise default should not carry per-row duration:\n{concise}"
    );
    assert!(
        concise.contains("detail=true"),
        "a concise response must document how to ask for detail:\n{concise}"
    );

    let detailed = super::tools::call_tool(
        &repo,
        "search_commands",
        &json!({"query": "cargo", "detail": true}),
        &mcp,
    )
    .unwrap();
    assert!(body(&detailed).contains("duration"), "{detailed}");
    assert_eq!(conv::command_ids(&detailed), ids, "IDs must be stable");
}

#[test]
fn project_context_is_concise_by_default_and_names_how_to_go_deeper() {
    let (_dir, repo) = seeded();
    let concise =
        super::tools::call_tool(&repo, "project_context", &json!({"days": 3650}), &mcp()).unwrap();
    assert!(
        concise.lines().count() <= 30,
        "the default briefing is a dump ({} lines):\n{concise}",
        concise.lines().count()
    );
    assert!(concise.contains("detail=true"), "{concise}");
}

// ── 3. Observed records vs inferred effects ─────────────────

#[test]
fn what_changed_separates_observed_commands_from_inferred_effects() {
    let (_dir, repo) = seeded();
    let text =
        super::tools::call_tool(&repo, "what_changed", &json!({"hours": 87_600}), &mcp()).unwrap();
    assert!(
        text.to_lowercase().contains("inferred"),
        "what_changed must label its categories as inferred:\n{text}"
    );
    assert!(
        conv::parse_trailer(&text).unwrap()["provenance"].contains("inferred"),
        "{text}"
    );
    assert!(
        text.contains(conv::NO_OUTPUT_NOTE),
        "what_changed must say suvadu never captured the output:\n{text}"
    );
    // `rm -rf ./target` exited 1 — it must not be presented as a deletion
    // that happened.
    assert!(
        text.contains("did not succeed") || text.contains("exit 1"),
        "a failed command was reported as a change that happened:\n{text}"
    );
}

#[test]
fn learn_from_failures_reports_observations_not_root_causes() {
    let (_dir, repo) = seeded();
    let text =
        super::tools::call_tool(&repo, "learn_from_failures", &json!({"days": 3650}), &mcp())
            .unwrap();
    assert!(text.contains("cargo test parser"), "{text}");
    assert!(
        text.contains(conv::NO_OUTPUT_NOTE),
        "failure learning must not imply the error text was captured:\n{text}"
    );
    for forbidden in ["root cause", "because", "caused by", "the fix was"] {
        assert!(
            !text.to_lowercase().contains(forbidden),
            "learn_from_failures claims `{forbidden}` it cannot know:\n{text}"
        );
    }
}

// ── 4. Exclusions and disabled capabilities ─────────────────

#[test]
fn excluded_directories_are_invisible_to_every_tool_and_resource() {
    let (_dir, repo) = seeded();
    let excluded = McpConfig {
        exclude_dirs: vec![SECRET_DIR.to_string()],
        ..mcp()
    };
    for (name, text) in text_responses(&repo, &excluded) {
        assert!(
            !text.contains("topsecret") && !text.contains(SECRET_DIR),
            "{name} leaked an excluded directory:\n{text}"
        );
    }
    // Session rows are records about excluded commands too.
    let sessions = super::tools::call_tool(&repo, "list_sessions", &json!({}), &excluded).unwrap();
    assert!(
        !sessions.contains("shell-secret"),
        "list_sessions exposed a session made entirely of excluded commands:\n{sessions}"
    );
    for entry in super::catalog::RESOURCES {
        let Ok(value) = super::resources::read_resource(&repo, &entry.uri(), &excluded) else {
            continue;
        };
        let text = value.to_string();
        assert!(
            !text.contains("topsecret") && !text.contains(SECRET_DIR),
            "resource {} leaked an excluded directory:\n{text}",
            entry.uri_suffix
        );
    }
}

#[test]
fn disabling_a_tool_also_disables_the_resource_that_mirrors_it() {
    let (_dir, repo) = seeded();
    for entry in super::catalog::RESOURCES {
        let Some(tool) = entry.mirrors_tool else {
            continue;
        };
        let cfg = McpConfig {
            disabled_tools: vec![tool.to_string()],
            ..mcp()
        };
        assert!(
            !super::catalog::resource_available(entry, &cfg),
            "{} stays advertised while {tool} is disabled",
            entry.uri_suffix
        );
        let error = super::resources::read_resource(&repo, &entry.uri(), &cfg)
            .expect_err("a resource mirroring a disabled tool must not be readable");
        assert!(error.contains(tool), "{error}");
    }
}

#[test]
fn every_resource_that_mirrors_a_tool_names_a_real_tool() {
    for entry in super::catalog::RESOURCES {
        if let Some(tool) = entry.mirrors_tool {
            assert!(
                super::catalog::find_tool(tool).is_some(),
                "{} mirrors unknown tool {tool}",
                entry.uri_suffix
            );
        }
    }
}

// ── 5. Malformed and unsupported upstream records ───────────

fn captured_session(repo: &Repository, dir: &std::path::Path, native: &str, cwd: &str) {
    let path = dir.join(format!("{native}.jsonl"));
    let records = [
        json!({"timestamp":"2026-09-19T09:00:00Z","type":"session_meta","payload":{"id":native,"cwd":cwd}}),
        json!({"timestamp":"2026-09-19T09:00:02Z","type":"event_msg","payload":{"type":"user_message","message":"Fix the parser"}}),
        json!({"timestamp":"2026-09-19T09:00:03Z","type":"event_msg","payload":{"type":"agent_message","phase":"final_answer","message":"Done."}}),
    ];
    std::fs::write(
        &path,
        records
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    repo.import_codex_session(&path, Some(native), |_| {
        Ok(crate::ai_sessions::CapturePolicy::default())
    })
    .unwrap();
}

#[test]
fn a_malformed_stored_record_is_reported_not_silently_dropped() {
    let (dir, repo) = crate::test_utils::test_repo();
    captured_session(&repo, dir.path(), "run-1", OPEN_DIR);
    repo.raw_execute_for_test(
        "UPDATE ai_events SET data='{not json' WHERE event_id=(SELECT event_id FROM ai_events LIMIT 1)",
    )
    .unwrap();

    let listed: Value = serde_json::from_str(
        &super::tools::call_tool(&repo, "list_agent_sessions", &json!({}), &mcp()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        listed["sessions"].as_array().unwrap().len(),
        1,
        "one bad record made the whole session vanish from the list: {listed}"
    );

    let read: Value = serde_json::from_str(
        &super::tools::call_tool(
            &repo,
            "get_agent_session",
            &json!({"session_id": "codex-run-1"}),
            &mcp(),
        )
        .expect("one unreadable record must not fail the whole read"),
    )
    .unwrap();
    assert_eq!(read["session"]["capture"]["unreadable_records"], 1);
    assert_eq!(read["session"]["capture"]["complete"], false);
    assert!(
        read["session"]["capture"]["known_missing"]
            .to_string()
            .contains("could not be decoded"),
        "{read}"
    );
}

#[test]
fn an_unsupported_adapter_version_is_declared_on_the_session() {
    let (dir, repo) = crate::test_utils::test_repo();
    captured_session(&repo, dir.path(), "run-2", OPEN_DIR);
    repo.raw_execute_for_test("UPDATE ai_sources SET adapter_version=9999")
        .unwrap();

    let read: Value = serde_json::from_str(
        &super::tools::call_tool(
            &repo,
            "get_agent_session",
            &json!({"session_id": "codex-run-2"}),
            &mcp(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(read["session"]["capture"]["complete"], false);
    let missing = read["session"]["capture"]["known_missing"].to_string();
    assert!(
        missing.contains("9999") && missing.to_lowercase().contains("version"),
        "{missing}"
    );
}

#[test]
fn json_session_responses_declare_their_units_and_provenance() {
    let (dir, repo) = crate::test_utils::test_repo();
    captured_session(&repo, dir.path(), "run-3", OPEN_DIR);
    for (tool, args) in [
        ("list_agent_sessions", json!({})),
        ("get_agent_session", json!({"session_id": "codex-run-3"})),
        ("resolve_current_agent_session", json!({"cwd": OPEN_DIR})),
    ] {
        let value: Value =
            serde_json::from_str(&super::tools::call_tool(&repo, tool, &args, &mcp()).unwrap())
                .unwrap();
        assert_eq!(
            value["provenance"]["time_unit"],
            conv::JSON_TIME_UNIT,
            "{tool} does not declare its time unit: {value}"
        );
        assert!(
            value["provenance"]["note"]
                .as_str()
                .unwrap_or_default()
                .contains(conv::NO_OUTPUT_NOTE),
            "{tool} does not carry the capture note: {value}"
        );
    }
    assert_eq!(JSON_TOOLS.len(), 3);
}

// ── 6. Oversized sessions and long rows ─────────────────────

#[test]
fn long_rows_are_truncated_visibly_and_counted() {
    let (_dir, repo) = crate::test_utils::test_repo();
    session(&repo, SHELL_SESSION);
    let now = chrono::Utc::now().timestamp_millis();
    let long = format!("echo {}", "x".repeat(5_000));
    entry(
        &repo,
        SHELL_SESSION,
        &long,
        OPEN_DIR,
        Some(0),
        now - 1_000,
        Some(("human", "zsh")),
        None,
    );
    for index in 0..40 {
        entry(
            &repo,
            SHELL_SESSION,
            &format!("echo filler-{index}"),
            OPEN_DIR,
            Some(0),
            now - 2_000 - i64::from(index),
            Some(("human", "zsh")),
            None,
        );
    }
    let text =
        super::tools::call_tool(&repo, "recent_commands", &json!({"limit": 5}), &mcp()).unwrap();
    assert!(text.len() < 4_000, "an oversized row was not bounded");
    assert!(text.contains('…'), "truncation was not marked:\n{text}");
    let trailer = conv::parse_trailer(&text).unwrap();
    assert_eq!(trailer["shown"].split_whitespace().next().unwrap(), "5");
    assert_eq!(trailer["next_offset"], "5");
}

// ── 7. Discoverability of session tools ─────────────────────

#[test]
fn session_discovery_and_history_search_point_at_each_other() {
    let definitions = super::tools::list_tools(&json!(1), &mcp());
    let by_name: std::collections::HashMap<&str, &Value> = definitions["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| (tool["name"].as_str().unwrap(), tool))
        .collect();
    let describes = |name: &str| -> String {
        by_name[name]["description"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    // Whole-session questions must send the caller to session discovery…
    assert!(
        describes("search_commands").contains("find_agent_session"),
        "search_commands does not say when a session tool is the right one"
    );
    assert!(
        describes("recent_commands").contains("resolve_current_agent_session"),
        "recent_commands does not point at session discovery"
    );
    // …and the session tools must say when plain history search is better.
    for tool in [
        "find_agent_session",
        "list_agent_sessions",
        "resolve_current_agent_session",
    ] {
        assert!(
            describes(tool).contains("search_commands"),
            "{tool} does not say when to search history instead"
        );
    }
    // The distinction between the two session-discovery tools is stated.
    assert!(
        describes("find_agent_session").contains("shell commands"),
        "find_agent_session does not say it is built from recorded shell commands"
    );
    assert!(
        describes("list_agent_sessions").contains("transcript"),
        "list_agent_sessions does not say it lists captured transcripts"
    );
}
