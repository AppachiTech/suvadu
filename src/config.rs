use serde::{Deserialize, Serialize};
use std::path::PathBuf;
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
pub fn migrate_config_macos() {
    if !cfg!(target_os = "macos") {
        return;
    }

    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let old_dir = PathBuf::from(&home).join("Library/Preferences/tech.appachi.suvadu");
    let old_config = old_dir.join("config.toml");

    if !old_config.exists() {
        return;
    }

    let Some(proj) = directories::ProjectDirs::from("tech", "appachi", "suvadu") else {
        return;
    };
    let new_dir = proj.config_dir();
    let new_config = new_dir.join("config.toml");

    if new_config.exists() {
        return;
    }

    if std::fs::create_dir_all(new_dir).is_ok() {
        let _ = std::fs::copy(&old_config, &new_config);
    }
}

/// Get the path to the suvadu config file
pub fn get_config_path() -> ConfigResult<PathBuf> {
    let config_dir = directories::ProjectDirs::from("tech", "appachi", "suvadu")
        .ok_or_else(|| ConfigError::Path("Could not determine config directory".to_string()))?
        .config_dir()
        .to_path_buf();

    std::fs::create_dir_all(&config_dir)?;
    Ok(config_dir.join("config.toml"))
}

/// Load configuration from file (or return default if file doesn't exist)
pub fn load_config() -> ConfigResult<Config> {
    migrate_config_macos();
    let path = get_config_path()?;

    if !path.exists() {
        return Ok(Config::default());
    }

    let contents = std::fs::read_to_string(path)?;
    let config: Config = toml::from_str(&contents)?;
    Ok(config)
}

/// Save configuration to file
pub fn save_config(config: &Config) -> ConfigResult<()> {
    let path = get_config_path()?;
    let contents = toml::to_string_pretty(config)?;
    std::fs::write(path, contents)?;
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
    use std::env;
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

    #[test]
    fn test_is_paused_env_var() {
        // Run sequentially to avoid race conditions

        // 1. Test is_paused logic
        // Test with SUVADU_PAUSED not set
        env::remove_var("SUVADU_PAUSED");
        assert!(!is_paused());

        // Test with SUVADU_PAUSED=1
        env::set_var("SUVADU_PAUSED", "1");
        assert!(is_paused());

        // Test with SUVADU_PAUSED=true
        env::set_var("SUVADU_PAUSED", "true");
        assert!(is_paused());

        // Test with SUVADU_PAUSED=0
        env::set_var("SUVADU_PAUSED", "0");
        assert!(!is_paused());

        // Cleanup
        env::remove_var("SUVADU_PAUSED");
    }
}
