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
            duration_ms: ended_at - started_at,
            context: None,
            tag_name: None,
            tag_id: None,
            executor_type: None,
            executor: None,
        }
    }

    /// Returns `true` if this entry was executed by an agent (not human/unknown).
    pub fn is_agent(&self) -> bool {
        matches!(self.executor_type.as_deref(), Some(et) if et != "human" && et != "unknown")
    }

    /// Set `tag_id`
    pub const fn with_tag_id(mut self, tag_id: Option<i64>) -> Self {
        self.tag_id = tag_id;
        self
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

/// Aggregated usage statistics
#[derive(Debug, Clone)]
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

        entry.executor_type = Some("claude-code".to_string());
        assert!(entry.is_agent());
    }
}
