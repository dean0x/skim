//! CLI integration tests for caching functionality
//!
//! Tests cache creation, reuse, invalidation, and --no-cache flag

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

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
