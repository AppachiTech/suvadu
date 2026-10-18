// ── Atomic file writes ──────────────────────────────────────

use std::path::{Path, PathBuf};

/// Symlink hops followed before a write gives up (Linux's `MAXSYMLINKS`).
const MAX_SYMLINK_HOPS: usize = 40;

/// The file a write to `path` must replace. A symlink — a config linked in
/// from a dotfiles repository, say — is followed to its final target, even
/// one that doesn't exist yet, so the rename lands on the target and the
/// link itself stays in place. Any other path is returned unchanged.
fn resolve_write_target(path: &Path) -> std::io::Result<PathBuf> {
    let mut current = path.to_path_buf();
    for _ in 0..MAX_SYMLINK_HOPS {
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let link = std::fs::read_link(&current)?;
                current = if link.is_absolute() {
                    link
                } else {
                    parent_dir(&current).join(link)
                };
            }
            _ => return Ok(current),
        }
    }
    Err(std::io::Error::other(format!(
        "too many levels of symbolic links: {}",
        path.display()
    )))
}

/// Directory holding `path` (`.` for a bare file name).
fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    }
}

/// Write `data` to a temp file beside `target`, apply `perms` to it, and
/// rename it over `target`. The temp file lives in the target's directory so
/// the rename is atomic (same filesystem); on error it is auto-cleaned.
fn replace_via_temp(
    target: &Path,
    data: &str,
    perms: Option<std::fs::Permissions>,
) -> std::io::Result<()> {
    let tmp = tempfile::NamedTempFile::new_in(parent_dir(target))?;
    std::fs::write(tmp.path(), data)?;
    if let Some(perms) = perms {
        std::fs::set_permissions(tmp.path(), perms)?;
    }
    tmp.persist(target).map_err(|e| e.error)?;
    Ok(())
}

/// Write `data` to `path` atomically (temp file + rename).
///
/// If `path` is a symlink, its target is replaced and the link is kept. An
/// existing file keeps its permissions; a new file is created owner-only.
pub fn atomic_write(path: &Path, data: &str) -> std::io::Result<()> {
    let target = resolve_write_target(path)?;
    #[cfg(unix)]
    let perms = std::fs::metadata(&target).ok().map(|m| m.permissions());
    #[cfg(not(unix))]
    let perms = None;
    replace_via_temp(&target, data, perms)
}

/// Write `data` to `path` atomically with the given Unix file mode.
/// Permissions are set on the temp file *before* persist, so the final file
/// is never visible with wrong permissions (no TOCTOU window). If `path` is a
/// symlink, its target is replaced (with `mode`) and the link is kept.
/// On non-Unix platforms the `mode` parameter is ignored.
#[allow(unused_variables)]
pub fn atomic_write_with_mode(path: &Path, data: &str, mode: u32) -> std::io::Result<()> {
    let target = resolve_write_target(path)?;
    #[cfg(unix)]
    let perms = {
        use std::os::unix::fs::PermissionsExt;
        Some(std::fs::Permissions::from_mode(mode))
    };
    #[cfg(not(unix))]
    let perms = None;
    replace_via_temp(&target, data, perms)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn mode(path: &std::path::Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    fn is_symlink(path: &std::path::Path) -> bool {
        std::fs::symlink_metadata(path)
            .unwrap()
            .file_type()
            .is_symlink()
    }

    #[test]
    fn atomic_write_through_a_symlink_updates_the_target_and_keeps_the_link() {
        let dir = tempfile::tempdir().unwrap();
        let dotfiles = dir.path().join("dotfiles");
        std::fs::create_dir(&dotfiles).unwrap();
        let real = dotfiles.join("settings.json");
        std::fs::write(&real, "{}").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o644)).unwrap();
        let link = dir.path().join("settings.json");
        symlink(&real, &link).unwrap();

        atomic_write(&link, "{\"a\":1}").unwrap();

        assert!(is_symlink(&link), "the link must not become a regular file");
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "{\"a\":1}");
        assert_eq!(mode(&real), 0o644, "the target keeps its permissions");
        let entries = std::fs::read_dir(&dotfiles).unwrap().count();
        assert_eq!(entries, 1, "no temp file left beside the target");
    }

    #[test]
    fn atomic_write_follows_a_relative_symlink_chain() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("real")).unwrap();
        std::fs::write(dir.path().join("real/config.toml"), "old").unwrap();
        // link -> mid -> real/config.toml, both links relative
        symlink("real/config.toml", dir.path().join("mid")).unwrap();
        symlink("mid", dir.path().join("link")).unwrap();

        atomic_write(&dir.path().join("link"), "new").unwrap();

        assert!(is_symlink(&dir.path().join("link")));
        assert!(is_symlink(&dir.path().join("mid")));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("real/config.toml")).unwrap(),
            "new"
        );
    }

    #[test]
    fn atomic_write_through_a_dangling_symlink_creates_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("not-yet.toml");
        let link = dir.path().join("config.toml");
        symlink(&real, &link).unwrap();

        atomic_write(&link, "x = 1").unwrap();

        assert!(is_symlink(&link));
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "x = 1");
    }

    #[test]
    fn atomic_write_keeps_an_existing_regular_files_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();

        atomic_write(&path, "new").unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(mode(&path), 0o640);
    }

    #[test]
    fn atomic_write_creates_a_new_file_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fresh.txt");
        atomic_write(&path, "hi").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hi");
        assert_eq!(mode(&path), 0o600);
    }

    #[test]
    fn atomic_write_with_mode_through_a_symlink_keeps_the_link() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("hook-real.sh");
        std::fs::write(&real, "old").unwrap();
        let link = dir.path().join("hook.sh");
        symlink(&real, &link).unwrap();

        atomic_write_with_mode(&link, "#!/bin/sh\n", 0o700).unwrap();

        assert!(is_symlink(&link));
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "#!/bin/sh\n");
        assert_eq!(mode(&real), 0o700);
    }

    #[test]
    fn atomic_write_rejects_a_symlink_loop() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        symlink(&b, &a).unwrap();
        symlink(&a, &b).unwrap();
        assert!(atomic_write(&a, "x").is_err());
        assert!(is_symlink(&a) && is_symlink(&b));
    }
}
