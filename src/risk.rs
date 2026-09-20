use std::sync::{LazyLock, OnceLock};

use regex::Regex;

use crate::models::Entry;

/// Global pattern cache — compiled once, reused forever
static RISK_PATTERNS: LazyLock<Vec<RiskPattern>> = LazyLock::new(build_patterns);

/// User-configured ignore patterns (`agent.risk_ignore_patterns`). Commands
/// matching any of these are treated as safe so users can suppress false
/// positives. Empty until `set_ignore_patterns` is called at startup, so tests
/// (which never call it) and the hot path are unaffected.
static RISK_IGNORE_PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();

/// Install user-configured risk-ignore regexes. Call once at startup. Invalid
/// patterns are skipped with a warning. A second call is a no-op (`OnceLock`).
pub fn set_ignore_patterns(patterns: &[String]) {
    if patterns.is_empty() {
        return;
    }
    let compiled: Vec<Regex> = patterns
        .iter()
        .filter_map(|p| match Regex::new(p) {
            Ok(re) => Some(re),
            Err(e) => {
                eprintln!("suvadu: invalid risk_ignore_pattern '{p}': {e}");
                None
            }
        })
        .collect();
    let _ = RISK_IGNORE_PATTERNS.set(compiled);
}

/// Whether a command matches any of the given ignore patterns (pure helper).
fn matches_any(cmd: &str, pats: &[Regex]) -> bool {
    pats.iter().any(|re| re.is_match(cmd))
}

/// Whether a command matches a user-configured ignore pattern.
fn is_ignored(cmd: &str) -> bool {
    RISK_IGNORE_PATTERNS
        .get()
        .is_some_and(|pats| matches_any(cmd, pats))
}

/// A compiled user-defined risk pattern (`agent.risk_extra_patterns`).
/// `description`/`level` are owned (unlike the built-in `RiskPattern`'s
/// `&'static str`s) since they come from user config, not a compile-time
/// literal.
struct ExtraRiskPattern {
    regex: Regex,
    level: RiskLevel,
    description: String,
}

/// User-configured additional risk patterns to flag — the complement to
/// `RISK_IGNORE_PATTERNS`, which only suppresses. Empty until
/// `set_extra_patterns` is called at startup.
static RISK_EXTRA_PATTERNS: OnceLock<Vec<ExtraRiskPattern>> = OnceLock::new();

/// Install user-configured extra risk patterns. Call once at startup. A
/// pattern with an invalid regex or an unparseable `level` is skipped with a
/// warning (matching `set_ignore_patterns`'s behavior for invalid regexes).
/// A second call is a no-op (`OnceLock`).
pub fn set_extra_patterns(patterns: &[crate::config::RiskPatternConfig]) {
    if patterns.is_empty() {
        return;
    }
    let compiled: Vec<ExtraRiskPattern> = patterns
        .iter()
        .filter_map(|p| {
            let regex = match Regex::new(&p.pattern) {
                Ok(re) => re,
                Err(e) => {
                    eprintln!("suvadu: invalid risk_extra_pattern '{}': {e}", p.pattern);
                    return None;
                }
            };
            let Ok(level) = p.level.parse::<RiskLevel>() else {
                eprintln!(
                    "suvadu: invalid risk_extra_pattern level '{}' for '{}' — \
                     expected low/medium/high/critical",
                    p.level, p.pattern
                );
                return None;
            };
            let description = if p.description.is_empty() {
                "Custom pattern".to_string()
            } else {
                p.description.clone()
            };
            Some(ExtraRiskPattern {
                regex,
                level,
                description,
            })
        })
        .collect();
    let _ = RISK_EXTRA_PATTERNS.set(compiled);
}

/// Risk severity levels for command classification
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskLevel {
    None,
    Low,
    Medium,
    High,
    Critical,
}

impl RiskLevel {
    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "safe",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }

    pub const fn icon(self) -> &'static str {
        match self {
            Self::None | Self::Low => "·",
            Self::Medium => "⚡",
            Self::High | Self::Critical => "⚠",
        }
    }

    pub const fn ansi_color(self) -> &'static str {
        match self {
            Self::None => "\x1b[0m",
            Self::Low => "\x1b[90m",        // dim
            Self::Medium => "\x1b[33m",     // yellow
            Self::High => "\x1b[38;5;208m", // orange
            Self::Critical => "\x1b[31m",   // red
        }
    }
}

impl std::str::FromStr for RiskLevel {
    type Err = ();

    /// Case-insensitive; matches `label()`'s output plus "none"/"safe".
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "none" | "safe" => Ok(Self::None),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "critical" => Ok(Self::Critical),
            _ => Err(()),
        }
    }
}

impl std::fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

/// A single risk pattern definition
struct RiskPattern {
    regex: Regex,
    level: RiskLevel,
    category: &'static str,
    description: &'static str,
}

/// How much the matched text actually settles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Certainty {
    /// The matched text *is* the operation: `rm -rf /srv` deletes `/srv`.
    MatchedText,
    /// The text names an indirection — a script to fetch, a package to
    /// install, a string to `eval` — so what actually runs is not in the
    /// command and cannot be judged from it.
    Unresolved,
}

/// The longest evidence snippet a report will show.
const MAX_EVIDENCE_CHARS: usize = 80;

/// Result of assessing risk for a single command. `category`/`description`
/// are `Cow` rather than `&'static str` because a match against a
/// user-configured `risk_extra_patterns` entry (see [`set_extra_patterns`])
/// carries an owned, config-supplied description — built-in matches still
/// borrow their `&'static str` literals at zero cost.
///
/// The three parts of a verdict are kept separate on purpose: `level` is the
/// rule's severity, `evidence` is the text that matched it, and `certainty`
/// says whether matching that text settles what the command does.
#[derive(Debug, Clone)]
pub struct RiskAssessment {
    pub level: RiskLevel,
    pub category: std::borrow::Cow<'static, str>,
    pub description: std::borrow::Cow<'static, str>,
    /// The matched span of the command, redacted and length-bounded.
    pub evidence: String,
    pub certainty: Certainty,
}

impl RiskAssessment {
    /// What this verdict cannot tell you, or `None` when the matched text is
    /// the whole story.
    pub const fn uncertainty(&self) -> Option<&'static str> {
        match self.certainty {
            Certainty::MatchedText => None,
            Certainty::Unresolved => Some(
                "what this actually runs is not in the command text, so the effect cannot be \
                 judged from the match alone",
            ),
        }
    }
}

/// Whether matching a rule in this category settles what the command does.
fn certainty_for(category: &str) -> Certainty {
    match category {
        // Fetched scripts, installed packages and dynamically built commands
        // all carry their real behaviour somewhere the command text isn't.
        "obfuscation" | "script-exec" | "package-install" => Certainty::Unresolved,
        _ => Certainty::MatchedText,
    }
}

/// Turn a matched span into evidence safe to print: secrets redacted (a risk
/// report must never be the thing that copies a token into a log) and length
/// bounded so one enormous command cannot flood the output.
fn evidence_from(cmd: &str, span: std::ops::Range<usize>) -> String {
    let matched = cmd.get(span).unwrap_or(cmd).trim();
    crate::util::truncate_str(
        &crate::redact::redact_secrets(matched),
        MAX_EVIDENCE_CHARS,
        "\u{2026}",
    )
}

/// Aggregate risk summary for a set of entries
#[derive(Debug, Clone, Default)]
pub struct SessionRisk {
    pub critical_count: usize,
    pub high_count: usize,
    pub medium_count: usize,
    pub low_count: usize,
    pub safe_count: usize,
    pub packages_installed: Vec<PackageInstall>,
    pub failed_commands: Vec<FailedCommand>,
}

#[derive(Debug, Clone)]
pub struct PackageInstall {
    pub manager: &'static str,
    pub packages: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct FailedCommand {
    pub command: String,
    pub exit_code: i32,
    pub executor: String,
    pub timestamp: i64,
}

/// Build the default risk pattern set (compiled once, reused)
fn build_patterns() -> Vec<RiskPattern> {
    let mut defs = Vec::with_capacity(40);
    defs.extend(critical_pattern_defs());
    defs.extend(high_pattern_defs());
    defs.extend(obfuscation_pattern_defs());
    defs.extend(medium_pattern_defs());
    defs.extend(low_pattern_defs());

    defs.into_iter()
        .filter_map(|(pat, level, cat, desc)| match Regex::new(pat) {
            Ok(regex) => Some(RiskPattern {
                regex,
                level,
                category: cat,
                description: desc,
            }),
            Err(e) => {
                eprintln!("suvadu: risk pattern failed to compile: {pat}: {e}");
                None
            }
        })
        .collect()
}

type PatternDef = (&'static str, RiskLevel, &'static str, &'static str);

fn critical_pattern_defs() -> Vec<PatternDef> {
    vec![
        (
            // A quote counts as a boundary: `bash -c "rm -rf /"` and
            // `ssh host 'rm -rf /srv'` really do delete, and a quote is the
            // usual way to wrap them. Quoted *mentions* (`grep "rm -rf"`)
            // are suppressed separately, by `is_quoted_mention`.
            r#"(^|\s|["'`(])rm\s+.*(-rf|--recursive|-r\s+-f|-f\s+-r)"#,
            RiskLevel::Critical,
            "destructive",
            "Recursive delete",
        ),
        (
            r"^git\s+push\s+.*--force(\s|$)",
            RiskLevel::Critical,
            "destructive",
            "Force push",
        ),
        (
            r"^git\s+reset\s+--hard",
            RiskLevel::Critical,
            "destructive",
            "Hard reset (discards changes)",
        ),
        (
            r"(?i)drop\s+(table|database|schema)",
            RiskLevel::Critical,
            "destructive",
            "SQL drop statement",
        ),
        (
            r">\s*/dev/sd",
            RiskLevel::Critical,
            "destructive",
            "Write to block device",
        ),
        (
            r"^mkfs\.",
            RiskLevel::Critical,
            "destructive",
            "Format filesystem",
        ),
        (
            r"^dd\s+.*of=/dev/",
            RiskLevel::Critical,
            "destructive",
            "Raw disk write",
        ),
    ]
}

fn high_pattern_defs() -> Vec<PatternDef> {
    vec![
        (
            r"^(npm|yarn|pnpm)\s+(install|add|i)\b",
            RiskLevel::High,
            "package-install",
            "JS package install",
        ),
        (
            r"^pip3?\s+install",
            RiskLevel::High,
            "package-install",
            "Python package install",
        ),
        (
            r"^cargo\s+(add|install)",
            RiskLevel::High,
            "package-install",
            "Rust package install",
        ),
        (
            r"^brew\s+install",
            RiskLevel::High,
            "package-install",
            "Homebrew install",
        ),
        (
            r"^gem\s+install",
            RiskLevel::High,
            "package-install",
            "Ruby gem install",
        ),
        (
            r"^go\s+(install|get)",
            RiskLevel::High,
            "package-install",
            "Go package install",
        ),
        (
            r"^apt(-get)?\s+install",
            RiskLevel::High,
            "package-install",
            "APT package install",
        ),
        (
            r"^chmod\s+(\+x|[0-7]*[1357][0-7]*)",
            RiskLevel::High,
            "permission",
            "Make executable",
        ),
        (
            r"curl\s+.*\|\s*(sh|bash|zsh)",
            RiskLevel::High,
            "script-exec",
            "Pipe curl to shell",
        ),
        (
            r"wget\s+.*\|\s*(sh|bash|zsh)",
            RiskLevel::High,
            "script-exec",
            "Pipe wget to shell",
        ),
        (
            r"\./[^\s]+\.sh\b",
            RiskLevel::High,
            "script-exec",
            "Execute shell script",
        ),
        (
            r"^bash\s+[^\s]+\.sh",
            RiskLevel::High,
            "script-exec",
            "Execute shell script via bash",
        ),
        (
            r"^sh\s+[^\s]+\.sh",
            RiskLevel::High,
            "script-exec",
            "Execute shell script via sh",
        ),
        // Irreversible operations an agent might run that previously slipped
        // through as "safe".
        (
            r"^git\s+clean\s+(-[a-zA-Z]*f|--force)",
            RiskLevel::High,
            "git",
            "Delete untracked files (git clean)",
        ),
        (
            // chmod 777 / other world-writable modes, including with flags like -R.
            r"^chmod\s+(-[a-zA-Z]+\s+)*[0-7]*[0-7][0-7][2367](\s|$)",
            RiskLevel::High,
            "permission",
            "World-writable permissions (chmod)",
        ),
        (
            r"^chown\s+-R\b",
            RiskLevel::High,
            "permission",
            "Recursive ownership change (chown -R)",
        ),
    ]
}

/// Obfuscation/indirection: regexes match literal command text, so anything
/// that constructs or hides the real command at runtime defeats every
/// pattern above it. These don't close that gap (no AST here), but they
/// flag the indirection itself as worth a second look — the same principle
/// as flagging `curl | sh` without needing to know what the downloaded
/// script does.
fn obfuscation_pattern_defs() -> Vec<PatternDef> {
    vec![
        (
            r"(?i)base64\s+(-d|--decode|-D)\b.*\|\s*(sh|bash|zsh)\b",
            RiskLevel::High,
            "obfuscation",
            "Decode base64 and pipe to a shell",
        ),
        (
            r"(^|\s)eval\s+\S",
            RiskLevel::High,
            "obfuscation",
            "eval executes a dynamically-built command — its real contents \
             can't be checked from the command text alone",
        ),
        (
            r"\$\([^)]*\b(rm\s+-rf|curl|wget|dd\s|mkfs|base64|eval)\b[^)]*\)",
            RiskLevel::High,
            "obfuscation",
            "Command substitution invokes a network/destructive command — \
             it runs before the outer command does",
        ),
    ]
}

fn medium_pattern_defs() -> Vec<PatternDef> {
    vec![
        (
            r"^sudo\s+",
            RiskLevel::Medium,
            "privilege",
            "Privilege escalation",
        ),
        (
            r"^docker\s+(rm|kill|stop|prune)",
            RiskLevel::Medium,
            "container",
            "Docker container modification",
        ),
        (
            r"^kill\s+",
            RiskLevel::Medium,
            "process",
            "Process termination",
        ),
        (
            r"^killall\s+",
            RiskLevel::Medium,
            "process",
            "Process termination (by name)",
        ),
        (r"^git\s+reset\s+", RiskLevel::Medium, "git", "Git reset"),
        (
            r"^git\s+checkout\s+--\s+\.",
            RiskLevel::Medium,
            "git",
            "Discard file changes",
        ),
        (
            r"^git\s+stash\s+drop",
            RiskLevel::Medium,
            "git",
            "Drop stashed changes",
        ),
        (
            r"^git\s+branch\s+-[dD]",
            RiskLevel::Medium,
            "git",
            "Delete git branch",
        ),
    ]
}

fn low_pattern_defs() -> Vec<PatternDef> {
    vec![
        (r"^curl\s+", RiskLevel::Low, "network", "HTTP request"),
        (r"^wget\s+", RiskLevel::Low, "network", "HTTP download"),
        (r"^ssh\s+", RiskLevel::Low, "network", "SSH connection"),
        (r"^scp\s+", RiskLevel::Low, "network", "Remote file copy"),
        (r"^rsync\s+", RiskLevel::Low, "network", "File sync"),
        (r"^git\s+push\s+", RiskLevel::Low, "git", "Push to remote"),
    ]
}

/// Assess the risk level of a single command
pub fn assess_risk(command: &str) -> Option<RiskAssessment> {
    let patterns = &*RISK_PATTERNS;
    let cmd = command.trim();

    // Skip commands that don't actually execute the matched operation
    if is_non_executing(cmd) {
        return None;
    }

    // Honor user-configured false-positive suppressions.
    if is_ignored(cmd) {
        return None;
    }

    // Find the highest-risk matching pattern across both the built-in set
    // and any user-configured `risk_extra_patterns` — a user pattern can
    // win (or lose to) a built-in one purely on severity, same as two
    // built-ins competing today.
    let mut best: Option<RiskAssessment> = None;

    for p in patterns {
        let Some(m) = p.regex.find(cmd) else { continue };
        if is_quoted_mention(cmd, m.range()) {
            continue;
        }
        if best.as_ref().is_none_or(|current| p.level > current.level) {
            best = Some(RiskAssessment {
                level: p.level,
                category: std::borrow::Cow::Borrowed(p.category),
                description: std::borrow::Cow::Borrowed(p.description),
                evidence: evidence_from(cmd, m.range()),
                certainty: certainty_for(p.category),
            });
        }
    }

    if let Some(extra) = RISK_EXTRA_PATTERNS.get() {
        for p in extra {
            let Some(m) = p.regex.find(cmd) else { continue };
            if is_quoted_mention(cmd, m.range()) {
                continue;
            }
            if best.as_ref().is_none_or(|current| p.level > current.level) {
                best = Some(RiskAssessment {
                    level: p.level,
                    category: std::borrow::Cow::Borrowed("custom"),
                    description: std::borrow::Cow::Owned(p.description.clone()),
                    evidence: evidence_from(cmd, m.range()),
                    // A user-supplied regex says what its author meant it to
                    // say; suvadu cannot vouch for what the match implies.
                    certainty: Certainty::Unresolved,
                });
            }
        }
    }

    best
}

/// Programs whose arguments are text to search for, print or record — not
/// text to run. `git commit -m "drop table users"` writes a message; it does
/// not drop a table.
fn is_text_only_program(cmd: &str) -> bool {
    const PROGRAMS: &[&str] = &[
        "grep ",
        "egrep ",
        "fgrep ",
        "rg ",
        "ag ",
        "ack ",
        "git grep ",
        "git commit ",
        "git log ",
        "man ",
        "history ",
    ];
    PROGRAMS.iter().any(|p| cmd.starts_with(p))
}

/// Byte ranges of `cmd` that sit inside single or double quotes.
fn quoted_spans(cmd: &str) -> Vec<std::ops::Range<usize>> {
    let bytes = cmd.as_bytes();
    let mut spans = Vec::new();
    let mut open: Option<(u8, usize)> = None;
    for (i, &b) in bytes.iter().enumerate() {
        match open {
            Some((quote, start)) => {
                if b == quote {
                    spans.push(start..i);
                    open = None;
                }
            }
            None => {
                if b == b'\'' || b == b'"' {
                    open = Some((b, i + 1));
                }
            }
        }
    }
    spans
}

/// Whether a matched span is only a *mention* of a dangerous command: quoted
/// text handed to a program that reads or records strings, in a command line
/// that chains nothing.
///
/// Deliberately narrow. `bash -c "rm -rf /"`, `psql -c "drop table users"`
/// and `grep foo . ; rm -rf /tmp` all execute their quoted text, so none of
/// them qualify: the program must be one that cannot run its argument, and a
/// single `&&`, `|`, `;` or `$(…)` anywhere disqualifies the whole line.
fn is_quoted_mention(cmd: &str, span: std::ops::Range<usize>) -> bool {
    if !is_text_only_program(cmd) || has_shell_chaining(cmd) {
        return false;
    }
    // A pattern may consume its left boundary (the quote or the space before
    // the command word), which would place the span just outside the quoted
    // range it actually sits in.
    let matched = cmd.get(span.clone()).unwrap_or_default();
    let trimmed = matched.trim_start_matches(['"', '\'', '`', '(', ' ', '\t']);
    let start = span.start + (matched.len() - trimmed.len());
    quoted_spans(cmd)
        .into_iter()
        .any(|quoted| quoted.start <= start && span.end <= quoted.end)
}

/// Returns true if the command doesn't actually execute the matched operation.
/// Catches false positives like comments, echo output, and alias definitions.
fn is_non_executing(cmd: &str) -> bool {
    // Shell comments
    if cmd.starts_with('#') {
        return true;
    }

    // echo/printf — output only, unless chained to another command
    if (cmd.starts_with("echo ") || cmd.starts_with("printf ")) && !has_shell_chaining(cmd) {
        return true;
    }

    // alias definitions — setting up an alias, not executing the aliased command
    if cmd.starts_with("alias ") {
        return true;
    }

    false
}

/// Check if the command contains shell operators that chain execution.
/// Quote-aware: ignores operators inside single or double-quoted strings.
const fn has_shell_chaining(cmd: &str) -> bool {
    let bytes = cmd.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;

    while i < len {
        let b = bytes[i];

        // Backslash escaping (not inside single quotes — bash single quotes are fully literal)
        if b == b'\\' && !in_single && i + 1 < len {
            i += 2;
            continue;
        }

        // Toggle single quote state (ignored inside double quotes)
        if b == b'\'' && !in_double {
            in_single = !in_single;
            i += 1;
            continue;
        }

        // Toggle double quote state (ignored inside single quotes)
        if b == b'"' && !in_single {
            in_double = !in_double;
            i += 1;
            continue;
        }

        // Command substitution executes even inside double quotes — only
        // single quotes are fully literal in bash. So `echo "$(rm -rf /)"`
        // still runs `rm -rf /` before echo ever sees its argument.
        if !in_single && b == b'`' {
            return true;
        }
        if !in_single && b == b'$' && i + 1 < len && bytes[i + 1] == b'(' {
            return true;
        }

        // Only detect operators when outside all quotes
        if !in_single && !in_double {
            // Semicolon (no surrounding spaces required)
            if b == b';' {
                return true;
            }

            if b == b' ' {
                // " | " — pipe (3 chars)
                if i + 2 < len && bytes[i + 1] == b'|' && bytes[i + 2] == b' ' {
                    return true;
                }

                // " && " — logical and (4 chars)
                if i + 3 < len
                    && bytes[i + 1] == b'&'
                    && bytes[i + 2] == b'&'
                    && bytes[i + 3] == b' '
                {
                    return true;
                }

                // " || " — logical or (4 chars)
                if i + 3 < len
                    && bytes[i + 1] == b'|'
                    && bytes[i + 2] == b'|'
                    && bytes[i + 3] == b' '
                {
                    return true;
                }
            }
        }

        i += 1;
    }

    false
}

/// Get the risk level for a command (convenience wrapper)
pub fn risk_level(command: &str) -> RiskLevel {
    assess_risk(command).map_or(RiskLevel::None, |a| a.level)
}

/// Compute aggregate risk summary for a set of entries
pub fn session_risk(entries: &[Entry]) -> SessionRisk {
    let mut result = SessionRisk::default();

    for entry in entries {
        let level = risk_level(&entry.command);
        match level {
            RiskLevel::Critical => result.critical_count += 1,
            RiskLevel::High => result.high_count += 1,
            RiskLevel::Medium => result.medium_count += 1,
            RiskLevel::Low => result.low_count += 1,
            RiskLevel::None => result.safe_count += 1,
        }

        // Extract package installs
        if let Some(pkg) = extract_packages(&entry.command) {
            result.packages_installed.push(pkg);
        }

        // Track failures
        if let Some(code) = entry.exit_code {
            if code != 0 {
                result.failed_commands.push(FailedCommand {
                    command: entry.command.clone(),
                    exit_code: code,
                    executor: entry.executor.clone().unwrap_or_default(),
                    timestamp: entry.started_at,
                });
            }
        }
    }

    result
}

/// Best-effort extraction of package names from install commands
pub fn extract_packages(command: &str) -> Option<PackageInstall> {
    let cmd = command.trim();

    // npm/yarn/pnpm install <packages>
    if let Some(rest) = strip_prefix_any(
        cmd,
        &[
            "npm install ",
            "npm i ",
            "yarn add ",
            "pnpm add ",
            "pnpm install ",
        ],
    ) {
        let packages = parse_package_args(rest);
        if packages.is_empty() {
            return None;
        }
        return Some(PackageInstall {
            manager: "npm",
            packages,
        });
    }

    // pip install <packages>
    if let Some(rest) = strip_prefix_any(cmd, &["pip install ", "pip3 install "]) {
        let packages = parse_package_args(rest);
        if packages.is_empty() {
            return None;
        }
        return Some(PackageInstall {
            manager: "pip",
            packages,
        });
    }

    // cargo add <packages>
    if let Some(rest) = strip_prefix_any(cmd, &["cargo add ", "cargo install "]) {
        let packages = parse_package_args(rest);
        if packages.is_empty() {
            return None;
        }
        return Some(PackageInstall {
            manager: "cargo",
            packages,
        });
    }

    // brew install <packages>
    if let Some(rest) = strip_prefix_any(cmd, &["brew install "]) {
        let packages = parse_package_args(rest);
        if packages.is_empty() {
            return None;
        }
        return Some(PackageInstall {
            manager: "brew",
            packages,
        });
    }

    // gem install <packages>
    if let Some(rest) = strip_prefix_any(cmd, &["gem install "]) {
        let packages = parse_package_args(rest);
        if packages.is_empty() {
            return None;
        }
        return Some(PackageInstall {
            manager: "gem",
            packages,
        });
    }

    // go install / go get
    if let Some(rest) = strip_prefix_any(cmd, &["go install ", "go get "]) {
        let packages = parse_package_args(rest);
        if packages.is_empty() {
            return None;
        }
        return Some(PackageInstall {
            manager: "go",
            packages,
        });
    }

    None
}

fn strip_prefix_any<'a>(s: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    for prefix in prefixes {
        if let Some(rest) = s.strip_prefix(prefix) {
            return Some(rest);
        }
    }
    None
}

/// Parse space-separated package names, skipping flags (--save-dev, -D, etc.)
fn parse_package_args(args: &str) -> Vec<String> {
    args.split_whitespace()
        .filter(|a| !a.starts_with('-'))
        .filter(|a| !a.is_empty())
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // matches_any takes &[Regex] (it backs real user-configured ignore patterns,
    // which are always regexes); a literal pattern here is still exercising that
    // Regex-based path, not a plain string comparison, so `==` isn't equivalent.
    #[allow(clippy::trivial_regex)]
    fn test_ignore_patterns_suppress_match() {
        // The pure matcher backs is_ignored(); a matching ignore pattern makes
        // assess_risk return None for that command. (Tested without touching the
        // process-global OnceLock so parallel risk tests stay hermetic.)
        let pats = vec![Regex::new(r"^rm -rf /tmp/build$").unwrap()];
        assert!(matches_any("rm -rf /tmp/build", &pats));
        assert!(!matches_any("rm -rf /etc", &pats));
        // Without ignores configured (the test default), risk is still assessed.
        assert_eq!(risk_level("rm -rf /tmp/build"), RiskLevel::Critical);
    }

    #[test]
    fn test_set_ignore_patterns_skips_invalid_regex() {
        // Invalid regexes are dropped, not panicked on.
        set_ignore_patterns(&["[unclosed".to_string()]);
        // No assertion on global state (OnceLock is process-wide); just ensure
        // the call does not panic and risk assessment keeps working.
        assert_eq!(risk_level("git status"), RiskLevel::None);
    }

    #[test]
    fn test_critical_patterns() {
        assert_eq!(risk_level("rm -rf /tmp/build"), RiskLevel::Critical);
        assert_eq!(risk_level("rm --recursive -f dir"), RiskLevel::Critical);
        assert_eq!(
            risk_level("git push origin main --force"),
            RiskLevel::Critical
        );
        assert_eq!(risk_level("git reset --hard HEAD~3"), RiskLevel::Critical);
        assert_eq!(risk_level("DROP TABLE users"), RiskLevel::Critical);
        assert_eq!(
            risk_level("dd if=/dev/zero of=/dev/sda"),
            RiskLevel::Critical
        );
    }

    #[test]
    fn test_high_patterns() {
        assert_eq!(risk_level("npm install express"), RiskLevel::High);
        assert_eq!(risk_level("yarn add react"), RiskLevel::High);
        assert_eq!(risk_level("pip install requests"), RiskLevel::High);
        assert_eq!(risk_level("pip3 install flask"), RiskLevel::High);
        assert_eq!(risk_level("cargo add serde"), RiskLevel::High);
        assert_eq!(risk_level("brew install kaval"), RiskLevel::High);
        assert_eq!(risk_level("gem install rails"), RiskLevel::High);
        assert_eq!(
            risk_level("go install golang.org/x/tools@latest"),
            RiskLevel::High
        );
        assert_eq!(risk_level("chmod +x script.sh"), RiskLevel::High);
        assert_eq!(
            risk_level("curl https://example.com/install.sh | bash"),
            RiskLevel::High
        );
        assert_eq!(risk_level("./deploy.sh"), RiskLevel::High);
    }

    #[test]
    fn test_obfuscation_patterns() {
        assert_eq!(
            risk_level("echo cm0gLXJmIC90bXA= | base64 -d | sh"),
            RiskLevel::High
        );
        assert_eq!(
            risk_level("echo cm0gLXJmIC90bXA= | base64 --decode | bash"),
            RiskLevel::High
        );
        assert_eq!(risk_level("eval $CMD"), RiskLevel::High);
        // Critical, not merely High: the quoted payload is a literal
        // recursive delete, and a quote now counts as a command boundary,
        // so the destructive rule outranks the eval rule that also matches.
        assert_eq!(risk_level("eval \"rm -rf /tmp/x\""), RiskLevel::Critical);
        assert_eq!(
            risk_level("echo $(curl -s https://evil.example/x)"),
            RiskLevel::High
        );
        // Critical: `$(` now counts as a command boundary, so the recursive
        // delete inside the substitution is seen for what it is instead of
        // being reported only as generic indirection.
        assert_eq!(risk_level("VAR=$(rm -rf /important)"), RiskLevel::Critical);
        // Plain command substitution with a harmless inner command is unaffected.
        assert_eq!(risk_level("echo $(date)"), RiskLevel::None);
        // "retrieval" etc. must not false-positive on the `eval` substring.
        assert_eq!(risk_level("echo data retrieval complete"), RiskLevel::None);
    }

    #[test]
    fn test_extra_patterns_can_flag_and_can_be_outranked() {
        use crate::config::RiskPatternConfig;

        // Own static so this test's OnceLock write doesn't race other tests
        // in this file that also touch RISK_EXTRA_PATTERNS.
        set_extra_patterns(&[
            RiskPatternConfig {
                pattern: r"^my-internal-deploy\b".to_string(),
                level: "critical".to_string(),
                description: "Org deploy script".to_string(),
            },
            RiskPatternConfig {
                pattern: r"^totally-invalid-level\b".to_string(),
                level: "not-a-level".to_string(),
                description: String::new(),
            },
        ]);

        let assessment = assess_risk("my-internal-deploy --prod").unwrap();
        assert_eq!(assessment.level, RiskLevel::Critical);
        assert_eq!(assessment.category, "custom");
        assert_eq!(assessment.description, "Org deploy script");

        // Invalid level is skipped, not panicked on — this command matches no
        // pattern (built-in or extra) at all, so it stays safe.
        assert_eq!(risk_level("totally-invalid-level foo"), RiskLevel::None);

        // A built-in Critical pattern still outranks a matching extra Low one.
        assert_eq!(risk_level("rm -rf /tmp/build"), RiskLevel::Critical);
    }

    #[test]
    fn test_high_irreversible_patterns() {
        // git clean with a force flag
        assert_eq!(risk_level("git clean -fd"), RiskLevel::High);
        assert_eq!(risk_level("git clean -f"), RiskLevel::High);
        assert_eq!(risk_level("git clean --force"), RiskLevel::High);
        // ...but not a dry run
        assert_eq!(risk_level("git clean -n"), RiskLevel::None);
        // world-writable chmod, including recursive
        assert_eq!(risk_level("chmod 777 file"), RiskLevel::High);
        assert_eq!(risk_level("chmod -R 777 dir"), RiskLevel::High);
        assert_eq!(risk_level("chmod 666 file"), RiskLevel::High);
        // recursive chown
        assert_eq!(risk_level("chown -R user:group /srv"), RiskLevel::High);
    }

    #[test]
    fn test_medium_patterns() {
        assert_eq!(risk_level("sudo apt update"), RiskLevel::Medium);
        assert_eq!(risk_level("docker rm container123"), RiskLevel::Medium);
        assert_eq!(risk_level("kill 12345"), RiskLevel::Medium);
        assert_eq!(risk_level("git reset HEAD~1"), RiskLevel::Medium);
        assert_eq!(risk_level("git branch -D feature"), RiskLevel::Medium);
    }

    #[test]
    fn test_low_patterns() {
        assert_eq!(
            risk_level("curl https://api.example.com/data"),
            RiskLevel::Low
        );
        assert_eq!(risk_level("ssh user@host"), RiskLevel::Low);
        assert_eq!(risk_level("git push origin main"), RiskLevel::Low);
    }

    #[test]
    fn test_safe_patterns() {
        assert_eq!(risk_level("ls -la"), RiskLevel::None);
        assert_eq!(risk_level("cat README.md"), RiskLevel::None);
        assert_eq!(risk_level("grep -r pattern src/"), RiskLevel::None);
        assert_eq!(risk_level("git status"), RiskLevel::None);
        assert_eq!(risk_level("git diff"), RiskLevel::None);
        assert_eq!(risk_level("cargo test"), RiskLevel::None);
        assert_eq!(risk_level("npm test"), RiskLevel::None);
        assert_eq!(risk_level("echo hello"), RiskLevel::None);
    }

    #[test]
    fn test_highest_risk_wins() {
        // "sudo rm -rf" matches both critical (rm -rf) and medium (sudo)
        let assessment = assess_risk("sudo rm -rf /tmp").unwrap();
        assert_eq!(assessment.level, RiskLevel::Critical);
    }

    #[test]
    fn test_package_extraction_npm() {
        let pkg = extract_packages("npm install express body-parser").unwrap();
        assert_eq!(pkg.manager, "npm");
        assert_eq!(pkg.packages, vec!["express", "body-parser"]);
    }

    #[test]
    fn test_package_extraction_npm_with_flags() {
        let pkg = extract_packages("npm install --save-dev jest @types/jest").unwrap();
        assert_eq!(pkg.manager, "npm");
        assert_eq!(pkg.packages, vec!["jest", "@types/jest"]);
    }

    #[test]
    fn test_package_extraction_pip() {
        let pkg = extract_packages("pip install flask gunicorn").unwrap();
        assert_eq!(pkg.manager, "pip");
        assert_eq!(pkg.packages, vec!["flask", "gunicorn"]);
    }

    #[test]
    fn test_package_extraction_cargo() {
        let pkg = extract_packages("cargo add serde tokio").unwrap();
        assert_eq!(pkg.manager, "cargo");
        assert_eq!(pkg.packages, vec!["serde", "tokio"]);
    }

    #[test]
    fn test_package_extraction_none() {
        assert!(extract_packages("git status").is_none());
        assert!(extract_packages("ls -la").is_none());
    }

    #[test]
    fn test_session_risk_aggregate() {
        let entries = vec![
            make_entry("npm install express", Some(0)),
            make_entry("cat package.json", Some(0)),
            make_entry("npm test", Some(1)),
            make_entry("git push origin main", Some(0)),
            make_entry("rm -rf build/", Some(0)),
        ];

        let risk = session_risk(&entries);
        assert_eq!(risk.critical_count, 1); // rm -rf
        assert_eq!(risk.high_count, 1); // npm install
        assert_eq!(risk.low_count, 1); // git push
        assert_eq!(risk.safe_count, 2); // cat, npm test
        assert_eq!(risk.failed_commands.len(), 1); // npm test exit 1
        assert_eq!(risk.packages_installed.len(), 1); // express
    }

    #[test]
    fn test_all_patterns_compile() {
        // Verify that all regex patterns compile successfully and none are silently dropped
        let patterns = &*RISK_PATTERNS;
        assert!(
            patterns.len() >= 33,
            "Expected at least 33 risk patterns, got {}. Some patterns may have failed to compile.",
            patterns.len()
        );
    }

    // ── False positive tests ────────────────────────────────────────────

    #[test]
    fn test_echo_rm_is_safe() {
        // echo just prints text — not destructive
        assert_eq!(risk_level(r#"echo "rm -rf /""#), RiskLevel::None);
        assert_eq!(risk_level("echo rm -rf /tmp"), RiskLevel::None);
        assert_eq!(risk_level("printf 'rm -rf /'"), RiskLevel::None);
    }

    #[test]
    fn test_echo_with_chaining_is_risky() {
        // echo piped/chained to something else — could be dangerous
        assert_ne!(
            risk_level("echo test && rm -rf /"),
            RiskLevel::None,
            "Chained commands should still be assessed"
        );
    }

    #[test]
    fn test_alias_definition_is_safe() {
        assert_eq!(risk_level("alias rm='rm -i'"), RiskLevel::None);
        assert_eq!(risk_level("alias gp='git push --force'"), RiskLevel::None);
    }

    #[test]
    fn test_comment_is_safe() {
        assert_eq!(risk_level("# rm -rf /tmp"), RiskLevel::None);
        assert_eq!(risk_level("# sudo apt install foo"), RiskLevel::None);
    }

    #[test]
    fn test_force_with_lease_is_not_critical() {
        // --force-with-lease is the safe variant of --force
        assert_ne!(
            risk_level("git push --force-with-lease"),
            RiskLevel::Critical,
            "force-with-lease should not trigger critical force-push"
        );
        // But plain --force is still critical
        assert_eq!(
            risk_level("git push origin main --force"),
            RiskLevel::Critical
        );
        assert_eq!(
            risk_level("git push --force origin main"),
            RiskLevel::Critical
        );
    }

    #[test]
    fn test_is_non_executing() {
        assert!(is_non_executing("# this is a comment"));
        assert!(is_non_executing("echo hello world"));
        assert!(is_non_executing("printf 'test'"));
        assert!(is_non_executing("alias ll='ls -la'"));

        assert!(!is_non_executing("rm -rf /tmp"));
        assert!(!is_non_executing("echo test && rm -rf /"));
        assert!(!is_non_executing("echo test | sh"));
        assert!(!is_non_executing("git push --force"));
    }

    // ── has_shell_chaining quote-awareness tests ────────────────────────

    #[test]
    fn test_chaining_command_substitution() {
        // Command substitution executes even inside double quotes or with
        // no quotes at all — only single quotes are fully literal in bash.
        assert!(has_shell_chaining("echo $(rm -rf /tmp)"));
        assert!(has_shell_chaining(r#"echo "$(rm -rf /tmp)""#));
        assert!(has_shell_chaining("echo `rm -rf /tmp`"));
        assert!(has_shell_chaining(r#"echo "`rm -rf /tmp`""#));
        // Single-quoted: bash treats this fully literally, nothing runs.
        assert!(!has_shell_chaining("echo '$(rm -rf /tmp)'"));
        assert!(!has_shell_chaining("echo '`rm -rf /tmp`'"));
        // A bare '$' with no following '(' is not substitution.
        assert!(!has_shell_chaining("echo price is $5"));
    }

    #[test]
    fn test_chaining_unquoted_pipe() {
        assert!(has_shell_chaining("echo test | sh"));
        assert!(has_shell_chaining("cat file | grep foo"));
    }

    #[test]
    fn test_chaining_unquoted_and() {
        assert!(has_shell_chaining("echo test && rm -rf /"));
        assert!(has_shell_chaining("make && make install"));
    }

    #[test]
    fn test_chaining_unquoted_or() {
        assert!(has_shell_chaining("test -f file || exit 1"));
    }

    #[test]
    fn test_chaining_unquoted_semicolon() {
        assert!(has_shell_chaining("cd /tmp; rm -rf build"));
        assert!(has_shell_chaining("echo hi;echo bye"));
    }

    #[test]
    fn test_chaining_none() {
        assert!(!has_shell_chaining("echo hello world"));
        assert!(!has_shell_chaining("ls -la"));
        assert!(!has_shell_chaining("git status"));
    }

    #[test]
    fn test_chaining_double_quoted_pipe_ignored() {
        assert!(!has_shell_chaining(r#"echo "hello | world""#));
        assert!(!has_shell_chaining(r#"echo "a | b | c""#));
    }

    #[test]
    fn test_chaining_single_quoted_pipe_ignored() {
        assert!(!has_shell_chaining("echo 'hello | world'"));
    }

    #[test]
    fn test_chaining_double_quoted_and_ignored() {
        assert!(!has_shell_chaining(r#"echo "foo && bar""#));
    }

    #[test]
    fn test_chaining_single_quoted_and_ignored() {
        assert!(!has_shell_chaining("echo 'foo && bar'"));
    }

    #[test]
    fn test_chaining_double_quoted_semicolon_ignored() {
        assert!(!has_shell_chaining(r#"echo "hello;world""#));
    }

    #[test]
    fn test_chaining_single_quoted_semicolon_ignored() {
        assert!(!has_shell_chaining("echo 'hello;world'"));
    }

    #[test]
    fn test_chaining_mixed_quoted_and_unquoted() {
        // Unquoted pipe after a quoted section → should detect
        assert!(has_shell_chaining(r#"echo "safe text" | sh"#));
        // Pipe only inside quotes → should NOT detect
        assert!(!has_shell_chaining(r#"echo "a | b" c"#));
    }

    #[test]
    fn test_chaining_escaped_quote_inside_double_quotes() {
        // Escaped quote doesn't end the double-quoted string
        assert!(!has_shell_chaining(r#"echo "it\"s a | test""#));
    }

    #[test]
    fn test_chaining_single_quote_inside_double_quotes() {
        // Single quote inside double quotes is literal
        assert!(!has_shell_chaining(r#"echo "it's a | test""#));
    }

    #[test]
    fn test_chaining_double_quote_inside_single_quotes() {
        // Double quote inside single quotes is literal
        assert!(!has_shell_chaining(r#"echo '"hello | world"'"#));
    }

    #[test]
    fn test_chaining_echo_with_quoted_operators_is_safe() {
        // These were false positives before the fix
        assert_eq!(risk_level(r#"echo "rm -rf / | sh""#), RiskLevel::None);
        assert_eq!(risk_level("echo 'foo && bar'"), RiskLevel::None);
        assert_eq!(risk_level(r#"echo "test;done""#), RiskLevel::None);
        assert_eq!(risk_level(r#"printf "a | b""#), RiskLevel::None);
    }

    #[test]
    fn test_chaining_echo_with_real_chain_still_detected() {
        // Real chaining after echo — the chained part contains a dangerous pattern
        assert_ne!(risk_level("echo ok && rm -rf /"), RiskLevel::None);
        assert_ne!(risk_level("echo ok; rm -rf /"), RiskLevel::None);
        // echo piped to sh: not flagged because no pattern matches "echo X | sh"
        // (only curl/wget | sh are patterned), but chaining IS correctly detected
        assert!(has_shell_chaining("echo done | sh"));
    }

    fn make_entry(command: &str, exit_code: Option<i32>) -> Entry {
        Entry {
            id: None,
            session_id: "test".into(),
            command: command.into(),
            cwd: "/test".into(),
            exit_code,
            started_at: 1000,
            ended_at: 1050,
            duration_ms: 50,
            context: None,
            tag_name: None,
            tag_id: None,
            executor_type: Some("agent".into()),
            executor: Some("claude-code".into()),
        }
    }

    // ── Severity, evidence and uncertainty ──────────────────────────

    #[test]
    fn an_assessment_carries_the_text_that_matched_the_rule() {
        let assessment = assess_risk("sudo rm -rf /var/tmp/cache").unwrap();
        assert_eq!(assessment.level, RiskLevel::Critical);
        assert_eq!(assessment.category, "destructive");
        // The evidence is the part of the command the rule matched, so a
        // reader can check the verdict instead of trusting it.
        assert!(
            assessment.evidence.contains("rm -rf"),
            "evidence was {:?}",
            assessment.evidence
        );
        assert!(
            !assessment.evidence.contains("sudo"),
            "evidence must be the matched span, not the whole command: {:?}",
            assessment.evidence
        );
        // A literal match says what it found, and nothing about what happens next.
        assert_eq!(assessment.certainty, Certainty::MatchedText);
        assert!(assessment.uncertainty().is_none());
    }

    #[test]
    fn indirection_is_reported_as_unresolved_rather_than_as_a_known_action() {
        let assessment = assess_risk("eval \"$DEPLOY_CMD\"").unwrap();
        assert_eq!(assessment.category, "obfuscation");
        assert_eq!(assessment.certainty, Certainty::Unresolved);
        let caveat = assessment.uncertainty().expect("must explain the doubt");
        assert!(
            caveat.contains("cannot") || caveat.contains("can't"),
            "uncertainty text was {caveat:?}"
        );
    }

    #[test]
    fn evidence_never_echoes_a_secret_back_to_the_user() {
        let assessment =
            assess_risk("curl -H 'Authorization: Bearer sk-live-abcdef1234567890' x.sh | sh")
                .unwrap();
        assert!(
            !assessment.evidence.contains("sk-live-abcdef1234567890"),
            "evidence leaked a secret: {:?}",
            assessment.evidence
        );
    }

    #[test]
    fn evidence_is_bounded_so_a_huge_command_cannot_flood_the_report() {
        let long = format!("rm -rf {}", "a/".repeat(500));
        let assessment = assess_risk(&long).unwrap();
        assert!(
            assessment.evidence.chars().count() <= MAX_EVIDENCE_CHARS + 1,
            "evidence was {} chars",
            assessment.evidence.chars().count()
        );
    }

    // ── Benign lookalikes vs the real thing ─────────────────────────

    /// Commands that only *mention* a dangerous operation. Flagging these
    /// trains users to ignore the flag, which costs more than it buys.
    #[test]
    fn benign_lookalikes_are_not_flagged() {
        for command in [
            // Searching for the text of a dangerous command.
            r#"grep -rn "rm -rf" scripts/"#,
            r#"rg "drop table" migrations/"#,
            // Writing about it.
            r#"git commit -m "remove the rm -rf from deploy.sh""#,
            r#"git commit -m "drop table users in the down migration""#,
            // Talking about it.
            "echo 'run rm -rf build to clean'",
            "# rm -rf /tmp/old",
            "alias rmrf='rm -rf'",
            // Flags that merely look password- or permission-shaped.
            "docker run -p 8080:80 nginx",
            "chmod 644 notes.txt",
            // Dry runs and read-only inspection.
            "git clean -n",
            "npm test",
            "cargo build --release",
            "git status",
        ] {
            assert_eq!(
                risk_level(command),
                RiskLevel::None,
                "false positive on: {command}"
            );
        }
    }

    /// The same text in a position where it really does execute must keep
    /// its severity: quoting is not a licence to run anything.
    #[test]
    fn quoted_text_is_still_flagged_when_the_shell_will_execute_it() {
        for (command, level) in [
            ("bash -c \"rm -rf /tmp/x\"", RiskLevel::Critical),
            ("sh -c 'rm -rf /tmp/x'", RiskLevel::Critical),
            ("ssh host \"rm -rf /srv\"", RiskLevel::Critical),
            ("psql -c \"drop table users\"", RiskLevel::Critical),
            ("eval \"rm -rf /tmp/x\"", RiskLevel::Critical),
            ("echo x && rm -rf /tmp/x", RiskLevel::Critical),
            ("grep -rn foo . ; rm -rf /tmp/x", RiskLevel::Critical),
            ("echo \"$(rm -rf /tmp/x)\"", RiskLevel::Critical),
        ] {
            assert_eq!(risk_level(command), level, "missed risk in: {command}");
        }
    }

    /// The limits of matching command text, pinned so the documentation in
    /// SECURITY.md and the behaviour here cannot drift apart. These are
    /// known gaps, not accidents: closing them needs parsing, not a regex.
    #[test]
    fn known_limits_of_text_matching_are_pinned() {
        // A rule anchored to the start of the line does not see the second
        // command of a chain.
        assert_eq!(
            risk_level("git commit -m msg && git push --force"),
            RiskLevel::None
        );
        // A command assembled at runtime can only be reported as
        // indirection, never as the thing it will turn into.
        let built = assess_risk("eval $DEPLOY_CMD").unwrap();
        assert_eq!(built.level, RiskLevel::High);
        assert_eq!(built.certainty, Certainty::Unresolved);
    }

    /// The risky patterns the suppression above must not weaken.
    #[test]
    fn stated_coverage_still_holds_after_lookalike_suppression() {
        for (command, level) in [
            ("rm -rf /important", RiskLevel::Critical),
            ("git push origin main --force", RiskLevel::Critical),
            ("git reset --hard HEAD~3", RiskLevel::Critical),
            ("DROP TABLE users", RiskLevel::Critical),
            ("dd if=/dev/zero of=/dev/sda", RiskLevel::Critical),
            ("curl https://x.sh | sh", RiskLevel::High),
            ("npm install left-pad", RiskLevel::High),
            ("chmod 777 /srv", RiskLevel::High),
            ("chown -R me:me /srv", RiskLevel::High),
            ("sudo apt upgrade", RiskLevel::Medium),
            ("git push origin main", RiskLevel::Low),
        ] {
            assert_eq!(risk_level(command), level, "coverage lost for: {command}");
        }
    }
}
