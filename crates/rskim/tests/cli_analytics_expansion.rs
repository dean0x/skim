//! Integration tests for gross/faithful expansion accounting and --show-stats
//! analytics reuse (#317 / #350).
//!
//! ## T10 (AC-F4/F5) — --show-stats records exactly one row with the same counts
//!
//! Run a wrapped command WITH and WITHOUT `--show-stats` against a temp analytics
//! DB; assert exactly ONE row is recorded each time.  Because the `--show-stats`
//! code path calls `try_record_command_with_counts` (reusing already-computed
//! counts) rather than `try_record_command` (background re-tokenization), the row
//! counts and parse tier should be identical for the same input.
//!
//! ## T11 (AC-N1 e2e) — expansion row stored with true count; tokens_saved = 0
//!
//! Seed the analytics DB with a raw expansion record (compressed_tokens >
//! raw_tokens) inserted directly via rusqlite, then run `skim stats --format json`
//! and assert the row shows true counts with saved = 0.
//!
//! ## Note on SKIM_PASSTHROUGH
//!
//! The outer cargo/nextest process may set `SKIM_PASSTHROUGH=1` to prevent
//! recursive skim compression.  These tests explicitly remove SKIM_PASSTHROUGH
//! from the child process env so the spawned skim performs real compression,
//! matching the pattern in `cli_no_expansion_317.rs`.

use std::path::PathBuf;
use tempfile::NamedTempFile;
mod common;

// ============================================================================
// Helpers
// ============================================================================

/// Read `skim stats --format json` output from an analytics DB file.
fn read_stats_json(db: &NamedTempFile) -> serde_json::Value {
    // Give the background analytics thread time to flush.
    std::thread::sleep(std::time::Duration::from_millis(300));

    // Use common::skim() (analytics OFF) to read stats from the isolated DB.
    // SKIM_ANALYTICS_DB points at the temp file, SKIM_DISABLE_ANALYTICS is
    // already set by common::skim() so this read-only stats call doesn't
    // record new rows.
    let output = common::skim()
        .arg("stats")
        .arg("--format")
        .arg("json")
        .env("SKIM_ANALYTICS_DB", db.path().as_os_str())
        .output()
        .expect("skim stats must run");

    assert!(
        output.status.success(),
        "skim stats --format json must exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("stats output must be UTF-8");
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Expected valid JSON from skim stats: {e}\nstdout: {stdout}"))
}

/// A small but real file that skim can process (used for --show-stats tests).
fn fixture_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/typescript/simple.ts")
        .canonicalize()
        .expect("typescript fixture must exist")
}

// ============================================================================
// T10 (AC-F4/F5) — --show-stats records exactly one row per run
//
// Note: These tests drive skim's FILE mode (reading a source file), not a
// wrapped subcommand.  The file mode goes through the same `record_and_report`
// code path; `--show-stats` triggers `try_record_command_with_counts` while the
// absence of `--show-stats` takes the background re-tokenization path.  Both
// paths must record exactly one row.
// ============================================================================

/// AC-F4: run `skim <file> --show-stats` against a temp DB.
/// Assert exactly one analytics row is recorded (not two — no double-record).
#[test]
fn test_show_stats_on_records_exactly_one_row() {
    let db = NamedTempFile::new().unwrap();
    let fixture = fixture_file();

    // Run WITH --show-stats; analytics enabled (no SKIM_DISABLE_ANALYTICS).
    let status = std::process::Command::new(common::skim_bin())
        .arg(fixture.as_os_str())
        .arg("--show-stats")
        .env("SKIM_ANALYTICS_DB", db.path().as_os_str())
        .env_remove("SKIM_PASSTHROUGH") // ensure compression is active
        .env_remove("SKIM_DISABLE_ANALYTICS")
        .env("NO_COLOR", "1")
        .status()
        .expect("skim must run");
    assert!(status.success(), "skim must exit 0 with --show-stats");

    let stats = read_stats_json(&db);
    let invocations = stats["summary"]["invocations"]
        .as_u64()
        .expect("summary.invocations must be a number");
    assert_eq!(
        invocations, 1,
        "exactly one analytics row must be recorded with --show-stats (not double-recorded)"
    );
}

// Corrected root cause for the WITHDRAWN `test_show_stats_off_records_exactly_one_row`
// (commit 8325d45) and the #[ignore]'d regression test below.
//
// That test asserted a plain `skim <file>` (no --show-stats) records exactly one row; it
// passed on macOS and flaked to 0 on Linux CI. The cause was NOT a fire-and-forget /
// background-writer race: `record_with_counts` registers its thread in PENDING_THREADS and
// `flush_pending()` (main.rs, after the file pipeline) joins it before the process exits, so
// reading the DB after exit is deterministic. The real cause is PARSER-CACHE STATE: the
// file-op path records only when token counts are already known, which — without
// --show-stats — happens only on a cache HIT of an entry written by a prior --show-stats run
// (process.rs `try_cached_result`). Tests share the default ~/.cache/skim, so macOS had a
// warm `simple.ts` entry (recorded 1) while cold CI recorded 0.
//
// That cache-state dependency is a real, pre-existing analytics-loss bug (tracked in #359):
// plain `skim <file>` — the common agent invocation — drops token-savings data on a cold or
// plain-warmed cache. The DESIRED behavior is asserted by the #[ignore]'d regression test
// below; remove #[ignore] when #359 is fixed. The no-double-record contract remains covered
// deterministically by `test_show_stats_on_records_exactly_one_row` (synchronous
// --show-stats path) and the unit test `test_expansion_stored_as_true_count_not_clamped`.

/// Regression test for #359 (pre-existing analytics loss): a plain `skim <file>` (no
/// `--show-stats`) SHOULD record exactly one token-savings row, independent of parser-cache
/// state. It currently records ZERO because the file-op path records only when token counts
/// are already computed — which, without `--show-stats`, happens only on a cache hit carrying
/// counts from a prior `--show-stats` run. `--no-cache` removes all cache-state variance, so
/// this is deterministic on every host: 0 today, 1 once #359 is fixed.
///
/// Determinism note: this is NOT the racy shape that flaked. `record_with_counts` registers
/// its thread and `flush_pending()` joins it before exit, so the post-exit direct DB read is
/// race-free. The DB is read with rusqlite directly — no `skim stats` subprocess, no sleep.
#[test]
#[ignore = "known pre-existing bug #359: plain file-op analytics is dropped unless the parser cache carries counts"]
fn test_plain_file_op_should_record_analytics_no_cache() {
    let db = NamedTempFile::new().unwrap();
    let fixture = fixture_file();

    // Plain run: NO --show-stats, --no-cache (eliminates parser-cache state entirely),
    // analytics enabled. This mirrors the common agent invocation shape.
    let status = std::process::Command::new(common::skim_bin())
        .arg(fixture.as_os_str())
        .arg("--no-cache")
        .env("SKIM_ANALYTICS_DB", db.path().as_os_str())
        .env_remove("SKIM_PASSTHROUGH") // ensure compression is active
        .env_remove("SKIM_DISABLE_ANALYTICS") // analytics on
        .env("NO_COLOR", "1")
        .status()
        .expect("skim must run");
    assert!(status.success(), "skim must exit 0");

    // Direct, deterministic read (flush_pending joined the writer before exit).
    let conn = rusqlite::Connection::open(db.path()).expect("must open analytics DB");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM token_savings", [], |r| r.get(0))
        .unwrap_or(0); // table is absent when nothing was recorded
    assert_eq!(
        count, 1,
        "a plain `skim <file>` should record exactly one analytics row regardless of cache \
         state (currently records 0 — see #359)"
    );
}

// ============================================================================
// T11 (AC-N1 e2e) — expansion row: true count stored, tokens_saved = 0
// ============================================================================

/// AC-N1 e2e: seed a row with compressed_tokens > raw_tokens into the analytics
/// DB by first running `skim stats` (which opens the DB and applies schema
/// migrations), then inserting an expansion row via rusqlite into the
/// properly-migrated schema, and finally asserting `skim stats --format json` shows:
///   1. compressed_tokens is the TRUE value (greater than raw_tokens)
///   2. tokens_saved for the session is 0 (floored per-row CASE WHEN)
///
/// **Scope note**: This test covers the *query layer* (CASE WHEN flooring) and
/// *JSON presentation* only.  The no-clamp write path (removal of the
/// `.min(raw_tokens)` clamp in `analytics/mod.rs`) is covered separately by the
/// in-crate unit test `test_expansion_stored_as_true_count_not_clamped`, which
/// exercises `db.record(&record)` directly.  Seeding here via raw rusqlite INSERT
/// deliberately bypasses the write path to test the read/query layer in isolation.
#[test]
fn test_expansion_row_stored_true_count_stats_shows_zero_saved() {
    let db = NamedTempFile::new().unwrap();

    // Step 1: run `skim stats` once to let skim open the DB and apply all schema
    // migrations (so the table has all expected columns including session_id).
    common::skim()
        .arg("stats")
        .env("SKIM_ANALYTICS_DB", db.path().as_os_str())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env("NO_COLOR", "1")
        .assert()
        .success();

    // Step 2: insert an expansion row directly via rusqlite now that the schema
    // is in place (all migrations applied by skim in step 1).
    {
        let conn = rusqlite::Connection::open(db.path()).expect("must open DB");
        // Expansion row: raw=100, compressed=150 (50 more than raw).
        conn.execute(
            "INSERT INTO token_savings \
             (timestamp, command_type, original_cmd, raw_tokens, compressed_tokens, \
              savings_pct, duration_ms, project_path) \
             VALUES (1711300000, 'heatmap', 'skim heatmap', 100, 150, 0.0, 10, '/tmp/test')",
            [],
        )
        .expect("insert must succeed");
    }

    // Step 3: read back via skim stats --format json.
    let stats = read_stats_json(&db);

    let summary = &stats["summary"];
    let raw_tokens = summary["raw_tokens"]
        .as_u64()
        .expect("summary.raw_tokens must be present");
    let compressed_tokens = summary["compressed_tokens"]
        .as_u64()
        .expect("summary.compressed_tokens must be present");
    let tokens_saved = summary["tokens_saved"]
        .as_u64()
        .expect("summary.tokens_saved must be present");

    // True counts must be stored (not clamped).
    assert_eq!(
        raw_tokens, 100,
        "raw_tokens must equal the recorded value (100)"
    );
    assert_eq!(
        compressed_tokens, 150,
        "compressed_tokens must be true (150), not clamped to raw (100)"
    );
    // tokens_saved must be floored to 0 for expansion rows.
    assert_eq!(
        tokens_saved, 0,
        "tokens_saved must be 0 for an expansion row (compressed > raw)"
    );

    // Daily breakdown must also show 0 tokens_saved for this day.
    // daily lives at top level, not nested under summary.
    let daily = stats["daily"].as_array().expect("daily must be an array");
    assert_eq!(daily.len(), 1, "one day's data");
    let day_saved = daily[0]["tokens_saved"]
        .as_u64()
        .expect("day tokens_saved must be present");
    assert_eq!(
        day_saved, 0,
        "daily tokens_saved must be 0 for expansion-only day"
    );
}
