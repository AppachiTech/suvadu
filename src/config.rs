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
    #[serde(default)]
    pub home: HomeConfig,
    #[serde(default)]
    pub update: UpdateConfig,
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
            home: HomeConfig::default(),
            update: UpdateConfig::default(),
        }
    }
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    #[serde(default = "default_page_limit")]
    pub page_limit: usize,
    /// Open recall in the Commands view — identical command text grouped
    /// into one row with its last use and run count — rather than listing
    /// every run. On by default: retrieving a command is the common task,
    /// and repeated runs otherwise push the other candidates down. A value
    /// written in `config.toml` (including by `suv settings`) is kept, so
    /// only a config without the key gets this default. `^U` switches views.
    #[serde(default = "default_true")]
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
    /// In Smart rank, boost what you have usually run next after the command
    /// you just ran in this shell session. On by default; `false` keeps
    /// Smart rank's other boosts and drops only this one.
    #[serde(default = "default_true")]
    pub sequence_boost: bool,
    /// How a typed query is interpreted: `terms` (default), `literal`,
    /// `prefix` or `fuzzy`. `terms` is what recall has always done, so an
    /// upgrade never reinterprets an existing query. Override per run with
    /// `--match`, or cycle with Ctrl+X in the search TUI.
    #[serde(default)]
    pub match_mode: crate::search::MatchMode,
    /// Which slice of history recall starts on: `all` (default),
    /// `directory`, `workspace` or `session`. Override with `--scope`,
    /// cycle with Ctrl+P, reset to `all` with Ctrl+R.
    #[serde(default)]
    pub scope: crate::search::RecallScope,
    /// When `true`, recall opens inline under the prompt instead of taking
    /// over the screen. Off by default: the full-screen inspector stays the
    /// default experience. Also available per run as `--compact`.
    #[serde(default = "default_false")]
    pub compact: bool,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            page_limit: 50,
            show_unique_by_default: true,
            filter_by_current_session_tag: false,
            context_boost: true,
            recall_show_agents: false,
            show_detail_pane: true,
            vim_mode: false,
            length_threshold: 80,
            human_boost_percent: 33,
            cwd_boost_percent: 50,
            sequence_boost: true,
            // The pre-PROD-09 behaviour, kept as the default on purpose:
            // upgrading must not change how anyone's queries are read.
            match_mode: crate::search::MatchMode::Terms,
            scope: crate::search::RecallScope::All,
            compact: false,
        }
    }
}

/// The largest `search.page_limit` the config accepts, and the most
/// `suv settings` will set.
pub const MAX_PAGE_LIMIT: usize = 10_000;

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
    /// Record the git branch checked out where each command started, read
    /// from the repository's files when the command is recorded.
    #[serde(default = "default_true")]
    pub capture_git_branch: bool,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            enable_arrow_navigation: true,
            capture_git_branch: true,
        }
    }
}

/// A user-defined risk pattern. Complements `risk_ignore_patterns` (which
/// only suppresses built-in matches) by letting a user flag their *own*
/// commands as risky — e.g. an org's own deploy script that the built-in
/// pattern set has no way to know about.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RiskPatternConfig {
    pub pattern: String,
    /// "low" | "medium" | "high" | "critical" (case-insensitive). An
    /// unparseable value is skipped with a warning at startup, same as an
    /// invalid regex.
    pub level: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Show risk assessment in search detail pane
    #[serde(default = "default_true")]
    pub show_risk_in_search: bool,
    /// Additional risk patterns to ignore (suppress false positives)
    #[serde(default)]
    pub risk_ignore_patterns: Vec<String>,
    /// Additional risk patterns to flag, beyond the built-in set (see
    /// [`RiskPatternConfig`]).
    #[serde(default)]
    pub risk_extra_patterns: Vec<RiskPatternConfig>,
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
            risk_extra_patterns: Vec::new(),
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
    /// Allow agents to call `propose_skill` over MCP, writing a new skill
    /// with `status = "pending_review"` for a human to approve from the
    /// review queue in `suv skills` (Ctrl+P). Off by default: a skill store
    /// that agents can
    /// both read and write is a shared-memory poisoning target, so writes
    /// require an explicit opt-in even though they only ever land as
    /// pending, never active.
    pub allow_skill_proposals: bool,
    /// Allow caller-generated session summaries to be stored over MCP. Off by default.
    pub allow_session_summaries: bool,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            disabled_tools: Vec::new(),
            disabled_resources: Vec::new(),
            default_days: 7,
            default_limit: 20,
            exclude_dirs: Vec::new(),
            allow_skill_proposals: false,
            allow_session_summaries: false,
        }
    }
}

/// What a bare `suv` at a terminal opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HomeStartup {
    /// The interactive Home screen.
    Home,
    /// The classic command overview, as `suv --help` prints it.
    Help,
}

/// Markers drawn beside Home's categories. ASCII works in every terminal
/// and font; Unicode uses a few monochrome symbols.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HomeIcons {
    #[default]
    Ascii,
    Unicode,
}

/// Preferences for the Home screen. They belong to the person, so only the
/// global config sets them: a project's `.suvadu.toml` cannot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeConfig {
    /// Unset means "this release's default", and stays unset when other
    /// settings are saved, so a later release can change the default for
    /// everyone who never chose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup: Option<HomeStartup>,
    #[serde(default)]
    pub icons: HomeIcons,
}

impl HomeConfig {
    /// What a bare `suv` at a terminal opens for someone who has not
    /// chosen: Home. `startup = "help"` keeps the classic overview, and
    /// scripts and pipes always get the overview whatever this says.
    pub const DEFAULT_STARTUP: HomeStartup = HomeStartup::Home;

    pub fn effective_startup(&self) -> HomeStartup {
        self.startup.unwrap_or(Self::DEFAULT_STARTUP)
    }
}

/// Whether Suvadu looks for a newer release, once a day, and says so. Like
/// the Home preferences, only the global config sets it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateConfig {
    #[serde(default = "default_true")]
    pub check: bool,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self { check: true }
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

/// Read one config file without creating, migrating or repairing anything:
/// `Ok(None)` when it does not exist, an error (and the file left exactly as
/// it is) when it cannot be read or parsed.
pub fn read_config_file(path: &std::path::Path) -> ConfigResult<Option<Config>> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let config: Config = toml::from_str(&contents)?;
    validate_config(&config)?;
    Ok(Some(config))
}

/// The global config, read the way Home needs it: no directory is created,
/// no older file is migrated, and no project overlay applies. On macOS a
/// config still at the pre-migration location is read from there.
pub fn read_global_config() -> ConfigResult<Option<Config>> {
    let dirs = crate::util::project_dirs()
        .ok_or_else(|| ConfigError::Path("Could not determine config directory".to_string()))?;
    let path = dirs.config_dir().join("config.toml");
    #[cfg(target_os = "macos")]
    if !path.exists() {
        if let Some(home) = std::env::var_os("HOME") {
            let old =
                PathBuf::from(home).join("Library/Preferences/tech.appachi.suvadu/config.toml");
            if old.exists() {
                return read_config_file(&old);
            }
        }
    }
    read_config_file(&path)
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

// ── Per-project config overlay ─────────────────────────────────

/// Find the nearest `.suvadu.toml`, searching `start_dir` and its ancestors
/// (the same walk-up-to-root discovery `.git` uses). Returns `None` if no
/// project overlay exists anywhere above `start_dir`.
pub fn find_project_overlay(start_dir: &std::path::Path) -> Option<PathBuf> {
    start_dir
        .ancestors()
        .map(|dir| dir.join(".suvadu.toml"))
        .find(|path| path.is_file())
}

/// Merge `overlay` onto `base` in place. A table merges key-by-key,
/// recursing into nested tables; anything else (a scalar or an array) is
/// replaced wholesale by the overlay's value when the overlay sets it.
/// Arrays are not concatenated here: one value per key, the more specific
/// one wins.
///
/// This is the generic rule only. [`overlay_config_for_dir`] then puts back
/// the privacy settings a project overlay is not allowed to weaken.
fn merge_toml_value(base: &mut toml::Value, overlay: toml::Value) {
    match overlay {
        toml::Value::Table(overlay_table) => {
            if let toml::Value::Table(base_table) = base {
                for (key, value) in overlay_table {
                    match base_table.get_mut(&key) {
                        Some(existing) => merge_toml_value(existing, value),
                        None => {
                            base_table.insert(key, value);
                        }
                    }
                }
            } else {
                *base = toml::Value::Table(overlay_table);
            }
        }
        other => *base = other,
    }
}

/// Load the effective config for `dir`: the global config, with any
/// `.suvadu.toml` found by walking up from `dir` merged on top. A field
/// left out of the overlay keeps the global config's value; a table in the
/// overlay only needs to name the fields it's actually changing, and the
/// overlay's value wins for the fields it sets.
///
/// Privacy is the exception, because a `.suvadu.toml` arrives with any
/// repository you clone: an overlay can only tighten it.
/// - `redaction.enabled = false` is ignored when the global config has
///   redaction on; an overlay can still turn it on.
/// - `enabled = true` is ignored when the global config has recording off;
///   an overlay can still turn recording off for its project.
/// - `shell.capture_git_branch = true` is likewise ignored when the global
///   config has it off.
/// - `exclusions` and `redaction.extra_patterns` are the union of the
///   global entries and the overlay's, never a replacement.
///
/// `home` and `update` are ignored in an overlay altogether.
///
/// Returns the global config unchanged if `dir` has no project overlay.
pub fn load_config_for_dir(dir: &std::path::Path) -> ConfigResult<Config> {
    overlay_config_for_dir(&load_config()?, dir)
}

/// [`load_config_for_dir`] against a base config the caller already holds.
///
/// Importers need this: they read the global config once and then resolve a
/// policy for each source row's own directory, without re-reading the global
/// file (and without silently picking up a different one) per directory.
pub fn overlay_config_for_dir(base: &Config, dir: &std::path::Path) -> ConfigResult<Config> {
    let Some(overlay_path) = find_project_overlay(dir) else {
        return Ok(base.clone());
    };

    let overlay_contents = std::fs::read_to_string(&overlay_path)?;
    let mut overlay_value: toml::Value = toml::from_str(&overlay_contents)?;
    // Home and update-check preferences are the person's, not the
    // project's; dropping the tables also keeps an invalid value there
    // from breaking every command.
    if let Some(table) = overlay_value.as_table_mut() {
        table.remove("home");
        table.remove("update");
    }
    let mut merged_value = toml::Value::try_from(base.clone())?;
    merge_toml_value(&mut merged_value, overlay_value);
    let mut merged: Config = merged_value.try_into()?;
    keep_global_privacy(base, &mut merged);
    validate_config(&merged)?;
    Ok(merged)
}

/// Undo anything a project overlay did to weaken `base`'s privacy settings
/// (see [`load_config_for_dir`]).
fn keep_global_privacy(base: &Config, merged: &mut Config) {
    merged.enabled &= base.enabled;
    merged.shell.capture_git_branch &= base.shell.capture_git_branch;
    merged.redaction.enabled |= base.redaction.enabled;
    merged.exclusions = union_keeping_order(&base.exclusions, &merged.exclusions);
    merged.redaction.extra_patterns = union_keeping_order(
        &base.redaction.extra_patterns,
        &merged.redaction.extra_patterns,
    );
}

/// `first` followed by each entry of `second` not already present.
fn union_keeping_order(first: &[String], second: &[String]) -> Vec<String> {
    let mut out = first.to_vec();
    for item in second {
        if !out.contains(item) {
            out.push(item.clone());
        }
    }
    out
}

/// `load_config_for_dir` for the current working directory. Falls back to
/// the global config alone if the working directory can't be determined.
pub fn load_config_for_cwd() -> ConfigResult<Config> {
    std::env::current_dir().map_or_else(|_| load_config(), |cwd| load_config_for_dir(&cwd))
}

// ── Capture-path fallback ──────────────────────────────────────

/// A config file the capture path could not use, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureConfigProblem {
    /// The global `config.toml` or the project `.suvadu.toml` that failed.
    pub path: PathBuf,
    pub message: String,
}

/// The config to record a command run in `dir` with. Unlike
/// [`load_config_for_dir`] this never fails, because a capture that errors
/// out records nothing and its stderr goes nowhere (shell hooks discard it):
/// a typo in a config file would silently stop all recording.
///
/// - The global config does not load: the built-in defaults are used, with
///   redaction on, and a project overlay still applies on top of them.
/// - Only the project overlay does not load: the global config is used.
///
/// The second value names the file that failed, so the caller can note it.
pub fn load_capture_config_for_dir(
    dir: &std::path::Path,
) -> (Config, Option<CaptureConfigProblem>) {
    let (base, global_problem) = match load_config() {
        Ok(config) => (config, None),
        Err(e) => (
            Config::default(),
            Some(CaptureConfigProblem {
                path: get_config_path().unwrap_or_else(|_| PathBuf::from("config.toml")),
                message: e.to_string(),
            }),
        ),
    };
    match overlay_config_for_dir(&base, dir) {
        Ok(config) => (config, global_problem),
        Err(e) => {
            let problem = global_problem.unwrap_or_else(|| CaptureConfigProblem {
                path: find_project_overlay(dir).unwrap_or_else(|| dir.join(".suvadu.toml")),
                message: e.to_string(),
            });
            (base, Some(problem))
        }
    }
}

/// [`load_capture_config_for_dir`], noting any file that failed for
/// `suv doctor` with [`record_capture_problem`]. What every capture path --
/// recording a command, caching an agent prompt, importing a transcript --
/// resolves its policy with.
pub fn capture_config_for_dir(dir: &std::path::Path) -> Config {
    let (config, problem) = load_capture_config_for_dir(dir);
    if let Some(problem) = problem {
        record_capture_problem(&problem);
    }
    config
}

/// The most recent [`CaptureConfigProblem`] the capture path hit, as left in
/// the data directory for `suv doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureProblemNote {
    /// When it was first seen (RFC 3339, local time).
    pub at: String,
    pub problem: CaptureConfigProblem,
}

const CAPTURE_PROBLEM_FILE: &str = "last-capture-error";

/// `<data dir>/last-capture-error`: one small file, overwritten, holding the
/// latest config failure the capture path fell back from.
pub fn capture_problem_note_path() -> Option<PathBuf> {
    crate::util::project_dirs().map(|dirs| dirs.data_dir().join(CAPTURE_PROBLEM_FILE))
}

/// Note `problem` for `suv doctor`. Best effort and cheap: the file is only
/// rewritten when the problem differs from the one already noted, so a
/// broken config costs one small read per recorded command, not a write.
pub fn record_capture_problem(problem: &CaptureConfigProblem) {
    if let Some(file) = capture_problem_note_path() {
        let now = chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
        let _ = write_capture_problem_note(&file, problem, &now);
    }
}

fn write_capture_problem_note(
    file: &std::path::Path,
    problem: &CaptureConfigProblem,
    at: &str,
) -> std::io::Result<()> {
    if read_capture_problem_note_at(file).is_some_and(|note| note.problem == *problem) {
        return Ok(());
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let contents = format!("{at}\n{}\n{}\n", problem.path.display(), problem.message);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    std::io::Write::write_all(&mut options.open(file)?, contents.as_bytes())
}

/// The note [`record_capture_problem`] left, if any.
pub fn read_capture_problem_note() -> Option<CaptureProblemNote> {
    read_capture_problem_note_at(&capture_problem_note_path()?)
}

fn read_capture_problem_note_at(file: &std::path::Path) -> Option<CaptureProblemNote> {
    let contents = std::fs::read_to_string(file).ok()?;
    let mut parts = contents.splitn(3, '\n');
    let at = parts.next()?.to_string();
    let path = PathBuf::from(parts.next()?);
    let message = parts.next()?.trim_end().to_string();
    Some(CaptureProblemNote {
        at,
        problem: CaptureConfigProblem { path, message },
    })
}

/// Whether the file `problem` names still fails to load today, so a note
/// about a file that has since been fixed (or deleted) is not reported.
pub fn capture_problem_persists(problem: &CaptureConfigProblem) -> bool {
    if get_config_path().is_ok_and(|global| global == problem.path) {
        return load_config().is_err();
    }
    if !problem.path.is_file() {
        return false;
    }
    let base = load_config().unwrap_or_default();
    problem
        .path
        .parent()
        .is_some_and(|dir| overlay_config_for_dir(&base, dir).is_err())
}

/// Validate config values after loading.
fn validate_config(config: &Config) -> ConfigResult<()> {
    if config.search.page_limit == 0 {
        return Err(ConfigError::Path(
            "search.page_limit must be at least 1".into(),
        ));
    }
    if config.search.page_limit > MAX_PAGE_LIMIT {
        return Err(ConfigError::Path(format!(
            "search.page_limit exceeds maximum of {MAX_PAGE_LIMIT}"
        )));
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
        // Exclusions match by substring, so a blank pattern would exclude
        // every command containing that whitespace.
        if pattern.trim().is_empty() {
            return Err(ConfigError::Path(
                "exclusion patterns must not be empty or whitespace".into(),
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

/// Serializes the tests that read or write the shared (temp-dir) config
/// file, which every test in the process points at.
#[cfg(test)]
static CONFIG_FILE_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Guard for tests that touch the on-disk config file.
#[cfg(test)]
pub fn config_file_test_lock() -> std::sync::MutexGuard<'static, ()> {
    CONFIG_FILE_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Sections of `Config` that are maps keyed by the user's own data (a path,
/// an agent name) rather than by field names. An entry missing from the
/// config being saved was deleted, so it is removed from the file too.
const CONFIG_MAP_SECTIONS: &[&str] = &["auto_tags", "agents"];

/// Serialize `config` for writing by editing `existing` in place.
///
/// `suv settings` edits one field at a time but saves the whole `Config`.
/// Re-serializing from scratch would drop every comment and every key the
/// struct has no field for (a hand-added key, or one from a different
/// suvadu version), so the existing document is edited with `toml_edit`
/// instead:
/// - every key `config` serializes is set to its new value, keeping the
///   comments and formatting around it (a value that has not changed is
///   left exactly as written);
/// - an entry of a map section ([`CONFIG_MAP_SECTIONS`]) that `config` no
///   longer has is removed, so a deleted auto-tag or agent stays deleted;
/// - anything else in the file — unknown keys, comments — is kept.
///
/// Lists are known values like any other: an emptied list is written as
/// empty, never merged with the old entries.
fn render_config_toml(config: &Config, existing: Option<&str>) -> ConfigResult<String> {
    let rendered = toml::to_string_pretty(config)?;
    let Some(existing) = existing else {
        return Ok(rendered);
    };
    // An unparseable existing file is replaced rather than merged: there is
    // nothing trustworthy to preserve, and refusing to save would strand the
    // user in the settings UI.
    let (Ok(mut doc), Ok(new)) = (
        existing.parse::<toml_edit::DocumentMut>(),
        rendered.parse::<toml_edit::DocumentMut>(),
    ) else {
        return Ok(rendered);
    };
    let new = new.into_table();
    for section in CONFIG_MAP_SECTIONS {
        let Some(old) = doc
            .get_mut(section)
            .and_then(toml_edit::Item::as_table_like_mut)
        else {
            continue;
        };
        let keep = new.get(section).and_then(toml_edit::Item::as_table_like);
        let removed: Vec<String> = old
            .iter()
            .map(|(key, _)| key.to_string())
            .filter(|key| !keep.is_some_and(|keep| keep.contains_key(key)))
            .collect();
        for key in removed {
            old.remove(&key);
        }
    }
    merge_edit_table(doc.as_table_mut(), new);
    Ok(doc.to_string())
}

/// Set every key of `new` in `target`, recursing into tables that both have
/// so that keys `new` does not mention (and their comments) are kept.
fn merge_edit_table(target: &mut dyn toml_edit::TableLike, new: toml_edit::Table) {
    for (key, item) in new {
        match target.get_mut(&key) {
            Some(slot) => merge_edit_item(slot, item),
            None => {
                target.insert(&key, without_positions(item));
            }
        }
    }
}

fn merge_edit_item(slot: &mut toml_edit::Item, new: toml_edit::Item) {
    let new = if slot.is_table_like() {
        match new.into_table() {
            Ok(table) => {
                if let Some(target) = slot.as_table_like_mut() {
                    merge_edit_table(target, table);
                }
                return;
            }
            Err(other) => other,
        }
    } else {
        new
    };
    if let Some(old) = slot.as_value_mut() {
        // A value stays a value (an inline table or array the user wrote
        // inline stays inline), keeping its trailing comment.
        if let Ok(mut value) = new.into_value() {
            if !same_toml_value(old, &value) {
                *value.decor_mut() = old.decor().clone();
                *old = value;
            }
        }
    } else {
        *slot = without_positions(new);
    }
}

/// Whether two values are equal as data, ignoring formatting and comments.
fn same_toml_value(a: &toml_edit::Value, b: &toml_edit::Value) -> bool {
    let parse = |v: &toml_edit::Value| {
        toml::from_str::<toml::Table>(&format!("v = {}", v.clone().decorated("", ""))).ok()
    };
    matches!((parse(a), parse(b)), (Some(a), Some(b)) if a == b)
}

/// A table taken from the freshly rendered document carries that
/// document's positions, which mean nothing in the one being edited; clear
/// them so new tables are placed after the existing ones.
fn without_positions(mut item: toml_edit::Item) -> toml_edit::Item {
    fn clear(table: &mut toml_edit::Table) {
        table.set_position(None);
        for (_, child) in table.iter_mut() {
            if let Some(child) = child.as_table_mut() {
                clear(child);
            } else if let Some(array) = child.as_array_of_tables_mut() {
                array.iter_mut().for_each(clear);
            }
        }
    }
    if let Some(table) = item.as_table_mut() {
        clear(table);
    } else if let Some(array) = item.as_array_of_tables_mut() {
        array.iter_mut().for_each(clear);
    }
    item
}

/// Save configuration to file atomically (temp file + rename).
pub fn save_config(config: &Config) -> ConfigResult<()> {
    let path = get_config_path()?;
    let contents = render_config_toml(config, std::fs::read_to_string(&path).ok().as_deref())?;
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

    /// Run `body` against the (temp-dir) config file, restoring whatever was
    /// there afterwards so parallel tests keep seeing a stable file.
    fn with_config_file<F: FnOnce(&std::path::Path)>(initial: &str, body: F) {
        let _guard = config_file_test_lock();
        let path = get_config_path().unwrap();
        let restore = std::fs::read_to_string(&path).ok();
        std::fs::write(&path, initial).unwrap();
        invalidate_cache();

        body(&path);

        match restore {
            Some(contents) => std::fs::write(&path, contents).unwrap(),
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
        invalidate_cache();
    }

    const EXISTING_CONFIG: &str = r#"
enabled = true
# A key this build does not know about (older/newer suvadu, hand-edited).
future_option = "keep me"

[search]
page_limit = 123

[redaction]
extra_patterns = ["AKIA[0-9A-Z]{16}"]

[mcp]
disabled_tools = ["assess_risk"]
exclude_dirs = ["/tmp/secret"]
unknown_mcp_key = 7
"#;

    #[test]
    fn save_keeps_unrelated_and_unknown_config_keys() {
        with_config_file(EXISTING_CONFIG, |path| {
            let mut config = load_config().expect("existing config must still load");
            // The one thing the settings UI changed.
            config.mcp.allow_session_summaries = true;
            save_config(&config).unwrap();

            let value: toml::Value = toml::from_str(&std::fs::read_to_string(path).unwrap())
                .expect("saved config must be valid TOML");
            assert_eq!(
                value["future_option"].as_str(),
                Some("keep me"),
                "an unknown top-level key must survive a save"
            );
            assert_eq!(
                value["mcp"]["unknown_mcp_key"].as_integer(),
                Some(7),
                "an unknown key inside a known table must survive a save"
            );

            let reloaded = load_config().unwrap();
            assert!(reloaded.mcp.allow_session_summaries);
            assert_eq!(reloaded.search.page_limit, 123);
            assert_eq!(reloaded.mcp.disabled_tools, vec!["assess_risk".to_string()]);
            assert_eq!(reloaded.mcp.exclude_dirs, vec!["/tmp/secret".to_string()]);
            assert_eq!(
                reloaded.redaction.extra_patterns,
                vec!["AKIA[0-9A-Z]{16}".to_string()]
            );
        });
    }

    #[test]
    fn save_still_applies_removals() {
        with_config_file(EXISTING_CONFIG, |path| {
            let mut config = load_config().unwrap();
            config.mcp.disabled_tools.clear();
            config.mcp.exclude_dirs.clear();
            save_config(&config).unwrap();

            let reloaded = load_config().unwrap();
            assert!(reloaded.mcp.disabled_tools.is_empty());
            assert!(reloaded.mcp.exclude_dirs.is_empty());
            // …without losing the keys nobody touched.
            let value: toml::Value =
                toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            assert_eq!(value["future_option"].as_str(), Some("keep me"));
        });
    }

    const COMMENTED_CONFIG: &str = r#"# My suvadu config — hand-maintained.
enabled = true
future_option = "keep me" # not known to this build
exclusions = [
    "^vault ", # never record vault logins
    "SECRETFILE",
]

# Search tuning.
[search]
page_limit = 123 # big screens
match_mode = "fuzzy"

[auto_tags]
"/work/a" = "alpha"
"/work/b" = "beta" # the second project

[agents.first]
env_var = "FIRST_AGENT"
executor_type = "agent"

[agents.second]
env_var = "SECOND_AGENT"
executor_type = "ide"
unknown_agent_key = 1
"#;

    #[test]
    fn save_drops_a_removed_auto_tag_and_agent() {
        with_config_file(COMMENTED_CONFIG, |_| {
            let mut config = load_config().unwrap();
            config.auto_tags.remove("/work/a");
            config.agents.remove("first");
            save_config(&config).unwrap();

            let reloaded = load_config().unwrap();
            assert!(
                !reloaded.auto_tags.contains_key("/work/a"),
                "removed tag came back"
            );
            assert!(
                !reloaded.agents.contains_key("first"),
                "removed agent came back"
            );
            assert_eq!(
                reloaded.auto_tags.get("/work/b").map(String::as_str),
                Some("beta")
            );
            assert_eq!(reloaded.agents["second"].env_var, "SECOND_AGENT");
        });
    }

    #[test]
    fn save_keeps_comments_and_unknown_keys_while_changing_values() {
        with_config_file(COMMENTED_CONFIG, |path| {
            let mut config = load_config().unwrap();
            config.search.page_limit = 200;
            config.auto_tags.insert("/work/c".into(), "gamma".into());
            config.agents.insert(
                "third".into(),
                CustomAgent {
                    env_var: "THIRD_AGENT".into(),
                    executor_type: "ci".into(),
                },
            );
            save_config(&config).unwrap();

            let saved = std::fs::read_to_string(path).unwrap();
            for comment in [
                "# My suvadu config — hand-maintained.",
                "# not known to this build",
                "# Search tuning.",
                "# big screens",
                "# never record vault logins",
                "# the second project",
            ] {
                assert!(saved.contains(comment), "lost {comment:?}:\n{saved}");
            }
            assert!(saved.contains("page_limit = 200 # big screens"), "{saved}");
            let value: toml::Value = toml::from_str(&saved).unwrap();
            assert_eq!(value["future_option"].as_str(), Some("keep me"));
            assert_eq!(
                value["agents"]["second"]["unknown_agent_key"].as_integer(),
                Some(1)
            );

            let reloaded = load_config().unwrap();
            assert_eq!(reloaded.search.page_limit, 200);
            assert_eq!(reloaded.auto_tags.len(), 3);
            assert_eq!(reloaded.agents["third"].executor_type, "ci");
            assert_eq!(reloaded.agents.len(), 3);
            assert_eq!(reloaded.exclusions, vec!["^vault ", "SECRETFILE"]);
        });
    }

    #[test]
    fn saving_twice_changes_nothing_the_second_time() {
        with_config_file(COMMENTED_CONFIG, |path| {
            save_config(&load_config().unwrap()).unwrap();
            let first = std::fs::read_to_string(path).unwrap();
            save_config(&load_config().unwrap()).unwrap();
            assert_eq!(std::fs::read_to_string(path).unwrap(), first);
        });
    }

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert!(config.enabled);
    }

    // ── Project overlay ──────────────────────────────────────

    #[test]
    fn find_project_overlay_none_when_absent() {
        let temp = TempDir::new().unwrap();
        let nested = temp.path().join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(find_project_overlay(&nested).is_none());
    }

    #[test]
    fn find_project_overlay_finds_file_in_start_dir() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join(".suvadu.toml"), "enabled = false").unwrap();
        let found = find_project_overlay(temp.path()).unwrap();
        assert_eq!(found, temp.path().join(".suvadu.toml"));
    }

    #[test]
    fn find_project_overlay_walks_up_from_a_child_directory() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join(".suvadu.toml"), "enabled = false").unwrap();
        let nested = temp.path().join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();
        let found = find_project_overlay(&nested).unwrap();
        assert_eq!(found, temp.path().join(".suvadu.toml"));
    }

    #[test]
    fn find_project_overlay_prefers_nearest_ancestor() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join(".suvadu.toml"), "enabled = false").unwrap();
        let nested = temp.path().join("project");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join(".suvadu.toml"), "enabled = true").unwrap();

        let found = find_project_overlay(&nested).unwrap();
        assert_eq!(found, nested.join(".suvadu.toml"));
    }

    #[test]
    fn merge_toml_scalar_overrides_base() {
        let mut base: toml::Value = toml::from_str("x = 1\ny = 2").unwrap();
        let overlay: toml::Value = toml::from_str("x = 99").unwrap();
        merge_toml_value(&mut base, overlay);
        assert_eq!(base.get("x").unwrap().as_integer(), Some(99));
        assert_eq!(base.get("y").unwrap().as_integer(), Some(2));
    }

    #[test]
    fn merge_toml_nested_table_merges_key_by_key() {
        let mut base: toml::Value = toml::from_str("[sub]\na = 1\nb = 2").unwrap();
        let overlay: toml::Value = toml::from_str("[sub]\na = 42").unwrap();
        merge_toml_value(&mut base, overlay);
        let sub = base.get("sub").unwrap();
        assert_eq!(sub.get("a").unwrap().as_integer(), Some(42));
        assert_eq!(sub.get("b").unwrap().as_integer(), Some(2));
    }

    #[test]
    fn merge_toml_array_is_replaced_wholesale_not_concatenated() {
        let mut base: toml::Value = toml::from_str("list = [1, 2, 3]").unwrap();
        let overlay: toml::Value = toml::from_str("list = [9]").unwrap();
        merge_toml_value(&mut base, overlay);
        let list = base.get("list").unwrap().as_array().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].as_integer(), Some(9));
    }

    #[test]
    fn merge_toml_key_absent_from_overlay_is_untouched() {
        let mut base: toml::Value = toml::from_str("x = 1\ny = 2").unwrap();
        let overlay: toml::Value = toml::from_str("z = 3").unwrap();
        merge_toml_value(&mut base, overlay);
        assert_eq!(base.get("x").unwrap().as_integer(), Some(1));
        assert_eq!(base.get("y").unwrap().as_integer(), Some(2));
        assert_eq!(base.get("z").unwrap().as_integer(), Some(3));
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
            val.is_some_and(|v| v == "1" || v.to_lowercase() == "true")
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
    fn recall_groups_identical_commands_unless_a_config_says_otherwise() {
        // No config, no [search] table, or a [search] table without the key:
        // recall opens in the Commands view.
        assert!(Config::default().search.show_unique_by_default);
        let no_table: Config = toml::from_str("enabled = true").unwrap();
        assert!(no_table.search.show_unique_by_default);
        let no_key: Config = toml::from_str("[search]\npage_limit = 20").unwrap();
        assert!(no_key.search.show_unique_by_default);

        // A value written in the file is kept, whichever it is.
        let every_run: Config = toml::from_str("[search]\nshow_unique_by_default = false").unwrap();
        assert!(!every_run.search.show_unique_by_default);
        let grouped: Config = toml::from_str("[search]\nshow_unique_by_default = true").unwrap();
        assert!(grouped.search.show_unique_by_default);
    }

    // ── Home preferences ─────────────────────────────────────

    #[test]
    fn home_preferences_default_to_home_and_ascii_when_unset() {
        for toml_str in ["", "enabled = true", "[home]"] {
            let config: Config = toml::from_str(toml_str).unwrap();
            assert_eq!(config.home.startup, None, "{toml_str:?}");
            assert_eq!(config.home.effective_startup(), HomeStartup::Home);
            assert_eq!(config.home.icons, HomeIcons::Ascii);
        }
        assert_eq!(Config::default().home, HomeConfig::default());
    }

    #[test]
    fn home_preferences_read_both_startup_screens_and_icon_sets() {
        let home: Config =
            toml::from_str("[home]\nstartup = \"home\"\nicons = \"unicode\"").unwrap();
        assert_eq!(home.home.startup, Some(HomeStartup::Home));
        assert_eq!(home.home.effective_startup(), HomeStartup::Home);
        assert_eq!(home.home.icons, HomeIcons::Unicode);

        let help: Config = toml::from_str("[home]\nstartup = \"help\"").unwrap();
        assert_eq!(help.home.startup, Some(HomeStartup::Help));
    }

    #[test]
    fn an_invalid_home_value_is_an_error_naming_the_choices() {
        let err = toml::from_str::<Config>("[home]\nstartup = \"sideways\"")
            .unwrap_err()
            .to_string();
        assert!(err.contains("sideways"), "{err}");
        assert!(err.contains("home") && err.contains("help"), "{err}");
    }

    /// `save_config` writes every known key. An unset startup screen must
    /// stay unset, or saving any setting would pin today's default and a
    /// later release could never change it for people who never chose.
    #[test]
    fn saving_other_settings_leaves_the_startup_screen_unset() {
        with_config_file("[search]\npage_limit = 40\n", |path| {
            let mut config = load_config().unwrap();
            config.search.page_limit = 41;
            save_config(&config).unwrap();

            let value: toml::Value =
                toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            assert!(
                value.get("home").and_then(|h| h.get("startup")).is_none(),
                "{value}"
            );
            assert_eq!(load_config().unwrap().home.startup, None);
        });
    }

    #[test]
    fn a_chosen_startup_screen_is_saved_beside_unknown_home_keys() {
        with_config_file("[home]\nstartup = \"help\"\nfuture = 1\n", |path| {
            let mut config = load_config().unwrap();
            assert_eq!(config.home.startup, Some(HomeStartup::Help));
            config.home.startup = Some(HomeStartup::Home);
            save_config(&config).unwrap();

            let value: toml::Value =
                toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            assert_eq!(value["home"]["startup"].as_str(), Some("home"));
            assert_eq!(value["home"]["future"].as_integer(), Some(1));
        });
    }

    /// Home preferences belong to the person, not the directory they happen
    /// to be in: a project's `.suvadu.toml` cannot change them — not even
    /// break them with an invalid value — while its other keys still apply.
    #[test]
    fn a_project_overlay_cannot_change_home_preferences() {
        let dir = TempDir::new().unwrap();
        let base = Config {
            home: HomeConfig {
                startup: Some(HomeStartup::Help),
                icons: HomeIcons::Ascii,
            },
            ..Config::default()
        };
        for overlay in [
            "[home]\nstartup = \"home\"\nicons = \"unicode\"\n[search]\npage_limit = 7\n",
            "[home]\nstartup = \"sideways\"\n[search]\npage_limit = 7\n",
        ] {
            std::fs::write(dir.path().join(".suvadu.toml"), overlay).unwrap();
            let merged = overlay_config_for_dir(&base, dir.path()).unwrap();
            assert_eq!(merged.home, base.home, "{overlay}");
            assert_eq!(merged.search.page_limit, 7, "{overlay}");
        }
    }

    #[test]
    fn update_checks_are_on_unless_turned_off() {
        for toml_str in ["", "[update]"] {
            let config: Config = toml::from_str(toml_str).unwrap();
            assert!(config.update.check, "{toml_str:?}");
        }
        assert!(Config::default().update.check);
        let off: Config = toml::from_str("[update]\ncheck = false").unwrap();
        assert!(!off.update.check);
    }

    /// Whether Suvadu contacts the network is the person's choice: a
    /// project's `.suvadu.toml` cannot turn update checks on or off.
    #[test]
    fn a_project_overlay_cannot_change_update_checks() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join(".suvadu.toml"),
            "[update]\ncheck = false\n[search]\npage_limit = 9\n",
        )
        .unwrap();
        let merged = overlay_config_for_dir(&Config::default(), dir.path()).unwrap();
        assert!(merged.update.check);
        assert_eq!(merged.search.page_limit, 9);
    }

    /// A `.suvadu.toml` arrives with any repository you clone, so it may
    /// only add privacy protection, never remove the person's own.
    #[test]
    fn a_project_overlay_cannot_turn_redaction_off() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join(".suvadu.toml"),
            "[redaction]\nenabled = false\n[search]\npage_limit = 9\n",
        )
        .unwrap();
        let merged = overlay_config_for_dir(&Config::default(), dir.path()).unwrap();
        assert!(merged.redaction.enabled);
        assert_eq!(merged.search.page_limit, 9, "other keys still apply");

        // Turning it on where the global config has it off is tightening.
        std::fs::write(
            dir.path().join(".suvadu.toml"),
            "[redaction]\nenabled = true\n",
        )
        .unwrap();
        let mut base = Config::default();
        base.redaction.enabled = false;
        assert!(
            overlay_config_for_dir(&base, dir.path())
                .unwrap()
                .redaction
                .enabled
        );
    }

    #[test]
    fn a_project_overlay_cannot_turn_recording_back_on() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(".suvadu.toml"), "enabled = true\n").unwrap();
        let base = Config {
            enabled: false,
            ..Config::default()
        };
        assert!(!overlay_config_for_dir(&base, dir.path()).unwrap().enabled);

        std::fs::write(dir.path().join(".suvadu.toml"), "enabled = false\n").unwrap();
        assert!(
            !overlay_config_for_dir(&Config::default(), dir.path())
                .unwrap()
                .enabled,
            "a project may still opt out of recording"
        );
    }

    #[test]
    fn git_branches_are_recorded_unless_turned_off() {
        assert!(Config::default().shell.capture_git_branch);
        let cfg: Config = toml::from_str("[shell]\nenable_arrow_navigation = false\n").unwrap();
        assert!(cfg.shell.capture_git_branch, "missing key means on");
        let cfg: Config = toml::from_str("[shell]\ncapture_git_branch = false\n").unwrap();
        assert!(!cfg.shell.capture_git_branch);
    }

    #[test]
    fn a_project_overlay_can_stop_recording_git_branches_but_not_restart_it() {
        let dir = TempDir::new().unwrap();
        let overlay = dir.path().join(".suvadu.toml");
        std::fs::write(&overlay, "[shell]\ncapture_git_branch = false\n").unwrap();
        assert!(
            !overlay_config_for_dir(&Config::default(), dir.path())
                .unwrap()
                .shell
                .capture_git_branch
        );

        std::fs::write(&overlay, "[shell]\ncapture_git_branch = true\n").unwrap();
        let mut base = Config::default();
        base.shell.capture_git_branch = false;
        assert!(
            !overlay_config_for_dir(&base, dir.path())
                .unwrap()
                .shell
                .capture_git_branch
        );
    }

    #[test]
    fn a_project_overlay_adds_to_privacy_lists_instead_of_replacing_them() {
        let dir = TempDir::new().unwrap();
        let mut base = Config {
            exclusions: vec!["secret-tool".into(), "shared".into()],
            ..Config::default()
        };
        base.redaction.extra_patterns = vec!["CORP-[0-9]{6}".into()];

        for overlay in [
            "exclusions = []\n[redaction]\nextra_patterns = []\n",
            "exclusions = [\"project-tool\", \"shared\"]\n\
             [redaction]\nextra_patterns = [\"PRJ-[a-z]{8}\"]\n",
        ] {
            std::fs::write(dir.path().join(".suvadu.toml"), overlay).unwrap();
            let merged = overlay_config_for_dir(&base, dir.path()).unwrap();
            for kept in &base.exclusions {
                assert!(merged.exclusions.contains(kept), "{overlay}: lost {kept}");
            }
            assert!(
                merged
                    .redaction
                    .extra_patterns
                    .contains(&"CORP-[0-9]{6}".to_string()),
                "{overlay}"
            );
        }

        // Entries the overlay adds are applied, once each.
        let merged = overlay_config_for_dir(&base, dir.path()).unwrap();
        assert_eq!(
            merged.exclusions,
            vec![
                "secret-tool".to_string(),
                "shared".to_string(),
                "project-tool".to_string()
            ]
        );
        assert_eq!(
            merged.redaction.extra_patterns,
            vec!["CORP-[0-9]{6}".to_string(), "PRJ-[a-z]{8}".to_string()]
        );
    }

    #[test]
    fn capture_uses_the_global_config_when_only_the_overlay_is_broken() {
        with_config_file("exclusions = [\"global-only\"]\n", |_| {
            let dir = TempDir::new().unwrap();
            let overlay = dir.path().join(".suvadu.toml");
            std::fs::write(&overlay, "[search]\nmatch_mode = \"sideways\"\n").unwrap();

            let (config, problem) = load_capture_config_for_dir(dir.path());
            assert_eq!(config.exclusions, vec!["global-only".to_string()]);
            let problem = problem.expect("the broken overlay must be reported");
            assert_eq!(problem.path, overlay);
            assert!(problem.message.contains("sideways"), "{}", problem.message);
        });
    }

    #[test]
    fn capture_uses_redacting_defaults_when_the_global_config_is_broken() {
        with_config_file(
            "[redaction]\nenabled = false\n[home]\nstartup = \"sideways\"\n",
            |path| {
                let dir = TempDir::new().unwrap();
                std::fs::write(dir.path().join(".suvadu.toml"), "exclusions = [\"prj\"]\n")
                    .unwrap();

                let (config, problem) = load_capture_config_for_dir(dir.path());
                assert!(config.redaction.enabled);
                assert_eq!(
                    config.exclusions,
                    vec!["prj".to_string()],
                    "overlay applies"
                );
                assert_eq!(problem.unwrap().path, path);
            },
        );
    }

    #[test]
    fn capture_reports_nothing_when_every_config_loads() {
        with_config_file("enabled = true\n", |_| {
            let dir = TempDir::new().unwrap();
            let (_, problem) = load_capture_config_for_dir(dir.path());
            assert!(problem.is_none());
        });
    }

    #[test]
    fn a_capture_problem_note_is_written_once_per_distinct_problem() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("data").join("last-capture-error");
        let first = CaptureConfigProblem {
            path: PathBuf::from("/p/.suvadu.toml"),
            message: "TOML parse error at line 1\n  |\n1 | x =\n  |    ^\ninvalid string".into(),
        };

        write_capture_problem_note(&file, &first, "2026-01-01T00:00:00+00:00").unwrap();
        write_capture_problem_note(&file, &first, "2026-01-02T00:00:00+00:00").unwrap();
        let note = read_capture_problem_note_at(&file).unwrap();
        assert_eq!(
            note.at, "2026-01-01T00:00:00+00:00",
            "same problem: not rewritten"
        );
        assert_eq!(note.problem, first);

        let second = CaptureConfigProblem {
            path: PathBuf::from("/q/.suvadu.toml"),
            message: "unknown variant".into(),
        };
        write_capture_problem_note(&file, &second, "2026-01-03T00:00:00+00:00").unwrap();
        let note = read_capture_problem_note_at(&file).unwrap();
        assert_eq!(note.at, "2026-01-03T00:00:00+00:00");
        assert_eq!(note.problem, second);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_fixed_overlay_is_no_longer_a_capture_problem() {
        let dir = TempDir::new().unwrap();
        let overlay = dir.path().join(".suvadu.toml");
        std::fs::write(&overlay, "[search]\nmatch_mode = \"sideways\"\n").unwrap();
        let problem = CaptureConfigProblem {
            path: overlay.clone(),
            message: String::new(),
        };
        assert!(capture_problem_persists(&problem));
        std::fs::write(&overlay, "[search]\nmatch_mode = \"fuzzy\"\n").unwrap();
        assert!(!capture_problem_persists(&problem));
        std::fs::remove_file(&overlay).unwrap();
        assert!(!capture_problem_persists(&problem));
    }

    #[test]
    fn a_blank_exclusion_pattern_is_invalid() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        for pattern in ["", " ", "  \t "] {
            std::fs::write(&path, format!("exclusions = [\"ok\", {pattern:?}]\n")).unwrap();
            let err = read_config_file(&path).unwrap_err().to_string();
            assert!(err.contains("exclusion"), "{pattern:?}: {err}");
        }
    }

    #[test]
    fn reading_a_missing_global_config_creates_nothing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("not-yet").join("config.toml");
        assert!(read_config_file(&path).unwrap().is_none());
        assert!(!dir.path().join("not-yet").exists());
    }

    #[test]
    fn reading_an_invalid_global_config_reports_it_and_leaves_it_alone() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let original = "enabled = true\n[home]\nstartup = \"sideways\"\n";
        std::fs::write(&path, original).unwrap();
        let err = read_config_file(&path).unwrap_err().to_string();
        assert!(err.contains("sideways"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);

        std::fs::write(&path, "[home]\nstartup = \"home\"\n").unwrap();
        let config = read_config_file(&path).unwrap().unwrap();
        assert_eq!(config.home.startup, Some(HomeStartup::Home));
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
        let toml_str = r"
enabled = true
";
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
        assert!(!config.mcp.allow_skill_proposals);
        assert!(!config.mcp.allow_session_summaries);
    }

    #[test]
    fn test_mcp_allow_session_summaries_deserialization() {
        let config: Config = toml::from_str("[mcp]\nallow_session_summaries = true\n").unwrap();
        assert!(config.mcp.allow_session_summaries);
        assert!(!config.mcp.allow_skill_proposals);
    }

    #[test]
    fn test_mcp_allow_skill_proposals_deserialization() {
        let toml_str = r"
[mcp]
allow_skill_proposals = true
";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.mcp.allow_skill_proposals);
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
        let toml_str = r"
enabled = true
[search]
page_limit = 100
";
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
