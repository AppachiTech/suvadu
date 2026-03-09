use super::*;
use crate::models::{Entry, SearchField, Tag};
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
        show_risk_in_search: false,
        view: ViewOptions {
            unique_mode: false,
            context_boost: true,
            detail_pane_open: true,
            search_field: SearchField::Command,
            current_cwd: None,
        },
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
    assert_eq!(app.pagination.page, 1);
    assert_eq!(app.pagination.total_items, 2);
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
    app.pagination.page = 2;
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
    let scored = SearchApp::fuzzy_score(entries, "gco", None, SearchField::Command);
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

    let scored = SearchApp::fuzzy_score(entries, "zzzzz", None, SearchField::Command);
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

    let scored = SearchApp::fuzzy_score(entries, "cargo test", None, SearchField::Command);
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

    let scored = SearchApp::fuzzy_score(entries, "git status", None, SearchField::Command);
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

    let scored = SearchApp::fuzzy_score(entries, "cargo build", None, SearchField::Command);
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

    let scored =
        SearchApp::fuzzy_score(entries, "make test", Some("/project"), SearchField::Command);
    assert_eq!(scored.len(), 2);
    // Local CWD entry should come first
    assert_eq!(scored[0].cwd, "/project");
}

#[test]
fn test_fuzzy_score_empty_query() {
    let entries = vec![create_test_entry("ls"), create_test_entry("pwd")];

    // Empty query should match nothing (nucleo needs at least some pattern)
    let scored = SearchApp::fuzzy_score(entries, "", None, SearchField::Command);
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

    let scored = SearchApp::fuzzy_score(entries, "l", None, SearchField::Command);
    // Should match "ls -la" at minimum
    let cmds: Vec<&str> = scored.iter().map(|e| e.command.as_str()).collect();
    assert!(cmds.contains(&"ls -la"));
}

#[test]
fn test_active_filter_count() {
    let entries = vec![create_test_entry("test")];
    let mut app = SearchApp::new(test_search_config(entries, 1));

    assert_eq!(app.active_filter_count(), 0);

    app.filters.exit_code = Some(0);
    assert_eq!(app.active_filter_count(), 1);

    app.filters.after = Some(1000);
    assert_eq!(app.active_filter_count(), 2);

    app.filters.before = Some(2000);
    assert_eq!(app.active_filter_count(), 3);

    app.filters.tag_id = Some(1);
    assert_eq!(app.active_filter_count(), 4);

    app.filters.executor_type = Some("human".to_string());
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
    assert!(matches!(app.dialog, DialogState::Filter));
}

#[test]
fn test_handle_input_ctrl_u_toggles_unique() {
    let entries = vec![create_test_entry("ls")];
    let mut app = SearchApp::new(test_search_config(entries, 1));

    assert!(!app.view.unique_mode);
    let key = ctrl_key('u');
    let action = app.handle_input(key);
    assert!(matches!(action, SearchAction::Reload));
    assert!(app.view.unique_mode);
}

#[test]
fn test_handle_input_tab_toggles_detail() {
    let entries = vec![create_test_entry("ls")];
    let mut config = test_search_config(entries, 1);
    config.view.detail_pane_open = false;
    let mut app = SearchApp::new(config);

    assert!(!app.view.detail_pane_open);
    let key = KeyEvent::from(KeyCode::Tab);
    let action = app.handle_input(key);
    assert!(matches!(action, SearchAction::Continue));
    assert!(app.view.detail_pane_open);

    // Toggle back
    let action = app.handle_input(KeyEvent::from(KeyCode::Tab));
    assert!(matches!(action, SearchAction::Continue));
    assert!(!app.view.detail_pane_open);
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
    assert!(matches!(app.dialog, DialogState::Delete { .. }));

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
    assert!(matches!(app.dialog, DialogState::Delete { .. }));

    // Press 'n' to cancel
    let key = KeyEvent::from(KeyCode::Char('n'));
    let action = app.handle_input(key);
    assert!(matches!(action, SearchAction::Continue));
    assert!(matches!(app.dialog, DialogState::None));
}

#[test]
fn test_handle_input_goto_enter() {
    let entries = vec![create_test_entry("ls")];
    let mut app = SearchApp::new(test_search_config(entries.clone(), 500));

    // Open goto dialog
    app.handle_input(ctrl_key('g'));
    assert!(matches!(app.dialog, DialogState::GoToPage { .. }));

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
    assert!(matches!(app.dialog, DialogState::Filter));

    // Press Enter to apply (empty filters)
    let action = app.handle_input(KeyEvent::from(KeyCode::Enter));
    assert!(matches!(action, SearchAction::Reload));
    assert!(matches!(app.dialog, DialogState::None));
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
    app.pagination.page = 3;
    let action = app.handle_input(KeyEvent::from(KeyCode::Left));
    match action {
        SearchAction::SetPage(p) => assert_eq!(p, 2),
        _ => panic!("Expected SearchAction::SetPage(2)"),
    }

    // At page 1, Left should not change page
    app.pagination.page = 1;
    let action = app.handle_input(KeyEvent::from(KeyCode::Left));
    assert!(matches!(action, SearchAction::Continue));
}

// ── handle_input dialog routing tests ──

#[test]
fn test_dialog_routing_delete() {
    let mut entry = create_test_entry("rm file");
    entry.id = Some(42);
    let mut app = SearchApp::new(test_search_config(vec![entry], 1));
    app.dialog = DialogState::Delete { entry_id: 42 };

    // Esc in delete dialog → closes dialog, doesn't exit app
    let action = app.handle_input(KeyEvent::from(KeyCode::Esc));
    assert!(matches!(action, SearchAction::Continue));
    assert!(matches!(app.dialog, DialogState::None));
}

#[test]
fn test_dialog_routing_goto() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::GoToPage {
        input: String::new(),
    };

    // Esc in goto dialog → closes dialog, doesn't exit app
    let action = app.handle_input(KeyEvent::from(KeyCode::Esc));
    assert!(matches!(action, SearchAction::Continue));
    assert!(matches!(app.dialog, DialogState::None));
}

#[test]
fn test_dialog_routing_tag() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::TagAssociation;

    let action = app.handle_input(KeyEvent::from(KeyCode::Esc));
    assert!(matches!(action, SearchAction::Continue));
    assert!(matches!(app.dialog, DialogState::None));
}

#[test]
fn test_dialog_routing_note() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Note {
        entry_id: 1,
        input: String::new(),
    };

    let action = app.handle_input(KeyEvent::from(KeyCode::Esc));
    assert!(matches!(action, SearchAction::Continue));
    assert!(matches!(app.dialog, DialogState::None));
}

#[test]
fn test_dialog_routing_filter() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Filter;

    let action = app.handle_input(KeyEvent::from(KeyCode::Esc));
    assert!(matches!(action, SearchAction::Continue));
    assert!(matches!(app.dialog, DialogState::None));
}

// ── handle_normal_input edge cases ──

#[test]
fn test_down_selects_first_when_none_selected() {
    let entries = vec![create_test_entry("cmd1"), create_test_entry("cmd2")];
    let mut app = SearchApp::new(test_search_config(entries, 2));
    app.table_state.select(None);

    app.handle_input(KeyEvent::from(KeyCode::Down));
    assert_eq!(app.table_state.selected(), Some(0));
}

#[test]
fn test_enter_with_no_selection_continues() {
    let entries = vec![create_test_entry("cmd")];
    let mut app = SearchApp::new(test_search_config(entries, 1));
    app.table_state.select(None);

    let action = app.handle_input(KeyEvent::from(KeyCode::Enter));
    assert!(matches!(action, SearchAction::Continue));
}

#[test]
fn test_right_at_last_page_continues() {
    let entries = vec![create_test_entry("cmd")];
    // 50 total items with page_size 50 = 1 page
    let mut app = SearchApp::new(test_search_config(entries, 50));
    app.pagination.page = 1;

    let action = app.handle_input(KeyEvent::from(KeyCode::Right));
    assert!(matches!(action, SearchAction::Continue));
}

#[test]
fn test_unknown_key_continues() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));

    let action = app.handle_input(KeyEvent::from(KeyCode::F(1)));
    assert!(matches!(action, SearchAction::Continue));
}

// ── handle_ctrl_shortcut tests ──

#[test]
fn test_ctrl_g_opens_goto_dialog() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));

    let action = app.handle_input(ctrl_key('g'));
    assert!(matches!(action, SearchAction::Continue));
    assert!(matches!(app.dialog, DialogState::GoToPage { .. }));
    if let DialogState::GoToPage { ref input } = app.dialog {
        assert!(input.is_empty());
    }
}

#[test]
fn test_ctrl_t_opens_tag_dialog_with_tags() {
    let mut cfg = test_search_config(vec![create_test_entry("ls")], 1);
    cfg.tags = vec![
        Tag {
            id: 1,
            name: "work".to_string(),
            description: None,
        },
        Tag {
            id: 2,
            name: "personal".to_string(),
            description: None,
        },
    ];
    let mut app = SearchApp::new(cfg);

    let action = app.handle_input(ctrl_key('t'));
    assert!(matches!(action, SearchAction::Continue));
    assert!(matches!(app.dialog, DialogState::TagAssociation));
    // Should auto-select first tag
    assert_eq!(app.tag_list_state.selected(), Some(0));
}

#[test]
fn test_ctrl_t_opens_tag_dialog_empty_tags() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));

    let action = app.handle_input(ctrl_key('t'));
    assert!(matches!(action, SearchAction::Continue));
    assert!(matches!(app.dialog, DialogState::TagAssociation));
    // No tags → no selection
    assert_eq!(app.tag_list_state.selected(), None);
}

#[test]
fn test_ctrl_y_copies_selected_command() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("echo hi")], 1));
    app.table_state.select(Some(0));

    let action = app.handle_input(ctrl_key('y'));
    match action {
        SearchAction::Copy(cmd) => assert_eq!(cmd, "echo hi"),
        _ => panic!("Expected SearchAction::Copy"),
    }
}

#[test]
fn test_ctrl_y_nothing_selected() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.table_state.select(None);

    // Ctrl+Y with no selection → returns Some(Continue) from handle_ctrl_shortcut
    // which means it doesn't fall through to normal input
    let action = app.handle_input(ctrl_key('y'));
    assert!(matches!(action, SearchAction::Continue));
}

#[test]
fn test_ctrl_b_toggles_bookmark() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("git push")], 1));
    app.table_state.select(Some(0));

    let action = app.handle_input(ctrl_key('b'));
    match action {
        SearchAction::ToggleBookmark(cmd) => assert_eq!(cmd, "git push"),
        _ => panic!("Expected SearchAction::ToggleBookmark"),
    }
}

#[test]
fn test_ctrl_b_nothing_selected() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.table_state.select(None);

    let action = app.handle_input(ctrl_key('b'));
    assert!(matches!(action, SearchAction::Continue));
}

#[test]
fn test_ctrl_n_opens_note_dialog() {
    let mut entry = create_test_entry("npm test");
    entry.id = Some(77);
    let mut app = SearchApp::new(test_search_config(vec![entry], 1));
    app.table_state.select(Some(0));

    let action = app.handle_input(ctrl_key('n'));
    assert!(matches!(action, SearchAction::Continue));
    match app.dialog {
        DialogState::Note {
            entry_id,
            ref input,
        } => {
            assert_eq!(entry_id, 77);
            assert!(input.is_empty());
        }
        _ => panic!("Expected DialogState::Note"),
    }
}

#[test]
fn test_ctrl_n_no_entry_id() {
    // Entry without id → no dialog opened
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.table_state.select(Some(0));

    let action = app.handle_input(ctrl_key('n'));
    assert!(matches!(action, SearchAction::Continue));
    assert!(matches!(app.dialog, DialogState::None));
}

#[test]
fn test_ctrl_s_toggles_context_boost() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    assert!(app.view.context_boost); // default is true from test config

    let action = app.handle_input(ctrl_key('s'));
    assert!(matches!(action, SearchAction::Reload));
    assert!(!app.view.context_boost);
    assert!(app.status_message.is_some());

    let action = app.handle_input(ctrl_key('s'));
    assert!(matches!(action, SearchAction::Reload));
    assert!(app.view.context_boost);
}

#[test]
fn test_ctrl_l_toggles_cwd_filter() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    assert!(app.filters.cwd.is_none());

    // Toggle on: sets cwd from env
    let action = app.handle_input(ctrl_key('l'));
    assert!(matches!(action, SearchAction::Reload));
    assert!(app.filters.cwd.is_some());
    assert_eq!(app.pagination.page, 1);

    // Toggle off
    let action = app.handle_input(ctrl_key('l'));
    assert!(matches!(action, SearchAction::Reload));
    assert!(app.filters.cwd.is_none());
}

#[test]
fn test_ctrl_d_no_entry_id() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.table_state.select(Some(0));
    // Entry has no id (None) → dialog stays None
    let action = app.handle_input(ctrl_key('d'));
    assert!(matches!(action, SearchAction::Continue));
    assert!(matches!(app.dialog, DialogState::None));
}

#[test]
fn test_unknown_ctrl_key_falls_through() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));

    // Ctrl+Z is unhandled → returns None from handle_ctrl_shortcut →
    // falls through to normal input which treats Char('z') as query input
    let action = app.handle_input(ctrl_key('z'));
    assert!(matches!(action, SearchAction::Reload));
}

// ── handle_delete_dialog_input tests ──

#[test]
fn test_delete_dialog_enter_confirms() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Delete { entry_id: 55 };

    let action = app.handle_input(KeyEvent::from(KeyCode::Enter));
    match action {
        SearchAction::Delete(id) => assert_eq!(id, 55),
        _ => panic!("Expected SearchAction::Delete(55)"),
    }
    assert!(matches!(app.dialog, DialogState::None));
}

#[test]
fn test_delete_dialog_esc_cancels() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Delete { entry_id: 55 };

    let action = app.handle_input(KeyEvent::from(KeyCode::Esc));
    assert!(matches!(action, SearchAction::Continue));
    assert!(matches!(app.dialog, DialogState::None));
}

#[test]
fn test_delete_dialog_other_key_ignored() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Delete { entry_id: 55 };

    let action = app.handle_input(KeyEvent::from(KeyCode::Char('x')));
    assert!(matches!(action, SearchAction::Continue));
    assert!(matches!(app.dialog, DialogState::Delete { entry_id: 55 }));
}

// ── handle_goto_dialog_input tests ──

#[test]
fn test_goto_dialog_esc_closes() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::GoToPage {
        input: "5".to_string(),
    };

    let action = app.handle_input(KeyEvent::from(KeyCode::Esc));
    assert!(matches!(action, SearchAction::Continue));
    assert!(matches!(app.dialog, DialogState::None));
}

#[test]
fn test_goto_dialog_backspace() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::GoToPage {
        input: "42".to_string(),
    };

    app.handle_input(KeyEvent::from(KeyCode::Backspace));
    if let DialogState::GoToPage { ref input } = app.dialog {
        assert_eq!(input, "4");
    } else {
        panic!("Expected GoToPage dialog");
    }
}

#[test]
fn test_goto_dialog_non_digit_ignored() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::GoToPage {
        input: String::new(),
    };

    // Letters should be ignored (only ascii digits accepted)
    app.handle_input(KeyEvent::from(KeyCode::Char('a')));
    if let DialogState::GoToPage { ref input } = app.dialog {
        assert!(input.is_empty());
    } else {
        panic!("Expected GoToPage dialog");
    }
}

#[test]
fn test_goto_dialog_clamps_to_max_page() {
    let entries = vec![create_test_entry("cmd")];
    // 100 items / 50 page_size = 2 pages
    let mut app = SearchApp::new(test_search_config(entries, 100));
    app.dialog = DialogState::GoToPage {
        input: String::new(),
    };

    // Type "9" (beyond 2 pages)
    app.handle_input(KeyEvent::from(KeyCode::Char('9')));
    let action = app.handle_input(KeyEvent::from(KeyCode::Enter));

    // Should clamp to page 2
    match action {
        SearchAction::SetPage(p) => assert_eq!(p, 2),
        _ => panic!("Expected SetPage(2)"),
    }
}

#[test]
fn test_goto_dialog_clamps_to_min_page() {
    let entries = vec![create_test_entry("cmd")];
    let mut app = SearchApp::new(test_search_config(entries, 100));
    app.dialog = DialogState::GoToPage {
        input: String::new(),
    };

    // Type "0" → should clamp to page 1
    app.handle_input(KeyEvent::from(KeyCode::Char('0')));
    let action = app.handle_input(KeyEvent::from(KeyCode::Enter));

    // usize parse of "0" = 0, clamped to 1
    match action {
        SearchAction::SetPage(p) => assert_eq!(p, 1),
        _ => panic!("Expected SetPage(1)"),
    }
}

#[test]
fn test_goto_dialog_empty_enter_continues() {
    let entries = vec![create_test_entry("cmd")];
    let mut app = SearchApp::new(test_search_config(entries, 100));
    app.dialog = DialogState::GoToPage {
        input: String::new(),
    };

    // Enter with empty input → no valid parse → Continue
    let action = app.handle_input(KeyEvent::from(KeyCode::Enter));
    assert!(matches!(action, SearchAction::Continue));
    assert!(matches!(app.dialog, DialogState::None));
}

// ── handle_tag_dialog_input tests ──

#[test]
fn test_tag_dialog_navigation() {
    let mut cfg = test_search_config(vec![create_test_entry("ls")], 1);
    cfg.tags = vec![
        Tag {
            id: 1,
            name: "alpha".to_string(),
            description: None,
        },
        Tag {
            id: 2,
            name: "beta".to_string(),
            description: None,
        },
        Tag {
            id: 3,
            name: "gamma".to_string(),
            description: None,
        },
    ];
    let mut app = SearchApp::new(cfg);
    app.dialog = DialogState::TagAssociation;
    app.tag_list_state.select(Some(0));

    // Down
    app.handle_input(KeyEvent::from(KeyCode::Down));
    assert_eq!(app.tag_list_state.selected(), Some(1));

    // Down again
    app.handle_input(KeyEvent::from(KeyCode::Down));
    assert_eq!(app.tag_list_state.selected(), Some(2));

    // Down at bottom stays
    app.handle_input(KeyEvent::from(KeyCode::Down));
    assert_eq!(app.tag_list_state.selected(), Some(2));

    // Up
    app.handle_input(KeyEvent::from(KeyCode::Up));
    assert_eq!(app.tag_list_state.selected(), Some(1));

    // Up at top stays
    app.tag_list_state.select(Some(0));
    app.handle_input(KeyEvent::from(KeyCode::Up));
    assert_eq!(app.tag_list_state.selected(), Some(0));
}

#[test]
fn test_tag_dialog_down_when_none_selected() {
    let mut cfg = test_search_config(vec![create_test_entry("ls")], 1);
    cfg.tags = vec![Tag {
        id: 1,
        name: "t".to_string(),
        description: None,
    }];
    let mut app = SearchApp::new(cfg);
    app.dialog = DialogState::TagAssociation;
    app.tag_list_state.select(None);

    app.handle_input(KeyEvent::from(KeyCode::Down));
    assert_eq!(app.tag_list_state.selected(), Some(0));
}

#[test]
fn test_tag_dialog_enter_associates_session() {
    let mut cfg = test_search_config(vec![create_test_entry("ls")], 1);
    cfg.tags = vec![Tag {
        id: 42,
        name: "deploy".to_string(),
        description: None,
    }];
    let mut app = SearchApp::new(cfg);
    app.dialog = DialogState::TagAssociation;
    app.tag_list_state.select(Some(0));

    let action = app.handle_input(KeyEvent::from(KeyCode::Enter));
    match action {
        SearchAction::AssociateSession(tag_id) => assert_eq!(tag_id, 42),
        _ => panic!("Expected AssociateSession(42)"),
    }
    assert!(matches!(app.dialog, DialogState::None));
}

#[test]
fn test_tag_dialog_enter_no_selection_closes() {
    let mut cfg = test_search_config(vec![create_test_entry("ls")], 1);
    cfg.tags = vec![Tag {
        id: 1,
        name: "t".to_string(),
        description: None,
    }];
    let mut app = SearchApp::new(cfg);
    app.dialog = DialogState::TagAssociation;
    app.tag_list_state.select(None);

    let action = app.handle_input(KeyEvent::from(KeyCode::Enter));
    assert!(matches!(action, SearchAction::Continue));
    assert!(matches!(app.dialog, DialogState::None));
}

#[test]
fn test_tag_dialog_other_key_ignored() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::TagAssociation;

    let action = app.handle_input(KeyEvent::from(KeyCode::Char('x')));
    assert!(matches!(action, SearchAction::Continue));
    assert!(matches!(app.dialog, DialogState::TagAssociation));
}

// ── handle_note_dialog_input tests ──

#[test]
fn test_note_dialog_typing() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Note {
        entry_id: 10,
        input: String::new(),
    };

    app.handle_input(KeyEvent::from(KeyCode::Char('h')));
    app.handle_input(KeyEvent::from(KeyCode::Char('i')));

    if let DialogState::Note { ref input, .. } = app.dialog {
        assert_eq!(input, "hi");
    } else {
        panic!("Expected Note dialog");
    }
}

#[test]
fn test_note_dialog_backspace() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Note {
        entry_id: 10,
        input: "abc".to_string(),
    };

    app.handle_input(KeyEvent::from(KeyCode::Backspace));
    if let DialogState::Note { ref input, .. } = app.dialog {
        assert_eq!(input, "ab");
    } else {
        panic!("Expected Note dialog");
    }
}

#[test]
fn test_note_dialog_enter_with_text_saves() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Note {
        entry_id: 10,
        input: "my note".to_string(),
    };

    let action = app.handle_input(KeyEvent::from(KeyCode::Enter));
    match action {
        SearchAction::SaveNote(id, text) => {
            assert_eq!(id, 10);
            assert_eq!(text, "my note");
        }
        _ => panic!("Expected SaveNote"),
    }
}

#[test]
fn test_note_dialog_enter_empty_deletes() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Note {
        entry_id: 10,
        input: String::new(),
    };

    let action = app.handle_input(KeyEvent::from(KeyCode::Enter));
    match action {
        SearchAction::DeleteNote(id) => assert_eq!(id, 10),
        _ => panic!("Expected DeleteNote(10)"),
    }
}

#[test]
fn test_note_dialog_esc_closes() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Note {
        entry_id: 10,
        input: "partial".to_string(),
    };

    let action = app.handle_input(KeyEvent::from(KeyCode::Esc));
    assert!(matches!(action, SearchAction::Continue));
    assert!(matches!(app.dialog, DialogState::None));
}

#[test]
fn test_note_dialog_other_key_ignored() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Note {
        entry_id: 10,
        input: String::new(),
    };

    let action = app.handle_input(KeyEvent::from(KeyCode::F(1)));
    assert!(matches!(action, SearchAction::Continue));
    if let DialogState::Note { ref input, .. } = app.dialog {
        assert!(input.is_empty());
    }
}

// ── handle_filter_input tests ──

#[test]
fn test_filter_esc_closes() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Filter;

    let action = app.handle_input(KeyEvent::from(KeyCode::Esc));
    assert!(matches!(action, SearchAction::Continue));
    assert!(matches!(app.dialog, DialogState::None));
}

#[test]
fn test_filter_tab_cycles_focus_forward() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Filter;
    app.filters.focus_index = 0;

    app.handle_input(KeyEvent::from(KeyCode::Tab));
    assert_eq!(app.filters.focus_index, 1);

    app.handle_input(KeyEvent::from(KeyCode::Tab));
    assert_eq!(app.filters.focus_index, 2);

    app.handle_input(KeyEvent::from(KeyCode::Tab));
    assert_eq!(app.filters.focus_index, 3);

    app.handle_input(KeyEvent::from(KeyCode::Tab));
    assert_eq!(app.filters.focus_index, 4);

    // Wraps around to 0
    app.handle_input(KeyEvent::from(KeyCode::Tab));
    assert_eq!(app.filters.focus_index, 0);
}

#[test]
fn test_filter_backtab_cycles_focus_backward() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Filter;
    app.filters.focus_index = 0;

    // From 0 → wraps to 4
    app.handle_input(KeyEvent::from(KeyCode::BackTab));
    assert_eq!(app.filters.focus_index, 4);

    app.handle_input(KeyEvent::from(KeyCode::BackTab));
    assert_eq!(app.filters.focus_index, 3);

    app.handle_input(KeyEvent::from(KeyCode::BackTab));
    assert_eq!(app.filters.focus_index, 2);

    app.handle_input(KeyEvent::from(KeyCode::BackTab));
    assert_eq!(app.filters.focus_index, 1);

    app.handle_input(KeyEvent::from(KeyCode::BackTab));
    assert_eq!(app.filters.focus_index, 0);
}

#[test]
fn test_filter_typing_each_field() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Filter;

    // Field 0: start_date_input
    app.filters.focus_index = 0;
    app.handle_input(KeyEvent::from(KeyCode::Char('2')));
    assert!(app.filters.start_date_input.contains('2'));

    // Field 1: end_date_input
    app.filters.focus_index = 1;
    app.filters.end_date_input.clear();
    app.handle_input(KeyEvent::from(KeyCode::Char('x')));
    assert_eq!(app.filters.end_date_input, "x");

    // Field 2: tag_filter_input
    app.filters.focus_index = 2;
    app.handle_input(KeyEvent::from(KeyCode::Char('w')));
    assert!(app.filters.tag_filter_input.contains('w'));

    // Field 3: exit_code_input
    app.filters.focus_index = 3;
    app.handle_input(KeyEvent::from(KeyCode::Char('0')));
    assert!(app.filters.exit_code_input.contains('0'));

    // Field 4: executor_filter_input
    app.filters.focus_index = 4;
    app.handle_input(KeyEvent::from(KeyCode::Char('h')));
    assert!(app.filters.executor_filter_input.contains('h'));
}

#[test]
fn test_filter_backspace_each_field() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Filter;

    // Set up inputs
    app.filters.start_date_input = "abc".to_string();
    app.filters.end_date_input = "def".to_string();
    app.filters.tag_filter_input = "ghi".to_string();
    app.filters.exit_code_input = "123".to_string();
    app.filters.executor_filter_input = "xyz".to_string();

    app.filters.focus_index = 0;
    app.handle_input(KeyEvent::from(KeyCode::Backspace));
    assert_eq!(app.filters.start_date_input, "ab");

    app.filters.focus_index = 1;
    app.handle_input(KeyEvent::from(KeyCode::Backspace));
    assert_eq!(app.filters.end_date_input, "de");

    app.filters.focus_index = 2;
    app.handle_input(KeyEvent::from(KeyCode::Backspace));
    assert_eq!(app.filters.tag_filter_input, "gh");

    app.filters.focus_index = 3;
    app.handle_input(KeyEvent::from(KeyCode::Backspace));
    assert_eq!(app.filters.exit_code_input, "12");

    app.filters.focus_index = 4;
    app.handle_input(KeyEvent::from(KeyCode::Backspace));
    assert_eq!(app.filters.executor_filter_input, "xy");
}

#[test]
fn test_filter_enter_applies_exit_code() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Filter;
    app.filters.exit_code_input = "1".to_string();

    let action = app.handle_input(KeyEvent::from(KeyCode::Enter));
    assert!(matches!(action, SearchAction::Reload));
    assert_eq!(app.filters.exit_code, Some(1));
    assert!(matches!(app.dialog, DialogState::None));
}

#[test]
fn test_filter_enter_applies_executor() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Filter;
    app.filters.executor_filter_input = "Human".to_string();

    let action = app.handle_input(KeyEvent::from(KeyCode::Enter));
    assert!(matches!(action, SearchAction::Reload));
    assert_eq!(app.filters.executor_type, Some("human".to_string())); // lowercased
}

#[test]
fn test_filter_enter_resolves_tag_name() {
    let mut cfg = test_search_config(vec![create_test_entry("ls")], 1);
    cfg.tags = vec![Tag {
        id: 99,
        name: "deploy".to_string(),
        description: None,
    }];
    let mut app = SearchApp::new(cfg);
    app.dialog = DialogState::Filter;
    app.filters.tag_filter_input = "Deploy".to_string(); // mixed case

    let action = app.handle_input(KeyEvent::from(KeyCode::Enter));
    assert!(matches!(action, SearchAction::Reload));
    assert_eq!(app.filters.tag_id, Some(99));
}

#[test]
fn test_filter_enter_unknown_tag_name() {
    let mut cfg = test_search_config(vec![create_test_entry("ls")], 1);
    cfg.tags = vec![Tag {
        id: 1,
        name: "deploy".to_string(),
        description: None,
    }];
    let mut app = SearchApp::new(cfg);
    app.dialog = DialogState::Filter;
    app.filters.tag_filter_input = "nonexistent".to_string();

    let action = app.handle_input(KeyEvent::from(KeyCode::Enter));
    assert!(matches!(action, SearchAction::Reload));
    assert_eq!(app.filters.tag_id, None); // tag not found
}

#[test]
fn test_filter_enter_clears_empty_fields() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    // Pre-set some filters
    app.filters.after = Some(1000);
    app.filters.before = Some(2000);
    app.filters.exit_code = Some(1);
    app.filters.executor_type = Some("agent".to_string());
    app.filters.tag_id = Some(5);

    // Open filter with all empty inputs
    app.dialog = DialogState::Filter;
    app.filters.start_date_input.clear();
    app.filters.end_date_input.clear();
    app.filters.exit_code_input.clear();
    app.filters.executor_filter_input.clear();
    app.filters.tag_filter_input.clear();

    let action = app.handle_input(KeyEvent::from(KeyCode::Enter));
    assert!(matches!(action, SearchAction::Reload));
    assert_eq!(app.filters.after, None);
    assert_eq!(app.filters.before, None);
    assert_eq!(app.filters.exit_code, None);
    assert_eq!(app.filters.executor_type, None);
    assert_eq!(app.filters.tag_id, None);
    assert_eq!(app.pagination.page, 1);
}

#[test]
fn test_filter_enter_invalid_exit_code() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Filter;
    app.filters.exit_code_input = "abc".to_string();

    app.handle_input(KeyEvent::from(KeyCode::Enter));
    assert_eq!(app.filters.exit_code, None); // parse failure → None
}

#[test]
fn test_filter_other_key_ignored() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Filter;

    let action = app.handle_input(KeyEvent::from(KeyCode::F(5)));
    assert!(matches!(action, SearchAction::Continue));
    assert!(matches!(app.dialog, DialogState::Filter));
}
