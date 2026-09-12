use crate::ai_sessions::AiEvent;
use crate::models::{Entry, SessionSummary};

#[derive(Debug, Clone)]
pub struct AiSessionData {
    pub summary: SessionSummary,
    pub usage: Option<AiSessionUsage>,
    pub items: Vec<AiTimelineItem>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AiSessionUsage {
    pub total: Option<u64>,
    pub input: Option<u64>,
    pub cached_input: Option<u64>,
    pub output: Option<u64>,
    pub reasoning_output: Option<u64>,
}

#[derive(Debug, Clone)]
pub enum AiTimelineItem {
    Prompt {
        at: i64,
        text: String,
        cwd: String,
        source_id: String,
        turn_id: Option<String>,
        model: Option<String>,
    },
    Response {
        at: i64,
        text: String,
        cwd: String,
        source_id: String,
        turn_id: Option<String>,
        model: Option<String>,
    },
    Command {
        entry: Entry,
        source_id: String,
    },
    Interrupted {
        at: i64,
        cwd: String,
        source_id: String,
        turn_id: Option<String>,
        model: Option<String>,
    },
}

impl AiTimelineItem {
    pub const fn at(&self) -> i64 {
        match self {
            Self::Prompt { at, .. } | Self::Response { at, .. } | Self::Interrupted { at, .. } => {
                *at
            }
            Self::Command { entry, .. } => entry.started_at,
        }
    }

    const fn sort_order(&self) -> u8 {
        match self {
            Self::Prompt { .. } => 0,
            Self::Command { .. } => 1,
            Self::Response { .. } => 2,
            Self::Interrupted { .. } => 3,
        }
    }

    pub fn source_id(&self) -> &str {
        match self {
            Self::Prompt { source_id, .. }
            | Self::Response { source_id, .. }
            | Self::Command { source_id, .. }
            | Self::Interrupted { source_id, .. } => source_id,
        }
    }

    pub fn copy_text(&self) -> &str {
        match self {
            Self::Prompt { text, .. } | Self::Response { text, .. } => text,
            Self::Command { entry, .. } => &entry.command,
            Self::Interrupted { .. } => "Turn interrupted",
        }
    }

    pub const fn kind_label(&self) -> &'static str {
        match self {
            Self::Prompt { .. } => "Prompt",
            Self::Response { .. } => "Response",
            Self::Command { .. } => "Command",
            Self::Interrupted { .. } => "Interrupted",
        }
    }

    pub fn cwd(&self) -> &str {
        match self {
            Self::Prompt { cwd, .. }
            | Self::Response { cwd, .. }
            | Self::Interrupted { cwd, .. } => cwd,
            Self::Command { entry, .. } => &entry.cwd,
        }
    }

    pub fn model(&self) -> Option<&str> {
        match self {
            Self::Prompt { model, .. }
            | Self::Response { model, .. }
            | Self::Interrupted { model, .. } => model.as_deref(),
            Self::Command { .. } => None,
        }
    }

    pub fn turn_id(&self) -> Option<&str> {
        match self {
            Self::Prompt { turn_id, .. }
            | Self::Response { turn_id, .. }
            | Self::Interrupted { turn_id, .. } => turn_id.as_deref(),
            Self::Command { entry, .. } => entry
                .context
                .as_ref()
                .and_then(|context| context.get("codex_turn_id"))
                .map(String::as_str),
        }
    }
}

pub fn build_ai_session_data(
    summary: SessionSummary,
    events: Vec<AiEvent>,
    entries: Vec<Entry>,
) -> AiSessionData {
    let usage = events
        .iter()
        .rev()
        .find(|event| event.kind == "usage")
        .map(|event| {
            let total = &event.data["total"];
            AiSessionUsage {
                total: total["total_tokens"].as_u64(),
                input: total["input_tokens"].as_u64(),
                cached_input: total["cached_input_tokens"].as_u64(),
                output: total["output_tokens"].as_u64(),
                reasoning_output: total["reasoning_output_tokens"].as_u64(),
            }
        });
    let mut items = events
        .into_iter()
        .filter_map(|event| match event.kind.as_str() {
            "prompt" => event.data["text"]
                .as_str()
                .map(|text| AiTimelineItem::Prompt {
                    at: event.at,
                    text: text.to_owned(),
                    cwd: event.cwd,
                    source_id: event.id,
                    turn_id: event.turn_id,
                    model: event.model,
                }),
            "response" => event.data["text"]
                .as_str()
                .map(|text| AiTimelineItem::Response {
                    at: event.at,
                    text: text.to_owned(),
                    cwd: event.cwd,
                    source_id: event.id,
                    turn_id: event.turn_id,
                    model: event.model,
                }),
            "turn_interrupted" => Some(AiTimelineItem::Interrupted {
                at: event.at,
                cwd: event.cwd,
                source_id: event.id,
                turn_id: event.turn_id,
                model: event.model,
            }),
            _ => None,
        })
        .collect::<Vec<_>>();
    items.extend(entries.into_iter().map(|entry| {
        let source_id = entry
            .id
            .map_or_else(|| "command-unknown".into(), |id| format!("command-{id}"));
        AiTimelineItem::Command { entry, source_id }
    }));
    items.sort_by(|left, right| {
        left.at()
            .cmp(&right.at())
            .then_with(|| left.sort_order().cmp(&right.sort_order()))
            .then_with(|| left.source_id().cmp(right.source_id()))
    });
    AiSessionData {
        summary,
        usage,
        items,
    }
}
