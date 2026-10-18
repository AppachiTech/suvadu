//! The git branch a directory is on, read straight from the repository's
//! files — no `git` process — so recording a command stays cheap.

use std::io::Read;
use std::path::{Path, PathBuf};

/// Longest `HEAD` or `.git` file read; real ones are well under 200 bytes.
const MAX_GIT_FILE_BYTES: u64 = 4096;

/// Longest branch name recorded. Git itself allows longer, but nobody
/// types one; anything past this is more likely a corrupt file.
const MAX_BRANCH_LEN: usize = 255;

/// Hex digits kept from a detached HEAD's commit hash, as `git log --oneline`
/// shows it.
const SHORT_HASH_LEN: usize = 7;

/// The branch checked out for `dir`: the nearest `.git` above it (a
/// directory, or a `gitdir:` file as in linked worktrees and submodules),
/// then its `HEAD`.
///
/// - `ref: refs/heads/<name>` gives `<name>`.
/// - A detached HEAD gives its short commit hash.
/// - Not in a repository, or anything unreadable or unexpected, gives
///   `None` — this never fails, because the caller is recording a command
///   and must not lose it over a branch name.
pub fn git_branch(dir: &Path) -> Option<String> {
    let git_dir = find_git_dir(dir)?;
    let head = read_small(&git_dir.join("HEAD"))?;
    parse_head(&head)
}

/// The git directory for `dir`, found by walking up from it. Stops at the
/// first `.git`, as git does: a broken one is not skipped in favour of an
/// outer repository.
fn find_git_dir(dir: &Path) -> Option<PathBuf> {
    for ancestor in dir.ancestors() {
        let dot_git = ancestor.join(".git");
        let Ok(meta) = std::fs::metadata(&dot_git) else {
            continue;
        };
        if meta.is_dir() {
            return Some(dot_git);
        }
        if meta.is_file() {
            let text = read_small(&dot_git)?;
            let target = text.lines().next()?.strip_prefix("gitdir:")?.trim();
            if target.is_empty() {
                return None;
            }
            // A relative gitdir is relative to the directory holding `.git`.
            return Some(ancestor.join(target));
        }
        return None;
    }
    None
}

/// The text of a small file, or `None` if it is missing, unreadable, too
/// large or not UTF-8.
fn read_small(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_GIT_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_GIT_FILE_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn parse_head(head: &str) -> Option<String> {
    let head = head.trim();
    if let Some(target) = head.strip_prefix("ref:") {
        let name = target.trim().strip_prefix("refs/heads/")?;
        let plausible = !name.is_empty()
            && name.len() <= MAX_BRANCH_LEN
            && !name.chars().any(|c| c.is_control() || c.is_whitespace());
        return plausible.then(|| name.to_string());
    }
    // Detached: a SHA-1 or SHA-256 object name.
    let is_hash =
        (head.len() == 40 || head.len() == 64) && head.bytes().all(|b| b.is_ascii_hexdigit());
    is_hash.then(|| head[..SHORT_HASH_LEN].to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn repo_with_head(head: &str) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        std::fs::write(root.path().join(".git/HEAD"), head).unwrap();
        root
    }

    #[test]
    fn a_branch_is_read_from_head() {
        let repo = repo_with_head("ref: refs/heads/main\n");
        assert_eq!(git_branch(repo.path()).as_deref(), Some("main"));
    }

    #[test]
    fn a_branch_name_keeps_its_slashes() {
        let repo = repo_with_head("ref: refs/heads/feature/login-v2\n");
        assert_eq!(git_branch(repo.path()).as_deref(), Some("feature/login-v2"));
    }

    #[test]
    fn a_subdirectory_finds_the_repository_above_it() {
        let repo = repo_with_head("ref: refs/heads/main\n");
        let nested = repo.path().join("src/deep/er");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(git_branch(&nested).as_deref(), Some("main"));
    }

    #[test]
    fn the_nearest_repository_wins() {
        let outer = repo_with_head("ref: refs/heads/outer\n");
        let inner = outer.path().join("vendor/inner");
        std::fs::create_dir_all(inner.join(".git")).unwrap();
        std::fs::write(inner.join(".git/HEAD"), "ref: refs/heads/inner\n").unwrap();
        assert_eq!(git_branch(&inner.join("src")).as_deref(), Some("inner"));
    }

    #[test]
    fn a_detached_head_gives_the_short_hash() {
        let repo = repo_with_head(&format!("{SHA}\n"));
        assert_eq!(git_branch(repo.path()).as_deref(), Some("0123456"));
        let sha256 = "ABCDEF0123456789".repeat(4);
        let repo = repo_with_head(&sha256);
        assert_eq!(git_branch(repo.path()).as_deref(), Some("abcdef0"));
    }

    #[test]
    fn a_gitdir_file_is_followed_relative_or_absolute() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("main/.git/worktrees/wt");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("HEAD"), "ref: refs/heads/topic\n").unwrap();

        let absolute = root.path().join("wt-abs");
        std::fs::create_dir_all(absolute.join("src")).unwrap();
        std::fs::write(
            absolute.join(".git"),
            format!("gitdir: {}\n", real.display()),
        )
        .unwrap();
        assert_eq!(git_branch(&absolute.join("src")).as_deref(), Some("topic"));

        let relative = root.path().join("wt-rel");
        std::fs::create_dir_all(&relative).unwrap();
        std::fs::write(relative.join(".git"), "gitdir: ../main/.git/worktrees/wt\n").unwrap();
        assert_eq!(git_branch(&relative).as_deref(), Some("topic"));
    }

    #[test]
    fn outside_a_repository_there_is_no_branch() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(git_branch(root.path()), None);
        assert_eq!(git_branch(Path::new("/definitely/not/here")), None);
    }

    #[test]
    fn an_odd_head_gives_no_branch() {
        for head in [
            "",
            "\n",
            "ref:",
            "ref: refs/heads/",
            "ref: refs/tags/v1",
            "ref: refs/heads/has space",
            "ref: refs/heads/bell\u{7}",
            "not a hash",
            "0123456",
            &"g".repeat(40),
            &format!("ref: refs/heads/{}", "x".repeat(MAX_BRANCH_LEN + 1)),
            &"x".repeat(usize::try_from(MAX_GIT_FILE_BYTES).unwrap() + 1),
        ] {
            let repo = repo_with_head(head);
            assert_eq!(git_branch(repo.path()), None, "HEAD {head:?}");
        }
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".git")).unwrap();
        std::fs::write(repo.path().join(".git/HEAD"), [0xff, 0xfe, b'\n']).unwrap();
        assert_eq!(git_branch(repo.path()), None, "non-UTF-8 HEAD");
    }

    #[test]
    fn a_missing_or_unusable_head_gives_no_branch() {
        // .git with no HEAD
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        assert_eq!(git_branch(repo.path()), None);

        // HEAD is a directory
        std::fs::create_dir(repo.path().join(".git/HEAD")).unwrap();
        assert_eq!(git_branch(repo.path()), None);

        // A .git file that is not a gitdir pointer, or points nowhere
        for text in ["", "garbage\n", "gitdir:\n", "gitdir: /no/such/dir\n"] {
            let wt = tempfile::tempdir().unwrap();
            std::fs::write(wt.path().join(".git"), text).unwrap();
            assert_eq!(git_branch(wt.path()), None, ".git file {text:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_head_gives_no_branch() {
        use std::os::unix::fs::PermissionsExt;
        let repo = repo_with_head("ref: refs/heads/main\n");
        let head = repo.path().join(".git/HEAD");
        std::fs::set_permissions(&head, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads anything; the check only means something otherwise.
        if std::fs::read(&head).is_err() {
            assert_eq!(git_branch(repo.path()), None);
        }
        std::fs::set_permissions(&head, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
}
