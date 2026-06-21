# Changelog

All notable changes to this project will be documented in this file.

## [0.3.4] - 2026-06-21

### Added
- **AI-agent commands are now hidden from history recall by default** — Up-arrow recall and `Ctrl+R` reverse search show your own (human-typed) commands only, so agent-issued commands no longer clutter your prompt history. Toggle agent commands on/off live with `Ctrl+A` (Up-arrow recall) or `Alt+A` (`Ctrl+R` search), per-invocation with `--include-agents`, or permanently via `search.recall_show_agents` in `config.toml`.
- **`suv backup [--out PATH]`** — write a snapshot of your history database to a chosen path (defaults to a timestamped file in the backups directory). A snapshot is also taken automatically before any destructive `suv delete`; pass `--no-backup` to skip it.
- **`suv bookmark pick`** — interactive picker that recalls a saved bookmark directly into your shell prompt for editing.
- **"Failed only" search filter** — press `Ctrl+E` in the search TUI or run `suv search --failed` to restrict results to commands that exited non-zero.
- **`suv stats --human`** — human-only analytics that exclude agent activity; also available as the `h` toggle inside the stats TUI.
- **`suv agent report --fail-on <low|medium|high|critical>`** — exits non-zero when findings meet or exceed the given risk level, for use in local CI checks and git hooks.
- **Configurable `agent.prompt_capture_max_chars`** — control how much prompt text is captured per agent command. Default raised from 500 to 4000 characters.
- **Configurable `redaction.extra_patterns`** — supply your own secret-matching regexes to redact in addition to the built-in patterns.
- **Idempotent JSONL import** — `suv import` now deduplicates, so re-importing the same export no longer creates duplicate entries. Pass `--allow-duplicates` to opt out.
- **New risk patterns** — `git clean -f`, world-writable `chmod`, and recursive `chown -R` are now flagged by risk assessment.

### Changed
- **Search ranking overhauled** — results must now contain your typed words as literal substrings; the previous loose character-subsequence matching is gone (so `gco` no longer matches `git checkout`). Results are ranked prefix matches first, then contiguous matches, then in-order words, then any-order words.
- **Up-arrow recall is now recency-first** — a command typed in another directory is no longer buried after a `cd`; the most recently used commands surface first.
- **`suv alias suggest` is now human-only** — alias suggestions are derived from your own commands and ignore agent activity.
- **`alias add` and `add-suggested` regenerate the alias file automatically** — no separate regenerate step is needed after adding an alias.
- **MCP project-scoped tools now match the full directory subtree** — queries scoped to a project include commands run in nested subdirectories.
- **MCP now honors `mcp.default_days` and `mcp.default_limit`** — configured defaults are applied to MCP queries.
- **`agent.risk_ignore_patterns` now actually suppress matching risk findings** — previously configured ignore patterns had no effect.
- **Dependencies** — `rusqlite` 0.38 → 0.40, `clap_mangen` 0.2 → 0.3; lockfile refreshed.

### Fixed
- **Redaction no longer corrupts legitimate command flags** — `docker -p`, `ssh -p`, and `curl -u` are left intact instead of being mangled by secret redaction.
- **Password redaction edge cases** — passwords containing `@`, and password-only database connection URIs, are now redacted correctly instead of leaking; base64 values following a key are now detected.
- **MCP multibyte panics** — fixed UTF-8 byte-slice panics when agent output or prompts contained multibyte characters.

### Security
- **Destructive prompts refuse non-TTY stdin** — `suv delete` and `suv uninstall` no longer accept a piped `y`, preventing accidental destructive actions when input is not an interactive terminal.
- **`SECURITY.md` now documents the at-rest data model and redaction coverage** — clarifying what is stored locally and what redaction does and does not cover.

## [0.3.3] - 2026-04-25

### Fixed
- **Cross-machine `suv export` → `suv import` no longer fails with `FOREIGN KEY constraint failed`** — previously, a JSONL export only carried entries, so importing into a fresh database on another machine died on the very first row because the entry's `session_id` (and any `tag_id`) referenced rows that did not exist locally. The importer now auto-creates a placeholder session for any unknown `session_id`, and re-maps each entry's `tag_id` by looking up the carried-over `tag_name` on the destination — creating the tag locally when missing, falling back to a NULL tag association if the 20-tag cap is reached. Addresses #19.
- **`suv import` now rejects non-JSONL files up front with a clear message** — passing a CSV (or JSON-array) export to `suv import` previously emitted a confusing per-line "expected struct Entry" parse error for every line. The importer now peeks the first non-empty line and aborts with a single actionable message pointing the user at `suv export > history.jsonl`. Addresses #19.
- **`suv export --format json` with zero matching entries now emits `[]` instead of an empty file** — previously an empty result produced a zero-byte file that downstream JSON parsers rejected as invalid.

## [0.3.2] - 2026-04-16

### Added
- **Paste support in search TUI** — `Cmd+V` / `Ctrl+V` now works in the search pane and all dialog inputs (filter, note, go-to-page). Pasted text is sanitized (control characters stripped), respects the 2000-character input limit, and routes to the active input field. In vim Normal mode, paste auto-switches to Insert mode. Closes #18.

### Fixed
- **Reduced false positives in secret redaction** — Environment variables like `AUTHOR_NAME`, `GIT_AUTHOR_EMAIL`, `TOKENIZERS_PARALLELISM`, `PASSWORD_FILE`, `CREDENTIAL_HELPER`, `SECRET_SCANNING`, and `REACT_APP_AUTH_DOMAIN` are no longer incorrectly redacted. The redaction engine now requires sensitive keywords (`SECRET`, `TOKEN`, `PASSWORD`, `AUTH`, etc.) to appear as the **final segment** of the variable name rather than matching as arbitrary substrings. Real secrets like `GITHUB_TOKEN=`, `DB_PASSWORD=`, `API_KEY=` are still correctly redacted. Closes #16.
- **Bash octal parsing error** — Fixed `value too great for base` crash in bash hooks when `EPOCHREALTIME` milliseconds had a leading zero (e.g., `068`). Added `10#` prefix to force base-10 evaluation. #17.

## [0.3.1] - 2026-04-05

### Added
- **`suv history`** — Non-interactive history dump with all standard filters (`--after`, `--before`, `--tag`, `--exit-code`, `--executor`, `--here`, `--cwd`). Supports `-n` for result count and `--json` for JSONL output. Pipeable to other tools. Newest-first by default.
- **`suv doctor`** — Diagnostic command that checks shell version, shell hooks, config validity, database health (schema version + integrity check), recording state, MCP server registration (Claude Code and Cursor), and agent hook scripts. Reports pass/warn/fail with actionable fix hints.
- **Configurable search scoring** — Three new `[search]` config fields: `length_threshold` (command length penalty, default 80), `human_boost_percent` (boost for human commands over agent commands, default 33%), `cwd_boost_percent` (boost for same-directory commands, default 50%). Tune via `suv settings` or `config.toml`. Setting a boost to 0 disables it.
- **pi.dev agent integration** — `suv init pi` installs a TypeScript extension for [pi.dev](https://pi.dev) that captures bash commands and prompts via pi.dev's event system. Commands are recorded with `executor=pi`.

### Changed
- **Full session UUID** — Session filenames now use the full UUID instead of a truncated 8-character prefix, avoiding rare collisions.

## [0.3.0] - 2026-04-01

### Added
- **Agent Session Discovery** — `find_agent_session` MCP tool searches past AI agent sessions by prompt text, directory, executor, or date range. Returns session summaries with command counts, success rates, risk breakdown, and `claude --resume` commands.
- **Session Replay** — `replay_agent_session` MCP tool returns the full chronological timeline of a specific agent session with prompts interleaved between commands. Supports session ID prefix normalization (passing `abc123` finds `claude-abc123`).
- **Learn from Failures** — `learn_from_failures` MCP tool analyzes recurring command failures in a project. Shows commands with high failure rates, agent vs human failure comparison, and last failure timestamps. Helps agents avoid repeating known-bad approaches.
- **Project Context** — `project_context` MCP tool returns a project briefing: common commands, build/test/lint patterns with success rates, recent failures, and agent activity. Available on-demand with directory and time range filters.
- **`suvadu://agents/sessions` resource** — auto-injected summary of the 5 most recent AI agent sessions with prompts and command counts. Agents get session awareness before the first tool call.
- **`suvadu://context/project` resource** — auto-injected project briefing at session start. Includes common commands, failure rates for frequent commands, and recent agent activity. Every new agent session starts informed.

- **MCP Configuration** — new `[mcp]` section in `config.toml` and a new MCP tab in `suv settings` TUI. Disable individual tools or resources with checkboxes, set default time windows (`default_days`) and result limits (`default_limit`), and exclude directories from MCP queries. Config is loaded at MCP server startup; disabled tools/resources are hidden from agents.

### Changed
- **MCP server expanded** — 15 tools (was 11) and 7 auto-injected resources (was 5). New tools: `find_agent_session`, `replay_agent_session`, `learn_from_failures`, `project_context`. New resources: `suvadu://agents/sessions`, `suvadu://context/project`.

## [0.2.1] - 2026-03-30

### Added
- **Vim keybindings** — optional vim-style modal navigation in the search TUI. Enable with `vim_mode = true` in `[search]` config or via `suv settings` → Search → Vim Mode. Insert mode for typing, Normal mode for `j`/`k` navigation, `Ctrl+U`/`Ctrl+D` half-page scroll, `g`/`G` jump to top/bottom, `h`/`l` page navigation, `/` or `i` to return to search, `q` to quit. Off by default. See #13.

### Changed
- **Updated README** — slimmed down from 840 lines to ~120 lines, linking to [suvadu.sh](https://suvadu.sh) for full documentation. Updated logo assets to new chevron stack design.
- **Updated website URL** — all references now point to `suvadu.sh` instead of `appachi.tech/suvadu`.

## [0.2.0] - 2026-03-25

### Added
- **MCP Server** — `suv mcp-serve` exposes shell history to AI agents via the Model Context Protocol. 11 tools including `assess_risk` for pre-execution safety checks. 5 browsable resources (`history/recent`, `failures/recent`, `stats/today`, `risk/summary`, `agents/activity`) plus a `session/{id}` template — agents get history context automatically without calling tools. 100% local, read-only, no network.
- **MCP auto-configuration** — `suv init claude-code` and `suv init cursor` now automatically register the MCP server in `~/.claude.json` and `~/.cursor/mcp.json`. Zero extra setup for agent memory.
- **Cursor agent integration** — `suv init cursor` installs `afterShellExecution` and `beforeSubmitPrompt` hooks into `~/.cursor/hooks.json`. Captures AI agent commands with exit codes and prompts.
- **Post-install tips** — all `suv init` commands now show actionable next steps (`suv agent prompts`, `suv agent dashboard`). Claude Code and Cursor init also hint that the agent can query history directly via MCP.
- **Enhanced `suv status`** — shows database path, total commands recorded, detected agents, and actionable tips.

### Fixed
- **Cursor executor detection** — Cursor agent commands (via `$CURSOR_AGENT`) detected as `executor_type="agent"`. Cursor and Antigravity checks moved before VS Code to avoid misidentification (both are VS Code forks).
- **Antigravity tagged as agent** — changed from `executor_type="ide"` to `executor_type="agent"` since `$ANTIGRAVITY_AGENT` signals AI agent execution.
- **Unique mode sort** — `Ctrl+U` in search now sorts by frequency (most used first) instead of recency, so common commands like `git status` appear at the top instead of one-off agent commands.
- **Relative date parsing** — `"N days ago"` now works in all date inputs, fixing `suv agent prompts` default `--after "7 days ago"` which was silently returning no filter.
- **Settings list scrolling** — exclusions, auto-tags, and agents lists now show scrollbars when items overflow.

### Changed
- **Unified TUI styling** — period selector uses pill-style highlight (bg) and left-aligned key numbers across Stats, Agent Dashboard, and Agent Stats. Executor color standardized to `badge_executor`, path to `badge_path`, session ID to `primary_dim` across all TUIs. All top-level TUIs show `q/Esc Quit` consistently.
- **Agent Stats title** — renamed from "AGENT ANALYTICS" to "AGENT STATS" to match the command name.
- **Full session ID** — search and dashboard detail panes show full session ID instead of truncated 8 chars.
- **Prompt shown in search** — agent prompt displayed in search detail pane when available.

## [0.1.5] - 2026-03-23

### Added
- **Prompt Explorer** — new two-screen TUI (`suv agent prompts` or press `p` in the agent dashboard) to browse agent prompts and drill into the commands they triggered. Right-side preview shows full prompt text, session, executor, path, timestamps, and success/fail stats. Supports `Ctrl+Y` to copy commands and `s` to jump to session timeline.
- **`suv agent prompts` CLI** — direct shortcut to launch the Prompt Explorer with `--after`, `--executor`, and `--here` flags.
- **PostToolUseFailure hook** — captures failed Claude Code commands with parsed exit codes. Run `suv init claude-code` to install the new hook.
- **Cursor agent integration** — `suv init cursor` installs `afterShellExecution` and `beforeSubmitPrompt` hooks into `~/.cursor/hooks.json`. Captures Cursor AI agent commands with exit codes, prompts, and session grouping.
- **Relative date parsing** — date inputs now support `"N days ago"` (e.g. `--after "7 days ago"`) in addition to `"today"`, `"yesterday"`, and `"YYYY-MM-DD"`.
- **Executor selector in search filter** — the executor field (`Ctrl+F` → field 5) is now an Up/Down selector showing actual executors from the database instead of free-text input.
- **Session picker improvements** — live search bar (type to filter by session ID or tag), `Ctrl+F` filter popup with Tag/Start Date/End Date fields (matching `suv search` design), full session ID display (40% width), first/last command timestamps.
- **Session timeline header** — shows full session ID and first/last command timestamps on two lines.
- **Alias resolution in Top Programs** — `suv stats` resolves shell aliases in the Top Programs breakdown. Fixes #11.

### Fixed
- **Dead text bug** (`suv search` standalone) — added `suv()` shell function wrapper that uses `print -z` (zsh) / `history -s` (bash) to inject the selected command into the editing buffer instead of printing it as dead text. Also hardened ZLE widgets with `emulate -L zsh`, `LBUFFER`/`RBUFFER`, and quoted command substitutions. Fixes #6.
- **Claude Code exit codes** — `PostToolUse` now defaults to exit code 0 (the hook only fires on success). Previously all agent commands were stored with NULL exit codes.
- **Missing recent entries in agent views** — `load_entries` no longer truncates recent agent entries when total entry count exceeds the 10k SQL limit.
- **Session ID display** — strip agent prefixes (`claude-`, `opencode-`, `cursor-`) before truncating for display.
- **Cursor executor detection** — Cursor agent commands (via `$CURSOR_AGENT`) are now detected as `executor_type="agent"`. Cursor IDE terminal check moved before VS Code to avoid misidentification (Cursor sets `TERM_PROGRAM=vscode`).

### Changed
- **Prompt stats** — `None` exit codes (common for agent commands) are treated as unknown, not failed. Status column shows `✔N ✘N` counts instead of a misleading percentage.

## [0.1.4] - 2026-03-17

### Added
- **Custom agent detection** — configurable `[agents]` section in `config.toml` and a new Agents tab in `suv settings` TUI. Define custom agent detection rules with a name, environment variable, and executor type. Custom agents are checked before built-in agents.
- **`suv init opencode`** — OpenCode integration via plugin. Installs a `tool.execute.after` plugin that records every bash command OpenCode executes, with prompt capture for agent command grouping.

### Fixed
- **Regex lookahead in secret redaction** — the OpenAI key pattern used an unsupported negative lookahead (`(?!...)`), causing `suv wrap` and other commands to error on first use. Replaced with a compatible pattern. Fixes #9.

## [0.1.3] - 2026-03-16

### Fixed
- **Powerlevel10k compatibility** — Ctrl+R search now works correctly with p10k instant prompt. Added ZLE display invalidation, terminal state save/restore, and queued-keystrokes guard. Fixes #6.

### Added
- **Update UX** — `suv update` now checks version before downloading, shows release notes, and displays an ASCII banner on success.

## [0.1.2] - 2026-03-13

### Fixed
- **Linux self-update** — `suv update` failed with "Text file busy" (ETXTBSY) on Linux because `cp` cannot overwrite a running binary. Fixed by removing the old binary before copying; the kernel keeps the old inode alive for the running process.

### Added
- **`scripts/install.sh`** — Universal installer script that handles both fresh installs and updates. Auto-detects OS (Linux/macOS) and architecture (x86_64/ARM64), verifies SHA256 checksums, uses `rm`-before-`cp` to avoid the Linux "Text file busy" issue, checks `version.txt` to skip updates when already on latest, and only shows shell integration instructions on fresh installs.
- **Cargo install detection** — `suv update` now detects Cargo-installed binaries (`~/.cargo/bin/`) and redirects users to `cargo install suvadu`, matching the existing Homebrew detection behavior.
- **`version.txt`** — CI now publishes a `version.txt` file to `downloads.appachi.tech` on each release, enabling the install script to compare versions and skip unnecessary downloads.

### Changed
- **Homebrew update command** — `suv update` for Homebrew users now suggests the full `brew update && brew tap AppachiTech/suvadu && brew upgrade suvadu` command to ensure the tap is present and the formula index is fresh.
- **CI** — Skip redundant `cargo test` in the macOS x86_64 cross-compile job (already tested in the ARM64 job).

## [0.1.1] - 2026-03-10

### Fixed
- Allow unknown fields in config file for upgrade compatibility from older versions

## [0.1.0] - 2026-03-10

A major milestone release with 75 commits since v0.0.2: new commands, secrets
redaction, a comprehensive security hardening pass, architecture overhaul, and
975 tests (up from ~100).

### Added

#### New Commands
- **`suv alias`** — Direct shell alias management: `add`, `remove`, `list`,
  `apply` (write to sourceable file), and `add-suggested` (interactive picker
  from history analysis).
- **`suv gc`** — Garbage collection: remove orphaned tags/sessions and compact
  the SQLite database with `VACUUM`.
- **`suv session`** — Interactive session timeline TUI with a session picker,
  command-level detail, and scroll/filter support.
- **`suv wrap`** — Execute and record a command without shell hooks. Designed
  for AI agents and scripts.

#### Search & Suggestions
- Field-specific search: `suv search --field cwd`, `--field session`,
  `--field executor` to search by directory, session, or executor instead of
  command text.
- Frequency-weighted suggestions in the suggest engine — commands used in more
  directories rank higher.
- Improved fuzzy search ranking: length penalty prevents short substring
  matches from outranking exact matches; human-executed commands boosted over
  agent commands.
- Help overlay (press `F1` or `?` in search) showing all keyboard shortcuts
  organized by category.
- Responsive column layout: terminals under 80 columns show command only;
  80-129 show time + command + status; 130+ show all columns. The detail pane
  (Tab) always shows full entry info.

#### Export & Import
- JSON export format: `suv export --format json` alongside existing JSONL and
  CSV formats.
- `--json` flag on applicable commands for machine-readable output.
- Transaction rollback on import errors — partial imports no longer leave the
  database in an inconsistent state.

#### Security
- **Secrets redaction** — Detects API keys, tokens, passwords, and credentials
  in commands and redacts them before storage. Patterns cover AWS, GitHub,
  Stripe, database URLs, Bearer tokens, and more.
- **Minisign signature verification** for self-update: downloaded binaries are
  verified against a signed checksum before replacing the running binary.
- SQLite foreign key enforcement (`PRAGMA foreign_keys = ON`).
- Database file permissions restricted to owner-only (`0o600`).
- Config file permissions enforcement (`0o600`).
- ReDoS protection via `regex::RegexBuilder::size_limit()` on user-supplied
  patterns.
- SQL identifier allowlisting — column names in dynamic queries are validated
  against a known set, preventing SQL injection via sort/filter parameters.
- CSV formula injection prevention — cell values starting with `=`, `+`, `-`,
  `@` are prefixed to neutralize spreadsheet formula execution.
- Bounded input fields: 2,000 characters in search, 500 in settings, 256 for
  session IDs.
- Session ID validation (alphanumeric, hyphens, underscores only).
- Secure update mechanism: temp directory isolation, tar path traversal
  validation, mandatory checksum verification.
- Shell-escaped hook script paths to prevent injection via directory names.
- HTTPS enforced for all update/download URLs.

#### Stats & Analytics
- `suv stats --tag <name>` — Filter statistics by tag for per-project analysis.
- Stats database indexes for faster queries on large histories.
- Hourly heatmap division-by-zero guard on empty datasets.

#### TUI & Display
- Command syntax highlighting across all TUI views (search, agent dashboard,
  session timeline, suggest). Commands, flags, strings, variables, paths, and
  operators are color-coded.
- Three-tier theme system: `dark` (RGB for dark terminals), `light` (RGB for
  light terminals), `terminal` (ANSI 16 — adapts to your color scheme). Themes
  hot-swap immediately in the settings UI.
- Risk level colors centralized in `theme.rs` as single source of truth,
  replacing 15+ hardcoded `Color::Rgb` literals.
- Dirty-tracking and save confirmation dialog in settings UI — unsaved changes
  are no longer silently lost on quit.
- Empty state hints in agent dashboard when no commands are found.
- Clipboard feedback message on copy.
- Session table headers and consistent column ordering across views.

#### Testing
- 975 tests total: 153 binary, 805 library, 17 integration (up from ~100).
- Integration test suite (`tests/integration.rs`) covering end-to-end flows.
- Comprehensive unit tests for: ingestion hot path, search input handlers,
  filter builder, stats helpers, agent UI, suggest UI, fuzzy scoring,
  timestamp edge cases, TUI pure-logic functions, settings flows, delete/replay
  /export commands, tag commands, and uninstall cleanup.

#### Other
- Schema version tracking with migration framework (v1 through v4) replacing
  ad-hoc migration checks, with downgrade guard for forward compatibility.
- `Entry::is_agent()` and `ExecutorKind` enum for type-safe executor
  classification.
- `SearchField`, `InitTarget`, `ReportFormat`, `SettingsTab` enums replacing
  stringly-typed parameters.
- `128 + signal` exit code convention for signal-killed processes.
- Detect macOS ARM architecture for correct binary downloads during
  self-update.
- `suv uninstall` now detects and removes all installation sources (Homebrew,
  cargo, curl script).

### Changed

#### Architecture
- **Module decomposition** — Large monolithic files split into focused modules:
  - `main.rs` (1,500 lines) → `commands/` directory with per-command handlers
    (entry, search, session, settings, stats, replay, tag, alias, wrap).
  - `search.rs` (2,357 lines) → `search/` directory (mod, input, render, data,
    format, tests).
  - `repository.rs` (2,464 lines) → `repository/` directory (mod, entries,
    tags, bookmarks, notes, aliases, stats, api, tests).
  - `agent_ui.rs` (1,620 lines) → `agent_ui/` directory (mod, dashboard,
    stats).
  - `util.rs` (1,043 lines) → `util/` directory (mod, terminal, format,
    timestamp, exclusion, highlight, file, cleanup).
  - `session_ui/` — new module with picker and timeline sub-modules.
- **`RepositoryApi` trait** — Dependency injection interface for all database
  operations, enabling unit tests with mock repositories.
- **SearchApp decomposition** — Extracted `DialogState` enum, `FilterState`,
  `PaginationState`, `ViewOptions` from the monolithic search state struct.
- **`SettingsTab` enum** — Replaced index-based tab dispatching (`if tab == 2`)
  with exhaustive enum matching.
- Extracted `Repository::init()` to eliminate repeated database initialization.
- Extracted `Repository::get_tag_id_by_name()` to deduplicate tag lookups.
- Extracted `build_pattern_sql` helper to share SQL construction between delete
  and count operations.
- `FilterBuilder` pattern for composable session/entry queries.
- Eliminated in-memory entry grouping and parallel risk vector in agent UI.
- Removed all `clippy::too_many_lines` suppressions via function decomposition.
- Shared `EXECUTOR_DETECTION_SCRIPT` constant between zsh/bash hooks.

#### CI & Distribution
- SHA-pinned all GitHub Actions for supply chain security (checkout, rust-
  toolchain, cache, codecov, r2-upload, gh-release).
- Pinned `cross` to v0.2.5 with SHA256-verified minisign download in Linux
  release workflow.
- All dependencies upgraded to latest versions.
- Clippy lint groups enabled: `pedantic`, `nursery`, `perf`, `complexity`,
  `style`, `cargo`, plus `unsafe_code` warning.

### Fixed

- **Shell hooks** — Doubled braces in executor detection caused `bad
  substitution` errors on `source ~/.zshrc`.
- **Arrow-key navigation** — Failed commands were incorrectly hidden when
  cycling through history with arrow keys.
- **Negative durations** — Commands with clock skew or out-of-order timestamps
  no longer produce negative duration values (saturating arithmetic).
- **UTF-8 byte-slicing** — Four locations in agent UI that sliced strings at
  byte boundaries instead of character boundaries, causing panics on
  multi-byte characters.
- **Fuzzy search threshold** — Miscalculated minimum score allowed irrelevant
  matches to appear in results.
- **Config cache TOCTOU** — Race condition between checking file mtime and
  reading content.
- **Timeline underflow** — Empty sessions caused arithmetic underflow in
  timeline calculations.
- **Alias name collision** — Removed arbitrary suffix limit of 99 that
  prevented generating unique alias names for similar commands.
- **Filter popup** — Crashed or rendered incorrectly on terminals smaller than
  the popup dimensions.
- **Division-by-zero** — Stats heatmap and percentage calculations guarded
  against empty datasets.
- **Thread-unsafe env access** — `std::env::set_var` calls replaced with
  thread-safe alternatives.
- **LIKE escaping** — Special characters (`%`, `_`, `\`) in search patterns are
  now properly escaped for SQLite LIKE queries.
- **REGEXP cache** — Eliminated `unwrap()` on regex compilation cache that
  could panic on invalid patterns.
- **Nanosecond timestamps** — Timestamps from tools reporting in nanoseconds
  are now normalized correctly.
- **Quote-aware shell chaining** — Risk assessment now correctly handles
  `&&`, `||`, `;` inside quoted strings rather than treating them as chain
  operators.
- **LIMIT injection** — Page size values are now validated before interpolation
  into SQL queries.
- **Ctrl+key fallthrough** — Ctrl+key combinations no longer trigger unintended
  actions in search input.
- **Bookmark `created_at`** — Timestamp now set at creation time instead of
  defaulting to zero.
- **Streaming export** — Large exports no longer load the entire dataset into
  memory.
- **Display-width truncation** — Uses Unicode display width so CJK and emoji
  characters are measured correctly instead of by byte count.
- **Atomic file writes** — All configuration and data file writes use
  `tempfile::NamedTempFile` + `persist()` to prevent corruption on crash or
  power loss.
- Eliminated all production `unwrap()` calls — replaced with proper error
  propagation or safe defaults.
- Graceful handling of clipboard, config parsing, and JSON serialization
  errors.

### Performance

- **Cached `ProjectDirs`** via `LazyLock` — eliminated repeated filesystem
  lookups on every command ingestion.
- **Config mtime caching** — config file is only re-parsed when the file's
  modification time changes.
- **Reordered early exits** in the ingestion hot path — exclusion checks and
  validation run before any database work.
- **Streaming export** — CSV/JSONL exports write row-by-row instead of
  collecting the entire dataset.
- **Stats query indexes** — Added database indexes for the most common
  analytics queries.

## [0.0.2] - 2025-05-20

### Added
- Post-install onboarding flow.
- Demo GIFs in README.

### Fixed
- Deduplicate suvadu hooks in Claude Code settings.

## [0.0.1] - 2025-05-18

Initial release of Suvadu — database-backed shell history for Zsh and Bash.

- Interactive TUI search (Ctrl+R replacement) with fuzzy matching.
- Session tracking and tagging.
- AI agent activity monitoring with risk assessment.
- Statistics dashboard with hourly heatmap.
- Shell completions and man page generation.
- Self-update mechanism.
- Homebrew tap and curl-based installation.
