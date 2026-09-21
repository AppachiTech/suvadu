<p align="center">
  <img src="assets/suvadu-logo.svg" alt="Suvadu" width="180">
</p>
<p align="center"><strong>Total recall for your terminal. Shared memory for your AI agents.</strong></p>
<p align="center">
  <a href="https://github.com/AppachiTech/suvadu/actions/workflows/ci.yml"><img src="https://github.com/AppachiTech/suvadu/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://crates.io/crates/suvadu"><img src="https://img.shields.io/crates/v/suvadu.svg" alt="crates.io"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <a href="https://github.com/AppachiTech/suvadu/releases"><img src="https://img.shields.io/github/v/release/AppachiTech/suvadu?label=latest" alt="Latest Release"></a>
</p>

<p align="center">
  <img src="demo/hero.gif" alt="Suvadu — search history, browse AI agent prompts" width="700">
</p>

**Suvadu** replaces your shell history with a SQLite-backed store. Every command gets structured context — exit code, duration, directory, executor, session. AI agents can query it via MCP. 100% local.

- **Measured, not asserted** — at 100,000 entries on one M4 Max: **151 µs** p95 to record a command, **10–44 ms** p95 for a judged search that scans the whole history. One machine, one run — the harness, the corpus and what is still unmeasured are in [BENCHMARKS.md](BENCHMARKS.md)
- **AI agent tracking** — full session capture for Claude Code, Codex and OpenCode; commands and prompts for Cursor and pi.dev; command tagging alone for Antigravity, Windsurf, Aider, Continue and Copilot ([what each tier means](#what-each-agent-actually-gives-you))
- **Prompt Explorer** — trace every recorded command back to the prompt that triggered it
- **MCP Server** — **21 read-only tools, 8 resources and 6 prompts, all read-only by default.** The two write tools stay off until you turn their opt-in on. Agent session discovery, project context, failure learning, risk assessment, a shared skills library. Configurable via `suv settings`
- **100% local** — no cloud, no telemetry, no account. MIT licensed.

> **Website & Docs:** [suvadu.sh](https://suvadu.sh) &middot; **CLI Reference:** [suvadu.sh/cli](https://suvadu.sh/cli/) &middot; **Blog:** [suvadu.sh/blog](https://suvadu.sh/blog/) &middot; **What's new:** [CHANGELOG](CHANGELOG.md)

---

## Install

```bash
# Homebrew (macOS)
brew tap AppachiTech/suvadu && brew install suvadu

# Install script (macOS & Linux)
curl -fsSL https://downloads.appachi.tech/suvadu/install.sh | bash

# Cargo
cargo install suvadu
```

Then add shell hooks:

```bash
# Zsh
echo 'eval "$(suv init zsh)"' >> ~/.zshrc && source ~/.zshrc

# Bash
echo 'eval "$(suv init bash)"' >> ~/.bashrc && source ~/.bashrc
```

Verify: `suv status`

---

## Troubleshooting

**`no such file or directory` pointing at an old path** (e.g. `suv:8: no such file or directory: /opt/homebrew/bin/suv`) — The shell hook records the absolute path of the `suv` binary when your session starts. If you move the binary — switching package managers (e.g. Homebrew → Cargo), running `brew unlink suvadu`, or uninstalling and reinstalling elsewhere — already-open shells keep pointing at the old, now-deleted path. Reload the shell to pick up the new location:

```bash
exec zsh    # or: exec bash
```

(or just open a new terminal). Also make sure the new location, such as `~/.cargo/bin`, is on your `PATH`.

More at [suvadu.sh/cli/shell-integration](https://suvadu.sh/cli/shell-integration/).

---

## Quick Start

```bash
suv search                  # Interactive search TUI (also Ctrl+R)
suv history                 # Print last 25 commands (pipeable)
suv history --json -n 100   # Last 100 commands as JSONL
suv stats                   # Stats dashboard with heatmap
suv replay --after today    # Timeline of today's commands
suv sessions                # Browse shell and AI sessions together
suv doctor                  # Check installation health
suv agent dashboard         # Monitor AI agent activity
suv agent prompts           # Browse prompts and their commands
suv agent sessions          # List captured Codex/Claude sessions and reported tokens
suv skills                  # Interactive skills management (browse, add, edit, sync, review)
suv skills add my-skill     # Add a skill any MCP-capable agent can read
suv skills sync             # Materialize skills into Claude Code/Cursor/Codex
```

---

## How recall matches and ranks

Matching decides **which** commands are eligible, scope decides **where** to
look, and ranking decides only the **order**. They are three separate
controls, and changing one never changes what another does.

**Matching** — `--match`, or `^X` in the search UI:

| Mode | Rule |
|---|---|
| `terms` (default) | every whitespace-separated word must appear as a substring, in any order |
| `literal` | the whole query must appear exactly as typed, spaces and punctuation included |
| `prefix` | the command must start with the query |
| `fuzzy` | the query's letters must appear in order, gaps allowed (`gco` finds `git checkout`) |

Words are ANDed, never ORed. There is no quoting syntax and punctuation is
never stripped, so `git-push` is one word. Matching is case-insensitive;
`terms` and `fuzzy` fold non-ASCII case too, while `literal` and `prefix` are
answered by SQLite's `LIKE`, which folds ASCII only. **Every mode narrows in
the database, so how old a match is never decides whether it is found.**

**Scope** — `--scope`, `^P` to cycle, `^R` to reset:

| Scope | Looks at |
|---|---|
| `all` (default) | everything recorded |
| `directory` | commands run in **exactly** the current directory — not its subdirectories. `--here` is the same thing |
| `workspace` | commands run anywhere under the nearest enclosing Git repository. A linked worktree is its own workspace, and a nested repository wins over its parent |
| `session` | commands from the current shell session |

A scope that cannot apply here (no repository, no session) says so on stderr
and falls back explicitly; it never silently widens.

**Ranking** — `terms` and `fuzzy` order matches by how well they match:
whole-query prefix first, then a contiguous substring, then your words in
query order, then in any order. Within a tier a fuzzy score decides, adjusted
for command length, for having been typed by you rather than by an agent, and
— in Smart rank (`^S`, `search.context_boost`) — for having run in *exactly*
this directory. **Nothing is boosted for having been run often, or for having
exited 0.** There is no frecency in interactive search. `literal`, `prefix`
and an empty query are not re-ranked at all: those come back newest first.

At most 5,000 matches are ordered. That caps the ranking work, not how much
history was searched, and not how much of it you can reach: matching and
counting finish in the database, so the result count is the real number of
matches and every page of it can be opened. Pages past the ranked window
come back newest first — a relevance order computed from only part of the
result set would be arbitrary there.

Up/Down arrow recall is a different, simpler path: prefix match, newest
first, no deduplication, with the current directory used only to break ties
between commands recorded at the same millisecond.

`suv search --compact` draws the same UI inline under your prompt instead of
taking over the screen, so the output you were reading stays visible.

---

## Bring your existing history

```bash
suv import --from bash-history --dry-run ~/.bash_history   # preview counts, writes nothing
suv import --from bash-history ~/.bash_history             # import
suv import --from zsh-history ~/.zsh_history               # zsh equivalent
```

The input file is only read, never modified, and re-running the same import
adds nothing while genuinely repeated executions in the file are all kept.
Both importers apply your redaction and exclusion patterns exactly as live
recording does, and report excluded/redacted counts without echoing any
command text. Neither takes a backup of your Suvadu database first — only
the Atuin importer does that; run `suv backup` yourself if you want one.

What a Bash history file can and cannot give you:

| Field | Imported |
|-------|----------|
| Command text | Yes — redaction and your exclusion patterns apply, exactly as for live recording |
| Timestamp | Only if `HISTTIMEFORMAT` was set when the command ran (`#<epoch>` lines). Otherwise an explicitly synthetic 1970-01-01 placeholder — never an invented time |
| Multi-line commands | Reconstructed in timestamped files (the `#<epoch>` line is the record boundary). A plain file has no boundaries, so each line is imported as its own command |
| Directory, exit code, duration, executor | Not in the file — stored as unknown, never guessed, and never recorded as a successful exit |

### Coming from Atuin

```bash
suv import --from atuin-db --dry-run ~/.local/share/atuin/history.db   # preview, writes nothing
suv import --from atuin-db ~/.local/share/atuin/history.db             # import
```

The Atuin database is opened **read-only** — Suvadu never writes to it, never
copies the file behind SQLite's back, and reads the whole history inside one
transaction so a running Atuin can keep recording. Before writing, Suvadu takes
a consistent backup of *its own* database and prints the `cp` command that
restores it, then verifies afterwards that the Atuin file is byte-identical and
that the entries it claims to have written are really there. Re-running the
import adds nothing.

Tested against Atuin 18.0.0 – 18.22.0 (history schema `20210422143411` –
`20260818000000`). A database carrying a migration this release has not been
tested against is rejected with the migration id rather than guessed at — run
`suv update` and try again.

| Atuin field | Imported as |
|-------------|-------------|
| `command` | Command text, verbatim (multi-line and Unicode preserved). Redaction and your exclusion patterns apply, exactly as for live recording |
| `timestamp` (nanoseconds) | `started_at`, truncated to milliseconds. Two runs of the same command inside one millisecond are kept apart by 1 ms so neither is lost |
| `duration` (nanoseconds) | `duration_ms` / `ended_at`, truncated to milliseconds. Atuin's `-1` ("never finished") is stored as unknown, not as zero work |
| `exit` | `exit_code`. Atuin's `-1` becomes `NULL` — never a fabricated success |
| `cwd` | Directory. Empty or Atuin's literal `"unknown"` becomes unknown |
| `session` | A Suvadu session per Atuin session, id `atuin-<session>` |
| `hostname` (`host:user`) | Session hostname, plus `atuin_user` in the entry's context |
| `author`, `author_kind` | `executor` and `executor_type` (`1`→human, `2`→agent). An unstated kind stays `unknown`: Atuin guesses "agent" from known author names, Suvadu records only what was stated |
| `id`, `intent`, `shell` | Kept in the entry's `context` (`atuin_id`, `atuin_intent`, `atuin_shell`) — Suvadu has no columns for them |
| `deleted_at` | Rows you deleted in Atuin are skipped and counted, never resurrected |
| — | Sub-millisecond precision is lost. Atuin has no tags, notes or command output to carry over, and Suvadu keeps no Atuin sync/record-store state |

---

## AI Agent Setup

```bash
suv init claude-code    # Claude Code — commands, sessions, tokens + MCP
suv init codex          # Codex — commands, sessions, tokens + MCP
suv init cursor         # Cursor — hooks + MCP + prompt capture
suv init opencode       # OpenCode — plugin + full session capture (commands, sessions, tokens)
suv init pi             # pi.dev — extension + prompt capture
suv init antigravity    # Antigravity — auto-detect
```

After setup, relaunch the configured agent. For either VS Code extension, fully quit and reopen VS Code. Codex also requires reviewing/trusting the Suvadu hooks when prompted (or through `/hooks`). Both installers preserve unrelated hooks and configure the Suvadu MCP server; Codex backs up an existing `hooks.json` before changing it and uses `CODEX_HOME` when set.

### What each agent actually gives you

Three different things get called "AI agent support". Here is which one you get:

| Agent | `suv init` | Commands | Prompts | Native session timeline | Tested against |
|---|---|---|---|---|---|
| Claude Code | `claude-code` | yes (hooks) | yes | **yes** | not pinned to a version — see the caveat below |
| Codex | `codex` | yes (hooks) | yes | **yes** | CLI 0.153.4 |
| OpenCode | `opencode` | yes (plugin) | yes | **yes** | CLI 1.18.30 |
| Cursor | `cursor` | yes (hooks) | yes | no | not pinned |
| pi.dev | `pi` | yes (extension) | yes (capped at 500 characters by the extension) | no | not pinned |
| Antigravity | `antigravity` | via the shell hook | no | no | — |
| Windsurf, Aider, Continue, Copilot | — | via the shell hook | no | no | — |

The last two rows are **command tagging, not an integration**: the shell hook
recognises the environment variable those tools set and labels commands with
the right executor. There is nothing to install and no prompt is captured.

Only the Codex and OpenCode figures above are versions Suvadu has actually
been exercised against; nothing enforces them at runtime except for Atuin
imports, where the schema really is checked. If an agent changes its hook or
plugin contract, capture can degrade silently — `suv doctor` reports per
agent whether its process was detected, its integration is installed, and
commands, native sessions and MCP registration have been seen.

### What is never captured

- **Command output and file contents.** Suvadu records that a command ran,
  how it exited, how long it took and where — never what it printed or what
  it changed. Every inferred MCP answer repeats this.
- **From native transcripts:** thinking blocks, attachments, images, file
  contents and raw tool results. Codex commentary and reasoning, injected
  AGENTS/environment context and developer instructions are excluded too.
- **Non-shell tool calls and file edits**, and child sessions started from a
  session — stated on every session header as `capture.unverifiable`.
- **Size limits:** hook input is capped (1 MiB, 16 MiB for OpenCode) and a
  single native transcript record at 16 MiB; an oversize record is rejected
  with a named error rather than silently truncated. Prompt text is capped at
  `agent.prompt_capture_max_chars` (default 4000).
- **Timing and exit codes are not equally trustworthy per agent.** Codex and
  Claude Code hook timestamps are *receipt* time, not execution time (Codex
  records `timing_source = hook_received`). Codex leaves the exit code
  unknown when it does not supply a structured one; Claude Code and Cursor
  infer 0 from a success hook; pi.dev's bash tool does not expose one, so its
  extension guesses.

A session header therefore reports what Suvadu *knows* it missed
(`known_missing`: a paused window, a directory where capture was switched
off, token counters it could not follow) separately from what it never
observes at all. `complete` means no gap was recorded — never that
everything the agent did is here.

```bash
suv history --executor openai-codex
suv agent prompts --executor openai-codex
```

Codex shell commands link to the prompt from the same turn. Prompts without recorded commands do not appear in the prompt explorer. Capture respects Suvadu's recording and redaction settings. Hook timestamps reflect receipt time; exit status stays unknown when Codex does not provide a structured exit code. This requires a Codex version supporting `UserPromptSubmit` and `PostToolUse` hooks (tested with CLI 0.153.4). Stop and SessionEnd hooks also incrementally import Codex's native transcript for prompts, final assistant answers, and provider-reported token usage, independent of shell commands (bounded to 16 MiB per record) — this part requires a Codex version supporting `Stop`/`SessionEnd` hooks too.

Claude Code commands link to their native prompt turn after transcript reconciliation. Stop and SessionEnd hooks incrementally import local transcript records for prompts, assistant text, models, and provider-reported token usage. Thinking blocks, attachments, images, file contents, and raw tool results are not stored in Suvadu.

`suv init opencode` installs a plugin at `~/.opencode/plugins/suvadu.js` and also registers that directory in `~/.config/opencode/opencode.jsonc`'s `plugin` array — OpenCode does not reliably auto-load plugins from the directory alone. If your `opencode.jsonc` already has JSONC-style comments (which this step can't safely parse and rewrite without risking the rest of your config), it prints the exact line to add yourself instead. Bash commands OpenCode executes are recorded immediately; prompts, assistant responses, model, and token usage are captured when a session goes idle, via OpenCode's own `session.messages` API. Rerun `suv init opencode` after upgrading Suvadu or OpenCode, then fully quit and relaunch OpenCode so the updated plugin and config take effect. Tested against OpenCode CLI 1.18.30; if OpenCode reports a plugin load error after an OpenCode upgrade, its plugin contract may have changed again and `suv init opencode` will need a matching update.

For agents configured with MCP, ask: *"What commands failed in this project recently?"*

### Prompt Explorer or session timeline?

Both show "what the agent did", from two different stores, and neither
subsumes the other.

- **Prompt Explorer** (`suv agent prompts`) is built from **recorded shell
  commands**. It groups them by the prompt that was live when they ran and
  reports that turn's command count, successes, failures and total time. A
  prompt that produced no recorded command never appears — there is nothing
  to group. It works for every agent whose prompt is captured, including
  Cursor and pi.dev, which have no session timeline, and it browses across
  sessions with a time window and an executor filter.
- **Session timelines** (`suv sessions`, and `suv agent sessions` for the
  non-interactive view) are built from the agent's **own transcript**,
  imported incrementally. They show prompts, assistant responses,
  interrupted turns, every observed model and provider-reported token
  totals — including sessions with no commands at all — plus capture
  completeness, saved summaries (`s`) and the handoff scaffold (`h`). Only
  Claude Code, Codex and OpenCode have one.

### MCP: read-only by default

The server advertises **21 read-only tools, 8 resources and 6 prompts**.
Every one of them reads. There are exactly two write tools, and both are off
until you turn their opt-in on:

| Write tool | Opt-in | What it can do |
|---|---|---|
| `save_session_summary` | `mcp.allow_session_summaries` | store a summary the calling agent generated, when you ask it to |
| `propose_skill` | `mcp.allow_skill_proposals` | propose a skill, always as **pending review**, never active until you approve it in `suv skills` (`Ctrl+P`) |

To turn one on: run `suv settings`, go to the **MCP** tab (`Tab` to cycle),
and under **Writes** press `Enter` on the row. Save with `Ctrl+S`, **then
restart your MCP client** — the server reads its configuration once at
startup, so a running client keeps the old settings until it reconnects.
Then ask: *"Summarize and save current session."*

The same tab lists every tool and resource with its *effective* state. A
write tool shows **why** it is off — because its opt-in is off, because you
turned that tool off, or both — instead of two switches that can disagree.
Turning an opt-in back off stops new writes; summaries already saved are
kept.

**Which config the server reads.** Tool and resource availability, the two
write opt-ins, `mcp.exclude_dirs` and the MCP defaults come from the
**global `config.toml` only** — a project `.suvadu.toml` overlay does not
change which MCP capabilities are on. Redaction, exclusions and your custom
risk rules *are* resolved through the overlay, but from the directory the
MCP server process was started in, once, at startup. Both are another reason
a change needs a client restart.

Every response follows one convention, so a caller can calibrate its trust:
RFC 3339 timestamps, `unknown` as the only missing-value token, percentages
that always carry their fraction, `limit`/`offset` with `next_offset`,
visible truncation, a stable `command-<id>` on every row, and a
`provenance:` line saying **observed** (records Suvadu stored), **inferred**
(anything derived from them) or **caller-reported** (text an agent wrote).
Anything inferred also repeats that Suvadu records commands, exit codes and
timings — never command output or file contents.

A saved summary is the agent's own text, stored back. Suvadu invokes no
model and generates no prose. The writing agent and model are recorded as
**caller-declared metadata, not verified identity**, and the summary's cited
evidence IDs are checked to exist in that session. Suvadu resolves the
current Codex, Claude Code or OpenCode session without guessing: no working
directory means no resolution, and more than one match is always your
choice. A later request extends an append-only checkpoint from its saved
event and command offsets; if the evidence it was written from changed, the
agent rebuilds from the full session.

See the [full integration guide](https://suvadu.sh/blog/track-ai-agent-commands-with-suvadu/) and [MCP server docs](https://suvadu.sh/cli/mcp-server/).

---

## Key Features

| Feature | Details |
|---------|---------|
| **Search** | Full-history search TUI with four [matching modes](#how-recall-matches-and-ranks) (`^X`) and four scopes (`^P`), your own commands by default (`Ctrl+A` shows agents, `Ctrl+E` failures only), filters, Smart rank, detail pane, bookmarks, and `--compact` inline recall |
| **History** | Non-interactive `suv history` with filters, `--json`, pipeable to other tools |
| **Agent Dashboard** | Timeline, risk assessment, per-agent analytics, exportable reports; `suv agent report --fail-on <low\|medium\|high\|critical>` for local CI / git-hook gating |
| **MCP Server** | 21 read-only tools, 8 resources and 6 prompts, plus two opt-in write tools that are off by default — agent session replay and incremental cross-agent summary checkpoints, project context, failure learning; one response convention with explicit provenance |
| **Skills Library** | `suv skills` — interactive TUI to browse, add, edit, delete, sync, and review shared skills any MCP-capable agent can read instead of each tool keeping its own copy. `sync --dry-run` diffs the managed region of each generated file; a region edited outside Suvadu is reported as a conflict and skipped, not overwritten (`--force` overrides). `add/list/show/edit/rm/disable/enable/sync/cleanup` also work as scriptable subcommands |
| **Prompt Explorer** | `suv agent prompts` groups **recorded commands** by the prompt that triggered them, with that turn's successes, failures and duration. A prompt that ran no recorded command does not appear — for the agent's own transcript, use a session timeline |
| **Unified Sessions** | `suv sessions` browses human and AI sessions together, with a **Capture** column that reports `full` or `gaps: N` from recorded gaps alone — never inferred from exit codes. Native Codex, Claude Code and OpenCode sessions show prompts, responses, commands, every observed model and provider-reported token totals; `s` opens saved summaries (badged CURRENT / NEW ACTIVITY / EVIDENCE CHANGED; `Tab` toggles rendered/raw, `Ctrl+Y` copies) and `h` builds a handoff scaffold from captured records only |
| **Stats** | Heatmap, hourly distribution, top commands, executor breakdown; `--human` (or `Ctrl+H` in the TUI) excludes AI-agent activity |
| **Doctor** | `suv doctor` separates what blocks shell-history capture from optional integrations, gives each check its own repair, reports what the stored data actually proves, and lists storage by category next to the retention rule that changes it |
| **Organization** | Tags, bookmarks (`suv bookmarks` opens an interactive picker that recalls one into your prompt), notes, and `suv aliases` — an interactive manager (add/edit/delete) for shell aliases, plus suggestions for your frequently-typed long commands |
| **Privacy & Safety** | Space-prefix exclusion, regex patterns, secret redaction (extend via `redaction.extra_patterns`), local-only — applied to every ingestion path, imports and saved summaries included. `suv backup`, an automatic snapshot before any `suv delete`, and `suv delete --dry-run` to see the matched commands and what a delete leaves behind. See [SECURITY.md](SECURITY.md). |
| **Arrow Keys** | Recency-first Up/Down recall — your most recent commands surface first, prefix-matched, with the current directory used only to separate commands recorded at the same millisecond. Agent commands are hidden by default; reveal them with `Alt+A` (per shell) or `--include-agents`. |
| **Vim Bindings** | Optional vim-style `j`/`k`/`Ctrl+U`/`Ctrl+D` navigation in search TUI |

Full feature documentation at [suvadu.sh/cli](https://suvadu.sh/cli/).

---

<details>
<summary><strong>More demos</strong></summary>

<p><em>These recordings predate the current search footer, status row and
session picker columns. They still show the shape of each screen, not its
exact chrome.</em></p>

<p align="center">
  <img src="demo/suvadu-search.gif" alt="Suvadu search TUI" width="700">
  <br>
  <em>Search, stats & settings</em>
</p>

<p align="center">
  <img src="demo/suvadu-agent.gif" alt="Suvadu agent dashboard" width="700">
  <br>
  <em>Agent dashboard — track what your AI agents execute</em>
</p>

<p align="center">
  <img src="demo/suvadu-prompts.gif" alt="Suvadu prompt explorer" width="700">
  <br>
  <em>Prompt Explorer — trace commands back to the prompt that triggered them</em>
</p>

</details>

---

## Development

```bash
git clone https://github.com/AppachiTech/suvadu.git
cd suvadu
make dev      # Run the app
make test     # Run tests
make lint     # Run clippy + format check
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines and the release
checklist, and [BENCHMARKS.md](BENCHMARKS.md) for how search is measured.

## Security

See [SECURITY.md](SECURITY.md) for vulnerability reporting, data storage design, and privacy details.

## License

[MIT](LICENSE)
