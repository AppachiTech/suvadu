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
  copies into `<data_dir>/backups/`. These copies are **also unencrypted**,
  and nothing prunes them — they accumulate until you delete them yourself;
  `--no-backup` skips the pre-delete snapshot. A backup restores by copying
  the file over `history.db`: it carries the whole database, including agent
  sessions and summaries, not just command history.
- Cross-machine transfer uses explicit `suv export` / `suv import`. The export
  file is plaintext JSONL; protect it in transit yourself (e.g. `scp`, an
  encrypted volume).

### Privacy Features

- Commands prefixed with a **space** are never recorded
- Configurable **exclusion patterns** (regex or substring) to ignore sensitive commands
- `suv delete` removes entries matching a pattern from the database — this is
  a delete, **not a secure erase** (see *Deleting data*)
- `suv pause` for temporary recording suspension (per-shell)
- `suv disable` for global recording opt-out

### What is stored, and how long

Nothing is deleted on a schedule: Suvadu has no retention timer, no size cap
and no automatic pruning. Everything below is kept until you remove it.
`suv doctor` prints the current size of each category next to the rule that
governs it.

| Category | Holds | Goes away when |
|---|---|---|
| Commands | command text, directory, exit code, timings, plus their notes, bookmarks, tags and the search index | you run `suv delete <pattern>` |
| Sessions | shell sessions, and agent sessions with their captured events and adapter checkpoints | an agent session is removed with `suv agent delete-session <id>`; a shell session row stays behind after its commands are deleted |
| Summaries | session summaries written over MCP | their agent session is deleted — editing or deleting commands only marks a summary *stale*, it does not remove it |
| Skills | shared skill documents | you run `suv skills remove`; a sync rewrites agent-side copies, not the stored row |
| Backups | full database copies from `suv backup` and the automatic pre-delete snapshot | **you delete the files yourself** |

How these interact:

- Deleting commands does not delete the agent sessions or summaries that
  referred to them. A summary whose evidence changed is reported as stale and
  keeps its text.
- Deleting an agent session *does* delete its captured events, its adapter
  checkpoint, its summaries and the shell commands recorded under it.
- Every `suv delete` writes a pre-delete backup first (unless `--no-backup`),
  so a delete that reduces the live database **adds** a full copy of the
  database as it was moments earlier. Backups accumulate until you remove
  them.

### Deleting data

`suv delete --dry-run` lists the commands a pattern would remove before
anything happens, and both the preview and the real delete print what is and
is not affected.

**A SQLite delete is not a secure erase.** After `suv delete` reports success:

- The deleted rows' pages remain inside `history.db` until SQLite reuses
  them. `suv doctor` reports this as reclaimable space; only a `VACUUM`
  rewrites the file without them.
- The pre-delete backup contains every command just deleted, and so does any
  earlier backup or `suv export` file.
- Filesystem snapshots, Time Machine backups and any copy you made are
  untouched.

To remove the last copies, delete the files in `<data_dir>/backups/` and any
exports yourself. On a copy-on-write or journalling filesystem, on an SSD, or
with full-disk snapshots in play, even that is not a guarantee that the bytes
are unrecoverable — full-disk encryption is the control that addresses that
threat, not a row delete.

### Redaction and exclusions by ingestion path

The same configuration governs every path that writes to the database, with
one documented exception.

| Path | Space-prefix skipped | Exclusions applied | Redaction applied |
|---|---|---|---|
| Live shell/agent recording (`suv add`, hooks) | yes | yes | yes |
| Bash import (`suv import --from bash-history`, including `--dry-run`) | yes | yes | yes |
| Zsh import (`suv import --from zsh-history`) | yes | **no** | **no** |
| JSONL import (`suv import`) | n/a | no — restoring a Suvadu export is meant to reproduce it exactly | no |
| Native transcript ingestion (Codex, Claude Code, OpenCode) | n/a | yes, per directory | yes, per directory |
| Session summaries saved over MCP | n/a | yes — matching text is refused, not trimmed | yes |

Known gap: the **Zsh importer stores what the file contains**. If
`~/.zsh_history` already holds secrets, importing it copies them into the
database verbatim, and `exclusions` are not consulted. Filter the file before
importing, or remove the entries afterwards with `suv delete` (and then the
backups, per *Deleting data*).

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

### Risk rules and `suv guard`

`suv guard` and the `assess_risk` MCP tool match **rules against the text of
a command**. That is all they do.

- **`suv guard` is not a sandbox.** It does not run, contain, intercept or
  supervise anything. It reads a string, prints a verdict and exits non-zero
  when the verdict is at or above the threshold. Whether that stops the
  command is entirely up to the caller that asked, and any caller is free to
  ignore it. It provides no protection against a program that is already
  running, and none at all against something that never asks it.
- A verdict reports three separate things: the rule's **severity**, the
  **matched text** (redacted and length-bounded, so a report never becomes
  the thing that copies a secret into a log), and any **uncertainty**. A
  literal match such as `rm -rf /srv` settles what the command does; a
  fetched script, an installed package or an `eval`'d string does not, and
  the verdict says so instead of implying otherwise.

Known limits of matching text, all verified by tests:

- A rule anchored to the start of a command does not see the second command
  of a chain: `git commit -m x && git push --force` is not flagged as a force
  push.
- A command built at runtime (`eval "$CMD"`, a decoded payload, a
  substitution) can only be reported as indirection, never as what it will
  become.
- Quoted text handed to a program that merely searches, prints or records
  strings (`grep`, `rg`, `git commit -m`, `git log`, `man`, `history`) is
  treated as a mention, not an action — but only when the command line chains
  nothing. `bash -c "rm -rf /"`, `ssh host 'rm -rf /srv'` and
  `psql -c "drop table users"` are still flagged.

#### Custom rules and overrides

Two config keys, both under `[agent]`, both lists of Rust regexes matched
against the raw command text:

- `risk_extra_patterns` — adds rules. Each entry has `pattern`, `level`
  (`low`/`medium`/`high`/`critical`) and an optional `description`. Matches
  are reported in the `custom` category, and always with uncertainty
  attached: Suvadu cannot vouch for what someone else's rule implies.
- `risk_ignore_patterns` — suppresses. A command matching one of these is
  treated as carrying no risk at all.

```toml
[agent]
risk_ignore_patterns = ["^rm -rf \\./build"]

[[agent.risk_extra_patterns]]
pattern = "^deploy-prod"
level = "critical"
description = "Production deploy script"
```

Precedence and scope:

1. `risk_ignore_patterns` wins over everything, built-in or custom.
2. Otherwise the **highest severity** among all matching rules wins, whether
   it came from a built-in rule or your own; on a tie the built-in rule's
   description is the one reported.
3. An invalid regex or an unknown level is skipped with a warning; the rest
   of the list still loads.
4. The lists are read from the configuration once per process, from the
   global config and any project `.suvadu.toml` that applies to the current
   directory. They affect `suv guard`, the risk columns in the TUI and the
   `assess_risk` MCP tool. They do not rewrite risk levels already shown in
   an earlier session, and they never change what is recorded.

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
