use std::sync::LazyLock;

use regex::Regex;

/// Placeholder that replaces detected secret values
const REDACTED: &str = "***REDACTED***";

/// Compiled secret patterns — built once, reused forever
static SECRET_PATTERNS: LazyLock<Vec<SecretPattern>> = LazyLock::new(build_patterns);

struct SecretPattern {
    regex: Regex,
}

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
        .filter_map(|pat| match Regex::new(pat) {
            Ok(regex) => Some(SecretPattern { regex }),
            Err(e) => {
                eprintln!("suvadu: secret pattern failed to compile: {pat}: {e}");
                None
            }
        })
        .collect()
}

/// Environment variable assignments with sensitive names.
///
/// The keyword must appear as the **final segment** of the variable name
/// (delimited by underscores) so that names like `AUTHOR_NAME`,
/// `TOKENIZERS_PARALLELISM`, or `PASSWORD_FILE` are not false-positived.
///
/// Captures: group(1) = `VAR_NAME=`, group(2) = the secret value
fn env_var_patterns() -> Vec<&'static str> {
    vec![
        // Matches: GITHUB_TOKEN=, MY_SECRET=, DB_PASSWORD=, export AUTH=, AWS_SECRET_ACCESS_KEY=, etc.
        // Rejects: AUTHOR_NAME=, TOKEN_BUCKET_SIZE=, SECRET_SCANNING=, PASSWORD_FILE=, etc.
        r"(?i)((?:export\s+)?(?:\w+_)?(?:SECRET|TOKEN|PASSWORD|PASSWD|CREDENTIAL|AUTH|(?:API|ACCESS|PRIVATE|SECRET)_KEY)=)(\S+)",
    ]
}

/// CLI flags that take passwords
fn cli_password_patterns() -> Vec<&'static str> {
    vec![
        // `-p<password>` ONLY for DB clients that use it that way (mysql family).
        // Scoped to the client command so we don't redact `docker run -p8080:80`,
        // `ssh -p2222`, or `redis-cli -p 6379` (where -p is a PORT, not a password).
        // group(1) = "<client> ... -p", group(2) = the password value.
        r"(?i)(\b(?:mysql|mysqldump|mariadb)\b[^\n]*?\s-p)([^\s-]\S*)",
        // --password=value or --password value
        r"(--password[=\s])(\S+)",
        // --token=value or --token value
        r"(--token[=\s])(\S+)",
        // --secret=value or --secret value
        r"(--secret[=\s])(\S+)",
        // --api-key=value or --apikey=value
        r"(?i)(--api[-_]?key[=\s])(\S+)",
    ]
}

/// Literal API key / token patterns (well-known prefixes)
fn api_key_patterns() -> Vec<&'static str> {
    vec![
        // AWS Access Key ID (always starts with AKIA)
        r"()(AKIA[0-9A-Z]{16})",
        // GitHub tokens: ghp_, gho_, ghs_, ghr_, github_pat_
        r"()(?:ghp_|gho_|ghs_|ghr_|github_pat_)[A-Za-z0-9_]{20,}",
        // OpenAI API key: sk-... (Anthropic keys have hyphens early in
        // "sk-ant-api..." so [A-Za-z0-9]{20,} naturally excludes them)
        r"()(sk-[A-Za-z0-9]{20,})",
        // Anthropic API key: sk-ant-api...
        r"()(sk-ant-api[A-Za-z0-9_-]{20,})",
        // Slack tokens: xoxb-, xoxp-, xoxo-, xoxa-
        r"()(xox[bpoa]-[A-Za-z0-9-]+)",
        // Stripe keys: sk_live_, sk_test_, pk_live_, pk_test_
        r"()([sr]k_(?:live|test)_[A-Za-z0-9]{20,})",
        // NPM tokens: npm_...
        r"()(npm_[A-Za-z0-9]{20,})",
        // PyPI tokens: pypi-...
        r"()(pypi-[A-Za-z0-9_-]{20,})",
        // GCP service account key (JSON key_id or private_key_id patterns)
        r#"(?i)("private_key":\s*")(-----BEGIN[^"]+)"#,
        // Azure AD client secret (common 34-40 char format)
        r"(?i)((?:AZURE_CLIENT_SECRET|AZURE_SECRET)\s*=\s*)(\S+)",
        // PEM private key headers (inline)
        r"()(-----BEGIN (?:RSA |EC |DSA |OPENSSH )?PRIVATE KEY-----)",
        // Generic long hex secrets (32+ hex chars, common for API keys)
        // Only match when preceded by a key-like assignment
        r"(?i)((?:SECRET|TOKEN|KEY|PASSWORD|AUTH|CREDENTIAL)\w*[=:]\s*)([0-9a-f]{32,})",
        // Base64 / base64url secrets after a secret-ish key name, for the `:`
        // (JSON/YAML) and `=` forms that the hex rule misses, e.g.
        // {"apiToken":"YWxhZGRpbjpvcGVuc2VzYW1l"} or --config token:AbC123...
        // The keyword must be the TAIL of the identifier (immediately before the
        // separator) so KEY_PATH=/usr/local/... is NOT redacted as a secret.
        r#"(?i)((?:secret|token|key|password|passwd|auth|credential)["']?\s*[=:]\s*["']?)([A-Za-z0-9+/_-]{24,}={0,2})"#,
    ]
}

/// Authorization headers in curl/wget/httpie commands
fn auth_header_patterns() -> Vec<&'static str> {
    vec![
        // curl -H "Authorization: Bearer xxx"
        r#"(?i)(-H\s*['"]?Authorization:\s*Bearer\s+)([^'"}\s]+)"#,
        // curl -H "Authorization: Basic xxx"
        r#"(?i)(-H\s*['"]?Authorization:\s*Basic\s+)([^'"}\s]+)"#,
        // curl -H "Authorization: token xxx" (GitHub style)
        r#"(?i)(-H\s*['"]?Authorization:\s*token\s+)([^'"}\s]+)"#,
        // curl -u user:password — scoped to HTTP clients so we don't redact
        // `docker run -u 1000:1000` (UID:GID) or other -u flags. The client
        // command and user are kept; only the password after the colon is hidden.
        // group(1) = "<client> ... -u user:", group(2) = password.
        r"(?i)(\b(?:curl|wget|http|https|xh)\b[^\n]*?-u\s+[^\s:]+:)(\S+)",
    ]
}

/// Database connection strings with embedded passwords
fn connection_string_patterns() -> Vec<&'static str> {
    vec![
        // scheme://[user]:password@host — redact only the password.
        // group(1) = "scheme://<user>:"  (user may be empty: redis://:pass@host)
        // group(2) = password (greedy within the token, so an '@' inside the
        //            password like postgres://u:p@ss@host doesn't leak the tail)
        // group(3) = "@host"  (the final @ + host, kept intact)
        r"((?:postgres(?:ql)?|mysql|mongodb(?:\+srv)?|redis|rediss|amqp|amqps)://[^/@\s]*:)(\S+)(@[^@\s/]+)",
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

    #[test]
    fn test_anthropic_api_key() {
        let cmd = "export ANTHROPIC_API_KEY=sk-ant-api03-abc123def456ghi789jkl012mno345";
        let redacted = redact_secrets(cmd);
        assert!(!redacted.contains("sk-ant-api03"));
    }

    #[test]
    fn test_npm_token() {
        let cmd =
            "npm config set //registry.npmjs.org/:_authToken npm_aBcDeFgHiJkLmNoPqRsTuVwXyZ012345";
        let redacted = redact_secrets(cmd);
        assert!(!redacted.contains("npm_aBcDeFg"));
    }

    #[test]
    fn test_pypi_token() {
        let cmd = "twine upload --password pypi-AgEIcHlwaS5vcmcABcDeFgHiJkLm_NoPq";
        let redacted = redact_secrets(cmd);
        assert!(!redacted.contains("pypi-AgEIcHlwaS5vcmcABcDeFgHiJkLm_NoPq"));
    }

    #[test]
    fn test_pem_private_key() {
        let cmd = "echo '-----BEGIN RSA PRIVATE KEY-----' > key.pem";
        let redacted = redact_secrets(cmd);
        assert!(redacted.contains("***REDACTED***"));
        assert!(!redacted.contains("BEGIN RSA PRIVATE KEY"));
    }

    #[test]
    fn test_azure_client_secret() {
        let cmd = "export AZURE_CLIENT_SECRET=abc123def456ghi789jkl012mno345pqr678";
        let redacted = redact_secrets(cmd);
        assert!(!redacted.contains("abc123def456"));
    }

    #[test]
    fn test_all_patterns_compile() {
        // Ensure every regex pattern compiles successfully.
        // This would have caught the lookahead bug (issue #9).
        let pattern_count = SECRET_PATTERNS.len();
        assert!(
            pattern_count > 0,
            "Expected at least one compiled secret pattern"
        );

        let expected_defs = [
            env_var_patterns(),
            cli_password_patterns(),
            api_key_patterns(),
            auth_header_patterns(),
            connection_string_patterns(),
        ];
        let total_defs: usize = expected_defs.iter().map(|v| v.len()).sum();
        assert_eq!(
            pattern_count, total_defs,
            "Some patterns failed to compile: expected {total_defs}, got {pattern_count}"
        );
    }

    #[test]
    fn test_bare_openai_key_redacted() {
        // Bare sk- key not inside an env var assignment
        let cmd = "curl https://api.openai.com -H 'Authorization: Bearer sk-proj1234567890abcdefghijklmnop'";
        let redacted = redact_secrets(cmd);
        assert!(!redacted.contains("sk-proj1234567890"));
    }

    #[test]
    fn test_openai_key_not_match_anthropic() {
        // Anthropic keys have hyphens early (sk-ant-api...) so the OpenAI
        // pattern sk-[A-Za-z0-9]{20,} should not match them. The dedicated
        // Anthropic pattern handles those separately.
        let cmd = "sk-ant-api03-abc123def456ghi789jkl012mno345";
        let redacted = redact_secrets(cmd);
        // Should be redacted by the Anthropic pattern, not the OpenAI one
        assert!(redacted.contains("***REDACTED***"));
    }

    #[test]
    fn test_wrap_command_no_panic() {
        // Regression test for issue #9: suv wrap -- ls should not error
        let cmd = "ls";
        let redacted = redact_secrets(cmd);
        assert_eq!(redacted, "ls");
    }

    // ── False positive regression tests (issue #16) ──

    #[test]
    fn test_no_false_positive_author_name() {
        let cmd = "AUTHOR_NAME=John git commit";
        assert_eq!(redact_secrets(cmd), cmd);
    }

    #[test]
    fn test_no_false_positive_git_author_email() {
        let cmd = "GIT_AUTHOR_EMAIL=me@test.com git commit";
        assert_eq!(redact_secrets(cmd), cmd);
    }

    #[test]
    fn test_no_false_positive_authentication_mode() {
        let cmd = "AUTHENTICATION_MODE=oauth2 app start";
        assert_eq!(redact_secrets(cmd), cmd);
    }

    #[test]
    fn test_no_false_positive_token_bucket_size() {
        let cmd = "TOKEN_BUCKET_SIZE=100 server start";
        assert_eq!(redact_secrets(cmd), cmd);
    }

    #[test]
    fn test_no_false_positive_tokenizers_parallelism() {
        let cmd = "TOKENIZERS_PARALLELISM=false python train.py";
        assert_eq!(redact_secrets(cmd), cmd);
    }

    #[test]
    fn test_no_false_positive_password_file() {
        let cmd = "PASSWORD_FILE=/etc/shadow cat";
        assert_eq!(redact_secrets(cmd), cmd);
    }

    #[test]
    fn test_no_false_positive_credential_helper() {
        let cmd = "CREDENTIAL_HELPER=store git config";
        assert_eq!(redact_secrets(cmd), cmd);
    }

    #[test]
    fn test_no_false_positive_private_key_path() {
        let cmd = "PRIVATE_KEY_PATH=/home/key.pem ssh-keygen";
        assert_eq!(redact_secrets(cmd), cmd);
    }

    #[test]
    fn test_no_false_positive_react_app_auth_domain() {
        let cmd = "REACT_APP_AUTH_DOMAIN=auth0.com npm start";
        assert_eq!(redact_secrets(cmd), cmd);
    }

    #[test]
    fn test_no_false_positive_secret_scanning() {
        let cmd = "SECRET_SCANNING=enabled lint";
        assert_eq!(redact_secrets(cmd), cmd);
    }

    #[test]
    fn test_no_false_positive_claude_code_no_flicker() {
        let cmd = "CLAUDE_CODE_NO_FLICKER=1 claude --allow-dangerously-skip-permissions";
        assert_eq!(redact_secrets(cmd), cmd);
    }

    #[test]
    fn test_no_false_positive_node_env() {
        let cmd = "NODE_ENV=production npm start";
        assert_eq!(redact_secrets(cmd), cmd);
    }

    // ── True positive tests: env vars that SHOULD be redacted ──

    #[test]
    fn test_env_var_auth_token() {
        let cmd = "AUTH_TOKEN=secret123 curl api.com";
        let redacted = redact_secrets(cmd);
        assert_eq!(redacted, "AUTH_TOKEN=***REDACTED*** curl api.com");
    }

    #[test]
    fn test_env_var_github_token() {
        let cmd = "GITHUB_TOKEN=ghp_abc123 gh pr list";
        let redacted = redact_secrets(cmd);
        assert!(redacted.contains("GITHUB_TOKEN=***REDACTED***"));
    }

    #[test]
    fn test_env_var_db_password() {
        let cmd = "DB_PASSWORD=pass123 psql";
        let redacted = redact_secrets(cmd);
        assert_eq!(redacted, "DB_PASSWORD=***REDACTED*** psql");
    }

    #[test]
    fn test_env_var_api_key() {
        let cmd = "API_KEY=abc123 curl api.com";
        let redacted = redact_secrets(cmd);
        assert_eq!(redacted, "API_KEY=***REDACTED*** curl api.com");
    }

    #[test]
    fn test_env_var_secret_key() {
        let cmd = "SECRET_KEY=abc123 python app.py";
        let redacted = redact_secrets(cmd);
        assert_eq!(redacted, "SECRET_KEY=***REDACTED*** python app.py");
    }

    #[test]
    fn test_env_var_aws_secret_access_key() {
        let cmd = "AWS_SECRET_ACCESS_KEY=wJalr123 aws s3 ls";
        let redacted = redact_secrets(cmd);
        assert!(redacted.contains("AWS_SECRET_ACCESS_KEY=***REDACTED***"));
    }

    #[test]
    fn test_env_var_basic_auth() {
        let cmd = "BASIC_AUTH=user:pass curl api.com";
        let redacted = redact_secrets(cmd);
        assert_eq!(redacted, "BASIC_AUTH=***REDACTED*** curl api.com");
    }

    #[test]
    fn test_env_var_bare_secret() {
        let cmd = "SECRET=mysecret deploy";
        let redacted = redact_secrets(cmd);
        assert_eq!(redacted, "SECRET=***REDACTED*** deploy");
    }

    #[test]
    fn test_env_var_bare_token() {
        let cmd = "TOKEN=abc123 app";
        let redacted = redact_secrets(cmd);
        assert_eq!(redacted, "TOKEN=***REDACTED*** app");
    }

    #[test]
    fn test_env_var_export_credential() {
        let cmd = "export MY_CREDENTIAL=secret123";
        let redacted = redact_secrets(cmd);
        assert_eq!(redacted, "export MY_CREDENTIAL=***REDACTED***");
    }

    #[test]
    fn test_env_var_private_key() {
        let cmd = "PRIVATE_KEY=-----BEGIN-RSA sshd";
        let redacted = redact_secrets(cmd);
        assert!(redacted.contains("PRIVATE_KEY=***REDACTED***"));
    }

    #[test]
    fn test_env_var_anthropic_api_key() {
        let cmd = "ANTHROPIC_API_KEY=sk-ant-api03-test123 claude";
        let redacted = redact_secrets(cmd);
        assert!(redacted.contains("ANTHROPIC_API_KEY=***REDACTED***"));
    }

    // ── Redaction hardening: -p/-u false positives must NOT be redacted ──

    #[test]
    fn test_no_false_positive_docker_port() {
        // -p here is a port mapping, not a password.
        for cmd in [
            "docker run -p 8080:80 nginx",
            "docker run -p8080:80 nginx",
            "docker run -p 127.0.0.1:5432:5432 postgres",
        ] {
            assert_eq!(redact_secrets(cmd), cmd, "should not redact docker -p: {cmd}");
        }
    }

    #[test]
    fn test_no_false_positive_ssh_port() {
        for cmd in ["ssh -p 2222 user@host", "ssh -p2222 user@host", "scp -P 2222 f host:"] {
            assert_eq!(redact_secrets(cmd), cmd, "should not redact ssh port: {cmd}");
        }
    }

    #[test]
    fn test_no_false_positive_redis_cli_port() {
        // redis-cli -p is a PORT, not a password.
        let cmd = "redis-cli -p 6379 ping";
        assert_eq!(redact_secrets(cmd), cmd);
    }

    #[test]
    fn test_no_false_positive_docker_user_uid_gid() {
        // -u 1000:1000 is UID:GID, not user:password.
        let cmd = "docker run -u 1000:1000 alpine id";
        assert_eq!(redact_secrets(cmd), cmd);
    }

    #[test]
    fn test_mysql_password_still_redacted() {
        let cmd = "mysql -u root -pMyP@ssw0rd mydb";
        let redacted = redact_secrets(cmd);
        assert!(redacted.contains("-p***REDACTED***"));
        assert!(!redacted.contains("MyP@ssw0rd"));
    }

    // ── Redaction hardening: connection-string leaks ──

    #[test]
    fn test_connection_string_at_sign_in_password_no_leak() {
        // Password contains '@'; the whole password must be redacted, host kept.
        let cmd = "psql postgresql://admin:p@ss@db.example.com:5432/mydb";
        let redacted = redact_secrets(cmd);
        assert!(!redacted.contains("p@ss"), "leaked @-password: {redacted}");
        assert!(redacted.contains("@db.example.com:5432/mydb"), "host mangled: {redacted}");
        assert!(redacted.contains("postgresql://admin:***REDACTED***@"));
    }

    #[test]
    fn test_connection_string_password_only_uri() {
        // No username (redis://:password@host) must still be redacted.
        let cmd = "redis-cli -u redis://:authpass123@cache.example.com:6379";
        let redacted = redact_secrets(cmd);
        assert!(!redacted.contains("authpass123"), "leaked password-only URI: {redacted}");
    }

    #[test]
    fn test_no_false_positive_redis_uri_without_auth() {
        // No '@' → no credentials → must not be touched.
        let cmd = "redis-cli -u redis://cache.example.com:6379/0";
        assert_eq!(redact_secrets(cmd), cmd);
    }

    // ── Redaction hardening: base64 secret after a key name ──

    #[test]
    fn test_base64_token_in_json_redacted() {
        let cmd = r#"curl -d '{"apiToken":"YWxhZGRpbjpvcGVuc2VzYW1lMTIzNDU2"}' api.com"#;
        let redacted = redact_secrets(cmd);
        assert!(!redacted.contains("YWxhZGRpbjpvcGVuc2VzYW1lMTIzNDU2"), "leaked base64: {redacted}");
    }

    #[test]
    fn test_no_false_positive_key_path() {
        // KEY_PATH ends in PATH, not a secret keyword → its path value is kept.
        let cmd = "KEY_PATH=/home/user/projects/myapp/config/settings npm start";
        assert_eq!(redact_secrets(cmd), cmd);
    }

    /// Consolidated corpus: each (input, must_be_absent) pair locks in a known
    /// false-positive (input unchanged) or known leak (secret redacted). Keeps
    /// the multi-regex redaction rules from regressing.
    #[test]
    fn test_redaction_corpus() {
        // (command, substring_that_must_NOT_survive)
        let must_redact = [
            ("mysql -u root -psup3rs3cret db", "sup3rs3cret"),
            ("curl -u admin:s3cretpw https://api.com", "s3cretpw"),
            ("psql postgresql://u:p@ss@host/db", "p@ss"),
            ("redis-cli -u redis://:onlypass@host", "onlypass"),
            (r#"http POST api.com token:abcdefGHIJKLmnop12345678="#, "abcdefGHIJKLmnop12345678"),
        ];
        for (cmd, secret) in must_redact {
            let out = redact_secrets(cmd);
            assert!(!out.contains(secret), "FAILED to redact `{secret}` in: {cmd} -> {out}");
        }

        // commands that must be returned untouched (no false positives)
        let must_keep = [
            "docker run -p 8080:80 nginx",
            "docker run -u 1000:1000 alpine",
            "ssh -p 2222 host",
            "redis-cli -p 6379 ping",
            "redis-cli -u redis://cache:6379/0",
            "KEY_PATH=/usr/local/share/app/keys/dir cmd",
            "git status",
        ];
        for cmd in must_keep {
            assert_eq!(redact_secrets(cmd), cmd, "false positive on: {cmd}");
        }
    }
}
