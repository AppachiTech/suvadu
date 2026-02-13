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
  main.rs          # CLI entry point, core command handlers (add, search, delete, etc.)
  cli.rs           # Clap command definitions
  config.rs        # TOML configuration management
  db.rs            # SQLite initialization, schema, migrations
  models.rs        # Data types: Entry, Session, Tag, Bookmark, Note, AliasSuggestion
  repository.rs    # Database queries (CRUD, filtering, pagination, stats)
  util.rs          # Date parsing, exclusion matching, path formatting, shared utilities
  hooks.rs         # Shell hook generation (Zsh, Bash)
  integrations.rs  # Claude Code, Cursor, Antigravity IDE integrations
  import_export.rs # History import (JSONL, Zsh history) and export (JSONL, CSV)
  update.rs        # Self-update mechanism
  suggest.rs       # Alias suggestion logic and handlers
  agent.rs         # Agent activity report handlers and formatting
  search.rs        # Interactive search TUI (ratatui + crossterm)
  settings_ui.rs   # Settings TUI
  stats_ui.rs      # Stats/analytics TUI
  suggest_ui.rs    # Alias suggestion TUI
  agent_ui.rs      # Agent monitoring TUI (dashboard + agent stats)
  risk.rs          # Command risk assessment (levels, categories, session risk)
  theme.rs         # Shared TUI theme colors
```

## Reporting Issues

Use [GitHub Issues](https://github.com/AppachiTech/suvadu/issues) with the provided templates.
