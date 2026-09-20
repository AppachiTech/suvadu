//! Read-only migration of an existing Atuin history database into Suvadu.
//!
//! # Why the database and not an export
//!
//! Atuin (checked against 18.22.0) has no export command. The closest thing is
//! `atuin history list --format "…"`, whose variables are `{command}`,
//! `{directory}`, `{duration}`, `{user}`, `{host}`, `{author}`, `{intent}`,
//! `{exit}`, `{time}`, `{session}` and `{uuid}`. That output is lossy in ways
//! we cannot repair: `{duration}` is humanised ("3s"), `{time}` is rendered in
//! a configured timezone at second resolution, `{command}` is trimmed, and
//! even with `--print0` there is no escaping *inside* a record, so a command
//! containing the field separator is ambiguous. Nanosecond timestamps and raw
//! durations are simply not reachable through it.
//!
//! So the source contract is the database itself, opened **read-only**, and
//! only for the schema versions we have explicitly tested. Atuin's schema is
//! tracked by `sqlx` in `_sqlx_migrations`; an unrecognised migration is
//! rejected rather than guessed at.
//!
//! # What Atuin stores
//!
//! ```text
//! history(id text, timestamp integer, duration integer, exit integer,
//!         command text, cwd text, session text, hostname text,
//!         deleted_at integer, author text, intent text, shell text,
//!         author_kind integer)
//! ```
//!
//! `timestamp`, `duration` and `deleted_at` are **nanoseconds**. `hostname` is
//! really `host:user`. `exit`/`duration` of `-1` is Atuin's "not known" (a
//! command that never finished, or a row Atuin itself imported), and `cwd` of
//! `"unknown"` is the placeholder Atuin's own importers write. `author_kind`
//! is `1` (human) or `2` (agent); absent means nobody stated one.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use crate::models::Entry;
use crate::repository::Repository;

use super::{apply_recording_policy, next_occurrence, print_dry_run_samples, RecordingPolicy};

/// Every `_sqlx_migrations` version this importer has been tested against —
/// the union of the migration sets shipped by Atuin 18.0.0 … 18.22.0. A
/// database containing anything else is rejected, not guessed at.
const KNOWN_MIGRATIONS: &[i64] = &[
    20_210_422_143_411, // create_history      (18.0+)
    20_220_505_083_406, // create-events       (18.0+)
    20_220_806_155_627, // interactive_search_index
    20_230_315_220_114, // drop-events
    20_230_319_185_725, // deleted_at
    20_260_224_000_100, // history_author_intent
    20_260_709_214_605, // shell
    20_260_723_000_000, // active_history_index
    20_260_723_000_001, // filtered_history_indexes
    20_260_723_000_002, // hostname_index
    20_260_723_000_003, // drop_command_index
    20_260_818_000_000, // history_author_kind
];

/// `create_history` — without it this is not an Atuin history database.
const MIGRATION_CREATE_HISTORY: i64 = 20_210_422_143_411;
/// `deleted_at` — present from Atuin 18.0.0 onwards. Older databases have no
/// tombstone column, so we cannot tell a deleted row from a live one.
const MIGRATION_DELETED_AT: i64 = 20_230_319_185_725;

/// Largest Unix timestamp (seconds) we accept from the source — 2100-01-01Z.
/// Beyond that the column holds something that is not a time.
const EPOCH_MAX_SECS: i64 = 4_102_444_800;
const NANOS_PER_MS: i64 = 1_000_000;

/// Provenance marker written into every imported entry's `context`.
pub const IMPORT_SOURCE: &str = "atuin-db";

/// What a supported Atuin database looks like: its newest applied migration
/// plus the optional columns that migration set implies.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // one flag per optional Atuin column
pub struct AtuinSchema {
    /// Newest applied `_sqlx_migrations` version.
    pub version: i64,
    pub has_author: bool,
    pub has_intent: bool,
    pub has_shell: bool,
    pub has_author_kind: bool,
}

impl AtuinSchema {
    /// Human-readable summary of the optional metadata this database carries.
    fn extras(&self) -> String {
        let mut extras = Vec::new();
        if self.has_author {
            extras.push("author");
        }
        if self.has_intent {
            extras.push("intent");
        }
        if self.has_shell {
            extras.push("shell");
        }
        if self.has_author_kind {
            extras.push("author_kind");
        }
        if extras.is_empty() {
            "no author/intent/shell columns".to_string()
        } else {
            extras.join(", ")
        }
    }
}

/// One row read from Atuin's `history` table, with Atuin's own "unknown"
/// conventions already resolved to `None`. Nothing here is invented: a value
/// the source did not record stays absent all the way into storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtuinRow {
    /// Atuin's row UUID — kept as provenance so a row can be traced back.
    pub id: String,
    /// Execution time, nanoseconds since the Unix epoch.
    pub timestamp_ns: i64,
    /// Wall-clock duration in nanoseconds, or `None` when Atuin stored `-1`
    /// (command never finished, or the row came from Atuin's own importer).
    pub duration_ns: Option<i64>,
    /// Exit status, or `None` for Atuin's `-1` ("not known").
    pub exit: Option<i64>,
    pub command: String,
    /// Working directory, or `None` for an empty value or Atuin's `"unknown"`.
    pub cwd: Option<String>,
    /// Atuin session id (empty when the row predates sessions).
    pub session: String,
    /// Host half of Atuin's `hostname` column.
    pub host: String,
    /// User half of Atuin's `hostname` column, when it has one.
    pub user: Option<String>,
    pub author: Option<String>,
    pub intent: Option<String>,
    pub shell: Option<String>,
    /// `1` = human, `2` = agent, anything else = not stated.
    pub author_kind: Option<i64>,
}

/// What the read pass saw in the source database.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct AtuinReadStats {
    /// Rows handed to the callback.
    pub rows: u64,
    /// Rows whose required columns could not be read as the schema promises.
    pub malformed: u64,
    /// Rows Atuin has soft-deleted (`deleted_at` set) — never imported.
    pub deleted: u64,
}

/// Outcome of an Atuin import (or of a dry run).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct AtuinImportStats {
    /// Usable rows recovered from the source (before policy filtering).
    pub parsed: u64,
    /// Entries written (or, in a dry run, that would be written).
    pub imported: u64,
    /// Rows already present in the destination database.
    pub duplicates: u64,
    /// Rows dropped by the configured exclusion patterns.
    pub excluded: u64,
    /// Rows whose text was changed by redaction before storage.
    pub redacted: u64,
    /// Blank or space-prefixed rows.
    pub ignored: u64,
    /// Rows skipped because a required column was unreadable.
    pub malformed: u64,
    /// Rows Atuin has soft-deleted.
    pub deleted: u64,
    /// Rows whose exit status Atuin did not know.
    pub unknown_exit: u64,
    /// Rows whose duration Atuin did not know.
    pub unknown_duration: u64,
    /// Rows whose directory Atuin did not know.
    pub unknown_cwd: u64,
    /// Rows where nobody stated who ran the command.
    pub unknown_executor: u64,
    /// Atuin sessions carried over (one Suvadu session each).
    pub sessions: u64,
    /// Up to ten redacted samples for the dry-run preview.
    pub samples: Vec<(String, Option<i64>)>,
}

/// Inputs for [`import_atuin_history`] that don't come from the source.
pub struct AtuinImportOptions<'a> {
    /// Hostname recorded on a session whose source row carried none.
    pub hostname_fallback: &'a str,
    /// Wall-clock time of this import run, recorded as provenance.
    pub imported_at_ms: i64,
    /// Count everything, write nothing.
    pub dry_run: bool,
}

// ── reading the source ──────────────────────────────────────────────────

/// Open an Atuin database **read-only**.
///
/// The connection is opened with `SQLITE_OPEN_READ_ONLY` and `query_only`, so
/// `SQLite` itself enforces that we never write — and normal WAL/locking rules
/// still apply, which is why this is not a file copy and never sets
/// `immutable=1`: either would risk reading a torn snapshot while Atuin (or
/// its daemon) is mid-transaction.
pub fn open_source(path: &Path) -> Result<Connection, Box<dyn std::error::Error>> {
    if !path.exists() {
        return Err(format!(
            "No Atuin database at {}.\n\
             Atuin usually keeps it at ~/.local/share/atuin/history.db — pass that path:\n\
             \x20 suv import --from atuin-db ~/.local/share/atuin/history.db",
            path.display()
        )
        .into());
    }

    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| read_only_open_error(path, &e))?;
    conn.pragma_update(None, "query_only", true)
        .map_err(|e| read_only_open_error(path, &e))?;
    Ok(conn)
}

/// Turn a failed read-only open into advice. The common cause is a write-ahead
/// log that still needs a checkpoint: `SQLite` cannot build the shared-memory
/// index for a read-only connection unless the `-shm` file is already there.
fn read_only_open_error(path: &Path, err: &rusqlite::Error) -> String {
    format!(
        "Could not open {} read-only: {err}\n\
         Suvadu never writes to the Atuin database, so it cannot recover a\n\
         write-ahead log on its own. Let Atuin check-point it first — run any\n\
         Atuin command (for example `atuin history list -n 1`) and stop the\n\
         Atuin daemon if you run one, then try this import again.",
        path.display()
    )
}

/// Read and validate the source schema, or explain why we will not touch it.
pub fn read_schema(conn: &Connection) -> Result<AtuinSchema, Box<dyn std::error::Error>> {
    if !table_exists(conn, "history")? {
        return Err(format!(
            "{} has no `history` table, so it is not an Atuin history database.\n\
             Atuin keeps its history at ~/.local/share/atuin/history.db.",
            db_label(conn)
        )
        .into());
    }
    if !table_exists(conn, "_sqlx_migrations")? {
        return Err(
            "This database has no `_sqlx_migrations` table, so its Atuin schema version \
                    cannot be established. Suvadu will not guess at an unversioned layout.\n\
                    Open it with Atuin 18 or newer once (Atuin will record its migrations), then \
                    re-run this import."
                .into(),
        );
    }

    let mut stmt =
        conn.prepare("SELECT version, success FROM _sqlx_migrations ORDER BY version")?;
    let mut applied: Vec<i64> = Vec::new();
    let mut unknown: Vec<i64> = Vec::new();
    let mut failed: Vec<i64> = Vec::new();
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let version: i64 = row.get(0)?;
        let success: bool = row.get::<_, Option<bool>>(1)?.unwrap_or(false);
        if !success {
            failed.push(version);
            continue;
        }
        if KNOWN_MIGRATIONS.contains(&version) {
            applied.push(version);
        } else {
            unknown.push(version);
        }
    }

    if !failed.is_empty() {
        return Err(format!(
            "This Atuin database has migration(s) {} recorded as failed, so its layout is \
             unknown.\nRun Atuin once so it can finish migrating, then re-run this import.",
            join_versions(&failed)
        )
        .into());
    }
    if !unknown.is_empty() {
        return Err(format!(
            "This Atuin database was written by a newer Atuin than this importer has been tested \
             against\n(unrecognised history schema migration {}).\n\
             Tested: Atuin 18.0.0 – 18.22.0 (schema {} – {}).\n\
             Next step: run `suv update` and try again with a build that lists your Atuin \
             release,\nor report the migration id above at \
             https://github.com/AppachiTech/suvadu/issues.\n\
             Nothing was read from the database and nothing was written.",
            join_versions(&unknown),
            MIGRATION_CREATE_HISTORY,
            KNOWN_MIGRATIONS[KNOWN_MIGRATIONS.len() - 1]
        )
        .into());
    }
    if !applied.contains(&MIGRATION_CREATE_HISTORY) {
        return Err(
            "This database has a `history` table but no Atuin `create_history` migration, so it \
             is not an Atuin history database Suvadu can read."
                .into(),
        );
    }
    if !applied.contains(&MIGRATION_DELETED_AT) {
        return Err(format!(
            "This Atuin database predates Atuin 18.0.0 (no `deleted_at` migration), so deleted \
             commands cannot be told apart from live ones.\n\
             Next step: upgrade Atuin to 18.x and run it once — it migrates the database in \
             place — then re-run:\n\
             \x20 suv import --from atuin-db {}",
            db_label(conn)
        )
        .into());
    }

    Ok(AtuinSchema {
        version: applied.iter().copied().max().unwrap_or(0),
        has_author: column_exists(conn, "author")?,
        has_intent: column_exists(conn, "intent")?,
        has_shell: column_exists(conn, "shell")?,
        has_author_kind: column_exists(conn, "author_kind")?,
    })
}

fn table_exists(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    let mut stmt =
        conn.prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1")?;
    stmt.exists([name])
}

fn column_exists(conn: &Connection, column: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare("SELECT 1 FROM pragma_table_info('history') WHERE name = ?1")?;
    stmt.exists([column])
}

/// Path of the attached main database, for error messages.
fn db_label(conn: &Connection) -> String {
    conn.path().map_or_else(
        || "the Atuin database".to_string(),
        std::string::ToString::to_string,
    )
}

fn join_versions(versions: &[i64]) -> String {
    versions
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Stream every live row of the source `history` table, oldest first.
///
/// The whole pass runs inside one deferred read transaction, so it sees a
/// single consistent snapshot even if Atuin keeps recording while we read.
/// Rows are ordered by `(timestamp, id)` so that two runs over the same
/// database hand rows to the callback in exactly the same order — which is
/// what makes the derived timestamps, and therefore the import, idempotent.
pub fn stream_atuin_history<F>(
    conn: &Connection,
    schema: &AtuinSchema,
    mut on_row: F,
) -> Result<AtuinReadStats, Box<dyn std::error::Error>>
where
    F: FnMut(AtuinRow) -> Result<(), Box<dyn std::error::Error>>,
{
    let mut columns =
        String::from("id, timestamp, duration, exit, command, cwd, session, hostname, deleted_at");
    for (present, name) in [
        (schema.has_author, "author"),
        (schema.has_intent, "intent"),
        (schema.has_shell, "shell"),
        (schema.has_author_kind, "author_kind"),
    ] {
        if present {
            columns.push_str(", ");
            columns.push_str(name);
        }
    }

    // A read transaction pins one snapshot for the whole pass; dropping it
    // rolls back, which on a read-only connection is simply "release".
    let tx = conn.unchecked_transaction()?;
    let mut stats = AtuinReadStats::default();
    {
        let sql = format!("SELECT {columns} FROM history ORDER BY timestamp ASC, id ASC");
        let mut stmt = tx.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            // A tombstone we cannot read is still a tombstone: skip it.
            if !matches!(row.get::<_, Option<i64>>("deleted_at"), Ok(None)) {
                stats.deleted += 1;
                continue;
            }
            match parse_row(row, schema) {
                Some(parsed) => {
                    stats.rows += 1;
                    on_row(parsed)?;
                }
                None => stats.malformed += 1,
            }
        }
    }
    drop(tx);
    Ok(stats)
}

/// Convert one `SQLite` row into an [`AtuinRow`], or `None` when a column the
/// schema declares `not null` holds something we cannot trust.
fn parse_row(row: &rusqlite::Row<'_>, schema: &AtuinSchema) -> Option<AtuinRow> {
    let timestamp_ns: i64 = row.get("timestamp").ok()?;
    if timestamp_ns <= 0 || timestamp_ns / 1_000_000_000 > EPOCH_MAX_SECS {
        return None;
    }
    let command: String = row.get("command").ok()?;
    let id: String = row.get::<_, Option<String>>("id").ok().flatten()?;

    let duration_ns = row
        .get::<_, Option<i64>>("duration")
        .ok()
        .flatten()
        .filter(|d| *d >= 0);
    let exit = row
        .get::<_, Option<i64>>("exit")
        .ok()
        .flatten()
        .filter(|e| *e >= 0);
    let cwd = row
        .get::<_, Option<String>>("cwd")
        .ok()
        .flatten()
        // "unknown" is the literal placeholder Atuin's own importers write.
        .filter(|c| !c.trim().is_empty() && c != "unknown");
    let session = row
        .get::<_, Option<String>>("session")
        .ok()
        .flatten()
        .unwrap_or_default();
    let hostname = row
        .get::<_, Option<String>>("hostname")
        .ok()
        .flatten()
        .unwrap_or_default();
    let (host, user) = split_hostname(&hostname);

    let text = |name: &str, present: bool| -> Option<String> {
        present
            .then(|| row.get::<_, Option<String>>(name).ok().flatten())
            .flatten()
            .filter(|s| !s.trim().is_empty())
    };

    Some(AtuinRow {
        id,
        timestamp_ns,
        duration_ns,
        exit,
        command,
        cwd,
        session,
        host,
        user,
        author: text("author", schema.has_author),
        intent: text("intent", schema.has_intent),
        shell: text("shell", schema.has_shell),
        author_kind: schema
            .has_author_kind
            .then(|| row.get::<_, Option<i64>>("author_kind").ok().flatten())
            .flatten(),
    })
}

/// Atuin's `hostname` column is really `host:user`. Rows written before that
/// format existed hold a bare hostname and no user.
fn split_hostname(hostname: &str) -> (String, Option<String>) {
    hostname.split_once(':').map_or_else(
        || (hostname.to_string(), None),
        |(host, user)| {
            let user = user.trim();
            (
                host.to_string(),
                // Atuin's own placeholder for "no user recorded".
                (!user.is_empty() && user != "unknown-user").then(|| user.to_string()),
            )
        },
    )
}

// ── writing into Suvadu ─────────────────────────────────────────────────

/// Suvadu session id for an Atuin session, so the grouping survives the move.
fn session_id_for(atuin_session: &str) -> String {
    if atuin_session.trim().is_empty() {
        "atuin-unknown-session".to_string()
    } else {
        format!("atuin-{atuin_session}")
    }
}

/// Map `author_kind` to a Suvadu `executor_type`. An unstated or unrecognised
/// kind is `"unknown"` — Atuin's own UI guesses "agent" from known author
/// names, and we deliberately do not.
const fn executor_type_for(author_kind: Option<i64>) -> &'static str {
    match author_kind {
        Some(1) => "human",
        Some(2) => "agent",
        _ => "unknown",
    }
}

/// Build the stored entry for one Atuin row.
fn atuin_entry(
    opts: &AtuinImportOptions<'_>,
    row: &AtuinRow,
    command: String,
    started_at: i64,
    duration_ms: Option<i64>,
) -> Entry {
    let exit_code = row.exit.and_then(|e| i32::try_from(e).ok());
    let mut entry = Entry::new(
        session_id_for(&row.session),
        command,
        row.cwd.clone().unwrap_or_default(),
        exit_code,
        started_at,
        started_at + duration_ms.unwrap_or(0),
    );
    entry.executor_type = Some(executor_type_for(row.author_kind).to_string());
    entry.executor.clone_from(&row.author);

    let mut unknown_fields = Vec::new();
    if exit_code.is_none() {
        unknown_fields.push("exit_code");
    }
    if duration_ms.is_none() {
        unknown_fields.push("duration_ms");
    }
    if row.cwd.is_none() {
        unknown_fields.push("cwd");
    }
    if row.author_kind.is_none() {
        unknown_fields.push("executor");
    }

    let mut context = HashMap::new();
    context.insert("import_source".to_string(), IMPORT_SOURCE.to_string());
    context.insert("imported_at".to_string(), opts.imported_at_ms.to_string());
    // Every Atuin row carries a real execution time; none is ever synthesised.
    context.insert("timestamp_source".to_string(), "file".to_string());
    context.insert("atuin_id".to_string(), row.id.clone());
    if !row.host.is_empty() {
        context.insert("atuin_host".to_string(), row.host.clone());
    }
    for (key, value) in [
        ("atuin_user", row.user.as_ref()),
        ("atuin_author", row.author.as_ref()),
        ("atuin_intent", row.intent.as_ref()),
        ("atuin_shell", row.shell.as_ref()),
    ] {
        if let Some(value) = value {
            context.insert(key.to_string(), value.clone());
        }
    }
    if !unknown_fields.is_empty() {
        context.insert("unknown_fields".to_string(), unknown_fields.join(","));
    }
    entry.context = Some(context);
    entry
}

/// Import an Atuin history database into `repo`.
///
/// Field mapping (nothing outside this table is carried, and nothing missing
/// is invented):
///
/// | Atuin | Suvadu | conversion |
/// |---|---|---|
/// | `timestamp` (ns) | `started_at` (ms) | truncated to milliseconds |
/// | `duration` (ns) | `duration_ms`, `ended_at` | truncated; `-1` → unknown (`0`) |
/// | `exit` | `exit_code` | `-1` → `NULL`, never a fabricated `0` |
/// | `command` | `command` | verbatim, including newlines |
/// | `cwd` | `cwd` | `""`/`"unknown"` → empty (unknown) |
/// | `session` | `session_id` | `atuin-<session>` |
/// | `hostname` (`host:user`) | session hostname + `atuin_user` context | split |
/// | `author` | `executor` + `atuin_author` context | verbatim |
/// | `author_kind` | `executor_type` | `1`→human, `2`→agent, else unknown |
/// | `id`, `intent`, `shell` | `context` | provenance only |
/// | `deleted_at` | — | row skipped |
///
/// **Idempotency.** `started_at` is derived deterministically from the row: the
/// source timestamp in milliseconds, plus the number of times that exact
/// (redacted) command has already appeared at that millisecond in this pass —
/// Atuin keeps nanoseconds, so two runs inside one millisecond would otherwise
/// collide. Repeated executions therefore all survive with distinct times,
/// while re-running the import produces the same times and skips every row as
/// a duplicate. Existing Suvadu rows are never modified or removed.
pub fn import_atuin_history(
    repo: &Repository,
    conn: &Connection,
    schema: &AtuinSchema,
    config: &crate::config::Config,
    opts: &AtuinImportOptions<'_>,
) -> Result<AtuinImportStats, Box<dyn std::error::Error>> {
    const BATCH_SIZE: u64 = 5_000;
    const MAX_SAMPLES: usize = 10;

    let exclusions = (!config.exclusions.is_empty())
        .then(|| crate::util::compile_exclusions(&config.exclusions));

    let mut stats = AtuinImportStats::default();
    let mut occurrences: HashMap<(String, Option<i64>), i64> = HashMap::new();
    let mut sessions: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut batch_count = 0u64;

    let tx = if opts.dry_run {
        None
    } else {
        Some(repo.transaction()?)
    };

    let read_stats = stream_atuin_history(conn, schema, |row| {
        let command = match apply_recording_policy(&row.command, config, exclusions.as_deref()) {
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

        let source_ms = row.timestamp_ns.div_euclid(NANOS_PER_MS);
        let ordinal = next_occurrence(&mut occurrences, (command.clone(), Some(source_ms)));
        let started_at = source_ms.saturating_add(ordinal);
        let duration_ms = row.duration_ns.map(|d| d.div_euclid(NANOS_PER_MS));

        if row.exit.is_none() {
            stats.unknown_exit += 1;
        }
        if duration_ms.is_none() {
            stats.unknown_duration += 1;
        }
        if row.cwd.is_none() {
            stats.unknown_cwd += 1;
        }
        if row.author_kind.is_none() {
            stats.unknown_executor += 1;
        }

        if repo.entry_exists(&command, started_at)? {
            stats.duplicates += 1;
            return Ok(());
        }

        if stats.samples.len() < MAX_SAMPLES {
            stats.samples.push((command.clone(), Some(source_ms)));
        }

        if opts.dry_run {
            stats.imported += 1;
            if sessions.insert(session_id_for(&row.session)) {
                stats.sessions += 1;
            }
            return Ok(());
        }

        let session_id = session_id_for(&row.session);
        if sessions.insert(session_id.clone()) {
            let hostname = if row.host.is_empty() {
                opts.hostname_fallback
            } else {
                &row.host
            };
            repo.insert_session_if_missing(&session_id, hostname, started_at)?;
            stats.sessions += 1;
        }

        repo.insert_entry(&atuin_entry(opts, &row, command, started_at, duration_ms))?;
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

    stats.parsed = read_stats.rows;
    stats.malformed = read_stats.malformed;
    stats.deleted = read_stats.deleted;

    if let Some(tx) = tx {
        tx.commit()?;
    }
    Ok(stats)
}

// ── CLI ─────────────────────────────────────────────────────────────────

/// `suv import --from atuin-db <file>`
pub fn handle_import_atuin_db(
    file: &str,
    dry_run: bool,
    no_backup: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = std::path::PathBuf::from(file);
    let source = open_source(&path)?;
    let schema = read_schema(&source)?;

    println!("Atuin database: {} (read-only snapshot)", path.display());
    println!("  Atuin schema {} — {}", schema.version, schema.extras());

    // Fingerprint the source before we read it, and again afterwards, so the
    // report can state — not assume — that the import left Atuin alone.
    let before = source_fingerprint(&path);

    let config = crate::config::load_config()?;
    let repo = Repository::init()?;
    let now = chrono::Utc::now().timestamp_millis();

    let backup = if dry_run || no_backup {
        None
    } else {
        Some(back_up_destination(&repo)?)
    };

    let hostname = hostname::get()?.to_string_lossy().to_string();
    let stats = import_atuin_history(
        &repo,
        &source,
        &schema,
        &config,
        &AtuinImportOptions {
            hostname_fallback: &hostname,
            imported_at_ms: now,
            dry_run,
        },
    )?;
    drop(source);

    println!("Read {} history row(s) from the database.", stats.parsed);
    println!(
        "Note: Atuin's nanosecond times are truncated to milliseconds, and its row id,\n\
         \x20     intent and shell are kept in each entry's context rather than as Suvadu\n\
         \x20     columns. Rows Atuin never finished (exit/duration -1) are stored as\n\
         \x20     unknown, never as a success."
    );

    if dry_run {
        print_dry_run_samples(&stats.samples, stats.imported);
        println!(
            "\nDry run complete. {} entry(ies) would be imported.",
            stats.imported
        );
        print_atuin_import_counts(&stats);
        println!("  No backup taken and nothing written — this was a dry run.");
        return Ok(());
    }

    println!("\n✓ Import complete:");
    println!("  Imported: {}", stats.imported);
    print_atuin_import_counts(&stats);

    let verified = repo.count_entries_by_import(IMPORT_SOURCE, Some(now))?;
    if verified == i64::try_from(stats.imported).unwrap_or(i64::MAX) {
        println!(
            "  Verified: {verified} entry(ies) carry this import's provenance in the database."
        );
    } else {
        println!(
            "  WARNING: expected {} imported entry(ies) in the database but found {verified}.",
            stats.imported
        );
    }

    match (&before, source_fingerprint(&path)) {
        (Some(before), Some(after)) if *before == after => {
            println!("  Source: unchanged (SHA-256 of the Atuin database and its WAL match).");
        }
        (Some(_), Some(_)) => {
            println!(
                "  Source: the Atuin database changed while we read it. Suvadu only ever opened\n\
                 \x20         it read-only, so Atuin itself recorded something — re-run the import\n\
                 \x20         to pick up anything added after the snapshot."
            );
        }
        _ => println!("  Source: could not be re-read to confirm it is unchanged."),
    }

    if let Some(backup) = backup {
        println!("  Backup: {}", backup.display());
        println!(
            "  Rollback: restore that backup over the Suvadu database (close other `suv`\n\
             \x20           processes first):\n\
             \x20             cp \"{}\" \"{}\"",
            backup.display(),
            crate::db::get_db_path()?.display()
        );
    } else {
        println!("  Backup: skipped (--no-backup) — there is nothing to roll back to.");
    }
    Ok(())
}

/// Counts only — never the text of a skipped, redacted or malformed record.
fn print_atuin_import_counts(stats: &AtuinImportStats) {
    println!(
        "  Already present: {} (re-importing the same database adds nothing)",
        stats.duplicates
    );
    println!("  Excluded by config: {}", stats.excluded);
    println!("  Blank/space-prefixed, not recorded: {}", stats.ignored);
    println!("  Deleted in Atuin, skipped: {}", stats.deleted);
    println!("  Malformed rows skipped: {}", stats.malformed);
    println!("  Redacted before storage: {}", stats.redacted);
    println!("  Atuin sessions preserved: {}", stats.sessions);
    println!(
        "  Unknown in Atuin (stored unknown): {} exit code(s), {} duration(s), {} directory(ies), \
         {} executor(s)",
        stats.unknown_exit, stats.unknown_duration, stats.unknown_cwd, stats.unknown_executor
    );
    println!("  Not in Atuin at all: tags, notes and command output");
}

/// Take a consistent backup of the *destination* database before writing, so a
/// bad import can be undone wholesale.
fn back_up_destination(
    repo: &Repository,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let base = crate::commands::entry::timestamped_backup_path("pre-atuin-import")?;
    // The stamp has second resolution, and re-running an import twice within
    // one second must not fail for want of a free filename.
    let mut dest = base.clone();
    for n in 2..100 {
        if !dest.exists() {
            break;
        }
        dest = base.with_file_name(format!(
            "{}-{n}.db",
            base.file_stem().unwrap_or_default().to_string_lossy()
        ));
    }
    repo.backup_to(&dest).map_err(|e| {
        format!(
            "Backup of the Suvadu database failed ({e}); aborting the import. \
             Re-run with --no-backup to import without one."
        )
    })?;
    Ok(dest)
}

/// SHA-256 of the database and of any WAL/shared-memory file beside it.
fn source_fingerprint(path: &Path) -> Option<Vec<(String, Option<String>)>> {
    let main = hash_file(path)?;
    let mut out = vec![("db".to_string(), Some(main))];
    for suffix in ["-wal", "-shm"] {
        let side = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
        out.push((suffix.to_string(), hash_file(&side)));
    }
    Some(out)
}

fn hash_file(path: &Path) -> Option<String> {
    use sha2::Digest;
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = sha2::Sha256::new();
    std::io::copy(&mut file, &mut hasher).ok()?;
    Some(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    /// The `_sqlx_migrations` set an Atuin 18.22.0 database carries.
    const MIGRATIONS_18_22: &[i64] = KNOWN_MIGRATIONS;
    /// The set an Atuin 18.0.0 – 18.6.1 database carries (no author columns).
    const MIGRATIONS_18_0: &[i64] = &[
        20_210_422_143_411,
        20_220_505_083_406,
        20_220_806_155_627,
        20_230_315_220_114,
        20_230_319_185_725,
    ];

    /// One `history` row, in the column order the fixture writes.
    struct Row {
        id: &'static str,
        timestamp: i64,
        duration: i64,
        exit: i64,
        command: &'static str,
        cwd: &'static str,
        session: &'static str,
        hostname: &'static str,
        deleted_at: Option<i64>,
        author: Option<&'static str>,
        intent: Option<&'static str>,
        shell: Option<&'static str>,
        author_kind: Option<i64>,
    }

    impl Row {
        fn new(id: &'static str, timestamp: i64, command: &'static str) -> Self {
            Self {
                id,
                timestamp,
                duration: 1_500_000_000,
                exit: 0,
                command,
                cwd: "/home/ellie/work",
                session: "0193c0ffee",
                hostname: "laptop:ellie",
                deleted_at: None,
                author: None,
                intent: None,
                shell: Some("zsh"),
                author_kind: Some(1),
            }
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        path: std::path::PathBuf,
    }

    impl Fixture {
        /// An Atuin 18.22.0-shaped database.
        fn modern(rows: &[Row]) -> Self {
            Self::build(MIGRATIONS_18_22, true, rows)
        }

        /// An Atuin 18.0.0-shaped database: no author/intent/shell columns.
        fn legacy(rows: &[Row]) -> Self {
            Self::build(MIGRATIONS_18_0, false, rows)
        }

        fn build(migrations: &[i64], extra_columns: bool, rows: &[Row]) -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let path = dir.path().join("history.db");
            let conn = rusqlite::Connection::open(&path).unwrap();
            let extras = if extra_columns {
                ", author text, intent text, shell text, author_kind integer"
            } else {
                ""
            };
            conn.execute_batch(&format!(
                "create table history (
                    id text primary key,
                    timestamp integer not null,
                    duration integer not null,
                    exit integer not null,
                    command text not null,
                    cwd text not null,
                    session text not null,
                    hostname text not null,
                    deleted_at integer{extras}
                );
                create table _sqlx_migrations (
                    version bigint primary key,
                    description text not null,
                    installed_on timestamp not null default current_timestamp,
                    success boolean not null,
                    checksum blob not null,
                    execution_time bigint not null
                );"
            ))
            .unwrap();
            for v in migrations {
                conn.execute(
                    "insert into _sqlx_migrations (version, description, success, checksum, \
                     execution_time) values (?1, 'test', 1, x'00', 0)",
                    params![v],
                )
                .unwrap();
            }
            for r in rows {
                if extra_columns {
                    conn.execute(
                        "insert into history (id, timestamp, duration, exit, command, cwd, \
                         session, hostname, deleted_at, author, intent, shell, author_kind) \
                         values (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                        params![
                            r.id,
                            r.timestamp,
                            r.duration,
                            r.exit,
                            r.command,
                            r.cwd,
                            r.session,
                            r.hostname,
                            r.deleted_at,
                            r.author,
                            r.intent,
                            r.shell,
                            r.author_kind,
                        ],
                    )
                    .unwrap();
                } else {
                    conn.execute(
                        "insert into history (id, timestamp, duration, exit, command, cwd, \
                         session, hostname, deleted_at) values (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                        params![
                            r.id,
                            r.timestamp,
                            r.duration,
                            r.exit,
                            r.command,
                            r.cwd,
                            r.session,
                            r.hostname,
                            r.deleted_at,
                        ],
                    )
                    .unwrap();
                }
            }
            Self { _dir: dir, path }
        }

        fn open(&self) -> (rusqlite::Connection, AtuinSchema) {
            let conn = open_source(&self.path).unwrap();
            let schema = read_schema(&conn).unwrap();
            (conn, schema)
        }

        fn sha256(&self) -> Option<Vec<(String, Option<String>)>> {
            source_fingerprint(&self.path)
        }
    }

    fn import(
        fixture: &Fixture,
        repo: &Repository,
        config: &crate::config::Config,
        dry_run: bool,
    ) -> AtuinImportStats {
        let (conn, schema) = fixture.open();
        import_atuin_history(
            repo,
            &conn,
            &schema,
            config,
            &AtuinImportOptions {
                hostname_fallback: "test-host",
                imported_at_ms: 1_800_000_000_000,
                dry_run,
            },
        )
        .unwrap()
    }

    // ── schema gate ─────────────────────────────────────────────────────

    #[test]
    fn schema_of_a_modern_atuin_database_is_supported() {
        let fixture = Fixture::modern(&[]);
        let (_conn, schema) = fixture.open();
        assert_eq!(schema.version, 20_260_818_000_000);
        assert!(schema.has_author && schema.has_intent);
        assert!(schema.has_shell && schema.has_author_kind);
    }

    #[test]
    fn schema_of_an_atuin_18_0_database_is_supported_without_author_columns() {
        let fixture = Fixture::legacy(&[]);
        let (_conn, schema) = fixture.open();
        assert_eq!(schema.version, 20_230_319_185_725);
        assert!(!schema.has_author, "18.0 has no author column");
        assert!(!schema.has_author_kind);
    }

    #[test]
    fn an_unrecognised_migration_is_rejected_with_a_next_step() {
        let mut migrations = MIGRATIONS_18_22.to_vec();
        migrations.push(20_270_101_000_000);
        let fixture = Fixture::build(&migrations, true, &[]);
        let conn = open_source(&fixture.path).unwrap();
        let err = read_schema(&conn).unwrap_err().to_string();
        assert!(err.contains("20270101000000"), "{err}");
        assert!(err.contains("newer Atuin"), "{err}");
        assert!(err.contains("suv update"), "next step missing: {err}");
    }

    #[test]
    fn a_pre_18_database_is_rejected_because_deletions_are_indistinguishable() {
        let fixture = Fixture::build(&MIGRATIONS_18_0[..3], true, &[]);
        let conn = open_source(&fixture.path).unwrap();
        let err = read_schema(&conn).unwrap_err().to_string();
        assert!(err.contains("18.0.0"), "{err}");
        assert!(err.contains("upgrade Atuin"), "next step missing: {err}");
    }

    #[test]
    fn a_failed_migration_is_rejected() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("history.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "create table history (id text primary key, timestamp integer not null, \
                 duration integer not null, exit integer not null, command text not null, \
                 cwd text not null, session text not null, hostname text not null, \
                 deleted_at integer);
                 create table _sqlx_migrations (version bigint primary key, description text \
                 not null, success boolean not null, checksum blob not null, execution_time \
                 bigint not null);
                 insert into _sqlx_migrations values (20210422143411, 'x', 1, x'00', 0);
                 insert into _sqlx_migrations values (20230319185725, 'x', 0, x'00', 0);",
            )
            .unwrap();
        }
        let conn = open_source(&path).unwrap();
        let err = read_schema(&conn).unwrap_err().to_string();
        assert!(err.contains("failed"), "{err}");
    }

    #[test]
    fn a_database_without_the_migration_table_is_rejected() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("history.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch("create table history (id text primary key);")
                .unwrap();
        }
        let conn = open_source(&path).unwrap();
        let err = read_schema(&conn).unwrap_err().to_string();
        assert!(err.contains("_sqlx_migrations"), "{err}");
    }

    #[test]
    fn a_database_that_is_not_atuin_is_rejected() {
        let (dir, repo) = crate::test_utils::test_repo();
        drop(repo);
        let conn = open_source(&dir.path().join("test.db")).unwrap();
        let err = read_schema(&conn).unwrap_err().to_string();
        assert!(err.contains("not an Atuin history database"), "{err}");
    }

    #[test]
    fn a_missing_file_names_the_usual_atuin_location() {
        let err = open_source(std::path::Path::new("/nonexistent/history.db"))
            .unwrap_err()
            .to_string();
        assert!(err.contains(".local/share/atuin/history.db"), "{err}");
    }

    // ── reading ─────────────────────────────────────────────────────────

    #[test]
    fn rows_are_read_oldest_first_with_atuin_unknowns_resolved() {
        let mut unfinished = Row::new("b", 1_700_000_200_000_000_000, "sleep 100");
        unfinished.exit = -1;
        unfinished.duration = -1;
        unfinished.cwd = "unknown";
        unfinished.hostname = "laptop";
        let fixture =
            Fixture::modern(&[unfinished, Row::new("a", 1_700_000_100_000_000_000, "ls")]);
        let (conn, schema) = fixture.open();

        let mut rows = Vec::new();
        let stats = stream_atuin_history(&conn, &schema, |row| {
            rows.push(row);
            Ok(())
        })
        .unwrap();

        assert_eq!(stats.rows, 2);
        assert_eq!(rows[0].command, "ls", "oldest row first");
        assert_eq!(
            rows[1].exit, None,
            "-1 is Atuin's unknown, not an exit code"
        );
        assert_eq!(rows[1].duration_ns, None);
        assert_eq!(rows[1].cwd, None, "\"unknown\" is a placeholder");
        assert_eq!(rows[1].host, "laptop");
        assert_eq!(rows[1].user, None, "a colonless hostname has no user");
    }

    #[test]
    fn soft_deleted_rows_are_counted_and_never_read() {
        let mut deleted = Row::new("b", 1_700_000_200_000_000_000, "rm -rf /secrets");
        deleted.deleted_at = Some(1_700_000_300_000_000_000);
        let fixture = Fixture::modern(&[Row::new("a", 1_700_000_100_000_000_000, "ls"), deleted]);
        let (conn, schema) = fixture.open();

        let mut commands = Vec::new();
        let stats = stream_atuin_history(&conn, &schema, |row| {
            commands.push(row.command);
            Ok(())
        })
        .unwrap();
        assert_eq!(stats.deleted, 1);
        assert_eq!(commands, vec!["ls".to_string()]);
    }

    #[test]
    fn malformed_rows_are_counted_and_skipped() {
        let fixture = Fixture::modern(&[
            Row::new("ok", 1_700_000_100_000_000_000, "ls"),
            Row::new("zero", 0, "date"),
            Row::new("future", 9_000_000_000_000_000_000, "date"),
        ]);
        // A timestamp SQLite stored as text.
        {
            let conn = rusqlite::Connection::open(&fixture.path).unwrap();
            conn.execute(
                "insert into history (id, timestamp, duration, exit, command, cwd, session, \
                 hostname) values ('text', 'not-a-time', 0, 0, 'echo hi', '/tmp', 's', 'h:u')",
                [],
            )
            .unwrap();
        }
        let (conn, schema) = fixture.open();
        let mut rows = 0;
        let stats = stream_atuin_history(&conn, &schema, |_| {
            rows += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(rows, 1, "only the sound row is handed over");
        assert_eq!(stats.malformed, 3);
    }

    #[test]
    fn hostname_is_split_into_host_and_user() {
        assert_eq!(
            split_hostname("laptop:ellie"),
            ("laptop".to_string(), Some("ellie".to_string()))
        );
        assert_eq!(split_hostname("laptop"), ("laptop".to_string(), None));
        assert_eq!(
            split_hostname("laptop:unknown-user"),
            ("laptop".to_string(), None),
            "Atuin's placeholder user is not a user"
        );
    }

    // ── importing ───────────────────────────────────────────────────────

    #[test]
    fn import_converts_units_and_never_fabricates_unknown_metadata() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let mut agent = Row::new("agent", 1_700_000_200_123_456_789, "cargo test");
        agent.author = Some("claude");
        agent.author_kind = Some(2);
        agent.intent = Some("check the build");
        agent.exit = 101;
        agent.duration = 2_500_000_000;
        let mut unfinished = Row::new("unfinished", 1_700_000_300_000_000_000, "sleep 100");
        unfinished.exit = -1;
        unfinished.duration = -1;
        unfinished.cwd = "unknown";
        unfinished.author_kind = None;
        let fixture = Fixture::modern(&[agent, unfinished]);

        let stats = import(&fixture, &repo, &cfg, false);
        assert_eq!(stats.imported, 2);
        assert_eq!(stats.unknown_exit, 1);
        assert_eq!(stats.unknown_duration, 1);
        assert_eq!(stats.unknown_cwd, 1);
        assert_eq!(stats.unknown_executor, 1);

        let entries = repo.get_entries_by_session("atuin-0193c0ffee").unwrap();
        assert_eq!(entries.len(), 2);
        let agent = entries.iter().find(|e| e.command == "cargo test").unwrap();
        assert_eq!(
            agent.started_at, 1_700_000_200_123,
            "nanoseconds truncated to milliseconds"
        );
        assert_eq!(agent.duration_ms, 2_500);
        assert_eq!(agent.ended_at, agent.started_at + 2_500);
        assert_eq!(agent.exit_code, Some(101));
        assert_eq!(agent.cwd, "/home/ellie/work");
        assert_eq!(agent.executor_type.as_deref(), Some("agent"));
        assert_eq!(agent.executor.as_deref(), Some("claude"));
        let ctx = agent.context.as_ref().unwrap();
        assert_eq!(ctx.get("import_source").unwrap(), "atuin-db");
        assert_eq!(ctx.get("timestamp_source").unwrap(), "file");
        assert_eq!(ctx.get("atuin_id").unwrap(), "agent");
        assert_eq!(ctx.get("atuin_intent").unwrap(), "check the build");
        assert_eq!(ctx.get("atuin_shell").unwrap(), "zsh");
        assert_eq!(ctx.get("atuin_user").unwrap(), "ellie");
        assert!(!ctx.contains_key("unknown_fields"), "nothing was unknown");

        let unknown = entries.iter().find(|e| e.command == "sleep 100").unwrap();
        assert_eq!(unknown.exit_code, None, "never a fabricated success");
        assert_eq!(unknown.duration_ms, 0);
        assert_eq!(unknown.ended_at, unknown.started_at);
        assert_eq!(unknown.cwd, "");
        assert_eq!(unknown.executor_type.as_deref(), Some("unknown"));
        let ctx = unknown.context.as_ref().unwrap();
        assert_eq!(
            ctx.get("unknown_fields").unwrap(),
            "exit_code,duration_ms,cwd,executor"
        );
    }

    #[test]
    fn import_never_guesses_the_executor_from_an_agent_sounding_author() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let mut row = Row::new("a", 1_700_000_100_000_000_000, "ls");
        row.author = Some("claude");
        row.author_kind = None;
        let fixture = Fixture::modern(&[row]);
        import(&fixture, &repo, &cfg, false);

        let entry = &repo.get_entries_by_session("atuin-0193c0ffee").unwrap()[0];
        assert_eq!(
            entry.executor_type.as_deref(),
            Some("unknown"),
            "Atuin guesses from the author name; we record what was stated"
        );
        assert_eq!(entry.executor.as_deref(), Some("claude"));
    }

    #[test]
    fn import_preserves_unicode_and_multiline_commands_verbatim() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let fixture = Fixture::modern(&[
            Row::new("u", 1_700_000_100_000_000_000, "echo 'héllo 世界 🌍'"),
            Row::new(
                "m",
                1_700_000_200_000_000_000,
                "for i in 1 2; do\n  echo $i\ndone",
            ),
        ]);
        import(&fixture, &repo, &cfg, false);

        let entries = repo.get_entries_by_session("atuin-0193c0ffee").unwrap();
        let commands: Vec<&str> = entries.iter().map(|e| e.command.as_str()).collect();
        assert!(commands.contains(&"echo 'héllo 世界 🌍'"));
        assert!(commands.contains(&"for i in 1 2; do\n  echo $i\ndone"));
    }

    #[test]
    fn import_preserves_repeated_executions_including_within_one_millisecond() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        // Three `ls` runs: two inside the same millisecond, one later.
        let fixture = Fixture::modern(&[
            Row::new("a", 1_700_000_100_000_000_000, "ls"),
            Row::new("b", 1_700_000_100_000_500_000, "ls"),
            Row::new("c", 1_700_000_200_000_000_000, "ls"),
        ]);
        let stats = import(&fixture, &repo, &cfg, false);
        assert_eq!(stats.imported, 3, "repeated executions are not collapsed");

        let mut times: Vec<i64> = repo
            .get_entries_by_session("atuin-0193c0ffee")
            .unwrap()
            .iter()
            .map(|e| e.started_at)
            .collect();
        times.sort_unstable();
        assert_eq!(
            times,
            vec![1_700_000_100_000, 1_700_000_100_001, 1_700_000_200_000]
        );
    }

    #[test]
    fn a_second_identical_import_adds_nothing() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let fixture = Fixture::modern(&[
            Row::new("a", 1_700_000_100_000_000_000, "ls"),
            Row::new("b", 1_700_000_100_000_500_000, "ls"),
            Row::new("c", 1_700_000_200_000_000_000, "git status"),
        ]);

        assert_eq!(import(&fixture, &repo, &cfg, false).imported, 3);
        let second = import(&fixture, &repo, &cfg, false);
        assert_eq!(second.imported, 0, "re-import must be idempotent");
        assert_eq!(second.duplicates, 3);
        assert_eq!(repo.count_entries().unwrap(), 3);
    }

    #[test]
    fn import_leaves_existing_suvadu_entries_intact() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        repo.insert_session_if_missing("native", "test-host", 1_600_000_000_000)
            .unwrap();
        let mut existing = crate::models::Entry::new(
            "native".to_string(),
            "echo mine".to_string(),
            "/home/me".to_string(),
            Some(0),
            1_600_000_000_000,
            1_600_000_000_100,
        );
        existing.executor_type = Some("human".to_string());
        repo.insert_entry(&existing).unwrap();

        let fixture = Fixture::modern(&[Row::new("a", 1_700_000_100_000_000_000, "ls")]);
        import(&fixture, &repo, &cfg, false);

        assert_eq!(repo.count_entries().unwrap(), 2);
        let kept = &repo.get_entries_by_session("native").unwrap()[0];
        assert_eq!(kept.command, "echo mine");
        assert_eq!(kept.exit_code, Some(0));
        assert_eq!(kept.cwd, "/home/me");
    }

    #[test]
    fn import_preserves_atuin_sessions() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let mut other = Row::new("b", 1_700_000_200_000_000_000, "git status");
        other.session = "deadbeef";
        other.hostname = "desktop:ellie";
        let fixture = Fixture::modern(&[Row::new("a", 1_700_000_100_000_000_000, "ls"), other]);

        let stats = import(&fixture, &repo, &cfg, false);
        assert_eq!(stats.sessions, 2);
        assert_eq!(
            repo.get_session("atuin-deadbeef")
                .unwrap()
                .unwrap()
                .hostname,
            "desktop"
        );
        assert_eq!(
            repo.get_entries_by_session("atuin-deadbeef").unwrap().len(),
            1
        );
    }

    #[test]
    fn a_row_without_a_session_lands_in_one_named_unknown() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let mut row = Row::new("a", 1_700_000_100_000_000_000, "ls");
        row.session = "";
        let fixture = Fixture::modern(&[row]);
        import(&fixture, &repo, &cfg, false);
        assert_eq!(
            repo.get_entries_by_session("atuin-unknown-session")
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn an_atuin_18_0_database_imports_without_the_author_columns() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let fixture = Fixture::legacy(&[Row::new("a", 1_700_000_100_000_000_000, "ls")]);
        let stats = import(&fixture, &repo, &cfg, false);

        assert_eq!(stats.imported, 1);
        assert_eq!(stats.unknown_executor, 1);
        let entry = &repo.get_entries_by_session("atuin-0193c0ffee").unwrap()[0];
        assert_eq!(entry.executor_type.as_deref(), Some("unknown"));
        assert!(entry.executor.is_none());
        let ctx = entry.context.as_ref().unwrap();
        assert!(!ctx.contains_key("atuin_shell"), "18.0 has no shell column");
    }

    #[test]
    fn import_honours_exclusions_and_redaction_and_ignores_blank_commands() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config {
            exclusions: vec!["^vault ".to_string()],
            ..Default::default()
        };
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let fixture = Fixture::modern(&[
            Row::new("a", 1_700_000_100_000_000_000, "vault login"),
            Row::new("b", 1_700_000_200_000_000_000, " secret-thing"),
            Row::new("c", 1_700_000_300_000_000_000, "   "),
            Row::new(
                "d",
                1_700_000_400_000_000_000,
                "export GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            ),
        ]);

        let stats = import(&fixture, &repo, &cfg, false);
        assert_eq!(stats.excluded, 1);
        assert_eq!(stats.ignored, 2, "space-prefixed and blank");
        assert_eq!(stats.redacted, 1);
        assert_eq!(stats.imported, 1);

        let entry = &repo.get_entries_by_session("atuin-0193c0ffee").unwrap()[0];
        assert!(!entry.command.contains(secret), "{}", entry.command);
        assert!(entry.command.contains("REDACTED"));
    }

    #[test]
    fn dry_run_counts_everything_and_writes_nothing() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let mut deleted = Row::new("d", 1_700_000_300_000_000_000, "rm -rf /secrets");
        deleted.deleted_at = Some(1_700_000_400_000_000_000);
        let fixture = Fixture::modern(&[
            Row::new("a", 1_700_000_100_000_000_000, "ls"),
            Row::new("b", 1_700_000_200_000_000_000, "git status"),
            deleted,
        ]);

        let stats = import(&fixture, &repo, &cfg, true);
        assert_eq!(stats.imported, 2, "dry run reports what would be imported");
        assert_eq!(stats.deleted, 1);
        assert_eq!(repo.count_entries().unwrap(), 0, "nothing written");
        assert!(repo.get_session("atuin-0193c0ffee").unwrap().is_none());
        assert_eq!(stats.samples.len(), 2);
    }

    #[test]
    fn dry_run_samples_are_redacted() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let fixture = Fixture::modern(&[Row::new(
            "a",
            1_700_000_100_000_000_000,
            "export GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123456789",
        )]);
        let stats = import(&fixture, &repo, &cfg, true);
        assert!(
            !stats.samples.iter().any(|(cmd, _)| cmd.contains(secret)),
            "a dry-run preview must not print secrets"
        );
    }

    #[test]
    fn dry_run_reports_rows_already_present() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let fixture = Fixture::modern(&[Row::new("a", 1_700_000_100_000_000_000, "ls")]);
        assert_eq!(import(&fixture, &repo, &cfg, false).imported, 1);

        let dry = import(&fixture, &repo, &cfg, true);
        assert_eq!(dry.imported, 0);
        assert_eq!(dry.duplicates, 1);
    }

    #[test]
    fn the_source_database_is_byte_identical_after_an_import() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let fixture = Fixture::modern(&[
            Row::new("a", 1_700_000_100_000_000_000, "ls"),
            Row::new("b", 1_700_000_200_000_000_000, "git status"),
        ]);
        let before = fixture.sha256();

        import(&fixture, &repo, &cfg, false);
        import(&fixture, &repo, &cfg, true);

        assert_eq!(fixture.sha256(), before, "the Atuin database was modified");
    }

    #[test]
    fn a_wal_mode_source_with_uncheckpointed_writes_is_read_consistently() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("history.db");
        // A live Atuin-like writer: WAL on, rows written, no checkpoint.
        let writer = rusqlite::Connection::open(&path).unwrap();
        writer
            .pragma_update(None, "journal_mode", "WAL")
            .or_else(|_| writer.execute_batch("PRAGMA journal_mode=WAL"))
            .unwrap();
        writer
            .execute_batch(
                "create table history (id text primary key, timestamp integer not null, \
                 duration integer not null, exit integer not null, command text not null, \
                 cwd text not null, session text not null, hostname text not null, \
                 deleted_at integer, author text, intent text, shell text, author_kind integer);
                 create table _sqlx_migrations (version bigint primary key, description text \
                 not null, success boolean not null, checksum blob not null, execution_time \
                 bigint not null);",
            )
            .unwrap();
        for v in KNOWN_MIGRATIONS {
            writer
                .execute(
                    "insert into _sqlx_migrations values (?1, 'x', 1, x'00', 0)",
                    params![v],
                )
                .unwrap();
        }
        writer
            .execute(
                "insert into history (id, timestamp, duration, exit, command, cwd, session, \
                 hostname) values ('a', 1700000100000000000, 0, 0, 'ls', '/tmp', 's', 'h:u')",
                [],
            )
            .unwrap();

        let (_d, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let conn = open_source(&path).unwrap();
        let schema = read_schema(&conn).unwrap();
        let stats = import_atuin_history(
            &repo,
            &conn,
            &schema,
            &cfg,
            &AtuinImportOptions {
                hostname_fallback: "test-host",
                imported_at_ms: 1_800_000_000_000,
                dry_run: false,
            },
        )
        .unwrap();
        assert_eq!(stats.imported, 1, "a WAL source is readable read-only");

        // A row written after our snapshot is simply not in it; the import
        // never blocks the writer and never half-reads a transaction.
        writer
            .execute(
                "insert into history (id, timestamp, duration, exit, command, cwd, session, \
                 hostname) values ('b', 1700000200000000000, 0, 0, 'later', '/tmp', 's', 'h:u')",
                [],
            )
            .unwrap();
        assert_eq!(repo.count_entries().unwrap(), 1);
    }

    #[test]
    fn provenance_can_be_counted_back_out_of_the_destination() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let cfg = crate::config::Config::default();
        let fixture = Fixture::modern(&[
            Row::new("a", 1_700_000_100_000_000_000, "ls"),
            Row::new("b", 1_700_000_200_000_000_000, "git status"),
        ]);
        let stats = import(&fixture, &repo, &cfg, false);
        assert_eq!(stats.imported, 2);
        assert_eq!(
            repo.count_entries_by_import(IMPORT_SOURCE, Some(1_800_000_000_000))
                .unwrap(),
            2,
            "post-import validation counts this run's provenance"
        );
        assert_eq!(
            repo.count_entries_by_import(IMPORT_SOURCE, Some(1))
                .unwrap(),
            0
        );
    }

    #[test]
    fn executor_type_is_only_what_atuin_stated() {
        assert_eq!(executor_type_for(Some(1)), "human");
        assert_eq!(executor_type_for(Some(2)), "agent");
        assert_eq!(executor_type_for(Some(7)), "unknown", "future kind");
        assert_eq!(executor_type_for(None), "unknown");
    }
}
