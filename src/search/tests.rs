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
        executors: vec![],
        unique_counts: std::collections::HashMap::new(),
        filter_after: None,
        filter_before: None,
        filter_tag_id: None,
        filter_exit_code: None,
        filter_executor_type: None,
        show_agents: false,
        failed_only: false,
        start_date_input: None,
        end_date_input: None,
        tag_filter_input: None,
        exit_code_input: None,
        executor_filter_input: None,
        bookmarked_commands: std::collections::HashSet::new(),
        filter_cwd: None,
        noted_entry_ids: std::collections::HashSet::new(),
        show_risk_in_search: false,
        vim_enabled: false,
        view: ViewOptions {
            unique_mode: false,
            context_boost: true,
            detail_pane_open: true,
            search_field: SearchField::Command,
            current_cwd: None,
            length_threshold: 80,
            human_boost_percent: 33,
            cwd_boost_percent: 50,
        },
        recall: RecallState::default(),
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
fn test_fuzzy_contiguous_match_outranks_scattered_in_cwd() {
    // Regression: a contiguous "git add" match must rank above a scattered
    // atom match ("git ... add") even when the scattered one is in the current
    // directory (and thus cwd/human-boosted). nucleo alone scores them equally.
    let cwd = "/proj";
    let mk = |cmd: &str, dir: &str| {
        let mut e = create_test_entry(cmd);
        e.cwd = dir.to_string();
        e
    };
    let entries = vec![
        // current dir, scattered matches (boosted)
        mk("suv bookmark add \"git log --oneline -10\"", cwd),
        mk("git remote add origin https://github.com/x/y.git", cwd),
        // other dir, contiguous prefix match (not cwd-boosted)
        mk("git add .", "/other"),
    ];
    let scored = SearchApp::fuzzy_score(
        entries,
        "git add",
        Some(cwd),
        SearchField::Command,
        80,
        33,
        50,
    );
    let order: Vec<&str> = scored.iter().map(|e| e.command.as_str()).collect();
    assert_eq!(
        order.first(),
        Some(&"git add ."),
        "contiguous 'git add' must rank first; got {order:?}"
    );
    // In-order word match ("git" then "add") beats out-of-order ("add" before
    // "git"), even though the latter is in the current (boosted) directory.
    let remote = order.iter().position(|c| c.starts_with("git remote add"));
    let bookmark = order.iter().position(|c| c.starts_with("suv bookmark add"));
    assert!(
        remote < bookmark,
        "in-order 'git…add' should rank above out-of-order; got {order:?}"
    );
}

#[test]
fn test_multi_word_query_requires_literal_words() {
    // A 2+ word query must not return loose subsequence matches.
    let entries = vec![
        create_test_entry("git add ."),               // has both words
        create_test_entry("git remote add origin x"), // has both words
        create_test_entry("claude --resume $(git rev-parse)"), // 'git' but no 'add'
        create_test_entry("AUTHOR_NAME=x git commit"), // 'git' but no 'add'
    ];
    let scored = SearchApp::fuzzy_score(entries, "git add", None, SearchField::Command, 80, 33, 50);
    let cmds: Vec<&str> = scored.iter().map(|e| e.command.as_str()).collect();
    assert!(cmds.contains(&"git add ."));
    assert!(cmds.contains(&"git remote add origin x"));
    assert!(
        !cmds.iter().any(|c| c.contains("rev-parse")),
        "subsequence-only match must be dropped: {cmds:?}"
    );
    assert!(
        !cmds.iter().any(|c| c.contains("commit")),
        "subsequence-only match must be dropped: {cmds:?}"
    );
}

#[test]
fn test_multi_word_query_absent_word_returns_nothing() {
    // "git mango": no command contains "mango" → no results, despite the
    // letters appearing as a subsequence in many commands.
    let entries = vec![
        create_test_entry("git add ."),
        create_test_entry("echo '{\"method\":\"statistical\",\"name\":\"find_agent\"}'"),
        create_test_entry("make migrate-organization"),
    ];
    let scored =
        SearchApp::fuzzy_score(entries, "git mango", None, SearchField::Command, 80, 33, 50);
    assert!(
        scored.is_empty(),
        "no command has 'mango'; got {:?}",
        scored.iter().map(|e| &e.command).collect::<Vec<_>>()
    );
}

#[test]
fn test_single_token_requires_literal_substring() {
    // A single token must appear literally — no loose subsequence. So a
    // literal fragment matches, but gibberish (and abbreviations like "gco")
    // do not.
    let entries = vec![
        create_test_entry("git checkout main"),
        create_test_entry("cargo build"),
    ];
    let by = |q: &str| {
        SearchApp::fuzzy_score(entries.clone(), q, None, SearchField::Command, 80, 33, 50)
    };
    // literal fragment matches
    let cmds: Vec<String> = by("checkout").iter().map(|e| e.command.clone()).collect();
    assert!(cmds.iter().any(|c| c == "git checkout main"));
    // subsequence-only ("gco") and gibberish do NOT match
    assert!(
        by("gco").is_empty(),
        "subsequence abbreviation should not match"
    );
    assert!(
        by("adasdasdasreffsdscdsfeg").is_empty(),
        "gibberish must not match"
    );
}

#[test]
fn test_match_tier_levels() {
    use super::data::match_tier;
    let atoms = ["git", "add"];
    assert_eq!(match_tier("git add .", "git add", &atoms), 4); // prefix
    assert_eq!(match_tier("sudo git add .", "git add", &atoms), 3); // substring
    assert_eq!(match_tier("git remote add origin", "git add", &atoms), 2); // in order
    assert_eq!(match_tier("suv bookmark add git log", "git add", &atoms), 1); // out of order
                                                                              // pure subsequence (no literal "git"/"add" substring)
    assert_eq!(match_tier("gradle dist", "git add", &atoms), 0);
}

#[test]
fn test_fuzzy_score_ranking() {
    let entries = vec![
        create_test_entry("git checkout main"),
        create_test_entry("echo hello world"),
        create_test_entry("git commit -m 'fix'"),
        create_test_entry("cargo build"),
    ];

    // "git" should match git commands but not "echo" or "cargo build"
    let scored = SearchApp::fuzzy_score(entries, "git", None, SearchField::Command, 80, 33, 50);
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

    let scored = SearchApp::fuzzy_score(entries, "zzzzz", None, SearchField::Command, 80, 33, 50);
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

    let scored = SearchApp::fuzzy_score(
        entries,
        "cargo test",
        None,
        SearchField::Command,
        80,
        33,
        50,
    );
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

    let scored = SearchApp::fuzzy_score(
        entries,
        "git status",
        None,
        SearchField::Command,
        80,
        33,
        50,
    );
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

    let scored = SearchApp::fuzzy_score(
        entries,
        "cargo build",
        None,
        SearchField::Command,
        80,
        33,
        50,
    );
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

    let scored = SearchApp::fuzzy_score(
        entries,
        "make test",
        Some("/project"),
        SearchField::Command,
        80,
        33,
        50,
    );
    assert_eq!(scored.len(), 2);
    // Local CWD entry should come first
    assert_eq!(scored[0].cwd, "/project");
}

#[test]
fn test_fuzzy_score_empty_query() {
    let entries = vec![create_test_entry("ls"), create_test_entry("pwd")];

    // Empty query should match nothing (nucleo needs at least some pattern)
    let scored = SearchApp::fuzzy_score(entries, "", None, SearchField::Command, 80, 33, 50);
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

    let scored = SearchApp::fuzzy_score(entries, "l", None, SearchField::Command, 80, 33, 50);
    // Should match "ls -la" at minimum
    assert!(scored.iter().any(|e| e.command == "ls -la"));
}

#[test]
fn test_fuzzy_score_custom_length_threshold() {
    // With threshold=20, a 30-char command should be penalized
    let entries = vec![
        create_test_entry("git status"), // 10 chars — under threshold
        create_test_entry("git status --porcelain --branch"), // 34 chars — over threshold
    ];

    let scored = SearchApp::fuzzy_score(
        entries,
        "git status",
        None,
        SearchField::Command,
        20,
        33,
        50,
    );
    assert_eq!(scored.len(), 2);
    // Short command should come first since the long one gets penalized at threshold=20
    assert_eq!(scored[0].command, "git status");
}

#[test]
fn test_fuzzy_score_human_boost_zero() {
    // With human_boost=0, human and agent commands should score equally (tiebreaker: human first)
    let mut human_entry = create_test_entry("cargo build");
    human_entry.executor_type = Some("human".to_string());

    let mut agent_entry = create_test_entry("cargo build");
    agent_entry.executor_type = Some("agent".to_string());

    let entries = vec![agent_entry, human_entry];

    // human_boost_percent=0 means no boost — tiebreaker still favours human
    let scored = SearchApp::fuzzy_score(
        entries,
        "cargo build",
        None,
        SearchField::Command,
        80,
        0,
        50,
    );
    assert_eq!(scored.len(), 2);
    // Human still wins via tiebreaker, but scores are equal
    assert_eq!(scored[0].executor_type.as_deref(), Some("human"));
}

#[test]
fn test_fuzzy_score_cwd_boost_zero() {
    let mut local_entry = create_test_entry("make test");
    local_entry.cwd = "/project".to_string();

    let mut remote_entry = create_test_entry("make test");
    remote_entry.cwd = "/other".to_string();

    let entries = vec![remote_entry, local_entry];

    // cwd_boost_percent=0 means no directory boost
    let scored = SearchApp::fuzzy_score(
        entries,
        "make test",
        Some("/project"),
        SearchField::Command,
        80,
        33,
        0,
    );
    assert_eq!(scored.len(), 2);
    // With no CWD boost, order is determined by other factors (both score equally)
}

#[test]
fn test_fuzzy_score_high_boost() {
    // human_boost_percent=100 means doubling — verify no overflow via saturating_add
    let mut entry = create_test_entry("cargo build");
    entry.executor_type = Some("human".to_string());

    let entries = vec![entry];
    let scored = SearchApp::fuzzy_score(
        entries,
        "cargo build",
        None,
        SearchField::Command,
        80,
        100,
        100,
    );
    assert_eq!(scored.len(), 1);
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
    let mut app = SearchApp::new(test_search_config(entries, 500));

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
fn test_handle_input_page_down_scrolls_ten_rows() {
    let entries: Vec<Entry> = (0..20)
        .map(|i| create_test_entry(&format!("cmd{i}")))
        .collect();
    let mut app = SearchApp::new(test_search_config(entries, 20));
    app.table_state.select(Some(0));

    app.handle_input(KeyEvent::from(KeyCode::PageDown));
    assert_eq!(app.table_state.selected(), Some(10));
}

#[test]
fn test_handle_input_page_down_clamps_at_bottom() {
    let entries: Vec<Entry> = (0..15)
        .map(|i| create_test_entry(&format!("cmd{i}")))
        .collect();
    let mut app = SearchApp::new(test_search_config(entries, 15));
    app.table_state.select(Some(8));

    app.handle_input(KeyEvent::from(KeyCode::PageDown));
    assert_eq!(app.table_state.selected(), Some(14));
}

#[test]
fn test_handle_input_page_up_scrolls_ten_rows() {
    let entries: Vec<Entry> = (0..20)
        .map(|i| create_test_entry(&format!("cmd{i}")))
        .collect();
    let mut app = SearchApp::new(test_search_config(entries, 20));
    app.table_state.select(Some(15));

    app.handle_input(KeyEvent::from(KeyCode::PageUp));
    assert_eq!(app.table_state.selected(), Some(5));
}

#[test]
fn test_handle_input_page_up_clamps_at_top() {
    let entries: Vec<Entry> = (0..15)
        .map(|i| create_test_entry(&format!("cmd{i}")))
        .collect();
    let mut app = SearchApp::new(test_search_config(entries, 15));
    app.table_state.select(Some(3));

    app.handle_input(KeyEvent::from(KeyCode::PageUp));
    assert_eq!(app.table_state.selected(), Some(0));
}

#[test]
fn test_handle_input_home_jumps_to_top() {
    let entries: Vec<Entry> = (0..5)
        .map(|i| create_test_entry(&format!("cmd{i}")))
        .collect();
    let mut app = SearchApp::new(test_search_config(entries, 5));
    app.table_state.select(Some(4));

    app.handle_input(KeyEvent::from(KeyCode::Home));
    assert_eq!(app.table_state.selected(), Some(0));
}

#[test]
fn test_handle_input_end_jumps_to_bottom() {
    let entries: Vec<Entry> = (0..5)
        .map(|i| create_test_entry(&format!("cmd{i}")))
        .collect();
    let mut app = SearchApp::new(test_search_config(entries, 5));
    app.table_state.select(Some(0));

    app.handle_input(KeyEvent::from(KeyCode::End));
    assert_eq!(app.table_state.selected(), Some(4));
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

#[test]
fn down_on_last_result_requests_the_next_page() {
    let entries: Vec<Entry> = (0..50)
        .map(|i| create_test_entry(&format!("cmd{i}")))
        .collect();
    let mut app = SearchApp::new(test_search_config(entries, 51));
    app.table_state.select(Some(49));

    let action = app.handle_input(KeyEvent::from(KeyCode::Down));

    assert!(matches!(action, SearchAction::SetPage(2)));
}

#[test]
fn up_on_first_result_requests_the_previous_page() {
    let entries = vec![create_test_entry("cmd50")];
    let mut config = test_search_config(entries, 51);
    config.page = 2;
    let mut app = SearchApp::new(config);
    app.table_state.select(Some(0));

    let action = app.handle_input(KeyEvent::from(KeyCode::Up));

    assert!(matches!(action, SearchAction::SetPageLast(1)));
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
fn test_ctrl_o_toggles_bookmarks_only() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    assert!(!app.filters.bookmarks_only);

    // Toggle on
    let action = app.handle_input(ctrl_key('o'));
    assert!(matches!(action, SearchAction::Reload));
    assert!(app.filters.bookmarks_only);
    assert_eq!(app.pagination.page, 1);
    assert!(app.status_message.is_some());

    // Toggle off
    let action = app.handle_input(ctrl_key('o'));
    assert!(matches!(action, SearchAction::Reload));
    assert!(!app.filters.bookmarks_only);
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
fn test_unknown_ctrl_key_ignored() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));

    // Ctrl+Z is unhandled → returns Continue without inserting 'z' into query
    let action = app.handle_input(ctrl_key('z'));
    assert!(matches!(action, SearchAction::Continue));
    assert!(
        app.query.is_empty(),
        "Unrecognized Ctrl+key should not insert characters"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// PROD-09: explicit matching modes and predictable scopes
// ───────────────────────────────────────────────────────────────────────────

use crate::search::scope::RecallContext;
use crate::search::{MatchMode, RecallScope};

/// A repo-shaped temporary tree, so workspace scope is genuinely available.
fn workspace_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    (dir, root)
}

/// An app whose context has a directory, a workspace and a session, so every
/// scope is reachable.
fn app_with_full_context() -> (tempfile::TempDir, std::path::PathBuf, SearchApp) {
    let (dir, root) = workspace_fixture();
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.recall.context = RecallContext::resolve_at(
        Some(&root.join("src")),
        Some("sess-1".to_string()),
        Some(dir.path()),
    );
    app.sync_scope_filters();
    (dir, root, app)
}

#[test]
fn the_default_matching_mode_is_terms() {
    let app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    assert_eq!(app.recall.match_mode, MatchMode::Terms);
    assert_eq!(app.recall.scope, RecallScope::All);
}

#[test]
fn ctrl_x_cycles_the_matching_mode_and_reloads() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    for expected in [
        MatchMode::Literal,
        MatchMode::Prefix,
        MatchMode::Fuzzy,
        MatchMode::Terms,
    ] {
        let action = app.handle_input(ctrl_key('x'));
        assert!(matches!(action, SearchAction::Reload));
        assert_eq!(app.recall.match_mode, expected);
        assert!(app.status_message.is_some(), "the new mode must be named");
    }
}

#[test]
fn ctrl_p_cycles_scope_and_narrows_the_directory_filter() {
    let (_d, root, mut app) = app_with_full_context();
    let canonical_root = std::fs::canonicalize(&root).unwrap();
    let canonical_src = std::fs::canonicalize(root.join("src")).unwrap();

    assert_eq!(app.recall.scope, RecallScope::All);
    assert!(app.filters.cwd.is_none());

    assert!(matches!(
        app.handle_input(ctrl_key('p')),
        SearchAction::Reload
    ));
    assert_eq!(app.recall.scope, RecallScope::Directory);
    assert_eq!(
        app.filters.cwd.as_deref(),
        Some(canonical_src.to_string_lossy().as_ref())
    );

    app.handle_input(ctrl_key('p'));
    assert_eq!(app.recall.scope, RecallScope::Workspace);
    assert_eq!(
        app.filters.cwd.as_deref(),
        Some(canonical_root.to_string_lossy().as_ref()),
        "workspace scope must use the repository root, not the subdirectory"
    );

    app.handle_input(ctrl_key('p'));
    assert_eq!(app.recall.scope, RecallScope::Session);
    assert!(
        app.filters.cwd.is_none(),
        "session scope is not a directory filter"
    );

    app.handle_input(ctrl_key('p'));
    assert_eq!(app.recall.scope, RecallScope::All);
}

#[test]
fn ctrl_p_skips_scopes_that_are_unavailable_here() {
    let dir = tempfile::TempDir::new().unwrap();
    let plain = dir.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    // A directory, but no repository and no session.
    app.recall.context = RecallContext::resolve_at(Some(&plain), None, Some(dir.path()));

    app.handle_input(ctrl_key('p'));
    assert_eq!(app.recall.scope, RecallScope::Directory);
    // Workspace and session cannot apply here, so the cycle returns to All
    // rather than landing on a scope that shows nothing.
    app.handle_input(ctrl_key('p'));
    assert_eq!(app.recall.scope, RecallScope::All);
}

#[test]
fn ctrl_r_resets_every_narrowing_to_all_history_in_one_action() {
    let (_d, _root, mut app) = app_with_full_context();
    app.handle_input(ctrl_key('p')); // directory
    app.handle_input(ctrl_key('p')); // workspace
    app.handle_input(ctrl_key('x')); // literal
    app.handle_input(ctrl_key('e')); // failed only
    app.handle_input(ctrl_key('o')); // bookmarked only
    app.filters.after = Some(1);
    app.filters.tag_id = Some(2);
    app.filters.exit_code = Some(3);
    app.filters.executor_type = Some("bot".to_string());

    let action = app.handle_input(ctrl_key('r'));
    assert!(matches!(action, SearchAction::Reload));
    assert_eq!(app.recall.scope, RecallScope::All);
    assert_eq!(app.recall.match_mode, MatchMode::Terms);
    assert!(app.filters.cwd.is_none());
    assert!(!app.filters.failed_only);
    assert!(!app.filters.bookmarks_only);
    assert_eq!(app.active_filter_count(), 0);
}

#[test]
fn reset_never_pulls_excluded_agent_commands_into_view() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    assert!(!app.filters.show_agents, "agents are hidden by default");
    app.handle_input(ctrl_key('r'));
    assert!(
        !app.filters.show_agents,
        "resetting the scope must not silently include agent commands"
    );

    // ...and it equally must not switch them off for someone who opted in.
    app.handle_input(ctrl_key('a'));
    assert!(app.filters.show_agents);
    app.handle_input(ctrl_key('r'));
    assert!(app.filters.show_agents);
}

#[test]
fn matching_mode_and_ranking_mode_are_independent() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    let ranking_before = app.view.context_boost;

    // Changing the match mode leaves the ranking alone...
    app.handle_input(ctrl_key('x'));
    assert_eq!(app.recall.match_mode, MatchMode::Literal);
    assert_eq!(app.view.context_boost, ranking_before);

    // ...and changing the ranking leaves the match mode alone.
    app.handle_input(ctrl_key('s'));
    assert_ne!(app.view.context_boost, ranking_before);
    assert_eq!(app.recall.match_mode, MatchMode::Literal);

    // Unique is a display choice, not a matching one.
    app.handle_input(ctrl_key('u'));
    assert_eq!(app.recall.match_mode, MatchMode::Literal);
}

#[test]
fn changing_the_scope_leaves_the_matching_mode_alone() {
    let (_d, _root, mut app) = app_with_full_context();
    app.handle_input(ctrl_key('x'));
    app.handle_input(ctrl_key('x'));
    assert_eq!(app.recall.match_mode, MatchMode::Prefix);
    app.handle_input(ctrl_key('p'));
    assert_eq!(app.recall.match_mode, MatchMode::Prefix);
}

#[test]
fn ctrl_l_still_toggles_between_all_history_and_this_directory() {
    let (_d, _root, mut app) = app_with_full_context();
    assert!(matches!(
        app.handle_input(ctrl_key('l')),
        SearchAction::Reload
    ));
    assert_eq!(app.recall.scope, RecallScope::Directory);
    assert!(app.filters.cwd.is_some());

    assert!(matches!(
        app.handle_input(ctrl_key('l')),
        SearchAction::Reload
    ));
    assert_eq!(app.recall.scope, RecallScope::All);
    assert!(app.filters.cwd.is_none());
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

    // Field 4: executor selector (Up/Down cycles, not text input)
    app.filters.focus_index = 4;
    app.filters.executors = vec!["agent: claude-code".into(), "human: terminal".into()];
    app.filters.executor_sel = 0; // "All"
    app.handle_input(KeyEvent::from(KeyCode::Down));
    assert_eq!(app.filters.executor_sel, 1); // first executor
    app.handle_input(KeyEvent::from(KeyCode::Down));
    assert_eq!(app.filters.executor_sel, 2); // second executor
    app.handle_input(KeyEvent::from(KeyCode::Down));
    assert_eq!(app.filters.executor_sel, 0); // wraps to "All"
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

    // Field 4 is a selector — backspace is a no-op
    app.filters.focus_index = 4;
    app.filters.executor_sel = 1;
    app.handle_input(KeyEvent::from(KeyCode::Backspace));
    assert_eq!(app.filters.executor_sel, 1); // unchanged
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
    app.filters.executors = vec!["agent: claude-code".into(), "human: terminal".into()];
    app.filters.executor_sel = 2; // "human: terminal"

    let action = app.handle_input(KeyEvent::from(KeyCode::Enter));
    assert!(matches!(action, SearchAction::Reload));
    assert_eq!(
        app.filters.executor_type,
        Some("terminal".to_string()) // extracts name part after ": "
    );
}

#[test]
fn test_filter_enter_executor_all() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Filter;
    app.filters.executors = vec!["agent: claude-code".into()];
    app.filters.executor_sel = 0; // "All"

    let action = app.handle_input(KeyEvent::from(KeyCode::Enter));
    assert!(matches!(action, SearchAction::Reload));
    assert_eq!(app.filters.executor_type, None); // "All" clears the filter
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

// ========================================================================
// Vim mode tests
// ========================================================================

fn test_vim_config(entries: Vec<Entry>, total_items: usize) -> SearchConfig {
    let mut cfg = test_search_config(entries, total_items);
    cfg.vim_enabled = true;
    cfg
}

#[test]
fn test_vim_mode_defaults_to_insert() {
    let app = SearchApp::new(test_vim_config(vec![create_test_entry("ls")], 1));
    assert!(app.vim_enabled);
    assert_eq!(app.vim_mode, VimMode::Insert);
}

#[test]
fn test_vim_esc_switches_to_normal() {
    let mut app = SearchApp::new(test_vim_config(vec![create_test_entry("ls")], 1));
    assert_eq!(app.vim_mode, VimMode::Insert);

    let action = app.handle_input(KeyEvent::from(KeyCode::Esc));
    assert!(matches!(action, SearchAction::Continue));
    assert_eq!(app.vim_mode, VimMode::Normal);
}

#[test]
fn test_vim_slash_switches_to_insert() {
    let mut app = SearchApp::new(test_vim_config(vec![create_test_entry("ls")], 1));
    app.vim_mode = VimMode::Normal;

    let action = app.handle_input(KeyEvent::from(KeyCode::Char('/')));
    assert!(matches!(action, SearchAction::Continue));
    assert_eq!(app.vim_mode, VimMode::Insert);
}

#[test]
fn test_vim_i_switches_to_insert() {
    let mut app = SearchApp::new(test_vim_config(vec![create_test_entry("ls")], 1));
    app.vim_mode = VimMode::Normal;

    let action = app.handle_input(KeyEvent::from(KeyCode::Char('i')));
    assert!(matches!(action, SearchAction::Continue));
    assert_eq!(app.vim_mode, VimMode::Insert);
}

#[test]
fn test_vim_q_quits_in_normal_mode() {
    let mut app = SearchApp::new(test_vim_config(vec![create_test_entry("ls")], 1));
    app.vim_mode = VimMode::Normal;

    let action = app.handle_input(KeyEvent::from(KeyCode::Char('q')));
    assert!(matches!(action, SearchAction::Exit));
}

#[test]
fn test_vim_esc_quits_in_normal_mode() {
    let mut app = SearchApp::new(test_vim_config(vec![create_test_entry("ls")], 1));
    app.vim_mode = VimMode::Normal;

    let action = app.handle_input(KeyEvent::from(KeyCode::Esc));
    assert!(matches!(action, SearchAction::Exit));
}

#[test]
fn test_vim_j_navigates_down() {
    let entries = vec![
        create_test_entry("cmd1"),
        create_test_entry("cmd2"),
        create_test_entry("cmd3"),
    ];
    let mut app = SearchApp::new(test_vim_config(entries, 3));
    app.vim_mode = VimMode::Normal;
    assert_eq!(app.table_state.selected(), Some(0));

    app.handle_input(KeyEvent::from(KeyCode::Char('j')));
    assert_eq!(app.table_state.selected(), Some(1));

    app.handle_input(KeyEvent::from(KeyCode::Char('j')));
    assert_eq!(app.table_state.selected(), Some(2));
}

#[test]
fn test_vim_k_navigates_up() {
    let entries = vec![
        create_test_entry("cmd1"),
        create_test_entry("cmd2"),
        create_test_entry("cmd3"),
    ];
    let mut app = SearchApp::new(test_vim_config(entries, 3));
    app.vim_mode = VimMode::Normal;
    // Start at bottom
    app.table_state.select(Some(2));

    app.handle_input(KeyEvent::from(KeyCode::Char('k')));
    assert_eq!(app.table_state.selected(), Some(1));

    app.handle_input(KeyEvent::from(KeyCode::Char('k')));
    assert_eq!(app.table_state.selected(), Some(0));
}

#[test]
fn test_vim_j_clamps_at_bottom() {
    let entries = vec![create_test_entry("cmd1"), create_test_entry("cmd2")];
    let mut app = SearchApp::new(test_vim_config(entries, 2));
    app.vim_mode = VimMode::Normal;
    app.table_state.select(Some(1)); // already at last

    app.handle_input(KeyEvent::from(KeyCode::Char('j')));
    assert_eq!(app.table_state.selected(), Some(1)); // stays at last
}

#[test]
fn test_vim_k_clamps_at_top() {
    let entries = vec![create_test_entry("cmd1"), create_test_entry("cmd2")];
    let mut app = SearchApp::new(test_vim_config(entries, 2));
    app.vim_mode = VimMode::Normal;
    assert_eq!(app.table_state.selected(), Some(0)); // already at first

    app.handle_input(KeyEvent::from(KeyCode::Char('k')));
    assert_eq!(app.table_state.selected(), Some(0)); // stays at first
}

#[test]
fn test_vim_g_jumps_to_top() {
    let entries = vec![
        create_test_entry("cmd1"),
        create_test_entry("cmd2"),
        create_test_entry("cmd3"),
    ];
    let mut app = SearchApp::new(test_vim_config(entries, 3));
    app.vim_mode = VimMode::Normal;
    app.table_state.select(Some(2));

    app.handle_input(KeyEvent::from(KeyCode::Char('g')));
    assert_eq!(app.table_state.selected(), Some(0));
}

#[test]
fn test_vim_shift_g_jumps_to_bottom() {
    let entries = vec![
        create_test_entry("cmd1"),
        create_test_entry("cmd2"),
        create_test_entry("cmd3"),
    ];
    let mut app = SearchApp::new(test_vim_config(entries, 3));
    app.vim_mode = VimMode::Normal;
    assert_eq!(app.table_state.selected(), Some(0));

    app.handle_input(KeyEvent::from(KeyCode::Char('G')));
    assert_eq!(app.table_state.selected(), Some(2));
}

#[test]
fn test_vim_home_jumps_to_top() {
    let entries = vec![
        create_test_entry("cmd1"),
        create_test_entry("cmd2"),
        create_test_entry("cmd3"),
    ];
    let mut app = SearchApp::new(test_vim_config(entries, 3));
    app.vim_mode = VimMode::Normal;
    app.table_state.select(Some(2));

    app.handle_input(KeyEvent::from(KeyCode::Home));
    assert_eq!(app.table_state.selected(), Some(0));
}

#[test]
fn test_vim_end_jumps_to_bottom() {
    let entries = vec![
        create_test_entry("cmd1"),
        create_test_entry("cmd2"),
        create_test_entry("cmd3"),
    ];
    let mut app = SearchApp::new(test_vim_config(entries, 3));
    app.vim_mode = VimMode::Normal;
    assert_eq!(app.table_state.selected(), Some(0));

    app.handle_input(KeyEvent::from(KeyCode::End));
    assert_eq!(app.table_state.selected(), Some(2));
}

#[test]
fn test_vim_page_down_scrolls_ten_rows() {
    let entries: Vec<Entry> = (0..20)
        .map(|i| create_test_entry(&format!("cmd{i}")))
        .collect();
    let mut app = SearchApp::new(test_vim_config(entries, 20));
    app.vim_mode = VimMode::Normal;
    app.table_state.select(Some(0));

    app.handle_input(KeyEvent::from(KeyCode::PageDown));
    assert_eq!(app.table_state.selected(), Some(10));
}

#[test]
fn test_vim_page_up_scrolls_ten_rows() {
    let entries: Vec<Entry> = (0..20)
        .map(|i| create_test_entry(&format!("cmd{i}")))
        .collect();
    let mut app = SearchApp::new(test_vim_config(entries, 20));
    app.vim_mode = VimMode::Normal;
    app.table_state.select(Some(15));

    app.handle_input(KeyEvent::from(KeyCode::PageUp));
    assert_eq!(app.table_state.selected(), Some(5));
}

#[test]
fn test_vim_enter_selects_in_normal_mode() {
    let entries = vec![create_test_entry("cargo test")];
    let mut app = SearchApp::new(test_vim_config(entries, 1));
    app.vim_mode = VimMode::Normal;

    let action = app.handle_input(KeyEvent::from(KeyCode::Enter));
    match action {
        SearchAction::Select(cmd) => assert_eq!(cmd, "cargo test"),
        other => panic!("Expected Select, got {other:?}"),
    }
}

#[test]
fn test_vim_tab_toggles_detail_in_normal_mode() {
    let entries = vec![create_test_entry("ls")];
    let mut app = SearchApp::new(test_vim_config(entries, 1));
    app.vim_mode = VimMode::Normal;
    let was_open = app.view.detail_pane_open;

    app.handle_input(KeyEvent::from(KeyCode::Tab));
    assert_eq!(app.view.detail_pane_open, !was_open);
}

#[test]
fn test_vim_typing_works_in_insert_mode() {
    let mut app = SearchApp::new(test_vim_config(vec![create_test_entry("ls")], 1));
    assert_eq!(app.vim_mode, VimMode::Insert);

    let action = app.handle_input(KeyEvent::from(KeyCode::Char('g')));
    assert!(matches!(action, SearchAction::Reload));
    assert_eq!(app.query, "g");
}

#[test]
fn test_vim_typing_does_not_work_in_normal_mode() {
    let mut app = SearchApp::new(test_vim_config(vec![create_test_entry("ls")], 1));
    app.vim_mode = VimMode::Normal;

    // 'x' is not a vim binding, should be ignored
    let action = app.handle_input(KeyEvent::from(KeyCode::Char('x')));
    assert!(matches!(action, SearchAction::Continue));
    assert_eq!(app.query, ""); // query unchanged
}

#[test]
fn test_vim_ctrl_u_scrolls_up() {
    let entries: Vec<Entry> = (0..20)
        .map(|i| create_test_entry(&format!("cmd{i}")))
        .collect();
    let mut app = SearchApp::new(test_vim_config(entries, 20));
    app.vim_mode = VimMode::Normal;
    app.table_state.select(Some(15));

    let key = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
    app.handle_input(key);
    // half_page = 20/2 = 10, so 15-10 = 5
    assert_eq!(app.table_state.selected(), Some(5));
}

#[test]
fn test_vim_ctrl_d_scrolls_down() {
    let entries: Vec<Entry> = (0..20)
        .map(|i| create_test_entry(&format!("cmd{i}")))
        .collect();
    let mut app = SearchApp::new(test_vim_config(entries, 20));
    app.vim_mode = VimMode::Normal;
    app.table_state.select(Some(5));

    let key = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
    app.handle_input(key);
    // half_page = 10, so 5+10 = 15
    assert_eq!(app.table_state.selected(), Some(15));
}

#[test]
fn test_vim_ctrl_d_clamps_at_bottom() {
    let entries: Vec<Entry> = (0..10)
        .map(|i| create_test_entry(&format!("cmd{i}")))
        .collect();
    let mut app = SearchApp::new(test_vim_config(entries, 10));
    app.vim_mode = VimMode::Normal;
    app.table_state.select(Some(8));

    let key = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
    app.handle_input(key);
    // half_page = 5, 8+5 = 13, clamped to 9
    assert_eq!(app.table_state.selected(), Some(9));
}

#[test]
fn test_vim_ctrl_u_clamps_at_top() {
    let entries: Vec<Entry> = (0..10)
        .map(|i| create_test_entry(&format!("cmd{i}")))
        .collect();
    let mut app = SearchApp::new(test_vim_config(entries, 10));
    app.vim_mode = VimMode::Normal;
    app.table_state.select(Some(2));

    let key = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
    app.handle_input(key);
    // half_page = 5, 2-5 = saturates to 0
    assert_eq!(app.table_state.selected(), Some(0));
}

#[test]
fn test_vim_h_prev_page() {
    let entries = vec![create_test_entry("cmd")];
    let mut app = SearchApp::new(test_vim_config(entries, 100));
    app.vim_mode = VimMode::Normal;
    app.pagination.page = 3;

    let action = app.handle_input(KeyEvent::from(KeyCode::Char('h')));
    match action {
        SearchAction::SetPage(p) => assert_eq!(p, 2),
        other => panic!("Expected SetPage(2), got {other:?}"),
    }
}

#[test]
fn test_vim_l_next_page() {
    let entries = vec![create_test_entry("cmd")];
    let mut app = SearchApp::new(test_vim_config(entries, 100));
    app.vim_mode = VimMode::Normal;
    app.pagination.page = 1;

    let action = app.handle_input(KeyEvent::from(KeyCode::Char('l')));
    match action {
        SearchAction::SetPage(p) => assert_eq!(p, 2),
        other => panic!("Expected SetPage(2), got {other:?}"),
    }
}

#[test]
fn test_vim_disabled_esc_still_exits() {
    // When vim is NOT enabled, Esc should exit (not switch modes)
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    assert!(!app.vim_enabled);

    let action = app.handle_input(KeyEvent::from(KeyCode::Esc));
    assert!(matches!(action, SearchAction::Exit));
}

#[test]
fn test_vim_mode_roundtrip() {
    let mut app = SearchApp::new(test_vim_config(vec![create_test_entry("ls")], 1));
    assert_eq!(app.vim_mode, VimMode::Insert);

    // Insert → Normal via Esc
    app.handle_input(KeyEvent::from(KeyCode::Esc));
    assert_eq!(app.vim_mode, VimMode::Normal);

    // Normal → Insert via /
    app.handle_input(KeyEvent::from(KeyCode::Char('/')));
    assert_eq!(app.vim_mode, VimMode::Insert);

    // Insert → Normal via Esc again
    app.handle_input(KeyEvent::from(KeyCode::Esc));
    assert_eq!(app.vim_mode, VimMode::Normal);

    // Normal → Insert via i
    app.handle_input(KeyEvent::from(KeyCode::Char('i')));
    assert_eq!(app.vim_mode, VimMode::Insert);
}

// ========================================================================
// Paste handling tests
// ========================================================================

#[test]
fn test_paste_into_empty_query() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    assert!(app.query.is_empty());

    let needs_reload = app.handle_paste("hello world");
    assert!(needs_reload);
    assert_eq!(app.query, "hello world");
}

#[test]
fn test_paste_appends_to_existing_query() {
    let mut cfg = test_search_config(vec![create_test_entry("ls")], 1);
    cfg.initial_query = Some("git ".to_string());
    let mut app = SearchApp::new(cfg);

    let needs_reload = app.handle_paste("status");
    assert!(needs_reload);
    assert_eq!(app.query, "git status");
}

#[test]
fn test_paste_strips_control_characters() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));

    // Paste text with newlines, tabs, and other control chars. Newlines and
    // tabs separated words in the source, so they become spaces (PROD-09):
    // dropping them welded "cargo test" + "--offline" into one unmatchable
    // token. Genuine control bytes are still removed.
    let needs_reload = app.handle_paste("hello\nworld\t!\x00\x07");
    assert!(needs_reload);
    assert_eq!(app.query, "hello world !");
}

#[test]
fn test_paste_collapses_whitespace_and_trims() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    // A trailing newline must not leave a trailing space for `literal` mode
    // to have to match.
    assert!(app.handle_paste("  cargo   test \n"));
    assert_eq!(app.query, "cargo test");
}

#[test]
fn test_paste_preserves_spaces() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));

    let needs_reload = app.handle_paste("cargo build --release");
    assert!(needs_reload);
    assert_eq!(app.query, "cargo build --release");
}

#[test]
fn test_paste_respects_max_input_len() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));

    // Paste a very long string
    let long_string: String = "a".repeat(3000);
    let needs_reload = app.handle_paste(&long_string);
    assert!(needs_reload);
    assert_eq!(app.query.len(), 2000); // MAX_INPUT_LEN
}

#[test]
fn test_paste_respects_remaining_capacity() {
    let mut cfg = test_search_config(vec![create_test_entry("ls")], 1);
    cfg.initial_query = Some("x".repeat(1995));
    let mut app = SearchApp::new(cfg);

    let needs_reload = app.handle_paste("abcdefghij"); // 10 chars, only 5 fit
    assert!(needs_reload);
    assert_eq!(app.query.len(), 2000);
    assert!(app.query.ends_with("abcde"));
}

#[test]
fn test_paste_empty_string() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));

    let needs_reload = app.handle_paste("");
    assert!(!needs_reload);
    assert!(app.query.is_empty());
}

#[test]
fn test_paste_only_control_chars() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));

    let needs_reload = app.handle_paste("\n\r\t\x00\x07");
    assert!(!needs_reload);
    assert!(app.query.is_empty());
}

#[test]
fn test_paste_in_vim_normal_mode_switches_to_insert() {
    let mut app = SearchApp::new(test_vim_config(vec![create_test_entry("ls")], 1));
    app.vim_mode = VimMode::Normal;

    let needs_reload = app.handle_paste("search term");
    assert!(needs_reload);
    assert_eq!(app.vim_mode, VimMode::Insert);
    assert_eq!(app.query, "search term");
}

#[test]
fn test_paste_into_note_dialog() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Note {
        entry_id: 10,
        input: "existing ".to_string(),
    };

    let needs_reload = app.handle_paste("note text");
    assert!(!needs_reload); // Note paste doesn't trigger query reload
    if let DialogState::Note { ref input, .. } = app.dialog {
        assert_eq!(input, "existing note text");
    } else {
        panic!("Expected Note dialog");
    }
}

#[test]
fn test_paste_into_goto_dialog_digits_only() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::GoToPage {
        input: String::new(),
    };

    let needs_reload = app.handle_paste("page 42!");
    assert!(!needs_reload);
    if let DialogState::GoToPage { ref input } = app.dialog {
        assert_eq!(input, "42"); // Only digits kept
    } else {
        panic!("Expected GoToPage dialog");
    }
}

#[test]
fn test_paste_into_filter_start_date() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Filter;
    app.filters.focus_index = 0;
    app.filters.start_date_input.clear();

    let needs_reload = app.handle_paste("2026-04-01");
    assert!(!needs_reload);
    assert_eq!(app.filters.start_date_input, "2026-04-01");
}

#[test]
fn test_paste_into_filter_end_date() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Filter;
    app.filters.focus_index = 1;
    app.filters.end_date_input.clear();

    let needs_reload = app.handle_paste("2026-04-16");
    assert!(!needs_reload);
    assert_eq!(app.filters.end_date_input, "2026-04-16");
}

#[test]
fn test_paste_into_filter_tag() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Filter;
    app.filters.focus_index = 2;

    let needs_reload = app.handle_paste("deploy");
    assert!(!needs_reload);
    assert!(app.filters.tag_filter_input.contains("deploy"));
}

#[test]
fn test_paste_into_filter_exit_code() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Filter;
    app.filters.focus_index = 3;
    app.filters.exit_code_input.clear();

    let needs_reload = app.handle_paste("127");
    assert!(!needs_reload);
    assert_eq!(app.filters.exit_code_input, "127");
}

#[test]
fn test_paste_into_filter_executor_selector_ignored() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Filter;
    app.filters.focus_index = 4; // executor selector
    app.filters.executor_sel = 0;

    let needs_reload = app.handle_paste("agent");
    assert!(!needs_reload);
    assert_eq!(app.filters.executor_sel, 0); // unchanged
}

#[test]
fn test_paste_in_help_dialog_ignored() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Help;

    let needs_reload = app.handle_paste("anything");
    assert!(!needs_reload);
    assert!(app.query.is_empty());
}

#[test]
fn test_paste_in_delete_dialog_ignored() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));
    app.dialog = DialogState::Delete { entry_id: 1 };

    let needs_reload = app.handle_paste("anything");
    assert!(!needs_reload);
    assert!(app.query.is_empty());
}

#[test]
fn test_paste_unicode() {
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry("ls")], 1));

    let needs_reload = app.handle_paste("git log --author=\"\u{00e9}mile\"");
    assert!(needs_reload);
    assert_eq!(app.query, "git log --author=\"\u{00e9}mile\"");
}

// ── complete-history matching ──────────────────────────────────────
// Typed search used to fetch the newest 5,000 eligible rows and match in
// memory, so an exact match older than that simply vanished from the UI
// while still sitting in the database. Reproduction from the 19 Sep 2026
// audit, extended to the unique-mode branch that shares the same path.

/// Fills a repository with `total` entries, oldest first, where only the
/// oldest one contains `needle`.
fn repo_with_old_match(
    total: usize,
    needle: &str,
) -> (tempfile::TempDir, crate::repository::Repository) {
    let (dir, repo) = crate::test_utils::test_repo();
    repo.insert_session(&crate::models::Session {
        id: "session123".into(),
        hostname: "test".into(),
        created_at: 1000,
        tag_id: None,
    })
    .unwrap();
    for i in 0..i64::try_from(total).unwrap() {
        let cmd = if i == 0 {
            needle.to_string()
        } else {
            format!("echo recent_{i}")
        };
        let mut entry = create_test_entry(&cmd);
        entry.started_at = 1000 + i;
        entry.ended_at = 2000 + i;
        repo.insert_entry(&entry).unwrap();
    }
    (dir, repo)
}

#[test]
fn typed_search_finds_an_exact_match_older_than_the_candidate_window() {
    let (_dir, repo) = repo_with_old_match(5001, "echo audit_rare_old_command");

    let mut app = SearchApp::new(test_search_config(vec![], 5001));
    app.query = "audit_rare_old_command".into();
    app.reload_entries(&repo).unwrap();

    assert_eq!(
        app.entries.len(),
        1,
        "an exact older command must stay discoverable in interactive search"
    );
    assert_eq!(app.entries[0].command, "echo audit_rare_old_command");
}

#[test]
fn unique_mode_finds_an_exact_match_older_than_the_candidate_window() {
    let (_dir, repo) = repo_with_old_match(5001, "echo audit_rare_old_command");

    let mut config = test_search_config(vec![], 5001);
    config.view.unique_mode = true;
    let mut app = SearchApp::new(config);
    app.query = "audit_rare_old_command".into();
    app.reload_entries(&repo).unwrap();

    assert_eq!(
        app.entries.len(),
        1,
        "unique mode shares the same candidate window"
    );
}

/// Fills a repository with `newer` filler entries after one old `needle`.
fn repo_with_old_command(
    needle: &str,
    newer: usize,
) -> (tempfile::TempDir, crate::repository::Repository) {
    let (dir, repo) = crate::test_utils::test_repo();
    repo.insert_session(&crate::models::Session {
        id: "session123".into(),
        hostname: "test".into(),
        created_at: 1000,
        tag_id: None,
    })
    .unwrap();
    let mut oldest = create_test_entry(needle);
    oldest.started_at = 1000;
    oldest.ended_at = 2000;
    repo.insert_entry(&oldest).unwrap();
    for i in 1..=i64::try_from(newer).unwrap() {
        let mut entry = create_test_entry(&format!("echo recent_{i}"));
        entry.started_at = 1000 + i;
        entry.ended_at = 2000 + i;
        repo.insert_entry(&entry).unwrap();
    }
    (dir, repo)
}

fn search_old(repo: &crate::repository::Repository, query: &str) -> Vec<String> {
    let mut app = SearchApp::new(test_search_config(vec![], 5001));
    app.query = query.into();
    app.reload_entries(repo).unwrap();
    app.entries.iter().map(|e| e.command.clone()).collect()
}

#[test]
fn multi_token_queries_match_an_old_command_in_any_order() {
    let (_dir, repo) = repo_with_old_command("git commit --amend -m wip", 5000);

    assert_eq!(
        search_old(&repo, "git amend"),
        vec!["git commit --amend -m wip"]
    );
    assert_eq!(
        search_old(&repo, "amend git"),
        vec!["git commit --amend -m wip"]
    );
    // Every token must still be present: one absent token means no match.
    assert!(search_old(&repo, "git rebase").is_empty());
}

#[test]
fn short_tokens_still_match_an_old_command() {
    let (_dir, repo) = repo_with_old_command("cd /srv/xy", 5000);
    assert_eq!(search_old(&repo, "xy"), vec!["cd /srv/xy"]);
}

#[test]
fn like_wildcards_in_a_query_are_matched_literally() {
    let (_dir, repo) = repo_with_old_command("echo 100%_done", 5000);

    assert_eq!(search_old(&repo, "100%_done"), vec!["echo 100%_done"]);
    // `%` and `_` must not behave as SQL wildcards.
    assert!(search_old(&repo, "100%ZZ_done").is_empty());
}

#[test]
fn an_old_command_matches_case_insensitively() {
    let (_dir, repo) = repo_with_old_command("echo RARE_OLD_Command", 5000);
    assert_eq!(
        search_old(&repo, "rare_old_command"),
        vec!["echo RARE_OLD_Command"]
    );
}

#[test]
fn an_old_command_matches_case_insensitively_beyond_ascii() {
    let (_dir, repo) = repo_with_old_command("echo Émile_rare_old", 5000);
    assert_eq!(
        search_old(&repo, "émile_rare_old"),
        vec!["echo Émile_rare_old"]
    );
}

#[test]
fn filters_still_exclude_an_old_command_that_matches_the_text() {
    let (dir, repo) = crate::test_utils::test_repo();
    repo.insert_session(&crate::models::Session {
        id: "session123".into(),
        hostname: "test".into(),
        created_at: 1000,
        tag_id: None,
    })
    .unwrap();
    let mut agent_entry = create_test_entry("echo rare_agent_command");
    agent_entry.executor_type = Some("agent".into());
    agent_entry.executor = Some("claude-code".into());
    agent_entry.started_at = 1000;
    agent_entry.ended_at = 2000;
    repo.insert_entry(&agent_entry).unwrap();
    for i in 1..=5000 {
        let mut entry = create_test_entry(&format!("echo recent_{i}"));
        entry.started_at = 1000 + i;
        entry.ended_at = 2000 + i;
        repo.insert_entry(&entry).unwrap();
    }

    // Agent commands are hidden by default, however old the match is.
    assert!(search_old(&repo, "rare_agent_command").is_empty());

    let mut app = SearchApp::new(test_search_config(vec![], 5001));
    app.filters.show_agents = true;
    app.query = "rare_agent_command".into();
    app.reload_entries(&repo).unwrap();
    assert_eq!(
        app.entries.len(),
        1,
        "showing agents must reveal the old match"
    );
    drop(dir);
}

// ── broad queries: complete counting and reachable pages (R01) ─────
// The fixtures above plant one rare match behind thousands of *non*-matching
// rows, which only proves SQL narrowing reaches back far enough. These plant
// thousands of *eligible* matches with the sought one oldest, which is what
// exposes a ranking window being mistaken for the whole result set.

/// A repository where `needle` is the oldest entry and every one of the
/// `filler` newer entries also matches the same broad query.
fn repo_with_broad_matches(
    needle: &str,
    filler_template: &str,
    filler: usize,
) -> (tempfile::TempDir, crate::repository::Repository) {
    let (dir, repo) = crate::test_utils::test_repo();
    repo.insert_session(&crate::models::Session {
        id: "session123".into(),
        hostname: "test".into(),
        created_at: 1000,
        tag_id: None,
    })
    .unwrap();
    let mut oldest = create_test_entry(needle);
    oldest.started_at = 1000;
    oldest.ended_at = 2000;
    repo.insert_entry(&oldest).unwrap();
    for i in 1..=i64::try_from(filler).unwrap() {
        let mut entry = create_test_entry(&format!("{filler_template}{i}"));
        entry.started_at = 1000 + i;
        entry.ended_at = 2000 + i;
        repo.insert_entry(&entry).unwrap();
    }
    (dir, repo)
}

/// Walk every page the app claims to have and return what it showed.
fn walk_every_page(app: &mut SearchApp, repo: &crate::repository::Repository) -> Vec<String> {
    let pages = app
        .pagination
        .total_items
        .div_ceil(app.pagination.page_size);
    let mut seen = Vec::new();
    for page in 1..=pages {
        app.set_page(repo, page).unwrap();
        seen.extend(app.entries.iter().map(|e| e.command.clone()));
    }
    seen
}

#[test]
fn a_broad_query_reports_a_total_it_can_page_to() {
    // 5,001 eligible matches for "git": more than any ranking window.
    let (_d, repo) = repo_with_broad_matches("git", "echo git filler", 5000);

    let mut app = SearchApp::new(test_search_config(vec![], 0));
    app.query = "git".into();
    app.reload_entries(&repo).unwrap();

    assert_eq!(
        app.pagination.total_items, 5001,
        "the total must be the count of eligible matches, not of ranked ones"
    );

    let seen = walk_every_page(&mut app, &repo);
    assert_eq!(
        seen.len(),
        5001,
        "every claimed page must yield its entries"
    );
    assert!(
        seen.contains(&"git".to_string()),
        "the oldest exact match must be reachable by paging"
    );
    let unique: std::collections::HashSet<&String> = seen.iter().collect();
    assert_eq!(unique.len(), 5001, "paging must not repeat an entry");
}

#[test]
fn a_broad_unique_query_reports_a_total_it_can_page_to() {
    let (_d, repo) = repo_with_broad_matches("git", "echo git filler", 5000);

    let mut config = test_search_config(vec![], 0);
    config.view.unique_mode = true;
    let mut app = SearchApp::new(config);
    app.query = "git".into();
    app.reload_entries(&repo).unwrap();

    assert_eq!(
        app.pagination.total_items, 5001,
        "unique mode must count every distinct eligible command"
    );

    let seen = walk_every_page(&mut app, &repo);
    assert!(
        seen.contains(&"git".to_string()),
        "unique mode must reach the oldest exact match"
    );
    let unique: std::collections::HashSet<&String> = seen.iter().collect();
    assert_eq!(
        unique.len(),
        5001,
        "unique paging must not repeat a command"
    );
}

#[test]
fn a_broad_fuzzy_query_reports_a_total_it_can_page_to() {
    // Every entry is a "gco" subsequence match; the sought one is the oldest.
    let (_d, repo) = repo_with_broad_matches("git checkout", "echo git checkout filler", 5000);

    let mut app = SearchApp::new(test_search_config(vec![], 0));
    app.recall.match_mode = MatchMode::Fuzzy;
    app.query = "gco".into();
    app.reload_entries(&repo).unwrap();

    assert_eq!(
        app.pagination.total_items, 5001,
        "fuzzy must count the entries it would actually accept"
    );

    let seen = walk_every_page(&mut app, &repo);
    assert!(
        seen.contains(&"git checkout".to_string()),
        "fuzzy must reach the oldest abbreviation match"
    );
    let unique: std::collections::HashSet<&String> = seen.iter().collect();
    assert_eq!(unique.len(), 5001, "fuzzy paging must not repeat an entry");
}

#[test]
fn a_fuzzy_total_never_counts_a_candidate_the_mode_would_reject() {
    // SQL narrows fuzzy by distinct characters, which admits far more than
    // the subsequence rule accepts. The reported total must be the accepted
    // set, or the footer promises pages that hold nothing.
    let (_d, repo) = repo_with(&["git checkout", "ocg", "cog", "go cook"]);

    let mut app = SearchApp::new(test_search_config(vec![], 0));
    app.recall.match_mode = MatchMode::Fuzzy;
    app.query = "gco".into();
    app.reload_entries(&repo).unwrap();

    let expected = ["git checkout", "go cook"];
    assert_eq!(
        app.pagination.total_items,
        expected.len(),
        "total counted rejected candidates: showed {:?}",
        app.entries.iter().map(|e| &e.command).collect::<Vec<_>>()
    );
    assert_eq!(app.entries.len(), expected.len());
}

/// Rough guard that matching across the whole history stays interactive.
/// Ignored by default: it builds a 100k-entry database. Run with
/// `cargo test --release --bin suv typed_search_latency -- --ignored --nocapture`.
/// PROD-10 replaces this with a proper benchmark suite.
#[test]
#[ignore = "builds a 100k-entry database; run explicitly with --ignored"]
fn typed_search_latency_on_a_large_history() {
    let (_dir, repo) = repo_with_old_command("cargo test --workspace rare_old", 100_000);

    for query in ["cargo", "cargo test", "rare_old", "workspace rare_old"] {
        let mut app = SearchApp::new(test_search_config(vec![], 100_001));
        app.query = query.into();
        let start = std::time::Instant::now();
        app.reload_entries(&repo).unwrap();
        println!(
            "query {query:?}: {} results in {:?}",
            app.pagination.total_items,
            start.elapsed()
        );
    }
}

#[test]
#[ignore = "builds a 100k-entry database; run explicitly with --ignored"]
fn matching_mode_latency_on_a_large_history() {
    // Fuzzy narrows by single characters, which no trigram index can serve,
    // so it is expected to be the slowest mode. This prints the cost rather
    // than asserting it: latency is machine-dependent.
    let (_dir, repo) = repo_with_old_command("cargo test --workspace rare_old", 100_000);

    for (mode, query) in [
        (MatchMode::Terms, "workspace rare_old"),
        (MatchMode::Literal, "workspace rare_old"),
        (MatchMode::Prefix, "cargo test"),
        (MatchMode::Fuzzy, "wrkspc"),
    ] {
        let mut app = SearchApp::new(test_search_config(vec![], 100_001));
        app.recall.match_mode = mode;
        app.query = query.into();
        let start = std::time::Instant::now();
        app.reload_entries(&repo).unwrap();
        println!(
            "{:8} {query:?}: {} results in {:?}",
            mode.label(),
            app.pagination.total_items,
            start.elapsed()
        );
    }
}
// ───────────────────────────────────────────────────────────────────────────
// PROD-09: the matching modes and the recall scopes, end to end against a
// real database. These are the behaviour the docs and the website promise.
// ───────────────────────────────────────────────────────────────────────────

/// A repository holding exactly `commands`, newest last.
fn repo_with(commands: &[&str]) -> (tempfile::TempDir, crate::repository::Repository) {
    let (dir, repo) = crate::test_utils::test_repo();
    repo.insert_session(&crate::models::Session {
        id: "session123".into(),
        hostname: "test".into(),
        created_at: 1000,
        tag_id: None,
    })
    .unwrap();
    for (i, cmd) in commands.iter().enumerate() {
        let mut entry = create_test_entry(cmd);
        entry.started_at = 1000 + i64::try_from(i).unwrap();
        entry.ended_at = 2000 + i64::try_from(i).unwrap();
        repo.insert_entry(&entry).unwrap();
    }
    (dir, repo)
}

/// Run `query` in `mode` against `repo` and return the matching commands.
fn search_in_mode(
    repo: &crate::repository::Repository,
    mode: MatchMode,
    query: &str,
) -> Vec<String> {
    let mut app = SearchApp::new(test_search_config(vec![], 0));
    app.recall.match_mode = mode;
    app.query = query.into();
    app.reload_entries(repo).unwrap();
    app.entries.iter().map(|e| e.command.clone()).collect()
}

const MODE_CORPUS: &[&str] = &[
    "git checkout main",
    "git commit --amend",
    "go container ops",
    "cargo test --offline",
    "ls -la",
];

#[test]
fn literal_and_fuzzy_disagree_end_to_end_exactly_as_documented() {
    let (_d, repo) = repo_with(MODE_CORPUS);

    // "gco" is an abbreviation. Only fuzzy is allowed to resolve it.
    assert!(search_in_mode(&repo, MatchMode::Terms, "gco").is_empty());
    assert!(search_in_mode(&repo, MatchMode::Literal, "gco").is_empty());
    assert!(search_in_mode(&repo, MatchMode::Prefix, "gco").is_empty());
    let fuzzy = search_in_mode(&repo, MatchMode::Fuzzy, "gco");
    assert!(
        fuzzy.contains(&"git checkout main".to_string()),
        "fuzzy should resolve the abbreviation: {fuzzy:?}"
    );
}

#[test]
fn terms_matches_scattered_words_where_literal_demands_the_phrase() {
    let (_d, repo) = repo_with(MODE_CORPUS);

    // The words are present but not adjacent.
    assert_eq!(
        search_in_mode(&repo, MatchMode::Terms, "git main"),
        vec!["git checkout main".to_string()]
    );
    assert!(search_in_mode(&repo, MatchMode::Literal, "git main").is_empty());

    // The phrase itself is found by both.
    for mode in [MatchMode::Terms, MatchMode::Literal] {
        assert_eq!(
            search_in_mode(&repo, mode, "checkout main"),
            vec!["git checkout main".to_string()],
            "{mode:?}"
        );
    }
}

#[test]
fn prefix_anchors_at_the_start_of_the_command_end_to_end() {
    let (_d, repo) = repo_with(MODE_CORPUS);

    let prefixed = search_in_mode(&repo, MatchMode::Prefix, "git");
    assert_eq!(prefixed.len(), 2, "{prefixed:?}");
    assert!(prefixed.iter().all(|c| c.starts_with("git")));

    // "test" appears mid-command, so prefix finds nothing while terms does.
    assert!(search_in_mode(&repo, MatchMode::Prefix, "test").is_empty());
    assert_eq!(
        search_in_mode(&repo, MatchMode::Terms, "test"),
        vec!["cargo test --offline".to_string()]
    );
}

#[test]
fn case_and_punctuation_behave_as_the_help_describes() {
    let (_d, repo) = repo_with(MODE_CORPUS);
    // ASCII case folds.
    assert_eq!(
        search_in_mode(&repo, MatchMode::Literal, "GIT CHECKOUT"),
        vec!["git checkout main".to_string()]
    );
    // Punctuation is matched, never stripped.
    assert_eq!(
        search_in_mode(&repo, MatchMode::Terms, "--amend"),
        vec!["git commit --amend".to_string()]
    );
    // Quotes are ordinary characters, so this finds nothing.
    assert!(search_in_mode(&repo, MatchMode::Terms, "\"git checkout\"").is_empty());
}

#[test]
fn every_mode_finds_a_match_older_than_any_candidate_window() {
    // The needle is the oldest of 5,001 entries: older than the in-memory
    // ranking window, so only real SQL narrowing can find it in every mode.
    let (_d, repo) = repo_with_old_match(5001, "echo audit_rare_old_command");

    for (mode, query) in [
        (MatchMode::Terms, "audit_rare_old_command"),
        (MatchMode::Literal, "audit_rare_old_command"),
        (MatchMode::Prefix, "echo audit_rare"),
        (MatchMode::Fuzzy, "audit_rare_old_command"),
    ] {
        let found = search_in_mode(&repo, mode, query);
        assert!(
            found.contains(&"echo audit_rare_old_command".to_string()),
            "{mode:?} lost an old match ({} results)",
            found.len()
        );
    }
}

// ── fuzzy obeys its own ordering rule (R10) ───────────────────────

#[test]
fn fuzzy_rejects_a_query_whose_words_are_present_but_out_of_order() {
    let (_d, repo) = repo_with(&["git checkout"]);

    // "checkout git" is not a subsequence of "git checkout", so fuzzy — whose
    // help promises "letters in order, gaps allowed" — must reject it.
    assert!(
        search_in_mode(&repo, MatchMode::Fuzzy, "checkout git").is_empty(),
        "fuzzy accepted an out-of-order query"
    );
    // `terms` keeps unordered token matching; that difference is the point.
    assert_eq!(
        search_in_mode(&repo, MatchMode::Terms, "checkout git"),
        vec!["git checkout".to_string()]
    );
}

#[test]
fn fuzzy_eligibility_does_not_depend_on_the_ranking_tier() {
    // Each of these queries has at least one literal token in the command, so
    // the ranking tier is non-zero — but none is a whole-query subsequence.
    let (_d, repo) = repo_with(&["git checkout main", "cargo test --offline"]);

    for query in ["checkout git", "main git", "offline cargo", "test cargo"] {
        assert!(
            search_in_mode(&repo, MatchMode::Fuzzy, query).is_empty(),
            "fuzzy accepted {query:?} because its tier was non-zero"
        );
    }
}

/// Every mode's live result set must equal its documented predicate.
#[test]
fn every_mode_agrees_end_to_end_with_its_reference_predicate() {
    const CORPUS: &[&str] = &[
        "git checkout main",
        "git commit --amend",
        "go container ops",
        "cargo test --offline",
        "ls -la",
        "ocg",
        "echo GIT",
    ];
    let (_d, repo) = repo_with(CORPUS);

    for mode in [
        MatchMode::Terms,
        MatchMode::Literal,
        MatchMode::Prefix,
        MatchMode::Fuzzy,
    ] {
        for query in [
            "gco",
            "git",
            "GIT",
            "checkout git",
            "git checkout",
            "cargo test",
            "test cargo",
            "ls",
        ] {
            let mut expected: Vec<String> = CORPUS
                .iter()
                .filter(|c| mode.matches(c, query))
                .map(|c| (*c).to_string())
                .collect();
            expected.sort();
            let mut got = search_in_mode(&repo, mode, query);
            got.sort();
            assert_eq!(got, expected, "{mode:?} with {query:?}");
        }
    }
}

/// A repository whose entries live in three directories and two sessions.
fn repo_for_scopes(root: &std::path::Path) -> (tempfile::TempDir, crate::repository::Repository) {
    let (dir, repo) = crate::test_utils::test_repo();
    for sid in ["sess-1", "sess-2"] {
        repo.insert_session(&crate::models::Session {
            id: sid.into(),
            hostname: "test".into(),
            created_at: 1000,
            tag_id: None,
        })
        .unwrap();
    }
    let here = root.join("src").to_string_lossy().into_owned();
    let sibling = root.join("docs").to_string_lossy().into_owned();
    let elsewhere = "/somewhere/else".to_string();
    for (i, (cmd, cwd, sid)) in [
        ("cargo build here", &here, "sess-1"),
        ("cargo build sibling", &sibling, "sess-1"),
        ("cargo build elsewhere", &elsewhere, "sess-1"),
        ("cargo build other session", &here, "sess-2"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut entry = create_test_entry(cmd);
        entry.cwd = cwd.clone();
        entry.session_id = sid.to_string();
        entry.started_at = 1000 + i64::try_from(i).unwrap();
        entry.ended_at = 2000 + i64::try_from(i).unwrap();
        repo.insert_entry(&entry).unwrap();
    }
    (dir, repo)
}

/// An app whose context points into `root`'s worktree, session `sess-1`.
fn scoped_app(root: &std::path::Path, ceiling: &std::path::Path) -> SearchApp {
    let mut app = SearchApp::new(test_search_config(vec![], 0));
    app.recall.context = RecallContext::resolve_at(
        Some(&root.join("src")),
        Some("sess-1".to_string()),
        Some(ceiling),
    );
    app.sync_scope_filters();
    app
}

fn results(app: &SearchApp) -> Vec<String> {
    app.entries.iter().map(|e| e.command.clone()).collect()
}

#[test]
fn each_scope_returns_exactly_the_slice_of_history_it_names() {
    let (fixture, root) = workspace_fixture();
    let canonical = std::fs::canonicalize(&root).unwrap();
    let (_d, repo) = repo_for_scopes(&canonical);
    let mut app = scoped_app(&canonical, fixture.path());

    app.set_scope(RecallScope::All);
    app.reload_entries(&repo).unwrap();
    assert_eq!(results(&app).len(), 4, "all history: {:?}", results(&app));

    app.set_scope(RecallScope::Directory);
    app.reload_entries(&repo).unwrap();
    let dir_results = results(&app);
    assert_eq!(dir_results.len(), 2, "this directory: {dir_results:?}");
    assert!(dir_results.contains(&"cargo build here".to_string()));
    assert!(dir_results.contains(&"cargo build other session".to_string()));

    // The workspace is the whole project tree, so the sibling directory
    // joins in but the unrelated path does not.
    app.set_scope(RecallScope::Workspace);
    app.reload_entries(&repo).unwrap();
    let ws_results = results(&app);
    assert_eq!(ws_results.len(), 3, "workspace: {ws_results:?}");
    assert!(ws_results.contains(&"cargo build sibling".to_string()));
    assert!(!ws_results.contains(&"cargo build elsewhere".to_string()));

    app.set_scope(RecallScope::Session);
    app.reload_entries(&repo).unwrap();
    let session_results = results(&app);
    assert_eq!(session_results.len(), 3, "session: {session_results:?}");
    assert!(!session_results.contains(&"cargo build other session".to_string()));
}

#[test]
fn resetting_the_scope_brings_the_whole_history_back_in_one_action() {
    let (fixture, root) = workspace_fixture();
    let canonical = std::fs::canonicalize(&root).unwrap();
    let (_d, repo) = repo_for_scopes(&canonical);
    let mut app = scoped_app(&canonical, fixture.path());

    app.set_scope(RecallScope::Directory);
    app.filters.failed_only = true;
    app.reload_entries(&repo).unwrap();
    assert!(app.entries.is_empty(), "narrowed away to nothing");

    app.reset_to_all_history();
    app.reload_entries(&repo).unwrap();
    assert_eq!(results(&app).len(), 4);
}

#[test]
fn a_scope_keeps_its_meaning_even_if_the_repository_moves_underneath() {
    let (fixture, root) = workspace_fixture();
    let canonical = std::fs::canonicalize(&root).unwrap();
    let (_d, repo) = repo_for_scopes(&canonical);
    let mut app = scoped_app(&canonical, fixture.path());
    app.set_scope(RecallScope::Workspace);
    app.reload_entries(&repo).unwrap();
    let before = results(&app);

    // The boundary was resolved once, at the start. Deleting it must not
    // silently turn "workspace" into "all history" mid-search.
    std::fs::remove_dir_all(canonical.join(".git")).unwrap();
    app.reload_entries(&repo).unwrap();
    assert_eq!(results(&app), before);
}

#[test]
fn a_nested_repository_scopes_to_itself_not_its_parent() {
    let (fixture, outer) = workspace_fixture();
    let inner = outer.join("vendor/inner");
    std::fs::create_dir_all(inner.join(".git")).unwrap();
    std::fs::create_dir_all(inner.join("src")).unwrap();
    let canonical_outer = std::fs::canonicalize(&outer).unwrap();
    let canonical_inner = std::fs::canonicalize(&inner).unwrap();

    let (_d, repo) = crate::test_utils::test_repo();
    repo.insert_session(&crate::models::Session {
        id: "session123".into(),
        hostname: "test".into(),
        created_at: 1000,
        tag_id: None,
    })
    .unwrap();
    for (i, (cmd, cwd)) in [
        ("cargo build outer", canonical_outer.join("src")),
        ("cargo build inner", canonical_inner.join("src")),
    ]
    .into_iter()
    .enumerate()
    {
        let mut entry = create_test_entry(cmd);
        entry.cwd = cwd.to_string_lossy().into_owned();
        entry.started_at = 1000 + i64::try_from(i).unwrap();
        repo.insert_entry(&entry).unwrap();
    }

    let mut app = SearchApp::new(test_search_config(vec![], 0));
    app.recall.context = RecallContext::resolve_at(
        Some(&canonical_inner.join("src")),
        None,
        Some(fixture.path()),
    );
    app.set_scope(RecallScope::Workspace);
    app.reload_entries(&repo).unwrap();
    assert_eq!(results(&app), vec!["cargo build inner".to_string()]);
}

#[test]
fn a_linked_worktree_scopes_to_the_worktree_not_the_main_repository() {
    let (fixture, main) = workspace_fixture();
    let wt = fixture.path().join("wt-feature");
    std::fs::create_dir_all(wt.join("src")).unwrap();
    std::fs::write(
        wt.join(".git"),
        format!("gitdir: {}/.git/worktrees/wt-feature\n", main.display()),
    )
    .unwrap();
    let canonical_main = std::fs::canonicalize(&main).unwrap();
    let canonical_wt = std::fs::canonicalize(&wt).unwrap();

    let (_d, repo) = crate::test_utils::test_repo();
    repo.insert_session(&crate::models::Session {
        id: "session123".into(),
        hostname: "test".into(),
        created_at: 1000,
        tag_id: None,
    })
    .unwrap();
    for (i, (cmd, cwd)) in [
        ("cargo build main", canonical_main.join("src")),
        ("cargo build worktree", canonical_wt.join("src")),
    ]
    .into_iter()
    .enumerate()
    {
        let mut entry = create_test_entry(cmd);
        entry.cwd = cwd.to_string_lossy().into_owned();
        entry.started_at = 1000 + i64::try_from(i).unwrap();
        repo.insert_entry(&entry).unwrap();
    }

    let mut app = SearchApp::new(test_search_config(vec![], 0));
    app.recall.context =
        RecallContext::resolve_at(Some(&canonical_wt.join("src")), None, Some(fixture.path()));
    app.set_scope(RecallScope::Workspace);
    app.reload_entries(&repo).unwrap();
    assert_eq!(results(&app), vec!["cargo build worktree".to_string()]);
}

#[test]
fn no_mode_or_scope_ever_reveals_hidden_agent_commands() {
    let (_d, repo) = crate::test_utils::test_repo();
    repo.insert_session(&crate::models::Session {
        id: "session123".into(),
        hostname: "test".into(),
        created_at: 1000,
        tag_id: None,
    })
    .unwrap();
    let mut human = create_test_entry("cargo build human");
    human.started_at = 1000;
    repo.insert_entry(&human).unwrap();
    let mut agent = create_test_entry("cargo build agent");
    agent.executor_type = Some("agent".to_string());
    agent.executor = Some("claude-code".to_string());
    agent.started_at = 1001;
    repo.insert_entry(&agent).unwrap();

    for mode in [
        MatchMode::Terms,
        MatchMode::Literal,
        MatchMode::Prefix,
        MatchMode::Fuzzy,
    ] {
        let found = search_in_mode(&repo, mode, "cargo build");
        assert!(
            !found.contains(&"cargo build agent".to_string()),
            "{mode:?} leaked an agent command: {found:?}"
        );
    }

    // ...and they are one discoverable key away.
    let mut app = SearchApp::new(test_search_config(vec![], 0));
    app.query = "cargo build".into();
    app.handle_input(ctrl_key('a'));
    app.reload_entries(&repo).unwrap();
    assert_eq!(results(&app).len(), 2, "^A must include them");
}

#[test]
fn pasted_multiline_text_becomes_one_searchable_query_line() {
    let (_d, repo) = repo_with(MODE_CORPUS);
    let mut app = SearchApp::new(test_search_config(vec![], 0));

    // A paste of a wrapped command: the newlines must not end up in the
    // query, and the search must still run.
    assert!(app.handle_paste("cargo test\n--offline\n"));
    assert!(!app.query.contains('\n'), "query: {:?}", app.query);
    app.reload_entries(&repo).unwrap();
    assert_eq!(app.recall.match_mode, MatchMode::Terms);
    assert_eq!(
        results(&app),
        vec!["cargo test --offline".to_string()],
        "query was {:?}",
        app.query
    );
}

#[test]
fn cancelling_returns_nothing_so_the_shell_keeps_its_buffer() {
    let (_d, _repo) = repo_with(MODE_CORPUS);
    let mut app = SearchApp::new(test_search_config(
        vec![create_test_entry("rm -rf /important")],
        1,
    ));
    // Cycling modes and scopes must never accept anything on the way.
    for key in ['x', 'p', 'r', 'l', 'a'] {
        let action = app.handle_input(ctrl_key(key));
        assert!(
            !matches!(action, SearchAction::Select(_) | SearchAction::Exit),
            "^{key} must not accept or quit"
        );
    }
    assert!(matches!(
        app.handle_input(KeyEvent::from(KeyCode::Esc)),
        SearchAction::Exit
    ));
}

#[test]
fn accepting_hands_back_the_exact_command_and_nothing_else() {
    let mut app = SearchApp::new(test_search_config(
        vec![create_test_entry("rm -rf /important")],
        1,
    ));
    app.table_state.select(Some(0));
    let SearchAction::Select(cmd) = app.handle_input(KeyEvent::from(KeyCode::Enter)) else {
        panic!("Enter must select the highlighted command");
    };
    // Exactly the recorded text: no added newline, no shell wrapping. The
    // caller prints it for the shell to place on the line; nothing here runs.
    assert_eq!(cmd, "rm -rf /important");
}

// ───────────────────────────────────────────────────────────────────────────
// PROD-03: recall UI hierarchy — footer hint layout, persistent status area,
// detail-pane placement. These tests render the whole screen into a ratatui
// `TestBackend` and inspect the resulting character grid.
// ───────────────────────────────────────────────────────────────────────────

use ratatui::{backend::TestBackend, Terminal};

/// Serializes tests that mutate the process-global theme.
static THEME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Every key badge the search footer is allowed to render.
const FOOTER_KEY_TOKENS: &[&str] = &[
    "Esc",
    "\u{21b5}",
    "\u{2191}\u{2193}",
    "^F",
    "Tab",
    "?",
    "^Y",
    "^B",
    "^U",
    "^A",
    "^L",
    "^E",
    "^O",
    "^N",
    "^T",
    "^D",
    "^G",
    "^S",
    "^X",
    "^P",
    "^R",
    "j/k",
    "^U/^D",
    "/",
    "q",
    "NORMAL",
    "INSERT",
];

/// Every hint label word the search footer is allowed to render.
const FOOTER_LABEL_TOKENS: &[&str] = &[
    "Quit", "Run", "Nav", "Filter", "Detail", "Help", "Copy", "Bookmark", "Unique", "Agents",
    "Scope", "Failed", "Marked", "Note", "Tag", "Delete", "Goto", "Scroll", "Search", "Normal",
    // PROD-09: "Mode" (^X) and "Scope" (^P) choose what matches; "Rank" (^S,
    // renamed from "Match") and the rest only reorder it.
    "Mode", "Reset", "Here", "Rank",
];

fn render_lines(app: &mut SearchApp, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| app.draw(f)).unwrap();
    buffer_lines(terminal.backend().buffer())
}

fn buffer_lines(buf: &ratatui::buffer::Buffer) -> Vec<String> {
    let area = buf.area();
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| {
                    buf.cell((x, y))
                        .map_or(" ", ratatui::buffer::Cell::symbol)
                        .to_string()
                })
                .collect::<String>()
        })
        .collect()
}

fn footer_text(lines: &[String]) -> String {
    lines.last().cloned().unwrap_or_default()
}

fn is_page_token(token: &str) -> bool {
    // " 1/3 (33%) "
    token.ends_with("%)")
        || token
            .chars()
            .all(|c| c.is_ascii_digit() || c == '/' || c == '(' || c == ')' || c == '%')
}

/// A clipped badge shows up either as an unknown token (`"^"`, `"Bookmar"`) or
/// as a trailing key badge whose label never made it onto the screen.
fn assert_no_partial_hint(footer: &str, ctx: &str) {
    let trimmed = footer.trim_end();
    for token in trimmed.split_whitespace() {
        assert!(
            FOOTER_KEY_TOKENS.contains(&token)
                || FOOTER_LABEL_TOKENS.contains(&token)
                || is_page_token(token),
            "{ctx}: clipped/unknown footer token {token:?} in {footer:?}"
        );
    }
    if let Some(last) = trimmed.split_whitespace().next_back() {
        assert!(
            FOOTER_LABEL_TOKENS.contains(&last) || is_page_token(last),
            "{ctx}: footer ends mid-badge with {last:?} in {footer:?}"
        );
    }
}

fn assert_essential_hints(footer: &str, ctx: &str) {
    for expected in ["Esc", "Quit", "Run", "Nav", "Filter", "Detail", "Help"] {
        assert!(
            footer.contains(expected),
            "{ctx}: essential hint {expected:?} missing from footer {footer:?}"
        );
    }
}

/// The persistent status area must state scope, matching mode, result mode and
/// whether agent commands are included.
fn assert_status_area(lines: &[String], ctx: &str) {
    let screen = lines.join("\n");
    for expected in ["Scope", "Match", "Rank", "Agents"] {
        assert!(
            screen.contains(expected),
            "{ctx}: status indicator {expected:?} not visible on screen:\n{screen}"
        );
    }
}

fn render_app() -> SearchApp {
    let entries = vec![
        create_test_entry("cargo build --release"),
        create_test_entry("git status"),
        create_test_entry("kubectl get pods --all-namespaces"),
    ];
    SearchApp::new(test_search_config(entries, 3))
}

#[test]
fn prod03_footer_has_no_partial_hint_at_80x24() {
    let mut app = render_app();
    let lines = render_lines(&mut app, 80, 24);
    assert_no_partial_hint(&footer_text(&lines), "80x24");
}

#[test]
fn prod03_footer_has_no_partial_hint_at_100x30() {
    let mut app = render_app();
    let lines = render_lines(&mut app, 100, 30);
    assert_no_partial_hint(&footer_text(&lines), "100x30");
}

#[test]
fn prod03_essential_hints_visible_at_80x24() {
    let mut app = render_app();
    let lines = render_lines(&mut app, 80, 24);
    assert_essential_hints(&footer_text(&lines), "80x24");
}

#[test]
fn prod03_essential_hints_visible_at_100x30() {
    let mut app = render_app();
    let lines = render_lines(&mut app, 100, 30);
    assert_essential_hints(&footer_text(&lines), "100x30");
}

#[test]
fn prod03_status_area_visible_at_80x24() {
    let mut app = render_app();
    let lines = render_lines(&mut app, 80, 24);
    assert_status_area(&lines, "80x24");
}

#[test]
fn prod03_status_area_visible_at_100x30() {
    let mut app = render_app();
    let lines = render_lines(&mut app, 100, 30);
    assert_status_area(&lines, "100x30");
}

#[test]
fn prod03_status_area_reports_agent_visibility_and_scope() {
    let mut app = render_app();
    app.filters.show_agents = true;
    app.set_scope(RecallScope::Directory);
    app.view.unique_mode = true;
    let lines = render_lines(&mut app, 100, 30);
    let screen = lines.join("\n");
    assert!(screen.contains("Shown"), "agents shown state:\n{screen}");
    assert!(screen.contains("This dir"), "cwd scope state:\n{screen}");
    assert!(screen.contains("Unique"), "unique mode state:\n{screen}");

    app.filters.show_agents = false;
    app.set_scope(RecallScope::All);
    app.view.unique_mode = false;
    let lines = render_lines(&mut app, 100, 30);
    let screen = lines.join("\n");
    assert!(screen.contains("Hidden"), "agents hidden state:\n{screen}");
    assert!(
        screen.contains("All history"),
        "all-history scope:\n{screen}"
    );
}

#[test]
fn prod03_very_wide_terminal_shows_secondary_hints() {
    let mut app = render_app();
    let lines = render_lines(&mut app, 300, 40);
    let footer = footer_text(&lines);
    assert_no_partial_hint(&footer, "300x40");
    assert_essential_hints(&footer, "300x40");
    for expected in ["Copy", "Bookmark", "Note", "Tag", "Delete", "Goto"] {
        assert!(
            footer.contains(expected),
            "300x40: secondary hint {expected:?} missing from {footer:?}"
        );
    }
}

#[test]
fn prod03_narrow_terminal_keeps_help_discoverable() {
    let mut app = render_app();
    for width in [30_u16, 40, 50, 60, 70] {
        let lines = render_lines(&mut app, width, 24);
        let footer = footer_text(&lines);
        assert_no_partial_hint(&footer, &format!("{width}x24"));
        assert!(
            footer.contains("Help"),
            "{width}x24: help hint must always stay discoverable, got {footer:?}"
        );
    }
}

#[test]
fn prod03_resize_during_search_never_clips_hints() {
    let mut app = render_app();
    app.query = "cargo".to_string();
    for (w, h) in [
        (80_u16, 24_u16),
        (100, 30),
        (140, 40),
        (72, 20),
        (100, 30),
        (46, 16),
        (200, 50),
    ] {
        let lines = render_lines(&mut app, w, h);
        let footer = footer_text(&lines);
        assert_no_partial_hint(&footer, &format!("{w}x{h}"));
        assert!(
            footer.contains("Help"),
            "{w}x{h}: help hint missing after resize: {footer:?}"
        );
    }
}

#[test]
fn prod03_unicode_command_does_not_clip_hints_or_status() {
    let mut app = SearchApp::new(test_search_config(
        vec![
            create_test_entry("echo '\u{4f60}\u{597d}\u{4e16}\u{754c} \u{2014} caf\u{e9}'"),
            create_test_entry("grep -r '\u{1f680}' ."),
        ],
        2,
    ));
    for (w, h) in [(80_u16, 24_u16), (100, 30)] {
        let lines = render_lines(&mut app, w, h);
        assert_no_partial_hint(&footer_text(&lines), &format!("unicode {w}x{h}"));
        assert_status_area(&lines, &format!("unicode {w}x{h}"));
    }
}

#[test]
fn prod03_all_three_themes_render_without_clipped_hints() {
    let _guard = THEME_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for name in [
        crate::theme::ThemeName::Dark,
        crate::theme::ThemeName::Light,
        crate::theme::ThemeName::Terminal,
    ] {
        crate::theme::init_theme(name);
        let mut app = render_app();
        for (w, h) in [(80_u16, 24_u16), (100, 30)] {
            let lines = render_lines(&mut app, w, h);
            let ctx = format!("{name} {w}x{h}");
            assert_no_partial_hint(&footer_text(&lines), &ctx);
            assert_essential_hints(&footer_text(&lines), &ctx);
            assert_status_area(&lines, &ctx);
        }
    }
    crate::theme::init_theme(crate::theme::ThemeName::Dark);
}

#[test]
fn prod03_help_overlay_holds_advanced_hints_and_takes_focus() {
    let mut app = render_app();
    app.dialog = DialogState::Help;
    let lines = render_lines(&mut app, 100, 30);
    let screen = lines.join("\n");

    // Advanced/secondary shortcuts live in the overlay, not the footer.
    for expected in ["^G", "^T", "^S", "^N", "^D", "^O"] {
        assert!(
            screen.contains(expected),
            "help overlay missing {expected:?}:\n{screen}"
        );
    }
    // The search box must show it no longer has focus while the overlay is up.
    assert!(
        !screen.contains("Search (Typing)"),
        "search box must be dimmed while the help overlay has focus:\n{screen}"
    );
}

#[test]
fn prod03_detail_pane_moves_below_results_when_side_by_side_would_clip() {
    let long = "kubectl get pods --all-namespaces -o wide | grep suvadu";
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry(long)], 1));
    app.view.detail_pane_open = true;
    app.table_state.select(Some(0));

    for (w, h) in [(80_u16, 24_u16), (100, 30)] {
        let lines = render_lines(&mut app, w, h);
        let screen = lines.join("\n");
        assert!(
            lines.iter().any(|l| l.contains(long)),
            "{w}x{h}: full command must stay readable in the results list:\n{screen}"
        );
        let detail_row = lines
            .iter()
            .position(|l| l.contains("Detail"))
            .unwrap_or_else(|| panic!("{w}x{h}: detail pane must still be reachable:\n{screen}"));
        assert!(
            !lines[detail_row].contains("History"),
            "{w}x{h}: detail pane must move below the results instead of squeezing them, row was {:?}",
            lines[detail_row]
        );
    }
}

#[test]
fn prod03_detail_pane_stays_beside_results_on_wide_terminals() {
    let mut app = render_app();
    app.view.detail_pane_open = true;
    app.table_state.select(Some(0));
    let lines = render_lines(&mut app, 160, 40);
    // Side-by-side: the detail pane border shares rows with the results table.
    let detail_row = lines
        .iter()
        .position(|l| l.contains("Detail"))
        .expect("detail pane rendered");
    assert!(
        lines[detail_row].contains("History"),
        "160x40: detail pane should sit beside the results table, row was {:?}",
        lines[detail_row]
    );
}

#[test]
fn prod03_multiline_command_is_fully_inspectable() {
    let cmd = "for f in *.rs; do\n  rustfmt \"$f\"\ndone";
    let mut app = SearchApp::new(test_search_config(vec![create_test_entry(cmd)], 1));
    app.view.detail_pane_open = true;
    app.table_state.select(Some(0));

    let lines = render_lines(&mut app, 100, 30);
    let screen = lines.join("\n");
    for part in ["for f in *.rs; do", "rustfmt", "done"] {
        assert!(
            screen.contains(part),
            "multiline command part {part:?} not inspectable:\n{screen}"
        );
    }
    assert_no_partial_hint(&footer_text(&lines), "multiline 100x30");
}

#[test]
fn prod03_vim_mode_footer_is_not_clipped() {
    let mut app = render_app();
    app.vim_enabled = true;
    app.vim_mode = VimMode::Normal;
    let lines = render_lines(&mut app, 80, 24);
    let footer = footer_text(&lines);
    assert_no_partial_hint(&footer, "vim normal 80x24");
    assert!(footer.contains("Help"), "vim normal: {footer:?}");

    app.vim_mode = VimMode::Insert;
    let lines = render_lines(&mut app, 80, 24);
    let footer = footer_text(&lines);
    assert_no_partial_hint(&footer, "vim insert 80x24");
    assert!(footer.contains("Help"), "vim insert: {footer:?}");
}

/// A rendered app with nothing to show.
fn empty_render_app() -> SearchApp {
    let mut app = SearchApp::new(test_search_config(vec![], 0));
    app.query = "kubectl rollout".to_string();
    app
}

#[test]
fn prod09_no_results_state_names_the_mode_and_the_scope_on_screen() {
    let mut app = empty_render_app();
    app.recall.match_mode = MatchMode::Literal;
    app.set_scope(RecallScope::Directory);
    let screen = render_lines(&mut app, 100, 30).join("\n");

    assert!(screen.contains("No matches for"), "{screen}");
    assert!(
        screen.contains("literal"),
        "the mode must be named:\n{screen}"
    );
    assert!(
        screen.contains("This dir"),
        "the scope must be named:\n{screen}"
    );
    assert!(
        screen.contains("^R reset to all history"),
        "the way out must be offered:\n{screen}"
    );
}

#[test]
fn prod09_no_results_state_fits_an_80x24_terminal() {
    let mut app = empty_render_app();
    app.filters.failed_only = true;
    let lines = render_lines(&mut app, 80, 24);
    let screen = lines.join("\n");
    assert!(screen.contains("No matches for"), "{screen}");
    assert!(screen.contains("failed only"), "{screen}");
    assert!(screen.contains("^R reset"), "{screen}");
    // The footer is still laid out to the width, with nothing clipped.
    assert_no_partial_hint(&footer_text(&lines), "no-results 80x24");
    assert_essential_hints(&footer_text(&lines), "no-results 80x24");
}

#[test]
fn prod09_no_results_state_never_widens_the_scope_by_itself() {
    let mut app = empty_render_app();
    app.set_scope(RecallScope::Directory);
    let scope_before = app.recall.scope;
    let agents_before = app.filters.show_agents;
    let cwd_before = app.filters.cwd.clone();

    let screen = render_lines(&mut app, 100, 30).join("\n");

    assert_eq!(app.recall.scope, scope_before);
    assert_eq!(app.filters.show_agents, agents_before);
    assert_eq!(app.filters.cwd, cwd_before);
    assert!(
        screen.contains("^A include agent commands"),
        "agent inclusion must be offered, not performed:\n{screen}"
    );
}

#[test]
fn prod03_footer_snapshot_at_80x24() {
    let mut app = render_app();
    let lines = render_lines(&mut app, 80, 24);
    // PROD-09 changed this deliberately: `^X Mode` is now the first
    // secondary hint (it displaces `^Y Copy` at 80 columns) because the
    // matching mode decides what is eligible, and the status row renames
    // the ranking segment to `Rank` so `Match` can mean the matching mode.
    assert_eq!(
        footer_text(&lines),
        " Esc  Quit   \u{21b5}  Run   \u{2191}\u{2193}  Nav   ^F  Filter   Tab  Detail   ^X  Mode   ?  Help   "
    );
    assert_eq!(
        lines[4],
        " Scope  All history   Match  terms   Rank  Smart   Agents  Hidden   Show  All   "
    );
}

#[test]
fn prod03_footer_snapshot_at_100x30() {
    let mut app = render_app();
    let lines = render_lines(&mut app, 100, 30);
    assert_eq!(
        footer_text(&lines),
        " Esc  Quit   \u{21b5}  Run   \u{2191}\u{2193}  Nav   ^F  Filter   Tab  Detail   ^X  Mode   ^P  Scope   ?  Help           "
    );
}

#[test]
fn prod03_status_message_never_pushes_out_essential_hints() {
    let mut app = render_app();
    app.status_message = Some(("Copied to clipboard".to_string(), std::time::Instant::now()));
    for (w, h) in [(80_u16, 24_u16), (100, 30)] {
        let lines = render_lines(&mut app, w, h);
        let footer = footer_text(&lines);
        assert!(
            footer.contains("Copied to clipboard"),
            "{w}x{h}: status message missing: {footer:?}"
        );
        for expected in ["Esc", "Quit", "Filter", "Help"] {
            assert!(
                footer.contains(expected),
                "{w}x{h}: essential hint {expected:?} lost to the status message: {footer:?}"
            );
        }
    }
}

#[test]
fn prod03_help_overlay_fits_an_80x24_terminal() {
    let mut app = render_app();
    app.dialog = DialogState::Help;
    let lines = render_lines(&mut app, 80, 24);
    let screen = lines.join("\n");
    for expected in [
        "Bookmarked only",
        "Rank smart/recent",
        "Go to page...",
        "Press any key to close",
    ] {
        assert!(
            screen.contains(expected),
            "help overlay clipped {expected:?} at 80x24:\n{screen}"
        );
    }
}

#[test]
fn prod03_active_filters_shown_in_status_row() {
    let mut app = render_app();
    app.filters.after = Some(1);
    app.filters.failed_only = true;
    let lines = render_lines(&mut app, 100, 30);
    assert!(
        lines[4].contains("date") && lines[4].contains("failed"),
        "status row should list active filters, got {:?}",
        lines[4]
    );
}
