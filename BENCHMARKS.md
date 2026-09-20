# Search benchmarks

How Suvadu's search is measured, and the results of the first run. This exists
so a performance claim can be checked rather than believed: any number quoted
elsewhere should name the workflow, version, hardware and configuration, the
way the table below does.

Harness: [`tests/search_benchmark.rs`](tests/search_benchmark.rs).
Judged queries: [`tests/fixtures/search-cases.json`](tests/fixtures/search-cases.json).

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

## Baseline, 20 September 2026

Suvadu at the PROD-01 search repair, release profile.
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
  figures.
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
