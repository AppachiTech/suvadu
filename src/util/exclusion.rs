use regex::Regex;

/// A pre-compiled exclusion pattern: either a valid regex or a literal substring.
pub enum CompiledExclusion {
    Regex(Regex),
    Substring(String),
}

/// Compile exclusion patterns once for reuse across multiple `is_excluded` calls.
/// Invalid regex patterns fall back to substring matching with a warning.
pub fn compile_exclusions(patterns: &[String]) -> Vec<CompiledExclusion> {
    patterns
        .iter()
        .map(|p| {
            Regex::new(p).map_or_else(
                |e| {
                    eprintln!("suvadu: invalid exclusion regex '{p}', using substring match: {e}");
                    CompiledExclusion::Substring(p.clone())
                },
                CompiledExclusion::Regex,
            )
        })
        .collect()
}

/// Check if a command matches any of the pre-compiled exclusion patterns.
pub fn is_excluded_compiled(command: &str, exclusions: &[CompiledExclusion]) -> bool {
    for pattern in exclusions {
        match pattern {
            CompiledExclusion::Regex(re) => {
                if re.is_match(command) {
                    return true;
                }
            }
            CompiledExclusion::Substring(s) => {
                if command.contains(s.as_str()) {
                    return true;
                }
            }
        }
    }
    false
}

/// Check if a command matches any of the exclusion patterns.
/// Patterns are treated as Regex first, falling back to substring match if invalid regex.
/// Convenience wrapper that compiles exclusions on each call.
#[cfg(test)]
pub fn is_excluded(command: &str, exclusions: &[String]) -> bool {
    let compiled = compile_exclusions(exclusions);
    is_excluded_compiled(command, &compiled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_excluded() {
        let exclusions = vec![
            "^ls$".to_string(),                 // Strict regex
            "password".to_string(),             // Substring (also valid regex)
            "*.log".to_string(), // Invalid regex (glob-like), falls back to substring
            "^git (commit|status)".to_string(), // Complex regex
        ];

        // Strict Regex Match
        assert!(is_excluded("ls", &exclusions));
        assert!(!is_excluded("ls -la", &exclusions)); // Regex ^ls$ doesn't match start/end

        // Substring Match (valid regex "password")
        assert!(is_excluded("echo password123", &exclusions));
        assert!(!is_excluded("echo pass", &exclusions));

        // Substring Fallback (invalid regex "*.log")
        assert!(!is_excluded("tail -f app.log", &exclusions)); // "log" is substring? verify "*.log"
                                                               // Wait, "*.log" IS invalid regex because * cannot start.
                                                               // So it falls back to substring check: command.contains("*.log").
                                                               // "tail -f app.log" does NOT contain "*.log".
                                                               // So this should be FALSE unless command has literal "*.log".
        assert!(!is_excluded("tail -f app.log", &exclusions));
        assert!(is_excluded("rm *.log", &exclusions)); // This contains literally "*.log"

        // Complex Regex
        assert!(is_excluded("git commit -m 'fix'", &exclusions));
        assert!(is_excluded("git status", &exclusions));
        assert!(!is_excluded("git add .", &exclusions));

        // Edge Cases
        let empty: Vec<String> = vec![];
        assert!(!is_excluded("ls", &empty));

        let bad_regex = vec!["[".to_string()]; // Invalid regex
        assert!(is_excluded("this has [ inside", &bad_regex)); // Literal match
        assert!(!is_excluded("normal string", &bad_regex));
    }
}
