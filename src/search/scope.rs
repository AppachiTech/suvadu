//! Recall scopes and Git workspace boundary resolution.
//!
//! A **scope** answers "which slice of history am I looking at?". It is
//! independent of the matching mode (`matching::MatchMode`) and of ranking:
//! narrowing the scope never changes how a query is interpreted.
//!
//! | Scope       | Included entries                                    |
//! |-------------|-----------------------------------------------------|
//! | `all`       | everything recorded (the default)                   |
//! | `directory` | commands run in exactly the current directory       |
//! | `workspace` | commands run anywhere inside the Git workspace root |
//! | `session`   | commands from the current shell session             |
//!
//! `workspace` needs a Git workspace and `session` needs a recorded session
//! id. Both can be unavailable, and Suvadu says so rather than silently
//! widening: [`RecallContext::resolve_scope`] returns the scope that will
//! actually be used together with the reason it differs from the request.

use std::path::{Path, PathBuf};

/// Which slice of history recall looks at.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    clap::ValueEnum,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
#[clap(rename_all = "kebab-case")]
pub enum RecallScope {
    /// Everything recorded. The default, and the target of the one-key reset.
    #[default]
    All,
    /// Commands run in exactly the current directory.
    Directory,
    /// Commands run anywhere inside the current Git workspace.
    Workspace,
    /// Commands from the current shell session.
    Session,
}

impl RecallScope {
    /// Stable lower-case name used in `--scope` and config.
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Directory => "directory",
            Self::Workspace => "workspace",
            Self::Session => "session",
        }
    }

    /// Short value for the always-visible status row.
    pub const fn status_value(self) -> &'static str {
        match self {
            Self::All => "All history",
            Self::Directory => "This dir",
            Self::Workspace => "Workspace",
            Self::Session => "Session",
        }
    }

    /// Cycle order for the in-TUI scope key.
    pub const fn next(self) -> Self {
        match self {
            Self::All => Self::Directory,
            Self::Directory => Self::Workspace,
            Self::Workspace => Self::Session,
            Self::Session => Self::All,
        }
    }
}

/// How a Git workspace boundary was recognised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceKind {
    /// `.git` is a directory: an ordinary repository or a main worktree.
    Repository,
    /// `.git` is a `gitdir:` file: a linked worktree. Its own directory is the
    /// boundary, not the main repository it points at — commands run here
    /// belong to this worktree.
    LinkedWorktree,
}

/// A resolved Git workspace boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workspace {
    pub root: PathBuf,
    pub kind: WorkspaceKind,
}

/// Find the Git workspace that contains `start`.
///
/// Walks up from `start` and stops at the **nearest** `.git`, so a nested
/// repository or a submodule is its own workspace rather than part of its
/// parent. A `.git` file (`gitdir: ...`) marks a linked worktree, whose root
/// is the directory holding that file.
///
/// `ceiling`, when given, bounds the walk: directories above it are not
/// inspected. Tests use it so a stray `.git` above the temporary directory
/// cannot change the answer.
pub fn resolve_workspace(start: &Path, ceiling: Option<&Path>) -> Option<Workspace> {
    let start = std::fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
    let ceiling = ceiling.map(|c| std::fs::canonicalize(c).unwrap_or_else(|_| c.to_path_buf()));

    for dir in start.ancestors() {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() {
            return Some(Workspace {
                root: dir.to_path_buf(),
                kind: WorkspaceKind::Repository,
            });
        }
        if dot_git.is_file() {
            let kind = match std::fs::read_to_string(&dot_git) {
                Ok(text) if text.trim_start().starts_with("gitdir:") => {
                    WorkspaceKind::LinkedWorktree
                }
                // A `.git` file we cannot read or understand is still a
                // boundary — treating it as "no workspace" would silently
                // widen the scope, which is exactly what must not happen.
                _ => WorkspaceKind::Repository,
            };
            return Some(Workspace {
                root: dir.to_path_buf(),
                kind,
            });
        }
        if ceiling.as_deref() == Some(dir) {
            break;
        }
    }
    None
}

/// Everything a scope needs to know about "here", resolved once.
///
/// Resolution walks the filesystem, so it happens when recall starts (and on
/// an explicit reload) rather than on every keystroke or repaint. The stored
/// values stay put even if the directory changes underneath — a scope must
/// not silently mean something different halfway through a search.
#[derive(Clone, Debug, Default)]
pub struct RecallContext {
    pub cwd: Option<String>,
    pub workspace: Option<Workspace>,
    pub session_id: Option<String>,
}

impl RecallContext {
    /// Resolve from the process environment. Called once per recall session.
    pub fn resolve() -> Self {
        let cwd = std::env::current_dir().ok();
        let session_id = std::env::var("SUVADU_SESSION_ID")
            .ok()
            .filter(|s| !s.is_empty());
        Self::resolve_at(cwd.as_deref(), session_id, None)
    }

    /// Resolve for an explicit directory and session. The test seam.
    pub fn resolve_at(
        cwd: Option<&Path>,
        session_id: Option<String>,
        ceiling: Option<&Path>,
    ) -> Self {
        let workspace = cwd.and_then(|d| resolve_workspace(d, ceiling));
        Self {
            cwd: cwd.map(|d| {
                std::fs::canonicalize(d)
                    .unwrap_or_else(|_| d.to_path_buf())
                    .to_string_lossy()
                    .into_owned()
            }),
            workspace,
            session_id,
        }
    }

    /// The workspace root as a string, if there is one.
    pub fn workspace_root(&self) -> Option<String> {
        self.workspace
            .as_ref()
            .map(|w| w.root.to_string_lossy().into_owned())
    }

    /// `true` when `scope` can actually be applied here.
    pub const fn is_available(&self, scope: RecallScope) -> bool {
        match scope {
            RecallScope::All => true,
            RecallScope::Directory => self.cwd.is_some(),
            RecallScope::Workspace => self.workspace.is_some(),
            RecallScope::Session => self.session_id.is_some(),
        }
    }

    /// Why `scope` cannot be used, in words a user can act on.
    pub const fn unavailable_reason(&self, scope: RecallScope) -> Option<&'static str> {
        match scope {
            RecallScope::Directory if self.cwd.is_none() => {
                Some("the current directory could not be determined")
            }
            RecallScope::Workspace if self.workspace.is_none() => {
                Some("this directory is not inside a Git repository")
            }
            RecallScope::Session if self.session_id.is_none() => {
                Some("no shell session is recorded (SUVADU_SESSION_ID is unset)")
            }
            // Available scopes, and `All`, have nothing to explain.
            RecallScope::All
            | RecallScope::Directory
            | RecallScope::Workspace
            | RecallScope::Session => None,
        }
    }

    /// The scope that `requested` falls back to when it is unavailable.
    ///
    /// The fallback is always **narrower or equal to** the request where that
    /// is possible (`workspace` → `directory`), and never silently widens
    /// into history the user did not ask for without saying so.
    const fn fallback_for(&self, requested: RecallScope) -> RecallScope {
        match requested {
            RecallScope::Workspace if self.cwd.is_some() => RecallScope::Directory,
            _ => RecallScope::All,
        }
    }

    /// Resolve a requested scope to the one that will be used.
    ///
    /// Returns the effective scope and, when it differs from the request, an
    /// explanation naming both the reason and the fallback.
    pub fn resolve_scope(&self, requested: RecallScope) -> (RecallScope, Option<String>) {
        if self.is_available(requested) {
            return (requested, None);
        }
        let reason = self
            .unavailable_reason(requested)
            .unwrap_or("this scope is unavailable here");
        let fallback = self.fallback_for(requested);
        (
            fallback,
            Some(format!(
                "{} scope unavailable: {reason}. Using {} instead.",
                requested.label(),
                fallback.label()
            )),
        )
    }

    /// The next scope in cycle order that is actually available here.
    ///
    /// Skipping unavailable scopes keeps the key predictable: every press
    /// lands on a scope that shows something, and `All` always terminates the
    /// cycle because it is always available.
    pub fn next_available_scope(&self, from: RecallScope) -> RecallScope {
        let mut candidate = from.next();
        for _ in 0..4 {
            if self.is_available(candidate) {
                return candidate;
            }
            candidate = candidate.next();
        }
        RecallScope::All
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// `root/.git/` plus `root/sub/`.
    fn plain_repo() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("repo");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("sub/deeper")).unwrap();
        (dir, root)
    }

    #[test]
    fn a_subdirectory_resolves_to_the_repository_root() {
        let (dir, root) = plain_repo();
        let ws = resolve_workspace(&root.join("sub/deeper"), Some(dir.path())).unwrap();
        assert_eq!(ws.root, fs::canonicalize(&root).unwrap());
        assert_eq!(ws.kind, WorkspaceKind::Repository);
    }

    #[test]
    fn a_linked_worktree_is_its_own_workspace_not_the_main_repository() {
        let (dir, main) = plain_repo();
        let wt = dir.path().join("wt-feature");
        fs::create_dir_all(wt.join("src")).unwrap();
        fs::write(
            wt.join(".git"),
            format!("gitdir: {}/.git/worktrees/wt-feature\n", main.display()),
        )
        .unwrap();

        let ws = resolve_workspace(&wt.join("src"), Some(dir.path())).unwrap();
        assert_eq!(ws.root, fs::canonicalize(&wt).unwrap());
        assert_eq!(ws.kind, WorkspaceKind::LinkedWorktree);
        // The main repo must not be the answer: commands typed in the
        // worktree belong to the worktree.
        assert_ne!(ws.root, fs::canonicalize(&main).unwrap());
    }

    #[test]
    fn a_nested_repository_wins_over_its_parent() {
        let (dir, outer) = plain_repo();
        let inner = outer.join("vendor/inner");
        fs::create_dir_all(inner.join(".git")).unwrap();
        fs::create_dir_all(inner.join("src")).unwrap();

        let ws = resolve_workspace(&inner.join("src"), Some(dir.path())).unwrap();
        assert_eq!(ws.root, fs::canonicalize(&inner).unwrap());

        // ...and a sibling outside the nested repo still resolves to the outer one.
        let ws = resolve_workspace(&outer.join("sub"), Some(dir.path())).unwrap();
        assert_eq!(ws.root, fs::canonicalize(&outer).unwrap());
    }

    #[test]
    fn outside_a_repository_there_is_no_workspace() {
        let dir = TempDir::new().unwrap();
        let plain = dir.path().join("not-a-repo/deep");
        fs::create_dir_all(&plain).unwrap();
        assert!(resolve_workspace(&plain, Some(dir.path())).is_none());
    }

    #[test]
    fn an_unreadable_dot_git_file_is_still_a_boundary() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("weird");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join(".git"), "something unexpected").unwrap();
        let ws = resolve_workspace(&root.join("sub"), Some(dir.path())).unwrap();
        assert_eq!(ws.root, fs::canonicalize(&root).unwrap());
        assert_eq!(ws.kind, WorkspaceKind::Repository);
    }

    #[test]
    fn the_boundary_is_resolved_once_and_does_not_drift() {
        let (dir, root) = plain_repo();
        let ctx = RecallContext::resolve_at(
            Some(&root.join("sub")),
            Some("sess-1".into()),
            Some(dir.path()),
        );
        let before = ctx.workspace_root();
        assert!(before.is_some());

        // The repository disappears underneath us. The already-resolved
        // context must keep describing the scope the user is looking at.
        fs::remove_dir_all(root.join(".git")).unwrap();
        assert_eq!(ctx.workspace_root(), before);
        assert!(ctx.is_available(RecallScope::Workspace));
    }

    #[test]
    fn outside_a_repository_workspace_scope_explains_and_falls_back() {
        let dir = TempDir::new().unwrap();
        let plain = dir.path().join("plain");
        fs::create_dir_all(&plain).unwrap();
        let ctx = RecallContext::resolve_at(Some(&plain), None, Some(dir.path()));

        assert!(!ctx.is_available(RecallScope::Workspace));
        let (effective, note) = ctx.resolve_scope(RecallScope::Workspace);
        assert_eq!(effective, RecallScope::Directory);
        let note = note.expect("unavailability must be explained, never silent");
        assert!(note.contains("not inside a Git repository"), "{note}");
        assert!(note.contains("directory"), "{note}");
    }

    #[test]
    fn session_scope_without_a_session_explains_and_falls_back_to_all() {
        let dir = TempDir::new().unwrap();
        let ctx = RecallContext::resolve_at(Some(dir.path()), None, Some(dir.path()));
        let (effective, note) = ctx.resolve_scope(RecallScope::Session);
        assert_eq!(effective, RecallScope::All);
        assert!(note.unwrap().contains("SUVADU_SESSION_ID"));
    }

    #[test]
    fn an_available_scope_resolves_to_itself_without_a_note() {
        let (dir, root) = plain_repo();
        let ctx = RecallContext::resolve_at(Some(&root), Some("sess-1".into()), Some(dir.path()));
        for scope in [
            RecallScope::All,
            RecallScope::Directory,
            RecallScope::Workspace,
            RecallScope::Session,
        ] {
            assert_eq!(ctx.resolve_scope(scope), (scope, None), "{scope:?}");
        }
    }

    #[test]
    fn cycling_skips_unavailable_scopes() {
        let dir = TempDir::new().unwrap();
        let plain = dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        // A directory, but no repository and no session.
        let ctx = RecallContext::resolve_at(Some(&plain), None, Some(dir.path()));
        assert_eq!(
            ctx.next_available_scope(RecallScope::All),
            RecallScope::Directory
        );
        // Workspace and Session are both unavailable, so Directory wraps to All.
        assert_eq!(
            ctx.next_available_scope(RecallScope::Directory),
            RecallScope::All
        );
    }

    #[test]
    fn cycling_visits_every_scope_when_all_are_available() {
        let (dir, root) = plain_repo();
        let ctx = RecallContext::resolve_at(Some(&root), Some("sess-1".into()), Some(dir.path()));
        let mut scope = RecallScope::All;
        let mut seen = vec![scope];
        for _ in 0..4 {
            scope = ctx.next_available_scope(scope);
            seen.push(scope);
        }
        assert_eq!(
            seen,
            vec![
                RecallScope::All,
                RecallScope::Directory,
                RecallScope::Workspace,
                RecallScope::Session,
                RecallScope::All,
            ]
        );
    }

    #[test]
    fn all_history_is_the_default_and_always_available() {
        assert_eq!(RecallScope::default(), RecallScope::All);
        assert!(RecallContext::default().is_available(RecallScope::All));
    }

    #[test]
    fn labels_round_trip_through_clap() {
        use clap::ValueEnum;
        for s in RecallScope::value_variants() {
            assert_eq!(RecallScope::from_str(s.label(), true).unwrap(), *s);
        }
    }
}
