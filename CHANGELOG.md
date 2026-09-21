# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Added
- **Explicit matching modes and recall scopes in `suv search`** — how a query matches and where it looks are now two separate, visible controls instead of one fixed behaviour. `--match terms|literal|prefix|fuzzy` (`^X` in the UI) picks the matching rule: `terms` (the default, unchanged — every whitespace-separated word must appear as a substring, in any order), `literal` (the whole query including spaces and punctuation), `prefix` (the command starts with the query), and `fuzzy` (the query's letters appear in order with gaps, so `gco` finds `git checkout`). `--scope all|directory|workspace|session` (`^P` to cycle, `^R` to reset to `all`) picks the history to look at: everything, exactly the current directory, anywhere in the current Git repository or linked worktree, or the current shell session. Every mode narrows candidates in SQL, so none of them falls back to scanning a recent window; a scope that cannot apply here (no repository, no session) says so and falls back explicitly rather than silently. `--here` is now the spelling of `--scope directory`. `--compact` draws recall inline under the prompt instead of taking over the screen. All three have config keys — `search.match_mode`, `search.scope`, `search.compact` — and all default to the previous behaviour.
- **Bash history import** — `suv import --from bash-history <file>` (with `--dry-run`) brings an existing `~/.bash_history` into Suvadu. Timestamped files (`HISTTIMEFORMAT`, `#<epoch>` header lines) keep their real timestamps and their multi-line command boundaries; plain files have neither, so each line is imported as one command with an explicitly synthetic 1970-01-01 placeholder time rather than an invented one. Directory, exit code, duration, and executor are not in a bash history file and are stored as unknown — never a fabricated successful exit. Redaction and exclusion patterns apply exactly as for live recording, the report gives skipped/redacted/malformed counts without echoing any command text, the file is streamed (never loaded whole) and only ever read, and re-importing the same file adds nothing while repeated executions in the source are all preserved.
- **Atuin migration** — `suv import --from atuin-db <path/to/history.db>` (with `--dry-run`) brings an existing Atuin history into Suvadu. Atuin has no export command and `atuin history list --format` is lossy (humanised durations, second-resolution formatted times, trimmed commands), so the source contract is the database itself, opened **read-only** and read inside one transaction — no file copy behind SQLite's back and no `immutable=1` assumption, so a running Atuin keeps working. Tested against Atuin 18.0.0–18.22.0 (history schema `20210422143411`–`20260818000000`); an untested migration is rejected with its id and a next step instead of being guessed at. Time, directory, exit code, duration, session, host and author/author-kind are carried over (nanoseconds truncated to milliseconds); Atuin's row id, intent and shell are kept as entry context; rows deleted in Atuin are skipped; and Atuin's `-1` exit/duration is stored as unknown, never as a fabricated success. Redaction and exclusion patterns apply exactly as for live recording, repeated executions are all preserved, re-importing adds nothing, and existing Suvadu records are untouched. Unless `--no-backup` is given, a consistent backup of the Suvadu database is taken first and the exact rollback command is printed; afterwards the report confirms the Atuin file is byte-identical (SHA-256) and that the entries claimed were really written.
- **A session you can hand to another agent** — `suv sessions`' picker now has a real column set (project, prompt preview, last activity, agent, and **capture completeness as its own column**, never inferred from exit codes), so two sessions in the same project are distinguishable. A session header reports what Suvadu knows it missed (`known_missing`: a paused window, a directory where capture was off, token counters it could not follow) separately from what it never observes at all (`unverifiable`), so a clean run of exit codes is not mistaken for proof that everything was recorded. A saved summary now shows one of three states instead of a current/stale flag: **CURRENT**, **NEW ACTIVITY** (records were appended and the summary can be extended) and **EVIDENCE CHANGED** (what it was written from changed, so it must be rebuilt). Pressing `h` on a timeline builds a copyable handoff scaffold — goal, attempts, failures, decisions, changed-file evidence, verification state, open questions, next actions, source references — filled only from captured records, with every factual line citing an event or command ID and every judgement left as an explicit fill-in marker. Suvadu invokes no model and writes no prose. The same section list drives a new `prepare_session_handoff` MCP prompt.
- **`suv skills disable` / `enable` / `cleanup`, and a sync you can preview** — a skill can now be stopped from syncing without being deleted (`disable`, reversed by `enable`), and `suv skills cleanup` removes exactly the agent files Suvadu generated for skills that are gone or disabled, reporting anything it will not touch. `suv skills sync --dry-run` names the exact target file and shows a line diff of the managed region only. Every generated region carries a checksum marker of what Suvadu wrote, so a region edited outside Suvadu is reported as a **conflict and skipped** instead of being silently overwritten; `--force` is the way through. `list`/`show` and the TUI preview now show a skill's state, origin and the exact files it syncs to (or why it syncs nowhere), and `rm`/`disable` say plainly that generated files are left behind and name the cleanup step. A project-scoped skill shadows a global one of the same name instead of one overwriting the other.
- **`suv doctor` reports storage by category** — how much disk commands (with their FTS index, notes, bookmarks and tags), sessions, summaries, skills and backup files each use, measured from `dbstat` rather than estimated, plus the pages a delete freed inside the file but did not return to the OS. Every row carries the retention rule that changes it, and the backup line states that backups are never pruned automatically.
- **`suv delete --dry-run` previews the actual commands** — not just a count, so a pattern broader than intended is visible before anything goes. Both the preview and the real delete state what is removed (matching rows, their notes, their search-index entries) and what is **not**: empty shell sessions, agent session records and saved summaries stay, the deleted pages remain inside `history.db` until a `VACUUM`, and the pre-delete backup named in the output still contains every deleted command. `suv agent delete-session` states the events, commands and summaries it is about to remove before removing them.
- **A reproducible search benchmark** — `tests/search_benchmark.rs` measures correctness and speed separately over a deterministically generated corpus whose judged commands are planted as the *oldest* rows, so any recency-biased candidate window shows up as a missing match. Only correctness is asserted; latency is machine-specific. [`BENCHMARKS.md`](BENCHMARKS.md) records the method, the first baseline and, explicitly, what is still unmeasured.

### Changed
- **`suv search` control hierarchy** — the shortcut footer is now laid out to the terminal width: accept (`Enter`), cancel (`Esc`), navigation, `Ctrl+F` filters, `Tab` detail and `?` help are reserved first, secondary actions fill whatever space is left, and a hint that does not fit is dropped whole instead of being clipped mid-badge (at 100x30 the footer previously ended in a partial `^`). Every dropped shortcut is still listed in the help overlay, which is now two columns so the full list fits an 80x24 terminal; help stays visible in the footer at every width. A new always-on status row under the search box states the active scope, matching mode, unique/all results, whether AI-agent commands are included, and any other active filters. The detail pane moves below the results (instead of taking 30% of the width) on terminals narrower than 120 columns, keeping command text readable and giving multiline commands the full width. No keybindings changed.
- **One response convention across every MCP tool and resource** (`src/mcp/conventions.rs`) — handlers used to invent their own shape: three timestamp formats, `?`/`-1`/a silent `0` for the same missing exit code, silent truncation in some tools and "… and N more" in others, pagination in two tools out of twenty, and nothing telling a caller whether a line was a stored record or something Suvadu worked out. Now: RFC 3339 timestamps with an explicit offset, `unknown` as the only missing-value token, percentages that always carry their fraction, `limit`/`offset` on every list tool with `next_offset` in the trailer, visible truncation, a stable `command-<id>` on every row, concise rows by default with `detail: true` for directory, duration, executor and session, and a `provenance:` line saying **observed** (records Suvadu stored), **inferred** (anything derived from them) or **caller-reported** (text an agent wrote). Anything inferred also carries the note that Suvadu records commands, exit codes and timings — never command output or file contents. Consequences: `what_changed` no longer reports a failed `rm -rf` as a deletion that happened (the inferred category breakdown is drawn only from commands that exited 0, and failures are listed separately as attempted); `learn_from_failures` reports failure rates and says outright that it recorded no cause and observed no fix; `get_stats`, `suggest_next`, `learn_from_failures` and `project_context` state the bounded sample their rankings were computed over; `project_context` is a short briefing by default; `list_sessions` now honours `mcp.exclude_dirs`; and `get_agent_session` states `events_total`/`commands_total` alongside `events_complete`/`commands_complete`, so a convenient window cannot be mistaken for the whole session.
- **`suv status` separates recording from capture** — it used to claim "History IS being recorded." whenever the config said enabled and `SUVADU_PAUSED` was unset, which is configuration, not evidence. It now prints two independent facts: **Recording:** (enabled / disabled in config / paused in this shell / both) and **Capture:** (most recent record received *N* ago / hook observed earlier, not verified since / configuration present, capture not yet verified / not yet verified — no captured command found). Only a stored record ever counts as proof; a `SUVADU_SESSION_ID` in the environment is explicitly called out as insufficient. The newest record is shown exactly as stored, so a missing exit code prints `exit unknown` rather than a fabricated `0`, and the exact end-to-end verification sequence is printed on every run.
- **`suv doctor` groups blockers apart from optional integrations** — checks are split into "Required for shell history capture" and "Optional integrations", each check carries its own repair line, and the repairs are collected into one section, so an integration nobody uses no longer looks like a broken installation. A paused shell is repaired with `unset SUVADU_PAUSED`, not `suv enable` (which rewrites the config file and cannot clear a variable `eval $(suv pause)` exported into the running shell); both are printed when both apply. A new "Capture evidence" check reports what the stored data proves, separately from what the config allows. Agent setup is reported per agent as five separate facts — detected process, installed integration, captured commands, native sessions, MCP registration — and an agent that is neither running nor installed is reported as "not in use" with no repair.
- **A risk verdict separates severity, evidence and uncertainty** — `suv guard` and the `assess_risk` MCP tool now report the rule's severity, the span of text that matched it (redacted and length-bounded, so a report can never be the thing that copies a token into a log) and any uncertainty, on separate lines. A literal match such as `rm -rf /srv` settles what the command does; a fetched script, an installed package or an `eval`'d string does not, and the verdict says so. Both state on every path that this is a rule check on the command text, **not a sandbox**. A dangerous command quoted as an argument to a program that only searches, prints or records text (`grep`, `rg`, `git commit -m`, `git log`, `man`, `history`) is no longer flagged, provided the line chains nothing — while coverage grows, because a quote, backtick or opening command substitution now counts as a command boundary, so `bash -c "rm -rf /"`, `ssh host 'rm -rf /srv'` and `VAR=$(rm -rf /x)` are seen for what they are.
- **Documentation now describes the shipped behaviour** — README, `SECURITY.md` and `suv --help` were rewritten against the code rather than against intent: actual matching, ranking and scope semantics (with no frequency or success boost claimed, because none exists); default read-only MCP access, the two write opt-ins, the client restart and which config file the server actually reads; the stored-data inventory, real paths and backup retention; and measured benchmark figures in place of the unsupported "**<2ms** recording overhead, **<10ms** search across 1M+ entries". `CONTRIBUTING.md` gains a release checklist that covers help text, settings, docs, comparison tables, the privacy inventory and the demo recordings together.

### Fixed
- **`suv status` and `suv doctor` no longer read imported history as proof of capture** — both diagnostics classified any recent non-agent row as evidence that the shell hook was working. Importing a single timestamped Bash command dated *now* into a clean machine with no hooks installed therefore flipped `Capture:` to a green check and made `suv doctor` pass "Capture evidence", on the same screen as a warning that `~/.zshrc` did not exist. No command had ever been captured. Rows are now counted by **provenance** — `context.import_source` / `context.imported_at`, the importer's own session namespace (`import-bash-…`, `import-zsh-…`, `atuin-…`) and the JSONL importer's placeholder session hostname — and only a row no importer could have written counts. The verdict is also **scoped to the shell being diagnosed**: a live record from this shell's session proves capture outright; one from another session proves it only while this shell has `suv init` in its rc file; otherwise the diagnostics say a record arrived but not from here. An import-only setup now reports `not yet verified — all N stored shell record(s) came from an import, which is stored history, not capture`, names the rows in a `Note:`, and prints a `To start capturing:` block (`suv init zsh >> ~/.zshrc`, plus a reminder that imported history stays searchable either way) ahead of the unchanged three-step `echo suvadu-capture-check` verification sequence that resolves the state. `suv doctor` carries the same repair on the "Capture evidence" row.
- **The zsh importer now records import provenance** — unlike the Bash and Atuin importers it wrote no `context` at all, so a zsh-imported row was indistinguishable from one the live hook recorded. Every row it writes now carries `import_source=zsh-history`, `imported_at`, `timestamp_source` (`file`, or `synthetic` for a plain history file that carries no times) and `unknown_fields`. Rows imported by an earlier version have no such context and are still recognised by their `import-zsh-…` session.
- **A saved AI-session summary is fingerprinted over readable events, not raw rows** — `ai_session_prefix_hash` sliced `ai_events` with `LIMIT ?2`, but the count it was given (`source_event_count`) counts *readable* records, the same unit as `event_count`, `events_total` and `resume_event_offset`. In a session holding a record suvadu cannot decode, the fingerprint therefore stopped short of the last readable events the summary was actually written from, and a later change to one of them left the checkpoint reporting `NEW ACTIVITY` / `incremental_safe` instead of stale. Unreadable rows are now skipped before the window is cut, the same way `get_agent_session` pages them (see the event-pagination fix above). Command rows are unchanged: an unparseable `context` degrades to null rather than dropping the row, so their raw count is the right window.
  - **Effect on summaries saved before this fix:** the stored `source_prefix_hash` of a summary that was saved *while its session already contained an unreadable event row before the last covered event* no longer matches the recomputed one, so that summary reads as `INVALIDATED` (`stale: true`, `incremental_safe: false`, no `resume_event_offset`) and must be rebuilt from the evidence rather than extended. Its text, agent, model, source ids and timestamps are untouched, and nothing is deleted. Summaries in sessions with no unreadable rows — the overwhelming majority — hash identically and are unaffected.
- **An Atuin `intent` can no longer smuggle a secret past redaction** — only the command went through the recording policy, so free-form metadata Atuin stores beside it (`intent`, `author`, `shell`, `hostname`) was copied verbatim into the entry's `context`. Importing a perfectly clean `echo normal` whose `intent` held a GitHub-token-shaped string stored the whole token, and the import reported **zero** redactions. Every free-text field now goes through the same redaction and exclusion patterns as the command before it is persisted or previewed: a match is rewritten, a field an *exclusion* pattern matches is withheld entirely and named in `context.withheld_fields` rather than quietly vanishing, and the report counts both (`Redacted before storage: N row(s); M metadata field(s) rewritten or withheld`). A row whose only offending text was in metadata now counts as redacted instead of looking clean.
- **The Atuin importer no longer drops distinct executions** — it added an occurrence ordinal to each millisecond timestamp to separate sub-millisecond repeats and then deduplicated on (command, adjusted time), so the invented time could collide with a genuinely different execution: three `true` runs at `…000000000`, `…000000001` and `…001000000` ns previewed as three imports, stored **two**, reported one "Already present" and lost source row `2`. Idempotency is now decided by the row's own Atuin `id`, already stored as `context.atuin_id`: every `atuin_id` previously imported is read once before the pass, and a row is skipped only when its own identity is already present. Times are therefore never invented — `started_at` is the source timestamp truncated to milliseconds and nothing more — so two executions inside one millisecond both survive, an unrelated Suvadu row that happens to share a command and a millisecond is no longer mistaken for one of them, and a dry run reaches exactly the same verdict as the apply.
- **An Atuin import now honours each source directory's project policy** — the importer loaded the global configuration once, so a row whose `cwd` contained a `.suvadu.toml` with its own `redaction.extra_patterns` and `exclusions` imported both the matching secret and the excluded command. Policy is now resolved per source directory exactly as live recording resolves it for a command's own `cwd` (global config plus the nearest `.suvadu.toml` above that directory, cached per directory). A row whose directory Atuin did not record, or which does not exist on this machine, falls back to the global config, and an unreadable overlay stops the import rather than silently importing under a looser policy. The Bash and Zsh importers cannot do this — their history files carry no directory — and `SECURITY.md`'s per-path table now says so per path instead of claiming a flat "yes".
- **Typed interactive search now matches across the whole history** — `suv search`/`Ctrl+R` fetched the newest 5,000 eligible rows and then matched them in memory, so an exact match older than that vanished from the UI while still sitting in the database. Candidate selection now happens in SQL over the entire eligible history; the 5,000 limit remains, but it bounds how many *matches* are ranked, not how much history is searched. SQLite's `LIKE` folds case for ASCII only, so a query token containing non-ASCII characters used to miss a command differing only by the case of a non-ASCII letter; those tokens now go through a new `suvadu_contains_ci()` SQL function using Rust's Unicode lowercasing, while ASCII tokens keep using `LIKE` against the trigram index.
- **Interactive search reports a result count it can actually page to** — the 5,000-match ranking window was also used as the *total*, so a broad query claimed exactly 5,000 results, refused to go past page 100, and put any older eligible match — including an exact one — out of reach. Both the ordinary and the unique branch did this. Matching and counting now finish in the database for every mode, so the count is the true number of eligible matches and every page it implies can be opened. Ranking is the only part that stays bounded: the newest 5,000 matches are ordered by relevance and anything beyond them is paged newest-first, because a relevance order computed from part of a result set is arbitrary. A count is never reported that cannot be paged to.
- **`--query` now goes through the same pipeline as typing** — starting recall with a query loaded the SQL candidate set straight into the UI, skipping the eligibility and ranking stage every other interaction uses. In `fuzzy` mode SQL only checks that each distinct query character occurs *somewhere*, so `suv search --query gco --match fuzzy` could return `ocg` while typing `gco` returned `git checkout`. Startup, typing, editing, mode and scope changes and pagination are one pipeline with one answer.
- **`fuzzy` keeps its own ordering rule** — the subsequence check only ran when nothing matched literally, so a query whose words each appeared slipped past it: typing `checkout git` in `fuzzy` mode returned `git checkout`, which the documented "letters in order, gaps allowed" rule does not accept. Mode eligibility is now decided independently of how well an entry ranks, and SQLite evaluates the rule itself through a new `suvadu_subseq_ci()` function that shares its implementation with `MatchMode::matches`. `terms` still matches words in any order — that difference is the point of having both.
- **An upper-case query in `terms` mode matches case-insensitively, as documented** — the relevance scorer treats a capital letter as a request for a case-sensitive match, and it was also acting as the filter, so `GIT` found nothing while `git` found everything. Ranking no longer decides eligibility; an entry with no relevance signal is ordered last, not dropped.
- **`Enter` on a search with no results leaves recall** — it previously did nothing at all, so a query that matched nothing could only be escaped with `Esc`. Accepting when there is nothing to accept now closes recall with no selection and the shell keeps its buffer. `Enter` with results present but no row selected is unchanged.
- **`suv skills sync` no longer deletes instructions written below its marker** — the ownership check only looked at the text *above* the checksum marker, so anything appended under it was invisible to drift detection and to the `--dry-run` diff, while the write replaced the whole file: a hand-written paragraph at the bottom of a generated `SKILL.md` or `.mdc` rule was previously destroyed silently, without a conflict and without `--force`. The marker is the last line Suvadu writes, so anything after it is now treated as somebody else's text: the file is reported as a **conflict** and left alone, the preview shows that text as removed rather than promising a clean "would update", and `--force` remains the only way through. `suv skills cleanup` uses the same complete check and now leaves such a file in place instead of deleting it. Codex's `AGENTS.md` was never affected — Suvadu owns only a marked block there — and text outside that block is still preserved untouched.
- **A project-scoped skill no longer uninstalls the global skill of the same name** — `suv skills cleanup` derived the expected *global* destinations from the skills resolved for the current project, where a project-scoped `alpha` shadows global `alpha`; the global one therefore looked orphaned, and running cleanup from that project deleted the global `~/.claude/skills/alpha/SKILL.md` and stripped the global Codex managed block, while the library still listed both skills as active. Every other project lost the skill. Global destinations are now decided from the active global library entries, before any project shadowing; a global file is still removed once the global skill itself is gone or disabled.
- **Complete and consistent MCP settings** — the MCP tab in `suv settings` listed only 15 of the 21 default tools and seven of the eight resources, and exposed neither write opt-in, so enabling saved session summaries or skill proposals meant hand-editing `config.toml`. Tool and resource metadata now lives in one shared catalog (`src/mcp/catalog.rs`) that drives both the server's advertisement and the settings controls, with tests asserting the two sets are identical by name rather than by an expected count. `Allow Saved Session Summaries` and `Allow Skill Proposals` are now rows on that tab (still off by default), each tool row shows its *effective* state and the reason it is off instead of two switches that can disagree, and the tab states that the MCP server reads its configuration at startup so the client must be restarted for a change to take effect. Saving from the settings UI also preserves keys the running build does not know about, so a hand-edited or newer-version `config.toml` is no longer silently trimmed.
- **`curl … install.sh` on macOS downloaded the wrong build, or nothing at all** — the installer mapped architecture to an archive suffix without regard to the operating system, but the release workflow leaves each OS's primary target unsuffixed and they differ. On Apple Silicon it requested an archive that is never published (HTTP 404); on an Intel Mac it downloaded the arm64 build. Linux matched by coincidence. OS and architecture are now resolved together, a failed download explains itself and points at Homebrew and Cargo, all four supported combinations are checked against what is actually published, and the release workflow now publishes `scripts/install.sh` itself — the served copy had been uploaded by hand and drifted behind the repository.
- **Pasting a wrapped command into `suv search` no longer makes it unmatchable** — pasted newlines and tabs were dropped rather than turned into spaces, so `cargo test\n--offline` became `cargo test--offline`. They now collapse to a single space and the result is trimmed.
- **`resolve_current_agent_session` never resolves "this session" without a directory to match** — it previously fell through to the recent-list filter even when no working directory could be determined, so a single captured session from an unrelated project resolved as `source=unambiguous_cwd` although no directory was ever compared. No directory now means no resolution, more than one match is always the user's choice, a truncated candidate list says so, and a directory-only match carries its match reason and a verification note.
- **A recorded command can no longer forge an MCP row or repaint a terminal** — command text is arbitrary bytes someone typed, so a newline inside one could split a response row in two or append a second `---` trailer an agent would read as the real one, and an ANSI escape inside one reached whoever printed the response. `echo $'\e[2J'` is a real command to have run, so this needed no attacker. Control characters are now flattened to a single space before the length limit applies.
- **`suv doctor` no longer creates the database as a side effect** — it reports "not created yet" instead of silently initialising one, and `db::backup_dir_path` reports the backup location without creating it.
- **`suv uninstall` prints the real paths** — it listed an invented `~/Library/Application Support/suvadu/` (the bundle identifier is `tech.appachi.suvadu`) and a Linux hint that omitted `~/.local/share/suvadu`. It now lists the actual config and data directories resolved from `project_dirs()`.
- **`suv settings` no longer drops config keys it does not know** — saving one field re-serialized the whole config, silently discarding any key the running build has no struct field for, such as a hand-added key or one belonging to a newer version. Saves now merge over the existing file's TOML tree.

### Security
- **Session summaries saved over MCP now go through redaction and exclusions** — a summary was stored exactly as the agent wrote it. A summary is derived text: an agent that read a session can restate a secret the redactor caught on the way in. `save_session_summary` now applies the configured policy, **refusing** text that matches an exclusion rather than silently trimming it, and `suv mcp-serve` installs the user's redaction, exclusion and risk settings at startup.
- **The Zsh importer now applies redaction and exclusions like every other ingestion path** — `suv import --from zsh-history` stored whatever the file contained, so a secret already sitting in `~/.zsh_history` was copied in verbatim and configured exclusion patterns were never consulted. Importing could therefore store text that typing the same command would have redacted. It now runs the same recording policy as live recording and the Bash importer, and reports excluded/ignored/redacted counts in both the dry run and the real import. `SECURITY.md` carries a per-path table of which ingestion paths apply which rule.

## [0.4.1] - 2026-09-13

### Added
- Existing Codex users must run `suv init codex` after upgrading, review/trust Suvadu’s new Stop/SessionEnd handlers using `/hooks` in the Codex terminal CLI, then relaunch Codex (fully quit/reopen VS Code for the extension). Installer/update output, stale-hook reminders on interactive commands, and empty-session diagnostics surface these setup steps.
- Codex session capture: incremental native transcript import at Stop/SessionEnd, prompts and final answers independent of shell commands, and provider-reported token counters without double-counting cumulative updates. Inspect with `suv agent sessions` / `suv agent session`, import with `suv agent import-session`, and remove imported sessions with `suv agent delete-session`.
- Claude Code session capture: `suv init claude-code` now installs Stop/SessionEnd reconciliation hooks and incrementally captures human prompts, assistant text, every observed model, and deduplicated provider-reported usage. Claude cache-read and cache-creation tokens are normalized into total input while remaining visible separately in the raw session/event data (MCP `get_agent_session`); the `suv sessions` TUI currently surfaces cache-read tokens only. Thinking blocks, attachments, images, file contents, and tool results are excluded. Existing users must rerun `suv init claude-code` and relaunch Claude Code (fully quit/reopen VS Code for the extension).
- OpenCode session capture: prompts, assistant responses, model, and token usage are now captured (in addition to the existing bash-command tracking), browsable with `suv agent sessions` / `suv agent session` and summarizable through the same MCP tools and TUI summary panel as Claude Code and Codex. Capture happens when a session goes idle, via OpenCode's own `session.messages` API; a message still streaming or interrupted mid-turn (including the plugin's best-effort `dispose()` flush on process exit) is captured once it actually completes rather than from partial data, and multi-turn token totals are accumulated across the whole session, since OpenCode reports each model response's own usage rather than a running total. The live prompt cache that pairs a prompt with the commands run for it is redacted and length-capped the same way as every other integration, honoring a project's own `.suvadu.toml` overlay for both the cache and the command context it's attached to; a command in a more strictly-configured nested directory still links to its imported prompt (matched by an HMAC keyed with a per-install, owner-only secret, not a bare hash of the redacted secret text) even though its own copy is redacted more heavily than the session-wide import. `suv init opencode` now also registers the plugin directory in `~/.config/opencode/opencode.jsonc`'s `plugin` array, and `suvadu` as a local MCP server under its `mcp` key, automatically (OpenCode does not reliably auto-discover plugins from the directory alone, and had no other way to reach Suvadu's MCP tools before this); if that config already has JSONC comments this parse can't preserve, it prints the exact lines to add instead of risking corrupting the file. Existing users must rerun `suv init opencode` to install the updated plugin and pick up both registrations, then relaunch OpenCode.
- Cross-agent MCP session summaries: `list_agent_sessions`, `get_agent_session`, `resolve_current_agent_session`, and summary prompts let any connected agent summarize a captured session by ID or resolve “current session” conservatively. `resolve_current_agent_session` resolves OpenCode sessions the same way as Codex and Claude Code: an explicit native ID, or an unambiguous cwd/agent match. Long sessions are processed page by page into compact running notes with evidence IDs and revision consistency checks. Saved summaries are reusable checkpoints: append-only activity is fetched from independent event/command offsets and merged, while changed earlier evidence forces a full rebuild. Optional `save_session_summary` requires `mcp.allow_session_summaries = true`, current source revision, and session evidence IDs.
- Unified session browser: `suv sessions` now combines human terminal sessions, command-backed agent sessions, and native Codex/Claude Code sessions in one picker. `Ctrl+T` cycles All/Human/AI; AI timelines show captured prompts, responses, commands, model changes, and provider-reported token totals, including sessions with no commands.
- AI-session summary panel: press `s` on an AI session in `suv sessions` to view its saved MCP-generated summaries in a scrollable overlay. `Tab` toggles between a lightweight rendered view and the raw markdown, `Ctrl+Y` copies the raw markdown, and `[`/`]` cycle between multiple saved summaries for the same session.
- **Builtin session-memory skill** — `suv init claude-code` and `suv skills sync` now seed and materialize a suvadu-owned `suvadu-session-memory` skill (gated on `mcp.allow_session_summaries = true`) that steers connected agents toward Suvadu's own session-summary tools instead of writing a generic local memory note when asked to "summarize this session." Reaches Claude Code, and — since a skill isn't host-restricted — Cursor's project rules and Codex's `AGENTS.md` too on a default `suv skills sync`.

### Fixed
- `suv mcp-serve` no longer disconnects before completing the MCP handshake when the local database was already migrated to a newer schema by a different Suvadu build (e.g. a local dev build) — every tool/resource call now reports the underlying database error instead of the connected host seeing an opaque "Connection closed".
- Current Codex `response_item` transcripts now capture real `user.text` prompts and final assistant answers while excluding developer instructions, injected AGENTS/environment context, commentary, reasoning, and duplicate item-completion records.
- Codex session imports now accept bounded records up to 16 MiB, allowing image-bearing prompts and compaction snapshots to be read without storing embedded images or rolling back model and token capture.
- Up/Down navigation now continues across page boundaries in Search, Sessions, Agent Dashboard, and both Prompt Explorer views while preserving explicit Left/Right page navigation — this also fixes trackpad/scroll-wheel gestures on terminals that send them as Up/Down key presses, since Suvadu doesn't request mouse-capture mode.

## [0.4.0] - 2026-09-09

### Added
- **Shared skills library** — `suv skills` keeps reusable instructions in one place instead of every AI tool maintaining its own copy. `suv skills add/list/show/edit/rm` manage skills scoped to `global` or a specific project directory; three new read-only MCP tools (`list_skills`, `get_skill`, `search_skills`) plus an auto-injected `suvadu://skills/index` resource let any MCP-capable agent discover and read them directly.
- **`suv skills` is now a full interactive management app** — bare `suv skills` (no subcommand) launches a TUI to browse/filter skills with a live preview, add or edit one (body edited via `$EDITOR`), delete with confirmation, trigger a sync, and approve/reject pending agent-proposed skills. `add`/`list`/`show`/`edit`/`rm`/`sync` keep working exactly as before for scripting.
- **`suv skills sync`** — materializes active skills into each agent's own native format (`~/.claude/skills/<name>/SKILL.md`, `.cursor/rules/<name>.mdc`, a managed block in `AGENTS.md` for Codex) for hosts that don't pull from MCP. Idempotent — only rewrites a file when its content actually changed, and never touches hand-written content around its managed section.
- **`propose_skill` MCP tool** (off by default) — lets an agent propose a new skill, saved as `pending_review` only, never active. Enable with `mcp.allow_skill_proposals = true` in `config.toml`; a human approves or rejects proposals from the review queue in `suv skills` (`Ctrl+P`). Disabled by default since a skills store readable and writable by agents is a shared-memory poisoning target — the gate is checked before any database connection is opened.
- **MCP server now exposes 3 prompts** — `project_briefing`, `check_recent_failures`, and `assess_command_risk` are reusable, parameterized requests a client can surface directly (often as slash commands), instead of relying on the connected model to decide on its own to call a tool. The `initialize` handshake's `instructions` field is also now directive rather than purely descriptive, telling a connecting client when to proactively call `project_context`/`learn_from_failures`/`assess_risk`/`list_skills`.
- **Risk assessment now catches obfuscated commands** — `eval`, decoding base64 into a shell, and command substitution wrapping a destructive/network command (e.g. `` echo $(curl ... | sh) ``) are now flagged, closing a gap where wrapping a dangerous command in indirection let it slip past risk assessment entirely. You can also flag your own org-specific risky commands with a new `agent.risk_extra_patterns` config list (regex + level + description).
- **Prompt Explorer gets an always-on search box and filters** — `suv agent prompts` now live-filters by prompt text as you type, with `Ctrl+P`/`Ctrl+A` cycling time period/executor and `Ctrl+S` jumping to a prompt's session timeline, mirroring `suv search`'s conventions.
- **`suv guard <command>`** — assess a command's risk before it runs and exit non-zero if it's too dangerous, for use in git hooks, CI, or a zsh widget (documented in `--help`) that blocks risky commands interactively before they execute.
- **Per-project config overlay** — drop a `.suvadu.toml` in a project directory (or any ancestor, discovered the same way as `.git`) to override the global config just for that project, e.g. a stricter risk policy or a different exclusions list for one sensitive repo. Table fields merge key-by-key, so the overlay only needs to name what it's actually changing.
- **`suv bookmarks`** (renamed from `suv bookmark`, old name kept as an alias) **opens a full interactive picker** — fuzzy-search and recall a bookmarked command, or add, edit, and delete bookmarks right from the TUI (`Ctrl+A`/`Ctrl+E`/`Ctrl+D`) instead of separate `add`/`rm` invocations for every change. Opens even with zero bookmarks saved yet.
- **`suv aliases`** (renamed from `suv alias`, old name kept as an alias) **opens a full interactive manager** — fuzzy-search your aliases, or add, edit, and delete them right from the TUI (`Ctrl+A`/`Enter`/`Ctrl+D`); every change regenerates the sourced `aliases.sh` automatically, so it's live in new shells without a separate `suv aliases apply`. `add`/`remove`/`list`/`apply`/`add-suggested`/`suggest` keep working exactly as before for scripting.

### Changed
- **Command search is faster** — `suv search`/`Ctrl+R` substring matching is now backed by a trigram full-text index instead of a full table scan, so search stays fast as your history grows. Same results, just quicker.
- **`suv --help` groups commands by purpose** (Setup, Search & recall, Insights & safety, Organize, Data, AI integration, Other) instead of one flat list, and surfaces the `Ctrl+R`/arrow-key shortcuts up top.
- **Visual consistency pass across `suv search`, `suv agent dashboard`, `suv agent prompts`, `suv agent stats`, `suv stats`, `suv skills`, `suv bookmarks`, `suv aliases suggest`, and `suv sessions`** — all now share the same centered `SUVADU <SCREEN>` title and footer badge styling with Quit listed first; the search-driven screens also share an always-on live-filter search box and `Ctrl+<letter>` shortcuts. The dashboard's agent/risk summary, the prompt detail view's session info, and the session timeline's session info are now shown in their own boxed panels instead of dense single lines, agent stats' cards use rounded borders, and its High Risk Commands table now fills the available width instead of truncating commands at a fixed 30 characters.
- **`suv session` renamed to `suv sessions`** (old name kept as an alias) — same session picker and timeline as before; the picker's search box is now always-on like every other search-driven screen (Esc quits immediately instead of clearing the query first, and letters/digits are always query text rather than doubling as `q`/vim-style navigation shortcuts).

### Fixed
- **Closing a session timeline from `suv sessions` now returns to the session list** instead of exiting straight back to the shell — previously the only way out of a timeline was closing the picker entirely.
- **Pasting a multi-line skill into `suv skills`' Add/Edit form no longer misfires** — Enter used to submit the form (jumping straight to `$EDITOR` for the body) regardless of which field had focus, so a paste with an embedded line break could launch the editor before later fields were even filled in. Enter now advances fields like Tab, only submitting on the last one; the form is also bigger and each field caps at 200 characters.
- **`suv stats`'s content boxes now use rounded borders, and `suv settings`' title matches every other screen** — the earlier visual-consistency pass fixed both screens' title/footer styling but missed `suv stats`' border style and never touched `suv settings`' title casing.
- **Prompt Explorer's Session field now shows the full session ID** instead of truncating to 8 characters — for agent-run entries that ID is the underlying agent's own session ID (e.g. Claude Code's), and the truncated form wasn't enough to do anything useful with it, like `claude --resume <id>`.

### Security
- **`mcp.exclude_dirs` is now actually enforced** — this config option was documented and settable via `suv settings`, but no MCP tool or resource handler ever checked it, so directories you'd excluded were still fully readable by any MCP-connected agent. Every tool and resource that reads command history now filters them out at the query level (so it also correctly reduces aggregate counts, not just listed commands), and `~`-prefixed entries (e.g. `~/.ssh`) are expanded and matched against their whole subtree.

## [0.3.7] - 2026-09-07

### Added
- **Codex CLI is now a first-class agent integration** — `suv init codex` installs hooks that capture both prompts and shell commands, merging into any hooks you already have in `~/.codex/hooks.json` (synapse, plannotator, etc.) rather than overwriting them, and auto-registers the suvadu MCP server in `~/.codex/config.toml` so Codex can query your shell history directly. The config file is edited in place, so existing comments and formatting survive untouched. Closes #30.

### Fixed
- **`.bashrc` commands sourced before the first prompt are no longer recorded as if you'd typed them** — bash's `DEBUG` trap fires for every command bash runs, not just ones typed at an interactive prompt, so setup lines left in `.bashrc` after the `eval "$(suv init bash)"` line (an `export`, say) were getting captured and recorded when the first prompt was drawn. Fixes #32.

## [0.3.6] - 2026-08-01

### Added
- **`PageUp`/`PageDown`/`Home`/`End` navigation in the `Ctrl+R` search UI** — `PageUp`/`PageDown` scroll the selection 10 rows at a time, and `Home`/`End` jump to the first/last result. Works in both normal and vim-normal modes. Closes #29.

### Fixed
- **Codex CLI commands are now auto-recognized as agent activity** — detection previously checked for `CODEX_CLI`, which Codex CLI never actually sets; it now checks `CODEX_THREAD_ID`, the variable Codex really exports. Closes #30.
- **`cargo test` no longer backgrounds itself under Bash/Zsh alias detection** — running several interactive alias-detection shells concurrently (as `cargo test` does) could raise `SIGTTOU` and suspend the whole test process. These shells are now detached into their own session so they no longer contend for the controlling terminal. Closes #26.

## [0.3.5] - 2026-07-22

### Added
- **Bookmarks-only filter in the `Ctrl+R` search UI** — press `Ctrl+O` to show only bookmarked commands, mirroring the existing failed-only/agent filters.

### Fixed
- **`brew install` on Linux now installs the correct binary** — the Homebrew formula only ever pointed at the macOS build, so Linux installs (x86_64 and aarch64) silently downloaded a macOS binary and failed with "Exec format error". The formula and release automation now publish and select the right binary per OS/architecture.
- **Shell hooks self-heal a stale `suv` binary path** — switching install methods (e.g. `cargo install` → `brew`, or `brew unlink`) no longer silently breaks command recording; the zsh/bash wrappers now re-resolve the binary automatically or show a clear error if none can be found.
- **Recorded timestamps and durations were wrong under comma-decimal locales** (Polish, German, French, and others) — `$EPOCHREALTIME` parsing now forces `LC_ALL=C`, since locale-dependent decimal separators were silently corrupting the millisecond calculation.

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
