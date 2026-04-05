//! Adversarial CLI tests for `skim search`.
//!
//! Exercises hostile inputs, corrupted state, and boundary conditions through
//! the CLI binary.  Uses `assert_cmd` + `predicates` following the same
//! conventions as `cli_search.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use assert_cmd::Command;
use predicates::prelude::*;

fn skim_cmd() -> Command {
    Command::cargo_bin("skim").unwrap()
}

// ============================================================================
// 1. Long query does not crash the binary
// ============================================================================

#[test]
fn long_query_doesnt_crash() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().to_str().unwrap().to_string();

    // Build first so there is an index to query.
    skim_cmd()
        .args(["search", "--build"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .assert()
        .success();

    // 5 000-character query — must not crash (exit code may be 0 or 1,
    // but never a signal / panic).
    let long_query = "UserService".repeat(455); // ~5005 chars
    let output = skim_cmd()
        .args(["search", &long_query])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .output()
        .unwrap();

    // Any graceful exit is acceptable — we only rule out a crash (signal).
    assert!(
        output.status.code().is_some(),
        "process must exit cleanly with an exit code, not a signal"
    );
}

// ============================================================================
// 2. Special characters in query do not crash the binary
// ============================================================================

#[test]
fn special_chars_dont_crash() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().to_str().unwrap().to_string();

    skim_cmd()
        .args(["search", "--build"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .assert()
        .success();

    // Note: null bytes (\u{0000}) cannot be passed as CLI arguments on any OS —
    // the kernel rejects them before the process receives them.  We therefore
    // only test printable special characters here.
    for query in &[
        "!@#$%^&*()",
        "{}[]<>?",
        "'; DROP TABLE users;--",
    ] {
        let output = skim_cmd()
            .args(["search", query])
            .env("SKIM_CACHE_DIR", &cache_dir)
            .output()
            .unwrap();

        assert!(
            output.status.code().is_some(),
            "query {:?} must not crash the binary (got a signal instead of exit code)",
            query
        );
    }
}

// ============================================================================
// 3. Corrupt index shows a helpful error mentioning --rebuild
// ============================================================================

#[test]
fn corrupt_index_shows_helpful_error() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().to_str().unwrap().to_string();

    // Build a valid index.  The CLI stores the index at
    // {SKIM_CACHE_DIR}/search/{hash_of_repo_root}/, so we must locate the
    // actual `lexical.skidx` by walking the directory tree.
    skim_cmd()
        .args(["search", "--build"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .assert()
        .success();

    // Find the lexical.skidx written inside the hash-keyed subdirectory.
    let search_root = tmp.path().join("search");
    let idx_path = std::fs::read_dir(&search_root)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path().join("lexical.skidx"))
        .find(|p| p.exists())
        .expect("lexical.skidx should exist after --build");

    // Overwrite with garbage so the format reader rejects it.
    std::fs::write(&idx_path, b"THIS IS NOT A VALID SKIM INDEX").unwrap();

    // Querying should fail with a message that mentions --rebuild or corruption.
    skim_cmd()
        .args(["search", "UserService"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("--rebuild")
                .or(predicate::str::contains("corrupted"))
                .or(predicate::str::contains("corrupt"))
                .or(predicate::str::contains("invalid")),
        );
}

// ============================================================================
// 4. --stats --json produces valid JSON
// ============================================================================

#[test]
fn stats_json_is_valid_json() {
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

    assert!(
        output.status.success(),
        "search --stats --json should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.trim().is_empty(),
        "--stats --json must produce output on stdout"
    );

    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("--stats --json output must be valid JSON: {e}\ngot: {stdout}"));
    // Sanity-check expected keys.
    assert!(
        parsed.get("file_count").is_some(),
        "stats JSON must contain file_count, got: {parsed}"
    );
    assert!(
        parsed.get("total_ngrams").is_some(),
        "stats JSON must contain total_ngrams, got: {parsed}"
    );
}

// ============================================================================
// 5. --build then query succeeds (exit code 0)
// ============================================================================

#[test]
fn build_then_query_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().to_str().unwrap().to_string();

    // Build phase.
    skim_cmd()
        .args(["search", "--build"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .assert()
        .success();

    // Query phase — must exit 0 regardless of whether results exist.
    skim_cmd()
        .args(["search", "SearchLayer"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .assert()
        .success();
}

// ============================================================================
// 6. --limit 0 returns empty output (exit 0)
// ============================================================================

#[test]
fn limit_zero_returns_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().to_str().unwrap().to_string();

    skim_cmd()
        .args(["search", "--build"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .assert()
        .success();

    let output = skim_cmd()
        .args(["search", "--json", "--limit", "0", "fn"])
        .env("SKIM_CACHE_DIR", &cache_dir)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "--limit 0 must exit 0, got: {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("limit 0 JSON must be valid: {e}\ngot: {stdout}"));
        let arr = parsed.as_array().expect("--json must return a JSON array");
        assert!(
            arr.is_empty(),
            "--limit 0 must return an empty array, got {} items",
            arr.len()
        );
    }
}

// ============================================================================
// 7. help contains all documented flags
// ============================================================================

#[test]
fn help_contains_all_documented_flags() {
    let output = skim_cmd()
        .args(["search", "--help"])
        .output()
        .unwrap();

    assert!(output.status.success(), "search --help must exit 0");

    let stdout = String::from_utf8_lossy(&output.stdout);

    for flag in &["--json", "--stats", "--clear-cache"] {
        assert!(
            stdout.contains(flag),
            "search --help must document {flag}, output:\n{stdout}"
        );
    }
}
