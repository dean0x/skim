//! Integration tests for `skim search` subcommand (#3).

use assert_cmd::Command;
use predicates::prelude::*;

fn skim_cmd() -> Command {
    Command::cargo_bin("skim").unwrap()
}

// ============================================================================
// Help flag tests (unchanged behaviour)
// ============================================================================

#[test]
fn test_search_help() {
    skim_cmd()
        .args(["search", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim search"))
        .stdout(predicate::str::contains("--ast"));
}

#[test]
fn test_search_short_help() {
    skim_cmd()
        .args(["search", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim search"));
}

#[test]
fn test_search_help_contains_all_flags() {
    let assert = skim_cmd().args(["search", "--help"]).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let expected_flags = [
        "--build",
        "--rebuild",
        "--update",
        "--ast",
        "--blast-radius",
        "--limit",
        "--hot",
        "--cold",
        "--risky",
        "--stats",
        "--clear-cache",
        "--json",
        "--help",
    ];
    for flag in &expected_flags {
        assert!(stdout.contains(flag), "help output missing flag: {flag}");
    }
}

#[test]
fn test_search_help_contains_usage_line() {
    skim_cmd()
        .args(["search", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage: skim search"));
}

#[test]
fn test_search_help_at_end() {
    // --help after positional arg still shows help.
    skim_cmd()
        .args(["search", "test", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim search"));
}

// ============================================================================
// No-args / no-query error cases
// ============================================================================

#[test]
fn test_search_no_args_prints_usage() {
    // With no args and no index, we should get a usage message and fail.
    skim_cmd()
        .args(["search"])
        .env("SKIM_CACHE_DIR", "/tmp/skim_test_no_args")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Usage: skim search"));
}

#[test]
fn test_search_no_query_no_build_exit_code() {
    let output = skim_cmd()
        .args(["search"])
        .env("SKIM_CACHE_DIR", "/tmp/skim_test_no_query_exit")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "no-query search should exit with code 1"
    );
}

#[test]
fn test_search_empty_query_fails() {
    // An empty string positional arg should produce a usage message and fail.
    skim_cmd()
        .args(["search", ""])
        .env("SKIM_CACHE_DIR", "/tmp/skim_test_empty_query")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Usage: skim search"));
}

// ============================================================================
// --stats when no index exists
// ============================================================================

#[test]
fn test_search_stats_no_index_fails() {
    skim_cmd()
        .args(["search", "--stats"])
        .env(
            "SKIM_CACHE_DIR",
            "/tmp/skim_test_stats_no_index_definitely_missing",
        )
        .assert()
        .failure()
        .stderr(predicate::str::contains("No search index found"));
}

// ============================================================================
// --clear-cache succeeds unconditionally
// ============================================================================

#[test]
fn test_search_clear_cache_succeeds() {
    // Even if the cache directory doesn't exist, --clear-cache should succeed.
    skim_cmd()
        .args(["search", "--clear-cache"])
        .env("SKIM_CACHE_DIR", "/tmp/skim_test_clear_cache_missing")
        .assert()
        .success()
        .stderr(predicate::str::contains("cleared"));
}

// ============================================================================
// Build and query integration tests
// ============================================================================

#[test]
fn test_search_build_on_fixtures() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().to_str().unwrap().to_string();

    // Run `skim search --build` with the repo root set to the fixtures directory.
    // We control the cache dir via env var; the repo root is the CWD of the
    // process (the workspace root, which has .git).
    skim_cmd()
        .args(["search", "--build"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Indexed"));
}

#[test]
fn test_search_rebuild_recreates_index() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().to_str().unwrap().to_string();

    // First build.
    skim_cmd()
        .args(["search", "--build"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .assert()
        .success();

    // Rebuild should succeed without error.
    skim_cmd()
        .args(["search", "--rebuild"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Indexed"));
}

#[test]
fn test_search_query_returns_results() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().to_str().unwrap().to_string();

    // Build index first.
    skim_cmd()
        .args(["search", "--build"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .assert()
        .success();

    // Query for a term that exists in the Rust source (e.g., "SearchQuery" which
    // is defined in rskim-search/src/types/query.rs).
    let output = skim_cmd()
        .args(["search", "SearchQuery"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .output()
        .unwrap();

    // We expect either success (results found) or success with no output (no results).
    // Either way, the command must not crash.
    assert!(
        output.status.success(),
        "search query should exit successfully, got: {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_search_query_auto_builds_index() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().to_str().unwrap().to_string();

    // Do NOT explicitly build; the auto-build path should kick in.
    let output = skim_cmd()
        .args(["search", "LayerBuilder"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .output()
        .unwrap();

    // Auto-build should produce the "Building search index..." message on stderr.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Building search index") || stderr.contains("Indexed"),
        "expected auto-build message in stderr, got: {stderr}"
    );
    assert!(
        output.status.success(),
        "auto-build + search should succeed"
    );
}

#[test]
fn test_search_json_output() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().to_str().unwrap().to_string();

    // Build first.
    skim_cmd()
        .args(["search", "--build"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .assert()
        .success();

    // Query with --json; if there are results they must be valid JSON array.
    let output = skim_cmd()
        .args(["search", "--json", "SearchLayer"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .output()
        .unwrap();

    assert!(output.status.success(), "json search should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        // Must parse as a JSON array.
        let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("--json output must be valid JSON: {e}\ngot: {stdout}"));
        assert!(
            parsed.is_array(),
            "--json output must be a JSON array, got: {stdout}"
        );
    }
}

#[test]
fn test_search_stats_after_build() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().to_str().unwrap().to_string();

    skim_cmd()
        .args(["search", "--build"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .assert()
        .success();

    skim_cmd()
        .args(["search", "--stats"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Files indexed"))
        .stderr(predicate::str::contains("N-grams"));
}

#[test]
fn test_search_stats_json_after_build() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().to_str().unwrap().to_string();

    skim_cmd()
        .args(["search", "--build"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .assert()
        .success();

    let output = skim_cmd()
        .args(["search", "--stats", "--json"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("--stats --json must produce valid JSON: {e}\ngot: {stdout}"));
    assert!(parsed.get("file_count").is_some(), "stats JSON missing file_count");
    assert!(parsed.get("total_ngrams").is_some(), "stats JSON missing total_ngrams");
}

#[test]
fn test_search_limit_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().to_str().unwrap().to_string();

    skim_cmd()
        .args(["search", "--build"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .assert()
        .success();

    // --limit 1 with --json should return at most one result.
    let output = skim_cmd()
        .args(["search", "--json", "--limit", "1", "fn"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("limit output must be valid JSON: {e}\ngot: {stdout}"));
        let arr = parsed.as_array().expect("expected JSON array");
        assert!(
            arr.len() <= 1,
            "--limit 1 should return at most 1 result, got {}",
            arr.len()
        );
    }
}

#[test]
fn test_search_clear_cache_removes_index() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().to_str().unwrap().to_string();

    // Build then clear.
    skim_cmd()
        .args(["search", "--build"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .assert()
        .success();

    skim_cmd()
        .args(["search", "--clear-cache"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .assert()
        .success();

    // After clearing, --stats should fail with "No search index".
    skim_cmd()
        .args(["search", "--stats"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains("No search index found"));
}

// ============================================================================
// Global registration tests (unchanged)
// ============================================================================

#[test]
fn test_search_in_main_help() {
    skim_cmd()
        .args(["--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("search"));
}

#[test]
fn test_search_completions_registered() {
    skim_cmd()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("search"));
}
