//! Integration tests for `skim stats` subcommand (#56).
//!
//! All tests use `tempfile::NamedTempFile` + `SKIM_ANALYTICS_DB` env override
//! for isolation. `SKIM_DISABLE_ANALYTICS=1` prevents test invocations from
//! recording to the database. `NO_COLOR=1` prevents colored output from
//! interfering with assertions.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::NamedTempFile;

// ============================================================================
// Helper: build an isolated `skim stats` command
// ============================================================================

/// Create a `skim stats` command with an isolated analytics database.
///
/// Sets `SKIM_ANALYTICS_DB` to a temporary file path, `SKIM_DISABLE_ANALYTICS=1`
/// to prevent test interference, and `NO_COLOR=1` to disable colored output.
fn skim_stats_cmd(db_file: &NamedTempFile) -> Command {
    let mut cmd = Command::cargo_bin("skim").unwrap();
    cmd.arg("stats")
        .env("SKIM_ANALYTICS_DB", db_file.path().as_os_str())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env("NO_COLOR", "1");
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
        .stdout(predicate::str::contains("--cost"))
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

    assert!(output.status.success(), "skim stats --format json should exit 0");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Expected valid JSON, got parse error: {e}\nstdout: {stdout}"));

    // Verify expected top-level keys exist
    assert!(json.get("summary").is_some(), "JSON should contain 'summary' key");
    assert!(json.get("daily").is_some(), "JSON should contain 'daily' key");
    assert!(json.get("by_command").is_some(), "JSON should contain 'by_command' key");
    assert!(json.get("by_language").is_some(), "JSON should contain 'by_language' key");
    assert!(json.get("by_mode").is_some(), "JSON should contain 'by_mode' key");
    assert!(json.get("tier_distribution").is_some(), "JSON should contain 'tier_distribution' key");
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
// Cost flag — should include cost section in JSON output
// ============================================================================

#[test]
fn test_stats_cost_flag() {
    let db = NamedTempFile::new().unwrap();
    let output = skim_stats_cmd(&db)
        .args(["--format", "json", "--cost"])
        .output()
        .unwrap();

    assert!(output.status.success(), "skim stats --format json --cost should exit 0");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("Expected valid JSON, got parse error: {e}\nstdout: {stdout}"));

    // With --cost, the JSON should include cost_estimate section
    let cost = json.get("cost_estimate");
    assert!(cost.is_some(), "JSON should contain 'cost_estimate' key when --cost is passed");

    let cost = cost.unwrap();
    assert!(
        cost.get("model").is_some(),
        "cost_estimate should contain 'model' key"
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
