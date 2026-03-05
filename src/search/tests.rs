use super::*;
use crate::models::Entry;
use crossterm::event::{KeyCode, KeyEvent};

fn create_test_entry(cmd: &str) -> Entry {
    Entry {
        id: None,
        session_id: "session123".to_string(),
        command: cmd.to_string(),
        cwd: "/tmp".to_string(),
        exit_code: Some(0),
        started_at: 1000,
        ended_at: 2000,
        duration_ms: 1000,
        context: None,
        tag_name: None,
        tag_id: None,
        executor_type: Some("human".to_string()),
        executor: Some("terminal".to_string()),
    }
}

#[test]
fn test_search_app_initialization() {
    let entries = vec![
        create_test_entry("cargo build"),
        create_test_entry("git status"),
    ];
    let app = SearchApp::new(
        entries.clone(),
        None,
        2,
        1,
        50,
        vec![],
        false,
        std::collections::HashMap::new(),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        std::collections::HashSet::new(),
        None,
        std::collections::HashSet::new(),
        true,
        true,
        false,
    );

    assert_eq!(app.entries.len(), 2);
    assert_eq!(app.page, 1);
    assert_eq!(app.total_items, 2);
}

#[test]
fn test_pagination_logic() {
    let entries = vec![create_test_entry("cmd")];
    // Pretend we have 1500 items, page size 50. So 30 pages.
    let mut app = SearchApp::new(
        entries,
        None,
        1500,
        1,
        50,
        vec![],
        false,
        std::collections::HashMap::new(),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        std::collections::HashSet::new(),
        None,
        std::collections::HashSet::new(),
        true,
        true,
        false,
    );

    // Next page
    let key = KeyEvent::from(KeyCode::Right);
    let action = app.handle_input(key);
    match action {
        SearchAction::SetPage(p) => assert_eq!(p, 2),
        _ => panic!("Expected SetPage(2)"),
    }

    // Prev page (from page 2)
    app.page = 2;
    let key = KeyEvent::from(KeyCode::Left);
    let action = app.handle_input(key);
    match action {
        SearchAction::SetPage(p) => assert_eq!(p, 1),
        _ => panic!("Expected SetPage(1)"),
    }
}

#[test]
fn test_fuzzy_score_ranking() {
    let entries = vec![
        create_test_entry("git checkout main"),
        create_test_entry("echo hello world"),
        create_test_entry("git commit -m 'fix'"),
        create_test_entry("cargo build"),
    ];

    // "gco" should match git commands but not "echo" or "cargo build"
    let scored = SearchApp::fuzzy_score(entries, "gco", None);
    assert!(!scored.is_empty());
    // Both git commands should match, non-git commands should not
    let cmds: Vec<&str> = scored.iter().map(|e| e.command.as_str()).collect();
    assert!(cmds.contains(&"git checkout main"));
    assert!(cmds.contains(&"git commit -m 'fix'"));
    assert!(!cmds.contains(&"cargo build"));
}

#[test]
fn test_fuzzy_score_no_match() {
    let entries = vec![create_test_entry("ls -la"), create_test_entry("pwd")];

    let scored = SearchApp::fuzzy_score(entries, "zzzzz", None);
    assert!(scored.is_empty());
}

#[test]
fn test_fuzzy_score_filters_irrelevant() {
    let entries = vec![
        create_test_entry("cargo test --release"),
        create_test_entry("cargo build"),
        create_test_entry("npm install"),
        create_test_entry("cargo test"),
    ];

    let scored = SearchApp::fuzzy_score(entries, "cargo test", None);
    assert!(!scored.is_empty());
    // Both "cargo test" entries should match, "npm install" should not
    let cmds: Vec<&str> = scored.iter().map(|e| e.command.as_str()).collect();
    assert!(cmds.contains(&"cargo test"));
    assert!(cmds.contains(&"cargo test --release"));
    assert!(!cmds.contains(&"npm install"));
}
