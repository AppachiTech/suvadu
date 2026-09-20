//! The single source of truth for *which* MCP capabilities exist.
//!
//! Both the server's advertisement (`tools/list`, `resources/list`) and the
//! MCP tab in `suv settings` are generated from the tables below, so the two
//! can never drift apart the way they did before (settings listed 15 of 21
//! tools and 7 of 8 resources, and neither write opt-in at all).
//!
//! What lives here is only *metadata*: stable name, a one-line summary for
//! the settings UI, and whether a capability is gated behind a write opt-in.
//! The JSON-RPC schemas stay next to their handlers in `tools.rs` /
//! `resources.rs`, and dispatch stays an explicit `match` on the name — this
//! catalog deliberately holds no function pointers, so nothing here can make
//! an unimplemented name callable.

use crate::config::McpConfig;

/// A configuration opt-in that must be on before a write capability is
/// advertised or callable. Writes are always off by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOptIn {
    /// `mcp.allow_session_summaries`
    SessionSummaries,
    /// `mcp.allow_skill_proposals`
    SkillProposals,
}

impl WriteOptIn {
    /// Every opt-in, in the order the settings UI shows them.
    pub const ALL: &'static [Self] = &[Self::SessionSummaries, Self::SkillProposals];

    /// Short label for the settings row.
    pub const fn label(self) -> &'static str {
        match self {
            Self::SessionSummaries => "Allow Saved Session Summaries",
            Self::SkillProposals => "Allow Skill Proposals",
        }
    }

    /// The `config.toml` key this row writes, for messages that point a user
    /// at the file.
    pub const fn config_key(self) -> &'static str {
        match self {
            Self::SessionSummaries => "mcp.allow_session_summaries",
            Self::SkillProposals => "mcp.allow_skill_proposals",
        }
    }

    /// Description shown in the settings detail pane.
    pub const fn description(self) -> &'static str {
        match self {
            Self::SessionSummaries => {
                "Let a connected agent store a session summary it generated, when you explicitly \
                 ask it to save one. Off by default — this is the only way an agent can write \
                 session text into Suvadu. Turning it off later stops new writes; summaries \
                 already saved are kept."
            }
            Self::SkillProposals => {
                "Let a connected agent propose a new shared skill. Proposals always land as \
                 pending review and are never active until you approve them in `suv skills` \
                 (Ctrl+P). Off by default: a store agents can both read and write is a \
                 shared-memory poisoning target."
            }
        }
    }

    /// Whether the opt-in is currently on.
    pub const fn is_on(self, mcp: &McpConfig) -> bool {
        match self {
            Self::SessionSummaries => mcp.allow_session_summaries,
            Self::SkillProposals => mcp.allow_skill_proposals,
        }
    }

    /// Turn the opt-in on or off.
    pub const fn set(self, mcp: &mut McpConfig, on: bool) {
        match self {
            Self::SessionSummaries => mcp.allow_session_summaries = on,
            Self::SkillProposals => mcp.allow_skill_proposals = on,
        }
    }
}

/// One MCP tool as far as advertisement and settings are concerned.
#[derive(Debug, Clone, Copy)]
pub struct ToolEntry {
    /// Stable tool name used over the wire, in `mcp.disabled_tools`, and as
    /// the `call_tool` dispatch key.
    pub name: &'static str,
    /// One-line summary for the settings detail pane.
    pub summary: &'static str,
    /// `Some` when the tool writes and needs an explicit opt-in first.
    pub write_opt_in: Option<WriteOptIn>,
}

impl ToolEntry {
    /// Whether this tool is advertised on a default (untouched) config.
    /// False exactly for the write tools, which require an opt-in.
    pub const fn default_available(&self) -> bool {
        self.write_opt_in.is_none()
    }
}

/// One MCP resource.
///
/// Resources are read-only, so there is no write opt-in to record. Two
/// things can take one away: an explicit `mcp.disabled_resources` entry,
/// and — for a resource that serves the same records as a tool — that
/// tool being disabled. Turning `what_failed` off and still serving
/// `suvadu://failures/recent` would make the setting a decoration, so
/// `mirrors_tool` ties the two together in the one place both the server
/// and the settings UI read. Enforced by `resource_available` and by
/// `disabling_a_tool_also_disables_the_resource_that_mirrors_it`.
#[derive(Debug, Clone, Copy)]
pub struct ResourceEntry {
    /// URI suffix after `suvadu://`, which is also what
    /// `mcp.disabled_resources` stores.
    pub uri_suffix: &'static str,
    /// Human title advertised over the wire.
    pub title: &'static str,
    /// Description advertised over the wire and shown in settings.
    pub description: &'static str,
    /// The tool this resource serves the same records as, if any.
    /// Disabling that tool also withdraws this resource.
    pub mirrors_tool: Option<&'static str>,
}

impl ResourceEntry {
    /// Full `suvadu://…` URI.
    pub fn uri(&self) -> String {
        format!("suvadu://{}", self.uri_suffix)
    }
}

/// Every tool the MCP server can advertise, in advertisement order.
pub const TOOLS: &[ToolEntry] = &[
    ToolEntry {
        name: "list_agent_sessions",
        summary: "List AI sessions captured from an agent's own transcript, with coverage and recorded usage.",
        write_opt_in: None,
    },
    ToolEntry {
        name: "get_agent_session",
        summary: "Read one captured agent session: paginated events, correlated commands, and saved summary checkpoints.",
        write_opt_in: None,
    },
    ToolEntry {
        name: "resolve_current_agent_session",
        summary: "Resolve \"this session\" to a captured session conservatively, returning candidates instead of guessing.",
        write_opt_in: None,
    },
    ToolEntry {
        name: "search_commands",
        summary: "Search shell command history by text pattern, with directory, executor, exit-code and date filters.",
        write_opt_in: None,
    },
    ToolEntry {
        name: "recent_commands",
        summary: "List the most recent commands, optionally filtered by directory or executor.",
        write_opt_in: None,
    },
    ToolEntry {
        name: "command_status",
        summary: "Show whether a specific command has been run before and how those runs ended.",
        write_opt_in: None,
    },
    ToolEntry {
        name: "get_prompts",
        summary: "Browse AI agent prompts and the commands each one triggered.",
        write_opt_in: None,
    },
    ToolEntry {
        name: "session_history",
        summary: "Full chronological command history for one shell session.",
        write_opt_in: None,
    },
    ToolEntry {
        name: "get_stats",
        summary: "Aggregate statistics: command count, success rate, top commands and directories.",
        write_opt_in: None,
    },
    ToolEntry {
        name: "list_sessions",
        summary: "List recent shell sessions with command counts, time ranges, and tags.",
        write_opt_in: None,
    },
    ToolEntry {
        name: "what_changed",
        summary: "Recorded commands in a directory, plus the change categories inferred from the ones that succeeded.",
        write_opt_in: None,
    },
    ToolEntry {
        name: "what_failed",
        summary: "Recent command failures grouped by the prompt or session that triggered them.",
        write_opt_in: None,
    },
    ToolEntry {
        name: "suggest_next",
        summary: "Guess likely next commands for a directory from frecency-ranked history.",
        write_opt_in: None,
    },
    ToolEntry {
        name: "assess_risk",
        summary: "Rate a command safe/low/medium/high/critical by rule match before it runs; not a sandbox.",
        write_opt_in: None,
    },
    ToolEntry {
        name: "find_agent_session",
        summary: "Search past agent sessions reconstructed from recorded shell commands.",
        write_opt_in: None,
    },
    ToolEntry {
        name: "replay_agent_session",
        summary: "Full chronological timeline of one agent session: every prompt and command.",
        write_opt_in: None,
    },
    ToolEntry {
        name: "learn_from_failures",
        summary: "Commands in a project that fail often, with recorded failure rates — not causes.",
        write_opt_in: None,
    },
    ToolEntry {
        name: "project_context",
        summary: "Project briefing: common commands, build/test patterns, failure rates, agent activity.",
        write_opt_in: None,
    },
    ToolEntry {
        name: "list_skills",
        summary: "List skills in the shared cross-agent skills library.",
        write_opt_in: None,
    },
    ToolEntry {
        name: "get_skill",
        summary: "Read the full content of one shared skill by name.",
        write_opt_in: None,
    },
    ToolEntry {
        name: "search_skills",
        summary: "Search the shared skills library by keyword across name, description, and triggers.",
        write_opt_in: None,
    },
    ToolEntry {
        name: "propose_skill",
        summary: "WRITE: propose a new shared skill, saved as pending review until a human approves it.",
        write_opt_in: Some(WriteOptIn::SkillProposals),
    },
    ToolEntry {
        name: "save_session_summary",
        summary: "WRITE: store a summary the calling agent generated as a reusable session checkpoint.",
        write_opt_in: Some(WriteOptIn::SessionSummaries),
    },
];

/// Every resource the MCP server can advertise, in advertisement order.
pub const RESOURCES: &[ResourceEntry] = &[
    ResourceEntry {
        uri_suffix: "history/recent",
        title: "Recent Commands",
        description: "Last 20 commands with exit codes, directories, and executors",
        mirrors_tool: Some("recent_commands"),
    },
    ResourceEntry {
        uri_suffix: "failures/recent",
        title: "Recent Failures",
        description: "Commands that failed in the last 24 hours, grouped by prompt",
        mirrors_tool: Some("what_failed"),
    },
    ResourceEntry {
        uri_suffix: "stats/today",
        title: "Today's Stats",
        description: "Command count, success rate, top commands, and top directories for today",
        mirrors_tool: Some("get_stats"),
    },
    ResourceEntry {
        uri_suffix: "risk/summary",
        title: "Risk Summary",
        description: "Risk assessment summary of recent agent commands",
        mirrors_tool: Some("assess_risk"),
    },
    ResourceEntry {
        uri_suffix: "agents/activity",
        title: "Agent Activity",
        description:
            "Overview of AI agent activity: which agents, how many commands, success rates",
        mirrors_tool: Some("find_agent_session"),
    },
    ResourceEntry {
        uri_suffix: "agents/sessions",
        title: "Recent Agent Sessions",
        description:
            "Summary of the 5 most recent AI agent sessions, with prompts and command counts",
        mirrors_tool: Some("find_agent_session"),
    },
    ResourceEntry {
        uri_suffix: "context/project",
        title: "Project Context",
        description: "Project briefing for the current directory: common commands, recent failures, agent activity, and workflow tips",
        mirrors_tool: Some("project_context"),
    },
    ResourceEntry {
        uri_suffix: "skills/index",
        title: "Skills Index",
        description: "Active skills in the shared cross-agent skills library — reusable instructions any MCP-capable agent can read instead of keeping its own copy",
        mirrors_tool: Some("list_skills"),
    },
];

/// The effective state of one tool for a given configuration.
///
/// The settings UI renders this single value rather than an opt-in checkbox
/// and a per-tool checkbox that can contradict each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    /// Advertised and callable.
    Available,
    /// Listed in `mcp.disabled_tools`.
    Disabled,
    /// Write tool whose opt-in is off.
    NeedsOptIn(WriteOptIn),
    /// Write tool whose opt-in is off *and* which is also explicitly
    /// disabled — both would have to change for it to come back.
    NeedsOptInAndDisabled(WriteOptIn),
}

impl ToolState {
    /// Whether the tool is advertised and callable.
    pub const fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }

    /// Why the tool is unavailable, for the settings row and for the error
    /// returned when a disabled tool is called anyway.
    pub fn reason(self) -> Option<String> {
        match self {
            Self::Available => None,
            Self::Disabled => Some("turned off in Settings → MCP → Tools".to_string()),
            Self::NeedsOptIn(opt_in) => Some(format!(
                "needs \"{}\" ({} = true)",
                opt_in.label(),
                opt_in.config_key()
            )),
            Self::NeedsOptInAndDisabled(opt_in) => Some(format!(
                "needs \"{}\" ({} = true) and is also turned off in Settings → MCP → Tools",
                opt_in.label(),
                opt_in.config_key()
            )),
        }
    }
}

/// Effective state of `entry` under `mcp`.
pub fn tool_state(entry: &ToolEntry, mcp: &McpConfig) -> ToolState {
    let disabled = mcp.disabled_tools.iter().any(|d| d == entry.name);
    match entry.write_opt_in {
        Some(opt_in) if !opt_in.is_on(mcp) => {
            if disabled {
                ToolState::NeedsOptInAndDisabled(opt_in)
            } else {
                ToolState::NeedsOptIn(opt_in)
            }
        }
        _ if disabled => ToolState::Disabled,
        _ => ToolState::Available,
    }
}

/// Effective state of the tool called `name`. An unknown name is reported as
/// `Disabled`: nothing outside the catalog is ever advertised or callable.
pub fn tool_state_by_name(name: &str, mcp: &McpConfig) -> ToolState {
    find_tool(name).map_or(ToolState::Disabled, |entry| tool_state(entry, mcp))
}

/// The catalog entry for `name`, if the tool exists.
pub fn find_tool(name: &str) -> Option<&'static ToolEntry> {
    TOOLS.iter().find(|t| t.name == name)
}

/// Whether the resource is advertised and readable.
pub fn resource_available(entry: &ResourceEntry, mcp: &McpConfig) -> bool {
    resource_block(entry, mcp).is_none()
}

/// Why the resource is unavailable, phrased for the settings row and for
/// the error a disabled resource read returns.
pub fn resource_block(entry: &ResourceEntry, mcp: &McpConfig) -> Option<String> {
    if mcp.disabled_resources.iter().any(|d| d == entry.uri_suffix) {
        return Some("turned off in Settings → MCP → Resources".to_string());
    }
    // A resource that serves the same records as a tool cannot outlive
    // that tool being switched off, or the switch means nothing.
    let mirrored = entry.mirrors_tool?;
    (!tool_state_by_name(mirrored, mcp).is_available()).then(|| {
        format!(
            "it serves the same records as '{mirrored}', which is {}",
            tool_state_by_name(mirrored, mcp)
                .reason()
                .unwrap_or_else(|| "disabled".to_string())
        )
    })
}

/// Make `entry` effectively available (or not) by reconciling both gates
/// that can hold a resource down, the same way [`set_tool_available`]
/// does for tools: enabling clears the `disabled_resources` entry *and*
/// re-enables the tool the resource mirrors; disabling only adds the
/// `disabled_resources` entry, leaving the tool itself callable (other
/// resources and direct calls may still want it). Returns `true` if
/// anything changed.
pub fn set_resource_available(entry: &ResourceEntry, mcp: &mut McpConfig, available: bool) -> bool {
    let before = resource_available(entry, mcp);
    if available {
        mcp.disabled_resources.retain(|d| d != entry.uri_suffix);
        if let Some(tool) = entry.mirrors_tool.and_then(find_tool) {
            set_tool_available(tool, mcp, true);
        }
    } else if !mcp.disabled_resources.iter().any(|d| d == entry.uri_suffix) {
        mcp.disabled_resources.push(entry.uri_suffix.to_string());
    }
    before != resource_available(entry, mcp)
}

/// Make `entry` effectively available (or not) by reconciling *both* keys
/// that gate it, so the two can never end up contradicting each other:
/// enabling clears any `disabled_tools` entry and turns the write opt-in on;
/// disabling adds the `disabled_tools` entry and leaves the opt-in alone
/// (other tools may share it). Returns `true` if anything changed.
pub fn set_tool_available(entry: &ToolEntry, mcp: &mut McpConfig, available: bool) -> bool {
    let before = tool_state(entry, mcp);
    if available {
        mcp.disabled_tools.retain(|d| d != entry.name);
        if let Some(opt_in) = entry.write_opt_in {
            opt_in.set(mcp, true);
        }
    } else if !mcp.disabled_tools.iter().any(|d| d == entry.name) {
        mcp.disabled_tools.push(entry.name.to_string());
    }
    before != tool_state(entry, mcp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mcp() -> McpConfig {
        McpConfig::default()
    }

    #[test]
    fn tool_names_are_unique() {
        let mut names: Vec<&str> = TOOLS.iter().map(|t| t.name).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "duplicate tool name in the catalog");
    }

    #[test]
    fn resource_suffixes_are_unique() {
        let mut uris: Vec<&str> = RESOURCES.iter().map(|r| r.uri_suffix).collect();
        let total = uris.len();
        uris.sort_unstable();
        uris.dedup();
        assert_eq!(uris.len(), total, "duplicate resource URI in the catalog");
    }

    #[test]
    fn every_entry_has_a_summary() {
        for tool in TOOLS {
            assert!(!tool.summary.is_empty(), "{} has no summary", tool.name);
        }
        for res in RESOURCES {
            assert!(
                !res.description.is_empty(),
                "{} has no description",
                res.uri_suffix
            );
        }
    }

    #[test]
    fn writes_are_off_by_default() {
        let mcp = mcp();
        for tool in TOOLS {
            let available = tool_state(tool, &mcp).is_available();
            assert_eq!(
                available,
                tool.write_opt_in.is_none(),
                "{} default availability does not match its opt-in requirement",
                tool.name
            );
        }
    }

    #[test]
    fn both_write_opt_ins_gate_a_tool() {
        for opt_in in WriteOptIn::ALL {
            assert!(
                TOOLS.iter().any(|t| t.write_opt_in == Some(*opt_in)),
                "{} gates no tool",
                opt_in.config_key()
            );
        }
    }

    #[test]
    fn effective_state_distinguishes_opt_in_from_explicit_disable() {
        let entry = find_tool("save_session_summary").unwrap();
        let mut cfg = mcp();
        assert_eq!(
            tool_state(entry, &cfg),
            ToolState::NeedsOptIn(WriteOptIn::SessionSummaries)
        );

        cfg.disabled_tools = vec!["save_session_summary".to_string()];
        assert_eq!(
            tool_state(entry, &cfg),
            ToolState::NeedsOptInAndDisabled(WriteOptIn::SessionSummaries)
        );

        cfg.allow_session_summaries = true;
        assert_eq!(tool_state(entry, &cfg), ToolState::Disabled);

        cfg.disabled_tools.clear();
        assert_eq!(tool_state(entry, &cfg), ToolState::Available);
        assert!(tool_state(entry, &cfg).reason().is_none());
    }

    #[test]
    fn set_tool_available_reconciles_both_gates() {
        let entry = find_tool("save_session_summary").unwrap();
        let mut cfg = mcp();
        cfg.disabled_tools = vec!["save_session_summary".to_string()];

        assert!(set_tool_available(entry, &mut cfg, true));
        assert!(cfg.allow_session_summaries);
        assert!(cfg.disabled_tools.is_empty());
        assert!(tool_state(entry, &cfg).is_available());

        // Turning it back off never silently flips the opt-in for other tools.
        assert!(set_tool_available(entry, &mut cfg, false));
        assert!(cfg.allow_session_summaries);
        assert_eq!(cfg.disabled_tools, vec!["save_session_summary".to_string()]);
        assert_eq!(tool_state(entry, &cfg), ToolState::Disabled);
    }

    #[test]
    fn unknown_tool_is_never_available() {
        assert!(!tool_state_by_name("definitely_not_a_tool", &mcp()).is_available());
        assert!(find_tool("definitely_not_a_tool").is_none());
    }

    /// A resource that serves the same records as a tool must not outlive
    /// that tool being switched off, and turning the resource back on must
    /// reconcile both gates rather than leave it stuck off.
    #[test]
    fn a_mirrored_resource_follows_its_tool_and_can_be_reconciled() {
        let history = RESOURCES
            .iter()
            .find(|r| r.uri_suffix == "history/recent")
            .unwrap();
        assert_eq!(history.mirrors_tool, Some("recent_commands"));

        let mut cfg = mcp();
        assert!(resource_available(history, &cfg));
        cfg.disabled_tools = vec!["recent_commands".to_string()];
        assert!(!resource_available(history, &cfg));
        assert!(resource_block(history, &cfg)
            .unwrap()
            .contains("recent_commands"));

        assert!(set_resource_available(history, &mut cfg, true));
        assert!(cfg.disabled_tools.is_empty());
        assert!(resource_available(history, &cfg));

        // Turning the resource off leaves the tool itself callable.
        assert!(set_resource_available(history, &mut cfg, false));
        assert_eq!(cfg.disabled_resources, vec!["history/recent".to_string()]);
        assert!(tool_state_by_name("recent_commands", &cfg).is_available());
    }

    #[test]
    fn resources_are_available_until_disabled() {
        let mut cfg = mcp();
        for res in RESOURCES {
            assert!(
                resource_available(res, &cfg),
                "{} must be available on a default config",
                res.uri_suffix
            );
        }
        cfg.disabled_resources = vec!["skills/index".to_string()];
        let skills = RESOURCES
            .iter()
            .find(|r| r.uri_suffix == "skills/index")
            .unwrap();
        assert!(!resource_available(skills, &cfg));
        assert_eq!(skills.uri(), "suvadu://skills/index");
    }
}
