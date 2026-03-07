use crate::db::DbResult;
use crate::models::Stats;

use super::Repository;

/// Reusable filter state for stats queries.
struct StatsFilter {
    time_filter: Option<i64>,
    tag_id: Option<i64>,
    join_clause: &'static str,
    where_clause: String,
}

impl StatsFilter {
    fn new(days: Option<usize>, tag_id: Option<i64>) -> Self {
        let time_filter = days.map(|d| {
            let now = chrono::Utc::now().timestamp_millis();
            now - i64::try_from(d)
                .unwrap_or(i64::MAX)
                .saturating_mul(86_400_000)
        });

        let join_clause = if tag_id.is_some() {
            " JOIN sessions s ON e.session_id = s.id"
        } else {
            ""
        };

        let mut conditions: Vec<&str> = Vec::new();
        if time_filter.is_some() {
            conditions.push("e.started_at >= ?");
        }
        if tag_id.is_some() {
            conditions.push("(s.tag_id = ? OR e.tag_id = ?)");
        }
        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conditions.join(" AND "))
        };

        Self {
            time_filter,
            tag_id,
            join_clause,
            where_clause,
        }
    }

    /// Bind the base filter parameters (time, then `tag_id` twice).
    /// Returns the next free parameter index.
    fn bind(&self, stmt: &mut rusqlite::Statement) -> rusqlite::Result<usize> {
        let mut idx: usize = 1;
        if let Some(ts) = self.time_filter {
            stmt.raw_bind_parameter(idx, ts)?;
            idx += 1;
        }
        if let Some(tid) = self.tag_id {
            stmt.raw_bind_parameter(idx, tid)?;
            idx += 1;
            stmt.raw_bind_parameter(idx, tid)?;
            idx += 1;
        }
        Ok(idx)
    }

    /// Append an extra condition, choosing AND or WHERE as needed.
    fn where_with_extra(&self, extra: &str) -> String {
        format!(
            "{}{}",
            self.where_clause,
            if self.where_clause.is_empty() {
                format!(" WHERE {extra}")
            } else {
                format!(" AND {extra}")
            }
        )
    }
}

impl Repository {
    /// Execute a scalar aggregate query using the given filter and return a
    /// single `i64` value.
    fn query_scalar(&self, sql: &str, filter: &StatsFilter) -> DbResult<i64> {
        let mut stmt = self.conn.prepare(sql)?;
        filter.bind(&mut stmt)?;
        let val = stmt
            .raw_query()
            .next()?
            .ok_or(crate::db::DbError::Validation(
                "Expected row from aggregate query".into(),
            ))?
            .get(0)?;
        Ok(val)
    }

    /// Execute a grouped query that returns `(String, i64)` pairs.
    /// The filter parameters are bound first, then `top_n` is bound as a LIMIT.
    fn query_grouped(
        &self,
        sql: &str,
        filter: &StatsFilter,
        top_n: Option<usize>,
    ) -> DbResult<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(sql)?;
        let next_idx = filter.bind(&mut stmt)?;
        if let Some(n) = top_n {
            stmt.raw_bind_parameter(next_idx, i64::try_from(n).unwrap_or(i64::MAX))?;
        }
        let mut rows = stmt.raw_query();
        let mut results = Vec::new();
        while let Some(row) = rows.next()? {
            results.push((row.get(0)?, row.get(1)?));
        }
        Ok(results)
    }

    /// Query the hourly distribution of commands.
    fn query_hourly(&self, filter: &StatsFilter) -> DbResult<Vec<(u32, i64)>> {
        let sql = format!(
            "SELECT CAST(strftime('%H', datetime(e.started_at/1000, 'unixepoch', 'localtime')) \
             AS INTEGER) as hour, COUNT(*) as cnt FROM entries e{}{} GROUP BY hour ORDER BY hour",
            filter.join_clause, filter.where_clause
        );
        let mut stmt = self.conn.prepare(&sql)?;
        filter.bind(&mut stmt)?;
        let mut rows = stmt.raw_query();
        let mut results = Vec::new();
        while let Some(row) = rows.next()? {
            if let Some(h) = row.get::<_, Option<i64>>(0)? {
                let hour = u32::try_from(h).unwrap_or(0);
                results.push((hour, row.get(1)?));
            }
        }
        Ok(results)
    }

    /// Get aggregated usage statistics, optionally filtered by tag.
    pub fn get_stats(
        &self,
        days: Option<usize>,
        top_n: usize,
        tag_id: Option<i64>,
    ) -> DbResult<Stats> {
        let f = StatsFilter::new(days, tag_id);

        let total_commands = self.query_scalar(
            &format!(
                "SELECT COUNT(*) FROM entries e{}{}",
                f.join_clause, f.where_clause
            ),
            &f,
        )?;

        let unique_commands = self.query_scalar(
            &format!(
                "SELECT COUNT(DISTINCT command) FROM entries e{}{}",
                f.join_clause, f.where_clause
            ),
            &f,
        )?;

        let success_count = self.query_scalar(
            &format!(
                "SELECT COUNT(*) FROM entries e{}{}",
                f.join_clause,
                f.where_with_extra("e.exit_code = 0")
            ),
            &f,
        )?;

        let failure_count = self.query_scalar(
            &format!(
                "SELECT COUNT(*) FROM entries e{}{}",
                f.join_clause,
                f.where_with_extra("exit_code IS NOT NULL AND exit_code != 0")
            ),
            &f,
        )?;

        let avg_duration_ms = self.query_scalar(
            &format!(
                "SELECT COALESCE(CAST(AVG(duration_ms) AS INTEGER), 0) FROM entries e{}{}",
                f.join_clause, f.where_clause
            ),
            &f,
        )?;

        let top_commands = self.query_grouped(
            &format!(
                "SELECT command, COUNT(*) as cnt FROM entries e{}{} \
                 GROUP BY command ORDER BY cnt DESC LIMIT ?",
                f.join_clause, f.where_clause
            ),
            &f,
            Some(top_n),
        )?;

        let top_directories = self.query_grouped(
            &format!(
                "SELECT cwd, COUNT(*) as cnt FROM entries e{}{} \
                 GROUP BY cwd ORDER BY cnt DESC LIMIT ?",
                f.join_clause, f.where_clause
            ),
            &f,
            Some(top_n),
        )?;

        let hourly_distribution = self.query_hourly(&f)?;

        let executor_breakdown = self.query_grouped(
            &format!(
                "SELECT COALESCE(e.executor_type, 'human') as exec_type, COUNT(*) as cnt \
                 FROM entries e{}{} GROUP BY exec_type ORDER BY cnt DESC",
                f.join_clause, f.where_clause
            ),
            &f,
            None,
        )?;

        Ok(Stats {
            total_commands,
            unique_commands,
            success_count,
            failure_count,
            avg_duration_ms,
            top_commands,
            top_directories,
            hourly_distribution,
            executor_breakdown,
            period_days: days,
        })
    }

    /// Get daily command counts for the heatmap and trend chart.
    /// Returns `(date_string, day_of_week 0=Sun..6=Sat, count)`.
    pub fn get_daily_activity(
        &self,
        days: usize,
        tag_id: Option<i64>,
    ) -> DbResult<Vec<(String, u32, i64)>> {
        let now = chrono::Utc::now().timestamp_millis();
        let since = now
            - i64::try_from(days)
                .unwrap_or(i64::MAX)
                .saturating_mul(86_400_000);

        let join_clause = if tag_id.is_some() {
            " JOIN sessions s ON e.session_id = s.id"
        } else {
            ""
        };
        let tag_filter = if tag_id.is_some() {
            " AND (s.tag_id = ?2 OR e.tag_id = ?2)"
        } else {
            ""
        };

        let sql = format!(
            "SELECT \
                date(e.started_at/1000, 'unixepoch', 'localtime') as day, \
                CAST(strftime('%w', datetime(e.started_at/1000, 'unixepoch', 'localtime')) AS INTEGER) as dow, \
                COUNT(*) as cnt \
            FROM entries e{join_clause} \
            WHERE e.started_at >= ?1{tag_filter} \
            GROUP BY day \
            ORDER BY day ASC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        stmt.raw_bind_parameter(1, since)?;
        if let Some(tid) = tag_id {
            stmt.raw_bind_parameter(2, tid)?;
        }
        let mut rows = stmt.raw_query();
        let mut results = Vec::new();
        while let Some(row) = rows.next()? {
            let day: String = row.get(0)?;
            let dow: u32 = row
                .get::<_, i64>(1)
                .map(|v| u32::try_from(v).unwrap_or(0))?;
            let cnt: i64 = row.get(2)?;
            results.push((day, dow, cnt));
        }
        Ok(results)
    }

    /// Get frequently-used commands for alias suggestion.
    #[allow(clippy::cast_precision_loss)]
    pub fn get_frequent_commands_filtered(
        &self,
        days: Option<usize>,
        min_count: usize,
        min_length: usize,
        limit: usize,
        human_only: bool,
    ) -> DbResult<Vec<(String, i64, i64)>> {
        // Half-life: 30 days in ms. Commands used 30 days ago get ~50% recency weight.
        const HALF_LIFE_MS: f64 = 30.0 * 86_400_000.0;

        let now_ms = chrono::Utc::now().timestamp_millis();
        let time_filter = days.map(|d| {
            now_ms
                - i64::try_from(d)
                    .unwrap_or(i64::MAX)
                    .saturating_mul(86_400_000)
        });

        let mut conditions = vec!["LENGTH(e.command) >= ?1".to_string()];
        let mut param_idx: usize = 2;

        if time_filter.is_some() {
            conditions.push(format!("e.started_at >= ?{param_idx}"));
            param_idx += 1;
        }

        if human_only {
            conditions.push(
                "(e.executor_type IS NULL OR e.executor_type = 'human' OR e.executor_type = 'unknown')"
                    .to_string(),
            );
        }

        let where_clause = format!(" WHERE {}", conditions.join(" AND "));
        let having_idx = param_idx;
        let limit_idx = param_idx + 1;

        // Fetch candidates with MAX(started_at) for recency weighting
        let sql = format!(
            "SELECT e.command, COUNT(*) as cnt, COUNT(DISTINCT e.cwd) as dir_cnt, \
             MAX(e.started_at) as last_used \
             FROM entries e{where_clause} \
             GROUP BY e.command HAVING cnt >= ?{having_idx} \
             ORDER BY cnt DESC LIMIT ?{limit_idx}"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let min_len_i64 = i64::try_from(min_length).unwrap_or(i64::MAX);
        let min_cnt_i64 = i64::try_from(min_count).unwrap_or(i64::MAX);
        // Fetch more candidates than needed, then rank in Rust
        let fetch_limit = i64::try_from(limit).unwrap_or(i64::MAX).saturating_mul(3);

        let mut bind_idx: usize = 1;
        stmt.raw_bind_parameter(bind_idx, min_len_i64)?;
        bind_idx += 1;
        if let Some(ts) = time_filter {
            stmt.raw_bind_parameter(bind_idx, ts)?;
            bind_idx += 1;
        }
        stmt.raw_bind_parameter(bind_idx, min_cnt_i64)?;
        bind_idx += 1;
        stmt.raw_bind_parameter(bind_idx, fetch_limit)?;

        let mut rows = stmt.raw_query();
        let mut candidates: Vec<(String, i64, i64, i64)> = Vec::new();
        while let Some(row) = rows.next()? {
            candidates.push((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?));
        }

        // Rank by frequency x diversity x recency
        let mut scored: Vec<(String, i64, i64, f64)> = candidates
            .into_iter()
            .map(|(cmd, cnt, dir_cnt, last_used)| {
                let age_ms = (now_ms - last_used).max(0) as f64;
                let recency = 0.5_f64.powf(age_ms / HALF_LIFE_MS);
                let diversity = dir_cnt.min(5) as f64;
                let score = cnt as f64 * diversity * 0.5f64.mul_add(recency, 0.5);
                (cmd, cnt, dir_cnt, score)
            })
            .collect();

        scored.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        Ok(scored
            .into_iter()
            .map(|(cmd, cnt, dir, _)| (cmd, cnt, dir))
            .collect())
    }
}
