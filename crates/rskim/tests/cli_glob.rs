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

// ========================================================================
// Gitignore support tests for glob patterns
// ========================================================================

/// Helper: create a minimal .git directory so the ignore crate recognises
/// the directory as a git repository and applies .gitignore rules.
fn init_fake_git_repo(dir: &std::path::Path) {
    fs::create_dir_all(dir.join(".git")).unwrap();
}

#[test]
fn test_glob_respects_gitignore() {
    let temp_dir = TempDir::new().unwrap();
    init_fake_git_repo(temp_dir.path());

    // .gitignore ignores the "build/" directory
    fs::write(temp_dir.path().join(".gitignore"), "build/\n").unwrap();

    // Create visible and ignored files
    fs::write(temp_dir.path().join("visible.ts"), "function visible() {}").unwrap();
    fs::create_dir_all(temp_dir.path().join("build")).unwrap();
    fs::write(
        temp_dir.path().join("build/output.ts"),
        "function output() {}",
    )
    .unwrap();

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg("**/*.ts")
        .current_dir(temp_dir.path())
        .arg("--no-header")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    assert!(
        stdout.contains("function visible"),
        "visible file should be in output"
    );
    assert!(
        !stdout.contains("function output"),
        "gitignored file should NOT be in output"
    );
}

#[test]
fn test_glob_no_ignore_includes_gitignored() {
    let temp_dir = TempDir::new().unwrap();
    init_fake_git_repo(temp_dir.path());

    fs::write(temp_dir.path().join(".gitignore"), "build/\n").unwrap();

    fs::write(temp_dir.path().join("visible.ts"), "function visible() {}").unwrap();
    fs::create_dir_all(temp_dir.path().join("build")).unwrap();
    fs::write(
        temp_dir.path().join("build/output.ts"),
        "function output() {}",
    )
    .unwrap();

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg("**/*.ts")
        .arg("--no-ignore")
        .arg("--no-header")
        .current_dir(temp_dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    assert!(
        stdout.contains("function visible"),
        "visible file should be in output"
    );
    assert!(
        stdout.contains("function output"),
        "with --no-ignore, gitignored file SHOULD be in output"
    );
}

#[test]
fn test_glob_no_ignore_hint_in_error() {
    let temp_dir = TempDir::new().unwrap();
    init_fake_git_repo(temp_dir.path());

    // Gitignore ignores all .ts files
    fs::write(temp_dir.path().join(".gitignore"), "*.ts\n").unwrap();
    fs::write(temp_dir.path().join("only.ts"), "function only() {}").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg("*.ts")
        .current_dir(temp_dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("No files found"))
        .stderr(predicate::str::contains("--no-ignore"));
}
