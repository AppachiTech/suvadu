use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Represents a single command entry in the history
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: Option<i64>,
    pub session_id: String,
    pub command: String,
    pub cwd: String,
    pub exit_code: Option<i32>,
    pub started_at: i64, // Unix milliseconds
    pub ended_at: i64,   // Unix milliseconds
    pub duration_ms: i64,
    pub context: Option<HashMap<String, String>>,
    pub tag_name: Option<String>,
    pub tag_id: Option<i64>,
    pub executor_type: Option<String>,
    pub executor: Option<String>,
}

impl Entry {
    /// Create a new entry
    pub const fn new(
        session_id: String,
        command: String,
        cwd: String,
        exit_code: Option<i32>,
        started_at: i64,
        ended_at: i64,
    ) -> Self {
        Self {
            id: None,
            session_id,
            command,
            cwd,
            exit_code,
            started_at,
            ended_at,
            duration_ms: if ended_at > started_at {
                ended_at - started_at
            } else {
                0
            },
            context: None,
            tag_name: None,
            tag_id: None,
            executor_type: None,
            executor: None,
        }
    }

    /// Parse the `executor_type` string into a typed `ExecutorKind`.
    pub fn executor_kind(&self) -> ExecutorKind {
        ExecutorKind::from_str_opt(self.executor_type.as_deref())
    }

    /// Returns `true` if this entry was executed by a human (or unknown/missing executor).
    pub fn is_human(&self) -> bool {
        self.executor_kind().is_human()
    }

    /// Returns `true` if this entry was executed by an agent (not human/unknown).
    pub fn is_agent(&self) -> bool {
        !self.is_human()
    }

    /// Set `tag_id`
    pub const fn with_tag_id(mut self, tag_id: Option<i64>) -> Self {
        self.tag_id = tag_id;
        self
    }

    /// Test helper: create an entry with sensible defaults.
    /// Reduces duplicated `make_entry`/`create_test_entry` helpers across test modules.
    #[cfg(test)]
    pub fn test(command: &str) -> Self {
        Self::new(
            "test-session".to_string(),
            command.to_string(),
            "/test".to_string(),
            Some(0),
            1_000_000,
            1_000_050,
        )
    }

    /// Test helper: create an entry with a specific exit code.
    #[cfg(test)]
    pub fn test_with_exit(command: &str, exit_code: Option<i32>) -> Self {
        let mut entry = Self::test(command);
        entry.exit_code = exit_code;
        entry
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
}

/// A note attached to a history entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub id: i64,
    pub entry_id: i64,
    pub content: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A bookmarked command
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: i64,
    pub command: String,
    pub label: Option<String>,
    pub created_at: i64,
}

/// Known executor types for type-safe matching.
///
/// The `executor_type` field on `Entry` is stored as `Option<String>` for
/// database and serde compatibility. Use `Entry::executor_kind()` to get
/// a typed `ExecutorKind` for exhaustive matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutorKind {
    Human,
    Agent,
    Bot,
    Ide,
    Ci,
    Programmatic,
    Unknown,
}

impl ExecutorKind {
    /// Parse an executor type string into a typed variant.
    pub fn from_str_opt(s: Option<&str>) -> Self {
        match s {
            Some("human") => Self::Human,
            Some("agent") => Self::Agent,
            Some("bot") => Self::Bot,
            Some("ide") => Self::Ide,
            Some("ci") => Self::Ci,
            Some("programmatic") => Self::Programmatic,
            _ => Self::Unknown,
        }
    }

    /// Returns `true` for human or unknown/missing executor types.
    pub const fn is_human(self) -> bool {
        matches!(self, Self::Human | Self::Unknown)
    }
}

/// Which entry field to search on.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SearchField {
    #[default]
    Command,
    Cwd,
    Session,
    Executor,
}

/// A managed shell alias
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alias {
    pub id: i64,
    pub name: String,
    pub command: String,
    pub created_at: i64,
}

/// Summary of a session with aggregated stats (for session picker)
#[derive(Debug)]
pub struct SessionSummary {
    pub id: String,
    pub hostname: String,
    pub created_at: i64,
    pub tag_name: Option<String>,
    pub cmd_count: i64,
    pub success_count: i64,
    pub first_cmd_at: i64,
    pub last_cmd_at: i64,
}

/// Aggregated usage statistics
#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    pub total_commands: i64,
    pub unique_commands: i64,
    pub success_count: i64,
    pub failure_count: i64,
    pub avg_duration_ms: i64,
    pub top_commands: Vec<(String, i64)>,
    pub top_directories: Vec<(String, i64)>,
    pub hourly_distribution: Vec<(u32, i64)>,
    pub executor_breakdown: Vec<(String, i64)>,
    pub period_days: Option<usize>,
}

/// An alias suggestion for a frequently-used command
pub struct AliasSuggestion {
    pub name: String,
    pub command: String,
    pub count: i64,
    pub dir_count: i64,
    pub selected: bool,
}

/// Represents a shell session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub hostname: String,
    pub created_at: i64, // Unix milliseconds
    pub tag_id: Option<i64>,
}

impl Session {
    /// Create a new session with a generated UUID
    #[cfg(test)]
    pub fn new(hostname: String, created_at: i64) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            hostname,
            created_at,
            tag_id: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entry_creation() {
        let entry = Entry::new(
            "test-session".to_string(),
            "ls -la".to_string(),
            "/home/user".to_string(),
            Some(0),
            1000,
            1050,
        );

        assert_eq!(entry.session_id, "test-session");
        assert_eq!(entry.command, "ls -la");
        assert_eq!(entry.duration_ms, 50);
        assert_eq!(entry.exit_code, Some(0));
    }

    #[test]
    fn test_entry_with_context() {
        let mut context = HashMap::new();
        context.insert("shell".to_string(), "zsh".to_string());

        let mut entry = Entry::new(
            "test-session".to_string(),
            "echo test".to_string(),
            "/tmp".to_string(),
            Some(0),
            2000,
            2010,
        );
        entry.context = Some(context);

        assert!(entry.context.is_some());
        assert_eq!(entry.context.unwrap().get("shell").unwrap(), "zsh");
    }

    #[test]
    fn test_session_creation() {
        let session = Session::new("localhost".to_string(), 1000);

        assert_eq!(session.hostname, "localhost");
        assert_eq!(session.created_at, 1000);
        assert!(!session.id.is_empty());

        // Verify it's a valid UUID
        uuid::Uuid::parse_str(&session.id).expect("Should be valid UUID");
    }

    #[test]
    fn test_alias_suggestion_creation() {
        let suggestion = AliasSuggestion {
            name: "dcu".to_string(),
            command: "docker compose up".to_string(),
            count: 42,
            dir_count: 3,
            selected: true,
        };

        assert_eq!(suggestion.name, "dcu");
        assert_eq!(suggestion.command, "docker compose up");
        assert_eq!(suggestion.count, 42);
        assert_eq!(suggestion.dir_count, 3);
        assert!(suggestion.selected);

        // Test with selected = false
        let unselected = AliasSuggestion {
            name: "gs".to_string(),
            command: "git status".to_string(),
            count: 100,
            dir_count: 5,
            selected: false,
        };
        assert!(!unselected.selected);
    }

    #[test]
    fn test_executor_kind_from_str_opt() {
        assert_eq!(ExecutorKind::from_str_opt(None), ExecutorKind::Unknown);
        assert_eq!(
            ExecutorKind::from_str_opt(Some("human")),
            ExecutorKind::Human
        );
        assert_eq!(
            ExecutorKind::from_str_opt(Some("agent")),
            ExecutorKind::Agent
        );
        assert_eq!(ExecutorKind::from_str_opt(Some("bot")), ExecutorKind::Bot);
        assert_eq!(ExecutorKind::from_str_opt(Some("ide")), ExecutorKind::Ide);
        assert_eq!(ExecutorKind::from_str_opt(Some("ci")), ExecutorKind::Ci);
        assert_eq!(
            ExecutorKind::from_str_opt(Some("programmatic")),
            ExecutorKind::Programmatic
        );
        assert_eq!(
            ExecutorKind::from_str_opt(Some("unrecognized")),
            ExecutorKind::Unknown
        );
    }

    #[test]
    fn test_executor_kind_is_human() {
        assert!(ExecutorKind::Human.is_human());
        assert!(ExecutorKind::Unknown.is_human());
        assert!(!ExecutorKind::Agent.is_human());
        assert!(!ExecutorKind::Bot.is_human());
        assert!(!ExecutorKind::Ide.is_human());
        assert!(!ExecutorKind::Ci.is_human());
        assert!(!ExecutorKind::Programmatic.is_human());
    }

    #[test]
    fn test_entry_executor_kind() {
        let mut entry = Entry::test("ls");
        assert_eq!(entry.executor_kind(), ExecutorKind::Unknown);

        entry.executor_type = Some("agent".to_string());
        assert_eq!(entry.executor_kind(), ExecutorKind::Agent);

        entry.executor_type = Some("human".to_string());
        assert_eq!(entry.executor_kind(), ExecutorKind::Human);
    }

    #[test]
    fn test_entry_is_agent() {
        let mut entry = Entry::new(
            "s1".to_string(),
            "ls".to_string(),
            "/tmp".to_string(),
            Some(0),
            1000,
            1050,
        );

        // No executor_type → not agent
        assert!(!entry.is_agent());

        entry.executor_type = Some("human".to_string());
        assert!(!entry.is_agent());

        entry.executor_type = Some("unknown".to_string());
        assert!(!entry.is_agent());

        entry.executor_type = Some("agent".to_string());
        assert!(entry.is_agent());

        entry.executor_type = Some("ide".to_string());
        assert!(entry.is_agent());

        entry.executor_type = Some("ci".to_string());
        assert!(entry.is_agent());

        entry.executor_type = Some("programmatic".to_string());
        assert!(entry.is_agent());
    }
}
