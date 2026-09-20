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

- **<2ms** recording overhead, **<10ms** search across 1M+ entries
- **AI agent tracking** — auto-detects Claude Code, Cursor, OpenCode, Antigravity, Windsurf, pi.dev, Codex, Aider
- **Prompt Explorer** — trace every command back to the prompt that triggered it
- **MCP Server** — 21 read-only tools + 8 resources + 5 prompts, plus opt-in write tools. Agent session discovery, project context, failure learning, risk assessment, a shared skills library. Configurable via `suv settings`
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

## Bring your existing history

```bash
suv import --from bash-history --dry-run ~/.bash_history   # preview counts, writes nothing
suv import --from bash-history ~/.bash_history             # import
suv import --from zsh-history ~/.zsh_history               # zsh equivalent
```

The input file is only read, never modified, and re-running the same import
adds nothing while genuinely repeated executions in the file are all kept.

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

```bash
suv history --executor openai-codex
suv agent prompts --executor openai-codex
```

Codex shell commands link to the prompt from the same turn. Prompts without recorded commands do not appear in the prompt explorer. Capture respects Suvadu's recording and redaction settings. Hook timestamps reflect receipt time; exit status stays unknown when Codex does not provide a structured exit code. This requires a Codex version supporting `UserPromptSubmit` and `PostToolUse` hooks (tested with CLI 0.153.4). Stop and SessionEnd hooks also incrementally import Codex's native transcript for prompts, final assistant answers, and provider-reported token usage, independent of shell commands (bounded to 16 MiB per record) — this part requires a Codex version supporting `Stop`/`SessionEnd` hooks too.

Claude Code commands link to their native prompt turn after transcript reconciliation. Stop and SessionEnd hooks incrementally import local transcript records for prompts, assistant text, models, and provider-reported token usage. Thinking blocks, attachments, images, file contents, and raw tool results are not stored in Suvadu.

`suv init opencode` installs a plugin at `~/.opencode/plugins/suvadu.js` and also registers that directory in `~/.config/opencode/opencode.jsonc`'s `plugin` array — OpenCode does not reliably auto-load plugins from the directory alone. If your `opencode.jsonc` already has JSONC-style comments (which this step can't safely parse and rewrite without risking the rest of your config), it prints the exact line to add yourself instead. Bash commands OpenCode executes are recorded immediately; prompts, assistant responses, model, and token usage are captured when a session goes idle, via OpenCode's own `session.messages` API. Rerun `suv init opencode` after upgrading Suvadu or OpenCode, then fully quit and relaunch OpenCode so the updated plugin and config take effect. Tested against OpenCode CLI 1.18.30; if OpenCode reports a plugin load error after an OpenCode upgrade, its plugin contract may have changed again and `suv init opencode` will need a matching update.

For agents configured with MCP, ask: *"What commands failed in this project recently?"*

To create a cross-agent session checkpoint, run `suv settings`, go to the **MCP** tab (`Tab` to cycle), and under **Writes** turn on **Allow Saved Session Summaries** with `Enter`. Save with `Ctrl+S`, then restart your MCP client — the MCP server reads its configuration once at startup, so a running client keeps the old settings. Then ask: *"Summarize and save current session."*

The same tab lists every MCP tool and resource with its effective state. A write tool such as `save_session_summary` shows *why* it is off — because its opt-in is off, or because you turned that specific tool off — instead of two switches that can disagree. Turning the opt-in back off stops new writes; summaries already saved are kept. Suvadu resolves the current Codex or Claude session without guessing when multiple sessions match. Later requests extend a safe append-only checkpoint from its saved event and command offsets; if earlier captured evidence changed, the agent rebuilds the summary from the full session.

See the [full integration guide](https://suvadu.sh/blog/track-ai-agent-commands-with-suvadu/) and [MCP server docs](https://suvadu.sh/cli/mcp-server/).

---

## Key Features

| Feature | Details |
|---------|---------|
| **Search** | Substring search TUI (your own commands by default; `Ctrl+A` shows agents, `Ctrl+E` shows failures only) with filters, Smart mode, detail pane, bookmarks |
| **History** | Non-interactive `suv history` with filters, `--json`, pipeable to other tools |
| **Agent Dashboard** | Timeline, risk assessment, per-agent analytics, exportable reports; `suv agent report --fail-on <low\|medium\|high\|critical>` for local CI / git-hook gating |
| **MCP Server** | 21 read-only tools + 8 resources + 5 prompts, plus opt-in writes — agent session replay and incremental cross-agent summary checkpoints, project context, failure learning, configurable |
| **Skills Library** | `suv skills` — interactive TUI to browse, add, edit, delete, sync, and review shared skills any MCP-capable agent can read instead of each tool keeping its own copy; `add/list/show/edit/rm/sync` also work as scriptable subcommands |
| **Prompt Explorer** | Trace commands back to the prompt that triggered them |
| **Unified Sessions** | `suv sessions` browses human and AI sessions together; native Codex, Claude Code, and OpenCode sessions show prompts, responses, commands, every observed model, and provider-reported token totals — press `s` on an AI session to view its saved summaries in a scrollable overlay (`Tab` toggles rendered/raw, `Ctrl+Y` copies) |
| **Stats** | Heatmap, hourly distribution, top commands, executor breakdown; `--human` (or `Ctrl+H` in the TUI) excludes AI-agent activity |
| **Doctor** | `suv doctor` checks shell, hooks, config, database, MCP, and agent hooks health |
| **Organization** | Tags, bookmarks (`suv bookmarks` opens an interactive picker that recalls one into your prompt), notes, and `suv aliases` — an interactive manager (add/edit/delete) for shell aliases, plus suggestions for your frequently-typed long commands |
| **Privacy & Safety** | Space-prefix exclusion, regex patterns, secret redaction (extend via `redaction.extra_patterns`), local-only. `suv backup` + an automatic snapshot before any `suv delete`. |
| **Arrow Keys** | Recency-first Up/Down recall — your most recent commands surface first, with the current directory as a tiebreaker. Agent commands are hidden by default; reveal them with `Alt+A` (per shell) or `--include-agents`. |
| **Vim Bindings** | Optional vim-style `j`/`k`/`Ctrl+U`/`Ctrl+D` navigation in search TUI |

Full feature documentation at [suvadu.sh/cli](https://suvadu.sh/cli/).

---

<details>
<summary><strong>More demos</strong></summary>

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

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## Security

See [SECURITY.md](SECURITY.md) for vulnerability reporting, data storage design, and privacy details.

## License

[MIT](LICENSE)
