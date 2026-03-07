use crate::db::DbResult;
use crate::models::Stats;

use super::Repository;

impl Repository {
    /// Get aggregated usage statistics
    #[allow(clippy::too_many_lines)]
    pub fn get_stats(&self, days: Option<usize>, top_n: usize) -> DbResult<Stats> {
        let time_filter = days.map(|d| {
            let now = chrono::Utc::now().timestamp_millis();
            now - i64::try_from(d)
                .unwrap_or(i64::MAX)
                .saturating_mul(86_400_000)
        });
        let where_clause = match time_filter {
            Some(_) => " WHERE e.started_at >= ?1",
            None => "",
        };
        let bind = |stmt: &mut rusqlite::Statement| -> rusqlite::Result<()> {
            if let Some(ts) = time_filter {
                stmt.raw_bind_parameter(1, ts)?;
            }
            Ok(())
        };

        // Total commands
        let total_commands: i64 = {
            let sql = format!("SELECT COUNT(*) FROM entries e{where_clause}");
            let mut stmt = self.conn.prepare(&sql)?;
            bind(&mut stmt)?;
            let val = stmt
                .raw_query()
                .next()?
                .ok_or(crate::db::DbError::Validation(
                    "Expected row from aggregate query".into(),
                ))?
                .get(0)?;
            val
        };

        // Unique commands
        let unique_commands: i64 = {
            let sql = format!("SELECT COUNT(DISTINCT command) FROM entries e{where_clause}");
            let mut stmt = self.conn.prepare(&sql)?;
            bind(&mut stmt)?;
            let val = stmt
                .raw_query()
                .next()?
                .ok_or(crate::db::DbError::Validation(
                    "Expected row from aggregate query".into(),
                ))?
                .get(0)?;
            val
        };

        // Success / failure
        let success_count: i64 = {
            let extra = if where_clause.is_empty() {
                " WHERE e.exit_code = 0"
            } else {
                " AND e.exit_code = 0"
            };
            let sql = format!("SELECT COUNT(*) FROM entries e{where_clause}{extra}");
            let mut stmt = self.conn.prepare(&sql)?;
            bind(&mut stmt)?;
            let val = stmt
                .raw_query()
                .next()?
                .ok_or(crate::db::DbError::Validation(
                    "Expected row from aggregate query".into(),
                ))?
                .get(0)?;
            val
        };
        let failure_count = total_commands - success_count;

        // Average duration
        let avg_duration_ms: i64 = {
            let sql =
                format!("SELECT COALESCE(CAST(AVG(duration_ms) AS INTEGER), 0) FROM entries e{where_clause}");
            let mut stmt = self.conn.prepare(&sql)?;
            bind(&mut stmt)?;
            let val = stmt
                .raw_query()
                .next()?
                .ok_or(crate::db::DbError::Validation(
                    "Expected row from aggregate query".into(),
                ))?
                .get(0)?;
            val
        };

        // Top commands
        let top_commands: Vec<(String, i64)> = {
            let sql = format!(
                "SELECT command, COUNT(*) as cnt FROM entries e{where_clause} GROUP BY command ORDER BY cnt DESC LIMIT ?{}",
                if time_filter.is_some() { "2" } else { "1" }
            );
            let mut stmt = self.conn.prepare(&sql)?;
            bind(&mut stmt)?;
            let param_idx = if time_filter.is_some() { 2 } else { 1 };
            stmt.raw_bind_parameter(param_idx, i64::try_from(top_n).unwrap_or(i64::MAX))?;
            let mut rows = stmt.raw_query();
            let mut results = Vec::new();
            while let Some(row) = rows.next()? {
                results.push((row.get(0)?, row.get(1)?));
            }
            results
        };

        // Top directories
        let top_directories: Vec<(String, i64)> = {
            let sql = format!(
                "SELECT cwd, COUNT(*) as cnt FROM entries e{where_clause} GROUP BY cwd ORDER BY cnt DESC LIMIT ?{}",
                if time_filter.is_some() { "2" } else { "1" }
            );
            let mut stmt = self.conn.prepare(&sql)?;
            bind(&mut stmt)?;
            let param_idx = if time_filter.is_some() { 2 } else { 1 };
            stmt.raw_bind_parameter(param_idx, i64::try_from(top_n).unwrap_or(i64::MAX))?;
            let mut rows = stmt.raw_query();
            let mut results = Vec::new();
            while let Some(row) = rows.next()? {
                results.push((row.get(0)?, row.get(1)?));
            }
            results
        };

        // Hourly distribution
        let hourly_distribution: Vec<(u32, i64)> = {
            let sql = format!(
                "SELECT CAST(strftime('%H', datetime(e.started_at/1000, 'unixepoch', 'localtime')) AS INTEGER) as hour, \
                 COUNT(*) as cnt FROM entries e{where_clause} GROUP BY hour ORDER BY hour"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            bind(&mut stmt)?;
            let mut rows = stmt.raw_query();
            let mut results = Vec::new();
            while let Some(row) = rows.next()? {
                if let Some(h) = row.get::<_, Option<i64>>(0)? {
                    let hour = u32::try_from(h).unwrap_or(0);
                    results.push((hour, row.get(1)?));
                }
            }
            results
        };

        // Executor breakdown
        let executor_breakdown: Vec<(String, i64)> = {
            let sql = format!(
                "SELECT COALESCE(e.executor_type, 'human') as exec_type, COUNT(*) as cnt \
                 FROM entries e{where_clause} GROUP BY exec_type ORDER BY cnt DESC"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            bind(&mut stmt)?;
            let mut rows = stmt.raw_query();
            let mut results = Vec::new();
            while let Some(row) = rows.next()? {
                results.push((row.get(0)?, row.get(1)?));
            }
            results
        };

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
    pub fn get_daily_activity(&self, days: usize) -> DbResult<Vec<(String, u32, i64)>> {
        let now = chrono::Utc::now().timestamp_millis();
        let since = now
            - i64::try_from(days)
                .unwrap_or(i64::MAX)
                .saturating_mul(86_400_000);
        let sql = "SELECT \
                date(e.started_at/1000, 'unixepoch', 'localtime') as day, \
                CAST(strftime('%w', datetime(e.started_at/1000, 'unixepoch', 'localtime')) AS INTEGER) as dow, \
                COUNT(*) as cnt \
            FROM entries e \
            WHERE e.started_at >= ?1 \
            GROUP BY day \
            ORDER BY day ASC";
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query(rusqlite::params![since])?;
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
    /// Returns `(command, count, dir_count)` tuples filtered by minimum length and count.
    /// Results are ranked by frequency × directory diversity × recency:
    ///   `score = count * min(dir_count, 5) * recency_weight`
    /// where `recency_weight` boosts commands used recently (half-life = 30 days).
    #[allow(clippy::cast_precision_loss)]
    pub fn get_frequent_commands(
        &self,
        days: Option<usize>,
        min_count: usize,
        min_length: usize,
        limit: usize,
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

        let where_clause = if time_filter.is_some() {
            " WHERE LENGTH(e.command) >= ?1 AND e.started_at >= ?2"
        } else {
            " WHERE LENGTH(e.command) >= ?1"
        };

        let having_param = if time_filter.is_some() { "?3" } else { "?2" };
        let limit_param = if time_filter.is_some() { "?4" } else { "?3" };

        // Fetch candidates with MAX(started_at) for recency weighting
        let sql = format!(
            "SELECT e.command, COUNT(*) as cnt, COUNT(DISTINCT e.cwd) as dir_cnt, \
             MAX(e.started_at) as last_used \
             FROM entries e{where_clause} \
             GROUP BY e.command HAVING cnt >= {having_param} \
             ORDER BY cnt DESC LIMIT {limit_param}"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let min_len_i64 = i64::try_from(min_length).unwrap_or(i64::MAX);
        let min_cnt_i64 = i64::try_from(min_count).unwrap_or(i64::MAX);
        // Fetch more candidates than needed, then rank in Rust
        let fetch_limit = i64::try_from(limit).unwrap_or(i64::MAX).saturating_mul(3);

        stmt.raw_bind_parameter(1, min_len_i64)?;
        if let Some(ts) = time_filter {
            stmt.raw_bind_parameter(2, ts)?;
            stmt.raw_bind_parameter(3, min_cnt_i64)?;
            stmt.raw_bind_parameter(4, fetch_limit)?;
        } else {
            stmt.raw_bind_parameter(2, min_cnt_i64)?;
            stmt.raw_bind_parameter(3, fetch_limit)?;
        }

        let mut rows = stmt.raw_query();
        let mut candidates: Vec<(String, i64, i64, i64)> = Vec::new();
        while let Some(row) = rows.next()? {
            candidates.push((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?));
        }

        // Rank by frequency × diversity × recency
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
