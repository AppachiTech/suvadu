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
#[serde(deny_unknown_fields)]
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
    #[serde(default = "default_true")]
    pub show_detail_pane: bool,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            page_limit: 50,
            show_unique_by_default: false,
            filter_by_current_session_tag: false,
            context_boost: true,
            show_detail_pane: true,
        }
    }
}

const fn default_page_limit() -> usize {
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
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            show_risk_in_search: true,
            risk_ignore_patterns: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactionConfig {
    /// Enable automatic secret redaction before storage (default: true)
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for RedactionConfig {
    fn default() -> Self {
        Self { enabled: true }
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

#[cfg(not(target_os = "macos"))]
pub fn migrate_config_macos() {}

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
    let current_mtime = std::fs::metadata(&path)
        .ok()
        .and_then(|m| m.modified().ok());

    if let Ok(guard) = CONFIG_CACHE.lock() {
        if let Some(cached) = guard.as_ref() {
            if cached.mtime == current_mtime {
                return Ok(cached.config.clone());
            }
        }
    }

    let config = load_config()?;

    if let Ok(mut guard) = CONFIG_CACHE.lock() {
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
    for pattern in &config.exclusions {
        if pattern.is_empty() {
            return Err(ConfigError::Path(
                "exclusion patterns must not be empty".into(),
            ));
        }
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
    tmp.persist(&path).map_err(std::io::Error::from)?;

    // Restrict config file to owner-only on Unix (may contain exclusion patterns)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

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
    std::env::var("SUVADU_PAUSED")
        .map(|v| v == "1" || v.to_lowercase() == "true")
        .unwrap_or(false)
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
}
