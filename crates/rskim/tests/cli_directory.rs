//! CLI integration tests for directory processing
//!
//! Tests recursive directory processing with auto-detection

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

#[test]
fn test_directory_single_language() {
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
        .arg(temp_dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("function test1"))
        .stdout(predicate::str::contains("function test2"))
        .stdout(predicate::str::contains("function test3"));
}

#[test]
fn test_directory_mixed_languages() {
    let temp_dir = TempDir::new().unwrap();

    // Create files with different languages
    fs::write(temp_dir.path().join("test.ts"), "function tsFunc() {}").unwrap();
    fs::write(temp_dir.path().join("test.py"), "def py_func(): pass").unwrap();
    fs::write(temp_dir.path().join("test.rs"), "fn rust_func() {}").unwrap();

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg(temp_dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();

    // Verify all languages are processed
    assert!(stdout.contains("tsFunc"));
    assert!(stdout.contains("py_func"));
    assert!(stdout.contains("rust_func"));
}

#[test]
fn test_directory_recursive() {
    let temp_dir = TempDir::new().unwrap();

    // Create nested directory structure
    fs::create_dir_all(temp_dir.path().join("src/utils")).unwrap();
    fs::write(temp_dir.path().join("root.ts"), "function root() {}").unwrap();
    fs::write(temp_dir.path().join("src/main.ts"), "function main() {}").unwrap();
    fs::write(
        temp_dir.path().join("src/utils/helper.ts"),
        "function helper() {}",
    )
    .unwrap();

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg(temp_dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();

    // Verify all nested files are processed
    assert!(stdout.contains("root"));
    assert!(stdout.contains("main"));
    assert!(stdout.contains("helper"));
}

#[test]
fn test_directory_with_headers() {
    let temp_dir = TempDir::new().unwrap();

    fs::write(temp_dir.path().join("a.ts"), "function a() {}").unwrap();
    fs::write(temp_dir.path().join("b.ts"), "function b() {}").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(temp_dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("// === "))
        .stdout(predicate::str::contains("a.ts"))
        .stdout(predicate::str::contains("b.ts"));
}

#[test]
fn test_directory_no_header_flag() {
    let temp_dir = TempDir::new().unwrap();

    fs::write(temp_dir.path().join("a.ts"), "function a() {}").unwrap();
    fs::write(temp_dir.path().join("b.ts"), "function b() {}").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(temp_dir.path())
        .arg("--no-header")
        .assert()
        .success()
        .stdout(predicate::str::contains("// === ").not());
}

#[test]
fn test_directory_empty() {
    let temp_dir = TempDir::new().unwrap();

    // Empty directory - no supported files
    Command::cargo_bin("skim")
        .unwrap()
        .arg(temp_dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("No files found"));
}

#[test]
fn test_directory_only_unsupported_files() {
    let temp_dir = TempDir::new().unwrap();

    // Create files with unsupported extensions
    fs::write(temp_dir.path().join("file.txt"), "some text").unwrap();
    fs::write(temp_dir.path().join("file.md.bak"), "backup").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(temp_dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("No files found"));
}

#[test]
fn test_directory_with_modes() {
    let temp_dir = TempDir::new().unwrap();

    fs::write(
        temp_dir.path().join("test.ts"),
        "function test() { console.log('impl'); }",
    )
    .unwrap();

    // Test structure mode (default)
    Command::cargo_bin("skim")
        .unwrap()
        .arg(temp_dir.path())
        .arg("--mode=structure")
        .assert()
        .success()
        .stdout(predicate::str::contains("function test"))
        .stdout(predicate::str::contains("/* ... */"));

    // Test signatures mode
    Command::cargo_bin("skim")
        .unwrap()
        .arg(temp_dir.path())
        .arg("--mode=signatures")
        .assert()
        .success()
        .stdout(predicate::str::contains("function test"));
}

#[test]
fn test_directory_with_jobs_flag() {
    let temp_dir = TempDir::new().unwrap();

    fs::write(temp_dir.path().join("a.ts"), "function a() {}").unwrap();
    fs::write(temp_dir.path().join("b.ts"), "function b() {}").unwrap();
    fs::write(temp_dir.path().join("c.ts"), "function c() {}").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(temp_dir.path())
        .arg("--jobs")
        .arg("2")
        .assert()
        .success()
        .stdout(predicate::str::contains("function a"))
        .stdout(predicate::str::contains("function b"))
        .stdout(predicate::str::contains("function c"));
}

#[test]
fn test_directory_skips_symlinks() {
    let temp_dir = TempDir::new().unwrap();

    fs::write(temp_dir.path().join("real.ts"), "function real() {}").unwrap();

    // Create a symlink (skip on Windows if not supported)
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let _ = symlink(
            temp_dir.path().join("real.ts"),
            temp_dir.path().join("link.ts"),
        );

        // The ignore crate silently skips symlinks (follow_links=false).
        // Verify the real file is processed but the symlink is not duplicated.
        let output = Command::cargo_bin("skim")
            .unwrap()
            .arg(temp_dir.path())
            .arg("--no-header")
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();

        let stdout = String::from_utf8(output).unwrap();
        // The real file should be processed exactly once
        assert!(
            stdout.contains("function real"),
            "real file should be in output"
        );
        // Count occurrences of "function real" to ensure symlink was not followed
        let count = stdout.matches("function real").count();
        assert_eq!(
            count, 1,
            "expected exactly 1 occurrence (symlink should be skipped), got: {count}"
        );
    }
}

#[test]
fn test_directory_with_subdirectory() {
    let temp_dir = TempDir::new().unwrap();

    // Create subdirectory
    fs::create_dir_all(temp_dir.path().join("subdir")).unwrap();
    fs::write(temp_dir.path().join("subdir/file.ts"), "function sub() {}").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(temp_dir.path().join("subdir"))
        .assert()
        .success()
        .stdout(predicate::str::contains("function sub"));
}

#[test]
fn test_directory_language_override_ignored() {
    let temp_dir = TempDir::new().unwrap();

    // Create mixed language files
    fs::write(temp_dir.path().join("test.ts"), "function ts() {}").unwrap();
    fs::write(temp_dir.path().join("test.py"), "def py(): pass").unwrap();

    // Even with --language flag, each file should be auto-detected
    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg(temp_dir.path())
        .arg("--language=typescript")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();

    // Both should be processed correctly by their own language
    assert!(stdout.contains("ts"));
    assert!(stdout.contains("py"));
}

#[test]
fn test_directory_current_directory() {
    let temp_dir = TempDir::new().unwrap();

    fs::write(temp_dir.path().join("test.ts"), "function test() {}").unwrap();

    // Using "." should work
    Command::cargo_bin("skim")
        .unwrap()
        .arg(".")
        .current_dir(temp_dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("function test"));
}

// ========================================================================
// Gitignore support tests
// ========================================================================

/// Helper: create a minimal .git directory so the ignore crate recognises
/// the directory as a git repository and applies .gitignore rules.
fn init_fake_git_repo(dir: &std::path::Path) {
    fs::create_dir_all(dir.join(".git")).unwrap();
}

#[test]
fn test_directory_respects_gitignore() {
    let temp_dir = TempDir::new().unwrap();
    init_fake_git_repo(temp_dir.path());

    // Create .gitignore that ignores the "ignored_dir/" directory
    fs::write(temp_dir.path().join(".gitignore"), "ignored_dir/\n").unwrap();

    // Create a visible file and an ignored file
    fs::write(temp_dir.path().join("visible.ts"), "function visible() {}").unwrap();
    fs::create_dir_all(temp_dir.path().join("ignored_dir")).unwrap();
    fs::write(
        temp_dir.path().join("ignored_dir/secret.ts"),
        "function secret() {}",
    )
    .unwrap();

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg(temp_dir.path())
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
        !stdout.contains("function secret"),
        "gitignored file should NOT be in output"
    );
}

#[test]
fn test_directory_no_ignore_includes_gitignored() {
    let temp_dir = TempDir::new().unwrap();
    init_fake_git_repo(temp_dir.path());

    fs::write(temp_dir.path().join(".gitignore"), "ignored_dir/\n").unwrap();

    fs::write(temp_dir.path().join("visible.ts"), "function visible() {}").unwrap();
    fs::create_dir_all(temp_dir.path().join("ignored_dir")).unwrap();
    fs::write(
        temp_dir.path().join("ignored_dir/secret.ts"),
        "function secret() {}",
    )
    .unwrap();

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg(temp_dir.path())
        .arg("--no-header")
        .arg("--no-ignore")
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
        stdout.contains("function secret"),
        "with --no-ignore, gitignored file SHOULD be in output"
    );
}

#[test]
fn test_directory_skips_hidden_files_by_default() {
    let temp_dir = TempDir::new().unwrap();

    fs::write(temp_dir.path().join("visible.ts"), "function visible() {}").unwrap();
    fs::write(temp_dir.path().join(".hidden.ts"), "function hidden() {}").unwrap();

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg(temp_dir.path())
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
        !stdout.contains("function hidden"),
        "hidden file should NOT be in output by default"
    );
}

#[test]
fn test_directory_no_ignore_shows_hidden_files() {
    let temp_dir = TempDir::new().unwrap();

    fs::write(temp_dir.path().join("visible.ts"), "function visible() {}").unwrap();
    fs::write(temp_dir.path().join(".hidden.ts"), "function hidden() {}").unwrap();

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg(temp_dir.path())
        .arg("--no-header")
        .arg("--no-ignore")
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
        stdout.contains("function hidden"),
        "with --no-ignore, hidden file SHOULD be in output"
    );
}

#[test]
fn test_directory_gitignore_without_git_repo() {
    let temp_dir = TempDir::new().unwrap();
    // Deliberately NO .git/ directory — .gitignore should still be respected
    // because we configure require_git(false)

    fs::write(temp_dir.path().join(".gitignore"), "ignored.ts\n").unwrap();

    fs::write(temp_dir.path().join("visible.ts"), "function visible() {}").unwrap();
    fs::write(temp_dir.path().join("ignored.ts"), "function ignored() {}").unwrap();

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg(temp_dir.path())
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
        !stdout.contains("function ignored"),
        ".gitignore should be respected even without .git/ directory"
    );
}

#[test]
fn test_directory_skips_hidden_directories() {
    let temp_dir = TempDir::new().unwrap();

    fs::write(temp_dir.path().join("visible.ts"), "function visible() {}").unwrap();
    fs::create_dir_all(temp_dir.path().join(".hidden_dir")).unwrap();
    fs::write(
        temp_dir.path().join(".hidden_dir/file.ts"),
        "function in_hidden_dir() {}",
    )
    .unwrap();

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg(temp_dir.path())
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
        !stdout.contains("function in_hidden_dir"),
        "files in hidden directories should NOT be in output"
    );
}

#[test]
fn test_directory_no_ignore_hint_in_error() {
    let temp_dir = TempDir::new().unwrap();
    init_fake_git_repo(temp_dir.path());

    // Gitignore ignores all .ts files — so the directory has no processable files
    fs::write(temp_dir.path().join(".gitignore"), "*.ts\n").unwrap();
    fs::write(temp_dir.path().join("only.ts"), "function only() {}").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(temp_dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("No files found"))
        .stderr(predicate::str::contains("--no-ignore"));
}
