//! Database schema and migrations for analytics.

use rusqlite::Connection;

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
        // AD-CA-9 / AD-AN-5: alignment_decisions table — records per-request
        // cache-alignment outcomes (tools sorted, markers injected, fail-open flag,
        // SHA-256 pair for losslessness audit). Migration is UNCONDITIONAL (not
        // proxy-gated) so DB versions never fork across build variants (finding 19).
        // ADR-006: PRAGMA user_version = 4 is the FINAL statement in this batch.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS alignment_decisions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp INTEGER NOT NULL,
                request_id TEXT NOT NULL,
                provider TEXT NOT NULL,
                tools_key_sorted INTEGER NOT NULL,
                spans_compacted INTEGER NOT NULL,
                skim_breakpoints_injected INTEGER NOT NULL,
                client_breakpoint_count INTEGER NOT NULL,
                volatile_warn_count INTEGER NOT NULL,
                fail_open INTEGER NOT NULL,
                input_len INTEGER NOT NULL,
                output_len INTEGER NOT NULL,
                input_sha256 BLOB NOT NULL,
                output_sha256 BLOB NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_ad_timestamp ON alignment_decisions(timestamp);
            PRAGMA user_version = 4;",
        )?;
    }

    Ok(())
}
