//! CLI integration tests for caching functionality
//!
//! Tests cache creation, reuse, invalidation, --no-cache flag,
//! and SKIM_CACHE_DIR / SKIM_ANALYTICS_DB env var behavior (B1–B7, PF-002 fix).

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

// ============================================================================
// B1–B7: SKIM_CACHE_DIR relocation tests (Phase B — PF-002 fix)
// ============================================================================

/// Helper: build a basic skim invocation against a temp TypeScript file.
fn skim_with_ts_file(cache_dir: &std::path::Path) -> (Command, std::path::PathBuf) {
    let src_dir = TempDir::new().unwrap();
    let file_path = src_dir.path().join("test.ts");
    fs::write(
        &file_path,
        "function greet(name: string): string { return `Hello ${name}`; }",
    )
    .unwrap();

    // Leak the TempDir so the file outlives the command; callers that need it can
    // take ownership.  For path-only tests we just need the PathBuf.
    let file_path_owned = file_path.clone();
    std::mem::forget(src_dir); // keep file alive for test duration

    let mut cmd = Command::cargo_bin("skim").unwrap();
    cmd.env("SKIM_CACHE_DIR", cache_dir.as_os_str())
        .env("SKIM_DISABLE_ANALYTICS", "1"); // don't pollute real analytics DB
    (cmd, file_path_owned)
}

/// B2: SKIM_CACHE_DIR relocates parser-cache entries.
///
/// After running skim with SKIM_CACHE_DIR=<dir>, JSON cache files must appear
/// directly under <dir> (not under ~/.cache/skim).
#[test]
fn test_b2_skim_cache_dir_relocates_parser_cache() {
    let cache_dir = TempDir::new().unwrap();
    let (mut cmd, file_path) = skim_with_ts_file(cache_dir.path());
    cmd.arg(&file_path).assert().success();

    // At least one .json cache file should land under <cache_dir>
    let json_files: Vec<_> = fs::read_dir(cache_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .collect();

    assert!(
        !json_files.is_empty(),
        "B2: parser-cache .json files must land under SKIM_CACHE_DIR={}, found none. \
         Default cache dir may be in use instead.",
        cache_dir.path().display()
    );
}

/// B3: SKIM_CACHE_DIR relocates tee output directory.
///
/// This is a structural test — we verify get_cache_dir() respects SKIM_CACHE_DIR
/// by confirming that skim creates its cache structure under the override dir.
/// (Tee files only appear on command failure, but the tee directory creation
/// is gated through get_cache_dir, which now honors SKIM_CACHE_DIR.)
#[test]
fn test_b3_skim_cache_dir_relocates_tee_dir() {
    let cache_dir = TempDir::new().unwrap();
    let (mut cmd, file_path) = skim_with_ts_file(cache_dir.path());
    cmd.arg(&file_path).assert().success();

    // After a run, the cache root should exist under override dir
    // (even if the tee subdir isn't created until a failure occurs)
    assert!(
        cache_dir.path().exists(),
        "B3: SKIM_CACHE_DIR directory should exist after a skim run"
    );

    // The parser cache files demonstrate the root is the override dir,
    // which means tee (which calls get_cache_dir() + /tee) also points there.
    let entries: Vec<_> = fs::read_dir(cache_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        !entries.is_empty(),
        "B3: cache root must have entries under SKIM_CACHE_DIR (tee roots here)"
    );
}

/// B4: SKIM_ANALYTICS_DB wins over SKIM_CACHE_DIR for the analytics DB path.
///
/// When both are set, the DB is created at SKIM_ANALYTICS_DB, NOT at
/// <SKIM_CACHE_DIR>/analytics.db.
#[test]
fn test_b4_skim_analytics_db_wins_over_cache_dir() {
    let cache_dir = TempDir::new().unwrap();
    let analytics_dir = TempDir::new().unwrap();
    let explicit_db = analytics_dir.path().join("my-analytics.db");

    // Run skim stats (a subcommand that opens the analytics DB read-only)
    // with both vars set.  We use `skim stats` which opens the default DB.
    Command::cargo_bin("skim")
        .unwrap()
        .args(["stats"])
        .env("SKIM_CACHE_DIR", cache_dir.path().as_os_str())
        .env("SKIM_ANALYTICS_DB", explicit_db.to_str().unwrap())
        .assert()
        .success();

    // The DB must appear at the explicit path, NOT under <cache_dir>.
    assert!(
        explicit_db.exists(),
        "B4: SKIM_ANALYTICS_DB must take precedence — db should exist at {}, \
         not under SKIM_CACHE_DIR={}",
        explicit_db.display(),
        cache_dir.path().display()
    );

    let default_db = cache_dir.path().join("analytics.db");
    assert!(
        !default_db.exists(),
        "B4: <SKIM_CACHE_DIR>/analytics.db must NOT be created when SKIM_ANALYTICS_DB is set, \
         but found: {}",
        default_db.display()
    );
}

/// B5: Neither SKIM_CACHE_DIR nor SKIM_ANALYTICS_DB set => default path unchanged.
///
/// We can only verify the absence of regressions here (the default location is
/// the real ~/.cache/skim which we must not corrupt).  We run skim normally and
/// confirm it succeeds, trusting the unit tests in cache.rs for the path value.
#[test]
fn test_b5_default_cache_behavior_unchanged() {
    let src_dir = TempDir::new().unwrap();
    let file_path = src_dir.path().join("hello.ts");
    fs::write(&file_path, "const x: number = 1;").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .env_remove("SKIM_CACHE_DIR")
        .env_remove("SKIM_ANALYTICS_DB")
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success();
}

/// B7: Empty SKIM_CACHE_DIR is treated as unset (falls back to default).
///
/// Verified at unit level in cache.rs; this integration test confirms the binary
/// does not error out when SKIM_CACHE_DIR is set to an empty string.
#[test]
fn test_b7_empty_skim_cache_dir_does_not_error() {
    let src_dir = TempDir::new().unwrap();
    let file_path = src_dir.path().join("hello.ts");
    fs::write(&file_path, "const y: number = 2;").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .env("SKIM_CACHE_DIR", "")
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success();
}

#[test]
fn test_cache_basic_reuse() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function test() { return 42; }").unwrap();

    // First run - should create cache
    let output1 = Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // Second run - should use cache (output should be identical)
    let output2 = Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_eq!(output1, output2, "Cached output should match original");
}

#[test]
fn test_cache_invalidation_on_file_modification() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function original() { return 1; }").unwrap();

    // First run - creates cache
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("function original"));

    // Wait to ensure mtime changes (some filesystems have 1-second granularity)
    thread::sleep(Duration::from_secs(1));

    // Modify file
    fs::write(&file_path, "function modified() { return 2; }").unwrap();

    // Second run - should detect mtime change and invalidate cache
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("function modified"))
        .stdout(predicate::str::contains("function original").not());
}

#[test]
fn test_cache_different_modes() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function test() { return 42; }").unwrap();

    // Run with structure mode
    let structure_output = Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--mode=structure")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // Run with signatures mode (should produce different output, not use structure cache)
    let signatures_output = Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--mode=signatures")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_ne!(
        structure_output, signatures_output,
        "Different modes should produce different output"
    );
}

#[test]
fn test_no_cache_flag() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function test() { return 42; }").unwrap();

    // First run with --no-cache
    let output1 = Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--no-cache")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // Second run with --no-cache (should not use cache even if it exists)
    let output2 = Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--no-cache")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // Outputs should still match (deterministic transformation)
    assert_eq!(output1, output2);

    // Third run without --no-cache should still work
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success();
}

#[test]
fn test_clear_cache_command() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function test() { return 42; }").unwrap();

    // Create cache by running normally
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success();

    // Clear cache
    Command::cargo_bin("skim")
        .unwrap()
        .arg("--clear-cache")
        .assert()
        .success()
        .stdout(predicate::str::contains("Cache cleared successfully"));

    // Should still work after cache clear
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success();
}

#[test]
fn test_cache_with_glob_patterns() {
    let temp_dir = TempDir::new().unwrap();

    fs::write(temp_dir.path().join("file1.ts"), "function a() {}").unwrap();
    fs::write(temp_dir.path().join("file2.ts"), "function b() {}").unwrap();
    fs::write(temp_dir.path().join("file3.ts"), "function c() {}").unwrap();

    // First run with glob - creates cache for all files
    let output1 = Command::cargo_bin("skim")
        .unwrap()
        .arg("*.ts")
        .current_dir(temp_dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // Second run with glob - should use cache
    let output2 = Command::cargo_bin("skim")
        .unwrap()
        .arg("*.ts")
        .current_dir(temp_dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_eq!(output1, output2, "Cached glob output should match original");
}

#[test]
fn test_cache_stores_token_counts() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function test() { return 42; }").unwrap();

    // First run with --show-stats - should cache token counts
    let stderr1 = Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--show-stats")
        .assert()
        .success()
        .get_output()
        .stderr
        .clone();

    let stderr1_str = String::from_utf8_lossy(&stderr1);
    assert!(
        stderr1_str.contains("[skim]"),
        "First run should show token stats"
    );

    // Second run with --show-stats - should use cached token counts
    let stderr2 = Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--show-stats")
        .assert()
        .success()
        .get_output()
        .stderr
        .clone();

    let stderr2_str = String::from_utf8_lossy(&stderr2);
    assert!(
        stderr2_str.contains("[skim]"),
        "Second run should show cached token stats"
    );

    // Token counts should be identical
    assert_eq!(
        stderr1_str, stderr2_str,
        "Cached token counts should match original"
    );
}

#[test]
fn test_cache_with_explicit_language() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("noext");
    fs::write(&file_path, "function test() { return 42; }").unwrap();

    // First run with explicit language
    let output1 = Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--language=typescript")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // Second run - should use cache
    let output2 = Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--language=typescript")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_eq!(output1, output2);
}

#[test]
fn test_no_cache_with_show_stats() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function test() { return 42; }").unwrap();

    // Run with both --no-cache and --show-stats
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--no-cache")
        .arg("--show-stats")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"));
}

#[test]
fn test_cache_stats_computed_on_hit_when_missing() {
    // Scenario: First run without --show-stats caches (content, None, None).
    // Second run WITH --show-stats should still compute and display tokens.
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function test() { return 42; }").unwrap();

    // First run without --show-stats (caches without token counts)
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success();

    // Second run with --show-stats (should compute tokens from cache hit)
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--show-stats")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"));
}
