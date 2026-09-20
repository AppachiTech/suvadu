//! Search correctness and performance harness (PROD-10).
//!
//! Correctness is measured separately from speed, and only correctness is
//! asserted: latency depends on the machine, so failing a build on it would
//! produce noise rather than signal. `benchmark_search` prints the numbers and
//! is `#[ignore]`d; `search_finds_every_eligible_match_at_10k` runs in the
//! normal suite and fails if any eligible match is missing.
//!
//! Reproduce:
//!
//! ```sh
//! cargo test --release --test search_benchmark -- --ignored --nocapture
//! SUVADU_BENCH_SIZES=10000,100000,1000000 cargo test --release \
//!     --test search_benchmark -- --ignored --nocapture
//! ```
//!
//! Method and recorded results: `BENCHMARKS.md`.

use std::time::{Duration, Instant};

use serde::Deserialize;
use suvadu::db::init_db;
use suvadu::models::{Entry, SearchField, Session};
use suvadu::repository::{QueryFilter, Repository};
use tempfile::TempDir;

/// Ranking is not measured here: the search module is private to the binary,
/// so this harness exercises the repository layer, which is where candidate
/// selection (and therefore completeness) lives. When ranking changes under
/// PROD-09, extend this with judged first-relevant and top-five metrics.
const CANDIDATE_LIMIT: usize = 1_000_000;

#[derive(Deserialize)]
struct JudgedCases {
    cases: Vec<JudgedCase>,
}

#[derive(Deserialize)]
struct JudgedCase {
    name: String,
    query: String,
    expect: String,
}

fn judged_cases() -> Vec<JudgedCase> {
    let raw = include_str!("fixtures/search-cases.json");
    serde_json::from_str::<JudgedCases>(raw).unwrap().cases
}

/// Commands planted once each, referenced by the judged cases.
fn planted() -> Vec<&'static str> {
    vec![
        "kubectl rollout restart deployment/api-gateway",
        "psql -h analytics.internal -U reporting -c 'vacuum analyze events'",
        "pnpm build --filter @acme/checkout",
        "terraform apply -var-file=prod.tfvars",
        "for host in web01 web02 web03\ndo\n  ssh $host 'systemctl status suvadu'\ndone",
        "git commit -m 'café latte parser: handle Émile'",
        "cargo nextest run --workspace --no-fail-fast",
    ]
}

/// Deterministic filler so runs are comparable: a tiny LCG, no rand dependency.
struct Lcg(u64);

impl Lcg {
    const fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[usize::try_from(self.next()).unwrap() % items.len()]
    }
}

/// Builds a corpus of `size` entries: the planted commands oldest-first,
/// then deterministic filler with duplicates, directories, agent commands,
/// failures and multiline records.
fn generate_corpus(size: usize) -> (TempDir, Repository) {
    let dir = TempDir::new().unwrap();
    let repo = Repository::new(init_db(&dir.path().join("bench.db")).unwrap());
    repo.insert_session(&Session {
        id: "bench".into(),
        hostname: "bench-host".into(),
        created_at: 0,
        tag_id: None,
    })
    .unwrap();

    let vocabulary = [
        "git status",
        "git log --oneline -20",
        "cargo build",
        "cargo test --workspace",
        "npm run dev",
        "ls -la",
        "docker compose up -d",
        "kubectl get pods",
        "rg TODO src/",
        "make lint",
    ];
    let directories = [
        "/Users/dev/projects/api",
        "/Users/dev/projects/web",
        "/Users/dev/projects/infra",
        "/Users/dev",
    ];
    let mut rng = Lcg(42);

    let push =
        |repo: &Repository, i: usize, command: &str, cwd: &str, agent: bool, failed: bool| {
            let stamp = 1_000 + i64::try_from(i).unwrap();
            repo.insert_entry(&Entry {
                id: None,
                session_id: "bench".into(),
                command: command.to_string(),
                cwd: cwd.to_string(),
                exit_code: Some(i32::from(failed)),
                started_at: stamp,
                ended_at: stamp + 1,
                duration_ms: 25,
                context: None,
                tag_name: None,
                tag_id: None,
                executor_type: Some(if agent { "agent" } else { "human" }.into()),
                executor: Some(if agent { "claude-code" } else { "terminal" }.into()),
            })
            .unwrap();
        };

    // Oldest rows: everything the judged cases look for.
    for (i, command) in planted().iter().enumerate() {
        let agent = command.starts_with("cargo nextest");
        let failed = command.starts_with("terraform");
        push(&repo, i, command, "/Users/dev/projects/api", agent, failed);
    }

    for i in planted().len()..size {
        let command = vocabulary[usize::try_from(rng.next()).unwrap() % vocabulary.len()];
        let cwd = rng.pick(&directories);
        let agent = rng.next().is_multiple_of(10);
        let failed = rng.next().is_multiple_of(20);
        push(&repo, i, command, cwd, agent, failed);
    }

    (dir, repo)
}

/// Every entry whose command contains all of `query`'s tokens, compared the
/// way the interactive scorer compares them. The reference the database
/// result is judged against.
fn reference_matches(all: &[Entry], query: &str) -> Vec<String> {
    let tokens: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    all.iter()
        .filter(|e| {
            let haystack = e.command.to_lowercase();
            tokens.iter().all(|t| haystack.contains(t))
        })
        .map(|e| e.command.clone())
        .collect()
}

const fn query_filter(tokens: &[String], show_agents: bool) -> QueryFilter<'_> {
    QueryFilter {
        query_tokens: tokens,
        after: None,
        before: None,
        tag_id: None,
        exit_code: None,
        query: None,
        prefix_match: false,
        executor: None,
        cwd: None,
        field: SearchField::Command,
        exclude_agents: !show_agents,
        cwd_prefix: false,
        failed_only: false,
        bookmarked_only: false,
        exclude_dirs: &[],
    }
}

fn all_entries(repo: &Repository) -> Vec<Entry> {
    repo.get_entries_filtered(CANDIDATE_LIMIT, 0, &query_filter(&[], true))
        .unwrap()
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let idx = (((sorted.len() - 1) as f64) * p).round() as usize;
    sorted[idx]
}

/// Coverage per judged case: how many of the eligible matches the database
/// query actually returns, and whether the planted command is among them.
fn coverage(repo: &Repository, corpus: &[Entry]) -> Vec<(String, usize, usize, bool)> {
    judged_cases()
        .into_iter()
        .map(|case| {
            let tokens: Vec<String> = case.query.split_whitespace().map(str::to_string).collect();
            let expected = reference_matches(corpus, &case.query);
            let found = repo
                .get_entries_filtered(CANDIDATE_LIMIT, 0, &query_filter(&tokens, true))
                .unwrap();
            let found_expected = found.iter().any(|e| e.command == case.expect);
            (case.name, expected.len(), found.len(), found_expected)
        })
        .collect()
}

#[test]
fn search_finds_every_eligible_match_at_10k() {
    let (_dir, repo) = generate_corpus(10_000);
    let corpus = all_entries(&repo);

    for (name, expected, found, found_planted) in coverage(&repo, &corpus) {
        assert_eq!(
            found, expected,
            "case {name}: database returned {found} of {expected} eligible matches"
        );
        assert!(
            found_planted,
            "case {name}: the planted command was not returned"
        );
    }
}

#[test]
#[ignore = "generates large corpora; run explicitly with --ignored"]
fn benchmark_search() {
    let sizes: Vec<usize> = std::env::var("SUVADU_BENCH_SIZES")
        .unwrap_or_else(|_| "10000,100000".into())
        .split(',')
        .map(|s| {
            s.trim()
                .parse()
                .expect("SUVADU_BENCH_SIZES must be a comma-separated list")
        })
        .collect();

    for size in sizes {
        let build_start = Instant::now();
        let (dir, repo) = generate_corpus(size);
        let build = build_start.elapsed();
        let corpus = all_entries(&repo);

        println!("\n=== {size} entries (generated in {build:?}) ===");

        // Recording overhead: what a shell hook pays per command.
        let mut inserts = Vec::new();
        for i in 0..200 {
            let stamp = 10_000_000 + i64::from(i);
            let start = Instant::now();
            repo.insert_entry(&Entry {
                id: None,
                session_id: "bench".into(),
                command: format!("echo overhead_{i}"),
                cwd: "/Users/dev".into(),
                exit_code: Some(0),
                started_at: stamp,
                ended_at: stamp + 1,
                duration_ms: 1,
                context: None,
                tag_name: None,
                tag_id: None,
                executor_type: Some("human".into()),
                executor: Some("terminal".into()),
            })
            .unwrap();
            inserts.push(start.elapsed());
        }
        inserts.sort_unstable();
        println!(
            "recording overhead per command: p50 {:?}  p95 {:?}  p99 {:?}",
            percentile(&inserts, 0.50),
            percentile(&inserts, 0.95),
            percentile(&inserts, 0.99)
        );

        println!(
            "\n{:<24} {:>8} {:>8} {:>7} {:>10} {:>10}",
            "case", "eligible", "returned", "found", "p50", "p95"
        );
        for case in judged_cases() {
            let tokens: Vec<String> = case.query.split_whitespace().map(str::to_string).collect();
            let expected = reference_matches(&corpus, &case.query).len();

            let mut timings = Vec::new();
            let mut returned = 0;
            let mut found_planted = false;
            for _ in 0..20 {
                let start = Instant::now();
                let rows = repo
                    .get_entries_filtered(CANDIDATE_LIMIT, 0, &query_filter(&tokens, true))
                    .unwrap();
                timings.push(start.elapsed());
                returned = rows.len();
                found_planted = rows.iter().any(|e| e.command == case.expect);
            }
            timings.sort_unstable();
            println!(
                "{:<24} {:>8} {:>8} {:>7} {:>10?} {:>10?}",
                case.name,
                expected,
                returned,
                if found_planted { "yes" } else { "NO" },
                percentile(&timings, 0.50),
                percentile(&timings, 0.95)
            );
            assert_eq!(returned, expected, "case {}: incomplete results", case.name);
        }

        let bytes = std::fs::metadata(dir.path().join("bench.db"))
            .unwrap()
            .len();
        println!("\ndatabase size: {} MB", bytes / 1_048_576);
    }
}
