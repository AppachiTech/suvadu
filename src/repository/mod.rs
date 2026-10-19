mod ai_sessions;
mod aliases;
mod api;
mod bookmarks;
mod capture;
mod entries;
mod notes;
mod skills;
mod stats;
mod storage;
mod tags;

pub use capture::{CaptureRecordStats, PLACEHOLDER_IMPORT_HOSTNAME, RESTORED_AT_KEY};

// Re-exported for use as a mockable trait boundary in tests and downstream consumers.
#[allow(unused_imports)]
pub use api::RepositoryApi;

#[cfg(test)]
mod tests;

use crate::db::DbResult;
use crate::models::{Entry, Session};
use rusqlite::{params, Connection, OpenFlags};

/// Shared entry column list for SELECT queries
pub const ENTRY_COLUMNS: &str = "e.id, e.session_id, e.command, e.cwd, e.exit_code, e.started_at, e.ended_at, e.duration_ms, e.context, COALESCE(et.name, st.name) as tag_name, e.tag_id, e.executor_type, e.executor";

/// Shared FROM/JOIN clause for entry queries.
///
/// `sessions` is a `LEFT JOIN` so that an entry whose session row is missing is
/// still listed — and listed exactly when `count_filtered` and
/// `count_unique_filtered`, which use the same join, count it. Only
/// `s.tag_id` is read from the session, and a missing row reads as no tag.
pub const ENTRY_JOINS: &str = "FROM entries e
             LEFT JOIN sessions s ON e.session_id = s.id
             LEFT JOIN tags st ON s.tag_id = st.id
             LEFT JOIN tags et ON e.tag_id = et.id";

/// Maps a `SQLite` row to an `Entry`. `tag_id_col` is the column index where `tag_id` starts
/// (10 for standard queries, 11 for unique queries where COUNT(*) is at position 10).
pub fn entry_from_row(row: &rusqlite::Row, tag_id_col: usize) -> rusqlite::Result<Entry> {
    let context_str: Option<String> = row.get(8)?;
    let context = context_str.and_then(|s| match serde_json::from_str(&s) {
        Ok(ctx) => Some(ctx),
        Err(e) => {
            eprintln!("suvadu: malformed context JSON: {e}");
            None
        }
    });
    let tag_name: Option<String> = row.get(9)?;

    Ok(Entry {
        id: Some(row.get(0)?),
        session_id: row.get(1)?,
        command: row.get(2)?,
        cwd: row.get(3)?,
        exit_code: row.get(4)?,
        started_at: row.get(5)?,
        ended_at: row.get(6)?,
        duration_ms: row.get(7)?,
        context,
        tag_name,
        tag_id: row.get(tag_id_col)?,
        executor_type: row.get(tag_id_col + 1)?,
        executor: row.get(tag_id_col + 2)?,
    })
}

/// Filter parameters for entry queries. Replaces 10-12 positional parameters
/// with a single self-documenting struct.
#[derive(Default, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct QueryFilter<'a> {
    pub after: Option<i64>,
    pub before: Option<i64>,
    pub tag_id: Option<i64>,
    pub exit_code: Option<i32>,
    pub query: Option<&'a str>,
    pub prefix_match: bool,
    pub executor: Option<&'a str>,
    pub cwd: Option<&'a str>,
    pub field: crate::models::SearchField,
    /// When `true`, hide automated (agent/bot/ci/programmatic) commands so only
    /// interactively-typed history is returned. Ignored when an explicit
    /// `executor` filter is set (that filter wins). Defaults to `false`
    /// (show everything) to preserve behaviour for non-recall callers.
    pub exclude_agents: bool,
    /// When `true`, `cwd` matches the directory **and its subtree** rather than
    /// exactly. Used by project-scoped tools so "this directory" means the whole
    /// project tree. Defaults to `false` (exact match).
    pub cwd_prefix: bool,
    /// When `true`, return only commands that failed (non-zero, non-null exit
    /// code). Combined with an explicit `exit_code` it is redundant; on its own
    /// it answers "what did I run that failed?". Defaults to `false`.
    pub failed_only: bool,
    /// When `true`, return only commands that are bookmarked (the command text
    /// exists in the `bookmarks` table). Powers the Ctrl+O "only bookmarks"
    /// filter in the search TUI. Defaults to `false`.
    pub bookmarked_only: bool,
    /// Every one of these must appear in `field` as a case-insensitive
    /// substring. This is how interactive search narrows candidates to the
    /// rows its in-memory scorer could accept, across the whole history,
    /// rather than scoring only the newest N rows. Empty for other callers,
    /// which use the single `query` above.
    pub query_tokens: &'a [String],
    /// Directories (and their subtrees) to exclude from results, i.e.
    /// `mcp.exclude_dirs`. Empty for every non-MCP caller — only MCP tool/
    /// resource handlers populate this, from `McpConfig`.
    pub exclude_dirs: &'a [String],
}

impl QueryFilter<'_> {
    /// Build a `FilterBuilder` from this filter.
    pub fn to_filter_builder(&self) -> FilterBuilder {
        FilterBuilder::new()
            .with_date_range(self.after, self.before)
            .with_tag(self.tag_id)
            .with_exit_code(self.exit_code)
            .with_query_field(self.query, self.prefix_match, self.field)
            .with_query_tokens(self.query_tokens, self.field)
            .with_executor(self.executor)
            .with_cwd_mode(self.cwd, self.cwd_prefix)
            // An explicit executor filter takes precedence over the agent hide.
            .with_exclude_agents(self.exclude_agents && self.executor.is_none())
            .with_failed_only(self.failed_only)
            .with_bookmarked_only(self.bookmarked_only)
            .with_excluded_dirs(self.exclude_dirs)
    }
}

/// Anything that can describe which entries a query should return.
///
/// Entry queries take this rather than a concrete [`QueryFilter`] so a scope
/// can add its own clauses without every unrelated caller having to know
/// about them.
pub trait EntryQuery {
    fn entry_filter_builder(&self) -> FilterBuilder;
}

impl EntryQuery for QueryFilter<'_> {
    fn entry_filter_builder(&self) -> FilterBuilder {
        self.to_filter_builder()
    }
}

/// A [`QueryFilter`] pinned to one shell session — the `session` recall scope.
///
/// `session_id: None` means "do not narrow", so the wrapper is safe to use
/// unconditionally: it behaves exactly like the inner filter until a session
/// is actually supplied.
#[derive(Clone, Default)]
pub struct SessionScoped<'a> {
    pub filter: QueryFilter<'a>,
    pub session_id: Option<&'a str>,
}

impl EntryQuery for SessionScoped<'_> {
    fn entry_filter_builder(&self) -> FilterBuilder {
        self.filter
            .to_filter_builder()
            .with_session(self.session_id)
    }
}

/// Any entry query narrowed to commands an AI agent ran
/// (`executor_type = 'agent'`), so a caller grouping agent sessions reads
/// only those rows instead of a prefix of everything.
pub struct AgentCommands<Q: EntryQuery>(pub Q);

impl<Q: EntryQuery> EntryQuery for AgentCommands<Q> {
    fn entry_filter_builder(&self) -> FilterBuilder {
        self.0.entry_filter_builder().with_agent_commands_only()
    }
}

/// Any entry query plus the `fuzzy` mode's subsequence rule.
///
/// Wrapping rather than adding a field to [`QueryFilter`] keeps the rule where
/// it belongs — only interactive recall's `fuzzy` mode has one — and means
/// every other caller is untouched. `needle: None` behaves exactly like the
/// inner query, so the wrapper is safe to apply unconditionally.
pub struct Subsequence<'a, Q: EntryQuery> {
    pub inner: Q,
    pub needle: Option<&'a str>,
    pub field: crate::models::SearchField,
}

impl<Q: EntryQuery> EntryQuery for Subsequence<'_, Q> {
    fn entry_filter_builder(&self) -> FilterBuilder {
        self.inner
            .entry_filter_builder()
            .with_subsequence(self.needle, self.field)
    }
}

/// Any entry query plus separator-insensitive terms: each of `terms`, already
/// normalized (`util::normalize_separators`), must appear in `field` once
/// its separators are removed.
///
/// This is how recall retries a `terms` query that matched nothing, so that
/// `hiveagent` finds `hive-agent`. Like [`Subsequence`], it is a wrapper so
/// that only that caller ever sees the rule; empty `terms` behaves exactly
/// like the inner query.
pub struct SeparatorInsensitive<'a, Q: EntryQuery> {
    pub inner: Q,
    pub terms: &'a [String],
    pub field: crate::models::SearchField,
}

impl<Q: EntryQuery> EntryQuery for SeparatorInsensitive<'_, Q> {
    fn entry_filter_builder(&self) -> FilterBuilder {
        self.inner
            .entry_filter_builder()
            .with_separator_insensitive_terms(self.terms, self.field)
    }
}

use crate::models::SearchField;

/// Filter parameters for replay queries.
#[derive(Default)]
pub struct ReplayFilter<'a> {
    pub after: Option<i64>,
    pub before: Option<i64>,
    pub tag_id: Option<i64>,
    pub exit_code: Option<i32>,
    pub executor: Option<&'a str>,
    pub cwd: Option<&'a str>,
    /// Maximum number of entries to return (None = unlimited).
    pub limit: Option<usize>,
    /// Rows to skip before the first returned entry, so a long session can
    /// be walked one page at a time instead of re-reading its head.
    pub offset: usize,
    /// Directories (and their subtrees) to exclude from results, i.e.
    /// `mcp.exclude_dirs`. Empty for every non-MCP caller.
    pub exclude_dirs: &'a [String],
    /// Only rows [`crate::models::Entry::is_agent`] accepts, filtered in SQL
    /// so a `limit` counts agent rows only.
    pub agents_only: bool,
    /// With a `limit`, keep the newest rows rather than the oldest. The
    /// result is still oldest first.
    pub newest: bool,
}

/// Escape SQL LIKE wildcards (`%`, `_`) and the escape character (`\`) in user input.
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// `dir` without its trailing `/`, and the half-open byte range of the
/// paths beneath it: `[dir/, dir0)`. `0` is the byte after `/`, so a path
/// is in the range exactly when it starts with `dir/`.
fn subtree_bounds(dir: &str) -> (String, String, String) {
    let root = dir.trim_end_matches('/');
    (root.to_string(), format!("{root}/"), format!("{root}0"))
}

/// Expand a leading `~` to `$HOME`, matching how `mcp.exclude_dirs` entries
/// like `"~/.ssh"` are meant to be written in `config.toml` — recorded
/// `cwd` values are always absolute, so this must run before comparing.
pub fn expand_tilde(dir: &str) -> String {
    let Some(home) = std::env::var_os("HOME") else {
        return dir.to_string();
    };
    let home = home.to_string_lossy();
    if dir == "~" {
        home.into_owned()
    } else if let Some(rest) = dir.strip_prefix("~/") {
        format!("{home}/{rest}")
    } else {
        dir.to_string()
    }
}

/// Builds WHERE clauses and collects parameters for filtered queries.
pub struct FilterBuilder {
    clauses: Vec<String>,
    params: Vec<Box<dyn rusqlite::ToSql>>,
    /// Whether a clause reads the entry's session (`s.`), so the query
    /// must join `sessions s`.
    reads_session: bool,
}

impl FilterBuilder {
    pub fn new() -> Self {
        Self {
            clauses: Vec::new(),
            params: Vec::new(),
            reads_session: false,
        }
    }

    /// Whether the clauses read the entry's session (`s.`). A query that
    /// only filters can then leave `sessions` out: `ENTRY_JOINS` joins it on
    /// its primary key, so the join never adds or drops an entry.
    pub const fn reads_session(&self) -> bool {
        self.reads_session
    }

    pub fn with_date_range(mut self, after: Option<i64>, before: Option<i64>) -> Self {
        if let Some(start) = after {
            self.clauses.push("e.started_at >= ?".into());
            self.params.push(Box::new(start));
        }
        if let Some(end) = before {
            self.clauses.push("e.started_at <= ?".into());
            self.params.push(Box::new(end));
        }
        self
    }

    pub fn with_tag(mut self, tag_id: Option<i64>) -> Self {
        if let Some(tid) = tag_id {
            self.clauses.push("(s.tag_id = ? OR e.tag_id = ?)".into());
            self.reads_session = true;
            self.params.push(Box::new(tid));
            self.params.push(Box::new(tid));
        }
        self
    }

    pub fn with_exit_code(mut self, exit_code: Option<i32>) -> Self {
        if let Some(code) = exit_code {
            self.clauses.push("e.exit_code = ?".into());
            self.params.push(Box::new(code));
        }
        self
    }

    #[cfg(test)]
    pub fn with_query(self, query: Option<&str>, prefix_match: bool) -> Self {
        self.with_query_field(query, prefix_match, SearchField::Command)
    }

    pub fn with_query_field(
        mut self,
        query: Option<&str>,
        prefix_match: bool,
        field: SearchField,
    ) -> Self {
        let Some(q) = query else {
            return self;
        };
        // A substring search on `command` goes through the trigram index;
        // see `push_command_contains`.
        if field == SearchField::Command && !prefix_match {
            self.push_command_contains(q);
            return self;
        }

        // An anchored prefix (`text%`) on `command` seeks
        // `idx_entries_command_nocase` (schema v12): the index is in `NOCASE`
        // order, which folds exactly the ASCII case LIKE folds, so SQLite's
        // LIKE optimization reads the prefix as one range of it — the
        // `ESCAPE` clause included. `a_prefix_search_seeks_the_case_folding_index`
        // holds the plan to that.
        let escaped = escape_like(q);

        let column = match field {
            SearchField::Cwd => "e.cwd",
            SearchField::Session => "e.session_id",
            SearchField::Executor => "COALESCE(e.executor_type || ' ' || e.executor, '')",
            SearchField::Command => "e.command",
        };
        self.clauses.push(format!("{column} LIKE ? ESCAPE '\\'"));
        if prefix_match {
            self.params.push(Box::new(format!("{escaped}%")));
        } else {
            self.params.push(Box::new(format!("%{escaped}%")));
        }
        self
    }

    /// AND together one substring clause per token.
    ///
    /// `SQLite`'s LIKE only folds case for ASCII, so a token containing
    /// non-ASCII characters is matched with `suvadu_contains_ci` instead,
    /// which folds Unicode case; LIKE could drop a row that differs only by
    /// the case of a non-ASCII letter.
    pub fn with_query_tokens(mut self, tokens: &[String], field: SearchField) -> Self {
        for token in tokens.iter().filter(|t| !t.is_empty()) {
            let column = match field {
                SearchField::Cwd => "e.cwd",
                SearchField::Session => "e.session_id",
                SearchField::Executor => "COALESCE(e.executor_type || ' ' || e.executor, '')",
                SearchField::Command => "e.command",
            };

            if !token.is_ascii() {
                // LIKE cannot fold non-ASCII case; suvadu_contains_ci can
                // (db::register_contains_ci). It cannot use an index, so this
                // is reserved for the tokens that need it.
                self.clauses
                    .push(format!("suvadu_contains_ci({column}, ?)"));
                self.params.push(Box::new(token.clone()));
                continue;
            }

            if field == SearchField::Command {
                // Same trigram-index route as with_query_field.
                self.push_command_contains(token);
            } else {
                self.clauses.push(format!("{column} LIKE ? ESCAPE '\\'"));
                self.params
                    .push(Box::new(format!("%{}%", escape_like(token))));
            }
        }
        self
    }

    /// Require `e.command` to contain `needle`, ASCII case-insensitively —
    /// the same rule as `e.command LIKE '%needle%' ESCAPE '\'` with `needle`
    /// escaped — answered through the `entries_fts` trigram index.
    ///
    /// `SQLite` only offers a LIKE to FTS5 in its two-argument form. Add an
    /// `ESCAPE` clause and FTS5 never sees the constraint, so it walks every
    /// row of the index and `SQLite` tests each one (`INDEX 0:` in the query
    /// plan instead of `INDEX 0:L0`). The index lookup is therefore always
    /// the unescaped `LIKE '%needle%'`. Without an `ESCAPE` clause a
    /// backslash is an ordinary character, so only `%` and `_` can change the
    /// meaning: when `needle` holds one, the lookup admits a superset and an
    /// escaped LIKE on the column keeps only the literal matches.
    ///
    /// A needle shorter than three characters has no trigram to look up, so
    /// FTS5 falls back to testing every row — slower, but still exact.
    fn push_command_contains(&mut self, needle: &str) {
        self.clauses
            .push("e.id IN (SELECT rowid FROM entries_fts WHERE command LIKE ?)".into());
        self.params.push(Box::new(format!("%{needle}%")));
        if needle.contains(['%', '_']) {
            self.clauses.push("e.command LIKE ? ESCAPE '\\'".into());
            self.params
                .push(Box::new(format!("%{}%", escape_like(needle))));
        }
    }

    /// AND together one separator-insensitive clause per term: `field`, with
    /// its separators removed and ASCII lower-cased (`db::normalized_sql`),
    /// must contain the term, which the caller has normalized the same way.
    ///
    /// On `command` an ASCII term is looked up in the `entries_norm_fts`
    /// trigram index, by the same two-argument `LIKE` as
    /// `push_command_contains` and for the same reason. A normalized
    /// term holds no `_` (it is a separator) but can hold `%`, which the
    /// unescaped lookup reads as a wildcard, so such a term is checked again
    /// with an escaped `LIKE` on the normalized command. A non-ASCII term, or
    /// any term on another field, has no index and is tested row by row:
    /// `suvadu_contains_ci` for non-ASCII, so case folds as it does in
    /// strict `terms`.
    pub fn with_separator_insensitive_terms(
        mut self,
        terms: &[String],
        field: SearchField,
    ) -> Self {
        let column = match field {
            SearchField::Cwd => "e.cwd",
            SearchField::Session => "e.session_id",
            SearchField::Executor => "COALESCE(e.executor_type || ' ' || e.executor, '')",
            SearchField::Command => "e.command",
        };
        let normalized = crate::db::normalized_sql(column);
        for term in terms.iter().filter(|t| !t.is_empty()) {
            if !term.is_ascii() {
                self.clauses
                    .push(format!("suvadu_contains_ci({normalized}, ?)"));
                self.params.push(Box::new(term.clone()));
            } else if field == SearchField::Command {
                self.clauses.push(
                    "e.id IN (SELECT rowid FROM entries_norm_fts WHERE command_norm LIKE ?)".into(),
                );
                self.params.push(Box::new(format!("%{term}%")));
                if term.contains(['%', '_']) {
                    self.clauses
                        .push(format!("{normalized} LIKE ? ESCAPE '\\'"));
                    self.params
                        .push(Box::new(format!("%{}%", escape_like(term))));
                }
            } else {
                self.clauses
                    .push(format!("{normalized} LIKE ? ESCAPE '\\'"));
                self.params
                    .push(Box::new(format!("%{}%", escape_like(term))));
            }
        }
        self
    }

    /// Require `field` to contain `needle` as a case-insensitive subsequence
    /// — the `fuzzy` recall mode's own rule, evaluated by the database.
    ///
    /// This clause decides eligibility exactly, so `count_filtered`/
    /// `count_unique_filtered` return a number the caller can really page to
    /// and `LIMIT`/`OFFSET` walk only accepted matches. No index can answer
    /// it, so it tests every row the other clauses leave.
    pub fn with_subsequence(mut self, needle: Option<&str>, field: SearchField) -> Self {
        let Some(needle) = needle.filter(|n| !n.trim().is_empty()) else {
            return self;
        };
        let column = match field {
            SearchField::Cwd => "e.cwd",
            SearchField::Session => "e.session_id",
            SearchField::Executor => "COALESCE(e.executor_type || ' ' || e.executor, '')",
            SearchField::Command => "e.command",
        };
        self.clauses.push(format!("suvadu_subseq_ci({column}, ?)"));
        self.params.push(Box::new(needle.to_string()));
        self
    }

    pub fn with_cwd(mut self, cwd: Option<&str>) -> Self {
        if let Some(dir) = cwd {
            self.clauses.push("e.cwd = ?".into());
            self.params.push(Box::new(dir.to_string()));
        }
        self
    }

    /// Filter by directory, optionally including its subtree. With `prefix`,
    /// matches the directory itself plus any path under `dir/` (see
    /// [`Self::with_cwd_subtree`]). Without `prefix`, exact match.
    pub fn with_cwd_mode(self, cwd: Option<&str>, prefix: bool) -> Self {
        match (cwd, prefix) {
            (Some(dir), true) => self.with_cwd_subtree(dir),
            (cwd, _) => self.with_cwd(cwd),
        }
    }

    /// Match `dir` and everything beneath it (`dir` or `dir/...`), comparing
    /// paths exactly, case included: on a case-sensitive file system
    /// `/a/Proj` is another directory than `/a/proj`.
    ///
    /// "Starts with `dir/`" is the byte range from `dir/` up to, not
    /// including, `dir0` — `0` is the byte after `/`, and `cwd` compares
    /// bytewise — so the test is exact for any path, whatever `%`, `_`, `*`
    /// or `[` it holds, and it seeks `idx_entries_cwd_started` rather than
    /// scanning it. (A `LIKE 'dir/%'` folds ASCII case and cannot.)
    pub fn with_cwd_subtree(mut self, dir: &str) -> Self {
        let (root, start, end) = subtree_bounds(dir);
        self.clauses
            .push("(e.cwd = ? OR (e.cwd >= ? AND e.cwd < ?))".into());
        self.params.push(Box::new(root));
        self.params.push(Box::new(start));
        self.params.push(Box::new(end));
        self
    }

    /// Exclude `dir` and everything beneath it (`dir` or `dir/...`) from
    /// results, one clause per directory, all `AND`ed together with the rest
    /// of the query. Powers `mcp.exclude_dirs`: directories an MCP-connected
    /// agent must never see entries from, regardless of which tool/resource
    /// is asking.
    ///
    /// Unlike [`Self::with_cwd_subtree`], this folds ASCII case — the
    /// directory itself by `NOCASE`, its subtree by `LIKE` — so it errs
    /// towards hiding: on macOS's default file system `/a/Proj` and
    /// `/a/proj` are one directory, and a shell records the path as typed.
    pub fn with_excluded_dirs(mut self, dirs: &[String]) -> Self {
        for dir in dirs {
            let expanded = expand_tilde(dir);
            let trimmed = expanded.trim().trim_end_matches('/');
            if trimmed.is_empty() {
                continue;
            }
            self.clauses
                .push("NOT (e.cwd = ? COLLATE NOCASE OR e.cwd LIKE ? ESCAPE '\\')".into());
            self.params.push(Box::new(trimmed.to_string()));
            self.params
                .push(Box::new(format!("{}/%", escape_like(trimmed))));
        }
        self
    }

    pub fn with_session(mut self, session_id: Option<&str>) -> Self {
        if let Some(sid) = session_id {
            self.clauses.push("e.session_id = ?".into());
            self.params.push(Box::new(sid.to_string()));
        }
        self
    }

    /// Filter on `s.created_at >= ?` — for session-centric queries
    /// where the primary table is `sessions s`.
    pub fn with_session_created_after(mut self, after: Option<i64>) -> Self {
        if let Some(ts) = after {
            self.clauses.push("s.created_at >= ?".into());
            self.reads_session = true;
            self.params.push(Box::new(ts));
        }
        self
    }

    /// Filter on `s.tag_id = ?` — for session-centric queries.
    pub fn with_session_tag(mut self, tag_id: Option<i64>) -> Self {
        if let Some(tid) = tag_id {
            self.clauses.push("s.tag_id = ?".into());
            self.reads_session = true;
            self.params.push(Box::new(tid));
        }
        self
    }

    /// Hide automated (agent/bot/ci/programmatic) commands when `exclude` is
    /// true. NULL/human/ide/unknown rows always pass. The excluded values are
    /// hardcoded constants ([`crate::models::ExecutorKind::NON_INTERACTIVE_DB_VALUES`]),
    /// so they are safe to inline into SQL and add no bound parameters.
    pub fn with_exclude_agents(mut self, exclude: bool) -> Self {
        if exclude {
            let list = crate::models::ExecutorKind::NON_INTERACTIVE_DB_VALUES
                .iter()
                .map(|v| format!("'{v}'"))
                .collect::<Vec<_>>()
                .join(", ");
            self.clauses.push(format!(
                "(e.executor_type IS NULL OR e.executor_type NOT IN ({list}))"
            ));
        }
        self
    }

    /// Restrict to commands an AI agent ran (`executor_type = 'agent'`).
    /// Narrower than [`Self::with_agents_only`], which also keeps bot, CI,
    /// IDE and programmatic rows. Hardcoded SQL, no parameters.
    pub fn with_agent_commands_only(mut self) -> Self {
        self.clauses.push("e.executor_type = 'agent'".into());
        self
    }

    /// Restrict to rows [`crate::models::Entry::is_agent`] accepts. The
    /// values are hardcoded constants
    /// ([`crate::models::ExecutorKind::AGENT_DB_VALUES`]), so they are safe
    /// to inline into SQL and add no bound parameters.
    pub fn with_agents_only(mut self, only: bool) -> Self {
        if only {
            let list = crate::models::ExecutorKind::AGENT_DB_VALUES
                .iter()
                .map(|v| format!("'{v}'"))
                .collect::<Vec<_>>()
                .join(", ");
            self.clauses.push(format!("e.executor_type IN ({list})"));
        }
        self
    }

    /// Restrict to commands that failed (non-zero, non-null exit code).
    pub fn with_failed_only(mut self, failed_only: bool) -> Self {
        if failed_only {
            self.clauses
                .push("(e.exit_code IS NOT NULL AND e.exit_code != 0)".into());
        }
        self
    }

    /// Restrict to bookmarked commands. Matches by command text against the
    /// `bookmarks` table (same key bookmarks are stored under). The subquery
    /// contains only hardcoded SQL and adds no bound parameters.
    pub fn with_bookmarked_only(mut self, bookmarked_only: bool) -> Self {
        if bookmarked_only {
            self.clauses
                .push("e.command IN (SELECT command FROM bookmarks)".into());
        }
        self
    }

    pub fn with_executor(mut self, executor: Option<&str>) -> Self {
        if let Some(exec) = executor {
            self.clauses.push(
                "(e.executor_type LIKE ? ESCAPE '\\' OR e.executor LIKE ? ESCAPE '\\' OR (e.executor_type || '-' || e.executor) LIKE ? ESCAPE '\\')".into(),
            );
            let escaped = escape_like(exec);
            let pattern = format!("%{escaped}%");
            self.params.push(Box::new(pattern.clone()));
            self.params.push(Box::new(pattern.clone()));
            self.params.push(Box::new(pattern));
        }
        self
    }

    /// Build a WHERE clause string with only hardcoded column names and `?`
    /// placeholders. User values are in `self.params`, bound separately via
    /// `params_refs()`. The returned string is safe to interpolate into SQL.
    pub fn build_where(&self) -> String {
        if self.clauses.is_empty() {
            " WHERE 1=1".into()
        } else {
            format!(" WHERE {}", self.clauses.join(" AND "))
        }
    }

    pub fn params_refs(&self) -> Vec<&dyn rusqlite::ToSql> {
        self.params
            .iter()
            .map(std::convert::AsRef::as_ref)
            .collect()
    }

    pub fn push_param(&mut self, val: Box<dyn rusqlite::ToSql>) {
        self.params.push(val);
    }
}

/// RAII guard for a database transaction. Automatically rolls back on drop
/// unless `commit()` is called, preventing dangling transactions on error or panic.
pub struct TransactionGuard<'a> {
    repo: &'a Repository,
    committed: bool,
}

impl<'a> TransactionGuard<'a> {
    fn new(repo: &'a Repository) -> DbResult<Self> {
        repo.begin_transaction()?;
        Ok(Self {
            repo,
            committed: false,
        })
    }

    /// Commit the current transaction.
    pub fn commit(mut self) -> DbResult<()> {
        self.committed = true;
        self.repo.commit()
    }

    /// Commit the current batch and start a new transaction.
    /// Useful for batched imports where periodic commits bound WAL growth.
    pub fn recommit(&self) -> DbResult<()> {
        self.repo.commit()?;
        self.repo.begin_transaction()?;
        Ok(())
    }
}

impl Drop for TransactionGuard<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.repo.rollback();
        }
    }
}

/// Repository for managing history entries and sessions
pub struct Repository {
    conn: Connection,
}

impl Repository {
    /// Create a new repository with the given connection
    pub const fn new(conn: Connection) -> Self {
        Self { conn }
    }

    /// Corrupt or hand-edit stored rows from a test, so contract fixtures
    /// can reproduce a malformed or future-versioned record without a
    /// second copy of the schema. Test-only on purpose: production code
    /// must go through a typed repository method.
    #[cfg(test)]
    pub fn raw_execute_for_test(&self, sql: &str) -> DbResult<usize> {
        Ok(self.conn.execute(sql, [])?)
    }

    /// Open the database and return a ready-to-use repository.
    pub fn init() -> crate::db::DbResult<Self> {
        let db_path = crate::db::get_db_path()?;
        let conn = crate::db::init_db(&db_path)?;
        Ok(Self::new(conn))
    }

    /// A read-only connection to the same database file, for recall's query
    /// worker (see [`crate::db::open_search_reader`]). `None` for an
    /// in-memory database, which a second connection cannot see.
    pub fn search_reader(&self) -> Option<Self> {
        let path = self.conn.path().filter(|p| !p.is_empty())?;
        crate::db::open_search_reader(std::path::Path::new(path))
            .ok()
            .map(Self::new)
    }

    /// A handle that can abort whatever statement this connection is running,
    /// from another thread.
    pub fn interrupt_handle(&self) -> rusqlite::InterruptHandle {
        self.conn.get_interrupt_handle()
    }

    /// Open the database at `db_path` in **read-only** mode. No migrations
    /// are run. Used by the MCP server to prevent accidental writes; takes
    /// an explicit path so it can be exercised against a temp DB in tests.
    pub fn init_read_only(db_path: &std::path::Path) -> crate::db::DbResult<Self> {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(db_path, flags)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Self::new(conn))
    }

    /// Insert a new session
    pub fn insert_session(&self, session: &Session) -> DbResult<()> {
        self.conn.execute(
            "INSERT INTO sessions (id, hostname, created_at, tag_id) VALUES (?1, ?2, ?3, ?4)",
            params![
                session.id,
                session.hostname,
                session.created_at,
                session.tag_id
            ],
        )?;
        Ok(())
    }

    /// Insert a session row only if `id` is not already present.
    /// Used by `suv import` to satisfy the entries → sessions FK when the export
    /// did not include the source machine's session metadata, and by `suv add`,
    /// where parallel commands of one new session race to create its row.
    pub fn insert_session_if_missing(
        &self,
        id: &str,
        hostname: &str,
        created_at: i64,
    ) -> DbResult<bool> {
        let changed = self.conn.execute(
            "INSERT OR IGNORE INTO sessions (id, hostname, created_at, tag_id) VALUES (?1, ?2, ?3, NULL)",
            params![id, hostname, created_at],
        )?;
        Ok(changed > 0)
    }

    /// Get a session by ID
    pub fn get_session(&self, id: &str) -> DbResult<Option<Session>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, hostname, created_at, tag_id FROM sessions WHERE id = ?1")?;

        let mut rows = stmt.query(params![id])?;

        if let Some(row) = rows.next()? {
            Ok(Some(Session {
                id: row.get(0)?,
                hostname: row.get(1)?,
                created_at: row.get(2)?,
                tag_id: row.get(3)?,
            }))
        } else {
            Ok(None)
        }
    }

    /// Begin a transaction (for batch operations)
    pub fn begin_transaction(&self) -> DbResult<()> {
        self.conn.execute_batch("BEGIN")?;
        Ok(())
    }

    /// Commit a transaction
    pub fn commit(&self) -> DbResult<()> {
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    /// Roll back a transaction (undo all changes since `begin_transaction`)
    pub fn rollback(&self) -> DbResult<()> {
        self.conn.execute_batch("ROLLBACK")?;
        Ok(())
    }

    /// Start a RAII-guarded transaction. The transaction is automatically rolled
    /// back if the guard is dropped without calling `commit()` (e.g. on error or panic).
    pub fn transaction(&self) -> DbResult<TransactionGuard<'_>> {
        TransactionGuard::new(self)
    }

    /// Write a compact, consistent copy of the database to `dest` using
    /// `VACUUM INTO`. `dest` must not already exist. On Unix the copy is
    /// restricted to owner-only since it contains shell history.
    pub fn backup_to(&self, dest: &std::path::Path) -> DbResult<()> {
        // VACUUM INTO needs a string literal (no bind params); escape quotes.
        let escaped = dest.to_string_lossy().replace('\'', "''");
        self.conn
            .execute_batch(&format!("VACUUM INTO '{escaped}'"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    /// Check if a (command, `started_at`) pair already exists in the database.
    /// Used during import dedup — avoids loading the entire history into memory.
    pub fn entry_exists(&self, command: &str, started_at: i64) -> DbResult<bool> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT 1 FROM entries WHERE command = ? AND started_at = ? LIMIT 1")?;
        let exists = stmt.exists(params![command, started_at])?;
        Ok(exists)
    }

    /// Count rows of `command` with `from_ms <= started_at < to_ms`. Used by
    /// the zsh import to match whole-second history timestamps against rows
    /// stored with millisecond precision (live capture, earlier imports).
    pub fn count_entries_between(&self, command: &str, from_ms: i64, to_ms: i64) -> DbResult<i64> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT COUNT(*) FROM entries WHERE command = ? AND started_at >= ? AND started_at < ?",
        )?;
        let count = stmt.query_row(params![command, from_ms, to_ms], |row| row.get(0))?;
        Ok(count)
    }
}

#[cfg(test)]
mod filter_builder_tests {
    use super::{expand_tilde, FilterBuilder};
    use crate::models::SearchField;

    #[test]
    fn empty_builder_produces_where_1_eq_1() {
        let fb = FilterBuilder::new();
        assert_eq!(fb.build_where(), " WHERE 1=1");
        assert!(fb.params_refs().is_empty());
    }

    #[test]
    fn with_date_range_after_only() {
        let fb = FilterBuilder::new().with_date_range(Some(1000), None);
        assert_eq!(fb.build_where(), " WHERE e.started_at >= ?");
        assert_eq!(fb.params_refs().len(), 1);
    }

    #[test]
    fn with_date_range_before_only() {
        let fb = FilterBuilder::new().with_date_range(None, Some(2000));
        assert_eq!(fb.build_where(), " WHERE e.started_at <= ?");
        assert_eq!(fb.params_refs().len(), 1);
    }

    #[test]
    fn with_date_range_both() {
        let fb = FilterBuilder::new().with_date_range(Some(1000), Some(2000));
        assert_eq!(
            fb.build_where(),
            " WHERE e.started_at >= ? AND e.started_at <= ?"
        );
        assert_eq!(fb.params_refs().len(), 2);
    }

    #[test]
    fn with_tag_adds_two_params() {
        let fb = FilterBuilder::new().with_tag(Some(42));
        assert_eq!(fb.build_where(), " WHERE (s.tag_id = ? OR e.tag_id = ?)");
        assert_eq!(fb.params_refs().len(), 2);
    }

    #[test]
    fn with_tag_none_is_noop() {
        let fb = FilterBuilder::new().with_tag(None);
        assert_eq!(fb.build_where(), " WHERE 1=1");
        assert!(fb.params_refs().is_empty());
    }

    #[test]
    fn with_exit_code() {
        let fb = FilterBuilder::new().with_exit_code(Some(0));
        assert_eq!(fb.build_where(), " WHERE e.exit_code = ?");
        assert_eq!(fb.params_refs().len(), 1);
    }

    #[test]
    fn with_query_contains_mode() {
        // Non-prefix Command search routes through the entries_fts trigram
        // index (see push_command_contains) instead of a plain column LIKE.
        let fb = FilterBuilder::new().with_query(Some("git"), false);
        assert_eq!(
            fb.build_where(),
            " WHERE e.id IN (SELECT rowid FROM entries_fts WHERE command LIKE ?)"
        );
        assert_eq!(fb.params_refs().len(), 1);
    }

    #[test]
    fn with_query_prefix_mode() {
        let fb = FilterBuilder::new().with_query(Some("git"), true);
        assert_eq!(fb.build_where(), " WHERE e.command LIKE ? ESCAPE '\\'");
        assert_eq!(fb.params_refs().len(), 1);
    }

    #[test]
    fn with_query_field_cwd() {
        let fb = FilterBuilder::new().with_query_field(Some("home"), false, SearchField::Cwd);
        assert_eq!(fb.build_where(), " WHERE e.cwd LIKE ? ESCAPE '\\'");
    }

    #[test]
    fn with_query_field_session() {
        let fb = FilterBuilder::new().with_query_field(Some("abc"), false, SearchField::Session);
        assert_eq!(fb.build_where(), " WHERE e.session_id LIKE ? ESCAPE '\\'");
    }

    #[test]
    fn with_query_field_executor() {
        let fb =
            FilterBuilder::new().with_query_field(Some("claude"), false, SearchField::Executor);
        assert!(fb.build_where().contains("COALESCE"));
        assert!(fb.build_where().contains("ESCAPE '\\'"));
    }

    #[test]
    fn with_cwd() {
        let fb = FilterBuilder::new().with_cwd(Some("/home/user"));
        assert_eq!(fb.build_where(), " WHERE e.cwd = ?");
        assert_eq!(fb.params_refs().len(), 1);
    }

    #[test]
    fn with_session() {
        let fb = FilterBuilder::new().with_session(Some("sess-123"));
        assert_eq!(fb.build_where(), " WHERE e.session_id = ?");
        assert_eq!(fb.params_refs().len(), 1);
    }

    #[test]
    fn with_failed_only() {
        let fb = FilterBuilder::new().with_failed_only(true);
        assert_eq!(
            fb.build_where(),
            " WHERE (e.exit_code IS NOT NULL AND e.exit_code != 0)"
        );
        assert!(fb.params_refs().is_empty());
    }

    #[test]
    fn with_failed_only_false_is_noop() {
        let fb = FilterBuilder::new().with_failed_only(false);
        assert_eq!(fb.build_where(), " WHERE 1=1");
    }

    #[test]
    fn with_cwd_subtree_matches_dir_and_children() {
        let fb = FilterBuilder::new().with_cwd_subtree("/home/u/proj");
        assert_eq!(
            fb.build_where(),
            " WHERE (e.cwd = ? OR (e.cwd >= ? AND e.cwd < ?))"
        );
        assert_eq!(fb.params_refs().len(), 3);
        assert_eq!(
            super::subtree_bounds("/home/u/proj/"),
            (
                "/home/u/proj".to_string(),
                "/home/u/proj/".to_string(),
                "/home/u/proj0".to_string()
            )
        );
    }

    #[test]
    fn with_cwd_mode_exact_vs_prefix() {
        // prefix=false → exact
        let exact = FilterBuilder::new().with_cwd_mode(Some("/p"), false);
        assert_eq!(exact.build_where(), " WHERE e.cwd = ?");
        // prefix=true → subtree
        let sub = FilterBuilder::new().with_cwd_mode(Some("/p"), true);
        assert!(sub.build_where().contains("e.cwd >= ? AND e.cwd < ?"));
        // None → no clause regardless of prefix
        let none = FilterBuilder::new().with_cwd_mode(None, true);
        assert_eq!(none.build_where(), " WHERE 1=1");
    }

    #[test]
    fn with_excluded_dirs_negates_subtree_match_per_dir() {
        let dirs = vec!["/secrets".to_string(), "/home/x/.ssh".to_string()];
        let fb = FilterBuilder::new().with_excluded_dirs(&dirs);
        let where_clause = fb.build_where();
        assert_eq!(where_clause.matches("NOT (e.cwd = ?").count(), 2);
        // 2 params (exact + subtree) per excluded dir.
        assert_eq!(fb.params_refs().len(), 4);
    }

    #[test]
    fn with_excluded_dirs_empty_list_is_a_noop() {
        let fb = FilterBuilder::new().with_excluded_dirs(&[]);
        assert_eq!(fb.build_where(), " WHERE 1=1");
        assert!(fb.params_refs().is_empty());
    }

    #[test]
    fn with_excluded_dirs_skips_blank_entries() {
        // A blank/whitespace-only entry must never turn into an unconditional
        // `NOT (e.cwd = '' OR ...)` clause that (harmlessly, but pointlessly)
        // touches every row - it should just be ignored.
        let dirs = vec![String::new(), "  ".to_string()];
        let fb = FilterBuilder::new().with_excluded_dirs(&dirs);
        assert_eq!(fb.build_where(), " WHERE 1=1");
        assert!(fb.params_refs().is_empty());
    }

    #[test]
    fn expand_tilde_prefixes_with_home() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let home = home.to_string_lossy().into_owned();
        assert_eq!(expand_tilde("~/.ssh"), format!("{home}/.ssh"));
        assert_eq!(expand_tilde("~"), home);
        // No leading `~` - left untouched.
        assert_eq!(expand_tilde("/secrets"), "/secrets");
        // `~` not followed by `/` isn't a home reference (e.g. a username).
        assert_eq!(expand_tilde("~other"), "~other");
    }

    #[test]
    fn with_executor() {
        let fb = FilterBuilder::new().with_executor(Some("claude"));
        assert!(fb.build_where().contains("e.executor_type LIKE ?"));
        assert_eq!(fb.params_refs().len(), 3);
    }

    #[test]
    fn chained_filters() {
        let fb = FilterBuilder::new()
            .with_date_range(Some(1000), None)
            .with_tag(Some(5))
            .with_exit_code(Some(0))
            .with_query(Some("cargo"), false);
        let where_clause = fb.build_where();
        assert!(where_clause.contains("e.started_at >= ?"));
        assert!(where_clause.contains("s.tag_id = ? OR e.tag_id = ?"));
        assert!(where_clause.contains("e.exit_code = ?"));
        assert!(where_clause.contains("entries_fts WHERE command LIKE ?"));
        // 1 (date) + 2 (tag) + 1 (exit) + 1 (query) = 5
        assert_eq!(fb.params_refs().len(), 5);
    }

    #[test]
    fn push_param_adds_to_list() {
        let mut fb = FilterBuilder::new();
        fb.push_param(Box::new(42_i64));
        assert_eq!(fb.params_refs().len(), 1);
    }

    #[test]
    fn all_none_filters_produce_no_clauses() {
        let fb = FilterBuilder::new()
            .with_date_range(None, None)
            .with_tag(None)
            .with_exit_code(None)
            .with_query(None, false)
            .with_cwd(None)
            .with_session(None)
            .with_executor(None);
        assert_eq!(fb.build_where(), " WHERE 1=1");
        assert!(fb.params_refs().is_empty());
    }

    #[test]
    fn with_session_created_after() {
        let fb = FilterBuilder::new().with_session_created_after(Some(5000));
        assert_eq!(fb.build_where(), " WHERE s.created_at >= ?");
        assert_eq!(fb.params_refs().len(), 1);
    }

    #[test]
    fn with_session_created_after_none_is_noop() {
        let fb = FilterBuilder::new().with_session_created_after(None);
        assert_eq!(fb.build_where(), " WHERE 1=1");
        assert!(fb.params_refs().is_empty());
    }

    #[test]
    fn with_session_tag() {
        let fb = FilterBuilder::new().with_session_tag(Some(7));
        assert_eq!(fb.build_where(), " WHERE s.tag_id = ?");
        assert_eq!(fb.params_refs().len(), 1);
    }

    #[test]
    fn with_session_tag_none_is_noop() {
        let fb = FilterBuilder::new().with_session_tag(None);
        assert_eq!(fb.build_where(), " WHERE 1=1");
        assert!(fb.params_refs().is_empty());
    }

    #[test]
    fn chained_session_filters() {
        let fb = FilterBuilder::new()
            .with_session_created_after(Some(1000))
            .with_session_tag(Some(3));
        let where_clause = fb.build_where();
        assert!(where_clause.contains("s.created_at >= ?"));
        assert!(where_clause.contains("s.tag_id = ?"));
        assert_eq!(fb.params_refs().len(), 2);
    }
}
