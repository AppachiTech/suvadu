# Changelog

All notable changes to this project will be documented in this file.

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
