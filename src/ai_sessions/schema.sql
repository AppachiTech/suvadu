CREATE TABLE IF NOT EXISTS ai_sessions (
    id TEXT PRIMARY KEY, native_id TEXT NOT NULL, agent TEXT NOT NULL,
    cwd TEXT NOT NULL, parent_id TEXT, created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL, revision INTEGER NOT NULL DEFAULT 0,
    usage_complete INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE IF NOT EXISTS ai_events (
    session_id TEXT NOT NULL REFERENCES ai_sessions(id) ON DELETE CASCADE,
    event_id TEXT NOT NULL, kind TEXT NOT NULL, cwd TEXT NOT NULL,
    data TEXT NOT NULL, PRIMARY KEY(session_id, event_id)
);
CREATE INDEX IF NOT EXISTS ai_events_session ON ai_events(session_id);
-- Checkpoints contain adapter state only, including when capture is paused.
CREATE TABLE IF NOT EXISTS ai_sources (
    path TEXT PRIMARY KEY, session_id TEXT NOT NULL UNIQUE,
    identity TEXT NOT NULL, byte_offset INTEGER NOT NULL,
    state TEXT NOT NULL, adapter_version INTEGER NOT NULL,
    skipped INTEGER NOT NULL DEFAULT 0,
    gaps TEXT NOT NULL DEFAULT '{"all_before":0,"dirs":{}}'
);
CREATE TABLE IF NOT EXISTS ai_summaries (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES ai_sessions(id) ON DELETE CASCADE,
    source_revision TEXT NOT NULL, text TEXT NOT NULL,
    agent TEXT NOT NULL, model TEXT NOT NULL, source_ids TEXT NOT NULL,
    source_event_count INTEGER NOT NULL DEFAULT 0,
    source_command_count INTEGER NOT NULL DEFAULT 0,
    source_prefix_hash TEXT NOT NULL DEFAULT '',
    base_summary_id TEXT,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ai_summaries_session ON ai_summaries(session_id, created_at);

-- Command edits and removals invalidate summaries, as do new commands.
CREATE TRIGGER IF NOT EXISTS ai_command_insert AFTER INSERT ON entries BEGIN
    UPDATE ai_sessions SET revision=revision+1 WHERE id=NEW.session_id;
END;
CREATE TRIGGER IF NOT EXISTS ai_command_update AFTER UPDATE ON entries BEGIN
    UPDATE ai_sessions SET revision=revision+1 WHERE id=OLD.session_id OR id=NEW.session_id;
END;
CREATE TRIGGER IF NOT EXISTS ai_command_delete AFTER DELETE ON entries BEGIN
    UPDATE ai_sessions SET revision=revision+1 WHERE id=OLD.session_id;
END;
