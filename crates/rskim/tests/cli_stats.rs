//! Integration tests for `skim stats` subcommand (#56).
//!
//! All tests use `tempfile::NamedTempFile` + `SKIM_ANALYTICS_DB` env override
//! for isolation. `SKIM_DISABLE_ANALYTICS=1` prevents test invocations from
//! recording to the database. `NO_COLOR=1` prevents colored output from
//! interfering with assertions.

use predicates::prelude::*;
use std::path::PathBuf;
use tempfile::NamedTempFile;
mod common;

// ============================================================================
// Helper: build an isolated `skim stats` command
// ============================================================================

/// Create a `skim stats` command with an isolated analytics database.
///
/// Uses `common::skim()` (analytics OFF, NO_COLOR=1) and adds `SKIM_ANALYTICS_DB`
/// pointing at the temp file so the stats subcommand reads from the isolated DB.
fn skim_stats_cmd(db_file: &NamedTempFile) -> assert_cmd::Command {
    let mut cmd = common::skim();
    cmd.arg("stats")
        .env("SKIM_ANALYTICS_DB", db_file.path().as_os_str());
    cmd
}

// ============================================================================
// Help
// ============================================================================

#[test]
fn test_stats_help() {
    let db = NamedTempFile::new().unwrap();
    skim_stats_cmd(&db)
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("stats"))
        .stdout(predicate::str::contains("--since"))
        .stdout(predicate::str::contains("--format"))
        .stdout(predicate::str::contains("--verbose"))
        .stdout(predicate::str::contains("--clear"));
}

// ============================================================================
// Empty database — graceful message
// ============================================================================

#[test]
fn test_stats_empty_db() {
    let db = NamedTempFile::new().unwrap();
    skim_stats_cmd(&db)
        .assert()
        .success()
        .stdout(predicate::str::contains("No analytics data found"));
}

// ============================================================================
// JSON format — empty database should produce valid JSON
// ============================================================================

#[test]
fn test_stats_json_format() {
    let db = NamedTempFile::new().unwrap();
    let output = skim_stats_cmd(&db)
        .args(["--format", "json"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "skim stats --format json should exit 0"
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Expected valid JSON, got parse error: {e}\nstdout: {stdout}"));

    // Verify expected top-level keys exist
    assert!(
        json.get("summary").is_some(),
        "JSON should contain 'summary' key"
    );
    assert!(
        json.get("daily").is_some(),
        "JSON should contain 'daily' key"
    );
    assert!(
        json.get("by_command").is_some(),
        "JSON should contain 'by_command' key"
    );
    assert!(
        json.get("by_language").is_some(),
        "JSON should contain 'by_language' key"
    );
    assert!(
        json.get("by_mode").is_some(),
        "JSON should contain 'by_mode' key"
    );
    assert!(
        json.get("tier_distribution").is_some(),
        "JSON should contain 'tier_distribution' key"
    );
    assert!(
        json.get("by_original_cmd").is_some(),
        "JSON should contain 'by_original_cmd' key"
    );
}

// ============================================================================
// Clear — should succeed on empty or populated database
// ============================================================================

#[test]
fn test_stats_clear() {
    let db = NamedTempFile::new().unwrap();
    skim_stats_cmd(&db)
        .arg("--clear")
        .assert()
        .success()
        .stdout(predicate::str::contains("Analytics data cleared"));
}

// ============================================================================
// Cost estimate — always present in JSON output
// ============================================================================

#[test]
fn test_stats_json_always_includes_cost_estimate() {
    let db = NamedTempFile::new().unwrap();
    let output = skim_stats_cmd(&db)
        .args(["--format", "json"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "skim stats --format json should exit 0"
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Expected valid JSON, got parse error: {e}\nstdout: {stdout}"));

    // cost_estimate is always included in JSON output (no flag required)
    let cost = json.get("cost_estimate");
    assert!(
        cost.is_some(),
        "JSON should always contain 'cost_estimate' key"
    );

    let cost = cost.unwrap();
    assert!(
        cost.get("tier").is_some(),
        "cost_estimate should contain 'tier' key"
    );
    assert!(
        cost.get("input_cost_per_mtok").is_some(),
        "cost_estimate should contain 'input_cost_per_mtok' key"
    );
    assert!(
        cost.get("estimated_savings_usd").is_some(),
        "cost_estimate should contain 'estimated_savings_usd' key"
    );
    assert!(
        cost.get("tokens_saved").is_some(),
        "cost_estimate should contain 'tokens_saved' key"
    );
}

// ============================================================================
// --verbose: Parse Quality section
// ============================================================================

#[test]
fn test_stats_verbose_shows_parse_quality() {
    let db = NamedTempFile::new().unwrap();

    // Run skim on a real source file with analytics ENABLED so the DB contains
    // at least one record.  `--show-stats` is required to populate token counts;
    // without it `ProcessResult::original_tokens` is None and no record is saved.
    // Use common::skim_with_analytics() to enable recording into the isolated DB.
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/typescript/simple.ts")
        .canonicalize()
        .expect("fixture must exist");
    common::skim_with_analytics(db.path())
        .arg(fixture.as_os_str())
        .arg("--show-stats")
        .assert()
        .success();

    // Analytics recording is fire-and-forget on a background thread; give it a
    // brief moment to flush before querying stats.
    std::thread::sleep(std::time::Duration::from_millis(200));

    // `skim stats --verbose` should show the "Parse Quality" section when data
    // is present.
    skim_stats_cmd(&db)
        .arg("--verbose")
        .assert()
        .success()
        .stdout(predicate::str::contains("Parse Quality"));
}
