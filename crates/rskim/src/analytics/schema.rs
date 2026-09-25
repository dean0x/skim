//! Database schema and migrations for analytics.

use rusqlite::Connection;

/// The delivered-cost columns — what the reader actually received, beyond the
/// stdout body that `compressed_tokens` already counts.
///
/// The `V4_` in the name is a label for this column SET, not a `user_version`
/// this build stamps. No schema number is claimed for these columns; see
/// [`run_migrations`] for why claiming one is unsafe.
///
/// Every one is NULLABLE, and that is load-bearing rather than lenient: `NULL`
/// means "not measured in this regime" and stays distinguishable from a
/// measured `0`. A cache hit records no `served` because the guard did not run
/// on that invocation; `Mode::Full` records none because the guard is skipped
/// for it entirely; a row written before these columns existed has none because
/// the concept did not exist. Defaulting any of those to `0` / `'transformed'`
/// would manufacture measurements that were never taken.
///
/// That row-level NULL-ness is also the delivered series' ONLY selector:
/// `query_summary` keys on `notice_tokens IS NOT NULL`, row by row, and reads
/// no schema version anywhere. It is what lets this build claim no
/// `user_version` without affecting the series or the window that labels it.
///
/// `compressed_tokens` is deliberately NOT redefined to include the notice.
/// Folding it in would silently re-base a 90-day series: every historical row
/// would keep its old meaning while new rows carried a new one, under one
/// column name, with nothing in the schema to mark where the definition moved.
pub(super) const V4_COLUMNS: &[(&str, &str)] = &[
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
    // Delivered-cost columns (notice_tokens, notice_bytes, served)
    // ------------------------------------------------------------------
    //
    // NO `user_version` IS CLAIMED FOR THESE COLUMNS. This step is a pure
    // column-presence reconcile: it adds whichever of the three are missing and
    // writes no version at all. Whatever rung the ladder above left — or
    // whatever number a foreign lineage stamped — passes through untouched.
    // Nothing here raises a version, and nothing here lowers one.
    //
    // WHY CLAIMING 4 WOULD BE UNSAFE. Three lineages want a schema number
    // against the same `~/Library/Caches/skim/analytics.db`: `ticket/305`
    // allocates v4 for its own columns (`provider`/`model` +
    // `proxy_block_decisions`), `ticket/306` allocates v5
    // (`alignment_decisions`), and the author's live database is already at 5.
    // If this build stamped 4 on a fresh database, `ticket/305`'s binary would
    // later read that 4 as "my own v4 migration has already run" and skip it.
    // Its migration is version-gated, not presence-gated, so ITS columns would
    // never appear, every INSERT naming them would fail, and `persist_record`
    // discards those failures silently — recording stops dead, with no error
    // and no diagnostic anywhere. A number that buys us nothing is not worth
    // handing another lineage that failure path.
    //
    // WHY CLAIMING ANYTHING HIGHER WOULD BE WORSE. `ticket/306` carries a
    // forward guard (AD-AN-5 / ADR-006, `CURRENT_SCHEMA_VERSION = 5`) that
    // refuses to open a database whose `user_version` exceeds its own. Raising
    // the already-v5 live database to 6 would make that clone refuse it
    // outright.
    //
    // WHY NO NUMBER IS NEEDED. Presence-gating is the correctness mechanism on
    // its own: it is idempotent and order-independent, so it is right at ANY
    // version, which is what lets several lineages converge on one file instead
    // of one of them going quiet. And the number does no work downstream —
    // `query_summary` selects the delivered series on `notice_tokens IS NOT
    // NULL`, row by row, and derives its window from MIN/MAX over that same
    // predicate. The nullable columns already separate "not measured in this
    // regime" from a measured 0 at the ROW level. Nothing consults
    // `user_version` to know where the delivered series opens, so a schema
    // version would be a claim with no reader and a real collision cost.
    //
    // ALSO NOT DONE HERE, deliberately: no `bail!` when `version` exceeds what
    // this build knows. Adopting `ticket/306`'s forward guard here would turn
    // every open of the already-v5 live database into an error — ending both
    // recording and `skim stats` over a 90-day series, to defend against a case
    // that has already happened and is benign (the foreign columns are additive
    // and nullable). Who owns which rung is a merge-time decision for a human;
    // this branch stays out of the allocation entirely.
    //
    // ================================================================
    // HAZARD FOR WHOEVER RECONCILES THESE LINEAGES — READ BEFORE MERGING
    // ================================================================
    //
    // Presence-gating survives a foreign ALTER. It does NOT survive a foreign
    // table REBUILD, and `ticket/305`'s v4 migration is a rebuild: it creates
    // `token_savings_new` with an explicit column list, copies the v3 columns
    // across BY NAME, `DROP TABLE token_savings`, then renames. That column
    // list does not mention `notice_tokens`, `notice_bytes` or `served`. On any
    // database carrying them the rebuild therefore drops all three and every
    // measurement in them — and because the copy names its columns explicitly
    // instead of `SELECT *`, the mismatch raises nothing. The transaction
    // commits cleanly. The reconcile below then re-adds the three columns EMPTY
    // on the next open, `query_summary` reports `rows = 0`, and
    // `render_delivered` returns early on zero rows, so the dashboard just
    // stops printing the delivered line. Silent series reset, no diagnostic.
    //
    // This is live precisely BECAUSE we claim no number. A database this build
    // writes lands at 3, which is BELOW `ticket/305`'s `version < 4` gate, so
    // its rebuild is eligible to run over our data. (The 90-day database at 5
    // is not eligible — both lineages skip their v4 block above 4 — and
    // `ticket/305` cannot open it at all: its own forward guard tops out at 4.)
    //
    // The trade was made with eyes open: claiming 4 instead would leave
    // `ticket/305`'s OWN columns permanently absent and its INSERTs failing
    // forever into the silent `persist_record` discard, whereas this costs a
    // one-time drop that the presence-gate then self-heals. A recoverable reset
    // of a young series beats permanently breaking another lineage's recording.
    //
    // THE MERGE-TIME FIX IS ONE LINE OF DILIGENCE: carry `notice_tokens`,
    // `notice_bytes` and `served` in the rebuild's CREATE list and in BOTH
    // halves of its `INSERT ... SELECT`. A rebuild that forgets them does not
    // fail — it deletes.
    //
    // The returned list is of interest only to the idempotence test.
    add_missing_v4_columns(conn)?;

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

    /// A fresh database comes out of `run_migrations` with the delivered-cost
    /// columns present.
    ///
    /// The subject is column PRESENCE, not a version integer. This build claims
    /// no `user_version` for these columns (see `run_migrations` for why), and
    /// nothing downstream reads one: the delivered series selects on
    /// `notice_tokens IS NOT NULL`, row by row. Presence is the whole of what
    /// the series depends on, so it is the whole of what this pins. Asserting a
    /// number here would pin something no reader consults and would re-open the
    /// collision with `ticket/305`.
    #[test]
    fn fresh_db_gains_the_delivered_columns() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();

        let cols = columns(&conn);
        for (name, _) in V4_COLUMNS {
            assert!(cols.iter().any(|c| c == name), "missing column {name}");
        }
    }

    /// Re-running is a no-op: no column is added twice, and the version is not
    /// moved.
    ///
    /// The column half is asserted on the returned list rather than on "it did
    /// not error", because a duplicate `ALTER` is exactly the error that would
    /// otherwise hide.
    ///
    /// The version half is asserted as INVARIANCE against the value observed
    /// before the second pass, not against a literal. That is the property this
    /// build now guarantees — the delivered-cost step writes no `user_version`
    /// — and stating it without naming a number means the assertion survives
    /// whatever rung the ladder above happens to end on, and fails the moment
    /// anything here starts stamping one again.
    #[test]
    fn rerun_adds_nothing_and_writes_no_version() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        let version_after_first = user_version(&conn);

        let added = add_missing_v4_columns(&conn).unwrap();
        assert!(added.is_empty(), "second pass added {added:?}");

        run_migrations(&conn).unwrap();
        assert_eq!(
            user_version(&conn),
            version_after_first,
            "re-running migrations must not move the version"
        );
        let cols = columns(&conn);
        for (name, _) in V4_COLUMNS {
            assert!(cols.iter().any(|c| c == name), "lost column {name}");
        }
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
        // This 5 is `ticket/306`'s rung — a version THIS build does not define
        // and never writes — and it is the whole point of the test: it puts the
        // database AHEAD of us, which is the condition presence-gating exists
        // to survive.
        //
        // It tracks no number of ours, because there is none to track: the
        // delivered-cost step claims no `user_version` at all (see
        // `run_migrations`). This literal moves only if `ticket/306`'s own rung
        // moves. Raising it to follow some other version would delete the
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
