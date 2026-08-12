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

    if version < 5 {
        // AD-CA-9 / AD-AN-5: alignment_decisions table — records per-request
        // cache-alignment outcomes (tools sorted, markers injected, fail-open flag,
        // SHA-256 pair for losslessness audit). Migration is UNCONDITIONAL (not
        // proxy-gated) so DB versions never fork across build variants (finding 19).
        //
        // ADR-006: wrapped in a single BEGIN..COMMIT transaction so any mid-batch
        // failure (e.g. a pre-existing alignment_decisions with an incompatible
        // schema causing the CREATE INDEX to fail) rolls back the entire batch and
        // leaves user_version at 4, allowing a safe retry — identical structure to
        // the v4 batch above.  PRAGMA user_version = 5 is the FINAL statement so a
        // partial abort at any earlier statement rolls back before the version is
        // advanced.
        conn.execute_batch(
            "BEGIN;
            CREATE TABLE IF NOT EXISTS alignment_decisions (
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
            PRAGMA user_version = 5;
            COMMIT;",
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

    // AC19 / POSITIVE: run_migrations on a fresh (v0) database advances to v5.
    // DISCRIMINATING (PF-007): removing any migration block causes user_version to
    // stop at a lower value, failing the final assert.
    #[test]
    fn schema_fresh_migration_advances_to_v5() {
        let conn = open_mem();
        run_migrations(&conn).expect("migrations must succeed on fresh db");
        assert_eq!(
            user_version(&conn),
            5,
            "fresh db must reach user_version = 5"
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
            table_exists(&conn, "proxy_block_decisions"),
            "v4: proxy_block_decisions table must exist"
        );
        assert!(
            table_exists(&conn, "alignment_decisions"),
            "v5: alignment_decisions table must exist"
        );
    }

    // AC19 / POSITIVE: run_migrations on a v3 database advances through v4 then v5
    // in two discrete steps.  ADR-006: PRAGMA user_version = N is the FINAL
    // statement in each batch, so a partial-abort within any batch leaves
    // user_version at the prior value and allows a safe retry.
    //
    // DISCRIMINATING: removing the v4 migration block causes user_version to stay
    // at 3 (proxy_block_decisions absent, alignment_decisions absent); removing the
    // v5 block causes user_version to stop at 4 (alignment_decisions absent).
    #[test]
    fn schema_v3_to_v5_migration_stepwise() {
        let conn = open_mem();

        // Bootstrap v1–v3 schema, then force user_version back to 3 to simulate
        // a pre-v4 database (note: token_savings already has the v1 shape at this
        // point; the v4 rebuild will transform it, so we just need the table present).
        run_migrations(&conn).expect("bootstrap migration must succeed");
        conn.execute_batch("PRAGMA user_version = 3")
            .expect("force user_version = 3");
        // Restore the v3-era NOT NULL token columns so the v4 rebuild can exercise
        // the actual migration path (the full run already transformed them to nullable;
        // we rebuild from scratch in memory so we re-create the v1 shape here).
        conn.execute_batch(
            "DROP TABLE IF EXISTS proxy_block_decisions;
             DROP TABLE IF EXISTS alignment_decisions;
             DROP TABLE IF EXISTS token_savings;
             CREATE TABLE token_savings (
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
                 parse_tier TEXT,
                 session_id TEXT
             );",
        )
        .expect("recreate v3-era token_savings must succeed");
        assert_eq!(user_version(&conn), 3, "must be at v3 before stepwise test");

        // Re-run migrations — both v4 and v5 blocks must fire in order.
        run_migrations(&conn).expect("v3→v4→v5 migration must succeed");

        // Final state: user_version = 5, both new tables present.
        assert_eq!(
            user_version(&conn),
            5,
            "v3 db must advance to user_version = 5"
        );
        assert!(
            table_exists(&conn, "proxy_block_decisions"),
            "v4: proxy_block_decisions table must be created by v3→v4 migration"
        );
        assert!(
            table_exists(&conn, "alignment_decisions"),
            "v5: alignment_decisions table must be created by v4→v5 migration"
        );
    }

    // AC19 / POSITIVE: run_migrations is idempotent — calling it twice produces
    // the same schema and user_version = 5.
    // ADR-006: `CREATE TABLE IF NOT EXISTS` + version guard ensures idempotence.
    #[test]
    fn schema_migration_idempotent_double_run() {
        let conn = open_mem();
        run_migrations(&conn).expect("first migration run must succeed");
        run_migrations(&conn).expect("second migration run must succeed (idempotent)");
        assert_eq!(
            user_version(&conn),
            5,
            "double migration must keep user_version = 5"
        );
    }

    // AC19 / NEGATIVE: an injected mid-migration failure leaves user_version at the
    // PRIOR version so the migration can be retried cleanly.  ADR-006 mandates that
    // PRAGMA user_version = N is the FINAL statement in its batch, ensuring that any
    // abort before that point rolls back the entire batch.
    //
    // Strategy: place the DB at v4, pre-create alignment_decisions with a wrong
    // schema (no `timestamp` column).  Call run_migrations: the v5 batch's
    // `CREATE TABLE IF NOT EXISTS` is a no-op (table exists), but `CREATE INDEX
    // ... ON alignment_decisions(timestamp)` fails (no such column).  With
    // BEGIN..COMMIT wrapping the v5 batch (ADR-006), the whole batch rolls back
    // and user_version stays at 4.  Drop the conflicting table, re-run migrations
    // and confirm v5 completes.
    //
    // DISCRIMINATING (PF-007): if PRAGMA user_version = 5 were placed BEFORE the
    // CREATE INDEX in the v5 batch, it would autocommit before the index fails,
    // leaving user_version = 5 after the error — this test catches that violation.
    // Unlike the previous version of this test (which hand-wrote its own BEGIN and
    // never called run_migrations), Phase 2 here drives the production migration
    // code path — the statement ORDER of schema.rs:130-149 is what determines the
    // outcome.
    #[test]
    fn schema_mid_migration_failure_leaves_prior_version_intact() {
        // Phase 1: advance to v4.
        let conn = open_mem();
        run_migrations(&conn).expect("full migration must succeed");
        conn.execute_batch(
            "DROP TABLE IF EXISTS alignment_decisions;
             PRAGMA user_version = 4;",
        )
        .expect("simulate v4 state");
        assert_eq!(user_version(&conn), 4, "must be at v4 for the failure test");
        assert!(
            !table_exists(&conn, "alignment_decisions"),
            "alignment_decisions must be absent at v4"
        );

        // Phase 2: inject a conflicting alignment_decisions table (wrong schema —
        // no `timestamp` column).  The v5 CREATE INDEX will fail at the column
        // reference; with the BEGIN..COMMIT wrapper the whole batch rolls back and
        // user_version stays at 4.
        conn.execute_batch(
            "CREATE TABLE alignment_decisions (id INTEGER PRIMARY KEY, wrong_col TEXT);",
        )
        .expect("pre-create alignment_decisions with wrong schema");

        // Call the production migration — it must fail due to the index error.
        let result = run_migrations(&conn);
        assert!(
            result.is_err(),
            "run_migrations must return Err when mid-batch DDL fails (index on missing column)"
        );

        // ADR-006 check: user_version must still be 4 (batch rolled back before
        // PRAGMA user_version = 5 was reached).
        assert_eq!(
            user_version(&conn),
            4,
            "mid-migration failure must leave user_version at 4 (prior version)"
        );
        // The table exists but has the wrong schema — proving the batch did not
        // atomically commit the CREATE TABLE either.  (SQLite IF NOT EXISTS is a
        // no-op here, so the pre-created wrong-schema table survived; it was never
        // part of the failed transaction.)
        assert!(
            table_exists(&conn, "alignment_decisions"),
            "setup check: conflicting alignment_decisions must still be present"
        );

        // SQLite does not auto-rollback when execute_batch fails mid-batch: the
        // BEGIN; opened a transaction and the index-creation failure left it open.
        // Explicitly roll it back so the connection is in a clean state before
        // Phase 3 tries to start a new transaction.
        conn.execute_batch("ROLLBACK;")
            .expect("rollback the failed v5 transaction before retry");

        // Phase 3: remove the conflicting table, then re-run — v5 must complete.
        conn.execute_batch("DROP TABLE alignment_decisions;")
            .expect("remove conflicting table before retry");
        run_migrations(&conn).expect("retry migration after clearing conflict must succeed");
        assert_eq!(
            user_version(&conn),
            5,
            "re-migration after conflict removal must reach user_version = 5"
        );
        assert!(
            table_exists(&conn, "alignment_decisions"),
            "alignment_decisions must exist with correct schema after successful re-migration"
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

    // AC5 / AC6 / AC19: the v4 destructive migration (INSERT+DROP+RENAME) preserves
    // all pre-existing rows and their column values.  The v4 batch touches every row
    // in token_savings; without a seeded, migrated, then read-back test the data-copy
    // SQL cannot be verified (empty tables pass silently).
    //
    // Also verifies that after the full migration NULL-token inserts succeed (the
    // nullable-column change that the v4 rebuild exists for — AC5/AC6 proxy rows).
    //
    // DISCRIMINATING (PF-007): dropping or reordering a column in either the INSERT
    // list or the SELECT list of the v4 copy causes an error on a non-empty table or
    // produces NULL values in the wrong columns — both are caught here.  Reverting
    // the nullable constraint on raw_tokens/compressed_tokens/savings_pct is caught
    // by the NULL-insert assertion at the end.
    #[test]
    fn schema_v4_migration_preserves_rows_and_allows_null_tokens() {
        // Create a v3-era token_savings with the original NOT NULL schema and seed
        // two rows with known values before running the v4 migration.
        let conn = open_mem();
        run_migrations(&conn).expect("bootstrap must succeed");
        conn.execute_batch(
            "DROP TABLE IF EXISTS proxy_block_decisions;
             DROP TABLE IF EXISTS alignment_decisions;
             DROP TABLE IF EXISTS token_savings;
             CREATE TABLE token_savings (
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
                 parse_tier TEXT,
                 session_id TEXT
             );
             INSERT INTO token_savings
                 (timestamp, command_type, original_cmd, raw_tokens, compressed_tokens,
                  savings_pct, duration_ms, project_path, mode, language, parse_tier, session_id)
             VALUES
                 (1000, 'cat', 'cat file.ts', 100, 60, 40.0, 5, '/tmp',
                  'structure', 'typescript', 'tree-sitter', 'sess-a');
             INSERT INTO token_savings
                 (timestamp, command_type, original_cmd, raw_tokens, compressed_tokens,
                  savings_pct, duration_ms, project_path, mode, language, parse_tier, session_id)
             VALUES
                 (2000, 'ls', 'ls -la', 50, 30, 40.0, 2, '/home',
                  NULL, NULL, NULL, NULL);",
        )
        .expect("recreate v3 schema and seed rows");
        conn.execute_batch("PRAGMA user_version = 3")
            .expect("force user_version = 3");

        // Run migrations v3 → v4 → v5.
        run_migrations(&conn).expect("v3→v4→v5 migration must succeed");
        assert_eq!(user_version(&conn), 5, "must reach v5 after migration");

        // Row count must be preserved — the INSERT+DROP+RENAME must not lose rows.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM token_savings", [], |r| r.get(0))
            .expect("COUNT must succeed");
        assert_eq!(count, 2, "v4 migration must preserve both seeded rows");

        // Verify the first row's column values are byte-identical after the rebuild.
        let (ts, cmd, raw, comp, pct, sid): (i64, String, i64, i64, f64, Option<String>) = conn
            .query_row(
                "SELECT timestamp, command_type, raw_tokens, compressed_tokens,
                         savings_pct, session_id
                  FROM token_savings WHERE timestamp = 1000",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .expect("first row must be readable after migration");
        assert_eq!(ts, 1000, "timestamp must survive migration");
        assert_eq!(cmd, "cat", "command_type must survive migration");
        assert_eq!(raw, 100, "raw_tokens must survive migration");
        assert_eq!(comp, 60, "compressed_tokens must survive migration");
        assert!((pct - 40.0).abs() < 1e-9, "savings_pct must survive migration");
        assert_eq!(sid.as_deref(), Some("sess-a"), "session_id must survive migration");

        // AC5/AC6: after v4, NULL-token inserts must succeed (nullable columns).
        // A proxy row records timing/provider without token counts.
        conn.execute(
            "INSERT INTO token_savings
                 (timestamp, command_type, original_cmd, raw_tokens, compressed_tokens,
                  savings_pct, duration_ms, project_path)
             VALUES (?1, ?2, ?3, NULL, NULL, NULL, ?4, ?5)",
            rusqlite::params![3000_i64, "proxy", "proxy-req", 10_i64, "/proxy"],
        )
        .expect("AC5/AC6: NULL-token proxy row insert must succeed after v4 migration");

        let null_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM token_savings WHERE raw_tokens IS NULL",
                [],
                |r| r.get(0),
            )
            .expect("COUNT must succeed");
        // The v3-seeded rows had NOT NULL raw_tokens (the v3 schema required it).
        // Only the new proxy row has NULL raw_tokens.
        assert_eq!(
            null_count, 1,
            "one NULL-raw_tokens row: the proxy row inserted after v4 migration"
        );
    }
}
