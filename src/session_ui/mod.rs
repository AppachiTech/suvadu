mod picker;
mod session_data;
pub mod timeline;

pub use picker::run_session_picker;
pub use session_data::{build_ai_session_data, AiSessionData};
pub use timeline::{run_ai_session_timeline, run_session_timeline};

#[cfg(test)]
mod session_data_contract_tests {
    use super::*;
    use crate::ai_sessions::AiEvent;
    use crate::models::{Entry, SessionKind, SessionSummary};
    use serde_json::json;

    fn summary() -> SessionSummary {
        SessionSummary {
            id: "codex-test".into(),
            kind: SessionKind::Ai,
            hostname: String::new(),
            cwd: Some("/work".into()),
            agent: Some("openai-codex".into()),
            model: Some("model-b".into()),
            models: vec!["model-a".into(), "model-b".into()],
            total_tokens: Some(250),
            usage_complete: true,
            event_count: 3,
            created_at: 1_000,
            tag_name: None,
            cmd_count: 1,
            success_count: 1,
            first_activity_at: 1_000,
            last_activity_at: 4_000,
            preview: None,
            capture: None,
            revision: None,
        }
    }

    /// The handoff scaffold is built from captured records only: the first
    /// prompt as the goal, every command with its exit code as evidence, and
    /// the session's own capture gaps carried through verbatim.
    #[test]
    fn handoff_session_is_built_from_captured_records_and_their_ids() {
        let events = vec![
            AiEvent {
                id: "prompt-1".into(),
                turn_id: Some("turn-1".into()),
                kind: "prompt".into(),
                at: 1_000,
                model: Some("model-a".into()),
                cwd: "/work".into(),
                data: json!({"text": "Fix the flaky parser test"}),
            },
            AiEvent {
                id: "response-1".into(),
                turn_id: Some("turn-1".into()),
                kind: "response".into(),
                at: 4_000,
                model: Some("model-b".into()),
                cwd: "/work".into(),
                data: json!({"text": "The parser test passes now."}),
            },
        ];
        let mut failed = Entry::new(
            "codex-test".into(),
            "cargo test parser".into(),
            "/work".into(),
            Some(101),
            2_000,
            2_100,
        );
        failed.id = Some(7);
        let mut passed = Entry::new(
            "codex-test".into(),
            "cargo test parser".into(),
            "/work".into(),
            Some(0),
            3_000,
            3_100,
        );
        passed.id = Some(8);

        let mut summary = summary();
        summary.revision = Some("e2-c2-8".into());
        summary.capture = Some(crate::models::CaptureStatus {
            complete: false,
            known_missing: vec!["Records from a paused window were skipped".into()],
        });
        let data = build_ai_session_data(summary, events, vec![failed, passed], vec![], true);

        let handoff = session_data::handoff_session(&data);
        assert_eq!(handoff.session_id, "codex-test");
        assert_eq!(handoff.revision, "e2-c2-8");
        assert_eq!(
            handoff.goal,
            Some(("prompt-1".into(), "Fix the flaky parser test".into()))
        );
        assert_eq!(
            handoff.latest_answer,
            Some(("response-1".into(), "The parser test passes now.".into()))
        );
        assert_eq!(handoff.commands.len(), 2);
        assert_eq!(handoff.commands[0].id, "command-7");
        assert_eq!(handoff.commands[0].exit_code, Some(101));
        assert_eq!(handoff.commands[1].id, "command-8");
        assert_eq!(
            handoff.capture_known_missing,
            ["Records from a paused window were skipped"]
        );

        let text = crate::ai_sessions::handoff::scaffold(&handoff);
        assert!(text.contains("failed with exit 101 [command-7]"), "{text}");
        assert!(text.contains("Known capture gap"), "{text}");
    }

    #[test]
    fn ai_session_data_orders_prompts_commands_and_responses() {
        let events = vec![
            AiEvent {
                id: "response-1".into(),
                turn_id: Some("turn-1".into()),
                kind: "response".into(),
                at: 3_000,
                model: Some("model-b".into()),
                cwd: "/work".into(),
                data: json!({"text":"Done"}),
            },
            AiEvent {
                id: "prompt-1".into(),
                turn_id: Some("turn-1".into()),
                kind: "prompt".into(),
                at: 1_000,
                model: Some("model-a".into()),
                cwd: "/work".into(),
                data: json!({"text":"Run the check"}),
            },
        ];
        let mut command = Entry::new(
            "codex-test".into(),
            "cargo test".into(),
            "/work".into(),
            Some(0),
            2_000,
            2_100,
        );
        command.id = Some(42);

        let data = build_ai_session_data(summary(), events, vec![command], vec![], true);

        assert!(matches!(
            data.items[0],
            session_data::AiTimelineItem::Prompt { .. }
        ));
        assert!(matches!(
            data.items[1],
            session_data::AiTimelineItem::Command { .. }
        ));
        assert!(matches!(
            data.items[2],
            session_data::AiTimelineItem::Response { .. }
        ));
        assert_eq!(data.items[0].copy_text(), "Run the check");
        assert_eq!(data.items[1].source_id(), "command-42");
    }

    #[test]
    fn ai_only_session_keeps_conversation_without_commands() {
        let event = AiEvent {
            id: "prompt-1".into(),
            turn_id: None,
            kind: "prompt".into(),
            at: 1_000,
            model: Some("model-a".into()),
            cwd: "/work".into(),
            data: json!({"text":"Explain this"}),
        };
        let data = build_ai_session_data(summary(), vec![event], vec![], vec![], true);
        assert_eq!(data.items.len(), 1);
        assert!(matches!(
            data.items[0],
            session_data::AiTimelineItem::Prompt { .. }
        ));
    }

    #[test]
    fn build_ai_session_data_carries_summaries_through_unchanged() {
        let record = crate::models::AiSummaryRecord {
            id: "summary-1".into(),
            text: "Did the thing".into(),
            agent: "claude".into(),
            model: "sonnet".into(),
            created_at: 5_000,
            basis: crate::models::SummaryBasis::Current,
        };
        let data = build_ai_session_data(summary(), vec![], vec![], vec![record.clone()], true);
        assert_eq!(data.summaries, vec![record]);
    }

    #[test]
    fn ai_session_data_uses_latest_cumulative_usage_snapshot() {
        let usage_event = |id: &str, at: i64, total: u64| AiEvent {
            id: id.into(),
            turn_id: Some("turn-1".into()),
            kind: "usage".into(),
            at,
            model: Some("model-b".into()),
            cwd: "/work".into(),
            data: json!({
                "total": {
                    "total_tokens": total,
                    "input_tokens": total - 20,
                    "cached_input_tokens": 40,
                    "output_tokens": 20,
                    "reasoning_output_tokens": 10
                }
            }),
        };
        let data = build_ai_session_data(
            summary(),
            vec![
                usage_event("usage-1", 2_000, 120),
                usage_event("usage-2", 3_000, 250),
            ],
            vec![],
            vec![],
            true,
        );

        assert_eq!(data.usage.unwrap().total, Some(250));
    }
}
