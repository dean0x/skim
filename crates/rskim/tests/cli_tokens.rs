//! CLI integration tests for token counting and --show-stats flag
//!
//! Tests token reduction statistics output

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

#[test]
fn test_show_stats_single_file() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(
        &file_path,
        "function test(a: number, b: number): number {\n    return a + b;\n}",
    )
    .unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--show-stats")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("tokens"))
        .stderr(predicate::str::contains("→"))
        .stderr(predicate::str::contains("%"));
}

#[test]
fn test_show_stats_multiple_files() {
    let temp_dir = TempDir::new().unwrap();

    fs::write(
        temp_dir.path().join("file1.ts"),
        "function a(x: number) { return x * 2; }",
    )
    .unwrap();
    fs::write(
        temp_dir.path().join("file2.ts"),
        "function b(y: string) { return y.toUpperCase(); }",
    )
    .unwrap();
    fs::write(
        temp_dir.path().join("file3.ts"),
        "function c(z: boolean) { return !z; }",
    )
    .unwrap();

    // Stats should show aggregated counts for multiple files
    Command::cargo_bin("skim")
        .unwrap()
        .arg("*.ts")
        .arg("--show-stats")
        .current_dir(temp_dir.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("file(s)"));
}

#[test]
fn test_show_stats_with_structure_mode() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(
        &file_path,
        "function longFunction() {\n    const x = 1;\n    const y = 2;\n    return x + y;\n}",
    )
    .unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--mode=structure")
        .arg("--show-stats")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("tokens"));
}

#[test]
fn test_show_stats_with_signatures_mode() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(
        &file_path,
        "function add(a: number, b: number): number { return a + b; }",
    )
    .unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--mode=signatures")
        .arg("--show-stats")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("tokens"));
}

#[test]
fn test_show_stats_with_full_mode() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function test() { return 42; }").unwrap();

    // Full mode should show 0% reduction (or 100% of original)
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--mode=full")
        .arg("--show-stats")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"));
}

#[test]
fn test_show_stats_with_stdin() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg("--language=typescript")
        .arg("--show-stats")
        .write_stdin("function test(x: number): number { return x * 2; }")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("tokens"));
}

#[test]
fn test_no_stats_by_default() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function test() { return 42; }").unwrap();

    // Without --show-stats, stderr should be empty (no stats output)
    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success()
        .get_output()
        .stderr
        .clone();

    let stderr_str = String::from_utf8_lossy(&output);
    assert!(
        !stderr_str.contains("[skim]"),
        "Stats should not appear without --show-stats flag"
    );
}

#[test]
fn test_show_stats_format_contains_reduction_percentage() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(
        &file_path,
        "function calculate(a: number, b: number): number {\n    \
         const sum = a + b;\n    \
         const product = a * b;\n    \
         return sum + product;\n\
         }",
    )
    .unwrap();

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--mode=structure")
        .arg("--show-stats")
        .assert()
        .success()
        .get_output()
        .stderr
        .clone();

    let stderr_str = String::from_utf8_lossy(&output);

    // Stats should include token counts and reduction percentage
    assert!(stderr_str.contains("tokens"), "Should show token count");
    assert!(stderr_str.contains("→"), "Should show arrow separator");
    assert!(stderr_str.contains("%"), "Should show percentage");
}

#[test]
fn test_show_stats_with_python() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.py");
    fs::write(
        &file_path,
        "def calculate_sum(a: int, b: int) -> int:\n    result = a + b\n    return result",
    )
    .unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--show-stats")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("tokens"));
}

#[test]
fn test_show_stats_with_rust() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.rs");
    fs::write(
        &file_path,
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}",
    )
    .unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--show-stats")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("tokens"));
}

#[test]
fn test_show_stats_with_glob_and_no_header() {
    let temp_dir = TempDir::new().unwrap();

    fs::write(temp_dir.path().join("a.ts"), "function a() {}").unwrap();
    fs::write(temp_dir.path().join("b.ts"), "function b() {}").unwrap();

    // Stats should still work with --no-header flag
    Command::cargo_bin("skim")
        .unwrap()
        .arg("*.ts")
        .arg("--no-header")
        .arg("--show-stats")
        .current_dir(temp_dir.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("file(s)"));
}

#[test]
fn test_show_stats_empty_file() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("empty.ts");
    fs::write(&file_path, "").unwrap();

    // Empty file should still work with --show-stats (likely 0 → 0 tokens)
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--show-stats")
        .assert()
        .success();
}
