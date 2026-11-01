use std::sync::{Arc, LazyLock, Mutex};

use regex::Regex;

/// Placeholder that replaces detected secret values
const REDACTED: &str = "***REDACTED***";

/// Compiled secret patterns — built once, reused forever. They use ASCII word
/// classes and boundaries (`[[:word:]]`, `(?-u:\\b)`): Unicode's tables make a
/// rule several times slower to compile, and names and flags are ASCII anyway.
static SECRET_PATTERNS: LazyLock<Vec<SecretPattern>> = LazyLock::new(build_patterns);

/// (source strings, compiled regexes) for the extra-pattern cache.
type ExtraCache = (Vec<String>, Arc<Vec<Regex>>);

/// Cache of compiled user `extra_patterns`, recompiled only when the source
/// strings change (the hot recording path passes them on every command).
static EXTRA_CACHE: Mutex<Option<ExtraCache>> = Mutex::new(None);

struct SecretPattern {
    regex: Regex,
}

/// Text every rule needs at least one of, lowercased. Compiling the rules takes
/// longer than the rest of `suv add` together, and every recorded command is a
/// new process, so a command with none of these skips them entirely.
///
/// A new rule must be reachable from here: anything it can match has to hold
/// one of these, or the rule never runs. `test_each_trigger_reaches_its_rule`
/// holds this list to that.
const TRIGGERS: &[&str] = &[
    "pass",
    "pwd",
    "secret",
    "token",
    "credential",
    "auth",
    "key",
    "cookie",
    "login",
    "mysql",
    "mariadb",
    "redis-cli",
    "mongo",
    "sqlcmd",
    "ldap",
    "zip",
    "from-literal",
    "curl",
    "wget",
    "http",
    "xh",
    "://",
    "akia",
    "asia",
    "aiza",
    "eyj",
    "xox",
    "xapp-",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "sk-",
    "sk_",
    "rk_",
    "npm_",
    "pypi-",
    "hf_",
    "ntn_",
    "_v1_",
    "lin_",
    "xai-",
    "sntry",
    "sg.",
    "glpat-",
    "gldt-",
    "glrt-",
    "glptt-",
    "glcbt-",
    "glsoat-",
    "hooks.slack",
    "/webhooks/",
    "api.telegram",
    "sig=",
    "signature=",
];

fn may_hold_secret(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    TRIGGERS.iter().any(|trigger| lower.contains(trigger))
}

/// Redact secrets from a command string.
/// Returns the command with all detected secret values replaced by `***REDACTED***`.
pub fn redact_secrets(command: &str) -> String {
    if !may_hold_secret(command) {
        return command.to_string();
    }
    let patterns = &*SECRET_PATTERNS;
    let mut result = command.to_string();

    for p in patterns {
        result = p
            .regex
            .replace_all(&result, |caps: &regex::Captures| {
                if caps.get(2).is_some_and(|m| is_not_a_secret(m.as_str())) {
                    return caps[0].to_string();
                }
                let prefix = caps.get(1).map_or("", |m| m.as_str());
                let suffix = caps.get(3).map_or("", |m| m.as_str());
                format!("{prefix}{REDACTED}{suffix}")
            })
            .to_string();
    }

    result
}

/// A value that says nothing secret even under a secret name: a type
/// (`token: str` in a Python class, `password: string;` in an interface,
/// `"token":"string"` in an `OpenAPI` example) or a setting (`id-token: write`
/// in a GitHub workflow).
fn is_not_a_secret(value: &str) -> bool {
    let word = value.trim_end_matches([',', ';']).to_ascii_lowercase();
    matches!(
        word.as_str(),
        "str"
            | "&str"
            | "string"
            | "bool"
            | "boolean"
            | "int"
            | "integer"
            | "number"
            | "float"
            | "bytes"
            | "any"
            | "none"
            | "null"
            | "true"
            | "false"
            | "read"
            | "write"
    )
}

/// Like [`redact_secrets`], then apply user-configured `extra_patterns`. Each
/// extra pattern's whole match is replaced with the placeholder. Invalid
/// patterns are skipped (they're validated at config load). Compilation is
/// cached so the hot recording path doesn't recompile per command.
pub fn redact_secrets_with_extra(command: &str, extra_patterns: &[String]) -> String {
    let mut result = redact_secrets(command);
    if extra_patterns.is_empty() {
        return result;
    }
    let compiled = compiled_extra(extra_patterns);
    for re in compiled.iter() {
        result = re.replace_all(&result, REDACTED).to_string();
    }
    result
}

/// Return cached compiled extra patterns, recompiling only when they change.
fn compiled_extra(patterns: &[String]) -> Arc<Vec<Regex>> {
    if let Ok(guard) = EXTRA_CACHE.lock() {
        if let Some((src, compiled)) = guard.as_ref() {
            if src == patterns {
                return Arc::clone(compiled);
            }
        }
    }
    let compiled: Arc<Vec<Regex>> =
        Arc::new(patterns.iter().filter_map(|p| Regex::new(p).ok()).collect());
    if let Ok(mut guard) = EXTRA_CACHE.lock() {
        *guard = Some((patterns.to_vec(), Arc::clone(&compiled)));
    }
    compiled
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
        key_value_patterns(),
        auth_header_patterns(),
        connection_string_patterns(),
    ];

    defs.into_iter()
        .flatten()
        .filter_map(|pat| match Regex::new(&pat) {
            Ok(regex) => Some(SecretPattern { regex }),
            Err(e) => {
                eprintln!("suvadu: secret pattern failed to compile: {pat}: {e}");
                None
            }
        })
        .collect()
}

/// Lazy span between a client command and one of its own flags. It stops at
/// shell separators (`|`, `;`, `&&`, a background `&`, newline) so a flag of a
/// later command in a pipeline is never attributed to the client, while a
/// `&` inside a URL query (`?a=1&b=2`) is still crossed.
const SPAN: &str = r"(?:[^\n|;&]|&[^&\s])*?";

/// Expand one secret rule into its value forms. `prefix` is the regex for
/// what precedes the value (kept); `bare` is the regex for an unquoted value.
///
/// - `prefix"value"` and `prefix'value'`: the quotes are kept and everything
///   between them is redacted, so `DB_PASSWORD="correct horse battery"` loses
///   the whole phrase, not just its first word.
///   A quoted value can't start with whitespace: in `grep "PASSWORD=" .env`
///   that quote closes the pattern, and what follows it is the next argument.
/// - `prefix value`: `bare` should stop at a quote, so a value inside a quoted
///   argument (`-d "password=x"`) keeps its closing quote.
///
/// Captures: group(1) = prefix (+ opening quote), group(2) = secret,
/// group(3) = closing quote (quoted forms only).
fn value_forms(flags: &str, prefix: &str, bare: &str) -> [String; 3] {
    [
        format!(r#"{flags}({prefix}")([^"\s][^"]*)(")"#),
        format!(r"{flags}({prefix}')([^'\s][^']*)(')"),
        format!(r"{flags}({prefix})({bare})"),
    ]
}

/// Unquoted value that stops at whitespace or a quote.
const BARE: &str = r#"[^\s"']+"#;
/// Unquoted value that additionally may not start with `-` (so a following
/// flag is not mistaken for the value of a short option).
const BARE_NO_DASH: &str = r#"[^\s"'-][^\s"']*"#;

/// Environment variable assignments with sensitive names.
///
/// The keyword must appear as the **final segment** of the variable name
/// (delimited by underscores) so that names like `AUTHOR_NAME`,
/// `TOKENIZERS_PARALLELISM`, or `PASSWORD_FILE` are not false-positived.
///
/// Captures: group(1) = `VAR_NAME=`, group(2) = the secret value
fn env_var_patterns() -> Vec<String> {
    // Matches: GITHUB_TOKEN=, MY_SECRET=, DB_PASSWORD=, export AUTH=, AWS_SECRET_ACCESS_KEY=, etc.
    // Rejects: AUTHOR_NAME=, TOKEN_BUCKET_SIZE=, SECRET_SCANNING=, PASSWORD_FILE=, etc.
    let mut v: Vec<String> = value_forms(
        "(?i)",
        r"(?:export\s+)?(?:[[:word:]]+_)?(?:SECRET|TOKEN|PASSWORD|PASSWD|CREDENTIAL|AUTH|(?:API|ACCESS|PRIVATE|SECRET)_KEY)=",
        BARE,
    )
    .into();
    // Short spellings need a word boundary the names above do without, since
    // they turn up inside other words (`BYPASS=`). `PWD` is a directory unless
    // named otherwise: `BUILD_PWD=/srv`, `MYSQL_PWD=<password>`. An unquoted
    // `&` ends the value, in a shell word and in the query string `apikey=`
    // is often found in.
    v.extend(value_forms(
        "(?i)",
        r"(?:export\s+)?(?-u:\b)(?:(?:[[:word:]]+_)?(?:PASS|APIKEY)|(?:MYSQL|DB|SQL|ADMIN|ROOT)_PWD)=",
        r#"[^\s"'&]+"#,
    ));
    v
}

/// CLI flags that take passwords
fn cli_password_patterns() -> Vec<String> {
    let mut v = vec![
        // `-p<password>` ONLY for DB clients that use it that way (mysql family).
        // Scoped to the client command so we don't redact `docker run -p8080:80`,
        // `ssh -p2222`, or `redis-cli -p 6379` (where -p is a PORT, not a password).
        // Case-sensitive: mysql's `-P` is its port. The span stops at shell
        // separators so `mysql ... | perl -pe ...` leaves perl's -p alone.
        // group(1) = "<client> ... -p", group(2) = the password value.
        format!(r"((?-u:\b)(?:mysql|mysqldump|mysqladmin|mariadb)(?:\s{SPAN})?\s-p)([^\s-]\S*)"),
    ];
    // --password / --token / --secret / --api-key, `=` or space separated
    v.extend(value_forms("", r"--password[=\s]", BARE));
    v.extend(value_forms("", r"--token[=\s]", BARE));
    v.extend(value_forms("", r"--secret[=\s]", BARE));
    v.extend(value_forms("(?i)", r"--api[-_]?key[=\s]", BARE));
    // Other flags whose value is a credential. A list, not every name ending
    // in `token` or `password`: `--with-token` and `--no-password` take no
    // value, and the argument after them would be lost.
    v.extend(value_forms(
        "(?i)",
        r"--(?:pass|passwd|passphrase|oauth2-bearer|(?:http|ftp|proxy)-password|(?:client|app|api|shared|webhook)[-_]secret|(?:access|api|app|auth|bot|github|gitlab|id|oauth|refresh|session)[-_]token)[=\s]",
        BARE_NO_DASH,
    ));
    // sshpass -p <password> (other sshpass options may come first; -P is its
    // prompt flag, so this is case-sensitive)
    v.extend(value_forms(
        "",
        r"(?-u:\b)sshpass(?:\s+-[fdP]\s*\S+|\s+-e\S*|\s+-[vh])*\s+-p\s*",
        BARE,
    ));
    // docker/podman/nerdctl login -p <password>
    v.extend(value_forms(
        "",
        &format!(r"(?-u:\b)(?:docker|podman|nerdctl)\s+login(?:\s{SPAN})?\s-p(?:=|\s+)?"),
        BARE_NO_DASH,
    ));
    // Password flags that mean something else on other commands, so each is
    // scoped to its client like mysql's above. Case-sensitive for the same
    // reason: sqlcmd's -p and zip's -p are not their -P.
    let client = |names: &str, flag: &str| format!(r"(?:{names})(?:\s{SPAN})?\s{flag}");
    let clients = [
        client("redis-cli", "-a"),
        client(
            "mongosh|mongo|mongodump|mongorestore|mongoexport|mongoimport",
            "-p",
        ),
        client("sqlcmd", "-P"),
        client(r"ldap[[:word:]]+", "-w"),
        client("zip|unzip", "-P"),
        client(r"az\s+login", "-p"),
    ]
    .join("|");
    v.extend(value_forms(
        "",
        &format!(r"(?-u:\b)(?:{clients})\s+"),
        BARE_NO_DASH,
    ));
    // openssl -pass pass:<password>; env: and file: name where it lives
    v.extend(value_forms("", r"\s-pass(?:in|out)?\s+pass:", BARE));
    // gh secret set <name> --body <value>
    v.extend(value_forms(
        "",
        &format!(r"(?-u:\b)gh\s+secret\s+set(?:\s{SPAN})?\s(?:--body[=\s]\s*|-b\s*)"),
        BARE,
    ));
    // kubectl create secret --from-literal=<name>=<value>, as often as given.
    // A configmap's values go too, which costs less than a secret's staying.
    v.extend(value_forms("", r"--from-literal[=\s][^=\s]+=", BARE));
    // fly secrets set A=1 B=2: every pair, up to the next command
    v.push(r"((?-u:\b)fly(?:ctl)?\s+secrets\s+set\s+)((?:[^\n|;&]|&[^&\s])*[^\s|;&])".to_string());
    // aws configure set [profile.]aws_secret_access_key|aws_session_token <value>
    v.extend(value_forms(
        "(?i)",
        r"(?-u:\b)aws\s+configure\s+set\s+(?:[[:word:].-]+\.)?(?:aws_secret_access_key|aws_session_token)\s+",
        BARE,
    ));
    v
}

/// Literal API key / token patterns (well-known prefixes)
fn api_key_patterns() -> Vec<String> {
    [
        // AWS access key ids: long-term (AKIA) and temporary STS (ASIA)
        r"()((?-u:\b)(?:AKIA|ASIA)[0-9A-Z]{16}(?-u:\b))",
        // GitHub tokens: ghp_, gho_, ghu_, ghs_, ghr_, github_pat_
        r"()(?-u:\b)(?:gh[pousr]_|github_pat_)[A-Za-z0-9_]{20,}",
        // OpenAI API key: sk-... (Anthropic keys have hyphens early in
        // "sk-ant-api..." so [A-Za-z0-9]{20,} naturally excludes them). The
        // word boundary keeps `my-disk-<long id>` from matching.
        r"()((?-u:\b)sk-[A-Za-z0-9]{20,})",
        // OpenAI project / service-account / admin keys
        r"()((?-u:\b)sk-(?:proj|svcacct|admin)-[A-Za-z0-9_-]{20,})",
        // Anthropic API key: sk-ant-api...
        r"()(sk-ant-api[A-Za-z0-9_-]{20,})",
        // Slack tokens: xoxb-, xoxp-, xoxo-, xoxa-, xoxr-, xoxs-, xoxe-,
        // and the xoxe.xoxp- rotating form
        r"()((?-u:\b)xox[abeoprs](?:\.xox[abeoprs])?-[A-Za-z0-9-]+)",
        // Slack app-level tokens: xapp-<n>-...
        r"()((?-u:\b)xapp-\d-[A-Za-z0-9-]{10,})",
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
        // PEM private keys: the whole block through the END line (e.g. pasted
        // into a heredoc). Without an END line, the BEGIN marker plus any
        // base64 body that follows it (a truncated paste).
        r"()(-----BEGIN [A-Z0-9 ]*PRIVATE KEY[A-Z ]*-----(?:(?s:.*?)-----END [A-Z0-9 ]*PRIVATE KEY[A-Z ]*-----|(?:[A-Za-z0-9+/=\s:,-]*[A-Za-z0-9+/=:,-])?))",
        // Google API key
        r"()((?-u:\b)AIza[0-9A-Za-z_-]{35})",
        // GitLab personal, deploy, runner, trigger, CI job and service tokens
        r"()((?-u:\b)gl(?:pat|dt|rt|ptt|cbt|soat)-[A-Za-z0-9_-]{20,})",
        // OpenRouter, which the OpenAI rule's run of alphanumerics can't reach
        r"()((?-u:\b)sk-or-v\d+-[A-Za-z0-9_-]{20,})",
        // Hugging Face, Groq, Notion, DigitalOcean, Linear and xAI: a prefix
        // and a long alphanumeric run, so names with more words don't match
        r"()((?-u:\b)(?:(?:hf|gsk|ntn|do[opr]_v1|lin_api|lin_oauth)_|xai-)[A-Za-z0-9]{30,})",
        // Sentry auth tokens
        r"()((?-u:\b)sntry[su]_[A-Za-z0-9_=-]{30,})",
        // SendGrid: SG.<id>.<secret>
        r"()((?-u:\b)SG\.[A-Za-z0-9_-]{16,}\.[A-Za-z0-9_-]{16,})",
        // JWTs: every segment, up to the five of an encrypted one, and the
        // trailing dot of an unsigned one. Base64 JSON with no dots is data.
        r"()((?-u:\b)eyJ[A-Za-z0-9_-]{10,}(?:\.[A-Za-z0-9_-]+){1,4}\.?)",
        // Generic long hex secrets (32+ hex chars, common for API keys)
        // Only match when preceded by a key-like assignment
        r"(?i)((?:SECRET|TOKEN|KEY|PASSWORD|AUTH|CREDENTIAL)[[:word:]]*[=:]\s*)([0-9a-f]{32,})",
        // Base64 / base64url secrets after a secret-ish key name, for the `:`
        // (JSON/YAML) and `=` forms that the hex rule misses, e.g.
        // {"apiToken":"YWxhZGRpbjpvcGVuc2VzYW1l"} or --config token:AbC123...
        // The keyword must be the TAIL of the identifier (immediately before the
        // separator) so KEY_PATH=/usr/local/... is NOT redacted as a secret.
        // The identifier must start at a word character that follows neither
        // `-` nor `=`, so CLI flag names (`--sort-key=...`) and flag values
        // (`--grep=token:...`) are not mistaken for secret assignments. Dotted
        // segments after the long run go with it, so a dotted token is not
        // left half-masked.
        r#"(?i)((?:^|[^[:word:]=-])(?:[[:word:]][[:word:]-]*?)?(?:secret|token|key|password|passwd|auth|credential)["']?\s*[=:]\s*["']?)([A-Za-z0-9+/_-]{24,}(?:\.[A-Za-z0-9+/_-]+)*={0,2})"#,
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// The last word of a name whose value is a secret however short it is, so
/// `client_secret`, `accessToken` and `x-api-key` count, and `token_type`,
/// `max_tokens` and `password_hint` don't. `key` and `auth` stay out because
/// `"key":"id"` and `"auth":"oauth"` are ordinary, and `pwd` because it is
/// usually the working directory; a long value under those names is caught
/// by the long-value rule in [`api_key_patterns`].
const SECRET_NAME: &str = r"(?:[[:word:]][[:word:]-]*?)?(?:password|passwd|passphrase|secret|token|credential|(?:api|access|private|secret)[_-]?key)";

/// Short secrets under a secret name, in the shapes agents write them: JSON
/// and code with a quoted value, and YAML. A quoted value can't start with
/// whitespace, a quote or punctuation, because then that quote closed a string
/// ending in the name: `rg "password: " src/ | rg "foo"` searches for the key,
/// and `print("with token:", "OK")` has no value to lose.
fn key_value_patterns() -> Vec<String> {
    let key = format!(r#"(?i)((?:^|[^[:word:]=-]){SECRET_NAME}(?:\\?["'])?\]?\s*(?:=>|:=|=|:)\s*"#);
    vec![
        format!(r#"{key}")([^"'\s\\,;)\]}}+](?:[^"\\\n]|\\.)*)(")"#),
        format!(r#"{key}')([^'"\s\\,;)\]}}+](?:[^'\\\n]|\\.)*)(')"#),
        // JSON inside a double-quoted shell argument: {\"password\":\"x\"}
        format!(r#"{key}\\")([^"'\s\\,;)\]}}+](?:[^"\\\n]|\\[^"\n])*)(\\")"#),
        // A whole line of YAML with a one-word value, so prose keeps its words
        format!(r#"(?im)(^[ \t]*(?:-[ \t]+)?{SECRET_NAME}:[ \t]+)([^\s"'#]\S*)([ \t]*$)"#),
        // The same line echoed into a file. Scoped to echo and printf: a
        // quoted `token: rotated` is a commit message or a search anywhere else.
        format!(
            r#"(?i)((?-u:\b)(?:echo|printf)\s+(?:-[[:word:]]+\s+)*"{SECRET_NAME}:[ \t]*)([^\s"]+)(")"#
        ),
        format!(
            r"(?i)((?-u:\b)(?:echo|printf)\s+(?:-[[:word:]]+\s+)*'{SECRET_NAME}:[ \t]*)([^\s']+)(')"
        ),
    ]
}

/// Authorization headers and credentials in curl/wget/httpie commands
fn auth_header_patterns() -> Vec<String> {
    // The scheme word of an Authorization header, kept so it stays readable.
    const SCHEME: &str = r"(?:[A-Za-z][[:word:]-]*\s+)?";
    let mut v = vec![
        // Authorization: Bearer|Basic|token xxx — however it's passed:
        // curl -H / --header, httpie `Authorization:"Bearer xxx"`, JSON.
        r#"(?i)((?-u:\b)Authorization['"]?:\s*['"]?\s*(?:Bearer|Basic|token)\s+)([^'"}\s]+)"#
            .to_string(),
        // curl/wget -u user:password, -uuser:password, --user[=]user:password —
        // scoped to the client so `docker run -u 1000:1000` (UID:GID) is left
        // alone. The user is kept; only the password after the colon is hidden.
        // group(1) = "<client> ... -u user:", group(2) = password.
        format!(
            r#"(?i)((?-u:\b)(?:curl|wget)(?:\s{SPAN})?\s(?:-u|--user[=\s])\s*['"]?[^\s:'"]*:)([^\s'"]+)"#
        ),
        // httpie / xh: -a user:password, --auth[=]user:password (and -u, as
        // before). The client name must be followed by whitespace so the
        // `https` of a URL elsewhere in the command doesn't count.
        format!(
            r#"(?i)((?-u:\b)(?:http|https|xh|xhs)(?:\s{SPAN})?\s(?:-a|--auth[=\s]|-u)\s*['"]?[^\s:'"]*:)([^\s'"]+)"#
        ),
        // A header with a secret name, whatever its length. Only after
        // -H/--header: a quoted `token: x` anywhere else is text.
        format!(r#"(?i)(\s(?:-H|--header)[=\s]?\s*"{SECRET_NAME}:[ \t]*)([^\s"]+)(")"#),
        format!(r"(?i)(\s(?:-H|--header)[=\s]?\s*'{SECRET_NAME}:[ \t]*)([^\s']+)(')"),
        // Authorization under any scheme, and Cookie. Again only when sent:
        // `grep "Authorization: Bearer"` holds no secret.
        format!(
            r#"(?i)(\s(?:-H|--header)[=\s]?\s*['"](?:Proxy-)?(?:Authorization|Cookie):\s*{SCHEME})([^'"\s][^'"\n]*)(['"])"#
        ),
        // The same as a key with a quoted value, in code or JSON. The quote
        // right after the colon leaves f"Bearer {token}" alone.
        format!(
            r#"(?i)((?-u:\b)(?:Authorization|Cookie)['"]?\s*:\s*['"]{SCHEME})([^'"\s][^'"\n]*)(['"])"#
        ),
    ];
    // curl -b / --cookie name=value. Without a `=` it names a cookie file.
    v.extend(value_forms(
        "",
        &format!(r"(?-u:\b)curl(?:\s{SPAN})?\s(?:-b|--cookie)[=\s]\s*"),
        r#"[^\s"'=]+=[^\s"']*"#,
    ));
    v
}

/// Connection strings and URLs with embedded passwords
fn connection_string_patterns() -> Vec<String> {
    vec![
        // Database URLs: scheme://[user]:password@host — redact only the password.
        // group(1) = "scheme://<user>:"  (user may be empty: redis://:pass@host)
        // group(2) = password (greedy within the token, so an '@' inside the
        //            password like postgres://u:p@ss@host doesn't leak the tail)
        // group(3) = "@host"  (the final @ + host, kept intact)
        r"((?:postgres(?:ql)?|mysql|mongodb(?:\+srv)?|redis|rediss|amqp|amqps)://[^/@\s]*:)(\S+)(@[^@\s/]+)"
            .to_string(),
        // Any other scheme (https, git+ssh, ftp, ...): user:password@ in the
        // authority. The password stays inside the authority (no `/`, `?`,
        // `#`) so `host:8080/path?email=a@b` is not mistaken for credentials.
        r"((?-u:\b)[A-Za-z][A-Za-z0-9+.-]*://[^/@\s:]*:)([^\s/?#]+)(@[^@\s/?#]+)".to_string(),
        // URLs whose path is the credential: anyone holding one can post
        r"(hooks\.slack\.com/services/)(T[A-Za-z0-9]+/B[A-Za-z0-9]+/[A-Za-z0-9]+)".to_string(),
        r"(discord(?:app)?\.com/api/webhooks/\d+/)([A-Za-z0-9_-]+)".to_string(),
        r"(api\.telegram\.org/(?:file/)?bot)(\d+:[A-Za-z0-9_-]+)".to_string(),
        // Keys and signatures in a query string, by exact name so `?sort_key=`
        // and `?keyword=` stay, and not after `[`, which is a JMESPath filter.
        // A signed URL is a credential until it expires.
        r#"(?i)((?:[^\[\s]\?|&)(?:key|apikey|api-key|sig|signature|x-amz-signature|x-goog-signature)=)([^&\s"'#]+)"#
            .to_string(),
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
            key_value_patterns(),
            auth_header_patterns(),
            connection_string_patterns(),
        ];
        let total_defs: usize = expected_defs.iter().map(std::vec::Vec::len).sum();
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
            assert_eq!(
                redact_secrets(cmd),
                cmd,
                "should not redact docker -p: {cmd}"
            );
        }
    }

    #[test]
    fn test_no_false_positive_ssh_port() {
        for cmd in [
            "ssh -p 2222 user@host",
            "ssh -p2222 user@host",
            "scp -P 2222 f host:",
        ] {
            assert_eq!(
                redact_secrets(cmd),
                cmd,
                "should not redact ssh port: {cmd}"
            );
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
        assert!(
            redacted.contains("@db.example.com:5432/mydb"),
            "host mangled: {redacted}"
        );
        assert!(redacted.contains("postgresql://admin:***REDACTED***@"));
    }

    #[test]
    fn test_connection_string_password_only_uri() {
        // No username (redis://:password@host) must still be redacted.
        let cmd = "redis-cli -u redis://:authpass123@cache.example.com:6379";
        let redacted = redact_secrets(cmd);
        assert!(
            !redacted.contains("authpass123"),
            "leaked password-only URI: {redacted}"
        );
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
        assert!(
            !redacted.contains("YWxhZGRpbjpvcGVuc2VzYW1lMTIzNDU2"),
            "leaked base64: {redacted}"
        );
    }

    #[test]
    fn test_no_false_positive_key_path() {
        // KEY_PATH ends in PATH, not a secret keyword → its path value is kept.
        let cmd = "KEY_PATH=/home/user/projects/myapp/config/settings npm start";
        assert_eq!(redact_secrets(cmd), cmd);
    }

    #[test]
    fn test_extra_patterns_redact() {
        // A corporate token format the built-ins don't know about.
        let extra = vec![r"CORP-[A-Z0-9]{8}".to_string()];
        let out = redact_secrets_with_extra("deploy --key CORP-AB12CD34 prod", &extra);
        assert!(
            !out.contains("CORP-AB12CD34"),
            "extra pattern should redact: {out}"
        );
        assert!(out.contains("***REDACTED***"));
        // Built-in rules still apply through the same call.
        let out2 = redact_secrets_with_extra("export API_KEY=abc123 deploy", &extra);
        assert!(out2.contains("API_KEY=***REDACTED***"));
    }

    #[test]
    fn test_extra_patterns_empty_is_noop() {
        assert_eq!(redact_secrets_with_extra("git status", &[]), "git status");
    }

    // ── Table-driven: secrets that must be redacted (exact output) ──

    /// Each row is (input, expected output). All secrets are obvious fakes.
    const MUST_REDACT: &[(&str, &str)] = &[
        // OpenAI project / service-account / admin keys outside env assignments
        (
            "echo sk-proj-XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX | pbcopy",
            "echo ***REDACTED*** | pbcopy",
        ),
        (
            "llm keys set openai --value sk-svcacct-XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX",
            "llm keys set openai --value ***REDACTED***",
        ),
        (
            "echo sk-admin-XXXXXXXX_XXXXXXXX-XXXXXXXXXXXX",
            "echo ***REDACTED***",
        ),
        // curl --header (long form)
        (
            r#"curl --header "Authorization: Bearer fake-bearer-value-123" https://api.example.com"#,
            r#"curl --header "Authorization: Bearer ***REDACTED***" https://api.example.com"#,
        ),
        (
            "curl --header 'Authorization: token faketoken123' https://api.example.com",
            "curl --header 'Authorization: token ***REDACTED***' https://api.example.com",
        ),
        // curl --user / -u in every spelling
        (
            "curl --user alice:fakepass123 https://api.example.com",
            "curl --user alice:***REDACTED*** https://api.example.com",
        ),
        (
            "curl --user=alice:fakepass123 https://api.example.com",
            "curl --user=alice:***REDACTED*** https://api.example.com",
        ),
        (
            "curl -ualice:fakepass123 https://api.example.com",
            "curl -ualice:***REDACTED*** https://api.example.com",
        ),
        (
            "curl -u 'alice:fakepass123' https://api.example.com",
            "curl -u 'alice:***REDACTED***' https://api.example.com",
        ),
        (
            "curl -s https://api.example.com/x?a=1&b=2 -u alice:fakepass123",
            "curl -s https://api.example.com/x?a=1&b=2 -u alice:***REDACTED***",
        ),
        // httpie header and auth flag
        (
            r#"http GET api.example.com Authorization:"Bearer fakebearer123""#,
            r#"http GET api.example.com Authorization:"Bearer ***REDACTED***""#,
        ),
        (
            "http GET api.example.com 'Authorization: Bearer fakebearer123'",
            "http GET api.example.com 'Authorization: Bearer ***REDACTED***'",
        ),
        (
            "http -a alice:fakepass123 api.example.com",
            "http -a alice:***REDACTED*** api.example.com",
        ),
        // credentials embedded in any scheme://user:pass@host URL
        (
            "curl https://alice:fakepass123@example.com/api",
            "curl https://alice:***REDACTED***@example.com/api",
        ),
        (
            "git clone https://oauth2:faketoken123@gitlab.example.com/group/repo.git",
            "git clone https://oauth2:***REDACTED***@gitlab.example.com/group/repo.git",
        ),
        (
            "git remote add origin https://:faketoken123@gitlab.example.com/repo.git",
            "git remote add origin https://:***REDACTED***@gitlab.example.com/repo.git",
        ),
        (
            "lftp ftp://alice:fakepass123@ftp.example.com",
            "lftp ftp://alice:***REDACTED***@ftp.example.com",
        ),
        // temporary (STS) AWS access key ids
        ("echo ASIAXXXXXXXXXXXXXXXX", "echo ***REDACTED***"),
        // aws configure set <secret setting> <value>
        (
            "aws configure set aws_secret_access_key fakeSecretAccessKeyValue123",
            "aws configure set aws_secret_access_key ***REDACTED***",
        ),
        (
            "aws configure set default.aws_session_token fakeSessionToken --profile dev",
            "aws configure set default.aws_session_token ***REDACTED*** --profile dev",
        ),
        // Slack config / app-level tokens, split so secret scanners don't
        // mistake these placeholders for real tokens
        (concat!("echo xo", "xe-1-XXXXXXXXXXXXXXXXXXXX"), "echo ***REDACTED***"),
        (
            concat!("echo xo", "xe.xo", "xp-1-XXXXXXXXXXXXXXXXXXXX"),
            "echo ***REDACTED***",
        ),
        (
            concat!("echo xa", "pp-1-XXXXXXXXXXX-0000000000-XXXXXXXXXXXX"),
            "echo ***REDACTED***",
        ),
        // GitHub token families
        ("echo ghp_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx", "echo ***REDACTED***"),
        ("echo gho_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx", "echo ***REDACTED***"),
        ("echo ghu_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx", "echo ***REDACTED***"),
        ("echo ghs_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx", "echo ***REDACTED***"),
        ("echo ghr_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx", "echo ***REDACTED***"),
        // sshpass
        (
            "sshpass -p fakepass123 ssh user@host",
            "sshpass -p ***REDACTED*** ssh user@host",
        ),
        (
            "sshpass -pfakepass123 ssh user@host",
            "sshpass -p***REDACTED*** ssh user@host",
        ),
        (
            "sshpass -v -p 'fake pass phrase' ssh user@host",
            "sshpass -v -p '***REDACTED***' ssh user@host",
        ),
        // docker login
        (
            "docker login -u alice -p fakepass123 registry.example.com",
            "docker login -u alice -p ***REDACTED*** registry.example.com",
        ),
        (
            "docker login --password fakepass123 registry.example.com",
            "docker login --password ***REDACTED*** registry.example.com",
        ),
        (
            "podman login --password=fakepass123 registry.example.com",
            "podman login --password=***REDACTED*** registry.example.com",
        ),
        // quoted values containing spaces are redacted whole
        (
            r#"DB_PASSWORD="correct horse battery" ./migrate"#,
            r#"DB_PASSWORD="***REDACTED***" ./migrate"#,
        ),
        (
            "export API_TOKEN='correct horse battery'",
            "export API_TOKEN='***REDACTED***'",
        ),
        (
            r#"psql --password "correct horse battery" -h db"#,
            r#"psql --password "***REDACTED***" -h db"#,
        ),
        // a bare value inside a quoted argument keeps the closing quote
        (
            r#"curl -d "password=fakepass123" https://example.com/login"#,
            r#"curl -d "password=***REDACTED***" https://example.com/login"#,
        ),
        // PEM private key body in a heredoc, through the END line
        (
            "cat > id_ed25519 <<'EOF'\n-----BEGIN OPENSSH PRIVATE KEY-----\nZmFrZWZha2VmYWtlZmFrZWZha2VmYWtl\nZmFrZWZha2VmYWtl\n-----END OPENSSH PRIVATE KEY-----\nEOF",
            "cat > id_ed25519 <<'EOF'\n***REDACTED***\nEOF",
        ),
        (
            "printf '%s' '-----BEGIN ENCRYPTED PRIVATE KEY-----\nZmFrZWZha2Vm\n-----END ENCRYPTED PRIVATE KEY-----' > k.pem",
            "printf '%s' '***REDACTED***' > k.pem",
        ),
        // header / query / mysql forms that must keep working
        (
            r#"curl -H "x-api-key: XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX" https://api.example.com"#,
            r#"curl -H "x-api-key: ***REDACTED***" https://api.example.com"#,
        ),
        (
            r#"curl "https://api.example.com/v1?access_token=XXXXXXXXXXXXXXXXXXXXXXXX""#,
            r#"curl "https://api.example.com/v1?access_token=***REDACTED***""#,
        ),
        (
            r#"mysql -h db -P3306 -uroot -pfakepass123 -e "select 1""#,
            r#"mysql -h db -P3306 -uroot -p***REDACTED*** -e "select 1""#,
        ),
        // the single-line BEGIN marker keeps the rest of the command intact
        (
            "echo '-----BEGIN RSA PRIVATE KEY-----' > key.pem",
            "echo '***REDACTED***' > key.pem",
        ),
    ];

    #[test]
    fn test_redaction_table_must_redact() {
        for (input, expected) in MUST_REDACT {
            assert_eq!(&redact_secrets(input), expected, "input: {input}");
        }
    }

    /// A truncated PEM paste (no END line) must not leak its body.
    #[test]
    fn test_pem_body_without_end_line_redacted() {
        let cmd =
            "cat <<EOF > k.pem\n-----BEGIN RSA PRIVATE KEY-----\nZmFrZWZha2VmYWtl\nZmFrZWZha2Vm";
        let out = redact_secrets(cmd);
        assert!(!out.contains("ZmFrZWZha2Vm"), "leaked PEM body: {out}");
        assert!(
            out.starts_with("cat <<EOF > k.pem\n"),
            "prefix mangled: {out}"
        );
    }

    // ── Table-driven: commands that must be stored unchanged ──

    #[test]
    fn test_redaction_table_must_keep() {
        let cases = [
            // -P is mysql's port flag, not its password flag
            "mysql -h db.example.com -P3306 -u root -p mydb",
            "mysql -h db.example.com -P 3306 -u root mydb",
            // a later command's -p after a shell separator is not mysql's
            "mysql -u root -p mydb < dump.sql | perl -pe 's/foo/bar/'",
            "mysql -u root -p mydb; perl -pi -e 's/a/b/' f.txt",
            "mysqldump -u root -p db && tar -czpf backup.tgz dump",
            // mysql as an image name is not the client
            "docker run --rm -p3306:3306 -e MYSQL_ROOT_PASSWORD_FILE=/run/secrets/db mysql:8",
            // long flag values whose name merely ends in a keyword
            "mytool --sort-key=modification_time_then_name_ascending",
            "git log --grep=token:refresh_handling_for_expired_sessions",
            // `sk-` inside a word is not an OpenAI key
            "gcloud compute disks create my-disk-abcdefghij0123456789xyz",
            "kubectl delete task-abcdefghijklmnopqrstuvwxyz0123",
            // a URL in another command doesn't make -u a password flag
            "docker run -e URL=https://example.com -u 1000:1000 alpine id",
            // URLs without userinfo credentials
            "curl http://localhost:3000/users/@alice",
            "open https://example.com:8443/path?user=a@b.com",
            "git clone ssh://git@github.com:22/org/repo.git",
            "curl -H \"Accept: application/json\" https://example.com",
            // stdin / file based secrets have nothing to redact
            "docker login --password-stdin registry.example.com",
            "sshpass -f ~/.pw ssh -p 2222 host",
            "sshpass -e ssh -p 2222 host",
            "aws configure set region us-east-1",
            "ssh-keygen -t ed25519 -C me@example.com",
            "curl -o out.html https://example.com",
        ];
        for cmd in cases {
            assert_eq!(redact_secrets(cmd), cmd, "false positive on: {cmd}");
        }
    }

    /// Every row is checked before the test fails, so one run shows every miss.
    fn assert_redacts(table: &[(&str, &str)]) {
        let misses: Vec<String> = table
            .iter()
            .filter_map(|(input, want)| {
                let got = redact_secrets(input);
                (got != *want).then(|| format!("\n  in:   {input}\n  got:  {got}\n  want: {want}"))
            })
            .collect();
        assert!(
            misses.is_empty(),
            "{} rows:{}",
            misses.len(),
            misses.concat()
        );
    }

    fn assert_keeps(commands: &[&str]) {
        let misses: Vec<String> = commands
            .iter()
            .filter_map(|cmd| {
                let got = redact_secrets(cmd);
                (got != *cmd).then(|| format!("\n  in:   {cmd}\n  got:  {got}"))
            })
            .collect();
        assert!(
            misses.is_empty(),
            "{} rows:{}",
            misses.len(),
            misses.concat()
        );
    }

    /// Masking only a JWT's header would hide the one part that isn't
    /// sensitive and store the payload and signature beside the placeholder.
    #[test]
    fn test_jwt_is_redacted_whole() {
        assert_redacts(&[
            (
                r#"curl -d '{"access_token":"eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJmYWtlIn0.ZmFrZXNpZ25hdHVyZQ"}' https://example.com"#,
                r#"curl -d '{"access_token":"***REDACTED***"}' https://example.com"#,
            ),
            (
                "echo eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJmYWtlIn0.ZmFrZXNpZ25hdHVyZQ",
                "echo ***REDACTED***",
            ),
            // alg=none: the signature segment is empty
            (
                "echo eyJhbGciOiJub25lIn0.eyJzdWIiOiJmYWtlIn0. done",
                "echo ***REDACTED*** done",
            ),
            // An encrypted JWT has five segments
            (
                "echo eyJhbGciOiJSU0EtT0FFUCJ9.Zm9v.YmFy.YmF6.cXV4",
                "echo ***REDACTED***",
            ),
        ]);
    }

    /// Any dotted value after a secret-ish key, not only a JWT, has to go in
    /// one piece: bot tokens are three dotted segments too.
    #[test]
    fn test_dotted_value_after_key_is_redacted_whole() {
        assert_redacts(&[(
            r#"curl -d '{"token":"XXXXXXXXXXXXXXXXXXXXXXXXXXXX.XXXXXX.XXXXXXXXXXXXXXXXXXXXXXXXXXX"}' https://example.com"#,
            r#"curl -d '{"token":"***REDACTED***"}' https://example.com"#,
        )]);
    }

    /// Base64 of a JSON object starts `eyJ` as well; with no dots it is data.
    #[test]
    fn test_base64_json_is_not_a_jwt() {
        assert_keeps(&["echo eyJmb28iOiJiYXIiLCJiYXoiOjF9 | base64 -d"]);
    }

    /// A password is usually far shorter than the 24 characters the
    /// long-value rule asks for, and is no less a secret for it.
    #[test]
    fn test_short_secret_under_a_secret_name_is_redacted() {
        assert_redacts(&[
            // JSON bodies
            (
                r#"curl -d '{"email":"a@b.c","password":"hunter2"}' https://example.com/login"#,
                r#"curl -d '{"email":"a@b.c","password":"***REDACTED***"}' https://example.com/login"#,
            ),
            (
                r#"curl -d '{"password": "two words"}' https://example.com"#,
                r#"curl -d '{"password": "***REDACTED***"}' https://example.com"#,
            ),
            (
                r#"curl -d '{"apiKey":"short1","client_secret":"short2"}' https://example.com"#,
                r#"curl -d '{"apiKey":"***REDACTED***","client_secret":"***REDACTED***"}' https://example.com"#,
            ),
            (
                r#"curl -d '{"token":"short1","accessToken":"short2"}' https://example.com"#,
                r#"curl -d '{"token":"***REDACTED***","accessToken":"***REDACTED***"}' https://example.com"#,
            ),
            // JSON inside a double-quoted shell argument
            (
                r#"curl -d "{\"password\":\"hunter2\"}" https://example.com"#,
                r#"curl -d "{\"password\":\"***REDACTED***\"}" https://example.com"#,
            ),
            // Quotes inside the value don't end the masking early
            (
                r#"curl -d '{"password":"a\"b"}' https://example.com"#,
                r#"curl -d '{"password":"***REDACTED***"}' https://example.com"#,
            ),
            (
                r#"http POST example.com '{"password":"it's"}'"#,
                r#"http POST example.com '{"password":"***REDACTED***"}'"#,
            ),
            // Code
            (
                r#"python3 -c "login({'password': 'hunter2'})""#,
                r#"python3 -c "login({'password': '***REDACTED***'})""#,
            ),
            (
                "python3 - <<'EOF'\npassword = \"hunter2\"\nEOF",
                "python3 - <<'EOF'\npassword = \"***REDACTED***\"\nEOF",
            ),
            (
                r#"node -e "const token = 'short1'""#,
                r#"node -e "const token = '***REDACTED***'""#,
            ),
            (
                r#"python3 -c "os.environ['API_KEY'] = 'short1'""#,
                r#"python3 -c "os.environ['API_KEY'] = '***REDACTED***'""#,
            ),
            (
                r#"go run . -e 'token := "short1"'"#,
                r#"go run . -e 'token := "***REDACTED***"'"#,
            ),
            (
                r#"php -r "login(['password' => 'hunter2']);""#,
                r#"php -r "login(['password' => '***REDACTED***']);""#,
            ),
            // YAML written by heredoc or echo
            (
                "cat > config.yml <<'EOF'\ndb:\n  password: hunter2\n  host: db\nEOF",
                "cat > config.yml <<'EOF'\ndb:\n  password: ***REDACTED***\n  host: db\nEOF",
            ),
            (
                "echo 'password: hunter2' >> config.yml",
                "echo 'password: ***REDACTED***' >> config.yml",
            ),
            (
                r#"echo "api_key: short1" >> config.yml"#,
                r#"echo "api_key: ***REDACTED***" >> config.yml"#,
            ),
            (
                r"printf 'token: short1\n' > t.yml",
                "printf 'token: ***REDACTED***' > t.yml",
            ),
        ]);
    }

    /// Searches for these names, and ordinary text about them, are not
    /// secrets. Masking can't be undone, so each of these would be a command
    /// that no longer says what ran.
    #[test]
    fn test_text_about_secrets_is_kept() {
        assert_keeps(&[
            // Searches
            r#"grep -n '"password":' src/auth.py | sed 's/x/y/'"#,
            r#"rg "password: " src/ | rg "foo""#,
            "rg 'token = ' src | head",
            "grep -rn api_key src",
            "jq '.token' response.json",
            r#"rg "password: required" docs/"#,
            r#"git log --grep="token: rotation""#,
            // Prose
            r#"git commit -m "fix: password: must be 8 chars""#,
            r#"git commit -m "token: rotated""#,
            // Names that end in something other than the secret word
            r#"curl -d '{"max_tokens":"100"}' https://example.com"#,
            r#"curl -d '{"token_type":"bearer"}' https://example.com"#,
            r#"curl -d '{"password_hint":"pet"}' https://example.com"#,
            // Names too ordinary to treat a short value as secret
            r#"curl -d '{"key":"id","sort_key":"name","auth":"oauth"}' https://example.com"#,
            // `pwd` is the working directory far more often than a password
            r#"curl -d '{"pwd":"/srv/app"}' https://example.com"#,
            // Values that aren't strings
            r#"curl -d '{"password":null,"token":true,"secret":1}' https://example.com"#,
        ]);
    }

    #[test]
    fn test_credential_flags_are_redacted() {
        assert_redacts(&[
            (
                "az ad sp credential reset --client-secret fakevalue1",
                "az ad sp credential reset --client-secret ***REDACTED***",
            ),
            (
                "tool --auth-token=fakevalue1",
                "tool --auth-token=***REDACTED***",
            ),
            (
                "tool --access-token fakevalue1 --verbose",
                "tool --access-token ***REDACTED*** --verbose",
            ),
            (
                "tool --api-token fakevalue1",
                "tool --api-token ***REDACTED***",
            ),
            (
                "tool --github-token fakevalue1",
                "tool --github-token ***REDACTED***",
            ),
            (
                "tool --refresh_token fakevalue1",
                "tool --refresh_token ***REDACTED***",
            ),
            (
                "gpg --batch --passphrase fakevalue1 -d f.gpg",
                "gpg --batch --passphrase ***REDACTED*** -d f.gpg",
            ),
            ("tool --pass fakevalue1", "tool --pass ***REDACTED***"),
            ("tool --passwd=fakevalue1", "tool --passwd=***REDACTED***"),
            (
                "curl --oauth2-bearer fakevalue1 https://example.com",
                "curl --oauth2-bearer ***REDACTED*** https://example.com",
            ),
            (
                "wget --http-password=fakevalue1 https://example.com/f",
                "wget --http-password=***REDACTED*** https://example.com/f",
            ),
            (
                "wget --ftp-password fakevalue1 ftp://example.com/f",
                "wget --ftp-password ***REDACTED*** ftp://example.com/f",
            ),
            (
                r#"tool --client-secret "two words" --verbose"#,
                r#"tool --client-secret "***REDACTED***" --verbose"#,
            ),
        ]);
    }

    /// Each of these takes no value, or a value that isn't a secret. Treating
    /// them as credential flags would lose the argument that follows.
    #[test]
    fn test_flags_that_only_look_like_credentials_are_kept() {
        assert_keeps(&[
            "gh auth login --with-token < token.txt",
            "psql --no-password -h db",
            "tool --token-file ~/.token --host db",
            "tool --password-file ~/.pw --host db",
            "gpg --passphrase-file pw.txt -d f.gpg",
            "gpg --passphrase-fd 0 -d f.gpg",
            "tool --pwd /srv/app",
            "tool --pass --verbose",
            "tool --pass-through --host db",
            "llm --max-tokens 100",
            "kubectl run x --image-pull-secret regcred",
        ]);
    }

    #[test]
    fn test_secret_headers_are_redacted() {
        assert_redacts(&[
            (
                r#"curl -H "x-api-key: short1" https://example.com"#,
                r#"curl -H "x-api-key: ***REDACTED***" https://example.com"#,
            ),
            (
                r#"curl -H "x-api-key:short1" https://example.com"#,
                r#"curl -H "x-api-key:***REDACTED***" https://example.com"#,
            ),
            (
                r#"curl -H "X-Auth-Token: short1" -H "X-Api-Key: short2" https://example.com"#,
                r#"curl -H "X-Auth-Token: ***REDACTED***" -H "X-Api-Key: ***REDACTED***" https://example.com"#,
            ),
            (
                "curl -H 'PRIVATE-TOKEN: short1' https://gitlab.example.com/api",
                "curl -H 'PRIVATE-TOKEN: ***REDACTED***' https://gitlab.example.com/api",
            ),
            (
                r#"wget --header="X-Auth-Token: short1" https://example.com"#,
                r#"wget --header="X-Auth-Token: ***REDACTED***" https://example.com"#,
            ),
            (
                r#"curl -H "Authorization: Bot short1" https://discord.com/api"#,
                r#"curl -H "Authorization: Bot ***REDACTED***" https://discord.com/api"#,
            ),
            (
                r#"curl -H "Authorization: ApiKey short1" https://example.com"#,
                r#"curl -H "Authorization: ApiKey ***REDACTED***" https://example.com"#,
            ),
            (
                r#"curl -H "Authorization: short1" https://example.com"#,
                r#"curl -H "Authorization: ***REDACTED***" https://example.com"#,
            ),
            (
                r#"curl -H "Authorization: AWS4-HMAC-SHA256 Credential=short1/x, Signature=short2" https://example.com"#,
                r#"curl -H "Authorization: AWS4-HMAC-SHA256 ***REDACTED***" https://example.com"#,
            ),
            (
                r#"curl -H "Proxy-Authorization: Digest short1" https://example.com"#,
                r#"curl -H "Proxy-Authorization: Digest ***REDACTED***" https://example.com"#,
            ),
            (
                r#"python3 -c "requests.get(u, headers={'Authorization': 'Bot short1'})""#,
                r#"python3 -c "requests.get(u, headers={'Authorization': 'Bot ***REDACTED***'})""#,
            ),
        ]);
    }

    #[test]
    fn test_cookies_are_redacted() {
        assert_redacts(&[
            (
                r#"curl -H "Cookie: sessionid=short1; theme=dark" https://example.com"#,
                r#"curl -H "Cookie: ***REDACTED***" https://example.com"#,
            ),
            (
                r#"curl -b "session=short1" https://example.com"#,
                r#"curl -b "***REDACTED***" https://example.com"#,
            ),
            (
                "curl --cookie 'a=1; b=2' https://example.com",
                "curl --cookie '***REDACTED***' https://example.com",
            ),
            (
                "curl -b session=short1 https://example.com",
                "curl -b ***REDACTED*** https://example.com",
            ),
        ]);
    }

    #[test]
    fn test_secret_query_params_are_redacted() {
        assert_redacts(&[
            (
                r#"curl "https://example.com/x?key=short1""#,
                r#"curl "https://example.com/x?key=***REDACTED***""#,
            ),
            (
                r#"curl "https://example.com/x?a=1&apikey=short1&b=2""#,
                r#"curl "https://example.com/x?a=1&apikey=***REDACTED***&b=2""#,
            ),
            (
                r#"curl "https://acct.blob.core.windows.net/c/f?sv=2024&sig=short1""#,
                r#"curl "https://acct.blob.core.windows.net/c/f?sv=2024&sig=***REDACTED***""#,
            ),
            (
                r#"curl "https://b.s3.amazonaws.com/k?X-Amz-Signature=short1&X-Amz-Date=2026""#,
                r#"curl "https://b.s3.amazonaws.com/k?X-Amz-Signature=***REDACTED***&X-Amz-Date=2026""#,
            ),
            (
                "curl https://storage.googleapis.com/b/k?X-Goog-Signature=short1",
                "curl https://storage.googleapis.com/b/k?X-Goog-Signature=***REDACTED***",
            ),
        ]);
    }

    /// Searching for a header is not sending one, a `-b` without `=` names a
    /// cookie file, and a code header built from a variable holds no secret.
    #[test]
    fn test_header_shaped_text_is_kept() {
        assert_keeps(&[
            r#"grep -rn "Authorization: Bearer" src/"#,
            "rg 'Authorization: Basic' .",
            r#"rg "Cookie: session" src/"#,
            "curl -b cookies.txt https://example.com",
            "curl -c cookies.txt -b cookies.txt https://example.com",
            r#"curl -H "Content-Type: application/json" -H "X-Request-Id: abc" https://example.com"#,
            r#"python3 -c "h = {'Authorization': f'Bearer {token}'}""#,
            r#"curl "https://example.com/x?keyword=foo&monkey=1&sort_key=name""#,
        ]);
    }

    #[test]
    fn test_short_env_names_are_redacted() {
        assert_redacts(&[
            (
                "DB_PASS=hunter2 ./migrate",
                "DB_PASS=***REDACTED*** ./migrate",
            ),
            ("PASS=hunter2 ./migrate", "PASS=***REDACTED*** ./migrate"),
            (
                r#"DB_PASS="two words" ./migrate"#,
                r#"DB_PASS="***REDACTED***" ./migrate"#,
            ),
            (
                "MYSQL_PWD=hunter2 mysql -u root",
                "MYSQL_PWD=***REDACTED*** mysql -u root",
            ),
            (
                "DB_PWD=hunter2 ./migrate",
                "DB_PWD=***REDACTED*** ./migrate",
            ),
            ("export APIKEY=short1", "export APIKEY=***REDACTED***"),
            (
                "docker run -e PASS=hunter2 app",
                "docker run -e PASS=***REDACTED*** app",
            ),
            (
                "cat > .env <<'EOF'\nDB_PASS=hunter2\nEOF",
                "cat > .env <<'EOF'\nDB_PASS=***REDACTED***\nEOF",
            ),
        ]);
    }

    /// `PWD` and `PASS` turn up inside and beside ordinary names, and a bare
    /// `*_KEY=` is as often `SORT_KEY=name` as a secret.
    #[test]
    fn test_env_names_that_only_look_secret_are_kept() {
        assert_keeps(&[
            "PWD=/srv/app ./run",
            "OLDPWD=/srv ./run",
            "BYPASS=true ./run",
            "COMPASS=north ./run",
            "PASS_COUNT=3 ./run",
            "SORT_KEY=name ./run",
        ]);
    }

    /// Fakes are split so secret scanners don't take them for real tokens.
    #[test]
    fn test_more_vendor_tokens_are_redacted() {
        assert_redacts(&[
            (
                concat!("echo AI", "zaXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"),
                "echo ***REDACTED***",
            ),
            (
                concat!("echo gl", "pat-XXXXXXXXXXXXXXXXXXXX"),
                "echo ***REDACTED***",
            ),
            (
                concat!("echo sk-", "or-v1-XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"),
                "echo ***REDACTED***",
            ),
            (
                concat!("echo hf", "_XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"),
                "echo ***REDACTED***",
            ),
            (
                concat!("echo gs", "k_XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"),
                "echo ***REDACTED***",
            ),
            (
                concat!("echo xa", "i-XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"),
                "echo ***REDACTED***",
            ),
            (
                concat!("echo do", "p_v1_XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"),
                "echo ***REDACTED***",
            ),
            (
                concat!("echo S", "G.XXXXXXXXXXXXXXXXXXXXXX.XXXXXXXXXXXXXXXXXXXXXX"),
                "echo ***REDACTED***",
            ),
            (
                concat!("echo li", "n_api_XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"),
                "echo ***REDACTED***",
            ),
            (
                concat!("echo nt", "n_XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"),
                "echo ***REDACTED***",
            ),
            (
                concat!("echo sn", "trys_XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"),
                "echo ***REDACTED***",
            ),
        ]);
    }

    /// The path of these URLs is the credential; the host stays readable.
    #[test]
    fn test_secret_urls_are_redacted() {
        assert_redacts(&[
            (
                concat!(
                    "curl -X POST https://hooks.sl",
                    "ack.com/services/T00000000/B00000000/XXXXXXXXXXXXXXXXXXXXXXXX"
                ),
                concat!("curl -X POST https://hooks.sl", "ack.com/services/***REDACTED***"),
            ),
            (
                concat!(
                    "export SLACK_WEBHOOK_URL=https://hooks.sl",
                    "ack.com/services/T00000000/B00000000/XXXXXXXXXXXXXXXXXXXXXXXX"
                ),
                concat!(
                    "export SLACK_WEBHOOK_URL=https://hooks.sl",
                    "ack.com/services/***REDACTED***"
                ),
            ),
            (
                "curl -X POST https://discord.com/api/webhooks/123456789012345678/XXXXXXXXXXXX",
                "curl -X POST https://discord.com/api/webhooks/123456789012345678/***REDACTED***",
            ),
            (
                "curl https://api.telegram.org/bot123456789:XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX/getMe",
                "curl https://api.telegram.org/bot***REDACTED***/getMe",
            ),
        ]);
    }

    #[test]
    fn test_ordinary_names_near_vendor_prefixes_are_kept() {
        assert_keeps(&[
            "echo hf_model_name_for_the_new_release",
            "ollama run xai-grok-2-latest",
            "git checkout glpat-rotation-docs",
            "curl https://hooks.slack.com/commands/T00000000/123",
        ]);
    }

    #[test]
    fn test_tool_password_flags_are_redacted() {
        assert_redacts(&[
            ("redis-cli -a hunter2 ping", "redis-cli -a ***REDACTED*** ping"),
            (
                "redis-cli -h db -a 'two words' ping",
                "redis-cli -h db -a '***REDACTED***' ping",
            ),
            (
                "mongosh -u root -p hunter2 mongodb://localhost",
                "mongosh -u root -p ***REDACTED*** mongodb://localhost",
            ),
            (
                "mongodump -u root -p hunter2 --db app",
                "mongodump -u root -p ***REDACTED*** --db app",
            ),
            ("sqlcmd -S db -U sa -P hunter2", "sqlcmd -S db -U sa -P ***REDACTED***"),
            (
                "ldapsearch -D cn=admin -w hunter2",
                "ldapsearch -D cn=admin -w ***REDACTED***",
            ),
            ("zip -P hunter2 out.zip a", "zip -P ***REDACTED*** out.zip a"),
            ("unzip -P hunter2 out.zip", "unzip -P ***REDACTED*** out.zip"),
            (
                "openssl enc -aes-256-cbc -pass pass:hunter2 -in x",
                "openssl enc -aes-256-cbc -pass pass:***REDACTED*** -in x",
            ),
            (
                "openssl rsa -passin pass:hunter2 -passout pass:hunter3 -in k",
                "openssl rsa -passin pass:***REDACTED*** -passout pass:***REDACTED*** -in k",
            ),
            (
                "az login --service-principal -u id -p hunter2 --tenant t",
                "az login --service-principal -u id -p ***REDACTED*** --tenant t",
            ),
            (
                "gh secret set API_KEY --body short1",
                "gh secret set API_KEY --body ***REDACTED***",
            ),
            (
                "gh secret set API_KEY -b short1 --repo a/b",
                "gh secret set API_KEY -b ***REDACTED*** --repo a/b",
            ),
            (
                r#"gh secret set API_KEY --body "two words""#,
                r#"gh secret set API_KEY --body "***REDACTED***""#,
            ),
            (
                "fly secrets set STRIPE_WEBHOOK=short1 OTHER=short2 && fly deploy",
                "fly secrets set ***REDACTED*** && fly deploy",
            ),
            (
                "kubectl create secret generic db --from-literal=pass=hunter2 --from-literal=user=admin",
                "kubectl create secret generic db --from-literal=pass=***REDACTED*** --from-literal=user=***REDACTED***",
            ),
        ]);
    }

    /// The same flags mean something else on other commands, or are followed
    /// by a reference to a secret instead of the secret.
    #[test]
    fn test_tool_flags_that_hold_no_password_are_kept() {
        assert_keeps(&[
            "redis-cli -h db -p 6379 ping",
            "redis-cli ping; ls -a /tmp",
            "ls -a /tmp",
            "docker run -a stdout alpine",
            "mongosh --port 27017",
            "zip -r out.zip dir",
            "unzip -l out.zip",
            "gzip -k file",
            "az login --use-device-code",
            "sqlcmd -S db -Q 'select 1'",
            "gh secret list",
            "gh secret set API_KEY < key.txt",
            "gh secret set API_KEY --body-file key.txt",
            "openssl enc -pass env:PW -in x",
            "openssl enc -pass file:pw.txt -in x",
            "fly secrets list",
            "fly secrets unset OLD_NAME",
        ]);
    }

    /// Each of these came out of running the rules over a real history, and
    /// each is a command that would no longer say what ran.
    #[test]
    fn test_near_misses_from_real_commands_are_kept() {
        assert_keeps(&[
            // A directory, not a password
            r#"_SUVADU_START_PWD="$PWD""#,
            "BUILD_PWD=/srv/app ./run",
            // A search for the assignment: the quote after `=` closes the
            // pattern and opens nothing
            r#"P1=$(grep "^DB_PASS=" .env | cut -d= -f2); P2=$(grep "^X=" b)"#,
            r#"grep "PASSWORD=" .env | grep "foo""#,
            // JMESPath filters aren't query strings
            "aws ec2 describe-instances --query 'Reservations[*].Tags[?Key==`Name`].Value'",
            "aws sqs get-queue-attributes --query 'Attributes[?Key==`x`]' 2>&1",
            // A string that ends straight after the header name
            r#"let s = "authorization: " + token + "x""#,
            // The same for a secret name, with the next argument after it
            r#"print("with token:", "OK" if ok else "FAIL")"#,
            // Types under a secret name, in source written by heredoc
            "cat > m.py <<'EOF'\nclass Req(BaseModel):\n    token: str\n    api_key: str\nEOF",
            "cat > a.ts <<'EOF'\n  password: string;\nEOF",
            "cat > a.rs <<'EOF'\n    token: String,\n    secret: &str,\nEOF",
            // An OpenAPI example value, and a GitHub workflow permission
            r#"curl -d '{"token":"string"}' https://example.com"#,
            "cat > ci.yml <<'EOF'\npermissions:\n  id-token: write\nEOF",
        ]);
    }

    /// One command per trigger, holding no other. A trigger missing from the
    /// list leaves its rule unreachable and a secret stored as typed, and
    /// nothing else would show that: other tables' commands hold several.
    #[test]
    fn test_each_trigger_reaches_its_rule() {
        let commands = [
            "DB_PASS=fake",
            "ROOT_PWD=fake",
            "SECRET=fake",
            "TOKEN=fake",
            "CREDENTIAL=fake",
            "AUTH=fake",
            "PRIVATE_KEY=fake",
            r#"tool -H "Cookie: a=fake""#,
            "docker login -p fake r",
            "mysql -pfake db",
            "mariadb -pfake db",
            "redis-cli -a fake ping",
            "mongosh -p fake",
            "sqlcmd -P fake",
            "ldapsearch -w fake",
            "zip -P fake a f",
            "kubectl create configmap c --from-literal=a=fake",
            "curl -u a:fake example.com",
            "wget --user a:fake example.com",
            "http -a u:fake example.com",
            "xh -a u:fake example.com",
            "ftp://a:fake@example.com",
            "echo AKIAXXXXXXXXXXXXXXXX",
            "echo ASIAXXXXXXXXXXXXXXXX",
            concat!("echo AI", "zaXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"),
            "echo eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJmYWtlIn0.ZmFrZQ",
            concat!("echo xo", "xb-XXXXXXXXXX"),
            concat!("echo xa", "pp-1-XXXXXXXXXXXX"),
            "echo ghp_XXXXXXXXXXXXXXXXXXXXXXXX",
            "echo gho_XXXXXXXXXXXXXXXXXXXXXXXX",
            "echo ghu_XXXXXXXXXXXXXXXXXXXXXXXX",
            "echo ghs_XXXXXXXXXXXXXXXXXXXXXXXX",
            "echo ghr_XXXXXXXXXXXXXXXXXXXXXXXX",
            "echo github_pat_XXXXXXXXXXXXXXXXXXXXXXXX",
            "echo sk-XXXXXXXXXXXXXXXXXXXXXXXX",
            concat!("echo sk", "_live_XXXXXXXXXXXXXXXXXXXXXXXX"),
            concat!("echo rk", "_live_XXXXXXXXXXXXXXXXXXXXXXXX"),
            "echo npm_XXXXXXXXXXXXXXXXXXXXXXXX",
            "echo pypi-XXXXXXXXXXXXXXXXXXXXXXXX",
            "echo hf_XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX",
            "echo gsk_XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX",
            "echo ntn_XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX",
            "echo dop_v1_XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX",
            "echo lin_api_XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX",
            "echo xai-XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX",
            "echo sntrys_XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX",
            concat!("echo S", "G.XXXXXXXXXXXXXXXXXXXX.XXXXXXXXXXXXXXXXXXXX"),
            "echo glpat-XXXXXXXXXXXXXXXXXXXXXXXX",
            "echo gldt-XXXXXXXXXXXXXXXXXXXXXXXX",
            "echo glrt-XXXXXXXXXXXXXXXXXXXXXXXX",
            "echo glptt-XXXXXXXXXXXXXXXXXXXXXXXX",
            "echo glcbt-XXXXXXXXXXXXXXXXXXXXXXXX",
            "echo glsoat-XXXXXXXXXXXXXXXXXXXXXXXX",
            concat!("hooks.sl", "ack.com/services/T0/B0/fake"),
            "discord.com/api/webhooks/1/fake",
            "api.telegram.org/bot1:fake",
            "open example.com/a?sig=fake",
            "open example.com/a?signature=fake",
        ];
        let reaching = |trigger: &str, cmd: &str| cmd.to_ascii_lowercase().contains(trigger);
        let mut problems = Vec::new();
        for cmd in commands {
            let held: Vec<&str> = TRIGGERS
                .iter()
                .copied()
                .filter(|t| reaching(t, cmd))
                .collect();
            if held.len() != 1 {
                problems.push(format!("holds {held:?}: {cmd}"));
            }
            if redact_secrets(cmd) == cmd {
                problems.push(format!("no rule reached: {cmd}"));
            }
        }
        for trigger in TRIGGERS {
            if !commands.iter().any(|cmd| reaching(trigger, cmd)) {
                problems.push(format!("no command for: {trigger}"));
            }
        }
        assert!(problems.is_empty(), "\n{}", problems.join("\n"));
    }

    /// The point of the trigger list: these are what a shell mostly runs, and
    /// each must not cost a compile of every rule.
    #[test]
    fn test_ordinary_commands_are_not_candidates() {
        for cmd in [
            "ls -la /tmp",
            "git status",
            "git push origin main",
            "cd /home/user/projects",
            "cargo test --lib",
            "npm run build",
            "mkdir -p src/app",
            "docker run -p 8080:80 nginx",
        ] {
            assert!(!may_hold_secret(cmd), "{cmd}");
        }
    }

    /// Consolidated corpus: each (input, `must_be_absent`) pair locks in a known
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
            (
                r"http POST api.com token:abcdefGHIJKLmnop12345678=",
                "abcdefGHIJKLmnop12345678",
            ),
        ];
        for (cmd, secret) in must_redact {
            let out = redact_secrets(cmd);
            assert!(
                !out.contains(secret),
                "FAILED to redact `{secret}` in: {cmd} -> {out}"
            );
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
