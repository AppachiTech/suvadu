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
        }
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

        let data = build_ai_session_data(summary(), events, vec![command], vec![]);

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
        let data = build_ai_session_data(summary(), vec![event], vec![], vec![]);
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
            current: true,
        };
        let data = build_ai_session_data(summary(), vec![], vec![], vec![record.clone()]);
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
        );

        assert_eq!(data.usage.unwrap().total, Some(250));
    }
}
