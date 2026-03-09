use super::*;
use crate::models::Session;
use crate::test_utils::test_repo;
use std::collections::HashMap;

fn setup_test_db() -> (tempfile::TempDir, Repository) {
    test_repo()
}

#[test]
fn test_insert_and_get_session() {
    let (_temp, repo) = setup_test_db();

    let session = Session::new("test-host".to_string(), 1000);
    repo.insert_session(&session).unwrap();

    let retrieved = repo.get_session(&session.id).unwrap();
    assert!(retrieved.is_some());

    let retrieved = retrieved.unwrap();
    assert_eq!(retrieved.id, session.id);
    assert_eq!(retrieved.hostname, "test-host");
    assert_eq!(retrieved.created_at, 1000);
}

#[test]
fn test_insert_and_get_entry() {
    let (_temp, repo) = setup_test_db();

    let session = Session::new("test-host".to_string(), 1000);
    repo.insert_session(&session).unwrap();

    let entry = Entry::new(
        session.id.clone(),
        "ls -la".to_string(),
        "/home/user".to_string(),
        Some(0),
        1000,
        1050,
    );

    let entry_id = repo.insert_entry(&entry).unwrap();
    assert!(entry_id > 0);

    let retrieved = repo.get_entry(entry_id).unwrap();
    assert!(retrieved.is_some());

    let retrieved = retrieved.unwrap();
    assert_eq!(retrieved.command, "ls -la");
    assert_eq!(retrieved.exit_code, Some(0));
    assert_eq!(retrieved.duration_ms, 50);
}

#[test]
fn test_entry_with_context() {
    let (_temp, repo) = setup_test_db();

    let session = Session::new("test-host".to_string(), 1000);
    repo.insert_session(&session).unwrap();

    let mut context = HashMap::new();
    context.insert("shell".to_string(), "zsh".to_string());
    context.insert("user".to_string(), "testuser".to_string());

    let mut entry = Entry::new(
        session.id.clone(),
        "echo test".to_string(),
        "/tmp".to_string(),
        Some(0),
        2000,
        2010,
    );
    entry.context = Some(context);

    let entry_id = repo.insert_entry(&entry).unwrap();

    let retrieved = repo.get_entry(entry_id).unwrap().unwrap();
    assert!(retrieved.context.is_some());

    let ctx = retrieved.context.unwrap();
    assert_eq!(ctx.get("shell").unwrap(), "zsh");
    assert_eq!(ctx.get("user").unwrap(), "testuser");
}

#[test]
fn test_get_entries_by_session() {
    let (_temp, repo) = setup_test_db();

    let session = Session::new("test-host".to_string(), 1000);
    repo.insert_session(&session).unwrap();

    for i in 0..5 {
        let entry = Entry::new(
            session.id.clone(),
            format!("command_{i}"),
            "/tmp".to_string(),
            Some(0),
            1000 + i * 100,
            1050 + i * 100,
        );
        repo.insert_entry(&entry).unwrap();
    }

    let entries = repo.get_entries_by_session(&session.id).unwrap();
    assert_eq!(entries.len(), 5);

    assert_eq!(entries[0].command, "command_4");
    assert_eq!(entries[4].command, "command_0");
}

#[test]
fn test_count_entries() {
    let (_temp, repo) = setup_test_db();

    let session = Session::new("test-host".to_string(), 1000);
    repo.insert_session(&session).unwrap();

    assert_eq!(repo.count_entries().unwrap(), 0);

    let entry = Entry::new(
        session.id.clone(),
        "test".to_string(),
        "/tmp".to_string(),
        Some(0),
        1000,
        1050,
    );
    repo.insert_entry(&entry).unwrap();

    assert_eq!(repo.count_entries().unwrap(), 1);
}

#[test]
fn test_tag_limits_and_constraints() {
    {
        let (_temp, repo) = setup_test_db();
        for i in 0..20 {
            repo.create_tag(&format!("tag_{i}"), None).unwrap();
        }

        let err = repo.create_tag("tag_overflow", None);
        assert!(err.is_err());
        match err.unwrap_err() {
            crate::db::DbError::Validation(msg) => assert!(msg.contains("Maximum number")),
            other => panic!("Expected Validation error, got {:?}", other),
        }
    }

    {
        let (_temp, repo) = setup_test_db();
        let _id = repo.create_tag("UpPeR", None).unwrap();
        let tags = repo.get_tags().unwrap();
        assert_eq!(tags[0].name, "upper");

        let err = repo.create_tag("upper", None).unwrap_err();
        assert!(matches!(err, crate::db::DbError::Sqlite(_)));
    }
}

#[test]
fn test_entries_filtering_by_tag() {
    let (_temp, repo) = setup_test_db();

    let work_tag = repo.create_tag("work", None).unwrap();
    let session_work = Session::new("host".to_string(), 100);
    repo.insert_session(&session_work).unwrap();
    repo.tag_session(&session_work.id, Some(work_tag)).unwrap();

    let entry_work = Entry::new(
        session_work.id.clone(),
        "git commit".to_string(),
        "/work".to_string(),
        None,
        1000,
        1010,
    );
    repo.insert_entry(&entry_work).unwrap();

    let personal_tag = repo.create_tag("personal", None).unwrap();
    let session_personal = Session::new("host".to_string(), 200);
    repo.insert_session(&session_personal).unwrap();
    repo.tag_session(&session_personal.id, Some(personal_tag))
        .unwrap();

    let entry_personal = Entry::new(
        session_personal.id.clone(),
        "steam".to_string(),
        "/games".to_string(),
        None,
        2000,
        2010,
    );
    repo.insert_entry(&entry_personal).unwrap();

    let session_untagged = Session::new("host".to_string(), 300);
    repo.insert_session(&session_untagged).unwrap();
    let entry_untagged = Entry::new(
        session_untagged.id.clone(),
        "ls".to_string(),
        "/".to_string(),
        None,
        3000,
        3010,
    );
    repo.insert_entry(&entry_untagged).unwrap();

    let work_entries = repo
        .get_entries_filtered(
            10,
            0,
            &QueryFilter {
                tag_id: Some(work_tag),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(work_entries.len(), 1);
    assert_eq!(work_entries[0].command, "git commit");

    let work_count = repo
        .count_filtered(&QueryFilter {
            tag_id: Some(work_tag),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(work_count, 1);

    let personal_entries = repo
        .get_entries_filtered(
            10,
            0,
            &QueryFilter {
                tag_id: Some(personal_tag),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(personal_entries.len(), 1);
    assert_eq!(personal_entries[0].command, "steam");

    let all = repo
        .get_entries_filtered(10, 0, &QueryFilter::default())
        .unwrap();
    assert_eq!(all.len(), 3);
}

#[test]
fn test_unique_entries_filtering_by_tag() {
    let (_temp, repo) = setup_test_db();
    let work_tag = repo.create_tag("work", None).unwrap();

    let session_work = Session::new("host".to_string(), 100);
    repo.insert_session(&session_work).unwrap();
    repo.tag_session(&session_work.id, Some(work_tag)).unwrap();

    repo.insert_entry(&Entry::new(
        session_work.id.clone(),
        "ls".into(),
        "/".into(),
        None,
        100,
        200,
    ))
    .unwrap();
    repo.insert_entry(&Entry::new(
        session_work.id.clone(),
        "ls".into(),
        "/".into(),
        None,
        110,
        210,
    ))
    .unwrap();
    repo.insert_entry(&Entry::new(
        session_work.id.clone(),
        "make".into(),
        "/".into(),
        None,
        120,
        220,
    ))
    .unwrap();

    let session_other = Session::new("host".to_string(), 200);
    repo.insert_session(&session_other).unwrap();
    repo.insert_entry(&Entry::new(
        session_other.id.clone(),
        "ls".into(),
        "/".into(),
        None,
        300,
        400,
    ))
    .unwrap();

    let unique_work = repo
        .get_unique_entries_filtered(
            10,
            0,
            &QueryFilter {
                tag_id: Some(work_tag),
                ..Default::default()
            },
            false,
        )
        .unwrap();
    assert_eq!(unique_work.len(), 2);

    let ls_entry = unique_work.iter().find(|(e, _)| e.command == "ls").unwrap();
    assert_eq!(ls_entry.1, 2);

    let unique_count = repo
        .count_unique_filtered(&QueryFilter {
            tag_id: Some(work_tag),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(unique_count, 2);

    let unique_global = repo
        .get_unique_entries_filtered(10, 0, &QueryFilter::default(), false)
        .unwrap();
    assert_eq!(unique_global.len(), 2);
    let ls_global = unique_global
        .iter()
        .find(|(e, _)| e.command == "ls")
        .unwrap();
    assert_eq!(ls_global.1, 3);
}

#[test]
fn test_tag_lifecycle() {
    let (_temp, repo) = setup_test_db();

    let id = repo.create_tag("work", Some("Work stuff")).unwrap();
    let tags = repo.get_tags().unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].name, "work");

    let _id2 = repo.create_tag("Personal", None).unwrap();
    let tags = repo.get_tags().unwrap();
    assert_eq!(tags.len(), 2);
    assert_eq!(tags[0].name, "personal");
    assert_eq!(tags[1].name, "work");

    let err = repo.create_tag("WORK", None);
    assert!(err.is_err());

    repo.update_tag(id, "work_updated", None).unwrap();
    let tags = repo.get_tags().unwrap();
    assert_eq!(tags[1].name, "work_updated");

    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    repo.tag_session(&session.id, Some(id)).unwrap();
    let s = repo.get_session(&session.id).unwrap().unwrap();
    assert_eq!(s.tag_id, Some(id));

    repo.tag_session(&session.id, None).unwrap();
    let s = repo.get_session(&session.id).unwrap().unwrap();
    assert_eq!(s.tag_id, None);
}

#[test]
fn test_unique_entries_query() {
    let (_temp, repo) = setup_test_db();

    let session = Session::new("test-host".to_string(), 1000);
    repo.insert_session(&session).unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "ls".to_string(),
        "/tmp".to_string(),
        None,
        1000,
        1010,
    ))
    .unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "ls".to_string(),
        "/tmp".to_string(),
        None,
        2000,
        2010,
    ))
    .unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "ls".to_string(),
        "/tmp".to_string(),
        None,
        3000,
        3010,
    ))
    .unwrap();

    let entries = repo
        .get_unique_entries_filtered(10, 0, &QueryFilter::default(), false)
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0.command, "ls");
    assert_eq!(entries[0].0.started_at, 3000);
    assert_eq!(entries[0].1, 3);
}

#[test]
fn test_unique_entries_pagination_and_query() {
    let (_temp, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    let cmds = vec![
        ("git commit", 1000),
        ("git status", 2000),
        ("git commit", 3000),
        ("cargo build", 4000),
    ];

    for (cmd, time) in cmds {
        repo.insert_entry(&Entry::new(
            session.id.clone(),
            cmd.to_string(),
            "/".into(),
            None,
            time,
            time + 10,
        ))
        .unwrap();
    }

    let unique_git = repo
        .get_unique_entries_filtered(
            10,
            0,
            &QueryFilter {
                query: Some("git"),
                ..Default::default()
            },
            false,
        )
        .unwrap();
    assert_eq!(unique_git.len(), 2);
    assert_eq!(unique_git[0].0.command, "git commit");
    assert_eq!(unique_git[1].0.command, "git status");

    let page1 = repo
        .get_unique_entries_filtered(
            1,
            0,
            &QueryFilter {
                query: Some("git"),
                ..Default::default()
            },
            false,
        )
        .unwrap();
    assert_eq!(page1.len(), 1);
    assert_eq!(page1[0].0.command, "git commit");

    let page2 = repo
        .get_unique_entries_filtered(
            1,
            1,
            &QueryFilter {
                query: Some("git"),
                ..Default::default()
            },
            false,
        )
        .unwrap();
    assert_eq!(page2.len(), 1);
    assert_eq!(page2[0].0.command, "git status");

    let page3 = repo
        .get_unique_entries_filtered(
            1,
            2,
            &QueryFilter {
                query: Some("git"),
                ..Default::default()
            },
            false,
        )
        .unwrap();
    assert_eq!(page3.len(), 0);
}

#[test]
fn test_unique_entries_recency_priority() {
    let (_temp, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "cmd_A".into(),
        "/".into(),
        None,
        1000,
        1010,
    ))
    .unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "cmd_B".into(),
        "/".into(),
        None,
        2000,
        2010,
    ))
    .unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "cmd_C".into(),
        "/".into(),
        None,
        3000,
        3010,
    ))
    .unwrap();

    let page1 = repo
        .get_unique_entries_filtered(1, 0, &QueryFilter::default(), false)
        .unwrap();
    assert_eq!(page1.len(), 1);
    assert_eq!(page1[0].0.command, "cmd_C");

    let page2 = repo
        .get_unique_entries_filtered(1, 1, &QueryFilter::default(), false)
        .unwrap();
    assert_eq!(page2.len(), 1);
    assert_eq!(page2[0].0.command, "cmd_B");

    let page3 = repo
        .get_unique_entries_filtered(1, 2, &QueryFilter::default(), false)
        .unwrap();
    assert_eq!(page3.len(), 1);
    assert_eq!(page3[0].0.command, "cmd_A");
}

#[test]
fn test_unique_entries_reexecution() {
    let (_temp, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "cmd_A".into(),
        "/".into(),
        None,
        1000,
        1010,
    ))
    .unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "cmd_B".into(),
        "/".into(),
        None,
        2000,
        2010,
    ))
    .unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "cmd_A".into(),
        "/".into(),
        None,
        3000,
        3010,
    ))
    .unwrap();

    let page1 = repo
        .get_unique_entries_filtered(1, 0, &QueryFilter::default(), false)
        .unwrap();
    assert_eq!(page1.len(), 1);
    assert_eq!(page1[0].0.command, "cmd_A");

    let page2 = repo
        .get_unique_entries_filtered(1, 1, &QueryFilter::default(), false)
        .unwrap();
    assert_eq!(page2.len(), 1);
    assert_eq!(page2[0].0.command, "cmd_B");
}

#[test]
fn test_recent_entries_shows_failed_commands() {
    let (_temp, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    // Failed command at T=1000
    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "cargo build".into(),
        "/project".into(),
        Some(1), // failed
        1000,
        1010,
    ))
    .unwrap();

    // Same command succeeds at T=2000
    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "cargo build".into(),
        "/project".into(),
        Some(0), // success
        2000,
        2010,
    ))
    .unwrap();

    // get_recent_entries should return BOTH invocations (no dedup)
    let results = repo.get_recent_entries(10, 0, None, false, None).unwrap();
    assert_eq!(results.len(), 2);
    // Most recent first
    assert_eq!(results[0].started_at, 2000);
    assert_eq!(results[0].exit_code, Some(0));
    assert_eq!(results[1].started_at, 1000);
    assert_eq!(results[1].exit_code, Some(1));
}

#[test]
fn test_recent_entries_with_cwd_boost() {
    let (_temp, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    // Older command in /project
    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "make test".into(),
        "/project".into(),
        Some(0),
        1000,
        1010,
    ))
    .unwrap();

    // Newer command in /other
    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "ls".into(),
        "/other".into(),
        Some(0),
        2000,
        2010,
    ))
    .unwrap();

    // With boost_cwd=/project, /project commands should come first
    let results = repo
        .get_recent_entries(10, 0, None, false, Some("/project"))
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].command, "make test");
    assert_eq!(results[1].command, "ls");
}

#[test]
fn test_recent_entries_prefix_match() {
    let (_temp, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "git status".into(),
        "/".into(),
        Some(0),
        1000,
        1010,
    ))
    .unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "grep foo".into(),
        "/".into(),
        Some(0),
        2000,
        2010,
    ))
    .unwrap();

    // Prefix match for "git" should only return "git status"
    let results = repo
        .get_recent_entries(10, 0, Some("git"), true, None)
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].command, "git status");
}

#[test]
fn test_executor_tracking() {
    let (_temp, repo) = setup_test_db();

    let session = Session::new("test-host".to_string(), 1000);
    repo.insert_session(&session).unwrap();

    let mut entry = Entry::new(
        session.id.clone(),
        "cargo build".to_string(),
        "/home/user/project".to_string(),
        Some(0),
        1000,
        2000,
    );
    entry.executor_type = Some("human".to_string());
    entry.executor = Some("terminal".to_string());

    let entry_id = repo.insert_entry(&entry).unwrap();

    let retrieved = repo.get_entry(entry_id).unwrap().unwrap();
    assert_eq!(retrieved.executor_type, Some("human".to_string()));
    assert_eq!(retrieved.executor, Some("terminal".to_string()));
}

#[test]
fn test_executor_types() {
    let (_temp, repo) = setup_test_db();

    let session = Session::new("test-host".to_string(), 1000);
    repo.insert_session(&session).unwrap();

    let executors = vec![
        ("human", "terminal"),
        ("ide", "vscode"),
        ("bot", "antigravity"),
        ("ci", "github-actions"),
    ];

    for (exec_type, exec_name) in executors {
        let mut entry = Entry::new(
            session.id.clone(),
            format!("test command for {}", exec_type),
            "/tmp".to_string(),
            Some(0),
            1000,
            2000,
        );
        entry.executor_type = Some(exec_type.to_string());
        entry.executor = Some(exec_name.to_string());

        let entry_id = repo.insert_entry(&entry).unwrap();
        let retrieved = repo.get_entry(entry_id).unwrap().unwrap();

        assert_eq!(retrieved.executor_type, Some(exec_type.to_string()));
        assert_eq!(retrieved.executor, Some(exec_name.to_string()));
    }
}

#[test]
fn test_executor_null_values() {
    let (_temp, repo) = setup_test_db();

    let session = Session::new("test-host".to_string(), 1000);
    repo.insert_session(&session).unwrap();

    let entry = Entry::new(
        session.id.clone(),
        "old command".to_string(),
        "/tmp".to_string(),
        Some(0),
        1000,
        2000,
    );

    let entry_id = repo.insert_entry(&entry).unwrap();
    let retrieved = repo.get_entry(entry_id).unwrap().unwrap();

    assert_eq!(retrieved.executor_type, None);
    assert_eq!(retrieved.executor, None);
}

#[test]
fn test_executor_filter_in_count() {
    let (_temp, repo) = setup_test_db();

    let session = Session::new("test-host".to_string(), 1000);
    repo.insert_session(&session).unwrap();

    let mut entry1 = Entry::new(
        session.id.clone(),
        "ls".to_string(),
        "/tmp".to_string(),
        Some(0),
        1000,
        2000,
    );
    entry1.executor_type = Some("human".to_string());
    entry1.executor = Some("terminal".to_string());
    repo.insert_entry(&entry1).unwrap();

    let mut entry2 = Entry::new(
        session.id.clone(),
        "git status".to_string(),
        "/tmp".to_string(),
        Some(0),
        2000,
        3000,
    );
    entry2.executor_type = Some("bot".to_string());
    entry2.executor = Some("antigravity".to_string());
    repo.insert_entry(&entry2).unwrap();

    // Count all
    let total = repo.count_filtered(&QueryFilter::default()).unwrap();
    assert_eq!(total, 2);

    // Count only human
    let human_count = repo
        .count_filtered(&QueryFilter {
            executor: Some("human"),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(human_count, 1);

    // Count only bot
    let bot_count = repo
        .count_filtered(&QueryFilter {
            executor: Some("bot"),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(bot_count, 1);
}

#[test]
fn test_stats_empty_db() {
    let (_temp, repo) = setup_test_db();
    let stats = repo.get_stats(None, 10, None).unwrap();
    assert_eq!(stats.total_commands, 0);
    assert_eq!(stats.unique_commands, 0);
    assert_eq!(stats.success_count, 0);
    assert_eq!(stats.failure_count, 0);
    assert_eq!(stats.avg_duration_ms, 0);
    assert!(stats.top_commands.is_empty());
    assert!(stats.top_directories.is_empty());
}

#[test]
fn test_stats_with_entries() {
    let (_temp, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 1000);
    repo.insert_session(&session).unwrap();

    // Insert entries: 3x "git status" (success), 2x "cargo build" (1 success, 1 fail)
    for i in 0..3 {
        let mut entry = Entry::new(
            session.id.clone(),
            "git status".to_string(),
            "/project".to_string(),
            Some(0),
            2000 + i * 100,
            2050 + i * 100,
        );
        entry.executor_type = Some("human".to_string());
        repo.insert_entry(&entry).unwrap();
    }

    let mut entry = Entry::new(
        session.id.clone(),
        "cargo build".to_string(),
        "/project".to_string(),
        Some(0),
        3000,
        4000,
    );
    entry.executor_type = Some("agent".to_string());
    repo.insert_entry(&entry).unwrap();

    let mut entry = Entry::new(
        session.id.clone(),
        "cargo build".to_string(),
        "/other".to_string(),
        Some(1),
        5000,
        5500,
    );
    entry.executor_type = Some("agent".to_string());
    repo.insert_entry(&entry).unwrap();

    let stats = repo.get_stats(None, 10, None).unwrap();
    assert_eq!(stats.total_commands, 5);
    assert_eq!(stats.unique_commands, 2);
    assert_eq!(stats.success_count, 4);
    assert_eq!(stats.failure_count, 1);

    // Top commands: git status (3) > cargo build (2)
    assert_eq!(stats.top_commands[0].0, "git status");
    assert_eq!(stats.top_commands[0].1, 3);
    assert_eq!(stats.top_commands[1].0, "cargo build");
    assert_eq!(stats.top_commands[1].1, 2);

    // Top directories: /project (4) > /other (1)
    assert_eq!(stats.top_directories[0].0, "/project");
    assert_eq!(stats.top_directories[0].1, 4);

    // Executor: human (3) > agent (2)
    assert_eq!(stats.executor_breakdown[0].0, "human");
    assert_eq!(stats.executor_breakdown[0].1, 3);
    assert_eq!(stats.executor_breakdown[1].0, "agent");
    assert_eq!(stats.executor_breakdown[1].1, 2);
}

#[test]
fn test_stats_with_days_filter() {
    let (_temp, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 1000);
    repo.insert_session(&session).unwrap();

    let now_ms = chrono::Utc::now().timestamp_millis();

    // Recent entry (today)
    let entry = Entry::new(
        session.id.clone(),
        "recent".to_string(),
        "/tmp".to_string(),
        Some(0),
        now_ms - 1000,
        now_ms,
    );
    repo.insert_entry(&entry).unwrap();

    // Old entry (60 days ago)
    let old_ms = now_ms - 60 * 24 * 60 * 60 * 1000;
    let entry = Entry::new(
        session.id.clone(),
        "old".to_string(),
        "/tmp".to_string(),
        Some(0),
        old_ms,
        old_ms + 100,
    );
    repo.insert_entry(&entry).unwrap();

    // All time: 2 commands
    let stats = repo.get_stats(None, 10, None).unwrap();
    assert_eq!(stats.total_commands, 2);

    // Last 7 days: only 1 command
    let stats = repo.get_stats(Some(7), 10, None).unwrap();
    assert_eq!(stats.total_commands, 1);
    assert_eq!(stats.top_commands[0].0, "recent");
}

// ── Bookmark Tests ──────────────────────────────────────

#[test]
fn test_bookmark_crud() {
    let (_dir, repo) = setup_test_db();

    // Empty initially
    let bookmarks = repo.list_bookmarks().unwrap();
    assert!(bookmarks.is_empty());

    // Add bookmarks
    repo.add_bookmark("git status", Some("check repo")).unwrap();
    repo.add_bookmark("cargo test", None).unwrap();

    let bookmarks = repo.list_bookmarks().unwrap();
    assert_eq!(bookmarks.len(), 2);
    assert_eq!(bookmarks[0].command, "cargo test"); // Most recent first
    assert_eq!(bookmarks[1].command, "git status");
    assert_eq!(bookmarks[1].label.as_deref(), Some("check repo"));

    // Remove one
    let removed = repo.remove_bookmark("git status").unwrap();
    assert!(removed);

    let bookmarks = repo.list_bookmarks().unwrap();
    assert_eq!(bookmarks.len(), 1);
    assert_eq!(bookmarks[0].command, "cargo test");

    // Remove non-existent
    let removed = repo.remove_bookmark("nonexistent").unwrap();
    assert!(!removed);
}

#[test]
fn test_bookmark_duplicate_upsert() {
    let (_dir, repo) = setup_test_db();

    repo.add_bookmark("git push", Some("deploy")).unwrap();
    let original_created = repo.list_bookmarks().unwrap()[0].created_at;

    // Re-adding same command updates label but preserves created_at
    repo.add_bookmark("git push", Some("updated label"))
        .unwrap();

    let bookmarks = repo.list_bookmarks().unwrap();
    assert_eq!(bookmarks.len(), 1);
    assert_eq!(bookmarks[0].label.as_deref(), Some("updated label"));
    assert_eq!(bookmarks[0].created_at, original_created);
}

#[test]
fn test_get_bookmarked_commands() {
    let (_dir, repo) = setup_test_db();

    repo.add_bookmark("ls -la", None).unwrap();
    repo.add_bookmark("pwd", None).unwrap();

    let set = repo.get_bookmarked_commands().unwrap();
    assert_eq!(set.len(), 2);
    assert!(set.contains("ls -la"));
    assert!(set.contains("pwd"));
}

// ── Directory-Scoped Tests ──────────────────────────────

#[test]
fn test_filter_by_cwd() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    // Insert entries in different directories
    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "cargo build".into(),
        "/home/user/project".into(),
        Some(0),
        1000,
        1100,
    ))
    .unwrap();
    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "npm test".into(),
        "/home/user/webapp".into(),
        Some(0),
        2000,
        2100,
    ))
    .unwrap();
    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "cargo test".into(),
        "/home/user/project".into(),
        Some(0),
        3000,
        3100,
    ))
    .unwrap();

    // Filter by cwd
    let project_entries = repo
        .get_entries_filtered(
            10,
            0,
            &QueryFilter {
                cwd: Some("/home/user/project"),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(project_entries.len(), 2);
    assert!(project_entries
        .iter()
        .all(|e| e.cwd == "/home/user/project"));

    let webapp_entries = repo
        .get_entries_filtered(
            10,
            0,
            &QueryFilter {
                cwd: Some("/home/user/webapp"),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(webapp_entries.len(), 1);
    assert_eq!(webapp_entries[0].command, "npm test");

    // No filter returns all
    let all_entries = repo
        .get_entries_filtered(10, 0, &QueryFilter::default())
        .unwrap();
    assert_eq!(all_entries.len(), 3);

    // Count with cwd filter
    let project_count = repo
        .count_filtered(&QueryFilter {
            cwd: Some("/home/user/project"),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(project_count, 2);
}

#[test]
fn test_cwd_filter_with_other_filters() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    let mut entry1 = Entry::new(
        session.id.clone(),
        "cargo build".into(),
        "/home/user/project".into(),
        Some(0),
        1000,
        1100,
    );
    entry1.executor_type = Some("human".to_string());
    repo.insert_entry(&entry1).unwrap();

    let mut entry2 = Entry::new(
        session.id.clone(),
        "cargo test".into(),
        "/home/user/project".into(),
        Some(1),
        2000,
        2100,
    );
    entry2.executor_type = Some("agent".to_string());
    repo.insert_entry(&entry2).unwrap();

    // cwd + executor filter
    let human_project = repo
        .get_entries_filtered(
            10,
            0,
            &QueryFilter {
                executor: Some("human"),
                cwd: Some("/home/user/project"),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(human_project.len(), 1);
    assert_eq!(human_project[0].command, "cargo build");

    // cwd + exit code filter
    let failed_project = repo
        .get_entries_filtered(
            10,
            0,
            &QueryFilter {
                exit_code: Some(1),
                cwd: Some("/home/user/project"),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(failed_project.len(), 1);
    assert_eq!(failed_project[0].command, "cargo test");
}

#[test]
fn test_note_crud() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    let entry_id = repo
        .insert_entry(&Entry::new(
            session.id.clone(),
            "cargo build".into(),
            "/tmp".into(),
            Some(0),
            1000,
            1100,
        ))
        .unwrap();

    // No note initially
    assert!(repo.get_note(entry_id).unwrap().is_none());

    // Create note
    repo.upsert_note(entry_id, "Fixed the SSL bug").unwrap();
    let note = repo.get_note(entry_id).unwrap().unwrap();
    assert_eq!(note.entry_id, entry_id);
    assert_eq!(note.content, "Fixed the SSL bug");

    // Delete note
    assert!(repo.delete_note(entry_id).unwrap());
    assert!(repo.get_note(entry_id).unwrap().is_none());

    // Delete non-existent returns false
    assert!(!repo.delete_note(entry_id).unwrap());
}

#[test]
fn test_note_upsert_overwrites() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    let entry_id = repo
        .insert_entry(&Entry::new(
            session.id.clone(),
            "git push".into(),
            "/tmp".into(),
            Some(0),
            1000,
            1100,
        ))
        .unwrap();

    repo.upsert_note(entry_id, "First note").unwrap();
    let note1 = repo.get_note(entry_id).unwrap().unwrap();
    assert_eq!(note1.content, "First note");

    repo.upsert_note(entry_id, "Updated note").unwrap();
    let note2 = repo.get_note(entry_id).unwrap().unwrap();
    assert_eq!(note2.content, "Updated note");
    assert_eq!(note2.id, note1.id); // Same row, updated in place
}

#[test]
fn test_get_noted_entry_ids() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    let id1 = repo
        .insert_entry(&Entry::new(
            session.id.clone(),
            "cmd1".into(),
            "/tmp".into(),
            Some(0),
            1000,
            1100,
        ))
        .unwrap();
    let id2 = repo
        .insert_entry(&Entry::new(
            session.id.clone(),
            "cmd2".into(),
            "/tmp".into(),
            Some(0),
            2000,
            2100,
        ))
        .unwrap();
    let id3 = repo
        .insert_entry(&Entry::new(
            session.id.clone(),
            "cmd3".into(),
            "/tmp".into(),
            Some(0),
            3000,
            3100,
        ))
        .unwrap();

    // Empty initially
    let ids = repo.get_noted_entry_ids().unwrap();
    assert!(ids.is_empty());

    // Add notes to entries 1 and 3
    repo.upsert_note(id1, "note for cmd1").unwrap();
    repo.upsert_note(id3, "note for cmd3").unwrap();

    let ids = repo.get_noted_entry_ids().unwrap();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&id1));
    assert!(!ids.contains(&id2));
    assert!(ids.contains(&id3));
}

#[test]
fn test_get_frequent_commands() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    let now = chrono::Utc::now().timestamp();

    // Insert "cargo build --release" 10 times (long, frequent)
    for i in 0..10 {
        repo.insert_entry(&Entry::new(
            session.id.clone(),
            "cargo build --release".into(),
            "/project".into(),
            Some(0),
            now + i,
            now + i + 50,
        ))
        .unwrap();
    }

    // Insert "ls" 20 times (short, frequent)
    for i in 0..20 {
        repo.insert_entry(&Entry::new(
            session.id.clone(),
            "ls".into(),
            "/tmp".into(),
            Some(0),
            now + 100 + i,
            now + 100 + i + 10,
        ))
        .unwrap();
    }

    // Insert "git status" 3 times (long enough, but below min_count)
    for i in 0..3 {
        repo.insert_entry(&Entry::new(
            session.id.clone(),
            "git status --short".into(),
            "/project".into(),
            Some(0),
            now + 200 + i,
            now + 200 + i + 10,
        ))
        .unwrap();
    }

    // min_length=12 should exclude "ls", min_count=5 should exclude "git status --short"
    let results = repo
        .get_frequent_commands_filtered(None, 5, 12, 10, false)
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "cargo build --release");
    assert_eq!(results[0].1, 10);
    assert_eq!(results[0].2, 1); // all from /project
}

#[test]
fn test_get_frequent_commands_with_days() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    let now = chrono::Utc::now().timestamp_millis();
    let old = now - 100 * 86_400_000; // 100 days ago

    // Old commands
    for i in 0..10 {
        repo.insert_entry(&Entry::new(
            session.id.clone(),
            "cargo build --release".into(),
            "/project".into(),
            Some(0),
            old + i,
            old + i + 50,
        ))
        .unwrap();
    }

    // Recent commands
    for i in 0..10 {
        repo.insert_entry(&Entry::new(
            session.id.clone(),
            "cargo test --workspace".into(),
            "/project".into(),
            Some(0),
            now + i,
            now + i + 50,
        ))
        .unwrap();
    }

    // With days=30, only recent commands
    let results = repo
        .get_frequent_commands_filtered(Some(30), 5, 12, 10, false)
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "cargo test --workspace");
    assert_eq!(results[0].2, 1); // all from /project
}

#[test]
fn test_get_frequent_commands_dir_diversity_ranking() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    let now = chrono::Utc::now().timestamp_millis();

    // Command A: 10 uses from 1 directory -> score = 10 * 1 = 10
    for i in 0..10 {
        repo.insert_entry(&Entry::new(
            session.id.clone(),
            "cargo build --release".into(),
            "/project-a".into(),
            Some(0),
            now + i * 1000,
            now + i * 1000 + 50_000,
        ))
        .unwrap();
    }

    // Command B: 8 uses from 4 directories -> score = 8 * 4 = 32
    let dirs = ["/proj1", "/proj2", "/proj3", "/proj4"];
    for i in 0..8 {
        repo.insert_entry(&Entry::new(
            session.id.clone(),
            "git log --oneline".into(),
            dirs[i % 4].into(),
            Some(0),
            now + 100_000 + i as i64 * 1000,
            now + 100_000 + i as i64 * 1000 + 50_000,
        ))
        .unwrap();
    }

    let results = repo
        .get_frequent_commands_filtered(None, 5, 12, 10, false)
        .unwrap();

    // "git log --oneline" should rank first despite fewer uses (higher dir diversity)
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].0, "git log --oneline");
    assert_eq!(results[0].1, 8);
    assert_eq!(results[0].2, 4);
    assert_eq!(results[1].0, "cargo build --release");
    assert_eq!(results[1].1, 10);
    assert_eq!(results[1].2, 1);
}

// ── Delete Tests ────────────────────────────────────────

#[test]
fn test_delete_entries_by_pattern() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "git status".into(),
        "/tmp".into(),
        Some(0),
        1000,
        1100,
    ))
    .unwrap();
    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "git commit".into(),
        "/tmp".into(),
        Some(0),
        2000,
        2100,
    ))
    .unwrap();
    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "cargo build".into(),
        "/tmp".into(),
        Some(0),
        3000,
        3100,
    ))
    .unwrap();

    // Delete entries matching "git"
    let deleted = repo.delete_entries("git", false, None).unwrap();
    assert_eq!(deleted, 2);
    assert_eq!(repo.count_entries().unwrap(), 1);
}

#[test]
fn test_delete_entries_by_regex() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "git status".into(),
        "/tmp".into(),
        Some(0),
        1000,
        1100,
    ))
    .unwrap();
    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "git commit -m 'fix'".into(),
        "/tmp".into(),
        Some(0),
        2000,
        2100,
    ))
    .unwrap();
    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "cargo build".into(),
        "/tmp".into(),
        Some(0),
        3000,
        3100,
    ))
    .unwrap();

    // Regex: delete commands starting with "git"
    let deleted = repo.delete_entries("^git", true, None).unwrap();
    assert_eq!(deleted, 2);
    assert_eq!(repo.count_entries().unwrap(), 1);
}

#[test]
fn test_delete_entries_with_before_timestamp() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "git status".into(),
        "/tmp".into(),
        Some(0),
        1000,
        1100,
    ))
    .unwrap();
    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "git commit".into(),
        "/tmp".into(),
        Some(0),
        5000,
        5100,
    ))
    .unwrap();

    // Delete "git" entries older than 3000
    let deleted = repo.delete_entries("git", false, Some(3000)).unwrap();
    assert_eq!(deleted, 1);
    assert_eq!(repo.count_entries().unwrap(), 1);
}

#[test]
fn test_delete_entries_regex_with_before() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "git status".into(),
        "/tmp".into(),
        Some(0),
        1000,
        1100,
    ))
    .unwrap();
    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "git push".into(),
        "/tmp".into(),
        Some(0),
        5000,
        5100,
    ))
    .unwrap();

    // Regex delete "^git" before 3000 — should only delete the old one
    let deleted = repo.delete_entries("^git", true, Some(3000)).unwrap();
    assert_eq!(deleted, 1);
    assert_eq!(repo.count_entries().unwrap(), 1);
}

#[test]
fn test_delete_entries_no_match() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "ls -la".into(),
        "/tmp".into(),
        Some(0),
        1000,
        1100,
    ))
    .unwrap();

    let deleted = repo.delete_entries("nonexistent", false, None).unwrap();
    assert_eq!(deleted, 0);
    assert_eq!(repo.count_entries().unwrap(), 1);

    let deleted = repo.delete_entries("^zzz", true, None).unwrap();
    assert_eq!(deleted, 0);
    assert_eq!(repo.count_entries().unwrap(), 1);
}

#[test]
fn test_count_entries_by_pattern() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "git status".into(),
        "/tmp".into(),
        Some(0),
        1000,
        1100,
    ))
    .unwrap();
    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "git commit".into(),
        "/tmp".into(),
        Some(0),
        2000,
        2100,
    ))
    .unwrap();
    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "cargo build".into(),
        "/tmp".into(),
        Some(0),
        3000,
        3100,
    ))
    .unwrap();

    // LIKE pattern count
    assert_eq!(
        repo.count_entries_by_pattern("git", false, None).unwrap(),
        2
    );
    assert_eq!(
        repo.count_entries_by_pattern("cargo", false, None).unwrap(),
        1
    );
    assert_eq!(
        repo.count_entries_by_pattern("nonexistent", false, None)
            .unwrap(),
        0
    );

    // Regex count
    assert_eq!(
        repo.count_entries_by_pattern("^git", true, None).unwrap(),
        2
    );
    assert_eq!(
        repo.count_entries_by_pattern("commit$", true, None)
            .unwrap(),
        1
    );

    // With before timestamp
    assert_eq!(
        repo.count_entries_by_pattern("git", false, Some(1500))
            .unwrap(),
        1
    );
}

#[test]
fn test_delete_entry_by_id() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    let id = repo
        .insert_entry(&Entry::new(
            session.id.clone(),
            "ls".into(),
            "/tmp".into(),
            Some(0),
            1000,
            1100,
        ))
        .unwrap();

    assert_eq!(repo.count_entries().unwrap(), 1);
    repo.delete_entry(id).unwrap();
    assert_eq!(repo.count_entries().unwrap(), 0);
}

// ── Replay Tests ────────────────────────────────────────

#[test]
fn test_get_replay_entries_by_session() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    let other_session = Session::new("host".to_string(), 200);
    repo.insert_session(&other_session).unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "cmd1".into(),
        "/tmp".into(),
        Some(0),
        1000,
        1100,
    ))
    .unwrap();
    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "cmd2".into(),
        "/tmp".into(),
        Some(0),
        2000,
        2100,
    ))
    .unwrap();
    repo.insert_entry(&Entry::new(
        other_session.id.clone(),
        "other_cmd".into(),
        "/tmp".into(),
        Some(0),
        3000,
        3100,
    ))
    .unwrap();

    // Replay for session — should be chronological (ASC)
    let entries = repo
        .get_replay_entries(Some(&session.id), &super::ReplayFilter::default())
        .unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].command, "cmd1"); // ASC order
    assert_eq!(entries[1].command, "cmd2");
}

#[test]
fn test_get_replay_entries_with_date_filter() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "old_cmd".into(),
        "/tmp".into(),
        Some(0),
        1000,
        1100,
    ))
    .unwrap();
    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "new_cmd".into(),
        "/tmp".into(),
        Some(0),
        5000,
        5100,
    ))
    .unwrap();

    // After 3000
    let entries = repo
        .get_replay_entries(
            None,
            &super::ReplayFilter {
                after: Some(3000),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command, "new_cmd");

    // Before 3000
    let entries = repo
        .get_replay_entries(
            None,
            &super::ReplayFilter {
                before: Some(3000),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command, "old_cmd");
}

#[test]
fn test_get_replay_entries_with_exit_code_filter() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "ok_cmd".into(),
        "/tmp".into(),
        Some(0),
        1000,
        1100,
    ))
    .unwrap();
    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "fail_cmd".into(),
        "/tmp".into(),
        Some(1),
        2000,
        2100,
    ))
    .unwrap();

    let entries = repo
        .get_replay_entries(
            None,
            &super::ReplayFilter {
                exit_code: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command, "fail_cmd");
}

#[test]
fn test_get_replay_entries_with_cwd_filter() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "cmd_a".into(),
        "/project".into(),
        Some(0),
        1000,
        1100,
    ))
    .unwrap();
    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "cmd_b".into(),
        "/other".into(),
        Some(0),
        2000,
        2100,
    ))
    .unwrap();

    let entries = repo
        .get_replay_entries(
            None,
            &super::ReplayFilter {
                cwd: Some("/project"),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command, "cmd_a");
}

#[test]
fn test_get_replay_entries_with_executor_filter() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    let mut entry1 = Entry::new(
        session.id.clone(),
        "human_cmd".into(),
        "/tmp".into(),
        Some(0),
        1000,
        1100,
    );
    entry1.executor_type = Some("human".to_string());
    repo.insert_entry(&entry1).unwrap();

    let mut entry2 = Entry::new(
        session.id.clone(),
        "agent_cmd".into(),
        "/tmp".into(),
        Some(0),
        2000,
        2100,
    );
    entry2.executor_type = Some("agent".to_string());
    entry2.executor = Some("claude".to_string());
    repo.insert_entry(&entry2).unwrap();

    let entries = repo
        .get_replay_entries(
            None,
            &super::ReplayFilter {
                executor: Some("agent"),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command, "agent_cmd");
}

// ── Export Tests ─────────────────────────────────────────

#[test]
fn test_export_entries_all() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "cmd1".into(),
        "/tmp".into(),
        Some(0),
        1000,
        1100,
    ))
    .unwrap();
    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "cmd2".into(),
        "/tmp".into(),
        Some(0),
        2000,
        2100,
    ))
    .unwrap();

    let entries = repo.export_entries(None, None).unwrap();
    assert_eq!(entries.len(), 2);
    // Export is ASC order
    assert_eq!(entries[0].command, "cmd1");
    assert_eq!(entries[1].command, "cmd2");
}

#[test]
fn test_export_entries_with_date_filter() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "old".into(),
        "/tmp".into(),
        Some(0),
        1000,
        1100,
    ))
    .unwrap();
    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "new".into(),
        "/tmp".into(),
        Some(0),
        5000,
        5100,
    ))
    .unwrap();

    let entries = repo.export_entries(Some(3000), None).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command, "new");

    let entries = repo.export_entries(None, Some(3000)).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command, "old");
}

// ── Tag-Session Lookup Tests ────────────────────────────

#[test]
fn test_get_tag_by_session() {
    let (_dir, repo) = setup_test_db();

    let tag_id = repo.create_tag("work", Some("Work tasks")).unwrap();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    // No tag initially
    let tag_name = repo.get_tag_by_session(&session.id).unwrap();
    assert!(tag_name.is_none());

    // Associate tag
    repo.tag_session(&session.id, Some(tag_id)).unwrap();
    let tag_name = repo.get_tag_by_session(&session.id).unwrap();
    assert_eq!(tag_name.as_deref(), Some("work"));

    // Clear tag
    repo.tag_session(&session.id, None).unwrap();
    let tag_name = repo.get_tag_by_session(&session.id).unwrap();
    assert!(tag_name.is_none());
}

#[test]
fn test_get_tag_by_nonexistent_session() {
    let (_dir, repo) = setup_test_db();
    let tag_name = repo.get_tag_by_session("nonexistent").unwrap();
    assert!(tag_name.is_none());
}

// ── Entry Exists (import dedup) ─────────────────────────

#[test]
fn test_entry_exists() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "git status".into(),
        "/tmp".into(),
        Some(0),
        1_000_000,
        1_001_000,
    ))
    .unwrap();
    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "cargo build".into(),
        "/tmp".into(),
        Some(0),
        2_000_000,
        2_001_000,
    ))
    .unwrap();

    // Existing entries should be found
    assert!(repo.entry_exists("git status", 1_000_000).unwrap());
    assert!(repo.entry_exists("cargo build", 2_000_000).unwrap());

    // Non-existing entries should not be found
    assert!(!repo.entry_exists("git status", 9_999_999).unwrap());
    assert!(!repo.entry_exists("nonexistent", 1_000_000).unwrap());
}

// ── Transaction Tests ───────────────────────────────────

#[test]
fn test_begin_and_commit_transaction() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    let tx = repo.transaction().unwrap();
    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "in_transaction".into(),
        "/tmp".into(),
        Some(0),
        1000,
        1100,
    ))
    .unwrap();
    tx.commit().unwrap();

    assert_eq!(repo.count_entries().unwrap(), 1);
}

#[test]
fn test_transaction_guard_rollback_on_drop() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    // Start a transaction, insert, but drop without committing
    {
        let _tx = repo.transaction().unwrap();
        repo.insert_entry(&Entry::new(
            session.id.clone(),
            "should_be_rolled_back".into(),
            "/tmp".into(),
            Some(0),
            2000,
            2100,
        ))
        .unwrap();
        // _tx drops here — should auto-rollback
    }

    assert_eq!(
        repo.count_entries().unwrap(),
        0,
        "Entry should be rolled back when TransactionGuard drops without commit"
    );
}

#[test]
fn test_transaction_guard_recommit() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    let tx = repo.transaction().unwrap();
    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "batch1".into(),
        "/tmp".into(),
        Some(0),
        1000,
        1100,
    ))
    .unwrap();
    tx.recommit().unwrap();

    repo.insert_entry(&Entry::new(
        session.id.clone(),
        "batch2".into(),
        "/tmp".into(),
        Some(0),
        2000,
        2100,
    ))
    .unwrap();
    tx.commit().unwrap();

    assert_eq!(repo.count_entries().unwrap(), 2);
}

// ── GC Tests ────────────────────────────────────────────

#[test]
fn test_gc_orphaned_sessions() {
    let (_dir, repo) = setup_test_db();

    let s1 = Session::new("host".to_string(), 100);
    repo.insert_session(&s1).unwrap();
    let s2 = Session::new("host".to_string(), 200);
    repo.insert_session(&s2).unwrap();

    // Only s1 has entries
    repo.insert_entry(&Entry::new(
        s1.id.clone(),
        "cmd".into(),
        "/tmp".into(),
        Some(0),
        1000,
        1100,
    ))
    .unwrap();

    assert_eq!(repo.count_orphaned_sessions().unwrap(), 1);
    let deleted = repo.delete_orphaned_sessions().unwrap();
    assert_eq!(deleted, 1);
    assert_eq!(repo.count_orphaned_sessions().unwrap(), 0);
}

#[test]
fn test_gc_orphaned_notes() {
    let (_dir, repo) = setup_test_db();
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    let id = repo
        .insert_entry(&Entry::new(
            session.id.clone(),
            "cmd".into(),
            "/tmp".into(),
            Some(0),
            1000,
            1100,
        ))
        .unwrap();

    repo.upsert_note(id, "test note").unwrap();
    assert_eq!(repo.count_orphaned_notes().unwrap(), 0);

    // Delete the entry — note becomes orphaned
    // (CASCADE may handle this, but test the count query regardless)
    repo.delete_entry(id).unwrap();

    // SQLite ON DELETE CASCADE should clean up, but our GC catches stragglers
    let orphaned = repo.count_orphaned_notes().unwrap();
    let deleted = repo.delete_orphaned_notes().unwrap();
    assert_eq!(orphaned, deleted as i64);
}

#[test]
fn test_vacuum() {
    let (_dir, repo) = setup_test_db();
    repo.vacuum().unwrap();
}

// ── Stats Tests ──────────────────────────────────────────

/// Insert a batch of entries with varying commands, times, and exit codes for stats tests.
fn populate_stats_data(repo: &Repository) {
    let session = Session::new("host".to_string(), 100);
    repo.insert_session(&session).unwrap();

    let now = chrono::Utc::now().timestamp_millis();
    let day_ms = 86_400_000i64;

    let entries = vec![
        // Recent commands (today)
        ("git status", Some(0), now - 1000, None, None),
        ("git status", Some(0), now - 2000, None, None),
        ("git diff", Some(0), now - 3000, None, None),
        (
            "cargo test",
            Some(0),
            now - 4000,
            Some("human"),
            Some("terminal"),
        ),
        (
            "cargo test",
            Some(1),
            now - 5000,
            Some("human"),
            Some("terminal"),
        ),
        // Older commands (10 days ago)
        (
            "npm install",
            Some(0),
            now - 10 * day_ms,
            Some("agent"),
            Some("claude-code"),
        ),
        ("ls -la", Some(0), now - 10 * day_ms + 1000, None, None),
        // Very old commands (60 days ago)
        ("old-command", Some(0), now - 60 * day_ms, None, None),
    ];

    for (cmd, exit_code, started_at, etype, exec) in entries {
        let mut entry = Entry::new(
            session.id.clone(),
            cmd.to_string(),
            "/test".to_string(),
            exit_code,
            started_at,
            started_at + 500,
        );
        entry.executor_type = etype.map(String::from);
        entry.executor = exec.map(String::from);
        repo.insert_entry(&entry).unwrap();
    }
}

#[test]
fn test_get_stats_all_time() {
    let (_dir, repo) = setup_test_db();
    populate_stats_data(&repo);

    let stats = repo.get_stats(None, 5, None).unwrap();
    assert_eq!(stats.total_commands, 8);
    assert_eq!(stats.unique_commands, 6);
    assert!(stats.success_count >= 6);
    assert!(stats.failure_count >= 1);
    assert!(!stats.top_commands.is_empty());
    // Both "git status" and "cargo test" have count=2; either can be first
    assert_eq!(
        stats.top_commands[0].1, 2,
        "Top command should have count 2"
    );
}

#[test]
fn test_get_stats_with_day_filter() {
    let (_dir, repo) = setup_test_db();
    populate_stats_data(&repo);

    // Only last 7 days — should exclude 10-day-old and 60-day-old entries
    let stats = repo.get_stats(Some(7), 5, None).unwrap();
    assert_eq!(stats.total_commands, 5, "Only recent entries within 7 days");
    assert_eq!(stats.period_days, Some(7));
}

#[test]
fn test_get_stats_with_tag_filter() {
    let (_dir, repo) = setup_test_db();
    populate_stats_data(&repo);

    let tag_id = repo.create_tag("work", None).unwrap();
    // Tag the session
    let sessions = repo.list_sessions(None, None, 1).unwrap();
    repo.tag_session(&sessions[0].id, Some(tag_id)).unwrap();

    let stats = repo.get_stats(None, 5, Some(tag_id)).unwrap();
    assert_eq!(
        stats.total_commands, 8,
        "All entries belong to the tagged session"
    );
}

#[test]
fn test_get_stats_empty_db() {
    let (_dir, repo) = setup_test_db();
    let stats = repo.get_stats(None, 5, None).unwrap();
    assert_eq!(stats.total_commands, 0);
    assert_eq!(stats.unique_commands, 0);
    assert!(stats.top_commands.is_empty());
    assert!(stats.hourly_distribution.is_empty());
}

#[test]
fn test_get_stats_executor_breakdown() {
    let (_dir, repo) = setup_test_db();
    populate_stats_data(&repo);

    let stats = repo.get_stats(None, 5, None).unwrap();
    assert!(
        !stats.executor_breakdown.is_empty(),
        "Should have executor breakdown"
    );
    // Entries without executor_type default to 'human'
    let total: i64 = stats.executor_breakdown.iter().map(|(_, c)| c).sum();
    assert_eq!(total, 8);
}

#[test]
fn test_get_daily_activity() {
    let (_dir, repo) = setup_test_db();
    populate_stats_data(&repo);

    let activity = repo.get_daily_activity(30, None).unwrap();
    assert!(!activity.is_empty(), "Should have daily activity data");

    // Each row is (date_string, day_of_week, count)
    for (date, dow, count) in &activity {
        assert!(!date.is_empty());
        assert!(*dow <= 6, "Day of week should be 0-6");
        assert!(*count > 0);
    }
}

#[test]
fn test_get_daily_activity_with_tag() {
    let (_dir, repo) = setup_test_db();
    populate_stats_data(&repo);

    let tag_id = repo.create_tag("test-tag", None).unwrap();
    let sessions = repo.list_sessions(None, None, 1).unwrap();
    repo.tag_session(&sessions[0].id, Some(tag_id)).unwrap();

    let activity = repo.get_daily_activity(30, Some(tag_id)).unwrap();
    assert!(!activity.is_empty());
}

#[test]
fn test_get_frequent_commands_basic() {
    let (_dir, repo) = setup_test_db();
    populate_stats_data(&repo);

    let frequent = repo
        .get_frequent_commands_filtered(None, 2, 1, 10, false)
        .unwrap();
    assert!(!frequent.is_empty(), "Should find frequently used commands");

    // "git status" appears 2 times → should be in results
    let git_status = frequent.iter().find(|(cmd, _, _)| cmd == "git status");
    assert!(
        git_status.is_some(),
        "git status (count=2) should appear with min_count=2"
    );
}

#[test]
fn test_get_frequent_commands_human_only() {
    let (_dir, repo) = setup_test_db();
    populate_stats_data(&repo);

    let all = repo
        .get_frequent_commands_filtered(None, 1, 1, 50, false)
        .unwrap();
    let human = repo
        .get_frequent_commands_filtered(None, 1, 1, 50, true)
        .unwrap();

    // human_only should exclude agent-typed entries (npm install by claude-code)
    let has_npm_all = all.iter().any(|(cmd, _, _)| cmd == "npm install");
    let has_npm_human = human.iter().any(|(cmd, _, _)| cmd == "npm install");
    assert!(
        has_npm_all,
        "npm install should appear in unfiltered results"
    );
    assert!(
        !has_npm_human,
        "npm install (agent executor) should be excluded in human_only mode"
    );
}

#[test]
fn test_get_frequent_commands_with_day_filter() {
    let (_dir, repo) = setup_test_db();
    populate_stats_data(&repo);

    let frequent = repo
        .get_frequent_commands_filtered(Some(7), 1, 1, 50, false)
        .unwrap();

    // Should exclude entries older than 7 days
    let has_old = frequent.iter().any(|(cmd, _, _)| cmd == "old-command");
    assert!(
        !has_old,
        "old-command (60 days ago) should be excluded with 7-day filter"
    );
}
