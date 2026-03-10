use std::sync::LazyLock;

use regex::Regex;

use crate::models::Entry;

/// Global pattern cache — compiled once, reused forever
static RISK_PATTERNS: LazyLock<Vec<RiskPattern>> = LazyLock::new(build_patterns);

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

/// Result of assessing risk for a single command
#[derive(Debug, Clone)]
pub struct RiskAssessment {
    pub level: RiskLevel,
    pub category: &'static str,
    pub description: &'static str,
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
            r"(^|\s)rm\s+.*(-rf|--recursive|-r\s+-f|-f\s+-r)",
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

    // Find the highest-risk matching pattern
    let mut best: Option<&RiskPattern> = None;

    for p in patterns {
        if p.regex.is_match(cmd) {
            match &best {
                Some(current) if p.level > current.level => best = Some(p),
                None => best = Some(p),
                _ => {}
            }
        }
    }

    best.map(|p| RiskAssessment {
        level: p.level,
        category: p.category,
        description: p.description,
    })
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
fn has_shell_chaining(cmd: &str) -> bool {
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
}
