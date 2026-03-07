use crate::models::AliasSuggestion;
use crate::{repository, suggest_ui};

/// Run the user's shell to get current alias definitions.
fn get_shell_aliases() -> String {
    // Try zsh first, then bash
    for shell in &["zsh", "bash"] {
        if let Ok(output) = std::process::Command::new(shell)
            .args(["-ic", "alias"])
            .output()
        {
            if output.status.success() {
                return String::from_utf8_lossy(&output.stdout).to_string();
            }
        }
    }
    String::new()
}

/// Parse alias output into (set of alias values/commands, set of alias names).
/// Handles zsh format `name='value'` and bash format `alias name='value'`.
fn parse_alias_output(
    input: &str,
) -> (
    std::collections::HashSet<String>,
    std::collections::HashSet<String>,
) {
    let mut values = std::collections::HashSet::new();
    let mut names = std::collections::HashSet::new();

    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Strip leading "alias " (bash format)
        let line = line.strip_prefix("alias ").unwrap_or(line);

        if let Some(eq_pos) = line.find('=') {
            let name = line[..eq_pos].trim();
            let value = line[eq_pos + 1..].trim();
            // Remove surrounding quotes from value
            let value = value
                .strip_prefix('\'')
                .and_then(|v| v.strip_suffix('\''))
                .or_else(|| value.strip_prefix('"').and_then(|v| v.strip_suffix('"')))
                .unwrap_or(value);

            names.insert(name.to_string());
            values.insert(value.to_string());
        }
    }

    (values, names)
}

/// Generate a short alias name from a command string, avoiding collisions.
fn generate_alias_name(command: &str, taken: &std::collections::HashSet<String>) -> String {
    let words: Vec<&str> = command
        .split_whitespace()
        .filter(|w| !w.starts_with('-'))
        .collect();

    if words.is_empty() {
        return "cmd".to_string();
    }

    // Strategy 1: First letter of each word (dcu for "docker compose up")
    let s1: String = words.iter().filter_map(|w| w.chars().next()).collect();
    if !s1.is_empty() && !taken.contains(&s1) {
        return s1;
    }

    // Strategy 2: First letter of word 1 + first 2 of word 2
    if words.len() >= 2 {
        let s2 = format!(
            "{}{}",
            &words[0].chars().next().unwrap_or('x'),
            &words[1].chars().take(2).collect::<String>()
        );
        if !taken.contains(&s2) {
            return s2;
        }
    }

    // Strategy 3: First 2 of word 1 + first letters of rest
    if words.len() >= 2 {
        let prefix: String = words[0].chars().take(2).collect();
        let rest: String = words[1..].iter().filter_map(|w| w.chars().next()).collect();
        let s3 = format!("{prefix}{rest}");
        if !taken.contains(&s3) {
            return s3;
        }
    }

    // Strategy 4: Append incrementing digit to strategy 1
    for i in 2.. {
        let s4 = format!("{s1}{i}");
        if !taken.contains(&s4) {
            return s4;
        }
    }

    unreachable!("infinite iterator always finds a free suffix")
}

/// Escape a string for use in a shell alias value (single-quoted).
pub fn shell_quote(s: &str) -> String {
    // In single-quoted strings, replace ' with '\''
    s.replace('\'', "'\\''")
}

/// Build suggestions from history, filtering out already-aliased commands.
/// When `human_only` is true, only commands executed by humans are considered.
pub fn build_suggestions(
    min_count: usize,
    min_length: usize,
    days: Option<usize>,
    top: usize,
    human_only: bool,
) -> Result<(Vec<AliasSuggestion>, Vec<String>), Box<dyn std::error::Error>> {
    let repo = repository::Repository::init()?;

    let frequent =
        repo.get_frequent_commands_filtered(days, min_count, min_length, top * 3, human_only)?;

    let alias_output = get_shell_aliases();
    let (alias_values, mut alias_names) = parse_alias_output(&alias_output);

    let mut suggestions = Vec::new();
    let mut skipped = Vec::new();

    for (cmd, count, dir_count) in frequent {
        // Skip single-word commands
        let word_count = cmd.split_whitespace().count();
        if word_count < 2 {
            continue;
        }

        // Skip commands that are already aliased
        if alias_values.contains(&cmd) {
            skipped.push(cmd);
            continue;
        }

        let name = generate_alias_name(&cmd, &alias_names);
        alias_names.insert(name.clone());

        suggestions.push(AliasSuggestion {
            name,
            command: cmd,
            count,
            dir_count,
            selected: true,
        });

        if suggestions.len() >= top {
            break;
        }
    }

    Ok((suggestions, skipped))
}

pub fn handle_suggest_aliases_text(
    min_count: usize,
    min_length: usize,
    days: Option<usize>,
    top: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let (suggestions, skipped) = build_suggestions(min_count, min_length, days, top, false)?;

    if suggestions.is_empty() {
        let period = days.map_or_else(|| "all time".to_string(), |d| format!("last {d} days"));
        println!("No alias suggestions found.");
        println!("  Criteria: min {min_count} uses, min {min_length} chars, {period}");
        return Ok(());
    }

    println!("\n\x1b[1m── Suggested Aliases ──────────────────────────────\x1b[0m\n");

    let max_name_len = suggestions.iter().map(|s| s.name.len()).max().unwrap_or(4);
    for s in &suggestions {
        let dir_info = if s.dir_count > 1 {
            format!(", {} dirs", s.dir_count)
        } else {
            String::new()
        };
        println!(
            "  alias \x1b[36m{:<width$}\x1b[0m='{}'\x1b[90m  # {} uses{dir_info}\x1b[0m",
            s.name,
            shell_quote(&s.command),
            s.count,
            width = max_name_len
        );
    }

    if !skipped.is_empty() {
        println!(
            "\n\x1b[90m  Skipped (already aliased): {}\x1b[0m",
            skipped.join(", ")
        );
    }

    println!("\n\x1b[2m  Add these to your ~/.zshrc or ~/.bashrc\x1b[0m\n");

    Ok(())
}

pub fn handle_suggest_aliases_tui(
    min_count: usize,
    min_length: usize,
    days: Option<usize>,
    top: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let (suggestions, skipped) = build_suggestions(min_count, min_length, days, top, false)?;

    if suggestions.is_empty() {
        let period = days.map_or_else(|| "all time".to_string(), |d| format!("last {d} days"));
        println!("No alias suggestions found.");
        println!("  Criteria: min {min_count} uses, min {min_length} chars, {period}");
        return Ok(());
    }

    let mut guard = crate::util::TerminalGuard::new()?;
    let res = suggest_ui::run_suggest_ui(guard.terminal(), suggestions, skipped);
    drop(guard);

    match res {
        Ok(Some(selected)) => {
            if selected.is_empty() {
                println!("No aliases selected.");
            } else {
                println!();
                for s in &selected {
                    println!("alias {}='{}'", s.name, shell_quote(&s.command));
                }
                println!("\n\x1b[2m  Add these to your ~/.zshrc or ~/.bashrc\x1b[0m\n");
            }
        }
        Ok(None) => {
            // User quit without confirming
        }
        Err(e) => {
            eprintln!("Error in suggest UI: {e}");
        }
    }

    Ok(())
}

/// Run the suggest TUI and return the user's selected suggestions (or None if quit).
/// When `human_only` is true, only human-executed commands are considered.
pub fn run_suggest_and_select(
    min_count: usize,
    min_length: usize,
    days: Option<usize>,
    top: usize,
    human_only: bool,
) -> Result<Option<Vec<AliasSuggestion>>, Box<dyn std::error::Error>> {
    let (suggestions, skipped) = build_suggestions(min_count, min_length, days, top, human_only)?;

    if suggestions.is_empty() {
        let period = days.map_or_else(|| "all time".to_string(), |d| format!("last {d} days"));
        println!("No alias suggestions found.");
        println!("  Criteria: min {min_count} uses, min {min_length} chars, {period}");
        return Ok(None);
    }

    let mut guard = crate::util::TerminalGuard::new()?;
    let res = suggest_ui::run_suggest_ui(guard.terminal(), suggestions, skipped);
    drop(guard);

    match res {
        Ok(result) => Ok(result),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_alias_output_zsh() {
        let input = "gst='git status'\nll='ls -la'\nglog='git log --oneline'\n";
        let (values, names) = parse_alias_output(input);
        assert!(values.contains("git status"));
        assert!(values.contains("ls -la"));
        assert!(values.contains("git log --oneline"));
        assert!(names.contains("gst"));
        assert!(names.contains("ll"));
        assert!(names.contains("glog"));
    }

    #[test]
    fn test_parse_alias_output_bash() {
        let input = "alias gst='git status'\nalias ll='ls -la'\n";
        let (values, names) = parse_alias_output(input);
        assert!(values.contains("git status"));
        assert!(values.contains("ls -la"));
        assert!(names.contains("gst"));
        assert!(names.contains("ll"));
    }

    #[test]
    fn test_parse_alias_output_mixed_quotes() {
        let input = "foo=\"bar baz\"\nqux='hello world'\n";
        let (values, _names) = parse_alias_output(input);
        assert!(values.contains("bar baz"));
        assert!(values.contains("hello world"));
    }

    #[test]
    fn test_generate_alias_name() {
        let taken = std::collections::HashSet::new();
        // docker compose up → dcu
        assert_eq!(generate_alias_name("docker compose up", &taken), "dcu");
        // git log --oneline → gl (flags skipped)
        assert_eq!(generate_alias_name("git log --oneline", &taken), "gl");
        // cargo build --release → cb
        assert_eq!(generate_alias_name("cargo build --release", &taken), "cb");
    }

    #[test]
    fn test_generate_alias_name_collision() {
        let mut taken = std::collections::HashSet::new();
        taken.insert("dcu".to_string());
        // First choice "dcu" is taken, falls back to "dco" (d + first 2 of "compose")
        let name = generate_alias_name("docker compose up", &taken);
        assert_eq!(name, "dco");
    }

    #[test]
    fn test_generate_alias_name_all_collisions() {
        let mut taken = std::collections::HashSet::new();
        taken.insert("dcu".to_string());
        taken.insert("dco".to_string());
        taken.insert("docu".to_string());
        // All 3 strategies taken, falls back to digit suffix
        let name = generate_alias_name("docker compose up", &taken);
        assert_eq!(name, "dcu2");
    }

    #[test]
    fn test_generate_alias_name_high_suffix() {
        let mut taken = std::collections::HashSet::new();
        taken.insert("dcu".to_string());
        taken.insert("dco".to_string());
        taken.insert("docu".to_string());
        // Take suffixes 2 through 100
        for i in 2..=100 {
            taken.insert(format!("dcu{i}"));
        }
        let name = generate_alias_name("docker compose up", &taken);
        assert_eq!(name, "dcu101");
    }

    #[test]
    fn test_shell_quote_plain() {
        assert_eq!(shell_quote("git status"), "git status");
    }

    #[test]
    fn test_shell_quote_with_single_quotes() {
        assert_eq!(
            shell_quote("echo 'hello world'"),
            "echo '\\''hello world'\\''"
        );
    }

    #[test]
    fn test_shell_quote_empty() {
        assert_eq!(shell_quote(""), "");
    }

    #[test]
    fn test_shell_quote_special_chars() {
        // Dollar signs and backticks are safe inside single quotes — they are literal
        // shell_quote only needs to escape single quotes themselves
        let input = "echo $HOME `whoami`";
        let quoted = shell_quote(input);
        // No single quotes in input, so output should be identical
        assert_eq!(quoted, input);

        // Now test a string with single quotes AND special chars
        let input2 = "echo '$HOME' `date`";
        let quoted2 = shell_quote(input2);
        assert!(
            quoted2.contains("\\'"),
            "Should escape single quotes: {quoted2}"
        );
    }

    #[test]
    fn test_generate_alias_name_single_word() {
        let taken = std::collections::HashSet::new();
        // Single-word commands (with no flags) should use first letter
        let name = generate_alias_name("make", &taken);
        assert_eq!(name, "m", "Single-word command should use first letter");
    }
}
