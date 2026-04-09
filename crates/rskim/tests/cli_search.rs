//! Integration tests for `skim search` subcommand (#3).

use assert_cmd::Command;
use predicates::prelude::*;

fn skim_cmd() -> Command {
    Command::cargo_bin("skim").unwrap()
}

/// Recursively search `root` for a file named `name`.
fn dir_contains_file(root: &std::path::Path, name: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if dir_contains_file(&path, name) {
                return true;
            }
        } else if path.file_name().and_then(|n| n.to_str()) == Some(name) {
            return true;
        }
    }
    false
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
    let repo = init_temp_git_repo();
    let cache = tempfile::tempdir().unwrap();

    skim_cmd()
        .current_dir(repo.path())
        .args(["search", "--build"])
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .success();

    skim_cmd()
        .current_dir(repo.path())
        .args(["search", "--stats"])
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("Files indexed"))
        .stderr(predicate::str::contains("N-grams"));
}

#[test]
fn test_search_stats_json_after_build() {
    let repo = init_temp_git_repo();
    let cache = tempfile::tempdir().unwrap();

    skim_cmd()
        .current_dir(repo.path())
        .args(["search", "--build"])
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .success();

    let output = skim_cmd()
        .current_dir(repo.path())
        .args(["search", "--stats", "--json"])
        .env("SKIM_CACHE_DIR", cache.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("--stats --json must produce valid JSON: {e}\ngot: {stdout}"));

    // Wave 2: --build builds both lexical and temporal, so stats JSON is
    // the combined `{"lexical": {...}, "temporal": {...}}` object. When only
    // lexical is present, the JSON is a plain `IndexStats` object (backward
    // compatible with pre-Wave-2 callers).
    let lexical = parsed.get("lexical").unwrap_or(&parsed);
    assert!(
        lexical.get("file_count").is_some(),
        "stats JSON missing lexical.file_count: {parsed}"
    );
    assert!(
        lexical.get("total_ngrams").is_some(),
        "stats JSON missing lexical.total_ngrams: {parsed}"
    );
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

// ============================================================================
// Temporal flag integration tests (Wave 2 must-fix #5)
// ============================================================================

/// Create a temporary git repo with two committed files for temporal tests.
///
/// Two commits ensure git history is non-trivial so the temporal builder has
/// something to index.
fn init_temp_git_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let run_git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("git command");
    };
    run_git(&["init", "-q", "-b", "main"]);
    run_git(&["config", "user.name", "Test"]);
    run_git(&["config", "user.email", "test@example.com"]);
    std::fs::write(dir.join("a.rs"), "fn alpha() {}\n").unwrap();
    std::fs::write(dir.join("b.rs"), "fn beta() {}\n").unwrap();
    run_git(&["add", "a.rs", "b.rs"]);
    run_git(&["commit", "-q", "-m", "initial"]);
    std::fs::write(dir.join("a.rs"), "fn alpha() { 1 }\n").unwrap();
    std::fs::write(dir.join("b.rs"), "fn beta() { 1 }\n").unwrap();
    run_git(&["add", "a.rs", "b.rs"]);
    run_git(&["commit", "-q", "-m", "fix: update"]);
    tmp
}

#[test]
fn test_search_build_temporal_flag() {
    let repo = init_temp_git_repo();
    let cache = tempfile::tempdir().unwrap();
    skim_cmd()
        .current_dir(repo.path())
        .args(["search", "--build-temporal"])
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("Temporal index built"));
}

#[test]
fn test_search_hot_standalone() {
    let repo = init_temp_git_repo();
    let cache = tempfile::tempdir().unwrap();
    skim_cmd()
        .current_dir(repo.path())
        .args(["search", "--hot", "--limit", "5"])
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .success();
}

#[test]
fn test_search_blast_radius_with_text_rejected() {
    let cache = tempfile::tempdir().unwrap();
    skim_cmd()
        .args(["search", "hello", "--blast-radius", "foo.rs"])
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "cannot be combined with text search",
        ));
}

#[test]
fn test_search_blast_radius_with_hot_rejected() {
    let cache = tempfile::tempdir().unwrap();
    skim_cmd()
        .args(["search", "--blast-radius", "foo.rs", "--hot"])
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be combined with --hot"));
}

#[test]
fn test_search_hot_cold_conflict_warns() {
    let repo = init_temp_git_repo();
    let cache = tempfile::tempdir().unwrap();
    skim_cmd()
        .current_dir(repo.path())
        .args(["search", "--hot", "--cold"])
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("mutually exclusive"));
}

// ============================================================================
// Non-git directory regression tests (wave-2 critical fix)
// ============================================================================

/// `skim search --build` in a directory with no `.git` ancestor must exit 0,
/// build the lexical index, print a warning about temporal being skipped, and
/// NOT create a temporal.db.
#[test]
fn test_search_build_succeeds_in_non_git_dir() {
    // A bare tempdir has no .git — simulates tarball extract / scratch dir.
    let non_git_dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    let output = skim_cmd()
        .current_dir(non_git_dir.path())
        .args(["search", "--build"])
        .env("SKIM_CACHE_DIR", cache.path())
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "--build in non-git dir must exit 0; stderr: {stderr}"
    );
    assert!(
        stderr.contains("warning: skipping temporal index build: not a git repository"),
        "--build in non-git dir must warn about temporal skip; stderr: {stderr}"
    );

    // The temporal database must NOT have been created.
    // Recursively scan the cache dir looking for any temporal.db.
    assert!(
        !dir_contains_file(cache.path(), "temporal.db"),
        "temporal.db must not exist after --build in non-git dir"
    );
}

/// `skim search --build-temporal` in a directory with no `.git` ancestor must
/// hard-fail (exit 1) with an error mentioning git.
#[test]
fn test_search_build_temporal_hard_fails_in_non_git_dir() {
    let non_git_dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    let output = skim_cmd()
        .current_dir(non_git_dir.path())
        .args(["search", "--build-temporal"])
        .env("SKIM_CACHE_DIR", cache.path())
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "--build-temporal in non-git dir must exit non-zero; stderr: {stderr}"
    );
}

/// `skim search --build` inside a git repository must build both lexical and
/// temporal indexes successfully.
#[test]
fn test_search_build_in_git_dir_builds_temporal() {
    let repo = init_temp_git_repo();
    let cache = tempfile::tempdir().unwrap();

    skim_cmd()
        .current_dir(repo.path())
        .args(["search", "--build"])
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("Temporal index built"));

    // Confirm temporal.db was actually created somewhere under the cache.
    assert!(
        dir_contains_file(cache.path(), "temporal.db"),
        "temporal.db must exist after --build in a git repo"
    );
}
