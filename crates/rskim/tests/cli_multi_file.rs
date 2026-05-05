//! Integration tests for multiple file arguments.
//!
//! Covers `skim file1.ts file2.ts file3.ts` — the ability to pass multiple
//! positional arguments just like `cat file1 file2 file3`.  This is separate
//! from glob patterns (tested in `cli_glob.rs`) even though the underlying
//! dispatch overlaps.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

// ============================================================================
// Happy path — multiple plain file arguments
// ============================================================================

#[test]
fn test_multi_file_two_args_both_processed() {
    let temp = TempDir::new().unwrap();

    fs::write(
        temp.path().join("alpha.ts"),
        "function alpha() { return 1; }",
    )
    .unwrap();
    fs::write(
        temp.path().join("beta.ts"),
        "function beta() { return 2; }",
    )
    .unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(temp.path().join("alpha.ts"))
        .arg(temp.path().join("beta.ts"))
        .assert()
        .success()
        .stdout(predicate::str::contains("function alpha"))
        .stdout(predicate::str::contains("function beta"));
}

#[test]
fn test_multi_file_three_args() {
    let temp = TempDir::new().unwrap();

    fs::write(
        temp.path().join("one.ts"),
        "function one() { return 1; }",
    )
    .unwrap();
    fs::write(
        temp.path().join("two.ts"),
        "function two() { return 2; }",
    )
    .unwrap();
    fs::write(
        temp.path().join("three.ts"),
        "function three() { return 3; }",
    )
    .unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(temp.path().join("one.ts"))
        .arg(temp.path().join("two.ts"))
        .arg(temp.path().join("three.ts"))
        .assert()
        .success()
        .stdout(predicate::str::contains("function one"))
        .stdout(predicate::str::contains("function two"))
        .stdout(predicate::str::contains("function three"));
}

#[test]
fn test_multi_file_mixed_languages() {
    let temp = TempDir::new().unwrap();

    fs::write(
        temp.path().join("main.rs"),
        "fn main() { println!(\"hello\"); }",
    )
    .unwrap();
    fs::write(
        temp.path().join("script.py"),
        "def run():\n    pass\n",
    )
    .unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(temp.path().join("main.rs"))
        .arg(temp.path().join("script.py"))
        .assert()
        .success()
        .stdout(predicate::str::contains("fn main"))
        .stdout(predicate::str::contains("def run"));
}

// ============================================================================
// Headers in multi-file output
// ============================================================================

#[test]
fn test_multi_file_shows_headers() {
    let temp = TempDir::new().unwrap();

    fs::write(temp.path().join("a.ts"), "function a() {}").unwrap();
    fs::write(temp.path().join("b.ts"), "function b() {}").unwrap();

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg(temp.path().join("a.ts"))
        .arg(temp.path().join("b.ts"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8_lossy(&output);
    // Each file should have its path in a header comment
    assert!(
        stdout.contains("a.ts"),
        "Expected header for a.ts, got:\n{stdout}"
    );
    assert!(
        stdout.contains("b.ts"),
        "Expected header for b.ts, got:\n{stdout}"
    );
}

#[test]
fn test_multi_file_no_header_flag() {
    let temp = TempDir::new().unwrap();

    fs::write(temp.path().join("a.ts"), "function a() {}").unwrap();
    fs::write(temp.path().join("b.ts"), "function b() {}").unwrap();

    // Headers use the format "// {path}" — assert none appear with --no-header
    Command::cargo_bin("skim")
        .unwrap()
        .arg(temp.path().join("a.ts"))
        .arg(temp.path().join("b.ts"))
        .arg("--no-header")
        .assert()
        .success()
        .stdout(predicate::str::contains("// ").not());
}

// ============================================================================
// Mode flag propagates to all files
// ============================================================================

#[test]
fn test_multi_file_signatures_mode_applied_to_all() {
    let temp = TempDir::new().unwrap();

    // Signatures mode strips bodies — `return 1` must not appear
    fs::write(
        temp.path().join("a.ts"),
        "function a(): number { return 1; }",
    )
    .unwrap();
    fs::write(
        temp.path().join("b.ts"),
        "function b(): number { return 2; }",
    )
    .unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(temp.path().join("a.ts"))
        .arg(temp.path().join("b.ts"))
        .arg("--mode=signatures")
        .assert()
        .success()
        .stdout(predicate::str::contains("function a"))
        .stdout(predicate::str::contains("function b"))
        .stdout(predicate::str::contains("return").not());
}

// ============================================================================
// Error cases
// ============================================================================

#[test]
fn test_multi_file_nonexistent_file_warns_but_succeeds() {
    // When some files exist and some don't, skim processes the valid ones and
    // warns about the missing ones on stderr (like `cat` behaviour).
    let temp = TempDir::new().unwrap();

    fs::write(temp.path().join("real.ts"), "function real() {}").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(temp.path().join("real.ts"))
        .arg(temp.path().join("ghost.ts")) // does not exist
        .assert()
        .success()
        .stdout(predicate::str::contains("function real"))
        .stderr(predicate::str::contains("ghost.ts").or(predicate::str::contains("not found")));
}

#[test]
fn test_multi_file_all_nonexistent_fails() {
    // When ALL specified files are missing, the command fails.
    let temp = TempDir::new().unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(temp.path().join("ghost1.ts"))
        .arg(temp.path().join("ghost2.ts"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("ghost1.ts").or(predicate::str::contains("not found")));
}

#[test]
fn test_multi_file_stdin_mixed_fails() {
    // Stdin ('-') cannot be combined with file arguments.
    let temp = TempDir::new().unwrap();
    fs::write(temp.path().join("file.ts"), "function f() {}").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg(temp.path().join("file.ts"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("stdin").or(predicate::str::contains("cannot")));
}

// ============================================================================
// Mixing plain files and glob patterns in multiple args
// ============================================================================

#[test]
fn test_multi_file_mix_plain_and_glob() {
    let temp = TempDir::new().unwrap();

    fs::write(
        temp.path().join("explicit.ts"),
        "function explicit() {}",
    )
    .unwrap();
    fs::write(
        temp.path().join("glob1.ts"),
        "function glob1() {}",
    )
    .unwrap();
    fs::write(
        temp.path().join("glob2.ts"),
        "function glob2() {}",
    )
    .unwrap();

    // Pass explicit.ts as a plain arg and use a glob to match glob*.ts
    let glob_pattern = format!("{}/*.ts", temp.path().display());

    Command::cargo_bin("skim")
        .unwrap()
        .arg(temp.path().join("explicit.ts"))
        .arg(&glob_pattern)
        .assert()
        .success()
        .stdout(predicate::str::contains("function explicit"))
        .stdout(predicate::str::contains("function glob1").or(predicate::str::contains("function glob2")));
}
