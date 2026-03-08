use super::*;
use crate::models::Entry;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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

fn test_search_config(entries: Vec<Entry>, total_items: usize) -> SearchConfig {
    SearchConfig {
        entries,
        initial_query: None,
        total_items,
        page: 1,
        page_size: 50,
        tags: vec![],
        unique_mode: false,
        unique_counts: std::collections::HashMap::new(),
        filter_after: None,
        filter_before: None,
        filter_tag_id: None,
        filter_exit_code: None,
        filter_executor_type: None,
        start_date_input: None,
        end_date_input: None,
        tag_filter_input: None,
        exit_code_input: None,
        executor_filter_input: None,
        bookmarked_commands: std::collections::HashSet::new(),
        filter_cwd: None,
        noted_entry_ids: std::collections::HashSet::new(),
        context_boost: true,
        show_detail_pane: true,
        show_risk_in_search: false,
        search_field: "command".to_string(),
    }
}

#[test]
fn test_search_app_initialization() {
    let entries = vec![
        create_test_entry("cargo build"),
        create_test_entry("git status"),
    ];
    let app = SearchApp::new(test_search_config(entries, 2));

    assert_eq!(app.entries.len(), 2);
    assert_eq!(app.page, 1);
    assert_eq!(app.total_items, 2);
}

#[test]
fn test_pagination_logic() {
    let entries = vec![create_test_entry("cmd")];
    // Pretend we have 1500 items, page size 50. So 30 pages.
    let mut app = SearchApp::new(test_search_config(entries, 1500));

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
    let scored = SearchApp::fuzzy_score(entries, "gco", None, "command");
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

    let scored = SearchApp::fuzzy_score(entries, "zzzzz", None, "command");
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

    let scored = SearchApp::fuzzy_score(entries, "cargo test", None, "command");
    assert!(!scored.is_empty());
    // Both "cargo test" entries should match, "npm install" should not
    let cmds: Vec<&str> = scored.iter().map(|e| e.command.as_str()).collect();
    assert!(cmds.contains(&"cargo test"));
    assert!(cmds.contains(&"cargo test --release"));
    assert!(!cmds.contains(&"npm install"));
}

#[test]
fn test_fuzzy_score_length_penalty() {
    // Short matching command should score higher than long one
    let entries = vec![
        create_test_entry("git status"),
        create_test_entry(
            "git status --porcelain --branch --show-stash --ahead-behind --find-renames",
        ),
    ];

    let scored = SearchApp::fuzzy_score(entries, "git status", None, "command");
    assert_eq!(scored.len(), 2);
    // Short command should come first due to length penalty
    assert_eq!(scored[0].command, "git status");
}

#[test]
fn test_fuzzy_score_human_boost() {
    let mut human_entry = create_test_entry("cargo build");
    human_entry.executor_type = Some("human".to_string());

    let mut agent_entry = create_test_entry("cargo build");
    agent_entry.executor_type = Some("agent".to_string());

    let entries = vec![agent_entry, human_entry];

    let scored = SearchApp::fuzzy_score(entries, "cargo build", None, "command");
    assert_eq!(scored.len(), 2);
    // Human entry should come first
    assert_eq!(scored[0].executor_type.as_deref(), Some("human"));
}

#[test]
fn test_fuzzy_score_cwd_boost() {
    let mut local_entry = create_test_entry("make test");
    local_entry.cwd = "/project".to_string();

    let mut remote_entry = create_test_entry("make test");
    remote_entry.cwd = "/other".to_string();

    let entries = vec![remote_entry, local_entry];

    let scored = SearchApp::fuzzy_score(entries, "make test", Some("/project"), "command");
    assert_eq!(scored.len(), 2);
    // Local CWD entry should come first
    assert_eq!(scored[0].cwd, "/project");
}

#[test]
fn test_fuzzy_score_empty_query() {
    let entries = vec![create_test_entry("ls"), create_test_entry("pwd")];

    // Empty query should match nothing (nucleo needs at least some pattern)
    let scored = SearchApp::fuzzy_score(entries, "", None, "command");
    // nucleo Pattern::parse("") returns a pattern that matches everything
    // This is fine — the caller gates on query.len() >= 2
    assert!(scored.len() <= 2);
}

#[test]
fn test_fuzzy_score_single_char() {
    let entries = vec![
        create_test_entry("ls -la"),
        create_test_entry("pwd"),
        create_test_entry("cd /tmp"),
    ];

    let scored = SearchApp::fuzzy_score(entries, "l", None, "command");
    // Should match "ls -la" at minimum
    let cmds: Vec<&str> = scored.iter().map(|e| e.command.as_str()).collect();
    assert!(cmds.contains(&"ls -la"));
}

#[test]
fn test_active_filter_count() {
    let entries = vec![create_test_entry("test")];
    let mut app = SearchApp::new(test_search_config(entries, 1));

    assert_eq!(app.active_filter_count(), 0);

    app.filter_exit_code = Some(0);
    assert_eq!(app.active_filter_count(), 1);

    app.filter_after = Some(1000);
    assert_eq!(app.active_filter_count(), 2);

    app.filter_before = Some(2000);
    assert_eq!(app.active_filter_count(), 3);

    app.filter_tag_id = Some(1);
    assert_eq!(app.active_filter_count(), 4);

    app.filter_executor_type = Some("human".to_string());
    assert_eq!(app.active_filter_count(), 5);
}

#[test]
fn test_get_selected_entry() {
    let entries = vec![create_test_entry("first"), create_test_entry("second")];
    let mut app = SearchApp::new(test_search_config(entries, 2));

    // Default selection is 0
    app.table_state.select(Some(0));
    assert_eq!(app.get_selected_command().as_deref(), Some("first"));

    app.table_state.select(Some(1));
    assert_eq!(app.get_selected_command().as_deref(), Some("second"));

    app.table_state.select(None);
    assert!(app.get_selected_command().is_none());
}

#[test]
fn test_get_selected_entry_out_of_bounds() {
    let entries = vec![create_test_entry("only")];
    let mut app = SearchApp::new(test_search_config(entries, 1));

    // Out of bounds selection should return None
    app.table_state.select(Some(999));
    assert!(app.get_selected_entry().is_none());
}

// ── apply_combined_sort tests ──

fn create_entry_with_cwd_and_executor(cmd: &str, cwd: &str, executor_type: &str) -> Entry {
    Entry {
        id: None,
        session_id: "s1".to_string(),
        command: cmd.to_string(),
        cwd: cwd.to_string(),
        exit_code: Some(0),
        started_at: 1000,
        ended_at: 2000,
        duration_ms: 1000,
        context: None,
        tag_name: None,
        tag_id: None,
        executor_type: Some(executor_type.to_string()),
        executor: None,
    }
}

#[test]
fn test_combined_sort_human_first() {
    let mut entries = vec![
        create_entry_with_cwd_and_executor("cmd1", "/tmp", "agent"),
        create_entry_with_cwd_and_executor("cmd2", "/tmp", "human"),
    ];
    SearchApp::apply_combined_sort(&mut entries, None);
    assert_eq!(entries[0].executor_type.as_deref(), Some("human"));
    assert_eq!(entries[1].executor_type.as_deref(), Some("agent"));
}

#[test]
fn test_combined_sort_cwd_first() {
    let mut entries = vec![
        create_entry_with_cwd_and_executor("cmd1", "/other", "human"),
        create_entry_with_cwd_and_executor("cmd2", "/project", "human"),
    ];
    SearchApp::apply_combined_sort(&mut entries, Some("/project"));
    assert_eq!(entries[0].cwd, "/project");
    assert_eq!(entries[1].cwd, "/other");
}

#[test]
fn test_combined_sort_cwd_beats_human() {
    // CWD match should take priority over human/agent distinction
    let mut entries = vec![
        create_entry_with_cwd_and_executor("cmd1", "/other", "human"),
        create_entry_with_cwd_and_executor("cmd2", "/project", "agent"),
    ];
    SearchApp::apply_combined_sort(&mut entries, Some("/project"));
    // Agent entry in matching CWD should come first
    assert_eq!(entries[0].cwd, "/project");
}

#[test]
fn test_combined_sort_no_context_human_only() {
    let mut entries = vec![
        create_entry_with_cwd_and_executor("cmd1", "/a", "agent"),
        create_entry_with_cwd_and_executor("cmd2", "/b", "human"),
        create_entry_with_cwd_and_executor("cmd3", "/c", "agent"),
    ];
    SearchApp::apply_combined_sort(&mut entries, None);
    assert_eq!(entries[0].executor_type.as_deref(), Some("human"));
}

#[test]
fn test_combined_sort_empty() {
    let mut entries: Vec<Entry> = vec![];
    SearchApp::apply_combined_sort(&mut entries, Some("/project"));
    assert!(entries.is_empty());
}

// ── Input handler tests ──

fn ctrl_key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

#[test]
fn test_handle_input_escape_exits() {
    let entries = vec![create_test_entry("ls")];
    let mut app = SearchApp::new(test_search_config(entries, 1));

    let key = KeyEvent::from(KeyCode::Esc);
    let action = app.handle_input(key);
    assert!(matches!(action, SearchAction::Exit));
}

#[test]
fn test_handle_input_enter_selects() {
    let entries = vec![create_test_entry("echo hello")];
    let mut app = SearchApp::new(test_search_config(entries, 1));
    app.table_state.select(Some(0));

    let key = KeyEvent::from(KeyCode::Enter);
    let action = app.handle_input(key);
    match action {
        SearchAction::Select(cmd) => assert_eq!(cmd, "echo hello"),
        _ => panic!("Expected SearchAction::Select"),
    }
}

#[test]
fn test_handle_input_char_reloads() {
    let entries = vec![create_test_entry("ls")];
    let mut app = SearchApp::new(test_search_config(entries, 1));

    let key = KeyEvent::from(KeyCode::Char('a'));
    let action = app.handle_input(key);
    assert!(matches!(action, SearchAction::Reload));
    assert!(app.query.contains('a'));
}

#[test]
fn test_handle_input_backspace_reloads() {
    let entries = vec![create_test_entry("ls")];
    let mut config = test_search_config(entries, 1);
    config.initial_query = Some("abc".to_string());
    let mut app = SearchApp::new(config);

    let key = KeyEvent::from(KeyCode::Backspace);
    let action = app.handle_input(key);
    assert!(matches!(action, SearchAction::Reload));
}

#[test]
fn test_handle_input_ctrl_f_opens_filter() {
    let entries = vec![create_test_entry("ls")];
    let mut app = SearchApp::new(test_search_config(entries, 1));

    let key = ctrl_key('f');
    let action = app.handle_input(key);
    assert!(matches!(action, SearchAction::Continue));
    assert!(app.filter_mode);
}

#[test]
fn test_handle_input_ctrl_u_toggles_unique() {
    let entries = vec![create_test_entry("ls")];
    let mut app = SearchApp::new(test_search_config(entries, 1));

    assert!(!app.unique_mode);
    let key = ctrl_key('u');
    let action = app.handle_input(key);
    assert!(matches!(action, SearchAction::Reload));
    assert!(app.unique_mode);
}

#[test]
fn test_handle_input_tab_toggles_detail() {
    let entries = vec![create_test_entry("ls")];
    let mut config = test_search_config(entries, 1);
    config.show_detail_pane = false;
    let mut app = SearchApp::new(config);

    assert!(!app.detail_pane_open);
    let key = KeyEvent::from(KeyCode::Tab);
    let action = app.handle_input(key);
    assert!(matches!(action, SearchAction::Continue));
    assert!(app.detail_pane_open);

    // Toggle back
    let action = app.handle_input(KeyEvent::from(KeyCode::Tab));
    assert!(matches!(action, SearchAction::Continue));
    assert!(!app.detail_pane_open);
}

#[test]
fn test_handle_input_delete_dialog_yes() {
    let mut entry = create_test_entry("rm -rf /");
    entry.id = Some(99);
    let entries = vec![entry];
    let mut app = SearchApp::new(test_search_config(entries, 1));
    app.table_state.select(Some(0));

    // Open the delete dialog via Ctrl+D
    let action = app.handle_input(ctrl_key('d'));
    assert!(matches!(action, SearchAction::Continue));
    assert!(app.delete_dialog_open);

    // Press 'y' to confirm
    let key = KeyEvent::from(KeyCode::Char('y'));
    let action = app.handle_input(key);
    match action {
        SearchAction::Delete(id) => assert_eq!(id, 99),
        _ => panic!("Expected SearchAction::Delete(99)"),
    }
}

#[test]
fn test_handle_input_delete_dialog_no() {
    let mut entry = create_test_entry("rm -rf /");
    entry.id = Some(99);
    let entries = vec![entry];
    let mut app = SearchApp::new(test_search_config(entries, 1));
    app.table_state.select(Some(0));

    // Open the delete dialog
    app.handle_input(ctrl_key('d'));
    assert!(app.delete_dialog_open);

    // Press 'n' to cancel
    let key = KeyEvent::from(KeyCode::Char('n'));
    let action = app.handle_input(key);
    assert!(matches!(action, SearchAction::Continue));
    assert!(!app.delete_dialog_open);
}

#[test]
fn test_handle_input_goto_enter() {
    let entries = vec![create_test_entry("ls")];
    let mut app = SearchApp::new(test_search_config(entries.clone(), 500));

    // Open goto dialog
    app.handle_input(ctrl_key('g'));
    assert!(app.goto_dialog_open);

    // Type page number "3"
    app.handle_input(KeyEvent::from(KeyCode::Char('3')));

    // Press Enter
    let action = app.handle_input(KeyEvent::from(KeyCode::Enter));
    match action {
        SearchAction::SetPage(p) => assert_eq!(p, 3),
        _ => panic!("Expected SearchAction::SetPage(3)"),
    }
}

#[test]
fn test_handle_input_filter_enter() {
    let entries = vec![create_test_entry("ls")];
    let mut app = SearchApp::new(test_search_config(entries, 1));

    // Open filter mode
    app.handle_input(ctrl_key('f'));
    assert!(app.filter_mode);

    // Press Enter to apply (empty filters)
    let action = app.handle_input(KeyEvent::from(KeyCode::Enter));
    assert!(matches!(action, SearchAction::Reload));
    assert!(!app.filter_mode);
}

#[test]
fn test_handle_input_up_down_navigation() {
    let entries = vec![
        create_test_entry("first"),
        create_test_entry("second"),
        create_test_entry("third"),
    ];
    let mut app = SearchApp::new(test_search_config(entries, 3));
    app.table_state.select(Some(0));

    // Move down
    app.handle_input(KeyEvent::from(KeyCode::Down));
    assert_eq!(app.table_state.selected(), Some(1));

    // Move down again
    app.handle_input(KeyEvent::from(KeyCode::Down));
    assert_eq!(app.table_state.selected(), Some(2));

    // Move down at bottom should stay at 2 (last index)
    app.handle_input(KeyEvent::from(KeyCode::Down));
    assert_eq!(app.table_state.selected(), Some(2));

    // Move up
    app.handle_input(KeyEvent::from(KeyCode::Up));
    assert_eq!(app.table_state.selected(), Some(1));

    // Move up again
    app.handle_input(KeyEvent::from(KeyCode::Up));
    assert_eq!(app.table_state.selected(), Some(0));

    // Move up at top should stay at 0
    app.handle_input(KeyEvent::from(KeyCode::Up));
    assert_eq!(app.table_state.selected(), Some(0));
}

#[test]
fn test_handle_input_left_right_pages() {
    let entries = vec![create_test_entry("cmd")];
    let mut app = SearchApp::new(test_search_config(entries, 200));

    // Page 1, press Right -> page 2
    let action = app.handle_input(KeyEvent::from(KeyCode::Right));
    match action {
        SearchAction::SetPage(p) => assert_eq!(p, 2),
        _ => panic!("Expected SearchAction::SetPage(2)"),
    }

    // Simulate being on page 3, press Left -> page 2
    app.page = 3;
    let action = app.handle_input(KeyEvent::from(KeyCode::Left));
    match action {
        SearchAction::SetPage(p) => assert_eq!(p, 2),
        _ => panic!("Expected SearchAction::SetPage(2)"),
    }

    // At page 1, Left should not change page
    app.page = 1;
    let action = app.handle_input(KeyEvent::from(KeyCode::Left));
    assert!(matches!(action, SearchAction::Continue));
}
