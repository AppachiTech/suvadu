<p align="center">
  <h1 align="center">Suvadu</h1>
  <p align="center"><strong>Total recall for your terminal.</strong></p>
  <p align="center">
    <a href="https://github.com/AppachiTech/suvadu/actions/workflows/ci.yml"><img src="https://github.com/AppachiTech/suvadu/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
    <a href="https://crates.io/crates/suvadu"><img src="https://img.shields.io/crates/v/suvadu.svg" alt="crates.io"></a>
    <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
    <a href="https://github.com/AppachiTech/suvadu/releases"><img src="https://img.shields.io/github/v/release/AppachiTech/suvadu?label=latest" alt="Latest Release"></a>
  </p>
</p>

Suvadu (Tamil: "Trace" / "Footprint") replaces your shell's built-in history with a SQLite-backed store, giving you structured storage, millisecond-precision timestamps, and a modern interactive search UI — across every terminal, IDE, and AI agent you use.

<p align="center">
  <img src="demo/suvadu-search.gif" alt="Suvadu search TUI with fuzzy matching, filters, stats heatmap and settings" width="700">
  <br>
  <em>Search, stats & settings</em>
</p>

<p align="center">
  <img src="demo/suvadu-agent.gif" alt="Suvadu agent monitoring dashboard showing AI command tracking and risk assessment" width="700">
  <br>
  <em>Agent stats & dashboard — track what your AI agents execute</em>
</p>

---

## Table of Contents

- [Why Suvadu?](#why-suvadu)
- [Features](#features)
- [Installation](#installation)
- [Quick Start](#quick-start)
- [Usage](#usage)
  - [Interactive Search](#interactive-search-tui)
  - [Session Replay](#session-replay)
  - [Stats Dashboard](#stats-dashboard)
  - [Agent Monitoring](#agent-monitoring)
  - [Alias Suggestions](#alias-suggestions)
  - [Executor Tracking](#executor-tracking)
  - [Managing Recording](#managing-recording)
  - [Tags, Bookmarks & Notes](#tags-bookmarks--notes)
  - [Bulk Deletion](#bulk-deletion)
  - [Export & Import](#export--import)
  - [Privacy](#privacy)
- [Configuration](#configuration)
- [IDE & AI Agent Integrations](#ide--ai-agent-integrations)
- [How It Works](#how-it-works)
- [Command Reference](#command-reference)
- [Development](#development)
- [Contributing](#contributing)
- [Security](#security)
- [License](#license)

---

## Why Suvadu?

Your shell history is one of your most valuable productivity assets — but the default implementation is stuck in the 1970s. A flat text file with no structure, no search, no context, and no way to track what your AI agents are doing.

**Suvadu fixes this.** Every command gets a structured record with working directory, exit code, duration, executor identity, and session context. Search is fast. AI agent commands are tracked and risk-assessed automatically.

| | Default Shell History | Suvadu |
|---|---|---|
| **Storage** | Flat text file | SQLite + WAL |
| **Search** | Linear scan, regex only | Fuzzy search, indexed |
| **Context** | None | Directory, exit code, duration, executor, tags |
| **AI Agents** | Invisible | Auto-detected, risk-assessed, auditable |
| **Cross-shell** | Per-shell files | Unified database |
| **UI** | Reverse-i-search | Interactive TUI with filters, preview, bookmarks |

---

## Features

### High Performance
- **SQLite with WAL mode** — low-latency writes, even with millions of records
- **Indexed search** — fast results across your entire history
- **Fuzzy matching** — powered by nucleo-matcher (same engine as Helix editor)

### Interactive Search
- **Full TUI** — structured table with time, session/tag, executor, path, command, status, and duration columns
- **Syntax highlighting** — commands, flags, strings, variables, paths, and operators each get distinct colors
- **Smart mode** — context-aware ranking boosts same-directory results (`Ctrl+S`)
- **Directory scoping** — filter to current working directory (`Ctrl+L` or `--here`)
- **Date filters** — `Ctrl+F` panel with "today", "yesterday", or `YYYY-MM-DD` ranges
- **Detail pane** — `Tab` to preview full entry metadata
- **Deduplication** — toggle unique command view with `Ctrl+U`

### Smart Arrow Keys
- **Frecency ranking** — Up/Down arrow prefers same-directory commands using frequency × recency scoring
- Configurable via `suv settings` → Shell → Enable Arrow Key Navigation

### AI Agent Monitoring
- **Auto-detection** — identifies commands from Claude Code, Codex, Aider, VS Code, Cursor, Windsurf, and more
- **Risk assessment** — every agent command classified as Critical, High, Medium, Low, or Safe
- **Agent dashboard** — real-time TUI with timeline, risk indicators, and detail pane
- **Agent stats** — per-agent analytics with top commands, directories, and risk breakdown
- **Agent report** — export activity as text, markdown, or JSON
- **Claude Code integration** — `suv init claude-code` captures AI-executed commands via PostToolUse hook

### Tagging, Bookmarks & Notes
- **Session tags** — categorize sessions (e.g., "work", "personal") for filtering
- **Auto-tagging** — automatically assign tags based on working directory
- **Bookmarks** — star favorite commands (`Ctrl+B` in TUI) with optional labels
- **Notes** — annotate any entry with context (`Ctrl+N` in TUI)

### Privacy First
- Commands prefixed with a **space** are not recorded
- Configurable **regex exclusion patterns**
- Per-session **pause** (`suv pause`) and global **disable** (`suv disable`)
- **Bulk delete** matching entries by pattern or date range
- **All data stays local** — no telemetry, no external servers

### More
- **Shell integration** — Zsh (5.1+) and Bash, with `Ctrl+R` search and arrow key cycling
- **Session replay** — chronological timeline with date, directory, tag, and executor filters
- **Stats dashboard** — interactive TUI with heatmap, sparkline, hourly distribution, and top commands
- **Alias suggestions** — analyzes history to suggest shell aliases for frequently-typed commands
- **Export & import** — JSONL, CSV, and `~/.zsh_history` import
- **Shell completions** — Zsh, Bash, and Fish (`suv completions <shell>`)
- **Self-update** — `suv update` with SHA256 checksum verification

---

## Installation

### Prerequisites

- **macOS** (Apple Silicon or Intel) or **Linux** (x86_64 or ARM64)
- **Zsh 5.1+** or **Bash**

### Homebrew (macOS — Recommended)

```bash
brew tap AppachiTech/suvadu
brew install suvadu

# Add to your shell (choose one):
echo 'eval "$(suv init zsh)"' >> ~/.zshrc && source ~/.zshrc
# or
echo 'eval "$(suv init bash)"' >> ~/.bashrc && source ~/.bashrc
```

### Manual Install — macOS

```bash
curl -fsSL https://downloads.appachi.tech/macos/suv-macos-latest.tar.gz \
  | tar -xz \
  && sudo mv suv /usr/local/bin/ \
  && sudo ln -sf /usr/local/bin/suv /usr/local/bin/suvadu

echo 'eval "$(suv init zsh)"' >> ~/.zshrc
source ~/.zshrc
```

### Manual Install — Linux

```bash
# Auto-detects architecture (x86_64 or aarch64/Graviton)
ARCH=$(uname -m)
if [ "$ARCH" = "aarch64" ] || [ "$ARCH" = "arm64" ]; then
  URL="https://downloads.appachi.tech/linux/suv-linux-aarch64-latest.tar.gz"
else
  URL="https://downloads.appachi.tech/linux/suv-linux-latest.tar.gz"
fi

curl -fsSL "$URL" \
  | tar -xz \
  && sudo mv suv /usr/local/bin/ \
  && sudo ln -sf /usr/local/bin/suv /usr/local/bin/suvadu

# Add to your shell (choose one):
echo 'eval "$(suv init zsh)"' >> ~/.zshrc && source ~/.zshrc
# or
echo 'eval "$(suv init bash)"' >> ~/.bashrc && source ~/.bashrc
```

### Build from Source

```bash
git clone https://github.com/AppachiTech/suvadu.git
cd suvadu
cargo build --release
sudo cp target/release/suv /usr/local/bin/
```

### Updating

```bash
# Homebrew
brew upgrade suvadu

# Manual installations
suv update
```

### Uninstalling

```bash
suv uninstall
```

---

## Quick Start

```bash
# Verify installation
suv --help

# Check recording status
suv status

# Open interactive search (or press Ctrl+R)
suv search

# Open settings
suv settings
```

---

## Usage

### Interactive Search (TUI)

`Ctrl+R` is automatically bound to Suvadu's search when shell hooks are active.

```bash
suv search                        # Open search
suv search --query "git commit"   # Search with initial query
suv search --unique               # Unique commands only
suv search --here                 # Commands from current directory
suv search --executor agent       # Filter by executor type
```

#### Keyboard Shortcuts

| Key | Action |
|-----|--------|
| Type | Fuzzy search across history |
| `Up` / `Down` | Navigate results |
| `Tab` | Toggle detail preview pane |
| `Enter` | Select and execute command |
| `Esc` | Exit without selecting |
| `Ctrl+S` | Toggle Smart mode (context-aware ranking) |
| `Ctrl+L` | Toggle directory-scoped filter |
| `Ctrl+U` | Toggle unique/deduplicated view |
| `Ctrl+F` | Open filter panel (date, tag, exit code, executor) |
| `Ctrl+B` | Toggle bookmark on selected entry |
| `Ctrl+N` | Add or edit note on selected entry |
| `Ctrl+T` | Associate current session with a tag |
| `Ctrl+Y` | Copy selected command to clipboard |
| `Ctrl+D` | Delete selected entry (with confirmation) |
| `Ctrl+G` | Go to specific page |
| `Left` / `Right` | Previous / next page |

> **Smart Fallback:** If Suvadu is disabled or paused, `Ctrl+R` automatically reverts to your shell's default history search.

### Session Replay

```bash
suv replay                              # Current session
suv replay --after today                # Today's commands
suv replay --after yesterday --here     # Yesterday, this directory
suv replay --session <id>               # Specific session
```

### Stats Dashboard

```bash
suv stats                # Interactive TUI dashboard
suv stats --days 30      # Last 30 days
suv stats --text         # Plain text output
```

### Agent Monitoring

Monitor and audit every command your AI agents run.

```bash
# Interactive dashboard with timeline and risk indicators
suv agent dashboard
suv agent dashboard --executor claude-code
suv agent dashboard --after yesterday --here

# Per-agent analytics — breakdown cards, top commands, risk table
suv agent stats
suv agent stats --days 7

# Export agent activity report
suv agent report
suv agent report --format markdown > report.md
suv agent report --format json | jq .
```

#### Dashboard Controls

| Key | Action |
|-----|--------|
| `Up` / `Down` | Navigate timeline |
| `Tab` | Toggle detail pane |
| `1` / `2` / `3` / `4` | Period: 7d / 30d / 90d / All |
| `a` | Cycle agent filter |
| `r` | Toggle risk-only filter (medium+ risk) |
| `Ctrl+Y` | Copy selected command to clipboard |
| `q` / `Esc` | Quit |

#### Risk Levels

Every agent command is automatically classified:

| Level | Examples | Indicator |
|-------|----------|-----------|
| **Critical** | `rm -rf /`, `DROP TABLE`, `git push --force origin main` | `!!` |
| **High** | `chmod 777`, `npm install`, `pip install`, config overwrites | `!!` |
| **Medium** | `git reset`, `docker run`, environment modifications | `~` |
| **Low** | File writes, branch operations | `.` |
| **Safe** | `git status`, `ls`, `cargo test`, `grep` | `ok` |

### Alias Suggestions

```bash
suv suggest-aliases                    # Interactive TUI
suv suggest-aliases --text             # Plain text output
suv suggest-aliases --days 30 -c 5     # Last 30 days, min 5 uses
```

### Executor Tracking

Suvadu automatically detects and records **who or what** executed each command:

| Type | Executors | Detection |
|------|-----------|-----------|
| **Human** | Terminal | Interactive TTY check |
| **AI Agent** | Claude Code, Codex, Aider, Continue | Environment variables |
| **IDE** | VS Code, Cursor, Windsurf, Antigravity, IntelliJ, PyCharm | Environment variables |
| **Bot** | Copilot | Environment variables |
| **CI/CD** | GitHub Actions, GitLab CI, CircleCI | Environment variables |
| **Programmatic** | Subprocess | Non-interactive shell fallback |

Filter by executor in the search TUI (`Ctrl+F` → Executor) or via CLI:

```bash
suv search --executor agent
suv search --executor cursor
```

#### `suv wrap` — Agent & Script Integration

For agents and scripts that don't load shell hooks:

```bash
suv wrap -- git status
suv wrap --executor-type agent --executor claude-code -- npm test
suv wrap --executor-type ci --executor github-actions -- make deploy
```

### Managing Recording

```bash
suv disable              # Stop recording globally
suv enable               # Resume recording
eval $(suv pause)        # Pause current session only
suv status               # Check recording state
```

### Tags, Bookmarks & Notes

```bash
# Tags
suv tag create "work" --description "Work related"
suv tag list
suv tag update "work" --new-name "office" --description "Office stuff"
suv tag associate "work"

# Bookmarks
suv bookmark add "docker compose up -d"
suv bookmark list
suv bookmark remove "docker compose up -d"

# Notes
suv note <id> -c "remember: this fixed the build"
suv note <id>              # View
suv note <id> --delete     # Remove
```

### Bulk Deletion

```bash
suv delete "rm -rf" --dry-run              # Preview before deleting
suv delete "rm -rf"                        # Delete by substring
suv delete "^git (commit|status)" --regex  # Delete by regex
suv delete "" --before 2024-01-01          # Delete entries before a date
```

### Export & Import

```bash
# Export
suv export > history.jsonl
suv export --format csv > history.csv
suv export --after 2025-01-01 --before 2025-06-01 > q1-q2.jsonl

# Import
suv import history.jsonl
suv import history.jsonl --dry-run
suv import --from zsh-history ~/.zsh_history
```

### Privacy

Prefix a command with a space to prevent recording:

```bash
 secret_command_here   # NOT saved to history
```

Configure exclusion patterns in `suv settings` → Exclusions, or in `config.toml`:

```toml
exclusions = ["^ls$", "^pwd$", "password"]
```

---

## Configuration

**Config file location:**
- macOS: `~/Library/Application Support/suvadu/config.toml`
- Linux: `~/.config/suvadu/config.toml`

### Interactive Settings

```bash
suv settings
```

Opens a TUI with tabs for Search, Shell, Exclusions, and Auto Tags.

### Reference

```toml
# Master switch
enabled = true

[search]
page_limit = 50                      # Rows per page (10-5000)
show_unique_by_default = false        # Start in deduplicated mode
filter_by_current_session_tag = false # Scope search to current tag
context_boost = true                  # Boost same-directory results (Smart mode)
show_detail_pane = true               # Show detail pane on search open

[shell]
enable_arrow_navigation = true        # Up/Down arrows cycle history

[auto_tags]
"/Users/alice/work" = "work"
"/Users/alice/personal" = "personal"

# Regex exclusion patterns (invalid regex falls back to substring match)
exclusions = ["^ls$", "^pwd$", "^cd$"]
```

### Exclusion Patterns

| Pattern | Effect |
|---------|--------|
| `^ls$` | Ignores exactly `ls`, still records `ls -la` |
| `password` | Ignores any command containing "password" |
| `^git .*` | Ignores all git commands |

---

## IDE & AI Agent Integrations

### Claude Code

```bash
suv init claude-code
```

Installs a PostToolUse hook and configures `~/.claude/settings.json`. Restart Claude Code after setup.

### Cursor

```bash
suv init cursor
```

Auto-detects Cursor via `$CURSOR_INJECTION` and `$CURSOR_TRACE_ID` environment variables. No additional configuration needed.

### Antigravity

```bash
suv init antigravity
```

Auto-detects Antigravity via the `$ANTIGRAVITY_AGENT` environment variable. No additional configuration needed.

**Verify any integration:**

```bash
suv search --executor agent       # Claude Code
suv search --executor cursor      # Cursor
suv search --executor antigravity # Antigravity
```

---

## How It Works

```
┌──────────────────────────┐
│     Zsh / Bash Shell     │
│  preexec → start time    │
│  precmd  → exit code,    │
│            duration       │
│  suv add → record entry  │
└────────────┬─────────────┘
             │
             ▼
┌──────────────────────────┐
│    SQLite + WAL Mode     │
│    suvadu/history.db     │
└────────────┬─────────────┘
             │
             ▼
┌──────────────────────────┐
│    suv search (TUI)      │
│    Ctrl+R binding        │
│    Indexed queries       │
│    Fast response          │
└──────────────────────────┘
```

Shell hooks use native `$EPOCHREALTIME` (Zsh 5.1+ / Bash 5+) for millisecond-precision timestamps with zero external dependencies.

**Database location:**
- macOS: `~/Library/Application Support/suvadu/history.db`
- Linux: `~/.local/share/suvadu/history.db`

### Schema

| Table | Key Columns |
|-------|-------------|
| `sessions` | `id` (UUID), `hostname`, `created_at`, `tag_id` |
| `entries` | `command`, `cwd`, `exit_code`, `duration_ms`, `started_at`, `ended_at`, `executor_type`, `executor`, `tag_id`, `context` |
| `tags` | `name`, `description` |

---

## Command Reference

| Command | Description |
|---------|-------------|
| **Search & Browse** | |
| `suv search` | Interactive search TUI |
| `suv search --query "git"` | Search with initial query |
| `suv search --executor agent` | Filter by executor type |
| `suv search --unique` | Deduplicated results |
| `suv search --here` | Commands from current directory |
| `suv replay` | Replay current session as timeline |
| `suv replay --after today --here` | Today's commands in this directory |
| **Stats & Analytics** | |
| `suv stats` | Interactive stats dashboard |
| `suv stats --days 30` | Stats for the last 30 days |
| `suv stats --text` | Plain text output |
| **Agent Monitoring** | |
| `suv agent dashboard` | Interactive agent monitoring TUI |
| `suv agent dashboard --executor claude-code` | Filter to one agent |
| `suv agent stats` | Per-agent analytics and risk breakdown |
| `suv agent report` | Export agent activity report (text) |
| `suv agent report --format markdown` | Export as markdown |
| `suv agent report --format json` | Export as structured JSON |
| **Organization** | |
| `suv tag create <name>` | Create a tag |
| `suv tag list` | List all tags |
| `suv tag associate <name>` | Tag current session |
| `suv bookmark add <cmd>` | Bookmark a command |
| `suv bookmark list` | List all bookmarks |
| `suv note <id> -c "note"` | Add a note to an entry |
| `suv suggest-aliases` | Suggest shell aliases (interactive TUI) |
| **Recording Control** | |
| `suv status` | Show recording status |
| `suv enable` / `suv disable` | Toggle recording |
| `suv pause` | Pause current session |
| `suv settings` | Interactive settings TUI |
| **Data** | |
| `suv delete <pattern>` | Delete matching entries |
| `suv export` | Export history as JSONL |
| `suv export --format csv` | Export as CSV |
| `suv import <file>` | Import from JSONL file |
| `suv import --from zsh-history` | Import from `~/.zsh_history` |
| **Integration** | |
| `suv wrap -- <cmd>` | Record a command from agents/scripts |
| `suv init zsh` / `suv init bash` | Generate shell hooks |
| `suv init claude-code` | Set up Claude Code capture |
| `suv init cursor` | Set up Cursor tracking |
| `suv init antigravity` | Set up Antigravity tracking |
| **Utilities** | |
| `suv completions <shell>` | Generate shell completions (zsh, bash, fish) |
| `suv man` | Generate man page |
| `suv update` | Update to latest version |
| `suv uninstall` | Remove Suvadu |

---

## Development

```bash
git clone https://github.com/AppachiTech/suvadu.git
cd suvadu

make dev      # Run the app
make test     # Run tests
make lint     # Run clippy + format check
make help     # Show all available commands
```

```bash
cargo test                # Run all tests
cargo fmt -- --check      # Check formatting
cargo clippy -- -D warnings  # Lint
cargo build --release     # Release build
```

### Release

```bash
make release-patch  # 0.1.0 → 0.1.1
make release-minor  # 0.1.x → 0.2.0
make release-major  # 0.x.x → 1.0.0
```

---

## Contributing

Contributions are welcome! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, project structure, and guidelines.

## Security

For security-related matters including vulnerability reporting, data storage design, and privacy features, see [SECURITY.md](SECURITY.md).

## License

[MIT](LICENSE) — Appachi Tech
