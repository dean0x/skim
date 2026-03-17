//! Integration tests for stdin and single-file code paths after
//! the `process_stdin()` / `write_result_and_stats()` refactor.
//!
//! These cover multi-flag combinations that weren't exercised by the
//! existing test suite.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

// ============================================================================
// Stdin combination tests (exercises process_stdin)
// ============================================================================

#[test]
fn test_stdin_mode_and_stats_combined() {
    let input = "fn add(a: i32, b: i32) -> i32 { a + b }\nfn sub(x: i32, y: i32) -> i32 { x - y }";

    Command::cargo_bin("skim")
        .unwrap()
        .args(["-", "--lang=rust", "--mode=signatures", "--show-stats"])
        .write_stdin(input)
        .assert()
        .success()
        .stdout(predicate::str::contains("fn add(a: i32, b: i32) -> i32"))
        .stdout(predicate::str::contains("fn sub(x: i32, y: i32) -> i32"))
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("tokens"));
}

#[test]
fn test_stdin_tokens_and_stats_combined() {
    let input = "fn add(a: i32, b: i32) -> i32 { a + b }";

    Command::cargo_bin("skim")
        .unwrap()
        .args(["-", "--lang=rust", "--tokens=50", "--show-stats"])
        .write_stdin(input)
        .assert()
        .success()
        .stdout(predicate::str::contains("fn add"))
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("tokens"));
}

#[test]
fn test_stdin_filename_tokens_mode() {
    let input = "def greet(name):\n    return f'Hello {name}'";

    Command::cargo_bin("skim")
        .unwrap()
        .args([
            "-",
            "--filename=app.py",
            "--tokens=100",
            "--mode=signatures",
        ])
        .write_stdin(input)
        .assert()
        .success()
        .stdout(predicate::str::contains("def greet"));
}

#[test]
fn test_stdin_empty_input() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["-", "--lang=typescript"])
        .write_stdin("")
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn test_stdin_incomplete_code() {
    // tree-sitter should handle incomplete code gracefully
    Command::cargo_bin("skim")
        .unwrap()
        .args(["-", "--lang=typescript"])
        .write_stdin("function incomplete() {")
        .assert()
        .success()
        .stdout(predicate::str::contains("function incomplete"));
}

#[test]
fn test_stdin_binary_input_fails() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["-", "--lang=typescript"])
        .write_stdin(b"\x80\x81\x82\x00\xff" as &[u8])
        .assert()
        .failure()
        .stderr(predicate::str::contains("UTF-8").or(predicate::str::contains("utf-8")));
}

#[test]
fn test_stdin_max_lines_and_stats() {
    let input = "fn a() { 1 }\nfn b() { 2 }\nfn c() { 3 }";

    Command::cargo_bin("skim")
        .unwrap()
        .args(["-", "--lang=rust", "--max-lines=2", "--show-stats"])
        .write_stdin(input)
        .assert()
        .success()
        .stdout(predicate::str::contains("fn a"))
        .stdout(predicate::str::contains("truncated"))
        .stderr(predicate::str::contains("[skim]"));
}

// ============================================================================
// Single-file combination tests (exercises write_result_and_stats)
// ============================================================================

#[test]
fn test_single_file_tokens_and_stats() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("example.rs");
    fs::write(
        &file,
        "fn add(a: i32, b: i32) -> i32 { a + b }\nfn sub(a: i32, b: i32) -> i32 { a - b }",
    )
    .unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .args(["--tokens=100", "--show-stats"])
        .arg(&file)
        .assert()
        .success()
        .stdout(predicate::str::contains("fn add").or(predicate::str::contains("fn sub")))
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("tokens"));
}
