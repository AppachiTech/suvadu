# Security Policy

## Reporting Vulnerabilities

If you discover a security vulnerability in Suvadu, please report it responsibly:

1. **Do NOT open a public GitHub issue**
2. Email: **madhu@appachi.tech**
3. Include: description, steps to reproduce, and potential impact

We aim to acknowledge reports within 48 hours and provide a fix timeline within 7 days.

## Security Design

### Data Storage

- All history is stored **locally** in a SQLite database (WAL mode)
- Default locations:
  - macOS: `~/Library/Application Support/tech.appachi.suvadu/history.db`
  - Linux: `~/.local/share/suvadu/history.db`
- **No data is transmitted to external servers**
- **No telemetry or analytics** are collected

#### Data at rest

- Command history is stored **unencrypted** in the SQLite file. Secret
  redaction (below) is applied before writing, but the database itself is
  plaintext — treat it like your shell history file.
- On Unix the data directory is `0o700` and the database, config, prompt
  cache, alias file, and backups are written `0o600` (owner-only).
- `suv backup` and the automatic snapshot taken before `suv delete` write
  copies into `<data_dir>/backups/`. These copies are **also unencrypted** —
  prune or move them if needed; `--no-backup` skips the pre-delete snapshot.
- Cross-machine transfer uses explicit `suv export` / `suv import`. The export
  file is plaintext JSONL; protect it in transit yourself (e.g. `scp`, an
  encrypted volume).

### Privacy Features

- Commands prefixed with a **space** are never recorded
- Configurable **exclusion patterns** (regex or substring) to ignore sensitive commands
- `suv delete` for bulk removal of entries matching a pattern
- `suv pause` for temporary recording suspension (per-shell)
- `suv disable` for global recording opt-out

### Secret Redaction

Enabled by default; disable with `redaction.enabled = false` in the config.
Detected secret **values** are replaced with `***REDACTED***` before the
command is stored. Coverage includes:

- Sensitive environment-variable assignments (`*_TOKEN=`, `*_SECRET=`,
  `*_PASSWORD=`, `AUTH=`, `*_API_KEY=`, …), gated so names like `AUTHOR_NAME`
  or `PASSWORD_FILE` are not false-positived
- Password CLI flags: `--password`, `--token`, `--secret`, `--api-key`, and
  `-p<pw>` scoped to DB clients (`mysql`/`mysqldump`/`mariadb`)
- Well-known key formats: AWS (`AKIA…`), GitHub (`ghp_…`), OpenAI/Anthropic
  (`sk-…`), Slack, Stripe, npm, PyPI, Azure, and PEM private keys
- `Authorization:` headers (Bearer/Basic/token) and `curl -u user:pass`
  (scoped to HTTP clients, so `docker run -u 1000:1000` is left intact)
- Database connection-string passwords (`postgres://user:pass@host`),
  including passwords containing `@` and password-only URIs
- Long hex / base64 secrets that follow a secret-ish key name

**Limitations** — redaction is best-effort pattern matching, not a guarantee:

- Novel or custom secret formats may not be detected. Use exclusion patterns
  (or a space prefix) for commands you never want recorded.
- Redaction applies only to **newly recorded** commands; it does not
  retroactively rewrite already-stored history.
- To keep recalled commands runnable, non-secret flags that merely look
  password-shaped (e.g. `docker run -p 8080:80`, `ssh -p 2222`) are
  intentionally **not** redacted.

### Self-Update

- Binary downloads are served over **HTTPS** from `downloads.appachi.tech`
- Downloads are verified with a **minisign signature** (the public key is compiled into the binary, so a compromised download server cannot forge updates) and a **SHA256 checksum**
- Update files are written to a unique temporary directory to prevent TOCTOU attacks
- Homebrew installs are handled through the official Homebrew tap

### Shell Hooks

- Shell hooks are installed via `eval "$(suv init zsh)"` or `eval "$(suv init bash)"`
- Hooks only capture: command text, working directory, exit code, timestamps, and executor type
- No environment variables, arguments to other programs, or file contents are recorded

## Supported Versions

| Version | Supported          |
|---------|--------------------|
| Latest  | Yes                |
| < Latest | Best-effort       |
