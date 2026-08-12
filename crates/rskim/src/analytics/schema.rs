//! Database schema and migrations for analytics.

use rusqlite::Connection;

/// Current schema version of this skim release.
///
/// AD-AN-5: `AnalyticsDb::open()` rejects databases whose `user_version` exceeds
/// this constant before any schema mutation, WAL flip, or chmod (AC3).
pub(super) const CURRENT_SCHEMA_VERSION: i64 = 4;

/// Run all database migrations.
pub(super) fn run_migrations(conn: &Connection) -> anyhow::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    if version < 1 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS token_savings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp INTEGER NOT NULL,
                command_type TEXT NOT NULL,
                original_cmd TEXT NOT NULL,
                raw_tokens INTEGER NOT NULL,
                compressed_tokens INTEGER NOT NULL,
                savings_pct REAL NOT NULL,
                duration_ms INTEGER NOT NULL,
                project_path TEXT NOT NULL,
                mode TEXT,
                language TEXT,
                parse_tier TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_ts_timestamp ON token_savings(timestamp);
            CREATE INDEX IF NOT EXISTS idx_ts_command_type ON token_savings(command_type);
            PRAGMA user_version = 1;",
        )?;
    }

    if version < 2 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS analytics_meta (
                key TEXT PRIMARY KEY,
                value INTEGER
            );
            PRAGMA user_version = 2;",
        )?;
    }

    if version < 3 {
        // AD-AN-4: session_id is nullable for backward compatibility — rows
        // recorded before this migration have NULL session_id and are excluded
        // from per-session average calculations.
        conn.execute_batch(
            "ALTER TABLE token_savings ADD COLUMN session_id TEXT;
            CREATE INDEX IF NOT EXISTS idx_ts_session_id ON token_savings(session_id);
            PRAGMA user_version = 3;",
        )?;
    }

    if version < 4 {
        // AD-AN-5: v4 migration — single contiguous BEGIN..COMMIT block.
        //
        // Nullable token columns (raw_tokens, compressed_tokens, savings_pct)
        // require a table rebuild because SQLite cannot ALTER COLUMN to drop a
        // NOT NULL constraint (the v1 schema declared them NOT NULL).
        //
        // PRAGMA user_version = 4 is the LAST statement in the block so any
        // interrupted rebuild — at any step including the proxy_block_decisions
        // detail-table DDL — rolls back the entire transaction leaving intact v3
        // (ADR-006: abort before persisting, old state survives). SQLite
        // PRAGMA user_version is transactional and rolls back with the BEGIN block.
        //
        // AD-AN-13: proxy_block_decisions detail table (savings_id FK →
        // token_savings(id)) and its index are created inside the same transaction
        // so the FK invariant is established atomically with the parent table.
        //
        // AC5/AC6: provider, model, turn_id, upstream_error_status are nullable
        // to support both CLI rows (always non-NULL tokens, NULL provider/model)
        // and proxy rows (NULL or non-NULL tokens, present provider/model/turn_id).
        conn.execute_batch(
            "BEGIN;
            CREATE TABLE token_savings_new (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp INTEGER NOT NULL,
                command_type TEXT NOT NULL,
                original_cmd TEXT NOT NULL,
                raw_tokens INTEGER,
                compressed_tokens INTEGER,
                savings_pct REAL,
                duration_ms INTEGER NOT NULL,
                project_path TEXT NOT NULL,
                mode TEXT,
                language TEXT,
                parse_tier TEXT,
                session_id TEXT,
                provider TEXT,
                model TEXT,
                turn_id TEXT,
                upstream_error_status INTEGER
            );
            INSERT INTO token_savings_new
                (id, timestamp, command_type, original_cmd,
                 raw_tokens, compressed_tokens, savings_pct,
                 duration_ms, project_path, mode, language,
                 parse_tier, session_id)
                SELECT id, timestamp, command_type, original_cmd,
                       raw_tokens, compressed_tokens, savings_pct,
                       duration_ms, project_path, mode, language,
                       parse_tier, session_id
                FROM token_savings;
            DROP TABLE token_savings;
            ALTER TABLE token_savings_new RENAME TO token_savings;
            CREATE INDEX IF NOT EXISTS idx_ts_timestamp ON token_savings(timestamp);
            CREATE INDEX IF NOT EXISTS idx_ts_command_type ON token_savings(command_type);
            CREATE INDEX IF NOT EXISTS idx_ts_session_id ON token_savings(session_id);
            CREATE INDEX IF NOT EXISTS idx_ts_provider_model ON token_savings(command_type, provider, model);
            CREATE TABLE proxy_block_decisions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                savings_id INTEGER NOT NULL REFERENCES token_savings(id),
                block_index INTEGER NOT NULL,
                component TEXT NOT NULL,
                outcome TEXT NOT NULL,
                bytes_in INTEGER NOT NULL,
                bytes_out INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_pbd_savings_id ON proxy_block_decisions(savings_id);
            PRAGMA user_version = 4;
            COMMIT;",
        )?;
    }

    Ok(())
}
