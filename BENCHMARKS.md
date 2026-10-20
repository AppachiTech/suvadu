# Search benchmarks

How Suvadu's search is measured, and the results of the first run. This exists
so a performance claim can be checked rather than believed: any number quoted
elsewhere should name the workflow, version, hardware and configuration, the
way the table below does.

Harness: [`tests/search_benchmark.rs`](tests/search_benchmark.rs).
Judged queries: [`tests/fixtures/search-cases.json`](tests/fixtures/search-cases.json).

The README quotes the 100,000-entry line of the latest baseline below — the
207 µs p95 recording overhead and the 0.05–15 ms p95 query range — and
nothing else. **This file is the only place a Suvadu performance number may
originate.** If you re-run the baseline, update the README in the same
commit; if you quote a figure anywhere else, quote it from here with its
hardware and corpus attached. There is no measurement at 1,000,000 entries,
so there is no claim about one.

## What is measured

**Correctness and speed are reported separately, and only correctness is
asserted.** Latency depends on the machine, so failing a build on it would
produce noise. Completeness is not machine-dependent: a missing match is a
defect at any speed.

| Metric | Meaning |
|---|---|
| eligible | Matches found by an exhaustive in-memory scan of the whole corpus — the reference answer |
| returned | Matches the database query returned. Anything less than `eligible` is a bug |
| found | Whether the planted command the case is looking for came back |
| p50 / p95 | Query latency over 20 runs of the same query |
| recording overhead | Time to record one command, which is what a shell hook pays |
| database size | On-disk size of the generated corpus |

The corpus is generated deterministically (a small LCG, seed 42, no `rand`
dependency), so runs are comparable. It contains duplicates, several working
directories, agent-executed commands, failures, a multiline command, and
non-ASCII text. The ten judged commands are planted as the **oldest** rows, so
any candidate window that favours recent history shows up as missing matches.

## Reproducing

```sh
# Fast correctness check at 10k, part of the normal suite
cargo test --test search_benchmark

# Full run, printing the tables below
cargo test --release --test search_benchmark -- --ignored --nocapture

# Larger corpora (1M takes a few minutes to generate)
SUVADU_BENCH_SIZES=10000,100000,1000000 \
  cargo test --release --test search_benchmark -- --ignored --nocapture
```

## Baseline, 19 October 2026 (schema v13)

Suvadu 0.5.0 plus the unreleased changes on `feature/0.5.1` (schema v13),
release profile. **Apple M4 Max, 128 GB RAM, macOS 27.2, rustc 1.96.0.**
Single machine, one run, load average about 3–4; treat these as an order of
magnitude, not a specification.

Re-run after schema v13 widened `idx_entries_command_started` for Smart
rank (below). Every judged case returned exactly the eligible matches and
found its planted command at both sizes, and every query is within
run-to-run noise of the schema v12 baseline that follows: this harness
measures candidate selection, which v13 does not change. The database is
1 MB larger at 100k (40 → 41 MB). Recording p95 moved from 198–203 µs to
207 µs, inside the spread of earlier runs.

### 10,000 entries — database 4 MB

Recording overhead per command: p50 104µs, p95 215µs, p99 1.10ms.

| case | eligible | returned | p50 | p95 |
|---|---:|---:|---:|---:|
| exact-old-command | 1 | 1 | 0.74ms | 0.77ms |
| partial-phrase | 1 | 1 | 0.11ms | 0.12ms |
| reordered-words | 1 | 1 | 0.13ms | 0.13ms |
| short-query | 1 | 1 | 0.04ms | 0.04ms |
| directory-specific | 1 | 1 | 0.37ms | 0.38ms |
| failed-command | 1 | 1 | 0.10ms | 0.11ms |
| multiline-command | 1 | 1 | 0.91ms | 0.94ms |
| unicode-command | 1 | 1 | 1.2ms | 1.2ms |
| unicode-case-folded | 1 | 1 | 1.2ms | 1.2ms |
| agent-command | 1 | 1 | 0.86ms | 0.90ms |

### 100,000 entries — database 41 MB

Recording overhead per command: p50 110µs, p95 207µs, p99 1.27ms.

| case | eligible | returned | p50 | p95 |
|---|---:|---:|---:|---:|
| exact-old-command | 1 | 1 | 7.7ms | 8.0ms |
| partial-phrase | 1 | 1 | 0.12ms | 0.13ms |
| reordered-words | 1 | 1 | 0.15ms | 0.16ms |
| short-query | 1 | 1 | 0.05ms | 0.05ms |
| directory-specific | 1 | 1 | 5.0ms | 5.2ms |
| failed-command | 1 | 1 | 0.12ms | 0.13ms |
| multiline-command | 1 | 1 | 10.5ms | 10.8ms |
| unicode-command | 1 | 1 | 14.8ms | 15.0ms |
| unicode-case-folded | 1 | 1 | 14.7ms | 15.3ms |
| agent-command | 1 | 1 | 11.0ms | 12.8ms |

## Baseline, 19 October 2026 (schema v12)

Suvadu 0.5.0 plus the unreleased changes on `feature/0.5.1` (schema v12),
release profile. **Apple M4 Max, 128 GB RAM, macOS 27.2, rustc 1.96.0.**
Single machine, one run, load average about 4; treat these as an order of
magnitude, not a specification.

Every judged case returned exactly the eligible matches and found its planted
command at both sizes. Against the 18 October baseline below, the two Unicode
cases fall from about 21 ms to 15 ms p95 at 100k (the case-folding SQL
function now folds the query once per statement and skips plain-ASCII
commands when the query folds to non-ASCII); the rest are unchanged within
noise. The database is larger (37 → 40 MB at 100k) because of the
case-folding prefix index added in schema v12.

### 10,000 entries — database 4 MB

Recording overhead per command: p50 104µs, p95 225µs, p99 1.18ms.

| case | eligible | returned | p50 | p95 |
|---|---:|---:|---:|---:|
| exact-old-command | 1 | 1 | 0.74ms | 0.76ms |
| partial-phrase | 1 | 1 | 0.10ms | 0.11ms |
| reordered-words | 1 | 1 | 0.13ms | 0.13ms |
| short-query | 1 | 1 | 0.04ms | 0.05ms |
| directory-specific | 1 | 1 | 0.37ms | 0.39ms |
| failed-command | 1 | 1 | 0.10ms | 0.11ms |
| multiline-command | 1 | 1 | 0.92ms | 0.96ms |
| unicode-command | 1 | 1 | 1.2ms | 1.2ms |
| unicode-case-folded | 1 | 1 | 1.2ms | 1.2ms |
| agent-command | 1 | 1 | 0.86ms | 0.89ms |

### 100,000 entries — database 40 MB

Recording overhead per command: p50 105µs, p95 203µs, p99 1.35ms.

| case | eligible | returned | p50 | p95 |
|---|---:|---:|---:|---:|
| exact-old-command | 1 | 1 | 7.5ms | 8.1ms |
| partial-phrase | 1 | 1 | 0.12ms | 0.13ms |
| reordered-words | 1 | 1 | 0.15ms | 0.17ms |
| short-query | 1 | 1 | 0.05ms | 0.05ms |
| directory-specific | 1 | 1 | 4.7ms | 5.0ms |
| failed-command | 1 | 1 | 0.12ms | 0.13ms |
| multiline-command | 1 | 1 | 10.1ms | 10.3ms |
| unicode-command | 1 | 1 | 14.9ms | 15.0ms |
| unicode-case-folded | 1 | 1 | 14.9ms | 15.2ms |
| agent-command | 1 | 1 | 11.1ms | 11.2ms |

## Baseline, 18 October 2026

Suvadu 0.5.0 plus the unreleased changes on `feature/0.5.1` (schema v11),
release profile. **Apple M4 Max, 128 GB RAM, macOS 27.2, rustc 1.96.0.**
Single machine, one run, load average about 3.6; treat these as an order of
magnitude, not a specification.

Every judged case returned exactly the eligible matches and found its planted
command at both sizes.

Compared with the 20 September baseline below: substring search now seeks
the trigram index instead of scanning it (a `LIKE … ESCAPE` was never handed
to FTS5), so most judged queries fall from 10–40 ms to well under a
millisecond at 100k. The two Unicode cases still scan, because a non-ASCII
term is matched with `suvadu_contains_ci()`, which cannot use an index.
Recording costs more because every command now updates a second trigram
index, the one behind the separator-insensitive fallback.

### 10,000 entries — database 3 MB

Recording overhead per command: p50 103µs, p95 215µs, p99 1.22ms.

| case | eligible | returned | p50 | p95 |
|---|---:|---:|---:|---:|
| exact-old-command | 1 | 1 | 0.77ms | 0.80ms |
| partial-phrase | 1 | 1 | 0.11ms | 0.12ms |
| reordered-words | 1 | 1 | 0.13ms | 0.14ms |
| short-query | 1 | 1 | 0.04ms | 0.05ms |
| directory-specific | 1 | 1 | 0.39ms | 0.40ms |
| failed-command | 1 | 1 | 0.11ms | 0.11ms |
| multiline-command | 1 | 1 | 0.94ms | 0.98ms |
| unicode-command | 1 | 1 | 1.8ms | 1.8ms |
| unicode-case-folded | 1 | 1 | 1.8ms | 1.9ms |
| agent-command | 1 | 1 | 0.86ms | 0.88ms |

### 100,000 entries — database 37 MB

Recording overhead per command: p50 108µs, p95 219µs, p99 1.43ms.

| case | eligible | returned | p50 | p95 |
|---|---:|---:|---:|---:|
| exact-old-command | 1 | 1 | 7.9ms | 8.7ms |
| partial-phrase | 1 | 1 | 0.12ms | 0.14ms |
| reordered-words | 1 | 1 | 0.16ms | 0.17ms |
| short-query | 1 | 1 | 0.05ms | 0.05ms |
| directory-specific | 1 | 1 | 4.8ms | 5.0ms |
| failed-command | 1 | 1 | 0.12ms | 0.13ms |
| multiline-command | 1 | 1 | 10.4ms | 10.9ms |
| unicode-command | 1 | 1 | 20.1ms | 20.4ms |
| unicode-case-folded | 1 | 1 | 21.2ms | 21.7ms |
| agent-command | 1 | 1 | 11.1ms | 11.6ms |

## Baseline, 20 September 2026

Suvadu 0.4.2, release profile, measured at the commit that repaired typed search
(`a765027`). Later 0.4.2 commits did not re-run it.
**Apple M4 Max, 128 GB RAM, macOS 27.0, rustc 1.96.0.** Single machine, one
run; treat these as an order of magnitude, not a specification.

Every judged case returned exactly the eligible matches and found its planted
command at both sizes.

### 10,000 entries — database 3.5 MB

Recording overhead per command: p50 82µs, p95 149µs, p99 1.18ms.

| case | eligible | returned | p50 | p95 |
|---|---:|---:|---:|---:|
| exact-old-command | 1 | 1 | 3.6ms | 3.8ms |
| partial-phrase | 1 | 1 | 1.8ms | 1.9ms |
| reordered-words | 1 | 1 | 1.7ms | 1.8ms |
| short-query | 1 | 1 | 0.8ms | 1.0ms |
| directory-specific | 1 | 1 | 1.8ms | 1.9ms |
| failed-command | 1 | 1 | 1.7ms | 2.9ms |
| multiline-command | 1 | 1 | 2.7ms | 2.8ms |
| unicode-command | 1 | 1 | 1.8ms | 1.9ms |
| unicode-case-folded | 1 | 1 | 1.9ms | 2.0ms |
| agent-command | 1 | 1 | 2.1ms | 2.2ms |

### 100,000 entries — database 33.9 MB

Recording overhead per command: p50 82µs, p95 151µs, p99 1.42ms.

| case | eligible | returned | p50 | p95 |
|---|---:|---:|---:|---:|
| exact-old-command | 1 | 1 | 43.0ms | 44.1ms |
| partial-phrase | 1 | 1 | 20.1ms | 21.0ms |
| reordered-words | 1 | 1 | 20.0ms | 20.6ms |
| short-query | 1 | 1 | 10.1ms | 10.4ms |
| directory-specific | 1 | 1 | 20.7ms | 21.2ms |
| failed-command | 1 | 1 | 20.2ms | 20.6ms |
| multiline-command | 1 | 1 | 30.7ms | 31.4ms |
| unicode-command | 1 | 1 | 21.4ms | 21.8ms |
| unicode-case-folded | 1 | 1 | 22.5ms | 23.1ms |
| agent-command | 1 | 1 | 24.7ms | 25.6ms |

### Reading these numbers

- Against the plan's provisional budget (warm p95 ≤50ms at 100k, recording
  p95 ≤10ms), both hold on this machine, but **exact-old-command at 44ms p95
  leaves little headroom**. Latency grows with the number of tokens, because
  each token adds an indexed lookup: a five-token query costs roughly four
  times a one-token query.
- The harness asks for **every** match with no limit. Interactive search ranks
  at most 5,000 matches, so it does less work than this; these are worst-case
  figures. It does count every match, but only when that window comes back
  full — i.e. only when there is more to page through.
- Non-ASCII queries (`unicode-command`, `unicode-case-folded`) use
  `suvadu_contains_ci()`, which cannot use an index and scans. At 100k they
  are comparable to indexed queries, but expect them to degrade faster on a
  much larger history.
- 1M was not run for this baseline.

### Matching modes (PROD-09)

`suv search` gained explicit matching modes. All four narrow candidates in
SQL, so none of them falls back to scanning a recent window, but they do not
cost the same. Measured on the same machine at 100,000 entries with
`cargo test --release --bin suv matching_mode_latency_on_a_large_history --
--ignored --nocapture`:

| Mode      | Query                  | Time   |
|-----------|------------------------|--------|
| `terms`   | `workspace rare_old`   | 16.9ms |
| `literal` | `workspace rare_old`   | 16.9ms |
| `prefix`  | `cargo test`           | 16.2ms |
| `fuzzy`   | `wrkspc`               | 60.2ms |

`fuzzy` narrows by the query's distinct characters, which is the only sound
superset of a subsequence match. A single-character `LIKE` cannot use the
trigram index, so each character costs a scan — roughly 3.5x the default
mode here. That is the price of the mode, not a regression in it, and
`fuzzy` is opt-in. `terms` remains the default and is unchanged.

### Separator-insensitive fallback (schema v11), 18 October 2026

Measured before and after the `terms` fallback that ignores separators
(`hiveagent` → `hive-agent`), on **Apple M4 Max, 128 GB RAM, macOS 27.2,
rustc 1.96.0**, release profile, one run each. The full baseline above was
not re-run.

- **Queries that match as typed are unchanged.** The fallback only runs
  when the strict query found nothing. `typed_search_latency_on_a_large_history`
  (100,000 entries) took 93–208 µs per query before and 102–199 µs after;
  `matching_mode_latency_on_a_large_history` took 591 µs / 361 µs / 15.8 ms /
  23.9 ms (terms / literal / prefix / fuzzy) before and 553 µs / 336 µs /
  15.4 ms / 23.3 ms after. The judged harness at 100,000 entries stayed
  within run-to-run noise (46 µs – 22 ms p50 both times).
- **The fallback itself** — `terms` `workspacerareold`, which matches nothing
  as typed and one command with separators ignored — took 526 µs at 100,000
  entries; `no-such-command`, which matches nothing either way, 190 µs.
- **Recording a command costs about 25 µs more**, because each insert now
  updates a second trigram index: p50 ~78 µs → ~103 µs, p95 ~138 µs →
  ~207 µs at 100,000 entries (`search_benchmark`, two runs each).
- **Database size**: the harness corpus at 100,000 entries grew from 33 MB to
  37 MB. A corpus of longer commands (`v11_migration_cost_on_a_large_history`
  in `src/db.rs`, about 55 characters each) grew from 53.6 MB to 67.1 MB.
- **Upgrading** builds the new index once, holding the write lock for
  0.68 s at 100,000 entries (other shells wait up to 5 s). `suv gc`'s full
  rebuild of both indexes took 1.47 s.

### Git branch capture and the zsh-autosuggestions strategy, 18 October 2026

Measured on **Apple M4 Max, 128 GB RAM, macOS 27.2, rustc 1.96.0**, release
profile, one run each. The full baseline above was not re-run.

- **Reading the branch** takes 11 µs at a repository's root, 21 µs eight
  directories below it and 14 µs in a directory outside any repository
  (`cargo test --release --lib branch_lookup_latency -- --ignored
  --nocapture`, 20,000 lookups each). It reads files only; no `git` process
  runs.
- **A whole `suv add`**, as a shell hook runs it at each prompt, took
  13.4 ms p50 / 14.3 ms p95 with branch capture on and 13.3 ms / 14.1 ms
  with it off, from a directory seven levels inside a repository (300
  interleaved runs of each through a fresh process). The difference is
  inside run-to-run noise; process start-up and opening the database are
  what a prompt pays. The hook itself adds one variable assignment when a
  command starts and one string comparison when it ends.
- **One zsh-autosuggestions lookup** is one `suv get --prefix` process, the
  same one Up-arrow runs. On a 100,000-command history (50 MB, 200
  directories, a fifth of it agent commands, imported from JSONL) each
  query ran 200 times through a fresh process:

  | Line typed | p50 | p95 |
  |---|---:|---:|
  | `g` | 22.3 ms | 24.1 ms |
  | `cargo t` | 14.2 ms | 15.4 ms |
  | `git ch` | 16.2 ms | 18.7 ms |
  | `git checkout feature/x12` | 12.6 ms | 14.4 ms |
  | `terraform plan -var x=4999` | 12.1 ms | 13.5 ms |
  | `zz-no-such-command` (no match) | 12.2 ms | 14.0 ms |
  | `--` (no match) | 12.4 ms | 13.9 ms |

  `suv --version` alone took 4.7 ms p50, so about 5 ms of each lookup is
  starting the process. A one-character line, which matches the most
  history, is the slowest and exceeds 20 ms at p95. zsh-autosuggestions 0.7
  runs strategies asynchronously by default, so this delays the suggestion,
  not the keystroke; with `ZSH_AUTOSUGGEST_USE_ASYNC` unset in older versions
  it would delay typing by the same amount.

### Prefix search seeks a case-folding index (schema v12), 19 October 2026

Measured before and after schema v12's `idx_entries_command_nocase` on
**Apple M4 Max, 128 GB RAM, macOS 27.2, rustc 1.96.0**, release profile,
one run each. The full baseline above was not re-run.

- **`suv get --prefix`**, the lookup Up-arrow and the zsh-autosuggestions
  strategy run, through a fresh process as the zsh widget calls it
  (`--offset 0 --prefix --cwd <dir>`, agent commands hidden), 200 runs of
  each line, interleaved. The history: 100,000 commands imported from
  JSONL, 18 command shapes (`git`, `cargo`, `terraform`, `docker`,
  `kubectl`, …), 200 directories, a fifth of it agent commands; 77 MB
  after `VACUUM`.

  | Line typed | before p50 | before p95 | after p50 | after p95 |
  |---|---:|---:|---:|---:|
  | `g` | 22.7 ms | 25.3 ms | 7.8 ms | 8.3 ms |
  | `cargo t` | 14.3 ms | 15.9 ms | 5.7 ms | 6.1 ms |
  | `git ch` | 14.3 ms | 16.1 ms | 5.6 ms | 6.0 ms |
  | `git checkout feature/x12` | 12.7 ms | 14.6 ms | 5.1 ms | 5.5 ms |
  | `terraform plan -var x=4999` | 12.4 ms | 13.8 ms | 5.1 ms | 5.4 ms |
  | `zz-no-such-command` (no match) | 12.2 ms | 14.0 ms | 5.1 ms | 5.4 ms |
  | `--` (no match) | 12.2 ms | 13.8 ms | 5.1 ms | 5.4 ms |

  `suv --version` alone took 4.0–4.1 ms p50, so a lookup now costs about
  1–4 ms beyond starting the process. Before, every lookup read the whole
  table: `LIKE 'git%'` folds ASCII case and the binary-ordered
  `idx_entries_command` could not narrow it. Now the prefix is one range of
  the `NOCASE` index, and the command is chosen from that index alone; only
  the chosen row is read from the table.
- **Recall's `prefix` mode**: `matching_mode_latency_on_a_large_history`
  (100,000 entries) took 15.3 ms for `prefix` `cargo test` before and 70 µs
  after, two runs each; `terms`, `literal` and `fuzzy` were unchanged
  (320–555 µs, 316–330 µs and 23–24 ms both times).
- **The judged harness** at 100,000 entries stayed within run-to-run noise
  (46 µs – 20.6 ms p50); recording took p50 101 µs, p95 193 µs.
- **Database size**: the new index replaces `idx_entries_command`, which
  `idx_entries_command_started` made redundant. On the history above it
  is 10.2 MB against the 5.2 MB it replaces, so the database grew 6.5%
  (77.0 → 82.0 MB after `VACUUM`); the harness corpus grew from 37 MB to
  40 MB. Until a `VACUUM` the file keeps the dropped index's pages free for
  reuse rather than shrinking.
- **Upgrading** builds the index once, holding the write lock for about
  0.1 s on the history above when it is in the page cache (0.9 s when it
  was read cold from disk), and 68 ms on
  `v12_migration_cost_on_a_large_history`'s corpus (`src/db.rs`); other
  shells wait up to 5 s.

### Unicode case folding, 19 October 2026

Measured before and after `terms` and `fuzzy` moved from lowercasing to
case folding (`util::fold_case`), on **Apple M4 Max, 128 GB RAM, macOS
27.2, rustc 1.96.0**, release profile, the judged harness at 100,000
entries, two runs each. The full baseline above was not re-run.

- **Non-ASCII queries got faster**, because `suvadu_contains_ci()` now
  folds the query once per statement instead of once per row, and rejects
  an ASCII command without folding it when the query folds to non-ASCII:
  `unicode-command` 19.6–19.7 ms p50 before, 14.8–14.9 ms after;
  `unicode-case-folded` 20.7–20.8 ms before, 15.0 ms after. They still
  test every row, as before.
- Folding alone, without those two changes, cost about 25% more than
  lowercasing on 100,000 ASCII commands (6.8 ms against 5.4 ms in a
  micro-benchmark), and the two cases ran at 22.5–25 ms p50.

### Smart rank: frecency, outcomes and sequences (schema v13), 19 October 2026

Measured before and after recall's Smart rank started weighing frecency,
failure rates and what follows the session's last command, on **Apple M4
Max, 128 GB RAM, macOS 27.2, rustc 1.96.0**, release profile, one run each.

- **Typing a query** — `ranked_typing_latency_on_a_realistic_history`
  (`cargo test --release --bin suv ranked_typing_latency -- --ignored
  --nocapture`): 100,000 runs of about 1,400 distinct commands from 24
  command shapes, 200 shell sessions over 90 days, a tenth failing. It types
  `git com` one keystroke at a time into one recall session opened from the
  newest shell session, so the sequence boost is live, and reports the p50
  of five passes after a warm-up. Each keystroke is a whole reload: count,
  ranked window of 5,000, ranking, first page.

  | Keystroke | Executions before | after | Commands before | after |
  |---|---:|---:|---:|---:|
  | `g` | 97–99 ms | 99 ms | 45–46 ms | 57 ms |
  | `gi` | 60–61 ms | 56 ms | 26 ms | 26 ms |
  | `git` | 57–59 ms | 53 ms | 24 ms | 25 ms |
  | `git ` | 57–59 ms | 53 ms | 24 ms | 24 ms |
  | `git c` | 67–68 ms | 65 ms | 31–32 ms | 31 ms |
  | `git co` | 31–32 ms | 29 ms | 27 ms | 26 ms |
  | `git com` | 26 ms | 24 ms | 21 ms | 21 ms |

  "Before" is two runs on the code before the change. The first keystroke
  pays for reading the statistics of every distinct command in the window
  (876 for `g` in Executions, up to all 1,400 in Commands); later
  keystrokes reuse them and only read commands not seen yet, so they cost
  what they did. Without schema v13 the statistics needed one table lookup
  per run, and the first keystroke took 128 ms (Executions) and 89 ms
  (Commands); reading 876 commands' statistics on their own took 38–43 ms
  against 8.6–9.3 ms with the wider index. The sequence lookup — the
  session's last command, then the next command after each of its newest
  500 runs — is read once per recall session.
- **The other recall benchmarks** were unchanged within noise:
  `typed_search_latency_on_a_large_history` 91–201 µs before, 119–258 µs
  after (each query matches one row, so there is nothing to rank);
  `matching_mode_latency_on_a_large_history` 533 µs / 323 µs / 69 µs /
  25.3 ms (terms / literal / prefix / fuzzy) before and 778 µs / 280 µs /
  64 µs / 24.8 ms after.
- **Upgrading** to schema v13 rebuilds one index, holding the write lock for
  61 ms on `v13_migration_cost_on_a_large_history`'s 100,000-command corpus
  (`src/db.rs`); the database grew 0.3% (64,300 → 64,492 KiB after
  `VACUUM`).
- The judged harness was re-run as the baseline above.

### Failure output and fix pairing (schema v14), 19 October 2026

Measured on **Apple M4 Max, 128 GB RAM, macOS 27.2, rustc 1.96.0**, release
profile, one run each. The search baseline above was not re-run: neither
change touches how recall searches or ranks.

- **Pairing a failure with what fixed it** — what recall's detail pane
  reads when the selection moves to a failed command, and what
  `what_failed` and `learn_from_failures` read once per distinct command on
  a page — took 631–692 µs per lookup
  (`cargo test --release --bin suv fix_pairing_cost -- --ignored
  --nocapture`): 100,000 runs of ten commands across 200 sessions, one in
  seven failing, each lookup pairing the newest 50 failures of one command
  (one seek for the failures, one per failure for the next 20 runs in its
  session). Recall reads it once per selected command, never per frame or
  per keystroke.
- **Keeping a failure's output** — cutting a 640 KB build log to its last
  64 KB, stripping its colour codes and redacting it, then cutting the
  4 KB tail — took 697 µs (`cargo test --release --bin suv
  failure_output_cost -- --ignored --nocapture`, 50 runs). It runs only for
  a failed command, only with `[capture] failure_output` on, and only on the
  agent-hook, `suv add --output-stdin` and `suv wrap` paths; a shell hook's
  recording does no extra work.
- **Upgrading** to schema v14 creates one empty table: 0.17 ms on
  `v14_migration_cost_on_a_large_history`'s 100,000-command corpus
  (`src/db.rs`).

### Agent git snapshots (schema v15), 19 October 2026

Measured on **Apple M4 Max, 128 GB RAM, macOS 27.2, rustc 1.96.0, git
2.54.0**, release profile, one run of 40 timed hook invocations per line
after 3 warm-ups (`cargo test --release --test git_snapshots hook_overhead
-- --ignored --nocapture`), in a repository of 10,000 tracked files with 50
modified and 20 untracked. The search baseline above was not re-run: nothing
here touches search.

| Claude Code hook | snapshots on | snapshots off |
|---|---|---|
| `UserPromptSubmit`, the session's first prompt (takes the start snapshot) | 39.6 ms median, 45.3 ms p95 | 9.9 ms median, 10.4 ms p95 |
| `UserPromptSubmit`, any later prompt (one lookup, no `git`) | 10.8 ms median, 11.7 ms p95 | 9.9 ms median, 10.4 ms p95 |
| `Stop`, in the repository (replaces the end snapshot) | 39.2 ms median, 40.3 ms p95 | 4.2 ms median, 4.4 ms p95 |
| `Stop`, outside a repository (no `git` process) | 4.9 ms median, 6.2 ms p95 | — |

Nearly all of the added time is `git status` itself, which scales with the
working tree; it is cut off at 2 s. It is paid once per session at the first
prompt and once per turn end, never per command. **Upgrading** to schema v15
creates one empty table: 0.19 ms on `v15_migration_cost_on_a_large_history`'s
100,000-command corpus (`src/db.rs`).

### The zsh hook's Up-arrow check, 20 October 2026

Measured on **Apple M4 Max, 128 GB RAM, macOS 27.2, rustc 1.96.0, zsh 5.9**,
release profile, 200 calls of the check in one interactive zsh, three runs
(`cargo test --release --test shell_hooks up_arrow_check_overhead --
--ignored --nocapture`). The search baseline above was not re-run: nothing
here touches search.

| What the check finds | median | p95 |
|---|---:|---:|
| Up bound to `suvadu-up-arrow`, no note to remove | 0.76–0.79 ms | 0.91–1.10 ms |
| Up rebound, the same note already written (starts `suv`) | 5.5–5.6 ms | 5.9–6.9 ms |

It runs once per shell, at the first prompt, and never per command. The
first line is the one subshell that reads the bindings; when Up has been
rebound, nearly all of the rest is starting `suv`.

## Known gaps

- **Ranking quality is not measured.** The judged set records which command
  each query should find, but the scorer lives in the binary's private search
  module, so this harness measures candidate selection at the repository
  layer. Extend it with first-relevant and top-five metrics when ranking
  changes (PROD-09), which is the point at which those metrics start to move.
- **No comparison with another tool.** Any "faster than X" claim needs both
  tools measured on the same corpus and hardware, with every setting and
  background process documented, and that has not been done.
- **Cold start, peak memory and concurrent-write impact** are not yet
  recorded.
- One machine, one run, no variance reported.
