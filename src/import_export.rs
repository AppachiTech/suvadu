use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};

use crate::models::{Entry, Session};
use crate::repository::Repository;
use crate::util;
use crate::util::CompiledExclusion;

pub mod atuin;

// ── shared importer helpers ─────────────────────────────────────────────
//
// Every history importer applies the same recording policy, derives its
// timestamps the same way, and previews a dry run the same way. These live
// here so `--from bash-history` and `--from atuin-db` cannot drift apart.

/// What the recording policy says about one imported command.
pub enum RecordingPolicy {
    /// Blank, or space/tab-prefixed (`HISTCONTROL=ignorespace` style).
    Ignored,
    /// Dropped by a configured exclusion pattern.
    Excluded,
    /// Store it — possibly rewritten by redaction first.
    Keep { command: String, redacted: bool },
}

/// Apply the same policy live recording applies: never store a blank or
/// space-prefixed command, honour the configured exclusion patterns, and
/// redact secrets before the text reaches storage (or a dry-run preview).
pub fn apply_recording_policy(
    raw: &str,
    config: &crate::config::Config,
    exclusions: Option<&[CompiledExclusion]>,
) -> RecordingPolicy {
    if raw.trim().is_empty() || raw.starts_with([' ', '\t']) {
        return RecordingPolicy::Ignored;
    }
    if let Some(patterns) = exclusions {
        if crate::util::is_excluded_compiled(raw, patterns) {
            return RecordingPolicy::Excluded;
        }
    }
    let command = if config.redaction.enabled {
        crate::redact::redact_secrets_with_extra(raw, &config.redaction.extra_patterns)
    } else {
        raw.to_string()
    };
    let redacted = command != raw;
    RecordingPolicy::Keep { command, redacted }
}

/// What the recording policy says about one imported free-text metadata
/// field (an Atuin `intent`, an author name, a shell name — anything the
/// source recorded as prose and we persist beside the command).
pub enum MetadataPolicy {
    /// Store it — possibly rewritten by redaction first.
    Keep { text: String, redacted: bool },
    /// An exclusion pattern matched. The field is withheld entirely rather
    /// than trimmed: exclusions say "this text must never be stored", and a
    /// metadata field has no safe partial form.
    Withheld,
}

/// Apply the recording policy to one free-text metadata value.
///
/// This mirrors what live recording does to `context.agent_prompt` — the
/// closest analogue Suvadu records itself — with one addition: an exclusion
/// match withholds the field. It never drops the whole row, because the
/// command has already been judged on its own.
pub fn apply_metadata_policy(
    raw: &str,
    config: &crate::config::Config,
    exclusions: Option<&[CompiledExclusion]>,
) -> MetadataPolicy {
    if let Some(patterns) = exclusions {
        if crate::util::is_excluded_compiled(raw, patterns) {
            return MetadataPolicy::Withheld;
        }
    }
    let text = if config.redaction.enabled {
        crate::redact::redact_secrets_with_extra(raw, &config.redaction.extra_patterns)
    } else {
        raw.to_string()
    };
    let redacted = text != raw;
    MetadataPolicy::Keep { text, redacted }
}

/// How many times this (command, source timestamp) pair has already been seen
/// in this import. Importers add the ordinal to the derived `started_at`, so
/// repeated executions keep distinct timestamps — deterministically, which is
/// what makes a second import a no-op instead of a duplicate.
pub fn next_occurrence(
    occurrences: &mut HashMap<(String, Option<i64>), i64>,
    key: (String, Option<i64>),
) -> i64 {
    let occurrence = occurrences.entry(key).or_insert(0);
    let ordinal = *occurrence;
    *occurrence += 1;
    ordinal
}

/// Print the dry-run preview: a handful of (already redacted) samples with the
/// timestamp the source gave them, and how many more there are.
pub fn print_dry_run_samples(samples: &[(String, Option<i64>)], imported: u64) {
    if samples.is_empty() {
        return;
    }
    println!("\nDry run — no entries written. Sample:");
    for (i, (cmd, ts)) in samples.iter().enumerate() {
        let when = ts.map_or_else(
            || "no timestamp".to_string(),
            |ms| {
                chrono::DateTime::from_timestamp_millis(ms)
                    .map(|dt| {
                        dt.with_timezone(&chrono::Local)
                            .format("%Y-%m-%d %H:%M")
                            .to_string()
                    })
                    .unwrap_or_default()
            },
        );
        let display = cmd.replace('\n', "\\n");
        let truncated = crate::util::truncate_str(&display, 60, "…");
        println!("  {:>2}. [{when}] {truncated}", i + 1);
    }
    let shown = u64::try_from(samples.len()).unwrap_or(u64::MAX);
    if imported > shown {
        println!("  ... and {} more", imported - shown);
    }
}

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
    /// Entries skipped because an identical (`command`, `started_at`) row
    /// already existed — keeps re-syncing the same export idempotent.
    pub dropped_duplicates: u64,
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
                 `suv import` accepts JSONL only (use `--from zsh-history` or\n\
                 `--from bash-history` for shell history files).\n\
                 If you exported as CSV, re-export with: suv export > history.jsonl"
            )
            .into());
        }
        return Ok(());
    }
    Ok(())
}

pub fn handle_import(
    file: &str,
    dry_run: bool,
    allow_duplicates: bool,
) -> Result<(), Box<dyn std::error::Error>> {
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
    let stats = if allow_duplicates {
        import_jsonl_into_repo_opts(&repo, reader, true)?
    } else {
        import_jsonl_into_repo(&repo, reader)?
    };

    println!(
        "Imported {} entries ({} skipped).",
        stats.imported, stats.parse_errors
    );
    if stats.dropped_duplicates > 0 {
        println!(
            "  Skipped {} duplicate entry(ies) already present (use --allow-duplicates to keep them).",
            stats.dropped_duplicates
        );
    }
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
///
/// Duplicate entries (same `command` + `started_at` as an existing row) are
/// skipped so re-importing the same export — or merging the same history onto
/// several machines — is idempotent. Use [`import_jsonl_into_repo_opts`] with
/// `allow_duplicates = true` to keep every line.
pub fn import_jsonl_into_repo<R: BufRead>(
    repo: &Repository,
    reader: R,
) -> Result<ImportStats, Box<dyn std::error::Error>> {
    import_jsonl_into_repo_opts(repo, reader, false)
}

/// Like [`import_jsonl_into_repo`] but with explicit duplicate handling.
pub fn import_jsonl_into_repo_opts<R: BufRead>(
    repo: &Repository,
    reader: R,
    allow_duplicates: bool,
) -> Result<ImportStats, Box<dyn std::error::Error>> {
    const BATCH_SIZE: u64 = 10_000;
    // The session this row belonged to on its source machine does not exist
    // here. The placeholder hostname is also how the capture diagnostics
    // recognise a JSONL-imported row as stored history rather than something
    // this machine's hook observed, so it comes from the shared constant.
    const PLACEHOLDER_HOSTNAME: &str = crate::repository::PLACEHOLDER_IMPORT_HOSTNAME;

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

        // Skip entries already present so re-syncing the same export is
        // idempotent. Visible within the open transaction, so intra-file
        // duplicates are caught too.
        if !allow_duplicates && repo.entry_exists(&entry.command, entry.started_at)? {
            stats.dropped_duplicates += 1;
            continue;
        }

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

    // Apply the same policy live recording and the Bash importer apply, so a
    // secret already sitting in ~/.zsh_history is redacted before storage and
    // an excluded command is never imported. Until this was added, importing
    // could store text that typing the same command would have redacted.
    let config = crate::config::load_config()?;
    let exclusions = (!config.exclusions.is_empty())
        .then(|| crate::util::compile_exclusions(&config.exclusions));
    let mut ignored = 0u64;
    let mut excluded = 0u64;
    let mut redacted = 0u64;
    let parsed: Vec<(String, i64, i64)> = parsed
        .into_iter()
        .filter_map(|(raw, started_at, duration)| {
            match apply_recording_policy(&raw, &config, exclusions.as_deref()) {
                RecordingPolicy::Ignored => {
                    ignored += 1;
                    None
                }
                RecordingPolicy::Excluded => {
                    excluded += 1;
                    None
                }
                RecordingPolicy::Keep {
                    command,
                    redacted: was_redacted,
                } => {
                    if was_redacted {
                        redacted += 1;
                    }
                    Some((command, started_at, duration))
                }
            }
        })
        .collect();

    let policy_counts = |imported: &str| {
        println!("  {imported}");
        println!("  Excluded by config: {excluded}");
        println!("  Blank/space-prefixed, not recorded: {ignored}");
        println!("  Redacted before storage: {redacted}");
    };

    if dry_run {
        print_zsh_import_preview(&parsed);
        println!();
        policy_counts(&format!(
            "Dry run complete. {} entry(ies) would be imported.",
            parsed.len()
        ));
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
    policy_counts(&format!("Imported: {imported}"));
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
/// Provenance stamped on every row the zsh importer writes.
///
/// Without it a zsh-imported row was indistinguishable from one the live
/// hook recorded, and `suv status` / `suv doctor` read stored history as
/// proof that capture was working. The keys match the Bash and Atuin
/// importers so one check covers all three.
fn zsh_import_context(imported_at_ms: i64, timestamp_from_file: bool) -> HashMap<String, String> {
    let mut context = HashMap::new();
    context.insert("import_source".to_string(), "zsh-history".to_string());
    context.insert("imported_at".to_string(), imported_at_ms.to_string());
    context.insert(
        "timestamp_source".to_string(),
        if timestamp_from_file {
            "file".to_string()
        } else {
            // A plain (non-extended) ~/.zsh_history carries no times at all,
            // so the import's own clock stands in. Say so rather than let it
            // read as the moment the command actually ran.
            "synthetic".to_string()
        },
    );
    context.insert(
        "unknown_fields".to_string(),
        "cwd,exit_code,executor".to_string(),
    );
    context
}

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

        let mut entry = Entry::new(
            session_id.to_string(),
            cmd.clone(),
            String::new(), // CWD unknown for imported entries
            None,          // exit code unknown
            started_at,
            ended_at,
        );
        entry.context = Some(zsh_import_context(now, *ts > 0));

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

// ── Bash history import ─────────────────────────────────────────────────
//
// Bash writes its history file in one of two shapes:
//
// * **plain** — one physical line per recorded command, no metadata at all.
//   A multi-line command is written as several consecutive lines with *no*
//   marker separating it from the next command, so the boundaries simply are
//   not in the file. We therefore import one record per line and say so,
//   rather than guessing where a multi-line command started and ended.
// * **timestamped** — with `HISTTIMEFORMAT` set, bash writes a `#<epoch>`
//   comment line before each command. That header *is* a record boundary, so
//   multi-line commands are reconstructed exactly, and the epoch is preserved.
//
// Nothing else is in the file: no directory, no exit code, no duration, no
// executor. Those are stored as unknown (NULL/empty), never invented.

/// Smallest epoch (seconds) accepted in a `#<epoch>` header.
const BASH_EPOCH_MIN_SECS: i64 = 1;
/// Largest epoch (seconds) accepted in a `#<epoch>` header — 2100-01-01Z.
/// Anything beyond this is a corrupt header, not a timestamp.
const BASH_EPOCH_MAX_SECS: i64 = 4_102_444_800;

/// Records whose source file carried no timestamp are stored in a sentinel
/// window that starts one millisecond after the Unix epoch. The value is
/// explicitly synthetic: it sorts before every real record and is obviously
/// not a real execution time, instead of a plausible-looking lie such as
/// "the moment you happened to run the import".
const SYNTHETIC_TS_BASE_MS: i64 = 1;

/// End of the synthetic-timestamp sentinel window (1970-01-02). Every
/// synthetic `started_at` is below this; every real bash timestamp is above it.
pub const SYNTHETIC_TS_CEILING_MS: i64 = 86_400_000;

/// One record recovered from a Bash history file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashRecord {
    /// Command text exactly as the file contained it (multi-line commands keep
    /// their embedded newlines).
    pub command: String,
    /// Unix milliseconds from a `#<epoch>` header, or `None` when the file
    /// carried no timestamp for this record.
    pub timestamp_ms: Option<i64>,
}

/// What the parser saw in the file.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BashParseStats {
    /// Records handed to the callback.
    pub records: u64,
    /// Corrupt `#<epoch>` headers and headers with no command after them.
    pub malformed: u64,
    /// Lines that contained invalid UTF-8 (replaced with U+FFFD, never fatal).
    pub lossy_lines: u64,
    /// `true` once a valid `#<epoch>` header is seen.
    pub timestamped: bool,
}

/// Outcome of a Bash history import (or of a dry run).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BashImportStats {
    /// Records recovered from the file (before policy filtering).
    pub parsed: u64,
    /// Entries written (or, in a dry run, that would be written).
    pub imported: u64,
    /// Records already present in the database.
    pub duplicates: u64,
    /// Records dropped by the configured exclusion patterns.
    pub excluded: u64,
    /// Records whose text was changed by redaction before storage.
    pub redacted: u64,
    /// Blank or space-prefixed records (`HISTCONTROL=ignorespace` style).
    pub ignored: u64,
    /// Malformed records reported by the parser.
    pub malformed: u64,
    /// Lines with invalid UTF-8 bytes.
    pub lossy_lines: u64,
    /// Records that carried a real timestamp.
    pub with_timestamp: u64,
    /// Records stored with a synthetic sentinel timestamp.
    pub without_timestamp: u64,
    /// `true` when the file used the `#<epoch>` timestamped format.
    pub timestamped: bool,
    /// Up to ten redacted samples for the dry-run preview.
    pub samples: Vec<(String, Option<i64>)>,
}

/// Inputs for [`import_bash_history`] that don't come from the file itself.
pub struct BashImportOptions<'a> {
    /// Session the imported entries are attached to. Created lazily — a file
    /// with nothing to import leaves no empty session behind.
    pub session_id: &'a str,
    pub hostname: &'a str,
    /// Wall-clock time of this import run, recorded as provenance.
    pub imported_at_ms: i64,
    /// Count everything, write nothing.
    pub dry_run: bool,
}

/// One classified line of a Bash history file.
enum BashLine<'a> {
    /// `#<epoch>` record boundary.
    Header(i64),
    /// `#<digits>` that cannot be a real epoch (overflow / out of range).
    MalformedHeader,
    /// Anything else — command text, including genuine `# comment` commands.
    Command(&'a str),
}

fn classify_bash_line(line: &str) -> BashLine<'_> {
    if let Some(rest) = line.strip_prefix('#') {
        let digits = rest.trim_end();
        if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
            return match digits.parse::<i64>() {
                Ok(secs) if (BASH_EPOCH_MIN_SECS..=BASH_EPOCH_MAX_SECS).contains(&secs) => {
                    BashLine::Header(secs)
                }
                _ => BashLine::MalformedHeader,
            };
        }
    }
    BashLine::Command(line)
}

/// A timestamped record being accumulated across lines.
struct PendingBashRecord {
    timestamp_ms: i64,
    lines: Vec<String>,
}

/// Emit the record under construction, if any. A header with no command after
/// it is counted as malformed rather than stored as an empty command.
fn flush_pending_bash_record<F>(
    pending: Option<PendingBashRecord>,
    stats: &mut BashParseStats,
    on_record: &mut F,
) -> Result<(), Box<dyn std::error::Error>>
where
    F: FnMut(BashRecord) -> Result<(), Box<dyn std::error::Error>>,
{
    let Some(mut pending) = pending else {
        return Ok(());
    };
    while pending.lines.last().is_some_and(|l| l.trim().is_empty()) {
        pending.lines.pop();
    }
    if pending.lines.is_empty() {
        stats.malformed += 1;
        return Ok(());
    }
    stats.records += 1;
    on_record(BashRecord {
        command: pending.lines.join("\n"),
        timestamp_ms: Some(pending.timestamp_ms),
    })
}

/// Stream a Bash history file, handing one [`BashRecord`] at a time to
/// `on_record`. Reads line by line so a multi-gigabyte history file never
/// lands in memory, and decodes lossily so a few stray bytes (bash writes the
/// file in the terminal's encoding, whatever that was) don't abort the import.
pub fn stream_bash_history<R: BufRead, F>(
    mut reader: R,
    mut on_record: F,
) -> Result<BashParseStats, Box<dyn std::error::Error>>
where
    F: FnMut(BashRecord) -> Result<(), Box<dyn std::error::Error>>,
{
    let mut stats = BashParseStats::default();
    let mut pending: Option<PendingBashRecord> = None;
    let mut buf: Vec<u8> = Vec::new();

    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        while matches!(buf.last(), Some(b'\n' | b'\r')) {
            buf.pop();
        }
        let line = match String::from_utf8_lossy(&buf) {
            std::borrow::Cow::Borrowed(s) => s.to_string(),
            std::borrow::Cow::Owned(s) => {
                stats.lossy_lines += 1;
                s
            }
        };

        match classify_bash_line(&line) {
            BashLine::Header(secs) => {
                stats.timestamped = true;
                flush_pending_bash_record(pending.take(), &mut stats, &mut on_record)?;
                pending = Some(PendingBashRecord {
                    timestamp_ms: secs * 1000,
                    lines: Vec::new(),
                });
            }
            BashLine::MalformedHeader => {
                stats.malformed += 1;
                // The record before the broken header is still complete.
                flush_pending_bash_record(pending.take(), &mut stats, &mut on_record)?;
            }
            BashLine::Command(text) => {
                if let Some(p) = pending.as_mut() {
                    // Inside a timestamped record: the next header ends it, so
                    // this line belongs to the current (possibly multi-line)
                    // command.
                    p.lines.push(text.to_string());
                } else if !text.trim().is_empty() {
                    // No boundary information available — one line, one record.
                    stats.records += 1;
                    on_record(BashRecord {
                        command: text.to_string(),
                        timestamp_ms: None,
                    })?;
                }
            }
        }
    }

    flush_pending_bash_record(pending.take(), &mut stats, &mut on_record)?;
    Ok(stats)
}

/// Build the stored entry for one imported Bash command. Everything bash
/// history does not record is written as unknown — empty directory, `NULL`
/// exit code, zero duration, `"unknown"` executor — and the provenance
/// context marks the row (and its timestamp) as imported rather than observed.
fn bash_entry(
    opts: &BashImportOptions<'_>,
    command: String,
    started_at: i64,
    timestamp_from_file: bool,
) -> Entry {
    let mut entry = Entry::new(
        opts.session_id.to_string(),
        command,
        String::new(), // directory is not in a bash history file
        None,          // exit code unknown — never assume success
        started_at,
        started_at, // no duration information exists
    );
    entry.executor_type = Some("unknown".to_string());

    let mut context = HashMap::new();
    context.insert("import_source".to_string(), "bash-history".to_string());
    context.insert("imported_at".to_string(), opts.imported_at_ms.to_string());
    context.insert(
        "timestamp_source".to_string(),
        if timestamp_from_file {
            "file".to_string()
        } else {
            "synthetic".to_string()
        },
    );
    context.insert(
        "unknown_fields".to_string(),
        "cwd,exit_code,duration_ms,executor".to_string(),
    );
    entry.context = Some(context);
    entry
}

/// Import a Bash history stream into `repo`.
///
/// Storage contract for the fields bash history does not contain:
///
/// | field | stored as |
/// |---|---|
/// | `cwd` | empty string (unknown) |
/// | `exit_code` | `NULL` — never a fabricated `0` |
/// | `duration_ms` | `0` (`ended_at == started_at`; unknown, not measured) |
/// | `executor_type` | `"unknown"` |
/// | `started_at` (plain files) | synthetic sentinel below [`SYNTHETIC_TS_CEILING_MS`] |
///
/// Every entry also carries an `import_source` / `timestamp_source` /
/// `unknown_fields` provenance context so a row can never be mistaken for a
/// natively recorded one.
///
/// **Idempotency.** Each record's `started_at` is derived deterministically
/// from the file: a real epoch, or the sentinel base, plus the number of times
/// that exact (redacted) command has already appeared with that same
/// timestamp. Repeated executions therefore keep distinct timestamps and are
/// all imported, while re-running the import over the same file produces the
/// same timestamps and skips every record as a duplicate.
///
/// Work is done in a transaction that re-commits every `BATCH_SIZE` entries to
/// bound WAL growth. The input file is only ever read.
pub fn import_bash_history<R: BufRead>(
    repo: &Repository,
    reader: R,
    config: &crate::config::Config,
    opts: &BashImportOptions<'_>,
) -> Result<BashImportStats, Box<dyn std::error::Error>> {
    const BATCH_SIZE: u64 = 5_000;
    const MAX_SAMPLES: usize = 10;

    let exclusions = (!config.exclusions.is_empty())
        .then(|| crate::util::compile_exclusions(&config.exclusions));

    let mut stats = BashImportStats::default();
    // (command, source timestamp) → occurrences seen so far in this file.
    let mut occurrences: HashMap<(String, Option<i64>), i64> = HashMap::new();
    let mut session_created = false;
    let mut batch_count = 0u64;

    let tx = if opts.dry_run {
        None
    } else {
        Some(repo.transaction()?)
    };

    let parse_stats = stream_bash_history(reader, |record| {
        let raw = record.command;

        // Same recording policy as live capture: space-prefixed commands
        // (HISTCONTROL=ignorespace), blanks, exclusions and redaction.
        let command = match apply_recording_policy(&raw, config, exclusions.as_deref()) {
            RecordingPolicy::Ignored => {
                stats.ignored += 1;
                return Ok(());
            }
            RecordingPolicy::Excluded => {
                stats.excluded += 1;
                return Ok(());
            }
            RecordingPolicy::Keep { command, redacted } => {
                if redacted {
                    stats.redacted += 1;
                }
                command
            }
        };

        let ordinal = next_occurrence(&mut occurrences, (command.clone(), record.timestamp_ms));

        let started_at = if let Some(ts) = record.timestamp_ms {
            stats.with_timestamp += 1;
            // Bash timestamps have one-second resolution, so two runs of the
            // same command in the same second would otherwise collide.
            ts + ordinal
        } else {
            stats.without_timestamp += 1;
            // Clamped so a pathological repeat count can never push a
            // synthetic time out of the sentinel window and into a range that
            // would read as a real timestamp.
            SYNTHETIC_TS_BASE_MS + ordinal.min(SYNTHETIC_TS_CEILING_MS - SYNTHETIC_TS_BASE_MS - 1)
        };

        if repo.entry_exists(&command, started_at)? {
            stats.duplicates += 1;
            return Ok(());
        }

        if stats.samples.len() < MAX_SAMPLES {
            stats.samples.push((command.clone(), record.timestamp_ms));
        }

        if opts.dry_run {
            stats.imported += 1;
            return Ok(());
        }

        if !session_created {
            repo.insert_session(&Session {
                id: opts.session_id.to_string(),
                hostname: opts.hostname.to_string(),
                created_at: opts.imported_at_ms,
                tag_id: None,
            })?;
            session_created = true;
        }

        let entry = bash_entry(opts, command, started_at, record.timestamp_ms.is_some());
        repo.insert_entry(&entry)?;
        stats.imported += 1;
        batch_count += 1;
        if batch_count >= BATCH_SIZE {
            if let Some(tx) = tx.as_ref() {
                tx.recommit()?;
            }
            batch_count = 0;
        }
        Ok(())
    })?;

    stats.parsed = parse_stats.records;
    stats.malformed = parse_stats.malformed;
    stats.lossy_lines = parse_stats.lossy_lines;
    stats.timestamped = parse_stats.timestamped;

    if let Some(tx) = tx {
        tx.commit()?;
    }
    Ok(stats)
}

/// `suv import --from bash-history <file>`
pub fn handle_import_bash_history(
    file: &str,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let f = std::fs::File::open(file)?;
    let reader = std::io::BufReader::new(f);

    // The global config only — imported commands have no directory, so a
    // per-project `.suvadu.toml` overlay cannot be resolved for them.
    let config = crate::config::load_config()?;
    let repo = Repository::init()?;

    let session_id = format!("import-bash-{}", uuid::Uuid::new_v4());
    let hostname = hostname::get()?.to_string_lossy().to_string();
    let now = chrono::Utc::now().timestamp_millis();

    let stats = import_bash_history(
        &repo,
        reader,
        &config,
        &BashImportOptions {
            session_id: &session_id,
            hostname: &hostname,
            imported_at_ms: now,
            dry_run,
        },
    )?;

    let shape = if stats.timestamped {
        "timestamped format — `#<epoch>` headers"
    } else {
        "plain format — no timestamps"
    };
    println!("Parsed {} command(s) from {file} ({shape})", stats.parsed);

    if !stats.timestamped && stats.parsed > 0 {
        println!(
            "Note: plain Bash history has no record boundaries and no times. Each line is\n\
             \x20     imported as one command — multi-line commands cannot be reconstructed —\n\
             \x20     and every entry gets an explicitly synthetic placeholder timestamp\n\
             \x20     (1970-01-01) instead of an invented one. Set HISTTIMEFORMAT in bash to\n\
             \x20     record real timestamps from now on."
        );
    }
    if stats.lossy_lines > 0 {
        println!(
            "  {} line(s) contained invalid UTF-8; those bytes were replaced with \u{FFFD}.",
            stats.lossy_lines
        );
    }

    if dry_run {
        print_dry_run_samples(&stats.samples, stats.imported);
        println!(
            "\nDry run complete. {} entry(ies) would be imported.",
            stats.imported
        );
        print_bash_import_counts(&stats);
        return Ok(());
    }

    println!("\n✓ Import complete:");
    println!("  Imported: {}", stats.imported);
    print_bash_import_counts(&stats);
    if stats.imported > 0 {
        println!("  Session:  {session_id}");
    }
    Ok(())
}

/// Shared tail of the import / dry-run report. Counts only — never the text of
/// a skipped, redacted or malformed record.
fn print_bash_import_counts(stats: &BashImportStats) {
    println!(
        "  Already present: {} (re-importing the same file adds nothing)",
        stats.duplicates
    );
    println!("  Excluded by config: {}", stats.excluded);
    println!("  Blank/space-prefixed, not recorded: {}", stats.ignored);
    println!("  Malformed records skipped: {}", stats.malformed);
    println!("  Redacted before storage: {}", stats.redacted);
    println!(
        "  Timestamps: {} from the file, {} synthetic placeholders",
        stats.with_timestamp, stats.without_timestamp
    );
    println!("  Not in bash history (stored unknown): directory, exit code, duration, executor");
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

    /// Every zsh-imported row must carry the same provenance the Bash and
    /// Atuin importers write. Without it `suv status`/`suv doctor` could not
    /// tell a zsh-imported row from one the live hook recorded, and read
    /// stored history as proof of capture (R09).
    #[test]
    fn zsh_imported_rows_carry_import_provenance() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let session = Session {
            id: "import-zsh-provenance".to_string(),
            hostname: "test-host".to_string(),
            created_at: 1_000,
            tag_id: None,
        };
        repo.insert_session(&session).unwrap();

        let parsed = vec![
            ("git status".to_string(), 1_700_000_000_000i64, 0i64),
            // A plain (non-extended) history file carries no timestamp.
            ("ls -la".to_string(), 0, 0),
        ];
        let tx = repo.transaction().unwrap();
        import_entries_batch(&repo, &parsed, &session.id, 9_999_999).unwrap();
        tx.commit().unwrap();

        let mut seen = Vec::new();
        repo.stream_export_entries(None, None, |entry| {
            let ctx = entry
                .context
                .clone()
                .unwrap_or_else(|| panic!("no provenance on {}", entry.command));
            assert_eq!(
                ctx.get("import_source").map(String::as_str),
                Some("zsh-history")
            );
            assert_eq!(ctx.get("imported_at").map(String::as_str), Some("9999999"));
            seen.push((
                entry.command,
                ctx.get("timestamp_source").cloned().unwrap_or_default(),
            ));
            Ok(())
        })
        .unwrap();
        seen.sort();
        assert_eq!(
            seen,
            vec![
                ("git status".to_string(), "file".to_string()),
                ("ls -la".to_string(), "synthetic".to_string()),
            ],
            "a time the importer invented must not be presented as one the file carried"
        );

        // …and the diagnostics must see them as stored history, not capture.
        let stats = repo.capture_record_stats(Some(&session.id)).unwrap();
        assert_eq!(stats.imported_records, 2);
        assert_eq!(stats.live_records, 0);
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
            (String::new(), 1_700_000_000_000i64, 0i64), // empty
            ("   ".to_string(), 1_700_000_001_000, 0),   // whitespace-only
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
    fn test_import_jsonl_is_idempotent_by_default() {
        // Re-importing the same export must not double the history.
        let (_dir, repo) = crate::test_utils::test_repo();
        let session = Session::new("host".to_string(), 1_500_000_000_000);
        repo.insert_session(&session).unwrap();
        let entries = vec![
            Entry::new(
                session.id.clone(),
                "git status".into(),
                "/p".into(),
                Some(0),
                1_700_000_000_000,
                1_700_000_000_100,
            ),
            Entry::new(
                session.id,
                "cargo build".into(),
                "/p".into(),
                Some(0),
                1_700_000_001_000,
                1_700_000_001_100,
            ),
        ];
        let jsonl = make_jsonl(&entries);

        let first = import_jsonl_into_repo(&repo, jsonl.as_bytes()).unwrap();
        assert_eq!(first.imported, 2);
        assert_eq!(first.dropped_duplicates, 0);

        // Second import of the same data imports nothing new.
        let second = import_jsonl_into_repo(&repo, jsonl.as_bytes()).unwrap();
        assert_eq!(second.imported, 0);
        assert_eq!(second.dropped_duplicates, 2);

        // allow_duplicates=true keeps every line.
        let third = import_jsonl_into_repo_opts(&repo, jsonl.as_bytes(), true).unwrap();
        assert_eq!(third.imported, 2);
        assert_eq!(third.dropped_duplicates, 0);
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

    // ── bash history parsing ────────────────────────────────────────────

    fn parse_bash_bytes(bytes: &[u8]) -> (Vec<BashRecord>, BashParseStats) {
        let mut out = Vec::new();
        let stats = stream_bash_history(std::io::BufReader::new(bytes), |rec| {
            out.push(rec);
            Ok(())
        })
        .unwrap();
        (out, stats)
    }

    fn parse_bash(text: &str) -> (Vec<BashRecord>, BashParseStats) {
        parse_bash_bytes(text.as_bytes())
    }

    #[test]
    fn bash_timestamped_history_keeps_epoch_headers() {
        let (records, stats) = parse_bash("#1700000000\ngit status\n#1700000060\nls -la\n");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].command, "git status");
        assert_eq!(records[0].timestamp_ms, Some(1_700_000_000_000));
        assert_eq!(records[1].command, "ls -la");
        assert_eq!(records[1].timestamp_ms, Some(1_700_000_060_000));
        assert!(stats.timestamped, "file carries #<epoch> headers");
        assert_eq!(stats.malformed, 0);
    }

    #[test]
    fn bash_plain_history_has_no_timestamps() {
        let (records, stats) = parse_bash("echo hello\nls\n");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].command, "echo hello");
        assert_eq!(records[0].timestamp_ms, None);
        assert_eq!(records[1].timestamp_ms, None);
        assert!(!stats.timestamped);
    }

    #[test]
    fn bash_multiline_record_is_joined_between_epoch_headers() {
        // With HISTTIMEFORMAT set, the `#<epoch>` line is the record boundary,
        // so every line until the next header belongs to one command.
        let text = "#1700000000\nfor i in 1 2 3; do\n  echo $i\ndone\n#1700000060\nls\n";
        let (records, stats) = parse_bash(text);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].command, "for i in 1 2 3; do\n  echo $i\ndone");
        assert_eq!(records[0].timestamp_ms, Some(1_700_000_000_000));
        assert_eq!(records[1].command, "ls");
        assert_eq!(stats.malformed, 0);
    }

    #[test]
    fn bash_plain_history_cannot_reconstruct_multiline_boundaries() {
        // Documented limitation: a plain file has no record boundaries, so the
        // three physical lines of one loop become three records. We must not
        // guess boundaries that the file does not contain.
        let (records, _) = parse_bash("for i in 1 2 3; do\n  echo $i\ndone\n");
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].command, "for i in 1 2 3; do");
        assert_eq!(records[2].command, "done");
    }

    #[test]
    fn bash_empty_file_yields_no_records() {
        let (records, stats) = parse_bash("");
        assert!(records.is_empty());
        assert_eq!(stats.malformed, 0);
        assert!(!stats.timestamped);
    }

    #[test]
    fn bash_header_without_command_is_malformed() {
        let (records, stats) = parse_bash("#1700000000\n#1700000060\nls\n");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].command, "ls");
        assert_eq!(records[0].timestamp_ms, Some(1_700_000_060_000));
        assert_eq!(stats.malformed, 1, "the empty first record is malformed");
    }

    #[test]
    fn bash_trailing_header_at_eof_is_malformed() {
        let (records, stats) = parse_bash("#1700000000\nls\n#1700000060\n");
        assert_eq!(records.len(), 1);
        assert_eq!(stats.malformed, 1);
    }

    #[test]
    fn bash_out_of_range_epoch_header_is_malformed_not_a_timestamp() {
        let (records, stats) = parse_bash("#99999999999999999999\nls\n");
        assert_eq!(stats.malformed, 1);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].command, "ls");
        assert_eq!(
            records[0].timestamp_ms, None,
            "never invent a timestamp for a broken header"
        );
    }

    #[test]
    fn bash_comment_command_after_a_header_is_not_a_timestamp() {
        let (records, stats) = parse_bash("#1700000000\n# deploy notes\n");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].command, "# deploy notes");
        assert_eq!(records[0].timestamp_ms, Some(1_700_000_000_000));
        assert_eq!(stats.malformed, 0);
    }

    #[test]
    fn bash_comment_commands_are_kept_as_commands() {
        // `# deploy notes` is a real command bash records, not a timestamp.
        let (records, stats) = parse_bash("# deploy notes\nls\n");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].command, "# deploy notes");
        assert_eq!(stats.malformed, 0);
    }

    #[test]
    fn bash_non_utf8_bytes_are_replaced_and_counted() {
        let mut bytes = b"#1700000000\necho ".to_vec();
        bytes.push(0xFF);
        bytes.extend_from_slice(b"\n");
        let (records, stats) = parse_bash_bytes(&bytes);
        assert_eq!(records.len(), 1);
        assert!(
            records[0].command.contains('\u{FFFD}'),
            "invalid bytes become U+FFFD: {:?}",
            records[0].command
        );
        assert_eq!(stats.lossy_lines, 1);
    }

    // ── bash history import into a repository ───────────────────────────

    fn import_bash(
        repo: &Repository,
        text: &str,
        config: &crate::config::Config,
        session_id: &str,
        dry_run: bool,
    ) -> BashImportStats {
        import_bash_history(
            repo,
            std::io::BufReader::new(text.as_bytes()),
            config,
            &BashImportOptions {
                session_id,
                hostname: "test-host",
                imported_at_ms: 1_800_000_000_000,
                dry_run,
            },
        )
        .unwrap()
    }

    #[test]
    fn bash_import_preserves_timestamps_and_marks_unknown_metadata() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let stats = import_bash(&repo, "#1700000000\ngit status\n", &cfg, "s-ts", false);

        assert_eq!(stats.imported, 1);
        assert_eq!(stats.with_timestamp, 1);
        assert_eq!(stats.without_timestamp, 0);

        let entries = repo.get_entries_by_session("s-ts").unwrap();
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.command, "git status");
        assert_eq!(e.started_at, 1_700_000_000_000, "file timestamp preserved");
        assert_eq!(e.exit_code, None, "never fabricate a successful exit");
        assert_eq!(e.cwd, "", "directory is unknown in bash history");
        assert_eq!(e.duration_ms, 0);
        assert_eq!(e.executor_type.as_deref(), Some("unknown"));
        let ctx = e.context.as_ref().expect("provenance context");
        assert_eq!(
            ctx.get("import_source").map(String::as_str),
            Some("bash-history")
        );
        assert_eq!(
            ctx.get("timestamp_source").map(String::as_str),
            Some("file")
        );
    }

    #[test]
    fn bash_import_marks_missing_timestamps_as_synthetic() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let stats = import_bash(&repo, "echo hello\nls\n", &cfg, "s-plain", false);

        assert_eq!(stats.imported, 2);
        assert_eq!(stats.without_timestamp, 2);

        let entries = repo.get_entries_by_session("s-plain").unwrap();
        assert_eq!(entries.len(), 2);
        for e in &entries {
            assert!(
                e.started_at < SYNTHETIC_TS_CEILING_MS,
                "synthetic sentinel time, not a fabricated recent time: {}",
                e.started_at
            );
            let ctx = e.context.as_ref().expect("provenance context");
            assert_eq!(
                ctx.get("timestamp_source").map(String::as_str),
                Some("synthetic")
            );
        }
    }

    #[test]
    fn bash_import_preserves_repeated_executions() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        // Same command three times: twice inside one second, once later.
        let text = "#1700000000\nls\n#1700000000\nls\n#1700000100\nls\n";
        let stats = import_bash(&repo, text, &cfg, "s-rep", false);
        assert_eq!(stats.imported, 3, "repeated executions are not collapsed");
        assert_eq!(repo.count_entries().unwrap(), 3);
    }

    #[test]
    fn bash_import_preserves_repeated_commands_in_plain_files() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let stats = import_bash(&repo, "ls\ncd /tmp\nls\n", &cfg, "s-rep2", false);
        assert_eq!(stats.imported, 3);
    }

    #[test]
    fn bash_second_identical_import_adds_nothing() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let text = "#1700000000\ngit status\n#1700000000\ngit status\n#1700000100\nls\n";

        let first = import_bash(&repo, text, &cfg, "s-first", false);
        assert_eq!(first.imported, 3);

        let second = import_bash(&repo, text, &cfg, "s-second", false);
        assert_eq!(second.imported, 0, "re-import must be idempotent");
        assert_eq!(second.duplicates, 3);
        assert_eq!(repo.count_entries().unwrap(), 3);
    }

    #[test]
    fn bash_second_identical_plain_import_adds_nothing() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let text = "ls\ncd /tmp\nls\n";

        assert_eq!(import_bash(&repo, text, &cfg, "p1", false).imported, 3);
        let second = import_bash(&repo, text, &cfg, "p2", false);
        assert_eq!(second.imported, 0);
        assert_eq!(second.duplicates, 3);
        assert_eq!(repo.count_entries().unwrap(), 3);
    }

    #[test]
    fn bash_import_of_empty_file_writes_nothing() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let stats = import_bash(&repo, "", &cfg, "s-empty", false);
        assert_eq!(stats.imported, 0);
        assert_eq!(repo.count_entries().unwrap(), 0);
        assert!(
            repo.get_session("s-empty").unwrap().is_none(),
            "no import session for a file with nothing to import"
        );
    }

    #[test]
    fn bash_import_counts_malformed_records_without_aborting() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let text = "#1700000000\n#1700000060\ngit status\n#999999999999999999999\nls\n";
        let stats = import_bash(&repo, text, &cfg, "s-bad", false);
        assert_eq!(
            stats.malformed, 2,
            "empty first record + unparseable epoch header"
        );
        assert_eq!(stats.imported, 2, "the two real commands still land");
    }

    #[test]
    fn bash_import_survives_non_utf8_bytes() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let mut bytes = b"#1700000000\necho ".to_vec();
        bytes.push(0xFE);
        bytes.extend_from_slice(b"\n#1700000060\nls\n");
        let stats = import_bash_history(
            &repo,
            std::io::BufReader::new(&bytes[..]),
            &cfg,
            &BashImportOptions {
                session_id: "s-bin",
                hostname: "test-host",
                imported_at_ms: 1_800_000_000_000,
                dry_run: false,
            },
        )
        .unwrap();
        assert_eq!(stats.imported, 2);
        assert_eq!(stats.lossy_lines, 1);
    }

    #[test]
    fn bash_import_redacts_secrets_and_counts_them() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let text = format!("#1700000000\nexport GITHUB_TOKEN={secret}\n");
        let stats = import_bash(&repo, &text, &cfg, "s-secret", false);

        assert_eq!(stats.imported, 1);
        assert_eq!(stats.redacted, 1);
        let entries = repo.get_entries_by_session("s-secret").unwrap();
        assert!(
            !entries[0].command.contains(secret),
            "secret must never reach storage: {}",
            entries[0].command
        );
        assert!(entries[0].command.contains("REDACTED"));
    }

    #[test]
    fn bash_import_honours_configured_exclusions() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config {
            exclusions: vec!["^vault ".to_string()],
            ..Default::default()
        };
        let text = "#1700000000\nvault login\n#1700000060\nls\n";
        let stats = import_bash(&repo, text, &cfg, "s-excl", false);

        assert_eq!(stats.excluded, 1);
        assert_eq!(stats.imported, 1);
        let entries = repo.get_entries_by_session("s-excl").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].command, "ls");
    }

    #[test]
    fn bash_import_ignores_blank_and_space_prefixed_commands() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let stats = import_bash(&repo, " secret-thing\n\nls\n", &cfg, "s-ign", false);
        assert_eq!(stats.ignored, 1, "HISTCONTROL=ignorespace style entries");
        assert_eq!(stats.imported, 1);
    }

    #[test]
    fn bash_import_dry_run_writes_nothing_but_counts() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let text = "#1700000000\ngit status\n#1700000060\nls\n";
        let stats = import_bash(&repo, text, &cfg, "s-dry", true);

        assert_eq!(stats.imported, 2, "dry run reports what would be imported");
        assert_eq!(repo.count_entries().unwrap(), 0, "nothing written");
        assert!(repo.get_session("s-dry").unwrap().is_none());
        assert!(!stats.samples.is_empty(), "dry run collects a preview");
    }

    #[test]
    fn bash_dry_run_samples_are_redacted() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let text = format!("#1700000000\nexport GITHUB_TOKEN={secret}\n");
        let stats = import_bash(&repo, &text, &cfg, "s-dry2", true);
        assert!(
            !stats.samples.iter().any(|(cmd, _)| cmd.contains(secret)),
            "dry-run preview must not print secrets"
        );
    }

    #[test]
    fn bash_dry_run_reports_duplicates_already_present() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let text = "#1700000000\ngit status\n";
        assert_eq!(import_bash(&repo, text, &cfg, "d1", false).imported, 1);

        let dry = import_bash(&repo, text, &cfg, "d2", true);
        assert_eq!(dry.imported, 0);
        assert_eq!(dry.duplicates, 1);
    }
}
