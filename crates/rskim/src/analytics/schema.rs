//! Database schema and migrations for analytics.

use rusqlite::Connection;

/// Columns added by migration v4 — what the reader actually received, beyond
/// the stdout body that `compressed_tokens` already counts.
///
/// Every one is NULLABLE, and that is load-bearing rather than lenient: `NULL`
/// means "not measured in this regime" and stays distinguishable from a
/// measured `0`. A cache hit records no `served` because the guard did not run
/// on that invocation; `Mode::Full` records none because the guard is skipped
/// for it entirely; a row written before this migration has none because the
/// concept did not exist. Defaulting any of those to `0` / `'transformed'`
/// would manufacture measurements that were never taken.
///
/// `compressed_tokens` is deliberately NOT redefined to include the notice.
/// Folding it in would silently re-base a 90-day series: every historical row
/// would keep its old meaning while new rows carried a new one, under one
/// column name, with nothing in the schema to mark where the definition moved.
const V4_COLUMNS: &[(&str, &str)] = &[
    ("notice_tokens", "INTEGER"),
    ("notice_bytes", "INTEGER"),
    ("served", "TEXT"),
];

/// Add any [`V4_COLUMNS`] the `token_savings` table does not already have.
///
/// Returns the columns actually added, so a test can assert the second call is
/// a no-op rather than inferring idempotence from the absence of an error.
///
/// The `ALTER` statements interpolate `name`/`ty` because SQLite does not bind
/// identifiers; both come from the `V4_COLUMNS` const, so no caller-supplied
/// text reaches the statement. The table name is a literal here for the same
/// reason — it is not a parameter of this function and cannot become one.
fn add_missing_v4_columns(conn: &Connection) -> anyhow::Result<Vec<&'static str>> {
    let mut stmt = conn.prepare("PRAGMA table_info(token_savings)")?;
    let existing: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;

    let mut added = Vec::new();
    for (name, ty) in V4_COLUMNS {
        if existing.iter().any(|c| c == name) {
            continue;
        }
        conn.execute_batch(&format!(
            "ALTER TABLE token_savings ADD COLUMN {name} {ty};"
        ))?;
        added.push(*name);
    }
    Ok(added)
}

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

    // ------------------------------------------------------------------
    // v4 — delivered-cost columns (notice_tokens, notice_bytes, served)
    // ------------------------------------------------------------------
    //
    // Expressed as a column-presence reconcile plus a version bump, not as a
    // `if version < 4 { ALTER … }` body. Both halves are needed, and each
    // defends a different failure:
    //
    // FORWARD-ONLY is preserved by the bump. `user_version` is only ever
    // RAISED, and only from below 4 — nothing here lowers it, and a database
    // already at a higher version keeps that version. Every migration above
    // remains guarded by its own `version <` test and cannot re-run.
    //
    // PRESENCE-GATING is what makes the ALTERs correct when the version number
    // is not ours alone. The default analytics DB on the author's machine is at
    // `user_version = 5`, written by a binary built from a parallel clone: the
    // in-flight `ticket/305` / `ticket/306` branches allocate v4
    // (`provider`/`model` + `proxy_block_decisions`) and v5
    // (`alignment_decisions`) against the same `~/Library/Caches/skim/analytics.db`.
    // A plain `version < 4` body never executes on that database, the three
    // columns below never appear, every INSERT naming them fails, and
    // `persist_record` discards the failure silently — recording stops dead with
    // no error surfaced anywhere. Presence-gating also makes the step idempotent
    // and order-independent, which is the property that lets two lineages
    // converge on one file instead of one of them going quiet.
    //
    // NOT DONE HERE, deliberately: no `bail!` when `version` exceeds what this
    // build knows. `ticket/306` carries such a guard (AD-AN-5 / ADR-006) with
    // `CURRENT_SCHEMA_VERSION = 5`. Introducing it on this branch, where the
    // ladder tops out at 4, would turn every open of the already-v5 default
    // database into an error — ending both recording and `skim stats` over a
    // live 90-day series, to defend against a case that has already happened
    // and is benign (the foreign columns are additive and nullable). The
    // version-number collision is a merge-time decision for a human, not
    // something this branch should resolve by refusing to run.
    // The returned list is of interest only to the idempotence test.
    add_missing_v4_columns(conn)?;
    if version < 4 {
        conn.execute_batch("PRAGMA user_version = 4;")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_version(conn: &Connection) -> i64 {
        conn.query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap()
    }

    fn columns(conn: &Connection) -> Vec<String> {
        let mut stmt = conn.prepare("PRAGMA table_info(token_savings)").unwrap();
        stmt.query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    /// A fresh database climbs the whole ladder and lands on v4 with the
    /// delivered-cost columns present.
    #[test]
    fn fresh_db_migrates_to_v4_with_delivered_columns() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();

        // SCHEMA-LADDER-TOP (see analytics::tests for the full list).
        assert_eq!(user_version(&conn), 4);
        let cols = columns(&conn);
        for (name, _) in V4_COLUMNS {
            assert!(cols.iter().any(|c| c == name), "missing column {name}");
        }
    }

    /// Re-running is a no-op: no column is added twice, no version moves.
    /// Asserted on the returned list rather than on "it did not error", because
    /// a duplicate `ALTER` is exactly the error this would otherwise hide.
    #[test]
    fn rerun_adds_nothing_and_holds_the_version() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();

        let added = add_missing_v4_columns(&conn).unwrap();
        assert!(added.is_empty(), "second pass added {added:?}");

        run_migrations(&conn).unwrap();
        // SCHEMA-LADDER-TOP (see analytics::tests for the full list).
        assert_eq!(user_version(&conn), 4);
    }

    /// The case that motivated presence-gating: a database another lineage
    /// already carried past 4.
    ///
    /// DISCRIMINATING — replace `add_missing_v4_columns(conn)?` with a
    /// `if version < 4 { … }` body and this fails, because the columns never
    /// appear on a v5 database and every later INSERT naming them dies inside
    /// the silent `persist_record` discard.
    #[test]
    fn foreign_lineage_v5_db_gains_columns_and_keeps_its_version() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();

        // Simulate the observed default DB: a newer lineage's columns, its
        // version, and none of ours.
        for (name, _) in V4_COLUMNS {
            conn.execute_batch(&format!("ALTER TABLE token_savings DROP COLUMN {name};"))
                .unwrap();
        }
        // NOT a SCHEMA-LADDER-TOP literal. This 5 stands for a version THIS
        // build does not define — ticket/306's rung — and is the whole point of
        // the test. Bumping it in step with the ladder top would delete the
        // "database is ahead of us" condition and leave the test asserting
        // nothing.
        conn.execute_batch(
            "ALTER TABLE token_savings ADD COLUMN provider TEXT;
             PRAGMA user_version = 5;",
        )
        .unwrap();

        run_migrations(&conn).unwrap();

        let cols = columns(&conn);
        for (name, _) in V4_COLUMNS {
            assert!(
                cols.iter().any(|c| c == name),
                "v5 database did not gain {name}"
            );
        }
        assert!(
            cols.iter().any(|c| c == "provider"),
            "the foreign lineage's own column must survive untouched"
        );
        assert_eq!(
            user_version(&conn),
            5,
            "forward-only: a newer version must never be lowered (and this 5 is \
             the foreign lineage's rung, not ours — do not bump it with the ladder)"
        );
    }

    /// NULL and 0 must stay distinguishable — "not measured in this regime"
    /// is not "measured, and it was zero".
    #[test]
    fn delivered_columns_distinguish_null_from_zero() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();

        conn.execute_batch(
            "INSERT INTO token_savings
               (timestamp, command_type, original_cmd, raw_tokens, compressed_tokens,
                savings_pct, duration_ms, project_path, notice_tokens, served)
             VALUES (1, 'file', 'skim a.ts', 10, 5, 50.0, 0, '/p', NULL, NULL);
             INSERT INTO token_savings
               (timestamp, command_type, original_cmd, raw_tokens, compressed_tokens,
                savings_pct, duration_ms, project_path, notice_tokens, served)
             VALUES (2, 'file', 'skim b.ts', 10, 5, 50.0, 0, '/p', 0, 'raw');",
        )
        .unwrap();

        let unmeasured: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM token_savings WHERE notice_tokens IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let measured_zero: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM token_savings WHERE notice_tokens = 0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(unmeasured, 1);
        assert_eq!(measured_zero, 1);
    }
}
