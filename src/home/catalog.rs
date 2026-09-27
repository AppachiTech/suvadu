//! What Home knows about Suvadu: its task categories, the features in them,
//! how each one is opened, and the words people use to look for it.
//!
//! Everything here is static data. A feature's search words are curated —
//! the terms someone would type for the task — and its launch arguments are
//! a fixed list, never built from anything typed.

use std::ffi::OsString;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FeatureId(pub &'static str);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CategoryId(pub &'static str);

/// How an executable feature runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchMode {
    /// A full-screen tool that owns the terminal until it exits.
    Interactive,
    /// A picker whose choice arrives on stdout, shown here to copy.
    Selection,
    /// A short read-only command whose output is shown here.
    Report,
}

/// What Enter does on a feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Open(LaunchMode),
    /// Explain only; nothing is run.
    Guide,
    /// Open the command reference.
    Reference,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchRequest {
    pub feature: FeatureId,
    pub args: Vec<OsString>,
    pub mode: LaunchMode,
}

/// A command line to copy. `<ANGLE>` marks a value the reader supplies.
#[derive(Clone, Copy, Debug)]
pub struct Example {
    pub command: &'static str,
    pub note: &'static str,
}

impl Example {
    /// It has a `<NAME>` part to fill in before it can be used.
    pub fn needs_input(&self) -> bool {
        has_placeholder(self.command)
    }
}

/// Whether `text` holds a `<UPPER_CASE>` placeholder (not a redirection).
pub fn has_placeholder(text: &str) -> bool {
    text.split('<').skip(1).any(|rest| {
        rest.split_once('>').is_some_and(|(name, _)| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || matches!(c, '_' | '-'))
        })
    })
}

#[derive(Clone, Copy, Debug)]
pub struct Category {
    pub id: CategoryId,
    pub title: &'static str,
    pub description: &'static str,
    /// A monochrome symbol shown before the title with Unicode icons;
    /// ASCII icons show the words alone.
    pub unicode: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub struct Feature {
    pub id: FeatureId,
    pub category: CategoryId,
    pub title: &'static str,
    /// A few words on what it is for, for category previews.
    pub summary: &'static str,
    /// One sentence: what it does for you.
    pub description: &'static str,
    /// What opens or happens, including whether it can change data.
    pub opens: &'static str,
    /// The real command, as you would type it.
    pub command: &'static str,
    pub shortcut: Option<&'static str>,
    pub note: Option<&'static str>,
    pub synonyms: &'static [&'static str],
    /// Every public command path this feature explains.
    pub command_paths: &'static [&'static str],
    /// Arguments after the executable, for `Action::Open` only.
    pub args: &'static [&'static str],
    pub action: Action,
    pub open_label: &'static str,
    /// Paragraphs shown with the feature.
    pub guide: &'static [&'static str],
    pub examples: &'static [Example],
}

impl Feature {
    /// The primary action, named for what it really does.
    pub const fn action_label(&self) -> &'static str {
        match self.action {
            Action::Guide => "Read instructions",
            Action::Open(LaunchMode::Report) => "View report",
            Action::Open(_) | Action::Reference => self.open_label,
        }
    }
}

pub const REFERENCE: FeatureId = FeatureId("reference");
const REFERENCE_CATEGORY: CategoryId = CategoryId("reference");

const FIND: CategoryId = CategoryId("find");
const SESSION: CategoryId = CategoryId("session");
const ORGANIZE: CategoryId = CategoryId("organize");
const UNDERSTAND: CategoryId = CategoryId("understand");
const AI: CategoryId = CategoryId("ai");
const CONNECT: CategoryId = CategoryId("connect");
const MANAGE: CategoryId = CategoryId("manage");

static CATEGORIES: [Category; 7] = [
    Category {
        id: FIND,
        title: "Find a command",
        description: "Search past commands and narrow where to look",
        unicode: "⌕",
    },
    Category {
        id: SESSION,
        title: "Review a session",
        description: "Follow what happened in a shell or AI session",
        unicode: "◷",
    },
    Category {
        id: ORGANIZE,
        title: "Organize commands",
        description: "Save, annotate, and shorten commands you reuse",
        unicode: "★",
    },
    Category {
        id: UNDERSTAND,
        title: "Understand my activity",
        description: "Explore command usage and trends",
        unicode: "▤",
    },
    Category {
        id: AI,
        title: "Review AI activity",
        description: "Inspect agent commands, prompts, and reports",
        unicode: "◆",
    },
    Category {
        id: CONNECT,
        title: "Connect tools",
        description: "Set up your shell, AI integrations, and shared skills",
        unicode: "⇄",
    },
    Category {
        id: MANAGE,
        title: "Manage Suvadu",
        description: "Configure, diagnose, protect, and maintain your data",
        unicode: "≡",
    },
];

/// Defaults for the catalog below; every feature overrides what applies.
const BASE: Feature = Feature {
    id: FeatureId(""),
    category: FIND,
    title: "",
    summary: "",
    description: "",
    opens: "",
    command: "",
    shortcut: None,
    note: None,
    synonyms: &[],
    command_paths: &[],
    args: &[],
    action: Action::Guide,
    open_label: "",
    guide: &[],
    examples: &[],
};

const fn ex(command: &'static str, note: &'static str) -> Example {
    Example { command, note }
}

static FEATURES: &[Feature] = &[
    // ── Find a command ──────────────────────────────────────────
    Feature {
        id: FeatureId("search"),
        title: "Search history",
        summary: "Find and copy a past command",
        description: "Search everything you have run, and pick a command to reuse.",
        opens: "Opens the search screen with your saved search settings. The command you \
                pick is shown here to copy; nothing is run. The search screen can also \
                delete an entry (after asking), bookmark a command, add a note or tag a \
                session.",
        command: "suv search",
        shortcut: Some(
            "Ctrl+R in your shell opens the same search and puts the pick on your prompt",
        ),
        synonyms: &[
            "history",
            "recall",
            "find",
            "previous commands",
            "past commands",
            "ctrl+r",
            "reverse search",
            "look up",
        ],
        command_paths: &["search"],
        args: &["search"],
        action: Action::Open(LaunchMode::Selection),
        open_label: "Open search",
        examples: &[
            ex("suv search --query \"git push\"", "Start with a query"),
            ex("suv search --match fuzzy", "Letters in order, gaps allowed"),
            ex(
                "suv search --include-agents",
                "Include commands AI agents ran",
            ),
        ],
        ..BASE
    },
    Feature {
        id: FeatureId("search-directory"),
        title: "Commands in this directory",
        summary: "Only what ran in this folder",
        description: "Search only what you ran in the directory Home was opened from.",
        opens: "Opens search limited to exactly this directory, not its subdirectories. \
                The command you pick is shown here to copy; nothing is run. The search \
                screen can also delete an entry (after asking), bookmark a command, add a \
                note or tag a session.",
        command: "suv search --scope directory",
        shortcut: Some("Ctrl+P in search changes the scope"),
        synonyms: &[
            "folder",
            "directory",
            "here",
            "current directory",
            "this folder",
            "cwd",
            "path",
        ],
        args: &["search", "--scope", "directory"],
        action: Action::Open(LaunchMode::Selection),
        open_label: "Open search",
        ..BASE
    },
    Feature {
        id: FeatureId("search-workspace"),
        title: "Commands in this workspace",
        summary: "Search across the current project",
        description: "Search what you ran anywhere in the current Git repository or worktree.",
        opens: "Opens search limited to the enclosing Git repository. Outside one, search \
                says so and shows everything instead. The command you pick is shown here to \
                copy; nothing is run. The search screen can also delete an entry (after \
                asking), bookmark a command, add a note or tag a session.",
        command: "suv search --scope workspace",
        shortcut: Some("Ctrl+P in search changes the scope"),
        synonyms: &[
            "project",
            "workspace",
            "repository",
            "repo",
            "git",
            "this project",
        ],
        args: &["search", "--scope", "workspace"],
        action: Action::Open(LaunchMode::Selection),
        open_label: "Open search",
        ..BASE
    },
    Feature {
        id: FeatureId("search-failed"),
        title: "Failed commands",
        summary: "Find commands that failed",
        description: "Find commands that ended with an error, to fix and try again.",
        opens: "Opens search showing only commands with a non-zero exit status — any \
                failure, not just exit code 1. The command you pick is shown here to copy; \
                nothing is run. The search screen can also delete an entry (after asking), \
                bookmark a command, add a note or tag a session.",
        command: "suv search --failed",
        shortcut: Some("Ctrl+E in search shows failures only"),
        synonyms: &[
            "failed",
            "failure",
            "failures",
            "errors",
            "error",
            "non-zero",
            "exit code",
            "went wrong",
        ],
        args: &["search", "--failed"],
        action: Action::Open(LaunchMode::Selection),
        open_label: "Open search",
        ..BASE
    },
    Feature {
        id: FeatureId("history"),
        title: "Print history",
        summary: "A plain list to read or pipe",
        description: "Print recent commands as plain text to read, filter or pipe.",
        opens: "Shows your last 25 commands here. From your shell, suv history also filters \
                by date, directory, exit code or executor, and prints JSON lines.",
        command: "suv history",
        synonyms: &[
            "print",
            "list",
            "recent",
            "pipe",
            "grep",
            "json",
            "plain text",
            "filter",
        ],
        command_paths: &["history"],
        args: &["history"],
        action: Action::Open(LaunchMode::Report),
        examples: &[
            ex("suv history -n 100", "The last 100 commands"),
            ex("suv history --here", "Only this directory"),
            ex(
                "suv history --after today --json",
                "Today's commands as JSON lines",
            ),
            ex("suv history --executor agent", "Commands AI agents ran"),
            ex("suv history | grep cargo", "Pipe to another tool"),
        ],
        ..BASE
    },
    // ── Review a session ────────────────────────────────────────
    Feature {
        id: FeatureId("sessions"),
        category: SESSION,
        title: "Browse sessions",
        summary: "A shell or AI session as a timeline",
        description:
            "See a shell or AI session as a timeline of what ran, where, and how it ended.",
        opens: "Opens the session picker, then the timeline of the session you choose. \
                Esc returns here.",
        command: "suv sessions",
        synonyms: &[
            "session",
            "sessions",
            "what happened",
            "timeline",
            "shell session",
            "terminal session",
            "ai session",
            "story",
        ],
        command_paths: &["sessions"],
        args: &["sessions"],
        action: Action::Open(LaunchMode::Interactive),
        open_label: "Open sessions",
        examples: &[
            ex("suv sessions --list", "List sessions without opening them"),
            ex(
                "suv sessions --after <YYYY-MM-DD>",
                "Sessions active after a date",
            ),
            ex("suv sessions --tag <TAG>", "Sessions with a tag"),
        ],
        ..BASE
    },
    Feature {
        id: FeatureId("replay"),
        category: SESSION,
        title: "Replay a time range",
        summary: "Print commands in the order they ran",
        description: "Print the commands of a session or a time range in the order they ran.",
        command: "suv replay",
        synonyms: &[
            "replay",
            "chronological",
            "time range",
            "today",
            "yesterday",
            "what did i do",
            "in order",
        ],
        command_paths: &["replay"],
        guide: &[
            "suv replay prints commands in order. It shows them; it never runs them again.",
            "Without options it shows the current shell session. Dates take YYYY-MM-DD, \
             \"today\" or \"yesterday\".",
            "Neither does suv guard, which assesses a command line; suv wrap is the one that \
             runs a command, and records it.",
        ],
        examples: &[
            ex("suv replay --after today", "Everything since midnight"),
            ex(
                "suv replay --after yesterday --here",
                "Since yesterday, in this directory",
            ),
            ex("suv replay --session <SESSION_ID>", "One session, by ID"),
        ],
        ..BASE
    },
    // ── Organize commands ───────────────────────────────────────
    Feature {
        id: FeatureId("bookmarks"),
        category: ORGANIZE,
        title: "Bookmarks",
        summary: "Commands you saved to reuse",
        description: "Keep commands you reuse in one list, with an optional label.",
        opens: "Opens your bookmarks, where you can also add, edit and delete them \
                (deleting asks first). The command you pick is shown here to copy; nothing \
                is run.",
        command: "suv bookmarks",
        synonyms: &[
            "bookmark",
            "save command",
            "saved commands",
            "favorites",
            "favourites",
            "pin",
            "star",
            "keep",
        ],
        command_paths: &[
            "bookmarks",
            "bookmarks add",
            "bookmarks list",
            "bookmarks remove",
        ],
        args: &["bookmarks"],
        action: Action::Open(LaunchMode::Selection),
        open_label: "Open bookmarks",
        examples: &[
            ex(
                "suv bookmarks add \"<COMMAND>\" --label \"<LABEL>\"",
                "Save a command",
            ),
            ex("suv bookmarks list", "Print them"),
            ex("suv bookmarks remove \"<COMMAND>\"", "Forget one"),
        ],
        ..BASE
    },
    Feature {
        id: FeatureId("tags"),
        category: ORGANIZE,
        title: "Tags",
        summary: "Group sessions under a name",
        description: "Name a group of sessions, then filter search, history and stats by it.",
        opens: "Lists your tags here. Creating, renaming and attaching tags is done from \
                your shell.",
        command: "suv tag list",
        synonyms: &["tag", "tags", "label", "group", "categorize", "work"],
        command_paths: &[
            "tag",
            "tag list",
            "tag create",
            "tag associate",
            "tag update",
        ],
        args: &["tag", "list"],
        action: Action::Open(LaunchMode::Report),
        guide: &[
            "suv tag associate tags the shell session it is run in, so run it in the \
                  shell you want to tag.",
        ],
        examples: &[
            ex("suv tag create <NAME> -d \"<DESCRIPTION>\"", "Create a tag"),
            ex("suv tag associate <NAME>", "Tag the current shell session"),
            ex(
                "suv tag update <NAME> --new-name <NEW_NAME>",
                "Rename a tag",
            ),
        ],
        ..BASE
    },
    Feature {
        id: FeatureId("notes"),
        category: ORGANIZE,
        title: "Notes on commands",
        summary: "Note why a command mattered",
        description: "Attach a note to one recorded command, such as why it worked.",
        command: "suv note <ENTRY_ID>",
        synonyms: &[
            "note",
            "notes",
            "annotate",
            "comment",
            "remember why",
            "explain",
        ],
        command_paths: &["note"],
        guide: &[
            "A note belongs to one history entry, named by its ID. suv history --json prints \
             each entry's \"id\".",
        ],
        examples: &[
            ex(
                "suv note <ENTRY_ID> -c \"<TEXT>\"",
                "Add or replace the note",
            ),
            ex("suv note <ENTRY_ID>", "Show it"),
            ex("suv note <ENTRY_ID> --delete", "Remove it"),
            ex("suv history --json -n 5", "Find entry IDs"),
        ],
        ..BASE
    },
    Feature {
        id: FeatureId("aliases"),
        category: ORGANIZE,
        title: "Alias manager",
        summary: "Short names for long commands",
        description: "Give short names to commands you type often.",
        opens: "Opens the alias manager, which can add, change and delete your saved \
                aliases. Esc returns here.",
        command: "suv aliases",
        note: Some(
            "Saved aliases reach shells started afterwards. See Load aliases in your \
                    shell for one that is already open.",
        ),
        synonyms: &[
            "alias",
            "aliases",
            "shortcuts",
            "shorten",
            "abbreviation",
            "short names",
        ],
        command_paths: &[
            "aliases",
            "aliases add",
            "aliases remove",
            "aliases list",
            "aliases add-suggested",
        ],
        args: &["aliases"],
        action: Action::Open(LaunchMode::Interactive),
        open_label: "Open alias manager",
        examples: &[
            ex("suv aliases add <NAME> \"<COMMAND>\"", "Add one"),
            ex("suv aliases list", "Print them"),
            ex("suv aliases remove <NAME>", "Remove one"),
            ex(
                "suv aliases add-suggested",
                "Pick from suggestions and save",
            ),
        ],
        ..BASE
    },
    Feature {
        id: FeatureId("alias-suggestions"),
        category: ORGANIZE,
        title: "Alias suggestions",
        summary: "Aliases worth adding, from your habits",
        description: "See long commands you type often, each with a suggested alias.",
        opens: "Opens the suggestions screen. Esc returns here.",
        command: "suv aliases suggest",
        synonyms: &[
            "shortcuts",
            "long commands",
            "suggest",
            "suggestions",
            "frequent",
            "type less",
        ],
        command_paths: &["aliases suggest"],
        args: &["aliases", "suggest"],
        action: Action::Open(LaunchMode::Interactive),
        open_label: "Open alias suggestions",
        examples: &[
            ex("suv aliases suggest --text", "Print them instead"),
            ex(
                "suv aliases suggest --days 30 -c 5",
                "Last 30 days, at least 5 uses",
            ),
        ],
        ..BASE
    },
    Feature {
        id: FeatureId("alias-apply"),
        category: ORGANIZE,
        title: "Load aliases in your shell",
        summary: "Use saved aliases in your shell",
        description: "Write your aliases to the file your shell loads, and use them right away.",
        command: "suv aliases apply",
        synonyms: &["apply", "source", "load aliases", "aliases file", "reload"],
        command_paths: &["aliases apply"],
        guide: &[
            "suv aliases apply rewrites Suvadu's aliases file. Shells set up with suv init \
             load that file when they start, so new terminals have your aliases.",
            "A shell that is already open keeps its old aliases until it starts again. Home \
             cannot change the shell it was opened from.",
        ],
        examples: &[
            ex("suv aliases apply", "Rewrite the aliases file"),
            ex(
                "exec \"$SHELL\"",
                "Run in your shell: restart it to load them",
            ),
            ex(
                "suv aliases apply --stdout",
                "Print the alias lines instead",
            ),
        ],
        ..BASE
    },
    // ── Understand my activity ──────────────────────────────────
    Feature {
        id: FeatureId("stats"),
        category: UNDERSTAND,
        title: "Usage statistics",
        summary: "Your most used commands, and when",
        description: "See which commands and directories you use most, and when, over any period.",
        opens: "Opens the statistics screen. Esc returns here.",
        command: "suv stats",
        synonyms: &[
            "statistics",
            "stats",
            "usage",
            "trends",
            "most used",
            "top commands",
            "analytics",
            "activity",
            "habits",
        ],
        command_paths: &["stats"],
        args: &["stats"],
        action: Action::Open(LaunchMode::Interactive),
        open_label: "Open statistics",
        examples: &[
            ex("suv stats --days 30", "The last 30 days"),
            ex("suv stats --human", "Only commands you typed"),
            ex("suv stats --text", "Print instead of opening a screen"),
            ex("suv stats --json", "JSON for scripts"),
        ],
        ..BASE
    },
    // ── Review AI activity ──────────────────────────────────────
    Feature {
        id: FeatureId("agent-dashboard"),
        category: AI,
        title: "Agent dashboard",
        summary: "What AI agents ran, with risk",
        description: "Review what AI agents ran today, with a risk level for each command.",
        opens: "Opens the agent dashboard for today; the period can be changed there. Esc \
                returns here.",
        command: "suv agent dashboard",
        note: Some(
            "Agent commands appear once an integration is set up; see AI tool \
                    integrations.",
        ),
        synonyms: &[
            "ai",
            "agent",
            "agents",
            "claude",
            "codex",
            "cursor",
            "opencode",
            "antigravity",
            "risk",
            "assistant",
            "llm",
        ],
        command_paths: &["agent", "agent dashboard"],
        args: &["agent", "dashboard"],
        action: Action::Open(LaunchMode::Interactive),
        open_label: "Open agent dashboard",
        ..BASE
    },
    Feature {
        id: FeatureId("agent-prompts"),
        category: AI,
        title: "Prompt explorer",
        summary: "Prompts and the commands they led to",
        description: "Browse the prompts you gave AI agents and the commands each one led to.",
        opens: "Opens the prompt explorer for the last 7 days. Esc returns here.",
        command: "suv agent prompts",
        note: Some(
            "Prompts appear only from integrations that capture them; commands \
                    without a prompt are still in the dashboard.",
        ),
        synonyms: &[
            "prompts",
            "prompt",
            "what i asked",
            "conversation",
            "claude",
            "codex",
            "cursor",
            "opencode",
        ],
        command_paths: &["agent prompts"],
        args: &["agent", "prompts"],
        action: Action::Open(LaunchMode::Interactive),
        open_label: "Open prompt explorer",
        ..BASE
    },
    Feature {
        id: FeatureId("agent-stats"),
        category: AI,
        title: "Agent statistics",
        summary: "Usage per AI agent",
        description: "See usage analytics for each AI agent over the last 30 days.",
        opens: "Opens agent statistics. Esc returns here.",
        command: "suv agent stats",
        synonyms: &["agent usage", "ai stats", "per agent", "agent analytics"],
        command_paths: &["agent stats"],
        args: &["agent", "stats"],
        action: Action::Open(LaunchMode::Interactive),
        open_label: "Open agent statistics",
        ..BASE
    },
    Feature {
        id: FeatureId("agent-report"),
        category: AI,
        title: "Agent activity report",
        summary: "A shareable report of agent activity",
        description: "Summarize today's AI agent commands and their risk, as text to share.",
        opens: "Shows today's report here. From your shell it also takes dates, one agent, \
                markdown or JSON, and --fail-on for hooks and CI.",
        command: "suv agent report",
        synonyms: &["report", "audit", "markdown", "ci", "risky", "summary"],
        command_paths: &["agent report"],
        args: &["agent", "report"],
        action: Action::Open(LaunchMode::Report),
        examples: &[
            ex("suv agent report --format markdown", "Markdown to paste"),
            ex(
                "suv agent report --after \"3 days ago\" --here",
                "Three days, this directory",
            ),
            ex("suv agent report --executor claude-code", "One agent"),
            ex(
                "suv agent report --fail-on high",
                "Exit non-zero on a high-risk command",
            ),
        ],
        ..BASE
    },
    Feature {
        id: FeatureId("agent-sessions"),
        category: AI,
        title: "Captured AI sessions",
        summary: "Captured AI sessions as JSON",
        description:
            "Read captured AI sessions — events, commands, tokens and summaries — as JSON.",
        command: "suv agent sessions",
        synonyms: &[
            "ai sessions",
            "agent sessions",
            "transcript",
            "tokens",
            "summaries",
            "json",
        ],
        command_paths: &["agent sessions", "agent session"],
        guide: &[
            "These print JSON for scripts and agents. To read a session yourself, use \
                  Browse sessions, which shows shell and AI sessions as timelines.",
        ],
        examples: &[
            ex("suv agent sessions", "List captured sessions"),
            ex("suv agent session <SESSION_ID>", "One session in full"),
        ],
        ..BASE
    },
    Feature {
        id: FeatureId("agent-import"),
        category: AI,
        title: "Import an AI transcript",
        summary: "Add a missed Codex or Claude session",
        description: "Add a Codex or Claude Code session that was not captured as it happened.",
        command: "suv agent import-session <PATH>",
        synonyms: &[
            "import transcript",
            "transcript",
            "jsonl",
            "claude code transcript",
            "codex transcript",
            "missed session",
        ],
        command_paths: &["agent import-session"],
        guide: &[
            "Give it the path of a native Codex or Claude Code JSONL transcript; the \
                  format is detected. Importing the same file again adds only what is new. \
                  It prints a JSON result.",
        ],
        examples: &[ex(
            "suv agent import-session <PATH>",
            "Import one transcript",
        )],
        ..BASE
    },
    Feature {
        id: FeatureId("agent-delete"),
        category: AI,
        title: "Delete a captured AI session",
        summary: "Remove a captured AI session",
        description: "Remove one captured AI session and everything recorded with it.",
        command: "suv agent delete-session <SESSION_ID>",
        synonyms: &[
            "delete session",
            "remove ai session",
            "forget session",
            "erase",
        ],
        command_paths: &["agent delete-session"],
        guide: &[
            "This deletes the session's imported data and summaries, and the shell \
                  commands recorded with it. It cannot be undone; take a backup first if you \
                  may want them back.",
        ],
        examples: &[
            ex("suv backup", "Keep a copy first"),
            ex(
                "suv agent delete-session <SESSION_ID>",
                "Delete the session",
            ),
        ],
        ..BASE
    },
    Feature {
        id: FeatureId("guard"),
        category: AI,
        title: "Assess a command's risk",
        summary: "Check a command's risk before it runs",
        description: "Check a command line against Suvadu's risk rules before it runs.",
        command: "suv guard \"<COMMAND>\"",
        synonyms: &[
            "risk",
            "dangerous",
            "block",
            "check command",
            "pre-push",
            "guard",
        ],
        command_paths: &["guard"],
        guide: &[
            "suv guard reads the command line you give it and exits 0, or 2 with the reason \
             when the risk reaches --block-at (high unless you say otherwise). It never runs \
             the command.",
            "A verdict is a rule match, not a guarantee: a command that passes can still do \
             damage.",
            "To run a command and record it, use suv wrap; to see what already ran, suv replay.",
        ],
        examples: &[
            ex("suv guard \"rm -rf ./build\"", "Assess one command"),
            ex(
                "suv guard --block-at critical \"<COMMAND>\"",
                "Block only critical risk",
            ),
            ex(
                "suv guard --verbose \"<COMMAND>\"",
                "Explain even when it passes",
            ),
        ],
        ..BASE
    },
    // ── Connect tools ───────────────────────────────────────────
    Feature {
        id: FeatureId("shell-setup"),
        category: CONNECT,
        title: "Shell integration",
        summary: "Record commands in Zsh or Bash",
        description: "Record every command in Zsh or Bash, with Ctrl+R and Up/Down recall.",
        command: "suv init zsh",
        synonyms: &[
            "setup",
            "set up",
            "install",
            "zsh",
            "bash",
            "hooks",
            "shell",
            "ctrl+r",
            "zshrc",
            "bashrc",
            "not recording",
        ],
        command_paths: &["init"],
        guide: &[
            "Add the line for your shell to its startup file, then open a new terminal. \
             suv init only prints shell code: the eval line in your startup file is what \
             sets recording up, and it cannot be done from Home.",
            "Check the new terminal with suv status.",
        ],
        examples: &[
            ex("eval \"$(suv init zsh)\"", "Zsh: add this line to ~/.zshrc"),
            ex(
                "eval \"$(suv init bash)\"",
                "Bash: add this line to ~/.bashrc",
            ),
            ex("suv status", "Then, in a new terminal, check it"),
        ],
        ..BASE
    },
    Feature {
        id: FeatureId("ai-integrations"),
        category: CONNECT,
        title: "AI tool integrations",
        summary: "Record what AI tools run",
        description:
            "Record commands from Claude Code, Codex, Cursor, Antigravity, OpenCode and pi.",
        command: "suv init claude-code",
        synonyms: &[
            "claude",
            "claude code",
            "codex",
            "cursor",
            "antigravity",
            "opencode",
            "pi.dev",
            "mcp",
            "integration",
            "ai setup",
            "agent setup",
        ],
        command_paths: &["init"],
        guide: &[
            "Run suv init with your tool's name once. It installs that tool's hooks or \
             plugin so the commands it runs are recorded, and its prompts too where the \
             tool allows.",
            "Commands agents run stay out of your own recall unless you ask for them \
             (Ctrl+A in search).",
        ],
        examples: &[
            ex("suv init claude-code", "Claude Code"),
            ex("suv init codex", "Codex"),
            ex("suv init cursor", "Cursor"),
            ex("suv init antigravity", "Antigravity"),
            ex("suv init opencode", "OpenCode"),
            ex("suv init pi", "pi.dev"),
        ],
        ..BASE
    },
    Feature {
        id: FeatureId("skills"),
        category: CONNECT,
        title: "Shared skills",
        summary: "Instructions every agent can read",
        description:
            "Keep instructions for AI agents in one library every connected agent can read.",
        opens: "Opens the skills library, which can add, edit, delete and sync skills. Esc \
                returns here.",
        command: "suv skills",
        synonyms: &[
            "skills",
            "skill",
            "instructions",
            "agent instructions",
            "library",
            "playbook",
        ],
        command_paths: &["skills"],
        args: &["skills"],
        action: Action::Open(LaunchMode::Interactive),
        open_label: "Open skills",
        ..BASE
    },
    Feature {
        id: FeatureId("skills-cli"),
        category: CONNECT,
        title: "Skill commands",
        summary: "Manage skills from the command line",
        description: "Add, edit, sync and clean up skills from the command line.",
        command: "suv skills list",
        synonyms: &[
            "add skill",
            "edit skill",
            "sync skills",
            "generated files",
            "claude.md",
            "agents.md",
        ],
        command_paths: &[
            "skills add",
            "skills list",
            "skills show",
            "skills edit",
            "skills rm",
            "skills disable",
            "skills enable",
            "skills sync",
            "skills cleanup",
        ],
        guide: &[
            "The library lives in Suvadu. suv skills sync writes active skills into each \
             agent's own files (Claude Code, Cursor, Codex), inside sections Suvadu manages; \
             suv skills cleanup removes those sections for skills that are gone or disabled.",
            "rm and disable change the library only: files already generated stay until the \
             next cleanup.",
        ],
        examples: &[
            ex("suv skills list", "What is in the library"),
            ex("suv skills show <NAME>", "One skill in full"),
            ex(
                "suv skills add <NAME> --description \"<SUMMARY>\" --body \"<TEXT>\"",
                "Add a skill",
            ),
            ex("suv skills sync --dry-run", "Preview what sync would write"),
            ex(
                "suv skills cleanup --dry-run",
                "Preview what cleanup would remove",
            ),
        ],
        ..BASE
    },
    Feature {
        id: FeatureId("wrap"),
        category: CONNECT,
        title: "Record a wrapped command",
        summary: "Record commands from scripts and CI",
        description:
            "Run a command through Suvadu so it is recorded where no shell hook is loaded.",
        command: "suv wrap -- <COMMAND>",
        synonyms: &[
            "wrap",
            "scripts",
            "script",
            "ci",
            "without hooks",
            "automation",
        ],
        command_paths: &["wrap"],
        guide: &[
            "suv wrap runs the command you give it and records it — for scripts, CI and \
             agents that do not load shell hooks. It executes that command; Home only shows \
             how.",
            "It is the one of the three that runs anything: suv guard only assesses a command \
             line, and suv replay only prints what was recorded.",
        ],
        examples: &[
            ex("suv wrap -- <COMMAND>", "Run and record a command"),
            ex(
                "suv wrap --executor-type ci --executor <NAME> -- <COMMAND>",
                "Record it as CI",
            ),
        ],
        ..BASE
    },
    Feature {
        id: FeatureId("completions"),
        category: CONNECT,
        title: "Shell completions",
        summary: "Tab completion for suv",
        description: "Let Tab complete suv commands and options.",
        command: "suv completions zsh",
        synonyms: &["completion", "completions", "tab", "autocomplete", "fish"],
        command_paths: &["completions"],
        guide: &[
            "Completions only help you type suv commands; they do not record history. \
                  Recording needs Shell integration, for Zsh or Bash.",
        ],
        examples: &[
            ex("suv completions zsh > ~/.zsh/completions/_suv", "Zsh"),
            ex(
                "suv completions bash > ~/.local/share/bash-completion/completions/suv",
                "Bash",
            ),
            ex(
                "suv completions fish > ~/.config/fish/completions/suv.fish",
                "Fish",
            ),
        ],
        ..BASE
    },
    // ── Manage Suvadu ───────────────────────────────────────────
    Feature {
        id: FeatureId("settings"),
        category: MANAGE,
        title: "Settings",
        summary: "Search, recording, theme and more",
        description: "Change search defaults, recording, privacy, theme and the startup screen.",
        opens: "Opens settings. Changes are saved only when you save there (Ctrl+S); Esc \
                returns here.",
        command: "suv settings",
        synonyms: &[
            "settings",
            "preferences",
            "configure",
            "config",
            "theme",
            "privacy",
            "startup",
            "redaction",
            "exclusions",
        ],
        command_paths: &["settings"],
        args: &["settings"],
        action: Action::Open(LaunchMode::Interactive),
        open_label: "Open settings",
        ..BASE
    },
    Feature {
        id: FeatureId("status"),
        category: MANAGE,
        title: "Recording status",
        summary: "Is this shell recording?",
        description: "Check whether recording is on, and what shows it is working.",
        opens: "Shows the status report here: the configuration, this shell's pause, and \
                evidence of recent capture.",
        command: "suv status",
        synonyms: &[
            "status",
            "recording",
            "is it working",
            "am i recording",
            "check",
            "capture",
        ],
        command_paths: &["status"],
        args: &["status"],
        action: Action::Open(LaunchMode::Report),
        ..BASE
    },
    Feature {
        id: FeatureId("doctor"),
        category: MANAGE,
        title: "Diagnose setup",
        summary: "Find and fix setup problems",
        description: "Find what is wrong with an installation, and the fix for each problem.",
        opens: "Shows the diagnosis here. It reports and suggests fixes; it does not repair \
                anything.",
        command: "suv doctor",
        synonyms: &[
            "diagnose",
            "doctor",
            "broken",
            "not working",
            "problem",
            "troubleshoot",
            "fix",
            "health",
        ],
        command_paths: &["doctor"],
        args: &["doctor"],
        action: Action::Open(LaunchMode::Report),
        ..BASE
    },
    Feature {
        id: FeatureId("recording"),
        category: MANAGE,
        title: "Turn recording on or off",
        summary: "Stop or resume recording everywhere",
        description: "Stop or resume recording in every shell, until you change it back.",
        command: "suv disable",
        synonyms: &[
            "stop recording",
            "start recording",
            "privacy",
            "disable",
            "enable",
            "turn off",
            "turn on",
            "off the record",
        ],
        command_paths: &["enable", "disable"],
        guide: &[
            "suv disable and suv enable change your config, so they apply to every shell, \
             now and after restarts. Nothing already recorded is deleted.",
            "To stop only one shell for a while, see Pause this shell.",
        ],
        examples: &[
            ex("suv disable", "Stop recording everywhere"),
            ex("suv enable", "Resume recording"),
            ex("suv status", "Check"),
        ],
        ..BASE
    },
    Feature {
        id: FeatureId("pause"),
        category: MANAGE,
        title: "Pause this shell",
        summary: "Stop recording in one shell",
        description: "Stop recording in one shell only, while it stays paused.",
        command: "eval \"$(suv pause)\"",
        synonyms: &[
            "pause",
            "stop recording",
            "privacy",
            "incognito",
            "temporarily",
            "private",
        ],
        command_paths: &["pause"],
        guide: &[
            "Run this in the shell you want to pause. suv pause only prints a setting, which \
             eval applies to that shell; run any other way — or from Home — it pauses \
             nothing.",
            "Resume with unset SUVADU_PAUSED, or close that shell.",
        ],
        examples: &[
            ex("eval \"$(suv pause)\"", "Run in your shell: pause it"),
            ex("unset SUVADU_PAUSED", "Run in your shell: resume"),
        ],
        ..BASE
    },
    Feature {
        id: FeatureId("backup"),
        category: MANAGE,
        title: "Back up history",
        summary: "A safe copy of your history",
        description: "Save a consistent copy of your history database.",
        command: "suv backup",
        synonyms: &[
            "backup",
            "back up",
            "protect history",
            "copy database",
            "snapshot",
        ],
        command_paths: &["backup"],
        guide: &[
            "suv backup writes a snapshot of the database — to a timestamped file in \
             Suvadu's backups directory, or wherever --out says.",
            "There is no restore command. To go back to a backup, first close everything \
             that uses Suvadu — other suv commands, and any AI tool running its MCP server — \
             then copy the backup over history.db and delete history.db-wal and \
             history.db-shm beside it, so no leftover journal is applied to the restored \
             file.",
        ],
        examples: &[
            ex("suv backup", "Timestamped file in the backups directory"),
            ex("suv backup --out <PATH>", "A file you choose"),
        ],
        ..BASE
    },
    Feature {
        id: FeatureId("export"),
        category: MANAGE,
        title: "Export history",
        summary: "Save history as JSON or CSV",
        description: "Write all your history to a JSON, JSON Lines or CSV file.",
        command: "suv export",
        synonyms: &["export", "json", "csv", "jsonl", "spreadsheet", "archive"],
        command_paths: &["export"],
        guide: &[
            "suv export prints to standard output, so redirect it to a file. JSON Lines \
                  can be imported again with suv import.",
        ],
        examples: &[
            ex("suv export > history.jsonl", "JSON Lines"),
            ex("suv export --format csv > history.csv", "CSV"),
            ex(
                "suv export --after <YYYY-MM-DD> > recent.jsonl",
                "Only newer entries",
            ),
        ],
        ..BASE
    },
    Feature {
        id: FeatureId("import"),
        category: MANAGE,
        title: "Import history",
        summary: "Bring history from Zsh, Bash or Atuin",
        description: "Bring in history from Zsh, Bash, an Atuin database or a Suvadu export.",
        command: "suv import <FILE>",
        synonyms: &[
            "import",
            "atuin",
            "migrate history",
            "zsh history",
            "bash history",
            "bring history",
            "switch",
        ],
        command_paths: &["import"],
        guide: &[
            "Preview first with --dry-run: it reads and counts, and writes nothing.",
            "Zsh and Bash files hold little besides the command, so directory, exit code and \
             duration arrive as unknown. An Atuin database is only read, and Suvadu backs up \
             its own database before importing from it.",
        ],
        examples: &[
            ex(
                "suv import --from zsh-history --dry-run ~/.zsh_history",
                "Preview Zsh history",
            ),
            ex("suv import --from zsh-history ~/.zsh_history", "Import it"),
            ex(
                "suv import --from bash-history ~/.bash_history",
                "Bash history",
            ),
            ex(
                "suv import --from atuin-db --dry-run ~/.local/share/atuin/history.db",
                "Preview Atuin",
            ),
            ex("suv import <FILE>", "A JSON Lines file from suv export"),
        ],
        ..BASE
    },
    Feature {
        id: FeatureId("delete"),
        category: MANAGE,
        title: "Delete history",
        summary: "Remove commands for good",
        description: "Remove commands that contain some text, or match a pattern, for good.",
        command: "suv delete \"<TEXT>\" --dry-run",
        synonyms: &[
            "delete", "remove", "forget", "erase", "secret", "password", "purge", "leaked",
        ],
        command_paths: &["delete"],
        guide: &[
            "Preview with --dry-run first: it lists what matches and deletes nothing.",
            "Without it, suv delete asks before deleting and backs up the database first. \
             --yes skips the question and --no-backup skips the backup; neither is needed.",
        ],
        examples: &[
            ex("suv delete \"<TEXT>\" --dry-run", "Preview"),
            ex("suv delete \"<TEXT>\"", "Delete, after a question"),
            ex(
                "suv delete \"<REGEX>\" --regex --dry-run",
                "Preview a regular expression",
            ),
            ex(
                "suv delete \"\" --before <YYYY-MM-DD> --dry-run",
                "Preview everything older than a date",
            ),
        ],
        ..BASE
    },
    Feature {
        id: FeatureId("gc"),
        category: MANAGE,
        title: "Clean up the database",
        summary: "Clean up and shrink the database",
        description: "Remove leftover data no command refers to, and optionally shrink the file.",
        command: "suv gc",
        synonyms: &[
            "gc",
            "vacuum",
            "compact",
            "clean up",
            "orphaned",
            "disk space",
            "shrink",
        ],
        command_paths: &["gc"],
        guide: &[
            "--dry-run shows what would go. Without it, orphaned sessions and notes are \
                  removed; --vacuum also compacts the database file.",
        ],
        examples: &[
            ex("suv gc --dry-run", "Preview"),
            ex("suv gc", "Remove orphaned data"),
            ex("suv gc --vacuum", "Also compact the file"),
        ],
        ..BASE
    },
    Feature {
        id: FeatureId("version"),
        category: MANAGE,
        title: "Version",
        summary: "Which build is installed",
        description: "Show the version and build of this suv.",
        opens: "Shows the version and build here.",
        command: "suv version",
        synonyms: &["version", "build", "which version", "release"],
        command_paths: &["version"],
        args: &["version"],
        action: Action::Open(LaunchMode::Report),
        ..BASE
    },
    Feature {
        id: FeatureId("update"),
        category: MANAGE,
        title: "Update Suvadu",
        summary: "Get the latest release",
        description: "Install the latest release.",
        command: "suv update",
        synonyms: &[
            "update",
            "upgrade",
            "latest",
            "new version",
            "update check",
            "update available",
        ],
        command_paths: &["update"],
        guide: &[
            "Run suv update from your shell: Home never replaces the program it is \
             running. Homebrew and Cargo installs are updated by those tools, and \
             suv update says which command to use.",
            "Suvadu looks for a newer release once a day, in the background, and mentions \
             it before a command you run at a terminal, and here in Home. The request only \
             reads the latest version number. Turn it off with Check for Updates in suv \
             settings, [update] check = false in config.toml, or SUVADU_NO_UPDATE_CHECK=1.",
        ],
        examples: &[
            ex("suv update", "Direct installs"),
            ex("brew upgrade suvadu", "Homebrew"),
            ex("cargo install suvadu", "Cargo"),
        ],
        ..BASE
    },
    Feature {
        id: FeatureId("uninstall"),
        category: MANAGE,
        title: "Uninstall Suvadu",
        summary: "Remove Suvadu",
        description: "Remove Suvadu from this computer.",
        command: "suv uninstall",
        synonyms: &["uninstall", "remove suvadu"],
        command_paths: &["uninstall"],
        guide: &[
            "Run suv uninstall from your shell. It lists what it will remove and asks \
                  before removing anything. Your history and config are kept.",
        ],
        examples: &[ex("suv uninstall", "Remove Suvadu, after a question")],
        ..BASE
    },
    Feature {
        id: FeatureId("home-startup"),
        category: MANAGE,
        title: "Home and the startup screen",
        summary: "What a bare suv opens",
        description:
            "Choose whether a bare suv opens this Home screen or prints the command overview.",
        command: "suv home",
        synonyms: &[
            "home",
            "startup",
            "start screen",
            "classic help",
            "bare suv",
            "menu",
        ],
        command_paths: &["home"],
        guide: &[
            "A bare suv at a terminal opens Home, and so does suv home. To have a bare suv \
             print the command overview instead, set Startup Screen to help in suv \
             settings, or put this in your config.toml:",
            "[home]\nstartup = \"help\"   # or \"home\", the default",
            "suv home opens Home whatever you choose, suv --help always prints the \
             overview, and scripts and pipes always get the overview.",
        ],
        examples: &[
            ex("suv home", "Open Home"),
            ex("suv --help", "The command overview"),
        ],
        ..BASE
    },
    // ── Command reference (listed after the categories) ─────────
    Feature {
        id: REFERENCE,
        category: REFERENCE_CATEGORY,
        title: "Command reference",
        summary: "Every command and option",
        description: "Every command and option, as suv --help and suv <command> --help print them.",
        opens: "Opens the reference: the command overview, then each command's own help.",
        command: "suv --help",
        synonyms: &[
            "help",
            "options",
            "flags",
            "reference",
            "man page",
            "manual",
            "cli",
            "usage",
            "arguments",
        ],
        command_paths: &["help", "man"],
        action: Action::Reference,
        open_label: "Open command reference",
        guide: &["suv man prints the same reference as a man page, to redirect to a file."],
        ..BASE
    },
];

pub fn categories() -> &'static [Category] {
    &CATEGORIES
}

pub fn features() -> &'static [Feature] {
    FEATURES
}

pub fn feature(id: FeatureId) -> Option<&'static Feature> {
    features().iter().find(|f| f.id == id)
}

pub fn category(id: CategoryId) -> Option<&'static Category> {
    CATEGORIES.iter().find(|c| c.id == id)
}

/// A category's features, in catalog order.
pub fn category_features(id: CategoryId) -> Vec<&'static Feature> {
    features().iter().filter(|f| f.category == id).collect()
}

/// The fixed command line for an executable feature; `None` for guides and
/// the reference, which are never run.
pub fn launch_request(id: FeatureId) -> Option<LaunchRequest> {
    let feature = feature(id)?;
    let Action::Open(mode) = feature.action else {
        return None;
    };
    Some(LaunchRequest {
        feature: id,
        args: feature.args.iter().map(OsString::from).collect(),
        mode,
    })
}

pub fn search_features(query: &str) -> Vec<FeatureId> {
    search_in(features(), query)
}

/// Feature search over the catalog — never over history. Every word of the
/// query must appear in the title, command, a command path, a search word or
/// the description (case folded, Unicode-aware). An exact title or command
/// ranks first, then a title or command that starts with the query, then any
/// other match; ties keep catalog order.
pub fn search_in(features: &[Feature], query: &str) -> Vec<FeatureId> {
    let query = query.to_lowercase();
    let terms: Vec<&str> = query.split_whitespace().collect();
    if terms.is_empty() {
        return Vec::new();
    }
    let phrase = terms.join(" ");
    let mut hits: Vec<(u8, usize, FeatureId)> = Vec::new();
    for (index, feature) in features.iter().enumerate() {
        let title = feature.title.to_lowercase();
        let command = feature.command.to_lowercase();
        let paths: Vec<String> = feature
            .command_paths
            .iter()
            .map(|p| p.to_lowercase())
            .collect();
        let others = feature
            .synonyms
            .iter()
            .map(|s| s.to_lowercase())
            .chain(std::iter::once(feature.description.to_lowercase()));
        let fields: Vec<String> = [title.clone(), command.clone()]
            .into_iter()
            .chain(paths.iter().cloned())
            .chain(others)
            .collect();
        if !terms
            .iter()
            .all(|term| fields.iter().any(|f| f.contains(term)))
        {
            continue;
        }
        let names = || {
            std::iter::once(&title)
                .chain(&paths)
                .chain(std::iter::once(&command))
        };
        let tier = if names().any(|n| *n == phrase) {
            0
        } else if names().any(|n| n.starts_with(&phrase)) {
            1
        } else {
            2
        };
        hits.push((tier, index, feature.id));
    }
    hits.sort_by_key(|&(tier, index, _)| (tier, index));
    hits.into_iter().map(|(_, _, id)| id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};
    use std::ffi::OsString;

    fn ids(query: &str) -> Vec<&'static str> {
        search_features(query).into_iter().map(|id| id.0).collect()
    }

    #[test]
    fn backup_is_discoverable_but_not_executable() {
        assert!(search_features("protect history").contains(&FeatureId("backup")));
        assert_eq!(launch_request(FeatureId("backup")), None);
    }

    #[test]
    fn failed_search_uses_its_real_flag() {
        let request = launch_request(FeatureId("search-failed")).unwrap();
        assert_eq!(
            request.args,
            vec![OsString::from("search"), OsString::from("--failed")]
        );
        assert_eq!(request.mode, LaunchMode::Selection);
    }

    /// The discovery promises: each query finds its feature among the first
    /// three results.
    #[test]
    fn curated_queries_find_their_feature_in_the_first_three() {
        let cases: &[(&str, &[&str])] = &[
            ("failed", &["search-failed"]),
            ("errors", &["search-failed"]),
            ("folder", &["search-directory"]),
            ("directory", &["search-directory"]),
            ("project", &["search-workspace"]),
            ("workspace", &["search-workspace"]),
            ("save command", &["bookmarks"]),
            ("favorites", &["bookmarks"]),
            ("bookmark", &["bookmarks"]),
            ("shortcuts", &["alias-suggestions", "aliases"]),
            ("long commands", &["alias-suggestions", "aliases"]),
            ("what happened", &["sessions"]),
            ("timeline", &["sessions"]),
            (
                "claude",
                &["ai-integrations", "agent-dashboard", "agent-prompts"],
            ),
            (
                "codex",
                &["ai-integrations", "agent-dashboard", "agent-prompts"],
            ),
            ("prompts", &["agent-prompts"]),
            ("skills", &["skills"]),
            ("backup", &["backup"]),
            ("protect history", &["backup"]),
            ("atuin", &["import"]),
            ("migrate history", &["import"]),
            ("privacy", &["pause", "recording", "settings"]),
            ("stop recording", &["pause", "recording", "settings"]),
            ("broken", &["doctor"]),
            ("diagnose", &["doctor"]),
            ("help", &["reference"]),
            ("options", &["reference"]),
            ("new version", &["update"]),
            ("update check", &["update"]),
        ];
        for (query, wanted) in cases {
            let found = ids(query);
            let top: Vec<_> = found.iter().take(3).collect();
            assert!(
                wanted.iter().any(|w| top.contains(&w)),
                "{query:?}: wanted one of {wanted:?} in the first three, got {found:?}"
            );
        }
    }

    #[test]
    fn every_word_of_the_query_must_match() {
        assert!(ids("failed").contains(&"search-failed"));
        assert!(ids("failed zzzz").is_empty());
        assert!(ids("FAILED  Commands").contains(&"search-failed"));
    }

    #[test]
    fn an_exact_command_or_title_ranks_first() {
        assert_eq!(ids("backup").first(), Some(&"backup"));
        assert_eq!(ids("stats").first(), Some(&"stats"));
        assert_eq!(ids("help").first(), Some(&"reference"));
        assert_eq!(ids("settings").first(), Some(&"settings"));
        assert_eq!(ids("suv doctor").first(), Some(&"doctor"));
    }

    #[test]
    fn equally_good_matches_keep_catalog_order() {
        let order: Vec<&str> = features().iter().map(|f| f.id.0).collect();
        let found = ids("the");
        let mut positions: Vec<usize> = found
            .iter()
            .map(|id| order.iter().position(|o| o == id).unwrap())
            .collect();
        // No title or command is, or starts with, "the", so everything it
        // finds is an all-words match and comes back in catalog order.
        let sorted = {
            let mut p = positions.clone();
            p.sort_unstable();
            p
        };
        assert!(found.len() > 3);
        assert_eq!(positions, sorted);
        positions.dedup();
        assert_eq!(positions.len(), found.len(), "no feature twice");
    }

    #[test]
    fn unmatched_or_odd_queries_return_nothing_without_panicking() {
        assert!(ids("xyzzy").is_empty());
        for query in [
            "",
            " ",
            "[](){}*+?.\\^$|",
            "--",
            "\u{301}",
            "'\"",
            "\u{0}",
            "\u{202e}",
            "தமிழ்",
            "日本語",
        ] {
            let _ = search_features(query);
        }
        assert!(ids("   ").is_empty(), "a blank query is not a search");
    }

    /// Every public command — at any depth — is reachable from some feature,
    /// so a new command cannot ship without a place in Home.
    #[test]
    fn every_public_command_belongs_to_a_feature() {
        fn walk(cmd: &clap::Command, prefix: &str, out: &mut Vec<String>) {
            for sub in cmd.get_subcommands().filter(|s| !s.is_hide_set()) {
                let path = if prefix.is_empty() {
                    sub.get_name().to_string()
                } else {
                    format!("{prefix} {}", sub.get_name())
                };
                walk(sub, &path, out);
                out.push(path);
            }
        }
        let mut public = vec!["help".to_string()];
        walk(&crate::cli::Cli::command(), "", &mut public);
        let covered: Vec<&str> = features()
            .iter()
            .flat_map(|f| f.command_paths.iter().copied())
            .collect();
        let missing: Vec<&String> = public
            .iter()
            .filter(|p| !covered.contains(&p.as_str()))
            .collect();
        assert!(missing.is_empty(), "no feature covers {missing:?}");
    }

    #[test]
    fn every_listed_command_path_exists_and_is_public() {
        let root = crate::cli::Cli::command();
        for feature in features() {
            for path in feature.command_paths {
                if *path == "help" {
                    continue;
                }
                let mut cmd = &root;
                for part in path.split(' ') {
                    cmd = cmd
                        .find_subcommand(part)
                        .unwrap_or_else(|| panic!("{}: no command {path:?}", feature.id.0));
                    assert!(!cmd.is_hide_set(), "{path:?} is hidden");
                }
            }
        }
    }

    /// `bookmark`, `alias` and `session` are clap aliases: searching for one
    /// finds its canonical feature, once.
    #[test]
    fn command_aliases_find_their_canonical_feature_once() {
        let root = crate::cli::Cli::command();
        for sub in root.get_subcommands().filter(|s| !s.is_hide_set()) {
            for alias in sub.get_all_aliases() {
                let found = ids(alias);
                let owner = features()
                    .iter()
                    .find(|f| f.command_paths.first() == Some(&sub.get_name()))
                    .unwrap_or_else(|| panic!("no feature owns {}", sub.get_name()));
                assert_eq!(found.first(), Some(&owner.id.0), "{alias}: {found:?}");
                assert_eq!(
                    found.iter().filter(|id| **id == owner.id.0).count(),
                    1,
                    "{alias}"
                );
            }
        }
    }

    #[test]
    fn every_launch_is_a_real_command_line() {
        for feature in features() {
            let Some(request) = launch_request(feature.id) else {
                continue;
            };
            let mut argv = vec![OsString::from("suv")];
            argv.extend(request.args.iter().cloned());
            let parsed = crate::cli::Cli::try_parse_from(&argv);
            assert!(parsed.is_ok(), "{}: {argv:?} does not parse", feature.id.0);
            assert_eq!(
                feature.command,
                format!("suv {}", feature.args.join(" ")),
                "{}: the command shown is not the command run",
                feature.id.0
            );
        }
    }

    #[test]
    fn hidden_and_protocol_commands_are_never_offered() {
        let hidden = ["add", "get", "mcp-serve", "hook-"];
        for feature in features() {
            for path in feature.command_paths.iter().chain(feature.args.first()) {
                assert!(
                    !hidden
                        .iter()
                        .any(|h| *path == *h || (h.ends_with('-') && path.starts_with(h))),
                    "{} offers {path}",
                    feature.id.0
                );
            }
        }
    }

    /// Anything that changes data, replaces the binary or runs a command is
    /// explained, never run, from Home.
    #[test]
    fn changes_are_guides_never_launches() {
        for id in [
            "import",
            "delete",
            "gc",
            "update",
            "uninstall",
            "wrap",
            "recording",
            "pause",
            "backup",
            "export",
            "notes",
            "agent-import",
            "agent-delete",
            "alias-apply",
            "skills-cli",
            "shell-setup",
            "ai-integrations",
            "completions",
            "guard",
            "replay",
            "home-startup",
        ] {
            let feature = feature(FeatureId(id)).unwrap_or_else(|| panic!("no feature {id}"));
            assert_eq!(feature.action, Action::Guide, "{id}");
            assert_eq!(launch_request(feature.id), None, "{id}");
        }
    }

    #[test]
    fn launches_are_the_approved_screens_and_reports() {
        let approved: &[(&str, LaunchMode)] = &[
            ("search", LaunchMode::Selection),
            ("search-directory", LaunchMode::Selection),
            ("search-workspace", LaunchMode::Selection),
            ("search-failed", LaunchMode::Selection),
            ("bookmarks", LaunchMode::Selection),
            ("history", LaunchMode::Report),
            ("tags", LaunchMode::Report),
            ("agent-report", LaunchMode::Report),
            ("status", LaunchMode::Report),
            ("doctor", LaunchMode::Report),
            ("version", LaunchMode::Report),
            ("sessions", LaunchMode::Interactive),
            ("aliases", LaunchMode::Interactive),
            ("alias-suggestions", LaunchMode::Interactive),
            ("stats", LaunchMode::Interactive),
            ("agent-dashboard", LaunchMode::Interactive),
            ("agent-prompts", LaunchMode::Interactive),
            ("agent-stats", LaunchMode::Interactive),
            ("skills", LaunchMode::Interactive),
            ("settings", LaunchMode::Interactive),
        ];
        let mut launchable: Vec<(&str, LaunchMode)> = features()
            .iter()
            .filter_map(|f| launch_request(f.id).map(|r| (f.id.0, r.mode)))
            .collect();
        let mut approved = approved.to_vec();
        launchable.sort_by_key(|(id, _)| *id);
        approved.sort_by_key(|(id, _)| *id);
        assert_eq!(launchable, approved);
    }

    #[test]
    fn action_labels_say_what_enter_does() {
        let label = |id| feature(FeatureId(id)).unwrap().action_label();
        assert_eq!(label("backup"), "Read instructions");
        assert_eq!(label("search"), "Open search");
        assert_eq!(label("bookmarks"), "Open bookmarks");
        assert_eq!(label("status"), "View report");
        assert_eq!(label("settings"), "Open settings");
        assert_eq!(label("reference"), "Open command reference");
    }

    #[test]
    fn categories_are_stable_and_start_where_the_plan_says() {
        let first: Vec<(&str, &str)> = categories()
            .iter()
            .map(|c| (c.title, category_features(c.id).first().unwrap().id.0))
            .collect();
        assert_eq!(
            first,
            [
                ("Find a command", "search"),
                ("Review a session", "sessions"),
                ("Organize commands", "bookmarks"),
                ("Understand my activity", "stats"),
                ("Review AI activity", "agent-dashboard"),
                ("Connect tools", "shell-setup"),
                ("Manage Suvadu", "settings"),
            ]
        );
    }

    #[test]
    fn feature_ids_are_unique_and_every_feature_has_a_home() {
        let mut seen = std::collections::HashSet::new();
        for feature in features() {
            assert!(seen.insert(feature.id), "duplicate {}", feature.id.0);
            assert!(!feature.title.is_empty() && !feature.description.is_empty());
            if feature.id != REFERENCE {
                assert!(
                    categories().iter().any(|c| c.id == feature.category),
                    "{} has no category",
                    feature.id.0
                );
            }
        }
    }

    /// Split a command line the way a shell would for the simple quoting
    /// the examples use.
    fn shell_words(line: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut word = String::new();
        let mut quote: Option<char> = None;
        let mut started = false;
        for c in line.chars() {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (None, '"' | '\'') => {
                    quote = Some(c);
                    started = true;
                }
                (None, ' ') => {
                    if started || !word.is_empty() {
                        words.push(std::mem::take(&mut word));
                    }
                    started = false;
                }
                (_, c) => word.push(c),
            }
        }
        if started || !word.is_empty() {
            words.push(word);
        }
        words
    }

    /// The `suv …` invocation inside an example: after `$(`, before a pipe,
    /// a redirection or the closing parenthesis. `None` for other tools.
    fn suv_invocation(example: &str) -> Option<String> {
        let start = example.find("suv ")?;
        let rest = &example[start..];
        let end = rest.find(['|', '>', ')']).unwrap_or(rest.len());
        Some(rest[..end].trim().to_string())
    }

    /// Stand-in values for the parts a reader fills in.
    fn fill(line: &str) -> String {
        let values = [
            ("<ENTRY_ID>", "42"),
            ("<SESSION_ID>", "abc123"),
            ("<YYYY-MM-DD>", "2025-01-01"),
            ("<PATH>", "/tmp/example"),
            ("<FILE>", "history.jsonl"),
            ("<COMMAND>", "git status"),
            ("<REGEX>", "^git"),
        ];
        let mut out = line.to_string();
        for (placeholder, value) in values {
            out = out.replace(placeholder, value);
        }
        // Any other <NAME>-style value is a single word.
        while let (Some(open), Some(close)) = (out.find('<'), out.find('>')) {
            if close < open {
                break;
            }
            out.replace_range(open..=close, "value");
        }
        out
    }

    /// Every suv command a feature shows — its command and each example,
    /// with placeholders filled — is one the CLI accepts. Nothing is run.
    #[test]
    fn every_command_shown_is_one_the_cli_accepts() {
        let mut checked = 0;
        for feature in features() {
            let shown =
                std::iter::once(feature.command).chain(feature.examples.iter().map(|e| e.command));
            for line in shown {
                let Some(invocation) = suv_invocation(&fill(line)) else {
                    continue;
                };
                let words = shell_words(&invocation);
                // Asking for help or the version is a valid command line too.
                let parsed =
                    crate::cli::Cli::try_parse_from(&words)
                        .map(|_| ())
                        .or_else(|e| match e.kind() {
                            clap::error::ErrorKind::DisplayHelp
                            | clap::error::ErrorKind::DisplayVersion => Ok(()),
                            _ => Err(e),
                        });
                assert!(
                    parsed.is_ok(),
                    "{}: {line:?} does not parse: {}",
                    feature.id.0,
                    parsed.err().map(|e| e.to_string()).unwrap_or_default()
                );
                checked += 1;
            }
        }
        assert!(checked > 60, "only {checked} commands checked");
    }

    #[test]
    fn placeholders_are_told_apart_from_redirections() {
        assert!(has_placeholder("suv note <ENTRY_ID> -c \"<TEXT>\""));
        assert!(has_placeholder(
            "suv export --after <YYYY-MM-DD> > recent.jsonl"
        ));
        assert!(!has_placeholder("suv export > history.jsonl"));
        assert!(!has_placeholder("cat < file.md"));
        assert!(!has_placeholder("echo <lower>"));
        // An example needs input exactly when there is something to fill.
        for feature in features() {
            for example in feature.examples {
                assert_eq!(
                    example.needs_input(),
                    fill(example.command) != example.command,
                    "{}: {}",
                    feature.id.0,
                    example.command
                );
            }
        }
    }

    /// Wording review, enforced: nothing claims an outcome Home cannot know.
    #[test]
    fn no_feature_text_claims_what_home_cannot_know() {
        for feature in features() {
            let text = [feature.description, feature.opens]
                .into_iter()
                .chain(feature.guide.iter().copied())
                .chain(feature.note)
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
            for claim in [
                "is installed",
                "are installed",
                "is now paused",
                "has been paused",
                "is safe",
                "guaranteed",
                "was executed",
                "has been run",
                "verified",
            ] {
                assert!(!text.contains(claim), "{}: {claim:?}", feature.id.0);
            }
        }
    }

    #[test]
    fn wrap_guard_and_replay_say_which_one_runs_commands() {
        let guide = |id| feature(FeatureId(id)).unwrap().guide.join(" ");
        assert!(guide("wrap").contains("runs the command"));
        assert!(guide("guard").contains("never runs"));
        assert!(guide("replay").contains("never runs"));
        for id in ["wrap", "guard", "replay"] {
            let others: Vec<&str> = ["wrap", "guard", "replay"]
                .into_iter()
                .filter(|o| *o != id)
                .collect();
            let text = guide(id);
            assert!(
                others.iter().all(|o| text.contains(&format!("suv {o}"))),
                "{id} should point to {others:?}: {text}"
            );
        }
    }

    /// A screen that can change data says so before it is opened.
    #[test]
    fn screens_that_can_change_data_say_so() {
        let cases: &[(&str, &[&str])] = &[
            ("bookmarks", &["add", "edit", "delete"]),
            ("search", &["delete", "bookmark", "note", "tag"]),
            ("search-directory", &["delete", "bookmark", "note", "tag"]),
            ("search-workspace", &["delete", "bookmark", "note", "tag"]),
            ("search-failed", &["delete", "bookmark", "note", "tag"]),
            ("aliases", &["add", "change", "delete"]),
            ("skills", &["add", "edit", "delete"]),
            ("settings", &["saved"]),
        ];
        for (id, words) in cases {
            let opens = feature(FeatureId(id)).unwrap().opens.to_lowercase();
            for word in *words {
                assert!(
                    opens.contains(word),
                    "{id} should mention {word:?}: {opens}"
                );
            }
            assert!(!opens.contains("from your shell"), "{id}: {opens}");
        }
    }

    /// Restoring a backup under a live or crashed database would mix it with
    /// the old write-ahead log; the guide says how to avoid that.
    #[test]
    fn the_backup_guide_restores_safely() {
        let guide = feature(FeatureId("backup")).unwrap().guide.join(" ");
        assert!(
            guide.contains("history.db-wal") && guide.contains("history.db-shm"),
            "{guide}"
        );
        assert!(guide.contains("MCP"), "{guide}");
    }

    #[test]
    fn the_update_guide_explains_the_notice_and_how_to_turn_it_off() {
        let guide = feature(FeatureId("update")).unwrap().guide.join(" ");
        assert!(guide.contains("once a day"), "{guide}");
        assert!(guide.contains("Check for Updates"), "{guide}");
        assert!(guide.contains("SUVADU_NO_UPDATE_CHECK"), "{guide}");
    }

    /// Each feature has a short summary for category previews: what it is
    /// for, in a few words.
    #[test]
    fn every_feature_has_a_short_summary() {
        for feature in features() {
            let summary = feature.summary;
            assert!(!summary.is_empty(), "{}", feature.id.0);
            assert!(summary.chars().count() <= 44, "{}: {summary}", feature.id.0);
            assert!(!summary.ends_with('.'), "{}: {summary}", feature.id.0);
        }
    }

    /// Coverage instrumentation slows every line it counts, so the timing
    /// means nothing under `cargo tarpaulin`; ordinary test runs still check it.
    #[test]
    #[cfg_attr(tarpaulin, ignore)]
    fn searching_five_hundred_features_is_fast() {
        let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
        let many: Vec<Feature> = (0..500)
            .map(|i| Feature {
                id: FeatureId(leak(format!("synthetic-{i}"))),
                title: leak(format!("Synthetic feature number {i}")),
                description: leak(format!("A generated description with words {i} and more")),
                synonyms: Box::leak(
                    vec![leak(format!("alpha{i}")), "beta gamma"].into_boxed_slice(),
                ),
                ..features()[0]
            })
            .collect();
        // The fastest of several rounds, so a busy machine pausing the test
        // mid-round is not mistaken for a slow search.
        let per_query = (0..5)
            .map(|_| {
                let started = std::time::Instant::now();
                for query in [
                    "synthetic",
                    "feature 499",
                    "beta gamma",
                    "nothing here",
                    "a",
                ] {
                    let _ = search_in(&many, query);
                }
                started.elapsed() / 5
            })
            .min()
            .unwrap();
        assert!(per_query.as_millis() < 16, "{per_query:?} per query");
    }
}
