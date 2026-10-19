//! "Last time this failed, you fixed it with …": pairing a failed command
//! with the command that fixed it, from nothing but the recorded history.
//!
//! # The pairing rule
//!
//! For a failed run F — a known exit status other than 0 and 130 (Ctrl+C) —
//! look at what ran after it in **the same session**, in the order it was
//! recorded: at most [`LOOKAHEAD`] runs, none starting more than
//! [`WINDOW_MS`] (15 minutes) after F ended. Of those:
//!
//! 1. a run in **another directory** is ignored, and so is one that failed
//!    or whose exit status is unknown (another failed attempt);
//! 2. a successful run **similar** to F is the fix G, and the search ends;
//! 3. a successful run that only looks or moves — `cd`, `ls`, `cat`,
//!    `git status`/`diff`/`log`/`show`, an editor ([`NEUTRAL`]) — is passed
//!    over;
//! 4. a successful **setup step** — the same program as F but not similar
//!    (`npm install` after `npm test`), or a package install
//!    ([`crate::risk::extract_packages`]) — is remembered, the first one
//!    only, and the search goes on;
//! 5. any other success is an unrelated command: the search ends with no
//!    pairing.
//!
//! **Similar** means: the same text; or the same program and the same
//! first non-flag argument (`cargo build` → `cargo build --release`,
//! `git push` → `git push -u origin main`, `ls --colour` → `ls --color`),
//! the program found past wrappers as the risk rules find it
//! ([`crate::risk::program_words`]); or a small edit of the whole line — at
//! most one change in eight characters, at least one, counting a swap of
//! two neighbours as one (`gti status` → `git status`, `python` →
//! `python3`).
//!
//! What G says:
//! - G differs from F: F was fixed by **changing the command** to G.
//! - G is F again, after a setup step S: F was fixed by **S, then rerunning**.
//! - G is F again with no setup step: F **passed when rerun unchanged**.
//!
//! Across history, the newest [`SAMPLE`] failures of the exact same command
//! text are paired this way and the fixes counted. This is an inference
//! from what ran next, never an observed cause.

use std::collections::HashMap;

use crate::db::DbResult;
use crate::repository::{Repository, Run};

/// Longest gap after a failure that a fix may start within.
pub const WINDOW_MS: i64 = 15 * 60 * 1000;

/// Most runs after a failure that are looked at.
pub const LOOKAHEAD: usize = 20;

/// Newest failures of one command that are paired.
pub const SAMPLE: usize = 50;

/// Programs that only look at things, move around or edit a file: running
/// them between a failure and its fix does not end the search. `git` counts
/// only with `status`, `diff`, `log` or `show`.
pub const NEUTRAL: &[&str] = &[
    "cd", "pushd", "popd", "pwd", "ls", "ll", "la", "tree", "clear", "cat", "bat", "less", "more",
    "head", "tail", "echo", "which", "type", "history", "man", "vim", "vi", "nvim", "nano",
    "emacs", "code", "subl", "hx",
];

/// How a failure was fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FixKind {
    /// The command was changed to the fix.
    Changed,
    /// The fix ran, then the same command again, which passed.
    StepThenRerun,
    /// The same command passed when run again, unchanged.
    Rerun,
}

/// One failure's fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pairing {
    /// The command that fixed it: the changed command, the setup step, or
    /// for [`FixKind::Rerun`] the failed command itself.
    pub command: String,
    pub kind: FixKind,
    /// When the run that passed started.
    pub at: i64,
}

/// One fix, counted across a command's failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    pub command: String,
    pub kind: FixKind,
    /// How many of the paired failures it fixed.
    pub count: u32,
    /// When it last did.
    pub last_at: i64,
}

impl Fix {
    /// "you fixed it with: …", as recall and the MCP tools phrase it,
    /// without the count.
    pub fn describe(&self) -> String {
        match self.kind {
            FixKind::Changed => format!("you fixed it with: {}", self.command),
            FixKind::StepThenRerun => {
                format!("you fixed it with: {}, then reran it", self.command)
            }
            FixKind::Rerun => "it passed when you reran it unchanged".to_string(),
        }
    }
}

/// What fixed a command's failures, most often first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fixes {
    /// The failures looked at (at most [`SAMPLE`]).
    pub failures: usize,
    /// Each fix found, by how many failures it fixed, then how recently.
    pub fixes: Vec<Fix>,
}

/// The fix for the failed run `failed`, from `after`: the runs recorded
/// after it in its session, in order. See the module docs for the rule.
pub fn pair(failed: &Run, after: &[Run]) -> Option<Pairing> {
    let failed_words = crate::risk::program_words(&failed.command);
    let deadline = failed.ended_at.max(failed.started_at) + WINDOW_MS;
    let mut step: Option<&Run> = None;
    for run in after.iter().take(LOOKAHEAD) {
        if run.started_at > deadline {
            break;
        }
        if run.cwd != failed.cwd || run.exit_code != Some(0) {
            continue;
        }
        let words = crate::risk::program_words(&run.command);
        if similar(
            &failed.command,
            failed_words.as_deref(),
            &run.command,
            words.as_deref(),
        ) {
            let (command, kind) = if run.command != failed.command {
                (&run.command, FixKind::Changed)
            } else if let Some(step) = step {
                (&step.command, FixKind::StepThenRerun)
            } else {
                (&run.command, FixKind::Rerun)
            };
            return Some(Pairing {
                command: command.clone(),
                kind,
                at: run.started_at,
            });
        }
        if is_neutral(words.as_deref()) {
            continue;
        }
        if is_setup_step(failed_words.as_deref(), words.as_deref(), &run.command) {
            step.get_or_insert(run);
            continue;
        }
        return None;
    }
    None
}

/// What fixed the newest [`SAMPLE`] failures of exactly `command`, outside
/// `exclude_dirs`. One index seek for the failures and one per failure for
/// what followed it.
pub fn fixes_for(repo: &Repository, command: &str, exclude_dirs: &[String]) -> DbResult<Fixes> {
    let failures = repo.failed_runs_of(command, SAMPLE, exclude_dirs)?;
    let mut counted: HashMap<(String, FixKind), (u32, i64)> = HashMap::new();
    for failed in &failures {
        let after = repo.runs_after(&failed.session_id, failed.id, LOOKAHEAD)?;
        if let Some(fix) = pair(failed, &after) {
            let slot = counted.entry((fix.command, fix.kind)).or_insert((0, 0));
            slot.0 += 1;
            slot.1 = slot.1.max(fix.at);
        }
    }
    let mut fixes: Vec<Fix> = counted
        .into_iter()
        .map(|((command, kind), (count, last_at))| Fix {
            command,
            kind,
            count,
            last_at,
        })
        .collect();
    fixes.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then(b.last_at.cmp(&a.last_at))
            .then_with(|| a.command.cmp(&b.command))
    });
    Ok(Fixes {
        failures: failures.len(),
        fixes,
    })
}

/// Whether `run` (words `run_words`) is close enough to the failed `failed`
/// (words `failed_words`) to be its retry.
fn similar(
    failed: &str,
    failed_words: Option<&[String]>,
    run: &str,
    run_words: Option<&[String]>,
) -> bool {
    if failed.trim() == run.trim() {
        return true;
    }
    if let (Some(f), Some(r)) = (failed_words, run_words) {
        if f[0] == r[0] && subcommand(f) == subcommand(r) {
            return true;
        }
    }
    small_edit(failed.trim(), run.trim())
}

/// The first argument that is not a flag.
fn subcommand(words: &[String]) -> Option<&str> {
    words[1..]
        .iter()
        .map(String::as_str)
        .find(|w| !w.starts_with('-'))
}

/// At most one edit per eight characters (at least one) between `a` and
/// `b`: insertions, deletions, substitutions, and swaps of neighbours,
/// each counting one. Lines over 200 characters are never a small edit.
fn small_edit(a: &str, b: &str) -> bool {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len() > 200 || b.len() > 200 {
        return false;
    }
    let budget = (a.len().min(b.len()) / 8).max(1);
    if a.len().abs_diff(b.len()) > budget {
        return false;
    }
    edit_distance(&a, &b) <= budget
}

/// Optimal string alignment distance: Levenshtein plus adjacent swaps.
fn edit_distance(a: &[char], b: &[char]) -> usize {
    let width = b.len() + 1;
    let mut rows = vec![vec![0usize; width]; a.len() + 1];
    for (j, cell) in rows[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        rows[i][0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (rows[i - 1][j] + 1)
                .min(rows[i][j - 1] + 1)
                .min(rows[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(rows[i - 2][j - 2] + 1);
            }
            rows[i][j] = best;
        }
    }
    rows[a.len()][b.len()]
}

/// A run that only looks, moves or edits (see [`NEUTRAL`]).
fn is_neutral(words: Option<&[String]>) -> bool {
    let Some(words) = words else {
        return true;
    };
    let program = words[0].as_str();
    if program == "git" {
        return matches!(subcommand(words), Some("status" | "diff" | "log" | "show"));
    }
    NEUTRAL.contains(&program)
}

/// A run that prepares for the failed command to work: the same program
/// (`npm install` before `npm test`, `git pull` before `git push`), or a
/// package install.
fn is_setup_step(failed_words: Option<&[String]>, words: Option<&[String]>, run: &str) -> bool {
    let same_program = matches!((failed_words, words), (Some(f), Some(r)) if f[0] == r[0]);
    same_program || crate::risk::extract_packages(run).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Entry, Session};
    use crate::test_utils::test_repo;

    const MIN: i64 = 60_000;

    /// Records runs into a test repository, a second apart unless told.
    struct History {
        repo: Repository,
        _dir: tempfile::TempDir,
        at: i64,
    }

    impl History {
        fn new() -> Self {
            let (dir, repo) = test_repo();
            Self {
                repo,
                _dir: dir,
                at: 1_700_000_000_000,
            }
        }

        fn run_in(&mut self, session: &str, cwd: &str, command: &str, exit: i32) -> i64 {
            if self.repo.get_session(session).unwrap().is_none() {
                self.repo
                    .insert_session(&Session {
                        id: session.to_string(),
                        hostname: "host".to_string(),
                        created_at: 0,
                        tag_id: None,
                    })
                    .unwrap();
            }
            self.at += 1000;
            self.repo
                .insert_entry(&Entry::new(
                    session.to_string(),
                    command.to_string(),
                    cwd.to_string(),
                    Some(exit),
                    self.at,
                    self.at + 500,
                ))
                .unwrap()
        }

        fn run(&mut self, command: &str, exit: i32) -> i64 {
            self.run_in("s1", "/project", command, exit)
        }

        fn wait(&mut self, ms: i64) {
            self.at += ms;
        }

        fn fixes(&self, command: &str) -> Fixes {
            fixes_for(&self.repo, command, &[]).unwrap()
        }

        /// The one fix of `command`'s only failure.
        fn only_fix(&self, command: &str) -> Option<(String, FixKind)> {
            let fixes = self.fixes(command);
            assert!(fixes.fixes.len() <= 1, "{fixes:?}");
            fixes.fixes.first().map(|f| (f.command.clone(), f.kind))
        }
    }

    fn changed(command: &str) -> (String, FixKind) {
        (command.to_string(), FixKind::Changed)
    }

    #[test]
    fn a_typo_is_fixed_by_the_corrected_command() {
        let mut h = History::new();
        h.run("gti status", 127);
        h.run("git status", 0);
        assert_eq!(h.only_fix("gti status"), Some(changed("git status")));
    }

    #[test]
    fn a_wrong_flag_is_fixed_by_the_corrected_command() {
        let mut h = History::new();
        h.run("ls --colour=auto", 2);
        h.run("ls --color=auto", 0);
        assert_eq!(
            h.only_fix("ls --colour=auto"),
            Some(changed("ls --color=auto"))
        );

        h.run("cargo build --relase", 1);
        h.run("cargo build --release", 0);
        assert_eq!(
            h.only_fix("cargo build --relase"),
            Some(changed("cargo build --release"))
        );

        h.run("git push", 1);
        h.run("git push -u origin feature", 0);
        assert_eq!(
            h.only_fix("git push"),
            Some(changed("git push -u origin feature"))
        );
    }

    #[test]
    fn an_install_before_the_rerun_is_the_fix() {
        let mut h = History::new();
        h.run("npm test", 1);
        h.run("npm install", 0);
        h.run("npm test", 0);
        assert_eq!(
            h.only_fix("npm test"),
            Some(("npm install".to_string(), FixKind::StepThenRerun))
        );
        // A package manager of another program counts as a setup step too.
        h.run("python app.py", 1);
        h.run("pip install requests", 0);
        h.run("python app.py", 0);
        assert_eq!(
            h.only_fix("python app.py"),
            Some(("pip install requests".to_string(), FixKind::StepThenRerun))
        );
    }

    #[test]
    fn a_plain_rerun_that_passes_is_reported_as_such() {
        let mut h = History::new();
        h.run("cargo build", 101);
        h.run("vim src/main.rs", 0);
        h.run("git diff", 0);
        h.run("cargo build", 0);
        let fixes = h.fixes("cargo build");
        assert_eq!(fixes.fixes.len(), 1);
        assert_eq!(fixes.fixes[0].kind, FixKind::Rerun);
        assert_eq!(
            fixes.fixes[0].describe(),
            "it passed when you reran it unchanged"
        );
    }

    #[test]
    fn another_failed_attempt_does_not_end_the_search() {
        let mut h = History::new();
        h.run("gti status", 127);
        h.run("gti status", 127);
        h.run("git stauts", 1);
        h.run("git status", 0);
        let fixes = h.fixes("gti status");
        assert_eq!(fixes.failures, 2);
        assert_eq!(fixes.fixes.len(), 1);
        assert_eq!(fixes.fixes[0].command, "git status");
        assert_eq!(fixes.fixes[0].count, 2, "both failures were fixed by it");
    }

    #[test]
    fn nothing_pairs_across_sessions() {
        let mut h = History::new();
        h.run_in("s1", "/project", "gti status", 127);
        h.run_in("s2", "/project", "git status", 0);
        assert_eq!(h.only_fix("gti status"), None);
    }

    #[test]
    fn nothing_pairs_across_directories() {
        let mut h = History::new();
        h.run_in("s1", "/project", "make", 2);
        h.run_in("s1", "/other", "make", 0);
        assert_eq!(h.only_fix("make"), None);
        // Back in the directory it failed in, the rerun pairs.
        h.run_in("s1", "/project", "make", 0);
        assert_eq!(
            h.only_fix("make"),
            Some(("make".to_string(), FixKind::Rerun))
        );
    }

    #[test]
    fn an_unrelated_success_ends_the_search() {
        let mut h = History::new();
        h.run("cargo test", 101);
        h.run("docker ps", 0);
        h.run("cargo test", 0);
        assert_eq!(h.only_fix("cargo test"), None);
    }

    #[test]
    fn a_fix_too_long_after_the_failure_is_not_one() {
        let mut h = History::new();
        h.run("gti status", 127);
        h.wait(16 * MIN);
        h.run("git status", 0);
        assert_eq!(h.only_fix("gti status"), None);
    }

    #[test]
    fn a_fix_too_many_commands_later_is_not_one() {
        let mut h = History::new();
        h.run("gti status", 127);
        for _ in 0..LOOKAHEAD {
            h.run("ls", 0);
        }
        h.run("git status", 0);
        assert_eq!(h.only_fix("gti status"), None);
    }

    #[test]
    fn ctrl_c_is_not_a_failure_to_fix() {
        let mut h = History::new();
        h.run("npm run dev", 130);
        h.run("npm run dev", 0);
        assert_eq!(h.fixes("npm run dev"), Fixes::default());
    }

    #[test]
    fn fixes_are_counted_across_history_most_common_first() {
        let mut h = History::new();
        for _ in 0..3 {
            h.run("npm test", 1);
            h.run("npm install", 0);
            h.run("npm test", 0);
        }
        h.run("npm test", 1);
        h.run("npm test", 0);
        // A failure nothing fixed.
        h.run("npm test", 1);
        h.run("terraform plan", 0);
        let fixes = h.fixes("npm test");
        assert_eq!(fixes.failures, 5);
        let found: Vec<(&str, FixKind, u32)> = fixes
            .fixes
            .iter()
            .map(|f| (f.command.as_str(), f.kind, f.count))
            .collect();
        assert_eq!(
            found,
            [
                ("npm install", FixKind::StepThenRerun, 3),
                ("npm test", FixKind::Rerun, 1),
            ]
        );
        assert_eq!(
            fixes.fixes[0].describe(),
            "you fixed it with: npm install, then reran it"
        );
    }

    #[test]
    fn only_the_newest_failures_are_paired() {
        let mut h = History::new();
        for _ in 0..SAMPLE + 5 {
            h.run("make", 2);
            h.run("make", 0);
        }
        let fixes = h.fixes("make");
        assert_eq!(fixes.failures, SAMPLE);
        assert_eq!(fixes.fixes[0].count, u32::try_from(SAMPLE).unwrap());
    }

    #[test]
    fn small_edits_are_counted_with_swaps_as_one() {
        assert!(small_edit("gti status", "git status"));
        assert!(small_edit("python app.py", "python3 app.py"));
        assert!(!small_edit("git pull", "git push"), "two edits in eight");
        assert!(!small_edit("npm test", "npm install"));
        assert!(small_edit(
            "kubectl get pods -n kube-sytem",
            "kubectl get pods -n kube-system"
        ));
        assert!(!small_edit(&"a".repeat(300), &"a".repeat(301)));
    }

    /// What pairing costs on a large history: the query recall's detail
    /// pane and the MCP tools run for one failed command. Prints rather than
    /// asserts. Run with
    /// `cargo test --release --bin suv fix_pairing_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "builds a 100k-entry database; run explicitly with --ignored"]
    fn fix_pairing_cost_on_a_large_history() {
        let (_dir, repo) = test_repo();
        let commands = [
            "git status",
            "cargo build",
            "cargo test",
            "ls",
            "vim src/lib.rs",
            "git commit -m wip",
            "npm test",
            "npm install",
            "docker ps",
            "make",
        ];
        repo.begin_transaction().unwrap();
        let sessions = 200;
        for s in 0..sessions {
            repo.insert_session(&Session {
                id: format!("s{s}"),
                hostname: "host".to_string(),
                created_at: 0,
                tag_id: None,
            })
            .unwrap();
        }
        let mut seed = 0x2545_f491_u64;
        for i in 0..100_000_i64 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let command = commands[usize::try_from(seed % 10).unwrap()];
            let exit = i32::from(seed.is_multiple_of(7));
            let session = format!("s{}", (i / 50) % sessions);
            let at = 1_700_000_000_000 + i * 5_000;
            repo.insert_entry(&Entry::new(
                session,
                command.to_string(),
                "/project".to_string(),
                Some(exit),
                at,
                at + 1_000,
            ))
            .unwrap();
        }
        repo.commit().unwrap();
        for command in ["cargo build", "npm test", "make"] {
            let runs = 20;
            let start = std::time::Instant::now();
            let mut fixes = Fixes::default();
            for _ in 0..runs {
                fixes = fixes_for(&repo, command, &[]).unwrap();
            }
            println!(
                "fix pairing at 100k entries, {command}: {:?} per lookup ({} failures paired, {} fixes)",
                start.elapsed() / runs,
                fixes.failures,
                fixes.fixes.len()
            );
        }
    }
}
