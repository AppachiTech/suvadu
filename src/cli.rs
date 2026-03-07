use clap::{CommandFactory, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "suvadu",
    version,
    about = "Total recall for your terminal. A high-performance, database-backed shell history.",
    long_about = None
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Enable history recording globally (persistent)
    Enable,

    /// Disable history recording globally (persistent)
    Disable,

    /// Pause history recording for current shell session
    /// Usage: eval $(suv pause)
    Pause,

    /// Add a command to history
    #[command(hide = true)]
    Add {
        #[arg(long)]
        session_id: String,
        #[arg(long)]
        command: String,
        #[arg(long)]
        cwd: String,
        #[arg(long)]
        exit_code: Option<i32>,
        #[arg(long)]
        started_at: i64,
        #[arg(long)]
        ended_at: i64,
        #[arg(long)]
        executor_type: Option<String>,
        #[arg(long)]
        executor: Option<String>,
    },

    /// Set up shell hooks or AI tool integrations
    #[command(
        after_help = "Targets:\n  zsh          Generate Zsh shell hooks (add to ~/.zshrc)\n  bash         Generate Bash shell hooks (add to ~/.bashrc)\n  claude-code  Set up Claude Code AI command capture\n  cursor       Set up Cursor AI command tracking\n  antigravity  Set up Antigravity IDE command tracking\n\nExamples:\n  eval \"$(suv init zsh)\"        # Add to ~/.zshrc\n  eval \"$(suv init bash)\"       # Add to ~/.bashrc\n  suv init claude-code          # Set up Claude Code capture\n  suv init cursor               # Set up Cursor tracking\n  suv init antigravity           # Set up Antigravity tracking"
    )]
    Init {
        /// Target: 'zsh', 'bash', 'claude-code', 'cursor', or 'antigravity'
        target: String,
    },

    /// Process a Claude Code `PostToolUse` hook event (reads JSON from stdin)
    #[command(name = "hook-claude-code", hide = true)]
    HookClaudeCode,

    /// Process a Claude Code `UserPromptSubmit` hook event (reads JSON from stdin)
    #[command(name = "hook-claude-prompt", hide = true)]
    HookClaudePrompt,

    #[command(hide = true)]
    Get {
        /// Query string
        #[arg(long, default_value = "")]
        query: String,

        /// Offset (0 = most recent)
        #[arg(long, default_value_t = 0)]
        offset: usize,

        /// Match only as prefix
        #[arg(long)]
        prefix: bool,

        /// Current working directory (for context-aware ranking)
        #[arg(long)]
        cwd: Option<String>,
    },

    /// Configure Suvadu (interactive UI)
    Settings,

    /// Interactive search through history (Ctrl+R replacement)
    #[command(
        after_help = "Examples:\n  suv search --query \"git\"\n  suv search --unique\n  suv search --executor bot\n  suv search --after today\n  suv search --query \"/home\" --field cwd"
    )]
    Search {
        /// Optional initial query
        #[arg(short, long)]
        query: Option<String>,

        /// Deduplicate results (only show unique commands)
        #[arg(short, long)]
        unique: bool,

        /// Filter by date after (ISO 8601)
        #[arg(long)]
        after: Option<String>,

        /// Filter by date before (ISO 8601)
        #[arg(long)]
        before: Option<String>,

        /// Filter by tag name
        #[arg(long)]
        tag: Option<String>,

        /// Filter by exit code
        #[arg(long)]
        exit_code: Option<i32>,

        /// Filter by executor (type or name)
        #[arg(long)]
        executor: Option<String>,

        /// Filter to commands run in the current directory
        #[arg(long)]
        here: bool,

        /// Search field: command (default), cwd, session, or executor
        #[arg(long, default_value = "command")]
        field: String,
    },

    /// Show usage analytics and trends
    #[command(
        after_help = "Examples:\n  suv stats\n  suv stats --days 30\n  suv stats --days 7 -n 5\n  suv stats --tag work"
    )]
    Stats {
        /// Number of days to analyze (default: all time)
        #[arg(short, long)]
        days: Option<usize>,
        /// Number of top commands/directories to show
        #[arg(short = 'n', long, default_value_t = 10)]
        top: usize,
        /// Output plain text instead of interactive TUI
        #[arg(long)]
        text: bool,
        /// Output as JSON for scripting
        #[arg(long)]
        json: bool,
        /// Filter by tag name
        #[arg(long)]
        tag: Option<String>,
    },

    /// Replay commands chronologically (session timeline or time range)
    #[command(
        after_help = "Examples:\n  suv replay                          # Current session\n  suv replay --after today             # Today's commands\n  suv replay --after yesterday --here  # Yesterday, this directory\n  suv replay --session <id>            # Specific session"
    )]
    Replay {
        /// Replay a specific session ID
        #[arg(long)]
        session: Option<String>,
        /// Show commands after this date (YYYY-MM-DD, "today", "yesterday")
        #[arg(long)]
        after: Option<String>,
        /// Show commands before this date (YYYY-MM-DD, "today", "yesterday")
        #[arg(long)]
        before: Option<String>,
        /// Filter by tag name
        #[arg(long)]
        tag: Option<String>,
        /// Filter by exit code
        #[arg(long)]
        exit_code: Option<i32>,
        /// Filter by executor
        #[arg(long)]
        executor: Option<String>,
        /// Filter to commands run in the current directory
        #[arg(long)]
        here: bool,
        /// Filter to a specific directory
        #[arg(long)]
        cwd: Option<String>,
    },

    /// Check current recording status
    Status,

    /// Bulk delete commands matching a pattern
    #[command(
        after_help = "Examples:\n  suv delete \"rm -rf\"          # Delete by substring\n  suv delete \"^git\" --regex    # Delete using regex\n  suv delete \"\" --before 2024-01-01 # Delete all before date"
    )]
    Delete {
        /// Pattern to match (substring by default)
        pattern: String,

        /// Treat pattern as a Regular Expression
        #[arg(long)]
        regex: bool,

        /// Dry run (show what would be deleted without deleting)
        #[arg(long)]
        dry_run: bool,

        /// Skip confirmation prompt
        #[arg(long, short)]
        yes: bool,

        /// Delete entries older than this date (YYYY-MM-DD)
        #[arg(long)]
        before: Option<String>,
    },

    /// Manage tags
    #[command(
        subcommand,
        after_help = "Examples:\n  suv tag list\n  suv tag create \"work\"\n  suv tag associate \"project-x\""
    )]
    Tag(TagCommands),

    /// Annotate a history entry with a note
    #[command(
        after_help = "Examples:\n  suv note 42 -c \"Fixed the SSL bug\"\n  suv note 42            # View existing note\n  suv note 42 --delete   # Remove the note"
    )]
    Note {
        /// Entry ID to annotate
        entry_id: i64,
        /// Note content (omit to view existing note)
        #[arg(short, long)]
        content: Option<String>,
        /// Delete the note
        #[arg(long)]
        delete: bool,
    },

    /// Manage bookmarked commands
    #[command(
        subcommand,
        after_help = "Examples:\n  suv bookmark add \"git stash pop\"\n  suv bookmark add \"cargo test\" -l \"run tests\"\n  suv bookmark list\n  suv bookmark remove \"git stash pop\""
    )]
    Bookmark(BookmarkCommands),

    /// Manage shell aliases
    #[command(
        subcommand,
        after_help = "Examples:\n  suv alias add gst \"git status\"\n  suv alias list\n  suv alias apply --stdout\n  suv alias remove gst\n  suv alias add-suggested"
    )]
    Alias(AliasCommands),

    /// Interactive session timeline view
    #[command(
        after_help = "Examples:\n  suv session                    # Pick from recent sessions\n  suv session abc123             # Open session by ID prefix\n  suv session --list             # List sessions without opening\n  suv session --after 2025-01-01 # Sessions after date\n  suv session --tag work         # Sessions with tag"
    )]
    Session {
        /// Session ID or prefix (omit for interactive picker)
        session_id: Option<String>,
        /// List sessions and exit (no TUI)
        #[arg(long)]
        list: bool,
        /// Only show sessions after this date
        #[arg(long)]
        after: Option<String>,
        /// Filter by tag name
        #[arg(long)]
        tag: Option<String>,
        /// Max sessions to show (default: 50)
        #[arg(short = 'n', long, default_value_t = 50)]
        limit: usize,
    },

    /// Uninstall Suvadu (remove binaries from system)
    Uninstall,

    /// Show version and build info
    Version,

    /// Generate shell completions
    #[command(
        after_help = "Examples:\n  suv completions zsh > ~/.zsh/completions/_suv\n  suv completions bash > /etc/bash_completion.d/suv\n  suv completions fish > ~/.config/fish/completions/suv.fish"
    )]
    Completions {
        /// Shell to generate completions for (zsh, bash, fish)
        shell: clap_complete::Shell,
    },

    /// Generate man page to stdout
    #[command(
        after_help = "Example:\n  suv man | sudo tee /usr/local/share/man/man1/suv.1 > /dev/null\n  man suv"
    )]
    Man,

    /// Update to the latest version
    Update,

    /// Export history to a file (JSON, JSONL, or CSV format)
    #[command(
        after_help = "Examples:\n  suv export --format json > history.json\n  suv export > history.jsonl\n  suv export --format csv > history.csv\n  suv export --after 2025-01-01 > recent.jsonl"
    )]
    Export {
        /// Output format: json, jsonl (default), or csv
        #[arg(long, default_value = "jsonl")]
        format: String,
        /// Only export entries after this date (YYYY-MM-DD)
        #[arg(long)]
        after: Option<String>,
        /// Only export entries before this date (YYYY-MM-DD)
        #[arg(long)]
        before: Option<String>,
    },

    /// Import history from a file (JSONL or Zsh history format)
    #[command(
        after_help = "Examples:\n  suv import history.jsonl\n  suv import --from zsh-history ~/.zsh_history\n  suv import --from zsh-history --dry-run ~/.zsh_history"
    )]
    Import {
        /// Path to the file to import
        file: String,
        /// Source format: "jsonl" (default) or "zsh-history"
        #[arg(long, default_value = "jsonl")]
        from: String,
        /// Preview import without writing to database
        #[arg(long)]
        dry_run: bool,
    },

    /// Monitor and audit AI agent command activity
    #[command(
        subcommand,
        after_help = "Examples:\n  suv agent report                        # Today's agent report\n  suv agent report --executor claude-code  # Claude Code only\n  suv agent report --format markdown       # Markdown for PR descriptions"
    )]
    Agent(AgentCommands),

    /// Remove orphaned data and compact the database
    #[command(
        after_help = "Examples:\n  suv gc              # Remove orphaned sessions/notes\n  suv gc --dry-run    # Preview what would be cleaned\n  suv gc --vacuum     # Also compact the database file"
    )]
    Gc {
        /// Preview what would be deleted without deleting
        #[arg(long)]
        dry_run: bool,
        /// Run VACUUM after cleanup to compact the database file
        #[arg(long)]
        vacuum: bool,
    },

    /// Execute a command and record it in Suvadu history
    /// Useful for AI agents and scripts that don't load shell hooks
    #[command(
        after_help = "Examples:\n  suv wrap -- git status\n  suv wrap --executor-type agent --executor claude-code -- npm test"
    )]
    Wrap {
        /// The command to execute
        #[arg(trailing_var_arg = true, required = true)]
        command: Vec<String>,
        /// Executor type (e.g., "agent", "bot", "ci")
        #[arg(long, default_value = "agent")]
        executor_type: String,
        /// Executor name (e.g., "claude-code", "codex")
        #[arg(long, default_value = "unknown")]
        executor: String,
    },
}

/// Generate man page and write to stdout
pub fn generate_man_page() -> Result<(), Box<dyn std::error::Error>> {
    let cmd = Cli::command();
    let man = clap_mangen::Man::new(cmd);
    man.render(&mut std::io::stdout())?;
    Ok(())
}

/// Generate shell completions and write to stdout
pub fn generate_completions(shell: clap_complete::Shell) {
    let mut cmd = Cli::command();
    clap_complete::generate(shell, &mut cmd, "suv", &mut std::io::stdout());
}

#[derive(Subcommand, Debug)]
pub enum TagCommands {
    /// Create a new tag
    Create {
        /// Name of the tag (lowercase)
        name: Option<String>,
        /// Optional description
        #[arg(short, long)]
        description: Option<String>,
    },

    /// List all tags
    List,

    /// Associate a tag with the current session (or specific session)
    Associate {
        /// Name of the tag to associate
        tag_name: String,
        /// Optional session ID (defaults to current)
        #[arg(long)]
        session_id: Option<String>,
    },

    /// Update an existing tag
    Update {
        /// Current name of the tag
        name: String,
        /// New name for the tag (optional)
        #[arg(long)]
        new_name: Option<String>,
        /// New description (optional)
        #[arg(short, long)]
        description: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum BookmarkCommands {
    /// Bookmark a command for quick recall
    Add {
        /// The command text to bookmark
        command: String,
        /// Optional label/description
        #[arg(short, long)]
        label: Option<String>,
    },

    /// List all bookmarked commands
    List {
        /// Output as JSON for scripting
        #[arg(long)]
        json: bool,
    },

    /// Remove a bookmark
    Remove {
        /// The command text to un-bookmark
        command: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum AliasCommands {
    /// Register a shell alias
    Add {
        /// Alias name (alphanumeric, hyphens, underscores)
        name: String,
        /// The command the alias expands to
        command: String,
    },

    /// Remove a managed alias
    Remove {
        /// Alias name to remove
        name: String,
    },

    /// List all managed aliases
    List {
        /// Output as JSON for scripting
        #[arg(long)]
        json: bool,
    },

    /// Write aliases to a sourceable shell file (or stdout)
    Apply {
        /// Print alias lines to stdout instead of writing to file
        #[arg(long)]
        stdout: bool,
    },

    /// Interactively pick from suggestions and store to DB
    #[command(name = "add-suggested")]
    AddSuggested {
        /// Minimum times a command must appear (default: 10)
        #[arg(short = 'c', long, default_value_t = 10)]
        min_count: usize,
        /// Minimum character length of command (default: 12)
        #[arg(short = 'l', long, default_value_t = 12)]
        min_length: usize,
        /// Only analyze last N days
        #[arg(short, long)]
        days: Option<usize>,
        /// Max suggestions to show (default: 20)
        #[arg(short = 'n', long, default_value_t = 20)]
        top: usize,
    },

    /// Suggest aliases for frequently-typed long commands
    #[command(
        after_help = "Examples:\n  suv alias suggest                    # Interactive TUI\n  suv alias suggest --text             # Plain text output\n  suv alias suggest --days 30 -c 5     # Last 30 days, min 5 uses"
    )]
    Suggest {
        /// Minimum times a command must appear (default: 10)
        #[arg(short = 'c', long, default_value_t = 10)]
        min_count: usize,
        /// Minimum character length of command (default: 12)
        #[arg(short = 'l', long, default_value_t = 12)]
        min_length: usize,
        /// Only analyze last N days
        #[arg(short, long)]
        days: Option<usize>,
        /// Max suggestions to show (default: 20)
        #[arg(short = 'n', long, default_value_t = 20)]
        top: usize,
        /// Skip TUI, print suggestions to stdout
        #[arg(long)]
        text: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum AgentCommands {
    /// Generate a risk-assessed activity report for AI agent commands
    #[command(
        after_help = "Examples:\n  suv agent report\n  suv agent report --executor claude-code\n  suv agent report --format markdown\n  suv agent report --after \"3 days ago\" --here"
    )]
    Report {
        /// Start date (default: today)
        #[arg(long, default_value = "today")]
        after: String,
        /// End date
        #[arg(long)]
        before: Option<String>,
        /// Filter to a specific agent (e.g. claude-code, cursor)
        #[arg(long)]
        executor: Option<String>,
        /// Output format: text, markdown, or json
        #[arg(long, default_value = "text")]
        format: String,
        /// Filter to commands run in the current directory
        #[arg(long)]
        here: bool,
    },

    /// Interactive agent activity dashboard
    #[command(
        name = "dashboard",
        after_help = "Examples:\n  suv agent dashboard\n  suv agent dashboard --executor claude-code\n  suv agent dashboard --after yesterday --here"
    )]
    Dashboard {
        /// Start date (default: today)
        #[arg(long, default_value = "today")]
        after: String,
        /// Filter to a specific agent (e.g. claude-code, cursor)
        #[arg(long)]
        executor: Option<String>,
        /// Filter to commands run in the current directory
        #[arg(long)]
        here: bool,
    },

    /// Show agent-specific usage analytics
    #[command(
        after_help = "Examples:\n  suv agent stats\n  suv agent stats --days 30\n  suv agent stats --executor claude-code --text"
    )]
    Stats {
        /// Number of days to analyze (default: 30)
        #[arg(short, long, default_value_t = 30)]
        days: usize,
        /// Filter to a specific agent
        #[arg(long)]
        executor: Option<String>,
        /// Output plain text instead of interactive TUI
        #[arg(long)]
        text: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_cli_parses_enable() {
        let cli = Cli::try_parse_from(["suv", "enable"]).unwrap();
        assert!(matches!(cli.command, Commands::Enable));
    }

    #[test]
    fn test_cli_parses_search_with_args() {
        let cli = Cli::try_parse_from(["suv", "search", "-q", "git", "--unique"]).unwrap();
        match cli.command {
            Commands::Search { query, unique, .. } => {
                assert_eq!(query, Some("git".to_string()));
                assert!(unique);
            }
            _ => panic!("Expected Search command"),
        }
    }

    #[test]
    fn test_cli_parses_agent_report() {
        let cli = Cli::try_parse_from([
            "suv",
            "agent",
            "report",
            "--format",
            "json",
            "--executor",
            "claude-code",
        ])
        .unwrap();
        match cli.command {
            Commands::Agent(AgentCommands::Report {
                format, executor, ..
            }) => {
                assert_eq!(format, "json");
                assert_eq!(executor, Some("claude-code".to_string()));
            }
            _ => panic!("Expected Agent Report command"),
        }
    }

    #[test]
    fn test_cli_parses_stats_defaults() {
        let cli = Cli::try_parse_from(["suv", "stats"]).unwrap();
        match cli.command {
            Commands::Stats {
                days,
                top,
                text,
                json,
                tag,
            } => {
                assert!(days.is_none());
                assert_eq!(top, 10);
                assert!(!text);
                assert!(!json);
                assert!(tag.is_none());
            }
            _ => panic!("Expected Stats command"),
        }
    }

    #[test]
    fn test_cli_parses_stats_with_tag() {
        let cli = Cli::try_parse_from(["suv", "stats", "--tag", "work"]).unwrap();
        match cli.command {
            Commands::Stats { tag, .. } => {
                assert_eq!(tag, Some("work".to_string()));
            }
            _ => panic!("Expected Stats command"),
        }
    }

    #[test]
    fn test_cli_rejects_unknown_command() {
        let result = Cli::try_parse_from(["suv", "nonexistent"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cli_parses_wrap_with_trailing_args() {
        let cli = Cli::try_parse_from([
            "suv",
            "wrap",
            "--executor-type",
            "agent",
            "--executor",
            "claude-code",
            "--",
            "git",
            "status",
        ])
        .unwrap();
        match cli.command {
            Commands::Wrap {
                command,
                executor_type,
                executor,
            } => {
                assert_eq!(command, vec!["git".to_string(), "status".to_string()]);
                assert_eq!(executor_type, "agent");
                assert_eq!(executor, "claude-code");
            }
            _ => panic!("Expected Wrap command"),
        }
    }
}
