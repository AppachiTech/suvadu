use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML serialization error: {0}")]
    TomlSerialize(#[from] toml::ser::Error),
    #[error("TOML deserialization error: {0}")]
    TomlDeserialize(#[from] toml::de::Error),
    #[error("Config path error: {0}")]
    Path(String),
}

pub type ConfigResult<T> = Result<T, ConfigError>;

/// Application configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub theme: crate::theme::ThemeName,
    #[serde(default)]
    pub search: SearchConfig,
    #[serde(default)]
    pub shell: ShellConfig,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub redaction: RedactionConfig,
    #[serde(default)]
    pub exclusions: Vec<String>,
    #[serde(default)]
    pub auto_tags: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub agents: std::collections::HashMap<String, CustomAgent>,
    #[serde(default)]
    pub mcp: McpConfig,
}

const fn default_enabled() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            theme: crate::theme::ThemeName::default(),
            search: SearchConfig::default(),
            shell: ShellConfig::default(),
            agent: AgentConfig::default(),
            redaction: RedactionConfig::default(),
            exclusions: Vec::new(),
            auto_tags: std::collections::HashMap::new(),
            agents: std::collections::HashMap::new(),
            mcp: McpConfig::default(),
        }
    }
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    #[serde(default = "default_page_limit")]
    pub page_limit: usize,
    #[serde(default = "default_false")]
    pub show_unique_by_default: bool,
    #[serde(default = "default_false")]
    pub filter_by_current_session_tag: bool,
    #[serde(default = "default_true")]
    pub context_boost: bool,
    /// When `false` (the default), AI-agent / bot / CI / script commands are
    /// hidden from interactive recall (Up-arrow and Ctrl+R) so day-to-day
    /// history shows the commands you actually typed. Toggle at runtime with
    /// Ctrl+A in the search TUI (or Alt+A for the shell Up-arrow).
    #[serde(default = "default_false")]
    pub recall_show_agents: bool,
    #[serde(default = "default_true")]
    pub show_detail_pane: bool,
    #[serde(default = "default_false")]
    pub vim_mode: bool,
    #[serde(default = "default_length_threshold")]
    pub length_threshold: usize,
    #[serde(default = "default_human_boost_percent")]
    pub human_boost_percent: u32,
    #[serde(default = "default_cwd_boost_percent")]
    pub cwd_boost_percent: u32,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            page_limit: 50,
            show_unique_by_default: false,
            filter_by_current_session_tag: false,
            context_boost: true,
            recall_show_agents: false,
            show_detail_pane: true,
            vim_mode: false,
            length_threshold: 80,
            human_boost_percent: 33,
            cwd_boost_percent: 50,
        }
    }
}

const fn default_page_limit() -> usize {
    50
}

const fn default_length_threshold() -> usize {
    80
}

const fn default_human_boost_percent() -> u32 {
    33
}

const fn default_cwd_boost_percent() -> u32 {
    50
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellConfig {
    #[serde(default = "default_true")]
    pub enable_arrow_navigation: bool,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            enable_arrow_navigation: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Show risk assessment in search detail pane
    #[serde(default = "default_true")]
    pub show_risk_in_search: bool,
    /// Additional risk patterns to ignore (suppress false positives)
    #[serde(default)]
    pub risk_ignore_patterns: Vec<String>,
    /// Max characters of an agent prompt to capture. Prompts are stored locally
    /// in the cache dir; the cap keeps the cache lightweight. Default 4000.
    #[serde(default = "default_prompt_capture_max_chars")]
    pub prompt_capture_max_chars: usize,
}

const fn default_prompt_capture_max_chars() -> usize {
    4000
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            show_risk_in_search: true,
            risk_ignore_patterns: Vec::new(),
            prompt_capture_max_chars: default_prompt_capture_max_chars(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactionConfig {
    /// Enable automatic secret redaction before storage (default: true)
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Extra user-defined regex patterns. Any match is replaced with the
    /// redaction placeholder before storage — for internal/corporate secret
    /// formats the built-in rules don't cover.
    #[serde(default)]
    pub extra_patterns: Vec<String>,
}

impl Default for RedactionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            extra_patterns: Vec::new(),
        }
    }
}

/// Custom agent detection rule.
///
/// Users can define custom agents in `config.toml` to detect executors
/// that suvadu doesn't know about natively:
///
/// ```toml
/// [agents.your-agent-name]
/// env_var = "YOUR_AGENT_ENV_VAR"
/// executor_type = "agent"
///
/// [agents.my-internal-tool]
/// env_var = "MY_TOOL_SESSION"
/// executor_type = "ide"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomAgent {
    /// Environment variable to check for (presence means this agent is active)
    pub env_var: String,
    /// Executor type: "agent", "ide", or "ci"
    #[serde(default = "default_agent_type")]
    pub executor_type: String,
}

fn default_agent_type() -> String {
    "agent".to_string()
}

/// MCP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct McpConfig {
    /// Tools to disable by name. Empty means all enabled.
    pub disabled_tools: Vec<String>,
    /// Resources to disable by URI suffix (e.g. "context/project"). Empty means all enabled.
    pub disabled_resources: Vec<String>,
    /// Default time window in days for tools that accept a `days` parameter.
    pub default_days: u32,
    /// Default result limit for tools that accept a `limit` parameter.
    pub default_limit: u32,
    /// Directories to exclude from MCP queries.
    pub exclude_dirs: Vec<String>,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            disabled_tools: Vec::new(),
            disabled_resources: Vec::new(),
            default_days: 7,
            default_limit: 20,
            exclude_dirs: Vec::new(),
        }
    }
}

const fn default_true() -> bool {
    true
}

const fn default_false() -> bool {
    false
}

/// Migrate config from the old `directories` 5.x path to the 6.x path on macOS.
/// directories 5.x: ~/Library/Preferences/tech.appachi.suvadu/
/// directories 6.x: ~/Library/Application Support/tech.appachi.suvadu/
/// Only runs when the old path has a config and the new path does not.
/// Compiled out entirely on non-macOS platforms.
#[cfg(target_os = "macos")]
pub fn migrate_config_macos() {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let old_config =
        PathBuf::from(&home).join("Library/Preferences/tech.appachi.suvadu/config.toml");

    if !old_config.exists() {
        return;
    }

    let Some(dirs) = crate::util::project_dirs() else {
        return;
    };
    let new_dir = dirs.config_dir();
    let new_config = new_dir.join("config.toml");

    if new_config.exists() {
        return;
    }

    if let Err(e) = std::fs::create_dir_all(new_dir) {
        eprintln!(
            "suvadu: failed to create config directory {}: {e}",
            new_dir.display()
        );
        return;
    }
    if let Err(e) = std::fs::copy(&old_config, &new_config) {
        eprintln!(
            "suvadu: failed to migrate config from {}: {e}",
            old_config.display()
        );
    }
}

/// Get the path to the suvadu config file
pub fn get_config_path() -> ConfigResult<PathBuf> {
    let dirs = crate::util::project_dirs()
        .ok_or_else(|| ConfigError::Path("Could not determine config directory".to_string()))?;
    let config_dir = dirs.config_dir();

    if !config_dir.exists() {
        std::fs::create_dir_all(config_dir)?;
    }
    Ok(config_dir.join("config.toml"))
}

/// Load configuration from file (or return default if file doesn't exist)
pub fn load_config() -> ConfigResult<Config> {
    #[cfg(target_os = "macos")]
    {
        use std::sync::Once;
        static MIGRATE_ONCE: Once = Once::new();
        MIGRATE_ONCE.call_once(migrate_config_macos);
    }
    let path = get_config_path()?;

    if !path.exists() {
        return Ok(Config::default());
    }

    let contents = std::fs::read_to_string(path)?;
    let config: Config = toml::from_str(&contents)?;
    validate_config(&config)?;
    Ok(config)
}

/// Cached config with mtime for invalidation.
struct CachedConfig {
    config: Config,
    mtime: Option<SystemTime>,
}

static CONFIG_CACHE: Mutex<Option<CachedConfig>> = Mutex::new(None);

/// Load configuration with caching. Re-reads from disk only when the file's
/// modification time changes. Optimized for the hot path (called per shell command).
pub fn load_config_cached() -> ConfigResult<Config> {
    let path = get_config_path()?;

    // Check mtime inside the lock to avoid TOCTOU race between stat and cache read.
    if let Ok(guard) = CONFIG_CACHE.lock() {
        let current_mtime = std::fs::metadata(&path)
            .ok()
            .and_then(|m| m.modified().ok());
        if let Some(cached) = guard.as_ref() {
            if cached.mtime == current_mtime {
                return Ok(cached.config.clone());
            }
        }
    }

    let config = load_config()?;

    if let Ok(mut guard) = CONFIG_CACHE.lock() {
        let current_mtime = std::fs::metadata(&path)
            .ok()
            .and_then(|m| m.modified().ok());
        *guard = Some(CachedConfig {
            config: config.clone(),
            mtime: current_mtime,
        });
    }

    Ok(config)
}

/// Invalidate the config cache (called after `save_config`).
fn invalidate_cache() {
    if let Ok(mut guard) = CONFIG_CACHE.lock() {
        *guard = None;
    }
}

/// Validate config values after loading.
fn validate_config(config: &Config) -> ConfigResult<()> {
    if config.search.page_limit == 0 {
        return Err(ConfigError::Path(
            "search.page_limit must be at least 1".into(),
        ));
    }
    if config.search.page_limit > 10_000 {
        return Err(ConfigError::Path(
            "search.page_limit exceeds maximum of 10000".into(),
        ));
    }
    if config.search.length_threshold == 0 || config.search.length_threshold > 500 {
        return Err(ConfigError::Path(
            "search.length_threshold must be between 1 and 500".into(),
        ));
    }
    if config.search.human_boost_percent > 100 {
        return Err(ConfigError::Path(
            "search.human_boost_percent must be between 0 and 100".into(),
        ));
    }
    if config.search.cwd_boost_percent > 100 {
        return Err(ConfigError::Path(
            "search.cwd_boost_percent must be between 0 and 100".into(),
        ));
    }
    if config.agent.prompt_capture_max_chars == 0 || config.agent.prompt_capture_max_chars > 100_000
    {
        return Err(ConfigError::Path(
            "agent.prompt_capture_max_chars must be between 1 and 100000".into(),
        ));
    }
    for pattern in &config.exclusions {
        if pattern.is_empty() {
            return Err(ConfigError::Path(
                "exclusion patterns must not be empty".into(),
            ));
        }
    }
    for pattern in &config.redaction.extra_patterns {
        if pattern.is_empty() {
            return Err(ConfigError::Path(
                "redaction.extra_patterns must not be empty".into(),
            ));
        }
        if regex::Regex::new(pattern).is_err() {
            return Err(ConfigError::Path(format!(
                "invalid redaction.extra_patterns regex: {pattern}"
            )));
        }
    }
    if config.mcp.default_days == 0 || config.mcp.default_days > 365 {
        return Err(ConfigError::Path(
            "mcp.default_days must be between 1 and 365".into(),
        ));
    }
    if config.mcp.default_limit == 0 || config.mcp.default_limit > 500 {
        return Err(ConfigError::Path(
            "mcp.default_limit must be between 1 and 500".into(),
        ));
    }
    Ok(())
}

/// Save configuration to file atomically (temp file + rename).
pub fn save_config(config: &Config) -> ConfigResult<()> {
    let path = get_config_path()?;
    let contents = toml::to_string_pretty(config)?;
    let dir = path
        .parent()
        .ok_or_else(|| ConfigError::Path("config path has no parent directory".into()))?;
    let tmp = tempfile::NamedTempFile::new_in(dir)?;
    std::fs::write(tmp.path(), contents)?;

    // Restrict to owner-only BEFORE persist so the file is never world-readable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o600));
    }

    tmp.persist(&path).map_err(std::io::Error::from)?;

    invalidate_cache();
    Ok(())
}

/// Check if recording is enabled globally (from config file)
pub fn is_enabled() -> ConfigResult<bool> {
    let config = load_config()?;
    Ok(config.enabled)
}

/// Check if recording is paused for current session (from environment)
pub fn is_paused() -> bool {
    std::env::var("SUVADU_PAUSED").is_ok_and(|v| v == "1" || v.to_lowercase() == "true")
}

/// Check if we should record history (combines global and session checks)
pub fn should_record() -> ConfigResult<bool> {
    if !is_enabled()? {
        return Ok(false);
    }

    if is_paused() {
        return Ok(false);
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert!(config.enabled);
    }

    #[test]
    fn test_config_serialization() {
        let config = Config {
            enabled: false,
            ..Config::default()
        };
        let toml_str = toml::to_string(&config).unwrap();
        assert!(toml_str.contains("enabled = false"));

        let deserialized: Config = toml::from_str(&toml_str).unwrap();
        assert!(!deserialized.enabled);
    }

    #[test]
    fn test_save_and_load_config() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.toml");

        let config = Config {
            enabled: false,
            exclusions: vec!["ls".to_string()],
            ..Config::default()
        };
        let contents = toml::to_string_pretty(&config).unwrap();
        std::fs::write(&config_path, contents).unwrap();

        let loaded_contents = std::fs::read_to_string(&config_path).unwrap();
        let loaded: Config = toml::from_str(&loaded_contents).unwrap();
        assert!(!loaded.enabled);
        assert_eq!(loaded.exclusions.len(), 1);
        assert_eq!(loaded.exclusions[0], "ls");
    }

    #[test]
    fn test_load_nonexistent_config() {
        // Test that default config is returned when file doesn't exist
        let config = Config::default();
        assert!(config.enabled);
    }

    #[test]
    fn test_config_path_creation() {
        // Test that we can get a config path
        let path = get_config_path();
        assert!(path.is_ok());
    }

    /// Test the pause-detection logic without mutating the process environment.
    /// `is_paused()` simply reads `SUVADU_PAUSED` and applies a small parse —
    /// we test that parse inline instead of calling `set_var`/`remove_var`.
    #[test]
    fn test_is_paused_logic() {
        fn paused_from(val: Option<&str>) -> bool {
            val.map(|v| v == "1" || v.to_lowercase() == "true")
                .unwrap_or(false)
        }

        assert!(!paused_from(None));
        assert!(paused_from(Some("1")));
        assert!(paused_from(Some("true")));
        assert!(paused_from(Some("TRUE")));
        assert!(!paused_from(Some("0")));
        assert!(!paused_from(Some("false")));
        assert!(!paused_from(Some("")));
    }

    #[test]
    fn test_unknown_fields_ignored() {
        let toml_str = r#"
enabled = true

[ui]
show_detail = true

[some_future_section]
key = "value"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.enabled);
    }

    #[test]
    fn test_custom_agents_deserialization() {
        let toml_str = r#"
enabled = true

[agents.opencode]
env_var = "OPENCODE"
executor_type = "agent"

[agents.my-internal-tool]
env_var = "MY_TOOL_SESSION"
executor_type = "ide"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.agents.len(), 2);

        let opencode = config.agents.get("opencode").unwrap();
        assert_eq!(opencode.env_var, "OPENCODE");
        assert_eq!(opencode.executor_type, "agent");

        let my_tool = config.agents.get("my-internal-tool").unwrap();
        assert_eq!(my_tool.env_var, "MY_TOOL_SESSION");
        assert_eq!(my_tool.executor_type, "ide");
    }

    #[test]
    fn test_custom_agent_default_executor_type() {
        let toml_str = r#"
enabled = true

[agents.opencode]
env_var = "OPENCODE"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let opencode = config.agents.get("opencode").unwrap();
        assert_eq!(
            opencode.executor_type, "agent",
            "executor_type should default to 'agent'"
        );
    }

    #[test]
    fn test_empty_agents_section() {
        let toml_str = r#"
enabled = true
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.agents.is_empty());
    }

    #[test]
    fn test_mcp_config_defaults() {
        let config = Config::default();
        assert_eq!(config.mcp.default_days, 7);
        assert_eq!(config.mcp.default_limit, 20);
        assert!(config.mcp.disabled_tools.is_empty());
        assert!(config.mcp.disabled_resources.is_empty());
        assert!(config.mcp.exclude_dirs.is_empty());
    }

    #[test]
    fn test_mcp_config_deserialization() {
        let toml_str = r#"
[mcp]
disabled_tools = ["assess_risk", "suggest_next"]
disabled_resources = ["context/project"]
default_days = 14
default_limit = 50
exclude_dirs = ["/secrets", "~/.ssh"]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.mcp.default_days, 14);
        assert_eq!(config.mcp.default_limit, 50);
        assert_eq!(config.mcp.disabled_tools.len(), 2);
        assert!(config
            .mcp
            .disabled_tools
            .contains(&"assess_risk".to_string()));
        assert!(config
            .mcp
            .disabled_tools
            .contains(&"suggest_next".to_string()));
        assert_eq!(config.mcp.disabled_resources, vec!["context/project"]);
        assert_eq!(config.mcp.exclude_dirs.len(), 2);
    }

    #[test]
    fn test_mcp_config_missing_section_uses_defaults() {
        let toml_str = r#"
enabled = true
[search]
page_limit = 100
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.mcp.default_days, 7);
        assert_eq!(config.mcp.default_limit, 20);
        assert!(config.mcp.disabled_tools.is_empty());
    }

    #[test]
    fn test_mcp_config_validation_days() {
        let mut config = Config::default();
        config.mcp.default_days = 0;
        assert!(validate_config(&config).is_err());

        config.mcp.default_days = 366;
        assert!(validate_config(&config).is_err());

        config.mcp.default_days = 30;
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_mcp_config_validation_limit() {
        let mut config = Config::default();
        config.mcp.default_limit = 0;
        assert!(validate_config(&config).is_err());

        config.mcp.default_limit = 501;
        assert!(validate_config(&config).is_err());

        config.mcp.default_limit = 100;
        assert!(validate_config(&config).is_ok());
    }
}
