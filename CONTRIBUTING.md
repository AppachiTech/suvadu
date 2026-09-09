# Contributing to Suvadu

Thanks for your interest in contributing!

## Development Setup

```bash
# Clone the repo
git clone https://github.com/AppachiTech/suvadu.git
cd suvadu

# Build
cargo build

# Run tests
make test

# Run lints (format + clippy)
make lint
```

## Before Submitting a PR

1. Run `make lint && make test` — both must pass
2. Keep commits focused and descriptive
3. Update CHANGELOG.md under `[Unreleased]` if adding user-facing changes

## Project Structure

```
src/
  main.rs            # CLI entry point, dispatches to command handlers
  cli.rs             # Clap command definitions (suv <command> ...)
  lib.rs             # Library facade exposing db/models/repository/theme/util to integration tests
  config.rs          # TOML configuration, including per-project .suvadu.toml overlays
  db.rs              # SQLite initialization, schema, migrations
  models.rs          # Data types: Entry, Session, Tag, Bookmark, Alias, Note, Skill, ...
  repository/        # Database queries (CRUD, filtering, pagination, stats) — one file per domain
  util/              # Date/path/exclusion helpers, terminal guards, syntax highlighting
  hooks.rs           # Shell hook generation (Zsh, Bash)
  integrations.rs    # Claude Code, Cursor, OpenCode, Antigravity, pi.dev integrations
  integrations/      # Codex-specific hooks and the agent-integration registry
  import_export.rs   # History import (JSONL, Zsh history) and export (JSON/JSONL/CSV)
  redact.rs          # Secret detection and redaction applied before a command is recorded
  risk.rs            # Command risk assessment (levels, categories, obfuscation detection)
  update.rs          # Self-update mechanism
  suggest.rs         # Alias suggestion logic and handlers
  agent.rs           # Agent activity report handlers and formatting
  theme.rs           # Shared TUI theme colors

  commands/          # Non-interactive CLI command handlers, plus the bookmark/alias
                     # interactive managers (picker.rs, alias_picker.rs) — one file per command
  search/            # Interactive search TUI (suv search / Ctrl+R) — the reference screen
                     # every other TUI screen's look and feel is converged onto
  agent_ui/          # Agent dashboard, Prompt Explorer, and agent stats TUIs
  session_ui/        # Session picker and session timeline TUIs
  stats_ui.rs        # suv stats TUI
  settings_ui.rs     # suv settings TUI
  skills_ui.rs       # suv skills TUI (browse, add, edit, delete, sync, review)
  skills_sync.rs     # Materializes active skills into each agent's native file format
  suggest_ui.rs      # suv aliases suggest TUI

  mcp/               # MCP server: JSON-RPC protocol, tool/resource/prompt definitions
```

## Reporting Issues

Use [GitHub Issues](https://github.com/AppachiTech/suvadu/issues) with the provided templates.
