//! Git snapshots of an agent session's working directory: the commit and
//! branch checked out, and which paths git reports as changed, taken at the
//! session's first prompt and again at each turn end.
//!
//! A snapshot holds paths only — never file contents and never a diff. Two
//! of them say which paths changed state in the repository between those
//! moments, which is evidence of what happened there, not proof of who did
//! it: the user, another tool or a second agent could have changed a path
//! too.
//!
//! Running `git` is the expensive part, so it happens only at those turn
//! boundaries, never per command, with optional locks off (a snapshot must
//! not take the index lock from under the agent), `core.fsmonitor` off (a
//! repository's own config must not run a program because Suvadu looked at
//! it), and a time limit, after which the snapshot is simply not taken.
//! Outside a repository no `git` process starts at all.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::CapturePolicy;

/// Most changed paths one snapshot stores. More than this and the snapshot
/// is marked capped, with the full count kept.
pub const MAX_PATHS: usize = 500;

/// How long one `git` call may take before it is abandoned.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(2);

/// Most bytes of `git status` output read. A working tree with millions of
/// untracked files must not cost a hook megabytes of memory; past this the
/// snapshot is capped.
const MAX_STATUS_BYTES: usize = 4 * 1024 * 1024;

/// Commit subjects a comparison reads back from the repository.
pub const MAX_COMMIT_SUBJECTS: usize = 20;

/// When in a session a snapshot was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotKind {
    /// At the session's first prompt Suvadu saw. Kept once.
    Start,
    /// At the latest turn end. Replaced at every turn end.
    End,
}

impl SnapshotKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::End => "end",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "start" => Some(Self::Start),
            "end" => Some(Self::End),
            _ => None,
        }
    }
}

/// One path `git status` reported as changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedPath {
    /// Porcelain v2 status: index then worktree, `.` for unchanged (`M.`,
    /// `.M`, `A.`, `R.`, `.D`), `??` for untracked, `UU` and the like for a
    /// conflict.
    pub status: String,
    /// Relative to the repository root.
    pub path: String,
    /// For a rename or copy, the path it came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// A porcelain status in words: `modified`, `added (staged)`,
/// `untracked`, `renamed (staged)`, `conflicted`.
pub fn describe_status(status: &str) -> String {
    if status == "??" {
        return "untracked".into();
    }
    let mut chars = status.chars();
    let (x, y) = (chars.next().unwrap_or('.'), chars.next().unwrap_or('.'));
    if x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D') {
        return "conflicted".into();
    }
    let word = |c: char| match c {
        'M' => "modified",
        'A' => "added",
        'D' => "deleted",
        'R' => "renamed",
        'C' => "copied",
        'T' => "type changed",
        _ => "changed",
    };
    match (x, y) {
        ('.', '.') => "unchanged".into(),
        (x, '.') => format!("{} (staged)", word(x)),
        ('.', y) => word(y).into(),
        (x, y) if word(x) == word(y) => format!("{} (staged and unstaged)", word(x)),
        (x, y) => format!("{} (staged), {} (unstaged)", word(x), word(y)),
    }
}

/// What `git status` said about a working tree, before any privacy rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatus {
    pub repo_root: PathBuf,
    /// The commit checked out; `None` before the first commit.
    pub head: Option<String>,
    /// The branch checked out; `None` when HEAD is detached.
    pub branch: Option<String>,
    pub paths: Vec<ChangedPath>,
    /// Git printed more than [`MAX_STATUS_BYTES`], so `paths` is not all of
    /// them.
    pub truncated: bool,
}

/// One stored snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSnapshot {
    pub session_id: String,
    pub kind: SnapshotKind,
    pub at: i64,
    /// The directory the hook reported.
    pub cwd: String,
    pub repo_root: String,
    pub head: Option<String>,
    pub branch: Option<String>,
    /// At most [`MAX_PATHS`], exclusions applied and redacted.
    pub paths: Vec<ChangedPath>,
    /// Changed paths git reported, before exclusions and the cap; at least
    /// this many when git's own output was cut.
    pub path_count: usize,
    /// Paths left out because they matched an exclusion pattern.
    pub omitted: usize,
    /// Fewer paths are stored than git reported, beyond the excluded ones.
    pub capped: bool,
}

impl GitSnapshot {
    /// The stored form of `status`: each path checked against the policy's
    /// exclusions (a match is left out and counted), redacted when the
    /// policy redacts, and the list cut at [`MAX_PATHS`]. The branch name is
    /// redacted like a path.
    pub fn from_status(
        session_id: &str,
        kind: SnapshotKind,
        at: i64,
        cwd: &str,
        status: GitStatus,
        policy: &CapturePolicy,
    ) -> Self {
        let path_count = status.paths.len();
        let filtered = PathFilter::new(policy).apply(status.paths, MAX_PATHS);
        Self {
            session_id: session_id.to_string(),
            kind,
            at,
            cwd: cwd.to_string(),
            repo_root: status.repo_root.to_string_lossy().into_owned(),
            head: status.head,
            branch: status.branch.map(|branch| redact(&branch, policy)),
            paths: filtered.kept,
            path_count,
            omitted: filtered.omitted,
            capped: filtered.capped || status.truncated,
        }
    }
}

fn redact(text: &str, policy: &CapturePolicy) -> String {
    if policy.redact {
        crate::redact::redact_secrets_with_extra(text, &policy.extra_patterns)
    } else {
        text.to_string()
    }
}

/// Exclusions and redaction for paths, compiled once.
struct PathFilter<'a> {
    policy: &'a CapturePolicy,
    exclusions: Vec<crate::util::CompiledExclusion>,
}

struct Filtered {
    kept: Vec<ChangedPath>,
    omitted: usize,
    capped: bool,
}

impl<'a> PathFilter<'a> {
    fn new(policy: &'a CapturePolicy) -> Self {
        Self {
            policy,
            exclusions: crate::util::compile_exclusions(&policy.exclusions),
        }
    }

    fn excluded(&self, path: &ChangedPath) -> bool {
        crate::util::is_excluded_compiled(&path.path, &self.exclusions)
            || path
                .from
                .as_deref()
                .is_some_and(|from| crate::util::is_excluded_compiled(from, &self.exclusions))
    }

    fn apply(&self, paths: Vec<ChangedPath>, max: usize) -> Filtered {
        let mut out = Filtered {
            kept: Vec::new(),
            omitted: 0,
            capped: false,
        };
        for path in paths {
            if self.excluded(&path) {
                out.omitted += 1;
            } else if out.kept.len() == max {
                out.capped = true;
            } else {
                out.kept.push(ChangedPath {
                    status: path.status,
                    path: redact(&path.path, self.policy),
                    from: path.from.map(|from| redact(&from, self.policy)),
                });
            }
        }
        out
    }
}

// ── Running git ─────────────────────────────────────────────

/// Why a `git` call gave nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitError {
    /// No `git` could be started.
    Missing,
    /// Git exited non-zero: not a repository it will read, an unknown
    /// commit, and so on.
    Failed,
    /// Git did not finish within the time limit and was stopped.
    TimedOut,
}

impl GitError {
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Missing => "git could not be run",
            Self::Failed => "git could not read the commits (they may no longer exist)",
            Self::TimedOut => "git did not answer within 2 s",
        }
    }
}

/// The repository `dir` is in: the nearest ancestor holding a `.git`
/// directory or file, as git finds it. `None` outside one.
pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .find(|ancestor| std::fs::symlink_metadata(ancestor.join(".git")).is_ok())
        .map(Path::to_path_buf)
}

/// Run `git -C dir <args>` and return what it printed, at most `max_bytes`
/// of it (the flag says whether there was more). Read-only and bounded: no
/// optional locks, no fsmonitor, no prompts, no inherited `GIT_DIR`, and
/// stopped after `timeout`.
pub fn run_git(
    dir: &Path,
    args: &[&str],
    timeout: Duration,
    max_bytes: usize,
) -> Result<(Vec<u8>, bool), GitError> {
    let mut child = Command::new("git")
        .arg("--no-optional-locks")
        .args(["-c", "core.fsmonitor=false", "-c", "core.quotePath=false"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_NAMESPACE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| GitError::Missing)?;
    let started = Instant::now();
    let stdout = child.stdout.take().ok_or(GitError::Failed)?;
    let (sender, receiver) = std::sync::mpsc::channel();
    // A reader thread, so a git that prints nothing and hangs still meets
    // the deadline. It stops at `max_bytes`, closing the pipe.
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let limit = u64::try_from(max_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let read = stdout.take(limit).read_to_end(&mut bytes);
        let _ = sender.send(read.map(|_| bytes));
    });
    let Ok(read) = receiver.recv_timeout(timeout) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(GitError::TimedOut);
    };
    let mut bytes = read.map_err(|_| GitError::Failed)?;
    let truncated = bytes.len() > max_bytes;
    if truncated {
        bytes.truncate(max_bytes);
        let _ = child.kill();
        let _ = child.wait();
        return Ok((bytes, true));
    }
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok((bytes, false)),
            Ok(Some(_)) | Err(_) => return Err(GitError::Failed),
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(GitError::TimedOut);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(2)),
        }
    }
}

/// What `git status` says about the repository `dir` is in, or `None`
/// outside a repository (no `git` process is started then), when git is
/// missing, fails, or takes longer than `timeout`.
pub fn observe(dir: &Path, timeout: Duration) -> Option<GitStatus> {
    let root = repo_root(dir)?;
    let (bytes, truncated) = run_git(
        &root,
        &["status", "--porcelain=v2", "--branch", "-z"],
        timeout,
        MAX_STATUS_BYTES,
    )
    .ok()?;
    let mut status = parse_status(&bytes, truncated);
    status.repo_root = root;
    Some(status)
}

/// Parse `git status --porcelain=v2 --branch -z`. When the output was cut
/// (`truncated`), the last record may be incomplete and is dropped.
pub fn parse_status(bytes: &[u8], truncated: bool) -> GitStatus {
    let mut status = GitStatus {
        repo_root: PathBuf::new(),
        head: None,
        branch: None,
        paths: Vec::new(),
        truncated,
    };
    let mut records: Vec<&[u8]> = bytes.split(|b| *b == 0).collect();
    // A complete output ends in NUL, leaving an empty last piece; a cut one
    // ends mid-record.
    records.pop();
    let mut records = records.into_iter();
    while let Some(record) = records.next() {
        let text = String::from_utf8_lossy(record);
        if let Some(header) = text.strip_prefix("# ") {
            if let Some(oid) = header.strip_prefix("branch.oid ") {
                status.head = (oid != "(initial)").then(|| oid.to_string());
            } else if let Some(head) = header.strip_prefix("branch.head ") {
                status.branch = (head != "(detached)").then(|| head.to_string());
            }
            continue;
        }
        let (kind, rest) = text.split_once(' ').unwrap_or((&text, ""));
        let entry = match kind {
            "?" => Some(ChangedPath {
                status: "??".into(),
                path: rest.to_string(),
                from: None,
            }),
            // `1 XY sub mH mI mW hH hI path`
            "1" => fields(rest, 7).map(|(xy, path)| ChangedPath {
                status: xy,
                path,
                from: None,
            }),
            // `2 XY sub mH mI mW hH hI Xscore path` NUL `origPath`
            "2" => {
                let from = records
                    .next()
                    .map(|orig| String::from_utf8_lossy(orig).into_owned());
                fields(rest, 8).map(|(xy, path)| ChangedPath {
                    status: xy,
                    path,
                    from,
                })
            }
            // `u XY sub m1 m2 m3 mW h1 h2 h3 path`
            "u" => fields(rest, 9).map(|(xy, path)| ChangedPath {
                status: xy,
                path,
                from: None,
            }),
            _ => None,
        };
        status.paths.extend(entry);
    }
    status
}

/// The XY status and the path of a record whose path follows `before`
/// space-separated fields, XY the first of them.
fn fields(rest: &str, before: usize) -> Option<(String, String)> {
    let mut parts = rest.splitn(before + 1, ' ');
    let xy = parts.next()?.to_string();
    let path = parts.nth(before - 1)?.to_string();
    (xy.len() == 2 && !path.is_empty()).then_some((xy, path))
}

// ── Comparing two snapshots ─────────────────────────────────

/// One path whose state differs between the start and end snapshots, or
/// that the commits made in between touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    /// For a rename or copy at the end, where it came from.
    pub from: Option<String>,
    /// Status at the start; `None` when it was clean then.
    pub before: Option<String>,
    /// Status at the end; `None` when it was clean then.
    pub after: Option<String>,
    /// The commits made between the two snapshots touched it.
    pub in_commits: bool,
}

impl FileChange {
    /// What happened to the path, in words.
    pub fn describe(&self) -> String {
        match (&self.before, &self.after) {
            (None, Some(after)) => describe_status(after),
            (Some(before), Some(after)) => format!(
                "{} (was {} at the start)",
                describe_status(after),
                describe_status(before)
            ),
            (Some(before), None) if self.in_commits => {
                format!("committed (was {} at the start)", describe_status(before))
            }
            (Some(before), None) => {
                format!(
                    "clean at the end (was {} at the start)",
                    describe_status(before)
                )
            }
            (None, None) => "changed by the commits".into(),
        }
    }
}

/// What the repository can still say about the commits between two HEADs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Commits {
    /// HEAD did not move.
    Unchanged,
    /// HEAD moved, and git was asked now.
    Read {
        /// Commits reachable from the end HEAD and not the start one.
        ahead: usize,
        /// Commits reachable from the start HEAD and not the end one: a
        /// reset, rebase or checkout moved HEAD off them.
        behind: usize,
        /// `<short hash> <subject>` of the newest ahead commits, redacted.
        subjects: Vec<String>,
    },
    /// HEAD moved, but the commits between could not be read.
    Unread(String),
}

/// Two snapshots compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedChanges {
    pub start: GitSnapshot,
    pub end: GitSnapshot,
    pub commits: Commits,
    /// Sorted by path.
    pub files: Vec<FileChange>,
    /// Files the commits touched beyond those listed, after the cap.
    pub commit_files_capped: bool,
}

impl ObservedChanges {
    /// Whether every changed path made it into both snapshots, so `files`
    /// is the whole comparison.
    pub const fn complete(&self) -> bool {
        !self.start.capped && !self.end.capped && !self.commit_files_capped
    }

    /// The commits made in between, when known.
    pub const fn commit_count(&self) -> Option<usize> {
        match &self.commits {
            Commits::Unchanged => Some(0),
            Commits::Read { ahead, .. } => Some(*ahead),
            Commits::Unread(_) => None,
        }
    }
}

/// Why two snapshots cannot be compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotComparable {
    /// Only one of the two was taken.
    Missing(SnapshotKind),
    /// They were taken in different repositories.
    DifferentRepositories,
}

impl NotComparable {
    pub fn describe(&self) -> String {
        match self {
            Self::Missing(SnapshotKind::Start) => "no git snapshot was taken at the session's \
                 first prompt (it started before snapshots were recorded, outside a git \
                 repository, or with record_git_snapshots off)"
                .into(),
            Self::Missing(SnapshotKind::End) => {
                "no git snapshot was taken at a turn end yet".into()
            }
            Self::DifferentRepositories => {
                "the start and end snapshots were taken in different repositories".into()
            }
        }
    }
}

/// Compare a session's two snapshots. When HEAD moved and `read` allows it,
/// the repository is asked now for the commits in between and the paths
/// they touched, with `policy`'s exclusions and redaction applied, since
/// that text was never stored.
pub fn compare(
    start: Option<GitSnapshot>,
    end: Option<GitSnapshot>,
    read: Option<(&CapturePolicy, Duration)>,
) -> Result<ObservedChanges, NotComparable> {
    let start = start.ok_or(NotComparable::Missing(SnapshotKind::Start))?;
    let end = end
        // Taken before the start one: the session began before snapshots
        // were recorded, and no turn has ended since its first one.
        .filter(|end| end.at >= start.at)
        .ok_or(NotComparable::Missing(SnapshotKind::End))?;
    if start.repo_root != end.repo_root {
        return Err(NotComparable::DifferentRepositories);
    }
    let (commits, commit_files, commit_files_capped) = match (&start.head, &end.head) {
        (a, b) if a == b => (Commits::Unchanged, Vec::new(), false),
        (Some(a), Some(b)) => match read {
            Some((policy, timeout)) => {
                read_commits(Path::new(&end.repo_root), a, b, policy, timeout)
            }
            None => (
                Commits::Unread("the repository was not asked".into()),
                Vec::new(),
                false,
            ),
        },
        (None, Some(_)) => match read {
            // The first commit was made during the session.
            Some((policy, timeout)) => read_first_commits(
                Path::new(&end.repo_root),
                end.head.as_deref(),
                policy,
                timeout,
            ),
            None => (
                Commits::Unread("the repository was not asked".into()),
                Vec::new(),
                false,
            ),
        },
        (Some(_), None) => (
            Commits::Unread("HEAD has no commit at the end".into()),
            Vec::new(),
            false,
        ),
        (None, None) => (Commits::Unchanged, Vec::new(), false),
    };
    let files = diff_files(&start.paths, &end.paths, &commit_files);
    Ok(ObservedChanges {
        start,
        end,
        commits,
        files,
        commit_files_capped,
    })
}

/// Paths whose status differs between `start` and `end`, plus those in
/// `committed`.
fn diff_files(start: &[ChangedPath], end: &[ChangedPath], committed: &[String]) -> Vec<FileChange> {
    let mut files: BTreeMap<&str, FileChange> = BTreeMap::new();
    let before: BTreeMap<&str, &ChangedPath> = start
        .iter()
        .map(|path| (path.path.as_str(), path))
        .collect();
    for path in end {
        let was = before.get(path.path.as_str());
        if was.is_some_and(|was| was.status == path.status && was.from == path.from) {
            continue;
        }
        files.insert(
            &path.path,
            FileChange {
                path: path.path.clone(),
                from: path.from.clone(),
                before: was.map(|was| was.status.clone()),
                after: Some(path.status.clone()),
                in_commits: false,
            },
        );
    }
    let after: std::collections::HashSet<&str> = end.iter().map(|p| p.path.as_str()).collect();
    for path in start {
        if !after.contains(path.path.as_str()) {
            files.insert(
                &path.path,
                FileChange {
                    path: path.path.clone(),
                    from: None,
                    before: Some(path.status.clone()),
                    after: None,
                    in_commits: false,
                },
            );
        }
    }
    for path in committed {
        files
            .entry(path)
            .or_insert_with(|| FileChange {
                path: path.clone(),
                from: None,
                before: before.get(path.as_str()).map(|was| was.status.clone()),
                after: None,
                in_commits: false,
            })
            .in_commits = true;
    }
    files.into_values().collect()
}

/// A full or abbreviated object name, and nothing else, so it can never be
/// read by git as an option or a revision expression.
fn is_object_name(text: &str) -> bool {
    (4..=64).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The NUL-separated paths in `bytes`, cut at [`MAX_PATHS`] after the
/// policy's exclusions, redacted.
fn policy_paths(bytes: &[u8], truncated: bool, policy: &CapturePolicy) -> (Vec<String>, bool) {
    let mut pieces: Vec<&[u8]> = bytes.split(|b| *b == 0).collect();
    pieces.pop();
    let paths = pieces
        .into_iter()
        .filter(|piece| !piece.is_empty())
        .map(|piece| ChangedPath {
            status: String::new(),
            path: String::from_utf8_lossy(piece).into_owned(),
            from: None,
        })
        .collect();
    let filtered = PathFilter::new(policy).apply(paths, MAX_PATHS);
    (
        filtered.kept.into_iter().map(|p| p.path).collect(),
        filtered.capped || truncated,
    )
}

fn subjects(bytes: &[u8], policy: &CapturePolicy) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| redact(line, policy))
        .collect()
}

/// The commits between `start` and `end`, and the paths whose content
/// differs between the two.
fn read_commits(
    root: &Path,
    start: &str,
    end: &str,
    policy: &CapturePolicy,
    timeout: Duration,
) -> (Commits, Vec<String>, bool) {
    if !is_object_name(start) || !is_object_name(end) {
        return (
            Commits::Unread("a stored HEAD is not a commit id".into()),
            Vec::new(),
            false,
        );
    }
    if !root.is_dir() {
        return (
            Commits::Unread("the repository is no longer on this machine".into()),
            Vec::new(),
            false,
        );
    }
    let range = format!("{start}...{end}");
    let counts = match run_git(
        root,
        &["rev-list", "--left-right", "--count", &range, "--"],
        timeout,
        4096,
    ) {
        Ok((bytes, _)) => bytes,
        Err(error) => return (Commits::Unread(error.describe().into()), Vec::new(), false),
    };
    let counts = String::from_utf8_lossy(&counts);
    let mut numbers = counts.split_whitespace().map(str::parse::<usize>);
    let (Some(Ok(behind)), Some(Ok(ahead))) = (numbers.next(), numbers.next()) else {
        return (
            Commits::Unread(GitError::Failed.describe().into()),
            Vec::new(),
            false,
        );
    };
    let ahead_range = format!("{start}..{end}");
    let max = format!("--max-count={MAX_COMMIT_SUBJECTS}");
    let subjects = run_git(
        root,
        &[
            "log",
            &max,
            "--no-decorate",
            "--format=%h %s",
            &ahead_range,
            "--",
        ],
        timeout,
        64 * 1024,
    )
    .map(|(bytes, _)| subjects(&bytes, policy))
    .unwrap_or_default();
    let (files, capped) = run_git(
        root,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            start,
            end,
            "--",
        ],
        timeout,
        MAX_STATUS_BYTES,
    )
    .map_or((Vec::new(), false), |(bytes, truncated)| {
        policy_paths(&bytes, truncated, policy)
    });
    (
        Commits::Read {
            ahead,
            behind,
            subjects,
        },
        files,
        capped,
    )
}

/// The commits up to `end` in a repository that had none at the start, and
/// the paths they hold.
fn read_first_commits(
    root: &Path,
    end: Option<&str>,
    policy: &CapturePolicy,
    timeout: Duration,
) -> (Commits, Vec<String>, bool) {
    let Some(end) = end.filter(|end| is_object_name(end)) else {
        return (
            Commits::Unread("a stored HEAD is not a commit id".into()),
            Vec::new(),
            false,
        );
    };
    if !root.is_dir() {
        return (
            Commits::Unread("the repository is no longer on this machine".into()),
            Vec::new(),
            false,
        );
    }
    let ahead = match run_git(root, &["rev-list", "--count", end, "--"], timeout, 4096) {
        Ok((bytes, _)) => String::from_utf8_lossy(&bytes).trim().parse::<usize>().ok(),
        Err(error) => return (Commits::Unread(error.describe().into()), Vec::new(), false),
    };
    let Some(ahead) = ahead else {
        return (
            Commits::Unread(GitError::Failed.describe().into()),
            Vec::new(),
            false,
        );
    };
    let max = format!("--max-count={MAX_COMMIT_SUBJECTS}");
    let subjects = run_git(
        root,
        &["log", &max, "--no-decorate", "--format=%h %s", end, "--"],
        timeout,
        64 * 1024,
    )
    .map(|(bytes, _)| subjects(&bytes, policy))
    .unwrap_or_default();
    let (files, capped) = run_git(
        root,
        &["ls-tree", "-r", "--name-only", "-z", end],
        timeout,
        MAX_STATUS_BYTES,
    )
    .map_or((Vec::new(), false), |(bytes, truncated)| {
        policy_paths(&bytes, truncated, policy)
    });
    (
        Commits::Read {
            ahead,
            behind: 0,
            subjects,
        },
        files,
        capped,
    )
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A git command for a test repository, isolated from the developer's
    /// own git configuration so signing, hooks or templates cannot change
    /// what a test sees.
    pub fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args([
                "-c",
                "user.name=Suvadu Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// A repository with one commit holding `a.txt` and `b.txt`.
    pub fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "b\n").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "first"]);
        dir
    }

    /// Observed changes as a comparison reports them, without a repository:
    /// one commit, a modified path and an untracked one.
    pub fn sample_changes() -> ObservedChanges {
        let shot = |kind, at, head: &str| GitSnapshot {
            session_id: "codex-fixture".into(),
            kind,
            at,
            cwd: "/work/project".into(),
            repo_root: "/work/project".into(),
            head: Some(head.into()),
            branch: Some("main".into()),
            paths: Vec::new(),
            path_count: 0,
            omitted: 0,
            capped: false,
        };
        ObservedChanges {
            start: shot(SnapshotKind::Start, 1_000, "aaaaaaaaaaaaaaaa"),
            end: shot(SnapshotKind::End, 9_000, "bbbbbbbbbbbbbbbb"),
            commits: Commits::Read {
                ahead: 1,
                behind: 0,
                subjects: vec!["bbbbbbb fix the parser".into()],
            },
            files: vec![
                FileChange {
                    path: "src/parser.rs".into(),
                    from: None,
                    before: None,
                    after: None,
                    in_commits: true,
                },
                FileChange {
                    path: "notes.md".into(),
                    from: None,
                    before: None,
                    after: Some("??".into()),
                    in_commits: false,
                },
            ],
            commit_files_capped: false,
        }
    }

    fn policy() -> CapturePolicy {
        CapturePolicy::default()
    }

    fn snapshot(kind: SnapshotKind, dir: &Path, policy: &CapturePolicy) -> GitSnapshot {
        let status = observe(dir, GIT_TIMEOUT).expect("a repository");
        GitSnapshot::from_status("claude-s1", kind, 1, &dir.to_string_lossy(), status, policy)
    }

    fn path(status: &str, path: &str) -> ChangedPath {
        ChangedPath {
            status: status.into(),
            path: path.into(),
            from: None,
        }
    }

    #[test]
    fn porcelain_v2_records_parse_into_statuses_and_paths() {
        let oid = "0123456789abcdef0123456789abcdef01234567";
        let output = format!(
            "# branch.oid {oid}\0# branch.head feature/x\0# branch.ab +1 -0\0\
             1 .M N... 100644 100644 100644 {oid} {oid} src/with space.rs\0\
             2 R. N... 100644 100644 100644 {oid} {oid} R100 new name.rs\0old name.rs\0\
             u UU N... 100644 100644 100644 100644 {oid} {oid} {oid} conflict.rs\0\
             ? untracked dir/\0"
        );
        let status = parse_status(output.as_bytes(), false);
        assert_eq!(status.head.as_deref(), Some(oid));
        assert_eq!(status.branch.as_deref(), Some("feature/x"));
        assert!(!status.truncated);
        assert_eq!(
            status.paths,
            [
                path(".M", "src/with space.rs"),
                ChangedPath {
                    status: "R.".into(),
                    path: "new name.rs".into(),
                    from: Some("old name.rs".into()),
                },
                path("UU", "conflict.rs"),
                path("??", "untracked dir/"),
            ]
        );
    }

    #[test]
    fn an_unborn_or_detached_head_has_no_commit_or_no_branch() {
        let status = parse_status(b"# branch.oid (initial)\0# branch.head main\0", false);
        assert_eq!(
            (status.head, status.branch.as_deref()),
            (None, Some("main"))
        );
        let status = parse_status(b"# branch.oid abcd1234\0# branch.head (detached)\0", false);
        assert_eq!(
            (status.head.as_deref(), status.branch),
            (Some("abcd1234"), None)
        );
    }

    #[test]
    fn a_cut_output_drops_its_partial_last_record() {
        let status = parse_status(b"? one\0? tw", true);
        assert_eq!(status.paths, [path("??", "one")]);
        assert!(status.truncated);
    }

    #[test]
    fn statuses_read_as_words() {
        assert_eq!(describe_status("??"), "untracked");
        assert_eq!(describe_status(".M"), "modified");
        assert_eq!(describe_status("M."), "modified (staged)");
        assert_eq!(describe_status("MM"), "modified (staged and unstaged)");
        assert_eq!(describe_status("AM"), "added (staged), modified (unstaged)");
        assert_eq!(describe_status(".D"), "deleted");
        assert_eq!(describe_status("R."), "renamed (staged)");
        assert_eq!(describe_status("UU"), "conflicted");
        assert_eq!(describe_status("AA"), "conflicted");
    }

    #[test]
    fn a_snapshot_records_head_branch_and_every_kind_of_change() {
        let repo = repo();
        let dir = repo.path();
        std::fs::write(dir.join("a.txt"), "changed\n").unwrap();
        std::fs::remove_file(dir.join("b.txt")).unwrap();
        std::fs::write(dir.join("new.txt"), "new\n").unwrap();
        git(dir, &["mv", "a.txt", "renamed.txt"]);
        std::fs::create_dir(dir.join("sub")).unwrap();
        let shot = snapshot(SnapshotKind::Start, &dir.join("sub"), &policy());
        assert_eq!(shot.branch.as_deref(), Some("main"));
        assert_eq!(shot.head.as_ref().map(String::len), Some(40));
        assert_eq!(
            Path::new(&shot.repo_root).canonicalize().unwrap(),
            dir.canonicalize().unwrap()
        );
        let by_path: BTreeMap<&str, &ChangedPath> =
            shot.paths.iter().map(|p| (p.path.as_str(), p)).collect();
        assert_eq!(describe_status(&by_path["b.txt"].status), "deleted");
        assert_eq!(describe_status(&by_path["new.txt"].status), "untracked");
        let renamed = by_path["renamed.txt"];
        assert_eq!(renamed.from.as_deref(), Some("a.txt"));
        assert!(
            describe_status(&renamed.status).starts_with("renamed (staged)"),
            "{renamed:?}"
        );
        assert_eq!(shot.path_count, 3);
        assert!(!shot.capped);
    }

    #[test]
    fn outside_a_repository_no_snapshot_is_taken() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(repo_root(dir.path()), None);
        assert_eq!(observe(dir.path(), GIT_TIMEOUT), None);
        assert_eq!(
            observe(Path::new("/definitely/not/here"), GIT_TIMEOUT),
            None
        );
    }

    #[test]
    fn a_broken_repository_gives_no_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".git"), "gitdir: /no/such/place\n").unwrap();
        assert_eq!(observe(dir.path(), GIT_TIMEOUT), None);
    }

    #[test]
    fn a_huge_change_list_is_capped_with_its_full_count_kept() {
        let repo = repo();
        for i in 0..(MAX_PATHS + 20) {
            std::fs::write(repo.path().join(format!("f{i:04}.txt")), "x").unwrap();
        }
        let shot = snapshot(SnapshotKind::End, repo.path(), &policy());
        assert_eq!(shot.paths.len(), MAX_PATHS);
        assert_eq!(shot.path_count, MAX_PATHS + 20);
        assert!(shot.capped);
    }

    #[test]
    fn excluded_paths_are_left_out_and_counted_and_the_rest_redacted() {
        let token = "ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789";
        let status = GitStatus {
            repo_root: PathBuf::from("/repo"),
            head: None,
            branch: Some(format!("leak-{token}")),
            paths: vec![
                path("??", "secrets/prod.env"),
                ChangedPath {
                    status: "R.".into(),
                    path: "public.txt".into(),
                    from: Some("secrets/old.txt".into()),
                },
                path(".M", &format!("notes-{token}.md")),
                path(".M", "src/main.rs"),
            ],
            truncated: false,
        };
        let policy = CapturePolicy {
            exclusions: vec!["^secrets/".into()],
            ..CapturePolicy::default()
        };
        let shot = GitSnapshot::from_status("s", SnapshotKind::Start, 1, "/repo", status, &policy);
        assert_eq!(
            shot.omitted, 2,
            "an excluded rename source excludes the rename"
        );
        assert_eq!(shot.path_count, 4);
        assert_eq!(shot.paths.len(), 2);
        let text = format!("{shot:?}");
        assert!(!text.contains(token), "{text}");
        assert!(!text.contains("secrets/"), "{text}");
        assert!(shot.paths.iter().any(|p| p.path == "src/main.rs"));
    }

    #[test]
    fn a_hung_git_is_stopped_at_the_time_limit() {
        let repo = repo();
        // `git -c alias.x='!sleep 5' x` stands in for a status that hangs.
        let started = Instant::now();
        let result = run_git(
            repo.path(),
            &["-c", "alias.hang=!sleep 5", "hang"],
            Duration::from_millis(200),
            1024,
        );
        assert_eq!(result, Err(GitError::TimedOut));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_commit_made_between_snapshots_is_read_back_with_its_files() {
        let repo = repo();
        let dir = repo.path();
        std::fs::write(dir.join("dirty-before.txt"), "x").unwrap();
        let start = snapshot(SnapshotKind::Start, dir, &policy());
        std::fs::write(dir.join("a.txt"), "edited\n").unwrap();
        git(dir, &["commit", "-q", "-am", "edit a"]);
        std::fs::write(dir.join("b.txt"), "unstaged\n").unwrap();
        std::fs::remove_file(dir.join("dirty-before.txt")).unwrap();
        let end = snapshot(SnapshotKind::End, dir, &policy());

        let changes = compare(Some(start), Some(end), Some((&policy(), GIT_TIMEOUT))).unwrap();
        let Commits::Read {
            ahead,
            behind,
            subjects,
        } = &changes.commits
        else {
            panic!("{:?}", changes.commits)
        };
        assert_eq!((*ahead, *behind), (1, 0));
        assert_eq!(subjects.len(), 1);
        assert!(subjects[0].ends_with(" edit a"), "{subjects:?}");
        assert_eq!(changes.commit_count(), Some(1));
        let files: Vec<(&str, String)> = changes
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.describe()))
            .collect();
        assert_eq!(
            files,
            [
                ("a.txt", "changed by the commits".to_string()),
                ("b.txt", "modified".to_string()),
                (
                    "dirty-before.txt",
                    "clean at the end (was untracked at the start)".to_string()
                ),
            ]
        );
        assert!(changes.complete());
    }

    #[test]
    fn unchanged_paths_and_heads_report_nothing() {
        let repo = repo();
        std::fs::write(repo.path().join("a.txt"), "dirty\n").unwrap();
        let start = snapshot(SnapshotKind::Start, repo.path(), &policy());
        let end = snapshot(SnapshotKind::End, repo.path(), &policy());
        let changes = compare(Some(start), Some(end), Some((&policy(), GIT_TIMEOUT))).unwrap();
        assert_eq!(changes.commits, Commits::Unchanged);
        assert!(changes.files.is_empty(), "{:?}", changes.files);
    }

    #[test]
    fn a_vanished_repository_or_unknown_commit_leaves_the_commits_unread() {
        let repo = repo();
        let start = snapshot(SnapshotKind::Start, repo.path(), &policy());
        let mut end = start.clone();
        end.kind = SnapshotKind::End;
        end.head = Some("ffffffffffffffffffffffffffffffffffffffff".into());
        let changes = compare(
            Some(start.clone()),
            Some(end.clone()),
            Some((&policy(), GIT_TIMEOUT)),
        )
        .unwrap();
        assert!(
            matches!(changes.commits, Commits::Unread(_)),
            "{:?}",
            changes.commits
        );
        assert_eq!(changes.commit_count(), None);

        let mut gone_start = start;
        let mut gone_end = end;
        gone_start.repo_root = "/no/such/repo".into();
        gone_end.repo_root = "/no/such/repo".into();
        let changes = compare(
            Some(gone_start),
            Some(gone_end),
            Some((&policy(), GIT_TIMEOUT)),
        )
        .unwrap();
        assert_eq!(
            changes.commits,
            Commits::Unread("the repository is no longer on this machine".into())
        );
    }

    #[test]
    fn a_stored_head_that_is_not_a_commit_id_is_never_passed_to_git() {
        let repo = repo();
        let start = snapshot(SnapshotKind::Start, repo.path(), &policy());
        let mut end = start.clone();
        end.head = Some("--output=/tmp/x".into());
        let changes = compare(Some(start), Some(end), Some((&policy(), GIT_TIMEOUT))).unwrap();
        assert_eq!(
            changes.commits,
            Commits::Unread("a stored HEAD is not a commit id".into())
        );
    }

    #[test]
    fn snapshots_that_cannot_be_compared_say_why() {
        let repo = repo();
        let start = snapshot(SnapshotKind::Start, repo.path(), &policy());
        assert_eq!(
            compare(Some(start.clone()), None, None),
            Err(NotComparable::Missing(SnapshotKind::End))
        );
        assert_eq!(
            compare(None, Some(start.clone()), None),
            Err(NotComparable::Missing(SnapshotKind::Start))
        );
        let mut elsewhere = start.clone();
        elsewhere.repo_root = "/other".into();
        assert_eq!(
            compare(Some(start), Some(elsewhere), None),
            Err(NotComparable::DifferentRepositories)
        );
    }

    #[test]
    fn an_end_snapshot_older_than_the_start_one_is_no_end_yet() {
        let repo = repo();
        let mut start = snapshot(SnapshotKind::Start, repo.path(), &policy());
        start.at = 10;
        let mut end = start.clone();
        end.kind = SnapshotKind::End;
        end.at = 5;
        assert_eq!(
            compare(Some(start), Some(end), None),
            Err(NotComparable::Missing(SnapshotKind::End))
        );
    }

    #[test]
    fn the_first_commit_of_a_new_repository_is_read_back() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        std::fs::write(dir.path().join("x.txt"), "x").unwrap();
        let start = snapshot(SnapshotKind::Start, dir.path(), &policy());
        assert_eq!(start.head, None);
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "initial"]);
        let end = snapshot(SnapshotKind::End, dir.path(), &policy());
        let changes = compare(Some(start), Some(end), Some((&policy(), GIT_TIMEOUT))).unwrap();
        assert_eq!(changes.commit_count(), Some(1));
        assert_eq!(changes.files.len(), 1);
        assert_eq!(
            changes.files[0].describe(),
            "committed (was untracked at the start)"
        );
    }
}
