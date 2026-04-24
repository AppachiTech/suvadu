use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};

use crate::models::{Entry, Session};
use crate::repository::Repository;
use crate::util;

/// Escape a string for CSV: double internal quotes and prefix with `'` if the
/// field starts with a formula-triggering character (`=`, `+`, `-`, `@`, tab, CR).
/// This prevents formula injection in Excel / Google Sheets.
fn csv_safe(s: &str) -> String {
    let escaped = s.replace('"', "\"\"");
    if escaped.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{escaped}")
    } else {
        escaped
    }
}

pub fn handle_export(
    format: &str,
    after: Option<&str>,
    before: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::init()?;

    let after_ms = after.and_then(|d| util::parse_date_input(d, false));
    let before_ms = before.and_then(|d| util::parse_date_input(d, true));

    match format {
        "json" => {
            // Stream JSON array: print `[`, then comma-separated entries, then `]`.
            // Empty results still emit `[]` so the output is always valid JSON.
            let mut count = 0usize;
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            repo.stream_export_entries(after_ms, before_ms, |entry| {
                if count == 0 {
                    writeln!(out, "[")?;
                } else {
                    writeln!(out, ",")?;
                }
                write!(out, "  {}", serde_json::to_string(&entry)?)?;
                count += 1;
                Ok(())
            })?;
            if count == 0 {
                writeln!(out, "[]")?;
                eprintln!("No entries to export.");
            } else {
                writeln!(out, "\n]")?;
                eprintln!("Exported {count} entries.");
            }
        }
        "jsonl" => {
            let mut count = 0usize;
            repo.stream_export_entries(after_ms, before_ms, |entry| {
                println!("{}", serde_json::to_string(&entry)?);
                count += 1;
                Ok(())
            })?;
            if count == 0 {
                eprintln!("No entries to export.");
            } else {
                eprintln!("Exported {count} entries.");
            }
        }
        "csv" => {
            println!("command,cwd,exit_code,started_at,ended_at,duration_ms,session_id,executor_type,executor");
            let mut count = 0usize;
            repo.stream_export_entries(after_ms, before_ms, |entry| {
                let cmd = csv_safe(&entry.command);
                let cwd = csv_safe(&entry.cwd);
                let sid = csv_safe(&entry.session_id);
                let etype = csv_safe(entry.executor_type.as_deref().unwrap_or(""));
                let exec = csv_safe(entry.executor.as_deref().unwrap_or(""));
                println!(
                    "\"{cmd}\",\"{cwd}\",{},{},{},{},\"{sid}\",\"{etype}\",\"{exec}\"",
                    entry.exit_code.map_or(String::new(), |c| c.to_string()),
                    entry.started_at,
                    entry.ended_at,
                    entry.duration_ms,
                );
                count += 1;
                Ok(())
            })?;
            if count == 0 {
                eprintln!("No entries to export.");
            } else {
                eprintln!("Exported {count} entries.");
            }
        }
        _ => {
            return Err(format!("Unknown format: {format}. Use 'json', 'jsonl', or 'csv'.").into());
        }
    }

    Ok(())
}

/// Result of a JSONL import. Returned by `import_jsonl_into_repo` so callers
/// (and tests) can inspect what happened without parsing stdout.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ImportStats {
    pub imported: u64,
    pub parse_errors: u64,
    pub placeholder_sessions: u64,
    /// Tags that didn't exist on the destination and were created during import.
    pub created_tags: u64,
    /// Entries whose source `tag_id` could not be remapped (no matching name on
    /// destination and tag creation failed, or the export carried a `tag_id`
    /// without a `tag_name`). The entry is still imported but with `tag_id = NULL`.
    pub dropped_tag_associations: u64,
}

/// Inspect the first non-empty line of `file` and reject formats that clearly
/// aren't JSONL (e.g. CSV exports the user accidentally piped to `suv import`).
/// Returns `Ok(())` on a JSONL-shaped file or an empty file.
fn check_jsonl_shape(file: &str) -> Result<(), Box<dyn std::error::Error>> {
    let f = std::fs::File::open(file)?;
    let reader = std::io::BufReader::new(f);
    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !trimmed.starts_with('{') {
            return Err(format!(
                "{file} does not look like JSONL — its first non-empty line is not a JSON object.\n\
                 `suv import` accepts JSONL only (use `--from zsh-history` for zsh history files).\n\
                 If you exported as CSV, re-export with: suv export > history.jsonl"
            )
            .into());
        }
        return Ok(());
    }
    Ok(())
}

pub fn handle_import(file: &str, dry_run: bool) -> Result<(), Box<dyn std::error::Error>> {
    check_jsonl_shape(file)?;

    let f = std::fs::File::open(file)?;
    let reader = std::io::BufReader::new(f);

    if dry_run {
        let mut count = 0u64;
        let mut skipped = 0u64;
        for (line_num, line) in reader.lines().enumerate() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let entry: Entry = match serde_json::from_str(trimmed) {
                Ok(e) => e,
                Err(err) => {
                    eprintln!("Line {}: parse error: {err}", line_num + 1);
                    skipped += 1;
                    continue;
                }
            };
            println!("[dry-run] Would import: {} ({})", entry.command, entry.cwd);
            count += 1;
        }
        println!(
            "Dry run complete. {count} entries would be imported ({skipped} skipped due to errors)."
        );
        return Ok(());
    }

    let repo = Repository::init()?;
    let stats = import_jsonl_into_repo(&repo, reader)?;

    println!(
        "Imported {} entries ({} skipped).",
        stats.imported, stats.parse_errors
    );
    if stats.placeholder_sessions > 0 {
        println!(
            "  Created {} placeholder session(s) for entries from other machines.",
            stats.placeholder_sessions
        );
    }
    if stats.created_tags > 0 {
        println!(
            "  Created {} tag(s) carried over from the source machine.",
            stats.created_tags
        );
    }
    if stats.dropped_tag_associations > 0 {
        println!(
            "  {} entry(ies) imported with tag_id cleared (no name in export, or tag limit reached).",
            stats.dropped_tag_associations
        );
    }
    Ok(())
}

/// Stream JSONL entries from `reader` into `repo`.
///
/// Two integrity fixups are applied per entry to keep the import alive when
/// the destination DB doesn't share state with the source:
///
/// * If the entry's `session_id` doesn't exist locally, an `imported`-host
///   placeholder session is created (satisfies `entries.session_id` FK).
/// * If the entry's `tag_id` doesn't exist locally, it's remapped by
///   `tag_name` — looked up on the destination, created if missing, or
///   cleared to NULL if neither path works (satisfies `entries.tag_id` FK).
///
/// Wraps the work in a transaction with periodic re-commits to bound WAL growth.
/// A parse error skips the line; an insert error rolls back the current batch
/// and propagates.
pub fn import_jsonl_into_repo<R: BufRead>(
    repo: &Repository,
    reader: R,
) -> Result<ImportStats, Box<dyn std::error::Error>> {
    const BATCH_SIZE: u64 = 10_000;
    const PLACEHOLDER_HOSTNAME: &str = "imported";

    let mut stats = ImportStats::default();
    let mut batch_count = 0u64;
    let mut ensured_sessions: HashSet<String> = HashSet::new();
    // Lower-cased tag name → resolved local tag_id (or None if creation failed
    // and we had to drop the association).
    let mut tag_remap: HashMap<String, Option<i64>> = HashMap::new();

    let tx = repo.transaction()?;

    for (line_num, line) in reader.lines().enumerate() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut entry: Entry = match serde_json::from_str(trimmed) {
            Ok(e) => e,
            Err(err) => {
                eprintln!("Line {}: parse error: {err}", line_num + 1);
                stats.parse_errors += 1;
                continue;
            }
        };

        if !ensured_sessions.contains(&entry.session_id) {
            let created = repo.insert_session_if_missing(
                &entry.session_id,
                PLACEHOLDER_HOSTNAME,
                entry.started_at,
            )?;
            if created {
                stats.placeholder_sessions += 1;
            }
            ensured_sessions.insert(entry.session_id.clone());
        }

        if entry.tag_id.is_some() {
            let resolved = remap_tag_id(repo, &entry, &mut tag_remap, &mut stats)?;
            entry.tag_id = resolved;
        }

        match repo.insert_entry(&entry) {
            Ok(_) => {
                stats.imported += 1;
                batch_count += 1;
            }
            Err(e) => {
                eprintln!("Insert failed at line {}: {e}", line_num + 1);
                eprintln!("Rolling back — no entries from this batch were written.");
                return Err(e.into());
            }
        }

        if batch_count >= BATCH_SIZE {
            tx.recommit()?;
            batch_count = 0;
        }
    }

    tx.commit()?;
    Ok(stats)
}

/// Resolve the destination-local `tag_id` for an entry whose source `tag_id`
/// may not exist on this machine. Strategy: look up by `tag_name`, create the
/// tag if missing, fall back to `None` if there's no name to remap by or tag
/// creation fails (e.g. the 20-tag cap). Caches results per name to avoid
/// re-querying for every entry.
fn remap_tag_id(
    repo: &Repository,
    entry: &Entry,
    cache: &mut HashMap<String, Option<i64>>,
    stats: &mut ImportStats,
) -> Result<Option<i64>, Box<dyn std::error::Error>> {
    let Some(name) = entry.tag_name.as_deref() else {
        // tag_id without a name — defensively drop the association rather
        // than risk hitting an unrelated tag id on the destination.
        stats.dropped_tag_associations += 1;
        return Ok(None);
    };

    let key = name.to_lowercase();
    if let Some(cached) = cache.get(&key) {
        if cached.is_none() {
            stats.dropped_tag_associations += 1;
        }
        return Ok(*cached);
    }

    let resolved = if let Some(id) = repo.get_tag_id_by_name(name)? {
        Some(id)
    } else if let Ok(id) = repo.create_tag(name, None) {
        stats.created_tags += 1;
        Some(id)
    } else {
        // Tag cap reached, validation failed, etc. — drop the
        // association so the entry still imports.
        stats.dropped_tag_associations += 1;
        None
    };
    cache.insert(key, resolved);
    Ok(resolved)
}

/// Parse a single extended-history line: `: timestamp:duration;command`
/// Returns (`timestamp_seconds`, `duration_seconds`, command)
pub fn parse_extended_history_line(line: &str) -> Option<(i64, i64, String)> {
    let rest = line.strip_prefix(": ")?;
    let colon_pos = rest.find(':')?;
    let ts: i64 = rest[..colon_pos].parse().ok()?;
    let after_ts = &rest[colon_pos + 1..];
    let semi_pos = after_ts.find(';')?;
    let dur: i64 = after_ts[..semi_pos].parse().ok()?;
    let cmd = after_ts[semi_pos + 1..].to_string();
    Some((ts, dur, cmd))
}

pub fn handle_import_zsh_history(
    file: &str,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Read file with lossy UTF-8 conversion (zsh_history may contain binary data)
    let raw = std::fs::read(file)?;
    let text = String::from_utf8_lossy(&raw);
    if matches!(text, std::borrow::Cow::Owned(_)) {
        eprintln!(
            "Warning: {file} contains invalid UTF-8 bytes; those bytes were replaced with \u{FFFD}"
        );
    }

    let parsed = parse_zsh_history(&text);

    println!("Parsed {} commands from {file}", parsed.len());

    if dry_run {
        print_zsh_import_preview(&parsed);
        return Ok(());
    }

    // Phase 2: Open DB and deduplicate
    let repo = Repository::init()?;

    println!("Checking for duplicates against existing entries...");

    // Create a dedicated import session
    let session_id = format!("import-zsh-{}", uuid::Uuid::new_v4());
    let hostname = hostname::get()?.to_string_lossy().to_string();
    let now = chrono::Utc::now().timestamp_millis();

    let session = Session {
        id: session_id.clone(),
        hostname,
        created_at: now,
        tag_id: None,
    };
    repo.insert_session(&session)?;

    // Phase 3: Insert in a transaction for performance + atomicity.
    // TransactionGuard auto-rolls back on drop if commit() is not called.
    let tx = repo.transaction()?;

    let (imported, skipped) = import_entries_batch(&repo, &parsed, &session_id, now)?;
    tx.commit()?;
    println!("\n✓ Import complete:");
    println!("  Imported: {imported}");
    println!("  Skipped:  {skipped} (duplicates/empty)");
    println!("  Session:  {session_id}");

    Ok(())
}

/// Parse zsh history text into a list of (command, `started_at_ms`, `duration_ms`) tuples.
fn parse_zsh_history(text: &str) -> Vec<(String, i64, i64)> {
    let mut parsed: Vec<(String, i64, i64)> = Vec::new();
    let mut current_cmd = String::new();
    let mut current_ts: i64 = 0;
    let mut current_dur: i64 = 0;
    let mut in_multiline = false;

    for line in text.lines() {
        if in_multiline {
            // Continuation of previous command
            current_cmd.push('\n');
            if let Some(stripped) = line.strip_suffix('\\') {
                current_cmd.push_str(stripped);
            } else {
                current_cmd.push_str(line);
                let trimmed = current_cmd.trim_end().to_string();
                parsed.push((trimmed, current_ts, current_dur));
                current_cmd.clear();
                in_multiline = false;
            }
            continue;
        }

        // Try extended history format: ": timestamp:duration;command"
        if line.starts_with(": ") {
            if let Some((ts, dur, cmd)) = parse_extended_history_line(line) {
                let ts_ms = ts * 1000;
                let dur_ms = dur * 1000;
                if let Some(stripped) = cmd.strip_suffix('\\') {
                    current_cmd = stripped.to_string();
                    current_ts = ts_ms;
                    current_dur = dur_ms;
                    in_multiline = true;
                } else {
                    parsed.push((cmd, ts_ms, dur_ms));
                }
            }
        } else if !line.trim().is_empty() {
            // Plain format (no timestamp)
            if let Some(stripped) = line.strip_suffix('\\') {
                current_cmd = stripped.to_string();
                current_ts = 0;
                current_dur = 0;
                in_multiline = true;
            } else {
                parsed.push((line.to_string(), 0, 0));
            }
        }
    }

    // Flush any remaining multiline command
    if !current_cmd.is_empty() {
        let trimmed = current_cmd.trim_end().to_string();
        parsed.push((trimmed, current_ts, current_dur));
    }

    parsed
}

/// Print a preview of parsed zsh history entries (for dry-run mode).
fn print_zsh_import_preview(parsed: &[(String, i64, i64)]) {
    println!("\nDry run — no entries written. Sample:");
    for (i, (cmd, ts, _dur)) in parsed.iter().take(10).enumerate() {
        let date = if *ts > 0 {
            chrono::DateTime::from_timestamp_millis(*ts)
                .map(|dt| {
                    dt.with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M")
                        .to_string()
                })
                .unwrap_or_default()
        } else {
            "no timestamp".to_string()
        };
        let display = cmd.replace('\n', "\\n");
        let truncated = crate::util::truncate_str(&display, 60, "…");
        println!("  {:>2}. [{date}] {truncated}", i + 1);
    }
    if parsed.len() > 10 {
        println!("  ... and {} more", parsed.len() - 10);
    }
}

/// Insert parsed entries in a batch. Returns (imported, skipped) counts.
/// Errors are fatal — the caller is responsible for rolling back the transaction.
fn import_entries_batch(
    repo: &Repository,
    parsed: &[(String, i64, i64)],
    session_id: &str,
    now: i64,
) -> Result<(u64, u64), Box<dyn std::error::Error>> {
    let mut imported = 0u64;
    let mut skipped = 0u64;
    let total = parsed.len();

    for (i, (cmd, ts, dur)) in parsed.iter().enumerate() {
        // Skip empty or space-prefixed commands
        if cmd.trim().is_empty() || cmd.starts_with(' ') {
            skipped += 1;
            continue;
        }

        // Dedup: skip if (command, timestamp_ms) already exists.
        // Uses indexed SQL lookup instead of loading all entries into memory.
        if *ts > 0 && repo.entry_exists(cmd, *ts)? {
            skipped += 1;
            continue;
        }

        let started_at = if *ts > 0 { *ts } else { now };
        let ended_at = started_at + dur;

        let entry = Entry::new(
            session_id.to_string(),
            cmd.clone(),
            String::new(), // CWD unknown for imported entries
            None,          // exit code unknown
            started_at,
            ended_at,
        );

        repo.insert_entry(&entry)?;
        imported += 1;

        // Progress every 2000 entries
        if (i + 1) % 2000 == 0 {
            eprint!("\r  Progress: {}/{total}...", i + 1);
        }
    }

    if total >= 2000 {
        eprintln!(); // Clear progress line
    }

    Ok((imported, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_extended_history_line() {
        // Standard extended history format
        let (ts, dur, cmd) = parse_extended_history_line(": 1724827236:0;git status").unwrap();
        assert_eq!(ts, 1_724_827_236);
        assert_eq!(dur, 0);
        assert_eq!(cmd, "git status");
    }

    #[test]
    fn test_parse_extended_history_with_duration() {
        let (ts, dur, cmd) =
            parse_extended_history_line(": 1724827300:15;cargo build --release").unwrap();
        assert_eq!(ts, 1_724_827_300);
        assert_eq!(dur, 15);
        assert_eq!(cmd, "cargo build --release");
    }

    #[test]
    fn test_parse_extended_history_with_semicolons_in_command() {
        // Command itself contains semicolons
        let (ts, dur, cmd) =
            parse_extended_history_line(": 1724827236:0;echo hello; echo world").unwrap();
        assert_eq!(ts, 1_724_827_236);
        assert_eq!(dur, 0);
        assert_eq!(cmd, "echo hello; echo world");
    }

    #[test]
    fn test_parse_extended_history_invalid() {
        assert!(parse_extended_history_line("not a history line").is_none());
        assert!(parse_extended_history_line(": abc:0;cmd").is_none());
        assert!(parse_extended_history_line(": 123").is_none());
    }

    #[test]
    fn test_parse_extended_history_empty_command() {
        // Empty command after semicolon: `: 123:0;`
        let result = parse_extended_history_line(": 123:0;");
        assert!(result.is_some(), "Should parse even with empty command");
        let (ts, dur, cmd) = result.unwrap();
        assert_eq!(ts, 123);
        assert_eq!(dur, 0);
        assert_eq!(cmd, "", "Command should be empty string");
    }

    #[test]
    fn test_parse_extended_history_multiline_marker() {
        // Lines that start with continuation (backslash at end) are handled by
        // the multiline logic in handle_import_zsh_history, not by parse_extended_history_line.
        // But parse_extended_history_line should still correctly parse a command ending with backslash.
        let result = parse_extended_history_line(": 1724827236:0;echo hello \\");
        assert!(result.is_some());
        let (_ts, _dur, cmd) = result.unwrap();
        // The raw line parser just returns the command as-is, including the trailing backslash
        assert!(
            cmd.ends_with('\\'),
            "Command should preserve trailing backslash: {cmd}"
        );
    }

    // ── csv_safe tests ──────────────────────────────────────────────────

    #[test]
    fn test_csv_safe_plain_string() {
        assert_eq!(csv_safe("hello world"), "hello world");
    }

    #[test]
    fn test_csv_safe_escapes_double_quotes() {
        assert_eq!(csv_safe(r#"echo "hi""#), r#"echo ""hi"""#);
    }

    #[test]
    fn test_csv_safe_formula_injection_prefixes() {
        // Each formula-triggering character should get a leading single-quote
        for prefix in &["=", "+", "-", "@", "\t", "\r"] {
            let input = format!("{prefix}dangerous");
            let result = csv_safe(&input);
            assert!(
                result.starts_with('\''),
                "Expected leading quote for prefix {prefix:?}, got: {result}"
            );
        }
    }

    #[test]
    fn test_csv_safe_formula_injection_with_quotes() {
        // Both protections should compose: quotes escaped AND leading single-quote
        let result = csv_safe("=SUM(A1:A10)\"injected\"");
        assert!(result.starts_with('\''), "Should start with single-quote");
        assert!(result.contains("\"\""), "Internal quotes should be doubled");
    }

    // ── parse_zsh_history tests ─────────────────────────────────────────

    #[test]
    fn test_parse_zsh_history_extended_format() {
        let text = "\
: 1700000000:5;git status
: 1700000010:0;ls -la
";
        let parsed = parse_zsh_history(text);
        assert_eq!(parsed.len(), 2);

        assert_eq!(parsed[0].0, "git status");
        assert_eq!(parsed[0].1, 1_700_000_000_000); // seconds → ms
        assert_eq!(parsed[0].2, 5_000); // duration seconds → ms

        assert_eq!(parsed[1].0, "ls -la");
        assert_eq!(parsed[1].1, 1_700_000_010_000);
        assert_eq!(parsed[1].2, 0);
    }

    #[test]
    fn test_parse_zsh_history_plain_format() {
        let text = "echo hello\nls\n";
        let parsed = parse_zsh_history(text);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0, "echo hello");
        assert_eq!(parsed[0].1, 0, "Plain format has no timestamp");
        assert_eq!(parsed[1].0, "ls");
    }

    #[test]
    fn test_parse_zsh_history_multiline_command() {
        // Backslash at end of line signals continuation
        let text = "\
: 1700000000:2;echo hello \\\nworld\n\
: 1700000010:0;ls\n";
        let parsed = parse_zsh_history(text);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0, "echo hello \nworld");
        assert_eq!(parsed[1].0, "ls");
    }

    #[test]
    fn test_parse_zsh_history_skips_blank_lines() {
        let text = "\n\n: 1700000000:0;git diff\n\n\n";
        let parsed = parse_zsh_history(text);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "git diff");
    }

    #[test]
    fn test_parse_zsh_history_multiline_plain_format() {
        let text = "echo start \\\ncontinued\ndone\n";
        let parsed = parse_zsh_history(text);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0, "echo start \ncontinued");
        assert_eq!(parsed[1].0, "done");
    }

    // ── import_entries_batch + deduplication tests ───────────────────────

    #[test]
    fn test_import_entries_batch_inserts_entries() {
        let (_dir, repo) = crate::test_utils::test_repo();

        let session = Session {
            id: "test-import-session".to_string(),
            hostname: "test-host".to_string(),
            created_at: 1_000,
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();

        let parsed = vec![
            ("git status".to_string(), 1_700_000_000_000i64, 5_000i64),
            ("ls -la".to_string(), 1_700_000_010_000, 0),
        ];

        let tx = repo.transaction().unwrap();
        let (imported, skipped) =
            import_entries_batch(&repo, &parsed, &session.id, 9_999_999).unwrap();
        tx.commit().unwrap();

        assert_eq!(imported, 2);
        assert_eq!(skipped, 0);

        // Verify entries are actually in the database
        let mut count = 0u64;
        repo.stream_export_entries(None, None, |_entry| {
            count += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_import_entries_batch_deduplicates() {
        let (_dir, repo) = crate::test_utils::test_repo();

        let session = Session {
            id: "test-dedup-session".to_string(),
            hostname: "test-host".to_string(),
            created_at: 1_000,
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();

        let parsed = vec![
            ("git status".to_string(), 1_700_000_000_000i64, 5_000i64),
            ("ls -la".to_string(), 1_700_000_010_000, 0),
        ];

        // First import
        let tx = repo.transaction().unwrap();
        let (imported, _) = import_entries_batch(&repo, &parsed, &session.id, 9_999_999).unwrap();
        tx.commit().unwrap();
        assert_eq!(imported, 2);

        // Second import of the same data — should be skipped as duplicates
        let tx = repo.transaction().unwrap();
        let (imported2, skipped2) =
            import_entries_batch(&repo, &parsed, &session.id, 9_999_999).unwrap();
        tx.commit().unwrap();
        assert_eq!(imported2, 0, "Duplicates should not be imported again");
        assert_eq!(skipped2, 2, "Both entries should be skipped as duplicates");
    }

    #[test]
    fn test_import_entries_batch_skips_empty_and_space_prefixed() {
        let (_dir, repo) = crate::test_utils::test_repo();

        let session = Session {
            id: "test-skip-session".to_string(),
            hostname: "test-host".to_string(),
            created_at: 1_000,
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();

        let parsed = vec![
            ("".to_string(), 1_700_000_000_000i64, 0i64), // empty
            ("   ".to_string(), 1_700_000_001_000, 0),    // whitespace-only
            (" secret-cmd".to_string(), 1_700_000_002_000, 0), // space-prefixed (private)
            ("valid-cmd".to_string(), 1_700_000_003_000, 0), // should be imported
        ];

        let tx = repo.transaction().unwrap();
        let (imported, skipped) =
            import_entries_batch(&repo, &parsed, &session.id, 9_999_999).unwrap();
        tx.commit().unwrap();

        assert_eq!(imported, 1, "Only the valid command should be imported");
        assert_eq!(
            skipped, 3,
            "Empty, whitespace, and space-prefixed should be skipped"
        );
    }

    // ── JSONL roundtrip test ────────────────────────────────────────────

    #[test]
    fn test_jsonl_roundtrip() {
        // Create an entry, serialize to JSONL, deserialize back, and verify fields match.
        let mut entry = Entry::new(
            "session-rt".to_string(),
            "cargo test --release".to_string(),
            "/home/dev/project".to_string(),
            Some(0),
            1_700_000_000_000,
            1_700_000_005_000,
        );
        entry.executor_type = Some("human".to_string());
        entry.executor = Some("zsh".to_string());

        let json_line = serde_json::to_string(&entry).unwrap();
        let deserialized: Entry = serde_json::from_str(&json_line).unwrap();

        assert_eq!(deserialized.command, entry.command);
        assert_eq!(deserialized.cwd, entry.cwd);
        assert_eq!(deserialized.exit_code, entry.exit_code);
        assert_eq!(deserialized.started_at, entry.started_at);
        assert_eq!(deserialized.ended_at, entry.ended_at);
        assert_eq!(deserialized.duration_ms, entry.duration_ms);
        assert_eq!(deserialized.session_id, entry.session_id);
        assert_eq!(deserialized.executor_type, entry.executor_type);
        assert_eq!(deserialized.executor, entry.executor);
    }

    // ── CSV formatting test ─────────────────────────────────────────────

    #[test]
    fn test_csv_row_formatting() {
        // Verify that a complete CSV row is formatted correctly by replicating
        // the formatting logic from handle_export's CSV branch.
        let mut entry = Entry::new(
            "sess-csv".to_string(),
            "echo \"hello, world\"".to_string(),
            "/home/user".to_string(),
            Some(0),
            1_700_000_000_000,
            1_700_000_001_000,
        );
        entry.executor_type = Some("human".to_string());
        entry.executor = None;

        let cmd = csv_safe(&entry.command);
        let cwd = csv_safe(&entry.cwd);
        let sid = csv_safe(&entry.session_id);
        let etype = csv_safe(entry.executor_type.as_deref().unwrap_or(""));
        let exec = csv_safe(entry.executor.as_deref().unwrap_or(""));
        let row = format!(
            "\"{cmd}\",\"{cwd}\",{},{},{},{},\"{sid}\",\"{etype}\",\"{exec}\"",
            entry.exit_code.map_or(String::new(), |c| c.to_string()),
            entry.started_at,
            entry.ended_at,
            entry.duration_ms,
        );

        // Internal double-quotes should be doubled
        assert!(
            row.contains("\"\"hello, world\"\""),
            "Embedded quotes should be doubled in CSV: {row}"
        );
        // Verify field count by counting commas outside quotes (simple check: 8 commas for 9 fields)
        // The exit_code, started_at, ended_at, duration_ms are unquoted numerics
        assert!(
            row.contains(",0,"),
            "Exit code should appear as unquoted 0: {row}"
        );
        assert!(
            row.contains(",1000,"),
            "Duration should appear as unquoted 1000: {row}"
        );
    }

    // ── handle_import / import_jsonl_into_repo tests ────────────────────
    // These cover the GH#19 regression: exporting from one machine and
    // importing into a fresh DB on another machine used to fail with a
    // FOREIGN KEY constraint error because the export only carried entries,
    // not the parent sessions row that the entries.session_id FK requires.

    fn make_jsonl(entries: &[Entry]) -> String {
        entries
            .iter()
            .map(|e| serde_json::to_string(e).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn test_import_jsonl_creates_placeholder_session_for_unknown_session_id() {
        let (_dir, repo) = crate::test_utils::test_repo();

        // Entry references a session_id that does NOT exist in the destination DB.
        let entry = Entry::new(
            "session-from-other-machine".to_string(),
            "git status".to_string(),
            "/home/dev/project".to_string(),
            Some(0),
            1_700_000_000_000,
            1_700_000_001_000,
        );
        let jsonl = make_jsonl(&[entry]);

        let stats = import_jsonl_into_repo(&repo, jsonl.as_bytes()).expect("import should succeed");

        assert_eq!(stats.imported, 1);
        assert_eq!(stats.parse_errors, 0);
        assert_eq!(stats.placeholder_sessions, 1);

        let session = repo
            .get_session("session-from-other-machine")
            .unwrap()
            .expect("placeholder session should have been created");
        assert_eq!(session.hostname, "imported");
    }

    #[test]
    fn test_import_jsonl_reuses_existing_session_without_duplicate() {
        let (_dir, repo) = crate::test_utils::test_repo();

        // Pre-create the session that the imported entries reference.
        let session = Session {
            id: "preexisting-session".to_string(),
            hostname: "real-host".to_string(),
            created_at: 1_500_000_000_000,
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();

        let entry = Entry::new(
            session.id.clone(),
            "ls".to_string(),
            "/tmp".to_string(),
            Some(0),
            1_700_000_000_000,
            1_700_000_000_500,
        );
        let stats = import_jsonl_into_repo(&repo, make_jsonl(&[entry]).as_bytes()).unwrap();

        assert_eq!(stats.imported, 1);
        assert_eq!(
            stats.placeholder_sessions, 0,
            "no placeholder should be created when the session already exists"
        );

        // Original session row must be untouched (real-host, not 'imported').
        let stored = repo.get_session(&session.id).unwrap().unwrap();
        assert_eq!(stored.hostname, "real-host");
        assert_eq!(stored.created_at, 1_500_000_000_000);
    }

    #[test]
    fn test_import_jsonl_dedups_session_creation_across_entries() {
        let (_dir, repo) = crate::test_utils::test_repo();

        // Three entries from the SAME unknown session — should produce exactly
        // one placeholder, not three.
        let entries: Vec<Entry> = (0..3)
            .map(|i| {
                Entry::new(
                    "shared-session".to_string(),
                    format!("cmd-{i}"),
                    "/tmp".to_string(),
                    Some(0),
                    1_700_000_000_000 + i,
                    1_700_000_000_000 + i + 1,
                )
            })
            .collect();

        let stats = import_jsonl_into_repo(&repo, make_jsonl(&entries).as_bytes()).unwrap();

        assert_eq!(stats.imported, 3);
        assert_eq!(stats.placeholder_sessions, 1);
    }

    #[test]
    fn test_import_jsonl_export_roundtrip_into_fresh_db() {
        // The end-to-end scenario from GH#19: export entries from repo A,
        // import the resulting JSONL into a fresh repo B. Should succeed with
        // no FK errors and produce the same set of commands.
        let (_dir_a, repo_a) = crate::test_utils::test_repo();
        let session = Session {
            id: "mac1-session".to_string(),
            hostname: "mac1".to_string(),
            created_at: 1_600_000_000_000,
            tag_id: None,
        };
        repo_a.insert_session(&session).unwrap();
        let rows: [(&str, i64, i64); 3] = [
            ("git status", 1_700_000_000_000, 1_700_000_000_500),
            ("ls -la", 1_700_000_000_001, 1_700_000_000_501),
            ("cargo test", 1_700_000_000_002, 1_700_000_000_502),
        ];
        for (cmd, started, ended) in rows {
            let entry = Entry::new(
                session.id.clone(),
                cmd.to_string(),
                "/work".to_string(),
                Some(0),
                started,
                ended,
            );
            repo_a.insert_entry(&entry).unwrap();
        }

        let mut jsonl = String::new();
        repo_a
            .stream_export_entries(None, None, |entry| {
                jsonl.push_str(&serde_json::to_string(&entry)?);
                jsonl.push('\n');
                Ok(())
            })
            .unwrap();

        let (_dir_b, repo_b) = crate::test_utils::test_repo();
        let stats = import_jsonl_into_repo(&repo_b, jsonl.as_bytes())
            .expect("import into fresh DB must not hit FK constraint");

        assert_eq!(stats.imported, 3);
        assert_eq!(stats.parse_errors, 0);
        assert_eq!(stats.placeholder_sessions, 1);

        let mut imported_cmds: Vec<String> = Vec::new();
        repo_b
            .stream_export_entries(None, None, |e| {
                imported_cmds.push(e.command);
                Ok(())
            })
            .unwrap();
        imported_cmds.sort();
        assert_eq!(imported_cmds, vec!["cargo test", "git status", "ls -la"]);
    }

    #[test]
    fn test_check_jsonl_shape_rejects_csv_file() {
        use std::io::Write as _;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("history.csv");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            "command,cwd,exit_code,started_at,ended_at,duration_ms,session_id,executor_type,executor"
        )
        .unwrap();
        writeln!(f, "\"ls\",\"/tmp\",0,1,2,1,\"s\",\"human\",\"\"").unwrap();

        let err = check_jsonl_shape(path.to_str().unwrap())
            .expect_err("CSV file should be rejected up front");
        let msg = err.to_string();
        assert!(
            msg.contains("does not look like JSONL"),
            "error should mention JSONL: {msg}"
        );
        assert!(
            msg.contains("CSV"),
            "error should hint at the CSV mistake: {msg}"
        );
    }

    #[test]
    fn test_check_jsonl_shape_accepts_jsonl_file() {
        use std::io::Write as _;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("history.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"session_id":"s","command":"ls","cwd":"/tmp","exit_code":0,"started_at":1,"ended_at":2,"duration_ms":1}}"#).unwrap();

        check_jsonl_shape(path.to_str().unwrap()).expect("JSONL should be accepted");
    }

    #[test]
    fn test_check_jsonl_shape_accepts_empty_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("empty.jsonl");
        std::fs::File::create(&path).unwrap();
        check_jsonl_shape(path.to_str().unwrap()).expect("empty file should not error");
    }

    #[test]
    fn test_check_jsonl_shape_skips_blank_leading_lines() {
        use std::io::Write as _;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("history.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f).unwrap();
        writeln!(f, "   ").unwrap();
        writeln!(f, r#"{{"session_id":"s","command":"ls","cwd":"/tmp","exit_code":0,"started_at":1,"ended_at":2,"duration_ms":1}}"#).unwrap();

        check_jsonl_shape(path.to_str().unwrap())
            .expect("leading blank lines should be skipped before format check");
    }

    // ── tag_id FK remap on JSONL import ────────────────────────────────
    // Same class of bug as the session FK fix: entries.tag_id REFERENCES tags(id),
    // so importing an entry whose tag_id doesn't exist on the destination must
    // not blow up the whole import. Strategy: re-map by tag_name.

    /// Make an entry that will (after JSONL roundtrip) carry `tag_id` + `tag_name`.
    fn entry_with_tag(session_id: &str, command: &str, tag_id: i64, tag_name: &str) -> Entry {
        let mut e = Entry::new(
            session_id.to_string(),
            command.to_string(),
            "/tmp".to_string(),
            Some(0),
            1_700_000_000_000,
            1_700_000_000_500,
        );
        e.tag_id = Some(tag_id);
        e.tag_name = Some(tag_name.to_string());
        e
    }

    #[test]
    fn test_import_jsonl_remaps_tag_id_via_existing_tag_by_name() {
        let (_dir, repo) = crate::test_utils::test_repo();

        // Destination already has a tag named "demo" but with a different id
        // than the source machine assigned.
        let local_tag_id = repo.create_tag("demo", Some("local")).unwrap();
        assert_ne!(local_tag_id, 999, "local tag_id must differ from source");

        // Source machine had tag id=999 named "demo".
        let entry = entry_with_tag("src-session", "git status", 999, "demo");
        let stats =
            import_jsonl_into_repo(&repo, make_jsonl(&[entry]).as_bytes()).expect("import ok");

        assert_eq!(stats.imported, 1);
        assert_eq!(
            stats.created_tags, 0,
            "tag already existed on destination — none created"
        );
        assert_eq!(stats.dropped_tag_associations, 0);

        // Verify the entry was associated with the LOCAL tag id, not 999.
        let mut got_tag_ids: Vec<Option<i64>> = Vec::new();
        repo.stream_export_entries(None, None, |e| {
            got_tag_ids.push(e.tag_id);
            Ok(())
        })
        .unwrap();
        assert_eq!(got_tag_ids, vec![Some(local_tag_id)]);
    }

    #[test]
    fn test_import_jsonl_creates_missing_tag_during_import() {
        let (_dir, repo) = crate::test_utils::test_repo();
        // Destination has zero tags. Import an entry tagged "imported-tag".
        let entry = entry_with_tag("src-session", "ls", 7, "imported-tag");

        let stats =
            import_jsonl_into_repo(&repo, make_jsonl(&[entry]).as_bytes()).expect("import ok");

        assert_eq!(stats.imported, 1);
        assert_eq!(stats.created_tags, 1, "tag should have been created");
        assert_eq!(stats.dropped_tag_associations, 0);

        // The new tag should exist locally — name is lower-cased per create_tag.
        let new_id = repo
            .get_tag_id_by_name("imported-tag")
            .unwrap()
            .expect("tag should exist after import");

        let mut tag_ids: Vec<Option<i64>> = Vec::new();
        repo.stream_export_entries(None, None, |e| {
            tag_ids.push(e.tag_id);
            Ok(())
        })
        .unwrap();
        assert_eq!(tag_ids, vec![Some(new_id)]);
    }

    #[test]
    fn test_import_jsonl_clears_tag_id_when_no_tag_name() {
        // Defensive: an export with tag_id but no tag_name shouldn't be allowed
        // to point at a random tag id on the destination. Drop the association.
        let (_dir, repo) = crate::test_utils::test_repo();

        let mut entry = Entry::new(
            "src-session".to_string(),
            "ls".to_string(),
            "/tmp".to_string(),
            Some(0),
            1_700_000_000_000,
            1_700_000_000_500,
        );
        entry.tag_id = Some(42);
        entry.tag_name = None;

        let stats =
            import_jsonl_into_repo(&repo, make_jsonl(&[entry]).as_bytes()).expect("import ok");

        assert_eq!(stats.imported, 1);
        assert_eq!(stats.created_tags, 0);
        assert_eq!(stats.dropped_tag_associations, 1);

        let mut tag_ids: Vec<Option<i64>> = Vec::new();
        repo.stream_export_entries(None, None, |e| {
            tag_ids.push(e.tag_id);
            Ok(())
        })
        .unwrap();
        assert_eq!(tag_ids, vec![None], "tag_id should be cleared on import");
    }

    #[test]
    fn test_import_jsonl_handles_tag_cap_gracefully() {
        // Simulate: destination already has the 20-tag cap maxed out. Importing
        // an entry that references a NEW tag must drop the association rather
        // than fail the whole import.
        let (_dir, repo) = crate::test_utils::test_repo();
        for i in 0..20 {
            repo.create_tag(&format!("local-tag-{i}"), None).unwrap();
        }

        let entry = entry_with_tag("src-session", "ls", 999, "brand-new-tag");
        let stats =
            import_jsonl_into_repo(&repo, make_jsonl(&[entry]).as_bytes()).expect("import ok");

        assert_eq!(stats.imported, 1);
        assert_eq!(
            stats.created_tags, 0,
            "tag creation should fail under the cap"
        );
        assert_eq!(stats.dropped_tag_associations, 1);

        // Entry imported with NULL tag_id; tag count stays at 20.
        let tags = repo.get_tags().unwrap();
        assert_eq!(tags.len(), 20);
    }

    #[test]
    fn test_import_jsonl_tag_remap_is_cached_across_entries() {
        // Three entries reference the same source tag — should call create_tag
        // exactly once (verified indirectly by created_tags = 1).
        let (_dir, repo) = crate::test_utils::test_repo();

        let entries: Vec<Entry> = (0..3)
            .map(|i| {
                let mut e = entry_with_tag("src-session", "cmd", 999, "shared-tag");
                e.command = format!("cmd-{i}");
                e.started_at += i;
                e.ended_at += i;
                e
            })
            .collect();

        let stats = import_jsonl_into_repo(&repo, make_jsonl(&entries).as_bytes()).unwrap();
        assert_eq!(stats.imported, 3);
        assert_eq!(
            stats.created_tags, 1,
            "tag should be created once and cached"
        );
    }

    #[test]
    fn test_import_jsonl_full_roundtrip_preserves_tag_associations() {
        // End-to-end: source DB has a tag and entries tagged with it.
        // Export to JSONL, import into a fresh DB, verify entries are still
        // tagged with the same name (id will differ).
        let (_dir_a, repo_a) = crate::test_utils::test_repo();
        let session = Session {
            id: "mac1-session".to_string(),
            hostname: "mac1".to_string(),
            created_at: 1_600_000_000_000,
            tag_id: None,
        };
        repo_a.insert_session(&session).unwrap();
        let src_tag = repo_a.create_tag("project-x", Some("source")).unwrap();
        let mut entry = Entry::new(
            session.id,
            "make build".to_string(),
            "/work".to_string(),
            Some(0),
            1_700_000_000_000,
            1_700_000_000_500,
        );
        entry.tag_id = Some(src_tag);
        repo_a.insert_entry(&entry).unwrap();

        let mut jsonl = String::new();
        repo_a
            .stream_export_entries(None, None, |e| {
                jsonl.push_str(&serde_json::to_string(&e)?);
                jsonl.push('\n');
                Ok(())
            })
            .unwrap();

        let (_dir_b, repo_b) = crate::test_utils::test_repo();
        let stats = import_jsonl_into_repo(&repo_b, jsonl.as_bytes())
            .expect("roundtrip with tagged entries should not hit FK constraint");

        assert_eq!(stats.imported, 1);
        assert_eq!(stats.created_tags, 1);
        assert_eq!(stats.placeholder_sessions, 1);

        // Entry on the destination should be associated with the local
        // "project-x" tag, even if its numeric id differs from the source.
        let dst_tag_id = repo_b.get_tag_id_by_name("project-x").unwrap().unwrap();
        let mut dst_tag_ids: Vec<Option<i64>> = Vec::new();
        repo_b
            .stream_export_entries(None, None, |e| {
                dst_tag_ids.push(e.tag_id);
                Ok(())
            })
            .unwrap();
        assert_eq!(dst_tag_ids, vec![Some(dst_tag_id)]);
    }

    // ── JSON export shape ──────────────────────────────────────────────

    #[test]
    fn test_json_export_emits_valid_array_when_empty() {
        // Mirror handle_export's "json" branch with zero entries.
        // Empty result must still produce valid JSON ("[]"), not an empty file.
        let mut buf: Vec<u8> = Vec::new();
        let count = 0usize;

        if count == 0 {
            writeln!(buf, "[]").unwrap();
        } else {
            writeln!(buf, "\n]").unwrap();
        }

        let s = String::from_utf8(buf).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(s.trim()).expect("empty export must be valid JSON");
        assert!(parsed.is_array());
        assert_eq!(parsed.as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_json_export_format_is_valid_json_for_entries() {
        // Replicate handle_export's "json" branch for two entries and verify the
        // resulting bytes parse as a JSON array of two objects.
        let entries = [
            Entry::new(
                "s1".to_string(),
                "ls".to_string(),
                "/tmp".to_string(),
                Some(0),
                1,
                2,
            ),
            Entry::new(
                "s1".to_string(),
                "echo hi".to_string(),
                "/tmp".to_string(),
                Some(0),
                3,
                4,
            ),
        ];

        let mut buf: Vec<u8> = Vec::new();
        for (count, entry) in entries.iter().enumerate() {
            if count == 0 {
                writeln!(buf, "[").unwrap();
            } else {
                writeln!(buf, ",").unwrap();
            }
            write!(buf, "  {}", serde_json::to_string(&entry).unwrap()).unwrap();
        }
        writeln!(buf, "\n]").unwrap();

        let s = String::from_utf8(buf).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(&s).expect("two-entry export must be valid JSON");
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["command"], "ls");
        assert_eq!(arr[1]["command"], "echo hi");
    }

    // ── CSV export edge cases ──────────────────────────────────────────

    #[test]
    fn test_csv_row_with_embedded_newline_is_rfc4180_compliant() {
        // RFC 4180 §2.6: fields containing line breaks must be quoted.
        // csv_safe doesn't strip newlines (intentionally) — the surrounding
        // `"..."` in handle_export's format string carries the field across
        // line boundaries. Verify the output a CSV reader would see.
        let mut entry = Entry::new(
            "s".to_string(),
            "cat <<EOF\nhello\nEOF".to_string(),
            "/tmp".to_string(),
            Some(0),
            1,
            2,
        );
        entry.executor_type = None;
        entry.executor = None;

        let cmd = csv_safe(&entry.command);
        let row = format!("\"{cmd}\",\"{}\"", csv_safe(&entry.cwd));
        // The cell contains literal newlines, but they're inside double quotes,
        // so a compliant CSV reader treats them as part of the field.
        assert!(row.contains("\"cat <<EOF\nhello\nEOF\""));
        // Quote count is even (open + close pairs only — no broken quoting).
        let quotes = row.chars().filter(|c| *c == '"').count();
        assert_eq!(quotes % 2, 0);
    }

    #[test]
    fn test_csv_safe_strips_leading_cr_via_apostrophe() {
        // CR at start would otherwise be a formula-injection vector in some
        // spreadsheet apps; csv_safe prefixes with a single quote.
        let out = csv_safe("\rmalicious");
        assert!(out.starts_with('\''));
    }
}
