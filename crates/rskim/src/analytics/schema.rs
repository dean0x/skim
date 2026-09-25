//! Database schema and migrations for analytics.

use rusqlite::Connection;
use std::time::{SystemTime, UNIX_EPOCH};

/// The delivered-cost columns — what the reader actually received, beyond the
/// stdout body that `compressed_tokens` already counts.
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
/// `user_version` without affecting the series or the window that labels it —
/// see [`reconcile_additive_columns`] for why claiming one is unsafe.
///
/// `compressed_tokens` is deliberately NOT redefined to include the notice.
/// Folding it in would silently re-base a 90-day series: every historical row
/// would keep its old meaning while new rows carried a new one, under one
/// column name, with nothing in the schema to mark where the definition moved.
///
/// # `notice_bytes` is raw telemetry with no reader yet; `served` now has one
///
/// `notice_tokens` selects and sums the delivered series. `served` is read as
/// a three-way partition of that same population (`'raw'` / `'transformed'` /
/// a NULL-safe complement) in `query_summary`'s existing single pass, rendered
/// by `render_delivered` — the cut this block used to nominate, now taken.
///
/// `notice_bytes` remains written on every measured row and read by no
/// production SELECT that reports it: it appears only in
/// `delivered_unmeasured_rows`' predicate, as the thing that distinguishes an
/// untokenisable disclosure from an absent one, and its VALUE is never
/// surfaced. Under the 90-day prune those bytes expire unread. Recording ahead
/// of a reader is deliberate — the cut can then be added without a schema
/// change and a 90-day wait — but do not read the dashboard as evidence that
/// anything consumes the magnitude: it does not (PF-036's amendment: recording
/// a column is not measuring it).
pub(super) const DELIVERED_COST_COLUMNS: &[(&str, &str)] = &[
    ("notice_tokens", "INTEGER"),
    ("notice_bytes", "INTEGER"),
    ("served", "TEXT"),
];

/// `analytics_meta` key: the delivered-cost columns were reconciled onto this
/// database at least once, and when.
///
/// It lives in `analytics_meta` for one reason — that table is not
/// `token_savings`, so it survives a `token_savings` rebuild, which is the one
/// hazard presence-gating cannot see (see [`reconcile_additive_columns`]).
const DELIVERED_SERIES_OPENED_AT: &str = "delivered_series_opened_at";

/// `analytics_meta` key: the delivered-cost columns had to be re-added to a
/// database that had already been reconciled once — so something dropped them
/// in between, and every measurement they held went with them.
///
/// `pub(super)` because the reader half lives in `analytics/mod.rs`
/// ([`super::AnalyticsDb::delivered_series_reset_at`]) and a key written under
/// one spelling and read under another is a mark nobody ever sees. This const
/// is the only spelling either half knows.
///
/// NOT interchangeable with [`DELIVERED_SERIES_OPENED_AT`], and the difference
/// is the whole disclosure: `OPENED_AT` is written on the FIRST reconcile of
/// any database, so `opened_at` present with zero delivered rows is the normal
/// state of a fresh install, of an upgrade over existing history, of
/// `stats --clear` and of a 90-day prune. Measured on the author's live
/// database (69,742 rows, zero with `notice_tokens IS NOT NULL`, 2026-09-26):
/// the three columns are already present, so the next open writes `OPENED_AT`
/// and adds nothing — a reader keying on `opened_at` would announce a reset
/// that never happened, on the only database that exists. `RESET_AT` is
/// written only when all three columns reappear on an ALREADY-reconciled
/// database, which cannot happen without something having dropped them.
pub(super) const DELIVERED_SERIES_RESET_AT: &str = "delivered_series_reset_at";

/// Run all database migrations.
///
/// # Two idioms live here and they are NOT interchangeable
///
/// [`run_versioned_migrations`] is a LADDER: each rung is gated on
/// `PRAGMA user_version` and stamps the next number. A rung belongs there only
/// if this build owns the number it stamps — while other lineages hold unmerged
/// claims on the numbers above ours, nothing does.
///
/// [`reconcile_additive_columns`] is a PRESENCE RECONCILE: it adds whichever
/// columns are absent, claims no version, and is therefore idempotent and
/// order-independent at any version. **A new nullable column goes there.**
///
/// The split is the point. One function hosting both idioms gave the file two
/// contradictory answers to "how do I add a column here?", and which one a
/// contributor got depended on which half they scrolled to.
pub(super) fn run_migrations(conn: &Connection) -> anyhow::Result<()> {
    run_versioned_migrations(conn)?;
    reconcile_additive_columns(conn)?;
    Ok(())
}

/// The version-gated ladder: ascending, forward-only, one `PRAGMA user_version`
/// stamp per rung.
///
/// Every rung tests `version <` and writes the number it owns, so no rung can
/// re-run and no rung lowers a version. Do NOT add one while another lineage
/// holds an unmerged claim on the next number — additive nullable columns never
/// need one, and [`reconcile_additive_columns`] documents what claiming one
/// costs.
fn run_versioned_migrations(conn: &Connection) -> anyhow::Result<()> {
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

    Ok(())
}

/// The presence-gated reconcile for the [`DELIVERED_COST_COLUMNS`]: add
/// whichever `token_savings` lacks, claim no `user_version`, and record the
/// reconcile in `analytics_meta` so a later silent loss stays recoverable.
///
/// Idempotent and order-independent, which is what makes it correct at ANY
/// version — the property the comment block in the body exists to justify, and
/// the reason a new nullable column belongs here rather than on the ladder.
fn reconcile_additive_columns(conn: &Connection) -> anyhow::Result<()> {
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
    // WHAT PRESENCE-GATING DOES NOT BUY, and what this function does about it:
    // a NAME is not a contract. The gate therefore matches name AND declared
    // type and refuses a same-named column of another type rather than writing
    // skim's measurements into another lineage's quantity — see
    // `apply_delivered_cost_columns`.
    //
    // ALSO NOT DONE HERE, deliberately: no `bail!` when `version` exceeds what
    // this build knows. Adopting `ticket/306`'s forward guard here would turn
    // every open of the already-v5 live database into an error — ending both
    // recording and `skim stats` over a 90-day series, to defend against a case
    // that has already happened and is benign (the foreign columns are additive
    // and nullable). Who owns which rung is a merge-time decision for a human;
    // HEAD claims none. (An earlier commit of this branch did — see THE
    // `user_version = 4` ORPHAN below.)
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
    // stops printing the delivered line. Silent series reset — which is why
    // `note_delivered_columns_reconciled` leaves a mark OUTSIDE `token_savings`
    // where a rebuild of that table cannot reach it.
    //
    // (Not hypothetical: the live 151 MB database's `token_savings` is already
    // a post-rebuild reconstruction — `CREATE TABLE "token_savings"`, quoted
    // identifier — and that rebuild silently dropped NOT NULL from
    // `raw_tokens`/`compressed_tokens`/`savings_pct`, which ALTER cannot
    // restore.)
    //
    // This is live precisely BECAUSE we claim no number. A FRESH database this
    // build writes lands at 3 — the ladder's top, which main already sets —
    // and 3 is BELOW `ticket/305`'s `version < 4` gate, so its rebuild is
    // eligible to run over our data. (The 90-day database at 5 is not eligible
    // — both lineages skip their v4 block above 4 — and `ticket/305` cannot
    // open it at all: its own forward guard tops out at 4.)
    //
    // THE `user_version = 4` ORPHAN THIS BRANCH CREATED ITSELF. "A fresh
    // database lands at 3" is true only of a database no EARLIER commit of this
    // branch touched. Commit 0cb0255 ("feat(analytics): record disclosure cost,
    // serving decision, and surface expansion"), an ancestor of HEAD, DID stamp
    // `PRAGMA user_version = 4` for these columns before the claim was
    // retracted — so at-4 databases exist in the field, in exactly the state
    // the paragraphs above call fatal. Identify one precisely: `user_version`
    // is exactly 4, `token_savings` carries `notice_tokens`/`notice_bytes`/
    // `served`, and it carries NONE of `ticket/305`'s columns (`provider`,
    // `model`). That combination is 0cb0255's signature and nothing else writes
    // it. At HEAD such a database is INERT — every rung above is `version < N`
    // for N <= 3 and is skipped forever, while this reconcile keeps the columns
    // right without touching the number, so recording and `skim stats` both
    // work — but it is NOT eligible for `ticket/305`'s rebuild: that gate is
    // `version < 4`, so on a 4 it skips, ITS columns never appear, its INSERTs
    // fail, and `persist_record` discards those failures silently. REMEDY, for
    // whoever finds one: `PRAGMA user_version = 3` restores its eligibility.
    // That is safe because nothing above rung 3 ever ran on it — the 4 recorded
    // only these columns, and this reconcile re-establishes them from the
    // columns themselves rather than from a number. `starting_at_v4_is_a_noop`
    // pins the inert behaviour so it is exercised rather than reasoned about;
    // no test pins the remedy, which is a human's one-line fix.
    //
    // The trade was made with eyes open: claiming 4 for everyone instead would
    // leave `ticket/305`'s OWN columns permanently absent and its INSERTs
    // failing forever into the silent `persist_record` discard, whereas this
    // costs a one-time drop that the presence-gate then self-heals.
    //
    // THE MERGE-TIME FIX IS ONE LINE OF DILIGENCE: carry `notice_tokens`,
    // `notice_bytes` and `served` in the rebuild's CREATE list and in BOTH
    // halves of its `INSERT ... SELECT`. A rebuild that forgets them does not
    // fail — it deletes.
    //
    // And the obligation runs both ways: **every column a rebuild adds must be
    // nullable or carry a `DEFAULT`, or this lineage's INSERT dies silently.**
    // `AnalyticsDb::record` names a fixed 15 columns and omits every column it
    // does not know about, so a `NOT NULL` column with no `DEFAULT` makes that
    // statement violate a constraint on every row, forever, into
    // `persist_record`'s discard. Nothing self-heals it — unlike the drop
    // direction. This is averted today only by the other lineage's choice:
    // `provider`, `model`, `turn_id` and `upstream_error_status` on the live
    // database are all nullable.
    //
    // (Verified 2026-09-26 against a copy of the live 151 MB database:
    // `PRAGMA table_info(token_savings)` reports `notnull = 0` and a NULL
    // `dflt_value` for all four. Their being nullable is a property of that
    // file, not a rule anything enforces, which is why it is written down here
    // rather than relied on.)
    require_table(conn, "token_savings")?;
    let added = add_missing_delivered_cost_columns(conn)?;
    note_delivered_columns_reconciled(conn, &added)?;

    Ok(())
}

/// Fail with the database named when a table this reconcile needs is absent.
///
/// `PRAGMA table_info` on a missing table returns zero rows and no error, so
/// without this the first `ALTER` fails with a bare `no such table:
/// token_savings` that names no file. Reachable by pointing
/// `SKIM_ANALYTICS_DB` at an unrelated SQLite database: from `user_version >= 3`
/// every rung of the ladder is skipped, so a foreign file arrives here intact.
/// Below 3 it never gets this far — the v3 `session_id` ALTER already fails on
/// it, exactly as it did before this check existed — so this narrows a
/// diagnostic rather than adding a failure.
fn require_table(conn: &Connection, table: &str) -> anyhow::Result<()> {
    if table_exists(conn, table)? {
        return Ok(());
    }
    anyhow::bail!(
        "analytics: database {} has no `{table}` table, so the delivered-cost \
         columns have nothing to reconcile onto. Its `PRAGMA user_version` is \
         at or above the top of skim's own ladder, so no migration recreated \
         the table — this is most likely not a skim analytics database. \
         Remedy: point `SKIM_ANALYTICS_DB` at a skim analytics database, or \
         remove that file so a fresh one is created.",
        database_label(conn),
    )
}

/// Whether `table` exists in this database.
fn table_exists(conn: &Connection, table: &str) -> anyhow::Result<bool> {
    let present: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )?;
    Ok(present)
}

/// The database's path for diagnostics, or a stand-in for a connection that has
/// no file (in-memory and temporary databases report an empty filename).
fn database_label(conn: &Connection) -> &str {
    conn.path()
        .filter(|p| !p.is_empty())
        .unwrap_or("<in-memory>")
}

/// `(name, declared type)` for every column `PRAGMA table_info` reports on
/// `token_savings`.
///
/// The declared type is read because the presence gate needs it: a name alone
/// cannot tell skim's `notice_tokens` from a foreign lineage's, and SQLite
/// enforces no types, so adopting the wrong one fails silently forever rather
/// than loudly once.
fn declared_columns(conn: &Connection) -> anyhow::Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare("PRAGMA table_info(token_savings)")?;
    let columns = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                // A column declared with no type reports an EMPTY string here,
                // not NULL — but read it as `Option` anyway so that such a
                // column becomes a type MISMATCH this code can name, never a
                // rusqlite decode error from inside a migration.
                row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(columns)
}

/// Add any [`DELIVERED_COST_COLUMNS`] the `token_savings` table does not
/// already have.
///
/// Returns the columns added by THIS call, so a test can assert the second pass
/// is a no-op rather than inferring idempotence from the absence of an error,
/// and so [`note_delivered_columns_reconciled`] can tell a first reconcile from
/// a re-add.
fn add_missing_delivered_cost_columns(conn: &Connection) -> anyhow::Result<Vec<&'static str>> {
    let existing = declared_columns(conn)?;
    apply_delivered_cost_columns(conn, &existing)
}

/// The gate itself, taking the `PRAGMA table_info` snapshot as an argument.
///
/// Split out so a test can hand it a STALE snapshot: that is the concurrency
/// case, and it is unreachable through the combined function on one connection.
///
/// The `ALTER` statements interpolate `name`/`ty` because SQLite does not bind
/// identifiers; both come from the [`DELIVERED_COST_COLUMNS`] const, so no
/// caller-supplied text reaches the statement. The table name is a literal here
/// for the same reason — it is not a parameter of this function and cannot
/// become one.
fn apply_delivered_cost_columns(
    conn: &Connection,
    existing: &[(String, String)],
) -> anyhow::Result<Vec<&'static str>> {
    let mut added = Vec::new();
    for (name, ty) in DELIVERED_COST_COLUMNS {
        // Name match is case-INSENSITIVE because SQLite identifiers are: a
        // byte-exact gate reads `Notice_Tokens` as absent and then hands SQLite
        // an `ALTER` it rejects as a duplicate, which since the tolerance below
        // no longer errors — it silently adopts the column WITHOUT the type
        // check ever running. The two halves of this gate only work together.
        if let Some((found_name, found_ty)) =
            existing.iter().find(|(c, _)| c.eq_ignore_ascii_case(name))
        {
            if found_ty.eq_ignore_ascii_case(ty) {
                continue;
            }
            // A same-named column of another declared type is another lineage's
            // measurement, not ours. SQLite enforces no types, so `served`'s
            // 'raw'/'transformed' would land in an INTEGER column without
            // complaint, `record` would write skim's values into a foreign
            // quantity, and `query_summary` would publish the result as
            // measured fact — and because the gate then skips the column
            // forever, that never self-heals. Refuse to adopt it instead: write
            // nothing rather than write into something unverified.
            anyhow::bail!(
                "analytics: token_savings.{found_name} in database {} is declared \
                 {found_ty}, but the delivered-cost column {name} requires {ty}. A \
                 same-named column of another type belongs to another lineage; \
                 adopting it would publish its quantity as skim's measurement. \
                 Remedy: rename or drop token_savings.{found_name}, or point \
                 `SKIM_ANALYTICS_DB` at a different database.",
                database_label(conn),
            );
        }
        match conn.execute_batch(&format!(
            "ALTER TABLE token_savings ADD COLUMN {name} {ty};"
        )) {
            Ok(()) => added.push(*name),
            // The snapshot above and this write are not one atomic step, and
            // they cannot be: skim records fire-and-forget from background
            // threads, the PreToolUse hook fires on every Bash call, and many
            // skim processes open this database at once. Two of them both
            // seeing a column absent both attempt the ALTER, and the loser gets
            // `duplicate column name` — SQLITE_ERROR, NOT SQLITE_BUSY, so the
            // `busy_timeout` on the connection does not cover it. Propagating
            // it would fail `run_migrations`, fail `AnalyticsDb::open`, and lose
            // the row inside `persist_record`'s silent discard: the exact
            // failure mode the block above refuses to hand another lineage,
            // reproduced inside the mechanism meant to avoid it. The window is
            // the first opens after an upgrade — the window every user passes
            // through.
            //
            // Treat it as success. The error means another process already
            // achieved precisely what this one wanted, so the desired state
            // holds and there is nothing to retry; the column is deliberately
            // NOT pushed onto `added`, which stays "columns THIS call created".
            // The alternative — bracketing the read and the writes in
            // `BEGIN IMMEDIATE` — buys mutual exclusion at the cost of taking a
            // write lock on every open, including the steady-state opens where
            // nothing is missing, which is all of them after the first. Lock-free
            // convergence is cheaper and reaches the same state.
            Err(e) if is_duplicate_column(&e) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(added)
}

/// Whether a rusqlite error is SQLite's duplicate-column rejection.
///
/// Matched on the message because SQLite reports it as a generic
/// `SQLITE_ERROR`, with no distinct result code to test.
fn is_duplicate_column(err: &rusqlite::Error) -> bool {
    err.to_string()
        .to_ascii_lowercase()
        .contains("duplicate column name")
}

/// Record the reconcile in `analytics_meta`, and mark a re-add as a RESET.
///
/// The accepted hazard above — a foreign rebuild drops the columns, the next
/// open re-adds them empty, `query_summary` returns zero rows and the dashboard
/// silently stops printing the delivered line — is only recoverable if it is
/// visible. `analytics_meta` is a different table, so a `token_savings` rebuild
/// cannot take this mark with it.
///
/// [`DELIVERED_SERIES_OPENED_AT`] doubles as the row that asserts these columns
/// are INTENDED to exist on this database, so there is one row rather than two.
///
/// Why a re-add, and not "zero delivered rows", is the signal: an empty
/// delivered population is the NORMAL state of a fresh database, of an upgraded
/// one before its first measured invocation (measured live: the 151 MB database
/// holds 69k rows and zero with `notice_tokens IS NOT NULL`), of `stats --clear`
/// and of a 90-day prune. Treating that as a reset would manufacture a claim in
/// four common states. Columns being added to a database that had ALREADY been
/// reconciled cannot happen without something dropping them in between.
///
/// Requiring ALL of them to reappear is what keeps the marker honest under
/// concurrency: a rebuild drops the three together, whereas a lost ALTER race
/// leaves the loser having created only a subset. That trades a missed mark in
/// the narrow case of two processes splitting the re-add for never claiming a
/// reset that did not happen.
fn note_delivered_columns_reconciled(
    conn: &Connection,
    added: &[&'static str],
) -> anyhow::Result<()> {
    // `analytics_meta`'s own rung is v2, and every rung is skipped on a database
    // already past it — including a foreign lineage's file that never had the
    // table. The reconcile has to be right at ANY version, so it cannot assume
    // the ladder ran: create the table if it is missing, with the same
    // statement the rung uses. Guarded by a read so the steady-state open takes
    // no write lock.
    if !table_exists(conn, "analytics_meta")? {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS analytics_meta (
                key TEXT PRIMARY KEY,
                value INTEGER
            );",
        )?;
    }

    let already_noted: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM analytics_meta WHERE key = ?1)",
        [DELIVERED_SERIES_OPENED_AT],
        |row| row.get(0),
    )?;

    let now = unix_now();

    if !already_noted {
        conn.execute(
            "INSERT OR IGNORE INTO analytics_meta (key, value) VALUES (?1, ?2)",
            rusqlite::params![DELIVERED_SERIES_OPENED_AT, now],
        )?;
        return Ok(());
    }

    if added.len() == DELIVERED_COST_COLUMNS.len() {
        // The opening timestamp is left alone: it records when the series first
        // opened, which the reset did not change. `OR REPLACE` on the reset key
        // keeps the MOST RECENT reset, since only the latest one bounds what
        // survives.
        conn.execute(
            "INSERT OR REPLACE INTO analytics_meta (key, value) VALUES (?1, ?2)",
            rusqlite::params![DELIVERED_SERIES_RESET_AT, now],
        )?;
    }

    Ok(())
}

/// Seconds since the Unix epoch, `0` if the clock is before it.
///
/// Mirrors `maybe_prune`'s idiom — a migration must not panic on a clock.
fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
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

    fn meta(conn: &Connection, key: &str) -> Option<i64> {
        conn.query_row(
            "SELECT value FROM analytics_meta WHERE key = ?1",
            [key],
            |r| r.get(0),
        )
        .ok()
    }

    /// A fresh database comes out of `run_migrations` with the delivered-cost
    /// columns present, and on the ladder's top rung.
    ///
    /// Both halves are load-bearing and they are different subjects. PRESENCE is
    /// what the series depends on: nothing downstream reads a version, and
    /// `query_summary` selects the delivered series on `notice_tokens IS NOT
    /// NULL`, row by row. The literal 3 pins that the delivered-cost step still
    /// claims NOTHING — 3 is main's ladder top, set by the `session_id` rung, so
    /// asserting it is not a version claim for these columns but the absence of
    /// one. A 4 here is the reverted defect: `ticket/305` reads that 4 as "my
    /// own v4 already ran", skips it, and its INSERTs then fail forever into
    /// `persist_record`'s silent discard.
    ///
    /// This is where a re-stamp is caught. The invariance assertion in
    /// `rerun_adds_nothing_and_writes_no_version` cannot do it — an idempotent
    /// stamp is invariant across a rerun — so the number has to be named once,
    /// here.
    #[test]
    fn fresh_db_gains_the_delivered_columns() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();

        let cols = columns(&conn);
        for (name, _) in DELIVERED_COST_COLUMNS {
            assert!(cols.iter().any(|c| c == name), "missing column {name}");
        }
        assert_eq!(
            user_version(&conn),
            3,
            "the delivered-cost step must claim no user_version; a fresh DB stays \
             on the ladder top (3). A 4 here is the ticket/305 silent-recording-death \
             defect."
        );
    }

    /// Re-running is a no-op: no column is added twice, and the version is not
    /// moved.
    ///
    /// The column half is asserted on the returned list rather than on "it did
    /// not error", because a duplicate `ALTER` is exactly the error that would
    /// otherwise hide.
    ///
    /// The version half is INVARIANCE against the value observed before the
    /// second pass — a genuine property (this step writes no `user_version`),
    /// and one that holds whatever rung the ladder above ends on. It is NOT a
    /// re-stamp detector, and must not be read as one: re-adding
    /// `PRAGMA user_version = 4` to the delivered-cost step leaves this
    /// assertion green, because the first pass moves the version to 4 before it
    /// is sampled and the second pass's `4 < 4` gate then does nothing. Only a
    /// non-idempotent stamp (`= version + 1`) trips it. The literal in
    /// `fresh_db_gains_the_delivered_columns` is what catches a re-stamp.
    #[test]
    fn rerun_adds_nothing_and_writes_no_version() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        let version_after_first = user_version(&conn);

        let added = add_missing_delivered_cost_columns(&conn).unwrap();
        assert!(added.is_empty(), "second pass added {added:?}");

        run_migrations(&conn).unwrap();
        assert_eq!(
            user_version(&conn),
            version_after_first,
            "re-running migrations must not move the version"
        );
        let cols = columns(&conn);
        for (name, _) in DELIVERED_COST_COLUMNS {
            assert!(cols.iter().any(|c| c == name), "lost column {name}");
        }
    }

    /// The case that motivated presence-gating: a database another lineage
    /// already carried past 4.
    ///
    /// DISCRIMINATING — replace the reconcile with a `if version < 4 { … }` body
    /// and this fails, because the columns never appear on a v5 database and
    /// every later INSERT naming them dies inside the silent `persist_record`
    /// discard.
    #[test]
    fn foreign_lineage_v5_db_gains_columns_and_keeps_its_version() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();

        // Simulate the observed default DB: a newer lineage's columns, its
        // version, and none of ours.
        for (name, _) in DELIVERED_COST_COLUMNS {
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
        // `reconcile_additive_columns`). This literal moves only if
        // `ticket/306`'s own rung moves. Raising it to follow some other version
        // would delete the "database is ahead of us" condition and leave the
        // test asserting nothing.
        conn.execute_batch(
            "ALTER TABLE token_savings ADD COLUMN provider TEXT;
             PRAGMA user_version = 5;",
        )
        .unwrap();

        run_migrations(&conn).unwrap();

        let cols = columns(&conn);
        for (name, _) in DELIVERED_COST_COLUMNS {
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

    /// The at-4 ORPHAN this branch's own history produced: commit 0cb0255
    /// stamped `PRAGMA user_version = 4` for these columns before HEAD retracted
    /// the claim, so databases at exactly 4 exist in the field. HEAD must leave
    /// one inert.
    ///
    /// The start state is 0cb0255's signature — our three columns present, the 4
    /// stamped, none of `ticket/305`'s columns — and it is the state the hazard
    /// block in `reconcile_additive_columns` calls fatal for the OTHER lineage,
    /// which is why it is pinned here rather than only reasoned about. The
    /// remedy (`PRAGMA user_version = 3`, restoring eligibility for
    /// `ticket/305`'s rebuild) is a human's one-line fix and is deliberately not
    /// asserted: nothing in this build performs it.
    #[test]
    fn starting_at_v4_is_a_noop() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        conn.execute_batch("PRAGMA user_version = 4;").unwrap();

        run_migrations(&conn).unwrap();

        assert_eq!(
            user_version(&conn),
            4,
            "an at-4 orphan must be left at exactly 4 — every rung is `version < \
             N` for N <= 3 and this step stamps nothing, so neither raises it nor \
             lowers it"
        );
        let cols = columns(&conn);
        for (name, _) in DELIVERED_COST_COLUMNS {
            assert!(cols.iter().any(|c| c == name), "at-4 orphan lost {name}");
        }
        assert!(
            !cols.iter().any(|c| c == "provider"),
            "the at-4 state pinned here is 0cb0255's, not `ticket/305`'s v4 — a \
             provider column means the fixture drifted onto the other lineage"
        );
    }

    /// Two processes opening a database that has not been reconciled yet both
    /// see a column absent, and the loser's `ALTER` must not fail the migration.
    ///
    /// A FILE database, because `:memory:` is per-connection — two in-memory
    /// connections are two different databases and the race is unobservable.
    /// The loser's stale snapshot is taken explicitly rather than raced for, so
    /// the test is deterministic: a stale read followed by a write IS the defect.
    ///
    /// DISCRIMINATING — drop the duplicate-column tolerance and the final
    /// `unwrap` panics with `duplicate column name: notice_tokens`. That is
    /// SQLITE_ERROR, not SQLITE_BUSY, so the connection's `busy_timeout` never
    /// applies; in production it propagates out of `AnalyticsDb::open` and the
    /// row is discarded by `persist_record` with no diagnostic at all.
    #[test]
    fn a_concurrent_first_open_tolerates_the_duplicate_column_alter() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("analytics.db");

        let winner = Connection::open(&path).unwrap();
        run_migrations(&winner).unwrap();
        // Back to the unreconciled state both processes would open.
        for (name, _) in DELIVERED_COST_COLUMNS {
            winner
                .execute_batch(&format!("ALTER TABLE token_savings DROP COLUMN {name};"))
                .unwrap();
        }

        let loser = Connection::open(&path).unwrap();
        let stale = declared_columns(&loser).unwrap();
        assert!(
            !stale.iter().any(|(c, _)| c == "notice_tokens"),
            "fixture: the loser must snapshot the table BEFORE the columns exist"
        );

        let won = add_missing_delivered_cost_columns(&winner).unwrap();
        assert_eq!(
            won.len(),
            DELIVERED_COST_COLUMNS.len(),
            "fixture: the winner must add all three"
        );

        let lost = apply_delivered_cost_columns(&loser, &stale).unwrap();
        assert!(
            lost.is_empty(),
            "the loser must add nothing and must not error; got {lost:?}"
        );
    }

    /// A same-named column of another declared type is REFUSED, not adopted.
    ///
    /// SQLite enforces no types, so nothing stops `served`'s 'raw'/'transformed'
    /// from landing in an INTEGER column: the write succeeds, `query_summary`
    /// prints the result as measured fact, and a name-only gate skips the column
    /// forever so it never self-heals. Failing loudly once beats measuring the
    /// wrong quantity indefinitely.
    #[test]
    fn a_same_named_column_of_another_type_is_refused() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        conn.execute_batch("ALTER TABLE token_savings DROP COLUMN served;")
            .unwrap();
        conn.execute_batch("ALTER TABLE token_savings ADD COLUMN served INTEGER;")
            .unwrap();

        let err = run_migrations(&conn).unwrap_err().to_string();
        assert!(
            err.contains("token_savings.served"),
            "the error must name the column: {err}"
        );
        assert!(
            err.contains("INTEGER") && err.contains("TEXT"),
            "and both the type found and the type required: {err}"
        );
    }

    /// The gate matches names case-insensitively, because SQLite identifiers are.
    ///
    /// DISCRIMINATING — restore the byte-exact `c == name` comparison and this
    /// fails by SUCCEEDING: `Notice_Tokens` reads as absent, so the type check
    /// never runs, and the `ALTER`'s `duplicate column name` (tolerated as
    /// success for the concurrency case) silently adopts a TEXT column as this
    /// build's INTEGER measurement. The tolerance is what makes the case gap
    /// invisible, which is why the two must be read together.
    #[test]
    fn the_gate_matches_column_names_case_insensitively() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        conn.execute_batch("ALTER TABLE token_savings DROP COLUMN notice_tokens;")
            .unwrap();
        conn.execute_batch("ALTER TABLE token_savings ADD COLUMN Notice_Tokens TEXT;")
            .unwrap();

        let err = run_migrations(&conn).unwrap_err().to_string();
        assert!(
            err.contains("token_savings.Notice_Tokens"),
            "the differently-cased column must be seen and named: {err}"
        );
    }

    /// A database that is not skim's is named, rather than surfacing as a bare
    /// `no such table` from the first `ALTER`.
    ///
    /// The `user_version = 3` is the threshold that makes this reachable: from 3
    /// up, every rung of the ladder is skipped, so a foreign file arrives at the
    /// reconcile intact. Below 3 the v3 `session_id` ALTER already fails on it,
    /// as it did before this check existed.
    #[test]
    fn a_foreign_database_is_named_not_alter_errored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-skims.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE unrelated (x INTEGER); PRAGMA user_version = 3;")
            .unwrap();

        let err = run_migrations(&conn).unwrap_err().to_string();
        assert!(
            err.contains("not-skims.db"),
            "the error must name the database file: {err}"
        );
        assert!(
            err.contains("token_savings"),
            "and the missing table: {err}"
        );
        assert!(err.contains("SKIM_ANALYTICS_DB"), "and the remedy: {err}");
    }

    /// A first reconcile records the series opening, and is NOT a reset.
    ///
    /// A fresh database's delivered population is empty because nothing has been
    /// measured yet — the same shape as an upgraded database before its first
    /// measured invocation, as `stats --clear`, and as a 90-day prune. Calling
    /// any of those a reset would manufacture a claim, which is the failure this
    /// marker exists to avoid rather than commit.
    #[test]
    fn a_first_reconcile_records_the_opening_and_no_reset() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();

        assert!(
            meta(&conn, DELIVERED_SERIES_OPENED_AT).is_some(),
            "the opening must be recorded outside token_savings, where a rebuild \
             of that table cannot take it"
        );
        assert!(
            meta(&conn, DELIVERED_SERIES_RESET_AT).is_none(),
            "a first reconcile is not a reset"
        );

        run_migrations(&conn).unwrap();
        assert!(
            meta(&conn, DELIVERED_SERIES_RESET_AT).is_none(),
            "and neither is an idempotent re-open"
        );
    }

    /// A rebuild that drops the columns leaves a durable mark, so the silent
    /// series reset stops being silent.
    ///
    /// DISCRIMINATING — `ticket/305`'s rebuild drops all three columns, commits
    /// cleanly, and the next open re-adds them empty; `query_summary` then
    /// reports zero rows and `render_delivered` prints nothing. Without this
    /// mark there is no artifact anywhere that says the series was ever
    /// populated, and "reset" is indistinguishable from "never measured".
    #[test]
    fn a_rebuild_that_drops_the_columns_leaves_a_reset_mark() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        let opened = meta(&conn, DELIVERED_SERIES_OPENED_AT).expect("opening recorded");

        for (name, _) in DELIVERED_COST_COLUMNS {
            conn.execute_batch(&format!("ALTER TABLE token_savings DROP COLUMN {name};"))
                .unwrap();
        }

        run_migrations(&conn).unwrap();

        assert!(
            meta(&conn, DELIVERED_SERIES_RESET_AT).is_some(),
            "re-adding all three columns to a database that had already been \
             reconciled means something dropped them in between — the one signal \
             that separates a reset series from one never measured"
        );
        assert_eq!(
            meta(&conn, DELIVERED_SERIES_OPENED_AT),
            Some(opened),
            "the original opening must survive the reset rather than be overwritten"
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
