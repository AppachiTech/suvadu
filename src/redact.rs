use std::sync::LazyLock;

use regex::Regex;

/// Placeholder that replaces detected secret values
const REDACTED: &str = "***REDACTED***";

/// Compiled secret patterns — built once, reused forever
static SECRET_PATTERNS: LazyLock<Vec<SecretPattern>> = LazyLock::new(build_patterns);

struct SecretPattern {
    regex: Regex,
    #[allow(dead_code)]
    category: &'static str,
    #[allow(dead_code)]
    description: &'static str,
}

/// (regex, category, description)
type PatternDef = (&'static str, &'static str, &'static str);

/// Redact secrets from a command string.
/// Returns the command with all detected secret values replaced by `***REDACTED***`.
pub fn redact_secrets(command: &str) -> String {
    let patterns = &*SECRET_PATTERNS;
    let mut result = command.to_string();

    for p in patterns {
        result = p
            .regex
            .replace_all(&result, |caps: &regex::Captures| {
                let prefix = caps.get(1).map_or("", |m| m.as_str());
                let suffix = caps.get(3).map_or("", |m| m.as_str());
                format!("{prefix}{REDACTED}{suffix}")
            })
            .to_string();
    }

    result
}

/// Check if a command contains any secrets (without redacting).
#[cfg(test)]
fn contains_secrets(command: &str) -> bool {
    let patterns = &*SECRET_PATTERNS;
    patterns.iter().any(|p| p.regex.is_match(command))
}

fn build_patterns() -> Vec<SecretPattern> {
    let defs = [
        env_var_patterns(),
        cli_password_patterns(),
        api_key_patterns(),
        auth_header_patterns(),
        connection_string_patterns(),
    ];

    defs.into_iter()
        .flatten()
        .filter_map(|(pat, cat, desc)| match Regex::new(pat) {
            Ok(regex) => Some(SecretPattern {
                regex,
                category: cat,
                description: desc,
            }),
            Err(e) => {
                eprintln!("suvadu: secret pattern failed to compile: {pat}: {e}");
                None
            }
        })
        .collect()
}

/// Environment variable assignments with sensitive names
/// Captures: group(1) = `VAR_NAME=`, group(2) = the secret value
fn env_var_patterns() -> Vec<PatternDef> {
    vec![
        // export SECRET_KEY=value or SECRET_KEY=value (inline)
        (
            r"(?i)((?:export\s+)?(?:\w*(?:SECRET|TOKEN|PASSWORD|PASSWD|API_KEY|API_SECRET|ACCESS_KEY|PRIVATE_KEY|AUTH|CREDENTIAL)\w*)=)(\S+)",
            "env-var",
            "Sensitive environment variable assignment",
        ),
    ]
}

/// CLI flags that take passwords
fn cli_password_patterns() -> Vec<PatternDef> {
    vec![
        // mysql -pPassword or mysql -p'password' or mysql -p"password"
        (
            r"(\s-p)([^\s-][^\s]*)",
            "cli-password",
            "MySQL-style inline password (-p)",
        ),
        // --password=value or --password value
        (
            r"(--password[=\s])(\S+)",
            "cli-password",
            "CLI --password flag",
        ),
        // --token=value or --token value
        (r"(--token[=\s])(\S+)", "cli-password", "CLI --token flag"),
        // --secret=value or --secret value
        (r"(--secret[=\s])(\S+)", "cli-password", "CLI --secret flag"),
        // --api-key=value or --apikey=value
        (
            r"(?i)(--api[-_]?key[=\s])(\S+)",
            "cli-password",
            "CLI --api-key flag",
        ),
    ]
}

/// Literal API key / token patterns (well-known prefixes)
fn api_key_patterns() -> Vec<PatternDef> {
    vec![
        // AWS Access Key ID (always starts with AKIA)
        (r"()(AKIA[0-9A-Z]{16})", "api-key", "AWS Access Key ID"),
        // GitHub tokens: ghp_, gho_, ghs_, ghr_, github_pat_
        (
            r"()(?:ghp_|gho_|ghs_|ghr_|github_pat_)[A-Za-z0-9_]{20,}",
            "api-key",
            "GitHub token",
        ),
        // OpenAI API key: sk-...
        (r"()(sk-[A-Za-z0-9]{20,})", "api-key", "OpenAI API key"),
        // Slack tokens: xoxb-, xoxp-, xoxo-, xoxa-
        (r"()(xox[bpoa]-[A-Za-z0-9-]+)", "api-key", "Slack token"),
        // Stripe keys: sk_live_, sk_test_, pk_live_, pk_test_
        (
            r"()([sr]k_(?:live|test)_[A-Za-z0-9]{20,})",
            "api-key",
            "Stripe API key",
        ),
        // Generic long hex secrets (32+ hex chars, common for API keys)
        // Only match when preceded by a key-like assignment
        (
            r"(?i)((?:SECRET|TOKEN|KEY|PASSWORD|AUTH|CREDENTIAL)\w*[=:]\s*)([0-9a-f]{32,})",
            "api-key",
            "Hex secret value",
        ),
    ]
}

/// Authorization headers in curl/wget/httpie commands
fn auth_header_patterns() -> Vec<PatternDef> {
    vec![
        // curl -H "Authorization: Bearer xxx"
        (
            r#"(?i)(-H\s*['"]?Authorization:\s*Bearer\s+)([^'"}\s]+)"#,
            "auth-header",
            "Bearer token in Authorization header",
        ),
        // curl -H "Authorization: Basic xxx"
        (
            r#"(?i)(-H\s*['"]?Authorization:\s*Basic\s+)([^'"}\s]+)"#,
            "auth-header",
            "Basic auth in Authorization header",
        ),
        // curl -H "Authorization: token xxx" (GitHub style)
        (
            r#"(?i)(-H\s*['"]?Authorization:\s*token\s+)([^'"}\s]+)"#,
            "auth-header",
            "Token in Authorization header",
        ),
        // curl -u user:password
        (
            r"(-u\s+\S+:)(\S+)",
            "auth-header",
            "Basic auth credentials (-u user:pass)",
        ),
    ]
}

/// Database connection strings with embedded passwords
fn connection_string_patterns() -> Vec<PatternDef> {
    vec![
        // postgresql://user:password@host  or  mysql://user:password@host
        (
            r"((?:postgres(?:ql)?|mysql|mongodb(?:\+srv)?|redis|amqp)://[^:]+:)([^@]+)(@)",
            "connection-string",
            "Database connection string with password",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_env_var_export() {
        let cmd = "export AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
        let redacted = redact_secrets(cmd);
        assert_eq!(redacted, "export AWS_SECRET_ACCESS_KEY=***REDACTED***");
        assert!(!redacted.contains("wJalrXUtnFEMI"));
    }

    #[test]
    fn test_env_var_inline() {
        let cmd = "GITHUB_TOKEN=ghp_abc123def456ghi789jk0 git push";
        let redacted = redact_secrets(cmd);
        assert!(redacted.contains("GITHUB_TOKEN=***REDACTED***"));
        assert!(!redacted.contains("ghp_abc123"));
    }

    #[test]
    fn test_bearer_token() {
        let cmd = r#"curl -H "Authorization: Bearer sk-abc123def456" https://api.example.com"#;
        let redacted = redact_secrets(cmd);
        assert!(redacted.contains("***REDACTED***"));
        assert!(!redacted.contains("sk-abc123def456"));
    }

    #[test]
    fn test_mysql_password() {
        let cmd = "mysql -u root -pMyP@ssw0rd mydb";
        let redacted = redact_secrets(cmd);
        assert!(redacted.contains("-p***REDACTED***"));
        assert!(!redacted.contains("MyP@ssw0rd"));
    }

    #[test]
    fn test_password_flag() {
        let cmd = "psql --password=SuperSecret123 -h localhost";
        let redacted = redact_secrets(cmd);
        assert!(redacted.contains("--password=***REDACTED***"));
        assert!(!redacted.contains("SuperSecret123"));
    }

    #[test]
    fn test_aws_access_key() {
        let cmd = "aws configure set aws_access_key_id AKIAIOSFODNN7EXAMPLE";
        let redacted = redact_secrets(cmd);
        assert!(redacted.contains("***REDACTED***"));
        assert!(!redacted.contains("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn test_github_token() {
        let cmd = "git clone https://ghp_aBcDeFgHiJkLmNoPqRsT1234@github.com/user/repo.git";
        let redacted = redact_secrets(cmd);
        assert!(redacted.contains("***REDACTED***"));
        assert!(!redacted.contains("ghp_aBcDeFgHiJkLmNoPqRsT1234"));
    }

    #[test]
    fn test_openai_key() {
        let cmd = "export OPENAI_API_KEY=sk-proj1234567890abcdefghijklmnop";
        let redacted = redact_secrets(cmd);
        assert!(!redacted.contains("sk-proj1234567890"));
    }

    #[test]
    fn test_connection_string() {
        let cmd = "psql postgresql://admin:s3cretP@ss@db.example.com:5432/mydb";
        let redacted = redact_secrets(cmd);
        assert!(redacted.contains("postgresql://admin:***REDACTED***@"));
        assert!(!redacted.contains("s3cretP@ss"));
    }

    #[test]
    fn test_no_false_positive_safe_command() {
        let cmd = "git status";
        assert_eq!(redact_secrets(cmd), "git status");
    }

    #[test]
    fn test_no_false_positive_ls() {
        let cmd = "ls -la /tmp";
        assert_eq!(redact_secrets(cmd), "ls -la /tmp");
    }

    #[test]
    fn test_no_false_positive_cd() {
        let cmd = "cd /home/user/projects";
        assert_eq!(redact_secrets(cmd), "cd /home/user/projects");
    }

    #[test]
    fn test_no_false_positive_grep() {
        let cmd = "grep -r 'password' src/";
        assert_eq!(redact_secrets(cmd), "grep -r 'password' src/");
    }

    #[test]
    fn test_contains_secrets() {
        assert!(contains_secrets("export SECRET_KEY=abc123"));
        assert!(!contains_secrets("git status"));
    }

    #[test]
    fn test_multiple_secrets_in_one_command() {
        let cmd =
            "AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE AWS_SECRET_ACCESS_KEY=wJalrXU/bPxRfiCY command";
        let redacted = redact_secrets(cmd);
        assert!(!redacted.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(!redacted.contains("wJalrXU"));
    }

    #[test]
    fn test_slack_token() {
        let cmd = "curl -H 'Authorization: Bearer xoxb-123-456-abc' https://slack.com/api/test";
        let redacted = redact_secrets(cmd);
        assert!(!redacted.contains("xoxb-123-456-abc"));
    }

    #[test]
    fn test_stripe_key() {
        let cmd = "stripe listen --api-key sk_test_1234567890abcdefghijklmnop";
        let redacted = redact_secrets(cmd);
        assert!(!redacted.contains("sk_test_1234567890"));
    }

    #[test]
    fn test_basic_auth_curl() {
        let cmd = "curl -u admin:s3cret https://api.example.com";
        let redacted = redact_secrets(cmd);
        assert!(redacted.contains("-u admin:***REDACTED***"));
        assert!(!redacted.contains("s3cret"));
    }

    #[test]
    fn test_connection_string_mongodb() {
        let cmd = "mongosh mongodb+srv://user:p@ssw0rd@cluster.example.com/db";
        let redacted = redact_secrets(cmd);
        assert!(!redacted.contains("p@ssw0rd"));
    }
}
