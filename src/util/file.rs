// ── Atomic file writes ──────────────────────────────────────

/// Write `data` to `path` atomically using `tempfile::NamedTempFile` + persist.
/// The temp file is created in the same directory as `path` to ensure the
/// rename is atomic (same filesystem). On error the temp file is auto-cleaned.
pub fn atomic_write(path: &std::path::Path, data: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let tmp = tempfile::NamedTempFile::new_in(dir)?;
    std::fs::write(tmp.path(), data)?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Write `data` to `path` atomically with the given Unix file mode.
/// Permissions are set on the temp file *before* persist, so the final file
/// is never visible with wrong permissions (no TOCTOU window).
/// On non-Unix platforms the `mode` parameter is ignored.
#[allow(unused_variables)]
pub fn atomic_write_with_mode(
    path: &std::path::Path,
    data: &str,
    mode: u32,
) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let tmp = tempfile::NamedTempFile::new_in(dir)?;
    std::fs::write(tmp.path(), data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(mode))?;
    }
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}
