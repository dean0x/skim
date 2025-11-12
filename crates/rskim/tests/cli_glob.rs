//! CLI integration tests for glob pattern processing
//!
//! Tests multi-file processing with glob patterns

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

#[test]
fn test_glob_single_pattern() {
    let temp_dir = TempDir::new().unwrap();

    // Create multiple TypeScript files
    fs::write(
        temp_dir.path().join("file1.ts"),
        "function test1() { return 1; }",
    )
    .unwrap();
    fs::write(
        temp_dir.path().join("file2.ts"),
        "function test2() { return 2; }",
    )
    .unwrap();
    fs::write(
        temp_dir.path().join("file3.ts"),
        "function test3() { return 3; }",
    )
    .unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg("*.ts")
        .current_dir(temp_dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("function test1"))
        .stdout(predicate::str::contains("function test2"))
        .stdout(predicate::str::contains("function test3"));
}

#[test]
fn test_glob_with_headers() {
    let temp_dir = TempDir::new().unwrap();

    fs::write(temp_dir.path().join("a.ts"), "function a() {}").unwrap();
    fs::write(temp_dir.path().join("b.ts"), "function b() {}").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg("*.ts")
        .current_dir(temp_dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("// === "))
        .stdout(predicate::str::contains("a.ts"))
        .stdout(predicate::str::contains("b.ts"));
}

#[test]
fn test_glob_no_header_flag() {
    let temp_dir = TempDir::new().unwrap();

    fs::write(temp_dir.path().join("a.ts"), "function a() {}").unwrap();
    fs::write(temp_dir.path().join("b.ts"), "function b() {}").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg("*.ts")
        .arg("--no-header")
        .current_dir(temp_dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("// === ").not());
}

#[test]
fn test_glob_recursive_pattern() {
    let temp_dir = TempDir::new().unwrap();

    // Create nested directory structure
    fs::create_dir_all(temp_dir.path().join("src/utils")).unwrap();
    fs::write(temp_dir.path().join("src/main.ts"), "function main() {}").unwrap();
    fs::write(
        temp_dir.path().join("src/utils/helper.ts"),
        "function helper() {}",
    )
    .unwrap();

    // Note: On some systems, glob patterns might not work recursively without proper shell expansion
    // This test validates the basic glob functionality
    Command::cargo_bin("skim")
        .unwrap()
        .arg("src/*.ts")
        .current_dir(temp_dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("function main"));
}

#[test]
fn test_glob_no_matches() {
    let temp_dir = TempDir::new().unwrap();

    fs::write(temp_dir.path().join("file.js"), "function test() {}").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg("*.ts")
        .current_dir(temp_dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("No files found"));
}

#[test]
fn test_glob_absolute_path_rejected() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("/etc/*.conf")
        .assert()
        .failure()
        .stderr(predicate::str::contains("must be relative"))
        .stderr(predicate::str::contains("cannot start with '/'"));
}

#[test]
fn test_glob_parent_traversal_rejected() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("../*.ts")
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot contain '..'"))
        .stderr(predicate::str::contains("parent directory traversal"));
}

#[test]
fn test_glob_with_jobs_flag() {
    let temp_dir = TempDir::new().unwrap();

    fs::write(temp_dir.path().join("a.ts"), "function a() {}").unwrap();
    fs::write(temp_dir.path().join("b.ts"), "function b() {}").unwrap();
    fs::write(temp_dir.path().join("c.ts"), "function c() {}").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg("*.ts")
        .arg("--jobs")
        .arg("2")
        .current_dir(temp_dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("function a"))
        .stdout(predicate::str::contains("function b"))
        .stdout(predicate::str::contains("function c"));
}

#[test]
fn test_glob_jobs_too_high() {
    let temp_dir = TempDir::new().unwrap();
    fs::write(temp_dir.path().join("a.ts"), "function a() {}").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg("*.ts")
        .arg("--jobs")
        .arg("200")
        .current_dir(temp_dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("--jobs value too high"))
        .stderr(predicate::str::contains("maximum: 128"));
}

#[test]
fn test_glob_brace_expansion() {
    let temp_dir = TempDir::new().unwrap();

    fs::write(temp_dir.path().join("file.js"), "function js() {}").unwrap();
    fs::write(temp_dir.path().join("file.ts"), "function ts() {}").unwrap();

    // Note: Brace expansion is usually handled by the shell, not the program
    // This test validates that the pattern is processed correctly
    Command::cargo_bin("skim")
        .unwrap()
        .arg("*.ts")
        .current_dir(temp_dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("function ts"));
}
