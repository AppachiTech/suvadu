use crate::db::DbResult;
use crate::models::{Entry, Session};

use super::Repository;

/// Object-safe trait exposing all `Repository` query/mutation methods.
///
/// This allows callers to program against `&dyn RepositoryApi` instead of
/// a concrete `Repository`, which is useful for testing (mock repositories)
/// and for decoupling business logic from the storage backend.
///
/// Currently used in tests; will be adopted by command handlers incrementally.
#[allow(dead_code)]
pub trait RepositoryApi {
    // ── mod.rs (core) ───────────────────────────────────────────────────
    fn insert_session(&self, session: &Session) -> DbResult<()>;
    fn get_session(&self, id: &str) -> DbResult<Option<Session>>;
    fn begin_transaction(&self) -> DbResult<()>;
    fn commit(&self) -> DbResult<()>;
    fn rollback(&self) -> DbResult<()>;
    fn entry_exists(&self, command: &str, started_at: i64) -> DbResult<bool>;

    // ── entries.rs ──────────────────────────────────────────────────────
    fn insert_entry(&self, entry: &Entry) -> DbResult<i64>;
    fn get_entries_filtered(
        &self,
        limit: usize,
        offset: usize,
        filter: &super::QueryFilter,
    ) -> DbResult<Vec<Entry>>;
    fn get_replay_entries(
        &self,
        session_id: Option<&str>,
        filter: &super::ReplayFilter,
    ) -> DbResult<Vec<Entry>>;
    fn get_unique_entries_filtered(
        &self,
        limit: usize,
        offset: usize,
        filter: &super::QueryFilter,
        sort_alphabetically: bool,
    ) -> DbResult<Vec<(Entry, i64)>>;
    fn get_recent_entries(
        &self,
        limit: usize,
        offset: usize,
        query: Option<&str>,
        prefix_match: bool,
        boost_cwd: Option<&str>,
    ) -> DbResult<Vec<Entry>>;
    fn count_unique_filtered(&self, filter: &super::QueryFilter) -> DbResult<i64>;
    fn delete_entries(
        &self,
        pattern: &str,
        is_regex: bool,
        before_timestamp: Option<i64>,
    ) -> DbResult<usize>;
    fn count_entries_by_pattern(
        &self,
        pattern: &str,
        is_regex: bool,
        before_timestamp: Option<i64>,
    ) -> DbResult<usize>;
    fn delete_entry(&self, id: i64) -> DbResult<()>;
    fn count_filtered(&self, filter: &super::QueryFilter) -> DbResult<i64>;
    fn count_orphaned_sessions(&self) -> DbResult<i64>;
    fn delete_orphaned_sessions(&self) -> DbResult<usize>;
    fn count_orphaned_notes(&self) -> DbResult<i64>;
    fn delete_orphaned_notes(&self) -> DbResult<usize>;
    fn list_sessions(
        &self,
        after: Option<i64>,
        tag_id: Option<i64>,
        limit: usize,
    ) -> DbResult<Vec<crate::models::SessionSummary>>;
    fn find_sessions_by_prefix(&self, prefix: &str) -> DbResult<Vec<String>>;
    fn vacuum(&self) -> DbResult<()>;

    // ── tags.rs ─────────────────────────────────────────────────────────
    fn create_tag(&self, name: &str, description: Option<&str>) -> DbResult<i64>;
    fn get_tags(&self) -> DbResult<Vec<crate::models::Tag>>;
    fn update_tag(&self, id: i64, name: &str, description: Option<&str>) -> DbResult<()>;
    fn get_tag_id_by_name(&self, name: &str) -> DbResult<Option<i64>>;
    fn tag_session(&self, session_id: &str, tag_id: Option<i64>) -> DbResult<()>;
    fn get_tag_by_session(&self, session_id: &str) -> DbResult<Option<String>>;

    // ── notes.rs ────────────────────────────────────────────────────────
    fn upsert_note(&self, entry_id: i64, note: &str) -> DbResult<()>;
    fn get_note(&self, entry_id: i64) -> DbResult<Option<crate::models::Note>>;
    fn delete_note(&self, entry_id: i64) -> DbResult<bool>;
    fn get_noted_entry_ids(&self) -> DbResult<std::collections::HashSet<i64>>;

    // ── bookmarks.rs ────────────────────────────────────────────────────
    fn add_bookmark(&self, command: &str, label: Option<&str>) -> DbResult<i64>;
    fn remove_bookmark(&self, command: &str) -> DbResult<bool>;
    fn list_bookmarks(&self) -> DbResult<Vec<crate::models::Bookmark>>;
    fn get_bookmarked_commands(&self) -> DbResult<std::collections::HashSet<String>>;

    // ── aliases.rs ──────────────────────────────────────────────────────
    fn add_alias(&self, name: &str, command: &str) -> DbResult<i64>;
    fn remove_alias(&self, name: &str) -> DbResult<bool>;
    fn list_aliases(&self) -> DbResult<Vec<crate::models::Alias>>;

    // ── stats.rs ────────────────────────────────────────────────────────
    fn get_stats(
        &self,
        days: Option<usize>,
        top_n: usize,
        tag_id: Option<i64>,
    ) -> DbResult<crate::models::Stats>;
    fn get_daily_activity(
        &self,
        days: usize,
        tag_id: Option<i64>,
    ) -> DbResult<Vec<(String, u32, i64)>>;
    fn get_frequent_commands_filtered(
        &self,
        days: Option<usize>,
        min_count: usize,
        min_length: usize,
        limit: usize,
        human_only: bool,
    ) -> DbResult<Vec<(String, i64, i64)>>;
}

// ─── Blanket impl: delegate every method to the inherent impl ───────────────

impl RepositoryApi for Repository {
    // ── mod.rs (core) ───────────────────────────────────────────────────

    fn insert_session(&self, session: &Session) -> DbResult<()> {
        Self::insert_session(self, session)
    }

    fn get_session(&self, id: &str) -> DbResult<Option<Session>> {
        Self::get_session(self, id)
    }

    fn begin_transaction(&self) -> DbResult<()> {
        Self::begin_transaction(self)
    }

    fn commit(&self) -> DbResult<()> {
        Self::commit(self)
    }

    fn rollback(&self) -> DbResult<()> {
        Self::rollback(self)
    }

    fn entry_exists(&self, command: &str, started_at: i64) -> DbResult<bool> {
        Self::entry_exists(self, command, started_at)
    }

    // ── entries.rs ──────────────────────────────────────────────────────

    fn insert_entry(&self, entry: &Entry) -> DbResult<i64> {
        Self::insert_entry(self, entry)
    }

    fn get_entries_filtered(
        &self,
        limit: usize,
        offset: usize,
        filter: &super::QueryFilter,
    ) -> DbResult<Vec<Entry>> {
        Self::get_entries_filtered(self, limit, offset, filter)
    }

    fn get_replay_entries(
        &self,
        session_id: Option<&str>,
        filter: &super::ReplayFilter,
    ) -> DbResult<Vec<Entry>> {
        Self::get_replay_entries(self, session_id, filter)
    }

    fn get_unique_entries_filtered(
        &self,
        limit: usize,
        offset: usize,
        filter: &super::QueryFilter,
        sort_alphabetically: bool,
    ) -> DbResult<Vec<(Entry, i64)>> {
        Self::get_unique_entries_filtered(self, limit, offset, filter, sort_alphabetically)
    }

    fn get_recent_entries(
        &self,
        limit: usize,
        offset: usize,
        query: Option<&str>,
        prefix_match: bool,
        boost_cwd: Option<&str>,
    ) -> DbResult<Vec<Entry>> {
        Self::get_recent_entries(self, limit, offset, query, prefix_match, boost_cwd)
    }

    fn count_unique_filtered(&self, filter: &super::QueryFilter) -> DbResult<i64> {
        Self::count_unique_filtered(self, filter)
    }

    fn delete_entries(
        &self,
        pattern: &str,
        is_regex: bool,
        before_timestamp: Option<i64>,
    ) -> DbResult<usize> {
        Self::delete_entries(self, pattern, is_regex, before_timestamp)
    }

    fn count_entries_by_pattern(
        &self,
        pattern: &str,
        is_regex: bool,
        before_timestamp: Option<i64>,
    ) -> DbResult<usize> {
        Self::count_entries_by_pattern(self, pattern, is_regex, before_timestamp)
    }

    fn delete_entry(&self, id: i64) -> DbResult<()> {
        Self::delete_entry(self, id)
    }

    fn count_filtered(&self, filter: &super::QueryFilter) -> DbResult<i64> {
        Self::count_filtered(self, filter)
    }

    fn count_orphaned_sessions(&self) -> DbResult<i64> {
        Self::count_orphaned_sessions(self)
    }

    fn delete_orphaned_sessions(&self) -> DbResult<usize> {
        Self::delete_orphaned_sessions(self)
    }

    fn count_orphaned_notes(&self) -> DbResult<i64> {
        Self::count_orphaned_notes(self)
    }

    fn delete_orphaned_notes(&self) -> DbResult<usize> {
        Self::delete_orphaned_notes(self)
    }

    fn list_sessions(
        &self,
        after: Option<i64>,
        tag_id: Option<i64>,
        limit: usize,
    ) -> DbResult<Vec<crate::models::SessionSummary>> {
        Self::list_sessions(self, after, tag_id, limit)
    }

    fn find_sessions_by_prefix(&self, prefix: &str) -> DbResult<Vec<String>> {
        Self::find_sessions_by_prefix(self, prefix)
    }

    fn vacuum(&self) -> DbResult<()> {
        Self::vacuum(self)
    }

    // ── tags.rs ─────────────────────────────────────────────────────────

    fn create_tag(&self, name: &str, description: Option<&str>) -> DbResult<i64> {
        Self::create_tag(self, name, description)
    }

    fn get_tags(&self) -> DbResult<Vec<crate::models::Tag>> {
        Self::get_tags(self)
    }

    fn update_tag(&self, id: i64, name: &str, description: Option<&str>) -> DbResult<()> {
        Self::update_tag(self, id, name, description)
    }

    fn get_tag_id_by_name(&self, name: &str) -> DbResult<Option<i64>> {
        Self::get_tag_id_by_name(self, name)
    }

    fn tag_session(&self, session_id: &str, tag_id: Option<i64>) -> DbResult<()> {
        Self::tag_session(self, session_id, tag_id)
    }

    fn get_tag_by_session(&self, session_id: &str) -> DbResult<Option<String>> {
        Self::get_tag_by_session(self, session_id)
    }

    // ── notes.rs ────────────────────────────────────────────────────────

    fn upsert_note(&self, entry_id: i64, note: &str) -> DbResult<()> {
        Self::upsert_note(self, entry_id, note)
    }

    fn get_note(&self, entry_id: i64) -> DbResult<Option<crate::models::Note>> {
        Self::get_note(self, entry_id)
    }

    fn delete_note(&self, entry_id: i64) -> DbResult<bool> {
        Self::delete_note(self, entry_id)
    }

    fn get_noted_entry_ids(&self) -> DbResult<std::collections::HashSet<i64>> {
        Self::get_noted_entry_ids(self)
    }

    // ── bookmarks.rs ────────────────────────────────────────────────────

    fn add_bookmark(&self, command: &str, label: Option<&str>) -> DbResult<i64> {
        Self::add_bookmark(self, command, label)
    }

    fn remove_bookmark(&self, command: &str) -> DbResult<bool> {
        Self::remove_bookmark(self, command)
    }

    fn list_bookmarks(&self) -> DbResult<Vec<crate::models::Bookmark>> {
        Self::list_bookmarks(self)
    }

    fn get_bookmarked_commands(&self) -> DbResult<std::collections::HashSet<String>> {
        Self::get_bookmarked_commands(self)
    }

    // ── aliases.rs ──────────────────────────────────────────────────────

    fn add_alias(&self, name: &str, command: &str) -> DbResult<i64> {
        Self::add_alias(self, name, command)
    }

    fn remove_alias(&self, name: &str) -> DbResult<bool> {
        Self::remove_alias(self, name)
    }

    fn list_aliases(&self) -> DbResult<Vec<crate::models::Alias>> {
        Self::list_aliases(self)
    }

    // ── stats.rs ────────────────────────────────────────────────────────

    fn get_stats(
        &self,
        days: Option<usize>,
        top_n: usize,
        tag_id: Option<i64>,
    ) -> DbResult<crate::models::Stats> {
        Self::get_stats(self, days, top_n, tag_id)
    }

    fn get_daily_activity(
        &self,
        days: usize,
        tag_id: Option<i64>,
    ) -> DbResult<Vec<(String, u32, i64)>> {
        Self::get_daily_activity(self, days, tag_id)
    }

    fn get_frequent_commands_filtered(
        &self,
        days: Option<usize>,
        min_count: usize,
        min_length: usize,
        limit: usize,
        human_only: bool,
    ) -> DbResult<Vec<(String, i64, i64)>> {
        Self::get_frequent_commands_filtered(self, days, min_count, min_length, limit, human_only)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::test_repo;

    /// Verify `Repository` satisfies `RepositoryApi` trait through `&dyn` dispatch.
    #[test]
    fn repo_implements_trait() {
        let (_dir, repo) = test_repo();
        let api: &dyn RepositoryApi = &repo;
        let tags = api.get_tags().unwrap();
        assert!(tags.is_empty());
    }

    /// Verify basic CRUD through the trait interface.
    #[test]
    fn trait_crud_roundtrip() {
        let (_dir, repo) = test_repo();
        let api: &dyn RepositoryApi = &repo;

        let session = Session {
            id: "trait-test".to_string(),
            hostname: "host".to_string(),
            created_at: 1_000_000,
            tag_id: None,
        };
        api.insert_session(&session).unwrap();
        let fetched = api.get_session("trait-test").unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().hostname, "host");
    }
}
