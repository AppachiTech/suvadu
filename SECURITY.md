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
- **No data is transmitted to external servers**
- **No telemetry or analytics** are collected

Every path below is resolved at runtime from the `tech.appachi.suvadu`
application identifier, except the agent hook scripts, which are written to
`~/.config/suvadu/hooks/` on **every** platform, macOS included.

| What | macOS | Linux |
|---|---|---|
| Data directory | `~/Library/Application Support/tech.appachi.suvadu/` | `$XDG_DATA_HOME/suvadu/` (default `~/.local/share/suvadu/`) |
| Config directory | the same directory as the data directory | `$XDG_CONFIG_HOME/suvadu/` (default `~/.config/suvadu/`) |
| Agent hook scripts | `~/.config/suvadu/hooks/` | `~/.config/suvadu/hooks/` |

Inside the data directory:

| File | Holds |
|---|---|
| `history.db` (+ `-wal`, `-shm`) | everything in the table below |
| `backups/` | full database copies — see *What is stored, and how long* |
| `prompts/` | the live prompt cache pairing an agent prompt with the commands run for it, as `<session>.prompt` files. The only thing anything prunes: `suv gc` deletes cache files older than 7 days |
| `aliases.sh` | the managed alias file your shell sources |
| `reconcile.key` | 32 random bytes, used to key the HMAC that links a command to its prompt without storing a bare hash of the prompt text |

`config.toml` lives in the config directory, and a per-project
`.suvadu.toml` lives wherever you put it in your own repositories.

`suv uninstall` prints the real config and data directories it did not
remove, resolved the same way — never a hardcoded guess.

#### Data at rest

- Command history is stored **unencrypted** in the SQLite file. Secret
  redaction (below) is applied before writing, but the database itself is
  plaintext — treat it like your shell history file.
- On Unix the data directory and `backups/` are `0o700`, and `history.db`,
  the prompt cache, `aliases.sh` and `reconcile.key` are written `0o600`
  (owner-only). On Linux the config directory is a separate directory and is
  created with no explicit mode, so it follows your umask.
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
| Commands | command text, directory, exit code, start/end times, duration, executor type and name, an optional tag, and a `context` JSON blob; plus their notes, bookmarks, aliases, tags and the trigram search index | you run `suv delete <pattern>` |
| Sessions | shell sessions (id, **machine hostname**, creation time, tag), and agent sessions with their captured transcript events and the adapter checkpoints that record which transcript files were read and how far | an agent session is removed with `suv agent delete-session <id>`; a shell session row stays behind after its commands are deleted |
| Summaries | session summaries written over MCP, with their cited evidence IDs, the session revision they were written from, and the caller-declared agent and model | their agent session is deleted — editing or deleting commands only marks a summary *stale*, it does not remove it |
| Skills | shared skill documents | you run `suv skills remove`; a sync rewrites agent-side copies, not the stored row |
| Backups | full database copies from `suv backup`, the automatic pre-delete snapshot, and the pre-import snapshot taken by `suv import --from atuin-db` | **you delete the files yourself** |

The `context` blob is empty for an ordinary typed command. It carries the
captured agent prompt and turn id for an agent-run command (re-redacted with
that command's own directory policy before it is stored), and import
provenance — source, import time, whether the timestamp was real or
synthetic, and which fields the source did not have — for an imported one.
An Atuin import also stores that row's free text (`atuin_intent`,
`atuin_author`, `atuin_shell`, `atuin_host`, `atuin_user`) there. Every one of
those passes through the same redaction and exclusion policy as the command
before it is written; a field an exclusion pattern matched is withheld
entirely and named in `context.withheld_fields`, so a missing field is
visible rather than silent.

How these interact:

- Deleting commands does not delete the agent sessions or summaries that
  referred to them. A summary whose evidence changed is reported as stale and
  keeps its text.
- Deleting an agent session *does* delete its captured events, its adapter
  checkpoint, its summaries and the shell commands recorded under it.
- Every `suv delete` writes a pre-delete backup first (unless `--no-backup`),
  so a delete that reduces the live database **adds** a full copy of the
  database as it was moments earlier. Backups accumulate until you remove
  them. `suv import --from atuin-db` takes one too; the JSONL, Bash and Zsh
  importers do not.

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

The same configuration governs every path that writes to the database.

| Path | Space-prefix skipped | Exclusions applied | Redaction applied |
|---|---|---|---|
| Live shell/agent recording (`suv add`, hooks) | yes | yes, per directory | yes, per directory |
| Bash import (`suv import --from bash-history`, including `--dry-run`) | yes | yes, global config only | yes, global config only |
| Zsh import (`suv import --from zsh-history`, including `--dry-run`) | yes | yes, global config only | yes, global config only |
| Atuin import (`suv import --from atuin-db`, including `--dry-run`) | yes | yes, per source directory — command **and** free-text metadata | yes, per source directory — command **and** free-text metadata |
| JSONL import (`suv import`) | n/a | no — restoring a Suvadu export is meant to reproduce it exactly | no |
| Native transcript ingestion (Codex, Claude Code, OpenCode) | n/a | yes, per directory | yes, per directory |
| Session summaries saved over MCP | n/a | yes — matching text is refused, not trimmed | yes |

**"Per directory" means the policy of the directory the command ran in** —
the global config with the nearest `.suvadu.toml` above that directory merged
on top. Live recording resolves it from the command's own `cwd`; the Atuin
importer resolves it from the `cwd` Atuin recorded for each row, so a project
overlay governs imported history exactly as it governs new history. A row
whose directory Atuin did not record, or which no longer exists on this
machine, falls back to the global config. The Bash and Zsh importers cannot
do this: their history files have no directory at all, so only the global
config applies to them, and a project overlay's extra patterns and exclusions
will **not** be honoured for what they import.

Redaction rewrites the text before it is stored, so a secret already sitting
in `~/.zsh_history` is redacted on the way in rather than copied verbatim. It
is still worth removing secrets at the source: redaction recognises known
patterns and your configured `redaction.extra_patterns`, and cannot promise to
catch a format it has never seen.

**One gap, in the prompt cache rather than the database.** Before an agent
prompt is attached to a command it is written to `<data_dir>/prompts/`. The
Claude Code, Codex and OpenCode prompt hooks redact and truncate it there.
The **Cursor** hook truncates but does not redact at cache time, and the
**pi.dev** extension truncates to 500 characters in JavaScript, bypassing
both `agent.prompt_capture_max_chars` and redaction. In both cases the
prompt is redacted again, with that command's own directory policy, before
it reaches the database — so what is *stored* is redacted, but the cache
file on disk may briefly hold the original text. Those files are `0o600` in
a `0o700` directory, and `suv gc` deletes cache files older than 7 days.

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
   global config merged with any project `.suvadu.toml` that applies to the
   process's working directory. They affect `suv guard`, the risk columns in
   the TUI and the `assess_risk` MCP tool. For the MCP server "working
   directory" means the directory the server process was started in, fixed
   for its whole lifetime — so a rule you add for one project does not
   follow a command the tool is asked about from another. They do not
   rewrite risk levels already shown in an earlier session, and they never
   change what is recorded.

### What an MCP client can reach

The MCP server opens the database **read-only**. Every one of its 21 tools,
8 resources and 6 prompts reads; a short-lived writable connection is opened
only for an explicitly enabled write.

There are exactly two write capabilities, both **off by default**:

| Tool | Opt-in | Effect when on |
|---|---|---|
| `save_session_summary` | `mcp.allow_session_summaries` | stores a summary the calling agent generated |
| `propose_skill` | `mcp.allow_skill_proposals` | stores a proposed skill as `pending_review` — never active, and never synced to an agent, until a human approves it in `suv skills` (`Ctrl+P`) |

A skill store agents can both read and write is a shared-memory poisoning
target, which is why the proposal gate is checked before any database
connection is opened at all. Turning an opt-in back off stops new writes; it
does not delete what was already stored.

**Summaries are caller-reported text.** Suvadu invokes no model and
generates no prose — a summary is whatever the connected agent wrote, stored
back and labelled `provenance: caller-reported`. The writing agent and model
are **caller-declared metadata, not verified identity**. Suvadu does check
what it can: the summary must cite evidence IDs that exist in that session,
must declare the session revision it was written from, and is rejected if
that revision has moved. Text matching one of your exclusion patterns is
refused outright rather than trimmed, and everything else goes through the
same secret redactor as a command — because an agent that read a session can
restate a secret the redactor caught on the way in.

**Configuration scope and restart.** Which tools and resources are
available, the two write opt-ins, `mcp.exclude_dirs` and the MCP query
defaults are read from the **global `config.toml` only**; a project
`.suvadu.toml` does not change them. All of it is read **once, at server
startup**, so a change does not reach a running client until you restart it.

`mcp.exclude_dirs` is enforced at the query level by every tool and resource
that reads history, so it reduces aggregate counts too, not just listed
commands. `~`-prefixed entries are expanded and matched against the whole
subtree.

### Self-Update

- Binary downloads are served over **HTTPS** from `downloads.appachi.tech`
- Downloads are verified with a **minisign signature** (the public key is compiled into the binary, so a compromised download server cannot forge updates) and a **SHA256 checksum**
- Update files are written to a unique temporary directory to prevent TOCTOU attacks
- Homebrew installs are handled through the official Homebrew tap

### Shell Hooks — what a recorded command contains

Shell hooks are installed with `eval "$(suv init zsh)"` or
`eval "$(suv init bash)"`. For each command the hook sends Suvadu:

| Sent | From |
|---|---|
| The **whole command line as you typed it, arguments included** | zsh's `preexec` argument / bash's `$BASH_COMMAND` |
| Working directory | `$PWD` |
| Exit code | `$?` — left unrecorded rather than guessed when it is unknown |
| Start and end time (and the duration computed from them) | `$EPOCHREALTIME` at the prompt |
| Session id | `$SUVADU_SESSION_ID`, a UUID the hook generates per shell |
| Executor type and name | derived from the environment, see below |

**Commands contain arguments, and arguments contain secrets.** `git commit
-m "…"`, `curl -H 'Authorization: …'` and `psql postgres://user:pw@host` are
all stored as typed unless a redaction rule catches them. Redaction (below)
is on by default and rewrites recognised secrets before anything is written,
but it is pattern matching, not a guarantee. For a command you never want
stored at all, prefix it with a space or add an exclusion pattern.

**Environment variables.** The hook *reads* a fixed list of variables on
every command — `$CI`, `$GITHUB_ACTIONS`, `$CLAUDE_CODE`, `$CODEX_THREAD_ID`,
`$CURSOR_AGENT`, `$WINDSURF`, `$AIDER`, `$TERM_PROGRAM` and others, plus any
name you configure under `[agents]` — to work out who ran the command. Only
their *presence* is tested; none of their values is stored. What is stored
is the conclusion: an executor type (`human`, `agent`, `ide`, `ci`,
`programmatic`, `unknown`) and a name (`claude-code`, `openai-codex`,
`terminal`, …). The only environment values that reach the database are
`$PWD` and `$SUVADU_SESSION_ID`. An environment assignment typed on the
command line is part of the command text and *is* stored, subject to
redaction.

Also stored, once per shell session rather than per command: the **machine's
hostname**.

**Never captured:** command output, file contents, and anything another
program read or wrote. Suvadu records that a command ran, how it exited and
how long it took — nothing about what it did.

**Nothing is recorded at all when** the command starts with a space, matches
one of your exclusion patterns, `SUVADU_PAUSED` is set in that shell,
recording is disabled in the config, or the command exceeds 64 KB (or its
directory 4096 characters). Bash additionally ignores `PROMPT_COMMAND`
itself, tab completion, and lines sourced before the first interactive
prompt.

## Supported Versions

Security fixes go into the latest release. Older releases are best-effort;
there is no long-term support branch.

| Version | Supported |
|---------|-----------|
| Latest | Yes |
| < Latest | Best-effort |

### What this release has been tested against

| Surface | Tested against | Enforced at runtime |
|---|---|---|
| Shells | Zsh and Bash — the only two `suv init` generates hooks for | yes: `suv init` accepts no other shell |
| Atuin database import | Atuin 18.0.0 – 18.22.0 (history schema `20210422143411` – `20260818000000`) | **yes** — an untested migration is rejected by id with a next step, never guessed at |
| Codex | CLI 0.153.4, for `UserPromptSubmit`, `PostToolUse`, `Stop` and `SessionEnd` hooks | no |
| OpenCode | CLI 1.18.30, matching the published `@opencode-ai/plugin@1.18.30` types | no |
| Claude Code, Cursor, pi.dev, Antigravity | not pinned to a version | no |

Only the Atuin importer verifies the version of what it is reading. For the
agent integrations, if a tool changes its hook or plugin contract, capture
can degrade without an error — `suv doctor` reports, per agent, whether its
process was detected, its integration is installed, and whether commands,
native sessions and MCP registration have actually been seen. Re-run
`suv init <agent>` after upgrading either side.
