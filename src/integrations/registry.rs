//! Static registry of known agent/IDE integrations.
//!
//! Several places in the codebase (currently `suv doctor`'s MCP-registration
//! and hook-script checks) need the same per-agent facts — its `suv init`
//! name, its hook-script filename prefix, and where to look for its MCP
//! registration file. Before this registry existed those facts were
//! duplicated as separate hardcoded conditionals per call site, so adding or
//! renaming an integration meant hunting down every copy. Now it's one table.

/// Static facts about one supported agent/IDE integration.
pub struct AgentIntegration {
    /// Identifier used in `suv init <id>` and in hook-script filenames
    /// (e.g. "claude-code", "codex").
    pub id: &'static str,
    /// Human-readable name for diagnostic output (e.g. "Claude Code").
    pub display_name: &'static str,
    /// Path to this agent's MCP server registration file, relative to
    /// `$HOME`. `None` if this integration doesn't support the MCP server.
    pub mcp_config_relpath: Option<&'static str>,
}

impl AgentIntegration {
    /// The `suv init <id>` command a user would run to (re)install this
    /// integration, for use in diagnostic hints.
    pub fn init_hint(&self) -> String {
        format!("suv init {}", self.id)
    }
}

/// All known agent/IDE integrations, in the order `suv doctor` reports them.
pub const REGISTRY: &[AgentIntegration] = &[
    AgentIntegration {
        id: "claude-code",
        display_name: "Claude Code",
        mcp_config_relpath: Some(".claude.json"),
    },
    AgentIntegration {
        id: "codex",
        display_name: "Codex",
        mcp_config_relpath: None,
    },
    AgentIntegration {
        id: "cursor",
        display_name: "Cursor",
        mcp_config_relpath: Some(".cursor/mcp.json"),
    },
    AgentIntegration {
        id: "antigravity",
        display_name: "Antigravity",
        mcp_config_relpath: None,
    },
    AgentIntegration {
        id: "opencode",
        display_name: "OpenCode",
        mcp_config_relpath: None,
    },
    AgentIntegration {
        id: "pi",
        display_name: "pi.dev",
        mcp_config_relpath: None,
    },
];

/// Find the integration whose `id` is a filename prefix of `hook_filename`
/// (e.g. "codex-post-tool.sh" -> the "codex" entry). Hook scripts are named
/// `<id>-<event>.sh` by `agent_hook.rs`, so this recovers the owning agent
/// from a script path found on disk.
pub fn find_by_hook_filename(hook_filename: &str) -> Option<&'static AgentIntegration> {
    REGISTRY.iter().find(|a| hook_filename.starts_with(a.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_by_hook_filename_matches_prefix() {
        let found = find_by_hook_filename("claude-code-post-tool-use.sh").unwrap();
        assert_eq!(found.id, "claude-code");
    }

    #[test]
    fn find_by_hook_filename_no_match_returns_none() {
        assert!(find_by_hook_filename("unknown-agent-hook.sh").is_none());
    }

    #[test]
    fn find_by_hook_filename_picks_longest_unambiguous_prefix() {
        // "codex" isn't a prefix of "claude-code...", so no cross-matching.
        let found = find_by_hook_filename("codex-prompt.sh").unwrap();
        assert_eq!(found.id, "codex");
    }

    #[test]
    fn every_registry_entry_has_a_nonempty_id_and_display_name() {
        for a in REGISTRY {
            assert!(!a.id.is_empty());
            assert!(!a.display_name.is_empty());
        }
    }

    #[test]
    fn init_hint_formats_as_suv_init_command() {
        let claude = REGISTRY.iter().find(|a| a.id == "claude-code").unwrap();
        assert_eq!(claude.init_hint(), "suv init claude-code");
    }
}
