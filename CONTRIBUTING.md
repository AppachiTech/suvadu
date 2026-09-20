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

## Release checklist

A feature is not done when its code merges — it is done when everything
that *describes* it agrees with it. These surfaces drift independently and
have to be checked together, in one pass, before a release is tagged.

**1. Help text** — for every command, flag and key that changed:
- [ ] `suv <command> --help` describes the behaviour that shipped, not the
      behaviour intended. Run it; do not read the source string.
- [ ] The grouped command list in `TOP_LEVEL_HELP` (`src/cli.rs`) names
      every subcommand, and its one-line summaries are still true — it is
      hand-written and clap will not catch a stale line.
- [ ] Any accepted value added to an enum (`--from`, `--match`, `--scope`,
      `--target`, …) appears in the flag's own help *and* in the top-level
      summary.
- [ ] In-TUI footer badges and the `?` help overlay list the keys that
      exist, and the status row's vocabulary matches the CLI's.
- [ ] `suv man` renders.

**2. Settings** — `suv settings`:
- [ ] Every new config key is reachable from the UI, or the docs say
      plainly that it is config-file only.
- [ ] A new MCP tool or resource is in `src/mcp/catalog.rs`, which drives
      both the server's advertisement and the settings tab. Never add one to
      only one of them.
- [ ] Saving still preserves keys this build does not know about.
- [ ] Anything that needs a restart to take effect says so in the UI.

**3. Docs** — `README.md`, `SECURITY.md`, `BENCHMARKS.md`, `CHANGELOG.md`:
- [ ] Every entry under `## [Unreleased]` is there, in the right section,
      and describes shipped behaviour. **Nothing is filed under an already
      released heading.**
- [ ] Counts in prose are recounted, not assumed (tools, resources,
      prompts, supported agents, tested versions).
- [ ] Any claim with a number has a source: a benchmark run with its
      hardware and corpus, or a constant in the code. If neither exists,
      the claim goes, rather than being softened.
- [ ] `BENCHMARKS.md` is re-run, or its baseline is explicitly dated and
      described as the older measurement it is.
- [ ] Stated third-party versions (agent CLIs, Atuin schemas) match what
      was actually exercised this cycle.

**4. Comparison tables** — anywhere Suvadu is set against another tool,
here or on the website:
- [ ] Every row is a property of the *current* release of both tools.
- [ ] A "faster than X" row has both tools measured on the same corpus and
      hardware, or it is not published.
- [ ] Rows describing the other tool are re-checked against its current
      docs, not remembered.

**5. Privacy inventory** — `SECURITY.md`:
- [ ] A new column, table, file or directory is in the stored-data
      inventory with the rule that makes it go away.
- [ ] A new ingestion path is a row in the redaction/exclusion table, and
      the row is backed by a test.
- [ ] Anything written outside the database (caches, backups, generated
      agent files) is named, with its permissions and who prunes it.
- [ ] New paths come from `project_dirs()`, and anything hardcoded is
      called out as such.

**6. Demos** — `demo/`:
- [ ] Every `.gif` referenced from the README still shows the UI that
      ships. A footer, status row or column change invalidates a recording.
- [ ] Re-record from the `.tape` files, or mark the recording as dated in
      the README rather than letting it pass as current.

Then follow the repo's `release` skill for the version bump, tag and
publish steps.

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
  import_export.rs   # History import (JSONL, Zsh, Bash, Atuin) and export (JSON/JSONL/CSV)
  import_export/     # The read-only Atuin database adapter
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

  mcp/               # MCP server: JSON-RPC protocol, tool/resource/prompt definitions.
                     # catalog.rs is the single source of which capabilities exist;
                     # conventions.rs is the one response shape they all follow
  ai_sessions/       # Native agent transcript adapters and the handoff template
```

Docs and evidence that live at the repository root (`docs/` is gitignored):

```
README.md          # What Suvadu is and does — every claim in it must be checkable
SECURITY.md        # Threat model, stored-data inventory, redaction, retention
BENCHMARKS.md      # How search is measured, and the current baseline
CHANGELOG.md       # User-facing changes; new work goes under [Unreleased]
```

## Reporting Issues

Use [GitHub Issues](https://github.com/AppachiTech/suvadu/issues) with the provided templates.
