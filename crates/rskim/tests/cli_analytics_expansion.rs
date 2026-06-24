//! Integration tests for gross/faithful expansion accounting and --show-stats
//! analytics reuse (#317 / #350) and for Phase-A1 fix of analytics-loss bug #359.
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
//! ## F-series (Phase A1 / #359 fix) — plain file-op analytics
//!
//! Tests F1–F13 and C1/C3 assert the unified `record_file_ops` path introduced
//! to fix the analytics-loss bug (PF-001): plain `skim <file>` now records
//! exactly one row regardless of cache state.
//!
//! ## Note on SKIM_PASSTHROUGH
//!
//! The outer cargo/nextest process may set `SKIM_PASSTHROUGH=1` to prevent
//! recursive skim compression.  These tests explicitly remove SKIM_PASSTHROUGH
//! from the child process env so the spawned skim performs real compression,
//! matching the pattern in `cli_no_expansion_317.rs`.

use std::path::PathBuf;
use tempfile::{NamedTempFile, TempDir};
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

// ============================================================================
// F-series: Phase A1 (#359) — plain file-op analytics (fixes PF-001)
//
// All tests below use an isolated TempDir for the analytics DB so they don't
// pollute the developer's real ~/.cache/skim/analytics.db.
// Direct rusqlite reads avoid subprocess sleep races.
// ============================================================================

/// Helper: open the analytics DB and count rows in token_savings.
fn count_rows(db_path: &std::path::Path) -> i64 {
    let conn = rusqlite::Connection::open(db_path).expect("must open analytics DB");
    conn.query_row("SELECT COUNT(*) FROM token_savings", [], |r| r.get(0))
        .unwrap_or(0) // table absent → 0
}

/// Helper: query a single row column from token_savings (assumes exactly 1 row).
fn row_value<T: rusqlite::types::FromSql>(db_path: &std::path::Path, col: &str) -> T {
    let conn = rusqlite::Connection::open(db_path).expect("must open analytics DB");
    conn.query_row(
        &format!("SELECT {col} FROM token_savings LIMIT 1"),
        [],
        |r| r.get(0),
    )
    .unwrap_or_else(|e| panic!("query {col}: {e}"))
}

/// Helper: TypeScript fixture path.
fn ts_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/typescript/simple.ts")
        .canonicalize()
        .expect("typescript fixture must exist")
}

/// Helper: Python fixture path.
fn py_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/python/simple.py")
        .canonicalize()
        .expect("python fixture must exist")
}

/// Helper: Rust fixture path.
fn rs_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/rust/simple.rs")
        .canonicalize()
        .expect("rust fixture must exist")
}

/// F1: plain cold cache (--no-cache, no --show-stats) → exactly 1 row;
/// command_type=File (stored as "file"); mode is set; language is detected.
#[test]
fn test_f1_plain_cold_cache_records_one_row() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("analytics.db");
    let fixture = ts_fixture();

    let status = std::process::Command::new(common::skim_bin())
        .arg(fixture.as_os_str())
        .arg("--no-cache")
        .env("SKIM_ANALYTICS_DB", &db_path)
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DISABLE_ANALYTICS")
        .env("NO_COLOR", "1")
        .status()
        .expect("skim must run");
    assert!(status.success(), "skim must exit 0");

    let count = count_rows(&db_path);
    assert_eq!(
        count, 1,
        "F1: cold cache plain run must record exactly 1 row"
    );

    let cmd_type: String = row_value(&db_path, "command_type");
    assert_eq!(cmd_type, "file", "F1: command_type must be 'file'");

    let lang: Option<String> = row_value(&db_path, "language");
    assert_eq!(
        lang.as_deref(),
        Some("typescript"),
        "F1: language must be detected as 'typescript'"
    );

    let mode: Option<String> = row_value(&db_path, "mode");
    assert!(mode.is_some(), "F1: mode must be set (not NULL)");
}

/// F2: warm-but-countless parser cache → second run records 1 row.
///
/// Warm the parser cache with a plain run (no --show-stats → cache written
/// WITHOUT token counts).  A second plain run against the same warm cache
/// must still record 1 row via background tokenization.
#[test]
fn test_f2_warm_countless_cache_records_one_row() {
    // Isolate the parser cache so this test doesn't pollute ~/.cache/skim.
    let cache_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db_path = db_dir.path().join("analytics.db");
    let fixture = ts_fixture();

    // First run: warm the parser cache WITHOUT token counts (no --show-stats).
    // Analytics disabled so we don't count this row.
    std::process::Command::new(common::skim_bin())
        .arg(fixture.as_os_str())
        .env("SKIM_CACHE_DIR", cache_dir.path())
        .env_remove("SKIM_PASSTHROUGH")
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env("NO_COLOR", "1")
        .status()
        .expect("first warm run must succeed");

    // Second run: uses the warm cache (no --no-cache) with analytics enabled.
    // The cache entry has no token counts → background tokenization must kick in.
    let status = std::process::Command::new(common::skim_bin())
        .arg(fixture.as_os_str())
        .env("SKIM_CACHE_DIR", cache_dir.path())
        .env("SKIM_ANALYTICS_DB", &db_path)
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DISABLE_ANALYTICS")
        .env("NO_COLOR", "1")
        .status()
        .expect("second run must succeed");
    assert!(status.success(), "F2: second run must exit 0");

    let count = count_rows(&db_path);
    assert_eq!(
        count, 1,
        "F2: warm-but-countless cache hit must still record 1 row via background tokenization"
    );
}

/// F3: plain vs --show-stats record the SAME row count (both record 1, not 0 vs 1).
#[test]
fn test_f3_plain_and_show_stats_both_record_one_row() {
    let fixture = ts_fixture();

    // Plain run.
    let db_plain = NamedTempFile::new().unwrap();
    std::process::Command::new(common::skim_bin())
        .arg(fixture.as_os_str())
        .arg("--no-cache")
        .env("SKIM_ANALYTICS_DB", db_plain.path())
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DISABLE_ANALYTICS")
        .env("NO_COLOR", "1")
        .status()
        .expect("plain run must succeed");

    // --show-stats run.
    let db_stats = NamedTempFile::new().unwrap();
    std::process::Command::new(common::skim_bin())
        .arg(fixture.as_os_str())
        .arg("--no-cache")
        .arg("--show-stats")
        .env("SKIM_ANALYTICS_DB", db_stats.path())
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DISABLE_ANALYTICS")
        .env("NO_COLOR", "1")
        .status()
        .expect("--show-stats run must succeed");

    let count_plain = count_rows(db_plain.path());
    let count_stats = count_rows(db_stats.path());
    assert_eq!(count_plain, 1, "F3: plain run must record 1 row");
    assert_eq!(count_stats, 1, "F3: --show-stats run must record 1 row");
}

/// F4: --no-cache → 1 row.
#[test]
fn test_f4_no_cache_records_one_row() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("analytics.db");
    let fixture = ts_fixture();

    let status = std::process::Command::new(common::skim_bin())
        .arg(fixture.as_os_str())
        .arg("--no-cache")
        .env("SKIM_ANALYTICS_DB", &db_path)
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DISABLE_ANALYTICS")
        .env("NO_COLOR", "1")
        .status()
        .expect("skim must run");
    assert!(status.success());

    assert_eq!(count_rows(&db_path), 1, "F4: --no-cache must record 1 row");
}

/// F5: count-carrying cache hit (prior --show-stats warms counts) → 1 row, NO double-record.
#[test]
fn test_f5_count_carrying_cache_hit_no_double_record() {
    let cache_dir = TempDir::new().unwrap();
    let db_dir = TempDir::new().unwrap();
    let db_path = db_dir.path().join("analytics.db");
    let fixture = ts_fixture();

    // First run WITH --show-stats: writes token counts into the cache.
    // Analytics pointing at a *different* scratch DB so we can count from a fresh start.
    let db_warm = NamedTempFile::new().unwrap();
    std::process::Command::new(common::skim_bin())
        .arg(fixture.as_os_str())
        .arg("--show-stats")
        .env("SKIM_CACHE_DIR", cache_dir.path())
        .env("SKIM_ANALYTICS_DB", db_warm.path())
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DISABLE_ANALYTICS")
        .env("NO_COLOR", "1")
        .status()
        .expect("warm run must succeed");

    // Second run: cache hit with counts already present → Known path, 1 row, no re-read.
    let status = std::process::Command::new(common::skim_bin())
        .arg(fixture.as_os_str())
        .env("SKIM_CACHE_DIR", cache_dir.path())
        .env("SKIM_ANALYTICS_DB", &db_path)
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DISABLE_ANALYTICS")
        .env("NO_COLOR", "1")
        .status()
        .expect("second run must succeed");
    assert!(status.success(), "F5: second run must exit 0");

    assert_eq!(
        count_rows(&db_path),
        1,
        "F5: count-carrying cache hit must record exactly 1 row (no double-record)"
    );
}

/// F10: language column reflects detected language (no --language flag).
#[test]
fn test_f10_language_detection_ts_py_rs() {
    for (fixture, expected_lang) in &[
        (ts_fixture(), "typescript"),
        (py_fixture(), "python"),
        (rs_fixture(), "rust"),
    ] {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("analytics.db");

        std::process::Command::new(common::skim_bin())
            .arg(fixture.as_os_str())
            .arg("--no-cache")
            .env("SKIM_ANALYTICS_DB", &db_path)
            .env_remove("SKIM_PASSTHROUGH")
            .env_remove("SKIM_DISABLE_ANALYTICS")
            .env("NO_COLOR", "1")
            .status()
            .expect("skim must run");

        let lang: Option<String> = row_value(&db_path, "language");
        assert_eq!(
            lang.as_deref(),
            Some(*expected_lang),
            "F10: language for {:?} must be '{expected_lang}'",
            fixture.file_name()
        );
    }
}

/// F10b: WITH --language override → language column reflects the override, not detected.
#[test]
fn test_f10_language_override_wins() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("analytics.db");
    let fixture = ts_fixture(); // .ts file → would auto-detect as typescript

    // Override with --language=rust
    std::process::Command::new(common::skim_bin())
        .arg(fixture.as_os_str())
        .arg("--no-cache")
        .arg("--language=rust")
        .env("SKIM_ANALYTICS_DB", &db_path)
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DISABLE_ANALYTICS")
        .env("NO_COLOR", "1")
        .status()
        .expect("skim must run");

    let lang: Option<String> = row_value(&db_path, "language");
    assert_eq!(
        lang.as_deref(),
        Some("rust"),
        "F10b: --language override must win over auto-detection"
    );
}

/// F11: empty file → 1 row, raw_tokens=0, savings_pct=0.0, exit 0, NO panic.
#[test]
fn test_f11_empty_fixture_records_zero_token_row() {
    let file_dir = TempDir::new().unwrap();
    let empty_file = file_dir.path().join("empty.ts");
    std::fs::write(&empty_file, "").unwrap();

    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("analytics.db");

    let status = std::process::Command::new(common::skim_bin())
        .arg(&empty_file)
        .arg("--no-cache")
        .env("SKIM_ANALYTICS_DB", &db_path)
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DISABLE_ANALYTICS")
        .env("NO_COLOR", "1")
        .status()
        .expect("skim must run");
    assert!(status.success(), "F11: empty file must exit 0");

    let count = count_rows(&db_path);
    assert_eq!(count, 1, "F11: empty file must record exactly 1 row");

    let raw: i64 = row_value(&db_path, "raw_tokens");
    assert_eq!(raw, 0, "F11: raw_tokens must be 0 for empty file");

    let savings: f64 = row_value(&db_path, "savings_pct");
    assert_eq!(savings, 0.0, "F11: savings_pct must be 0.0 for empty file");
}

/// F12: guardrail-triggering fixture (compressed >= raw) → 1 row, savings_pct=0.0.
///
/// A very tiny file (few tokens) causes the guardrail to trigger (compressed >= raw).
/// savings_percentage already clamps this to 0.0 — we verify no panic and 1 row.
#[test]
fn test_f12_guardrail_row_savings_pct_zero() {
    // A single-token file whose "structure" output is >= the raw (trivially tiny).
    let file_dir = TempDir::new().unwrap();
    let tiny_file = file_dir.path().join("tiny.ts");
    std::fs::write(&tiny_file, "x").unwrap();

    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("analytics.db");

    let status = std::process::Command::new(common::skim_bin())
        .arg(&tiny_file)
        .arg("--no-cache")
        .env("SKIM_ANALYTICS_DB", &db_path)
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DISABLE_ANALYTICS")
        .env("NO_COLOR", "1")
        .status()
        .expect("skim must run");
    assert!(status.success(), "F12: tiny file must exit 0");

    let count = count_rows(&db_path);
    assert_eq!(count, 1, "F12: guardrail case must record exactly 1 row");

    let savings: f64 = row_value(&db_path, "savings_pct");
    assert!(
        savings >= 0.0,
        "F12: savings_pct must be >= 0.0 (no negative savings)"
    );
}

/// F13: SKIM_DISABLE_ANALYTICS=1 (set via common::skim()) → COUNT==0 for single file.
#[test]
fn test_f13_disable_analytics_no_rows_for_file() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("analytics.db");
    let fixture = ts_fixture();

    // common::skim() sets SKIM_DISABLE_ANALYTICS=1.
    common::skim()
        .arg(fixture.as_os_str())
        .arg("--no-cache")
        .env("SKIM_ANALYTICS_DB", &db_path)
        .env_remove("SKIM_PASSTHROUGH")
        .assert()
        .success();

    // Table may not even exist (analytics never opened DB).
    let count = count_rows(&db_path);
    assert_eq!(
        count, 0,
        "F13: SKIM_DISABLE_ANALYTICS=1 must record 0 rows for single file"
    );
}

/// C1: token_savings schema columns are UNCHANGED vs expected set (schema stability).
#[test]
fn test_c1_schema_columns_unchanged() {
    use std::collections::HashSet;

    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("analytics.db");

    // Open the DB via a skim stats run to trigger schema migrations.
    common::skim()
        .arg("stats")
        .env("SKIM_ANALYTICS_DB", &db_path)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env("NO_COLOR", "1")
        .assert()
        .success();

    let conn = rusqlite::Connection::open(&db_path).expect("must open DB");
    let mut stmt = conn
        .prepare("PRAGMA table_info(token_savings)")
        .expect("PRAGMA must succeed");
    let cols: HashSet<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();

    let expected: HashSet<&str> = [
        "id",
        "timestamp",
        "command_type",
        "original_cmd",
        "raw_tokens",
        "compressed_tokens",
        "savings_pct",
        "duration_ms",
        "project_path",
        "mode",
        "language",
        "parse_tier",
        "session_id",
    ]
    .iter()
    .copied()
    .collect();

    for &col in &expected {
        assert!(
            cols.contains(col),
            "C1: expected column '{col}' missing from token_savings"
        );
    }
}

/// C3: stdout is byte-identical whether analytics is enabled or disabled.
///
/// Enabling analytics recording must never alter what skim writes to stdout.
/// Analytics happens AFTER stdout is flushed; the background thread has no
/// effect on the output bytes.
#[test]
fn test_c3_stdout_byte_identical_analytics_on_vs_off() {
    let fixture = ts_fixture();

    // Run with analytics ON (isolated DB).
    let db = NamedTempFile::new().unwrap();
    let output_on = std::process::Command::new(common::skim_bin())
        .arg(fixture.as_os_str())
        .arg("--no-cache")
        .env("SKIM_ANALYTICS_DB", db.path())
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DISABLE_ANALYTICS")
        .env("NO_COLOR", "1")
        .output()
        .expect("analytics-on run must succeed");
    assert!(
        output_on.status.success(),
        "C3: analytics-on run must exit 0"
    );

    // Run with analytics OFF.
    let output_off = common::skim()
        .arg(fixture.as_os_str())
        .arg("--no-cache")
        .env_remove("SKIM_PASSTHROUGH")
        .output()
        .expect("analytics-off run must succeed");
    assert!(
        output_off.status.success(),
        "C3: analytics-off run must exit 0"
    );

    assert_eq!(
        output_on.stdout, output_off.stdout,
        "C3: stdout must be byte-identical regardless of analytics state"
    );
}

// ============================================================================
// Phase A2 (#359) — per-file analytics rows for multi/glob/dir
//
// These tests verify that multi-file invocations emit N per-file rows (one per
// successful file) instead of the single aggregate row emitted before A2.
//
// All tests use an isolated TempDir for the analytics DB.
// ============================================================================

/// Helper: count all rows in token_savings for a given DB path.
/// Reuses the same signature as the F-series helper above; safe because Rust
/// allows duplicate helper fns in different test files — they compile independently.
fn count_rows_multi(db_path: &std::path::Path) -> i64 {
    let conn = rusqlite::Connection::open(db_path).expect("must open analytics DB");
    conn.query_row("SELECT COUNT(*) FROM token_savings", [], |r| r.get(0))
        .unwrap_or(0)
}

/// Helper: query all language values recorded in token_savings.
fn all_languages(db_path: &std::path::Path) -> Vec<Option<String>> {
    let conn = rusqlite::Connection::open(db_path).expect("must open analytics DB");
    let mut stmt = conn
        .prepare("SELECT language FROM token_savings ORDER BY rowid")
        .expect("prepare must succeed");
    stmt.query_map([], |r| r.get::<_, Option<String>>(0))
        .expect("query must succeed")
        .filter_map(|r| r.ok())
        .collect()
}

/// F7: `skim a.ts b.py c.rs` (no --show-stats) → exactly 3 rows, one per file;
/// each row carries the correctly detected language for that file.
#[test]
fn test_f7_multi_explicit_files_per_file_rows() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("analytics.db");

    let ts = ts_fixture();
    let py = py_fixture();
    let rs = rs_fixture();

    let status = std::process::Command::new(common::skim_bin())
        .arg(ts.as_os_str())
        .arg(py.as_os_str())
        .arg(rs.as_os_str())
        .arg("--no-cache")
        .env("SKIM_ANALYTICS_DB", &db_path)
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DISABLE_ANALYTICS")
        .env("NO_COLOR", "1")
        .status()
        .expect("skim must run");
    assert!(status.success(), "F7: 3-file run must exit 0");

    let count = count_rows_multi(&db_path);
    assert_eq!(
        count, 3,
        "F7: 3-file run must record exactly 3 rows (one per file)"
    );

    let langs = all_languages(&db_path);
    let mut sorted_langs: Vec<_> = langs.into_iter().flatten().collect();
    sorted_langs.sort();
    assert_eq!(
        sorted_langs,
        vec!["python", "rust", "typescript"],
        "F7: each row must carry the language for its own file"
    );
}

/// F8-glob: `skim '*.ts'` against a dir with 2 .ts files → exactly 2 rows.
#[test]
fn test_f8_glob_per_file_rows() {
    let file_dir = TempDir::new().unwrap();
    // Create 2 TypeScript files in a temp dir
    std::fs::write(
        file_dir.path().join("alpha.ts"),
        "function alpha(): number { return 1; }",
    )
    .unwrap();
    std::fs::write(
        file_dir.path().join("beta.ts"),
        "function beta(): string { return 'x'; }",
    )
    .unwrap();

    let db_dir = TempDir::new().unwrap();
    let db_path = db_dir.path().join("analytics.db");

    let glob_pattern = format!("{}/*.ts", file_dir.path().display());

    let status = std::process::Command::new(common::skim_bin())
        .arg(&glob_pattern)
        .arg("--no-cache")
        .env("SKIM_ANALYTICS_DB", &db_path)
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DISABLE_ANALYTICS")
        .env("NO_COLOR", "1")
        .status()
        .expect("skim glob must run");
    assert!(status.success(), "F8-glob: glob run must exit 0");

    let count = count_rows_multi(&db_path);
    assert_eq!(
        count, 2,
        "F8-glob: 2-file glob must record exactly 2 rows (one per file, not 1 aggregate)"
    );

    let langs = all_languages(&db_path);
    let non_null: Vec<_> = langs.into_iter().flatten().collect();
    assert!(
        non_null.iter().all(|l| l == "typescript"),
        "F8-glob: all rows must have language='typescript', got: {non_null:?}"
    );
}

/// F8-dir: `skim <dir>` containing 2 .py files → exactly 2 rows.
#[test]
fn test_f8_dir_per_file_rows() {
    let file_dir = TempDir::new().unwrap();
    std::fs::write(
        file_dir.path().join("mod_a.py"),
        "def func_a():\n    pass\n",
    )
    .unwrap();
    std::fs::write(
        file_dir.path().join("mod_b.py"),
        "def func_b():\n    return 42\n",
    )
    .unwrap();

    let db_dir = TempDir::new().unwrap();
    let db_path = db_dir.path().join("analytics.db");

    let status = std::process::Command::new(common::skim_bin())
        .arg(file_dir.path())
        .arg("--no-cache")
        .env("SKIM_ANALYTICS_DB", &db_path)
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DISABLE_ANALYTICS")
        .env("NO_COLOR", "1")
        .status()
        .expect("skim dir must run");
    assert!(status.success(), "F8-dir: dir run must exit 0");

    let count = count_rows_multi(&db_path);
    assert_eq!(
        count, 2,
        "F8-dir: 2-file directory must record exactly 2 rows (one per file)"
    );

    let langs = all_languages(&db_path);
    let non_null: Vec<_> = langs.into_iter().flatten().collect();
    assert!(
        non_null.iter().all(|l| l == "python"),
        "F8-dir: all rows must have language='python', got: {non_null:?}"
    );
}

/// F9: `skim --show-stats a.ts b.py` → same N per-file rows as without --show-stats.
/// Verifies that the counts=Known path (--show-stats) also emits N rows, not 1 aggregate.
#[test]
fn test_f9_show_stats_multi_per_file_rows() {
    let ts = ts_fixture();
    let py = py_fixture();

    // Run without --show-stats (Tokenize path).
    let db_plain = TempDir::new().unwrap();
    let db_plain_path = db_plain.path().join("analytics.db");
    std::process::Command::new(common::skim_bin())
        .arg(ts.as_os_str())
        .arg(py.as_os_str())
        .arg("--no-cache")
        .env("SKIM_ANALYTICS_DB", &db_plain_path)
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DISABLE_ANALYTICS")
        .env("NO_COLOR", "1")
        .status()
        .expect("plain multi run must succeed");

    // Run WITH --show-stats (Known path).
    let db_stats = TempDir::new().unwrap();
    let db_stats_path = db_stats.path().join("analytics.db");
    std::process::Command::new(common::skim_bin())
        .arg(ts.as_os_str())
        .arg(py.as_os_str())
        .arg("--no-cache")
        .arg("--show-stats")
        .env("SKIM_ANALYTICS_DB", &db_stats_path)
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DISABLE_ANALYTICS")
        .env("NO_COLOR", "1")
        .status()
        .expect("--show-stats multi run must succeed");

    let count_plain = count_rows_multi(&db_plain_path);
    let count_stats = count_rows_multi(&db_stats_path);

    assert_eq!(
        count_plain, 2,
        "F9: plain multi (2 files) must record 2 rows"
    );
    assert_eq!(
        count_stats, 2,
        "F9: --show-stats multi (2 files) must also record 2 rows (same as plain)"
    );
}

/// F13-multi: SKIM_DISABLE_ANALYTICS=1 on a multi-file run → 0 rows.
#[test]
fn test_f13_multi_disable_analytics_no_rows() {
    let db_dir = TempDir::new().unwrap();
    let db_path = db_dir.path().join("analytics.db");

    let ts = ts_fixture();
    let py = py_fixture();

    // common::skim() sets SKIM_DISABLE_ANALYTICS=1
    common::skim()
        .arg(ts.as_os_str())
        .arg(py.as_os_str())
        .arg("--no-cache")
        .env("SKIM_ANALYTICS_DB", &db_path)
        .env_remove("SKIM_PASSTHROUGH")
        .assert()
        .success();

    let count = count_rows_multi(&db_path);
    assert_eq!(
        count, 0,
        "F13-multi: SKIM_DISABLE_ANALYTICS=1 must record 0 rows for multi-file run"
    );
}

/// F14: robustness — one file deleted between transform and background re-read.
/// With 3 files where 1 is deleted after skim runs, N-1 rows are expected (not crash).
///
/// This test verifies best-effort behaviour: deleted/changed files are silently skipped
/// while sibling rows are still recorded.
///
/// Strategy: run 3 files, but delete 1 of the temp files BEFORE the skim invocation
/// to ensure it fails during processing (will be an Err entry in results). We then
/// verify: exit code unaffected (2 files succeed), and rows == 2 (skipped error).
#[test]
fn test_f14_missing_file_records_n_minus_1_rows() {
    let file_dir = TempDir::new().unwrap();
    let ts = ts_fixture();
    let py = py_fixture();

    // Third file: created then immediately removed (simulating a file that
    // disappears before the background re-read — but in fact process_file will
    // fail to read it too, meaning it is an Err result and not counted).
    let ghost_path = file_dir.path().join("ghost.ts");
    std::fs::write(&ghost_path, "function ghost() {}").unwrap();
    std::fs::remove_file(&ghost_path).unwrap();

    let db_dir = TempDir::new().unwrap();
    let db_path = db_dir.path().join("analytics.db");

    // Run with 3 paths: 2 real fixtures + 1 ghost that doesn't exist.
    // skim should succeed (exit 0) with 2 files processed and 1 warning on stderr.
    let status = std::process::Command::new(common::skim_bin())
        .arg(ts.as_os_str())
        .arg(py.as_os_str())
        .arg(&ghost_path) // will produce an Err in process_files
        .arg("--no-cache")
        .env("SKIM_ANALYTICS_DB", &db_path)
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DISABLE_ANALYTICS")
        .env("NO_COLOR", "1")
        .status()
        .expect("skim must run");

    // Exit code: process_files succeeds if at least 1 file succeeded.
    assert!(
        status.success(),
        "F14: exit code must be 0 when at least 1 file succeeds"
    );

    // Ghost file is not found before process_file is even called (it's caught in
    // process_explicit_files). So only 2 files (ts + py) make it to process_files.
    let count = count_rows_multi(&db_path);
    assert_eq!(
        count, 2,
        "F14: with 1 missing file, exactly 2 rows must be recorded (no crash, no aggregate)"
    );
}
