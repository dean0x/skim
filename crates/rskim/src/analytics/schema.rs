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

// ============================================================================
// Tests (AC19 — migration correctness)
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Open an in-memory SQLite database.
    fn open_mem() -> Connection {
        Connection::open_in_memory().expect("in-memory db must open")
    }

    /// Read PRAGMA user_version from a connection.
    fn user_version(conn: &Connection) -> i64 {
        conn.query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("user_version must be readable")
    }

    /// Check whether a table exists in the database.
    fn table_exists(conn: &Connection, name: &str) -> bool {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [name],
                |row| row.get(0),
            )
            .unwrap_or(0);
        count > 0
    }

    // AC19 / POSITIVE: run_migrations on a fresh (v0) database advances to v4.
    // DISCRIMINATING (PF-007): removing a migration block causes user_version to
    // stop at a lower value, failing the final assert.
    #[test]
    fn schema_fresh_migration_advances_to_v4() {
        let conn = open_mem();
        run_migrations(&conn).expect("migrations must succeed on fresh db");
        assert_eq!(
            user_version(&conn),
            4,
            "fresh db must reach user_version = 4"
        );
        assert!(
            table_exists(&conn, "token_savings"),
            "v1: token_savings table must exist"
        );
        assert!(
            table_exists(&conn, "analytics_meta"),
            "v2: analytics_meta table must exist"
        );
        assert!(
            table_exists(&conn, "alignment_decisions"),
            "v4: alignment_decisions table must exist"
        );
    }

    // AC19 / POSITIVE: run_migrations on a v3 database advances ONLY to v4.
    // ADR-006: PRAGMA user_version = 4 is the FINAL statement in the v4 batch,
    // so a batch partial-abort (e.g. mid-CREATE TABLE) leaves user_version at 3,
    // allowing a retry. The `CREATE TABLE IF NOT EXISTS` guards make the migration
    // safe to re-apply after any abort.
    //
    // DISCRIMINATING: removing the v4 migration block causes user_version to stay
    // at 3 after calling run_migrations, failing the assert.
    #[test]
    fn schema_v3_to_v4_migration_stepwise() {
        let conn = open_mem();

        // Manually advance to v3 by calling run_migrations then setting user_version = 3.
        // This simulates a pre-v4 database.
        run_migrations(&conn).expect("initial migration must succeed");
        conn.execute_batch("PRAGMA user_version = 3")
            .expect("force user_version = 3");
        assert_eq!(user_version(&conn), 3, "must be at v3 before stepwise test");

        // Drop the alignment_decisions table so v4 migration can re-create it.
        conn.execute_batch("DROP TABLE IF EXISTS alignment_decisions")
            .expect("drop must succeed");

        // Now re-run migrations — only the v4 block should fire.
        run_migrations(&conn).expect("v3→v4 migration must succeed");
        assert_eq!(
            user_version(&conn),
            4,
            "v3 db must advance to user_version = 4"
        );
        assert!(
            table_exists(&conn, "alignment_decisions"),
            "v4: alignment_decisions table must be created by v3→v4 migration"
        );
    }

    // AC19 / POSITIVE: run_migrations is idempotent — calling it twice produces
    // the same schema and user_version = 4.
    // ADR-006: `CREATE TABLE IF NOT EXISTS` + version guard ensures idempotence.
    #[test]
    fn schema_migration_idempotent_double_run() {
        let conn = open_mem();
        run_migrations(&conn).expect("first migration run must succeed");
        run_migrations(&conn).expect("second migration run must succeed (idempotent)");
        assert_eq!(
            user_version(&conn),
            4,
            "double migration must keep user_version = 4"
        );
    }

    // AC19 / POSITIVE: the alignment_decisions table has the correct column schema.
    // DISCRIMINATING: removing a column from the CREATE TABLE statement would fail
    // the column-existence check here.
    #[test]
    fn schema_alignment_decisions_columns_correct() {
        let conn = open_mem();
        run_migrations(&conn).expect("migrations must succeed");

        // Query the pragma table_info to verify all required columns exist.
        let required_columns = [
            "id",
            "timestamp",
            "request_id",
            "provider",
            "tools_key_sorted",
            "spans_compacted",
            "skim_breakpoints_injected",
            "client_breakpoint_count",
            "volatile_warn_count",
            "fail_open",
            "input_len",
            "output_len",
            "input_sha256",
            "output_sha256",
        ];

        let mut stmt = conn
            .prepare("PRAGMA table_info(alignment_decisions)")
            .expect("pragma must succeed");
        let columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query_map must succeed")
            .filter_map(|r| r.ok())
            .collect();

        for col in &required_columns {
            assert!(
                columns.contains(&col.to_string()),
                "AC19: alignment_decisions table must have column '{col}'"
            );
        }
    }
}
