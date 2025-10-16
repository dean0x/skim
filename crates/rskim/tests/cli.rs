//! CLI integration tests using assert_cmd
//!
//! Tests the full CLI binary with real command-line arguments.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

// ============================================================================
// Basic CLI Tests
// ============================================================================

#[test]
fn test_cli_version() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("0.3.0"));
}

#[test]
fn test_cli_help() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim"))
        .stdout(predicate::str::contains("--mode"))
        .stdout(predicate::str::contains("--language"));
}

// ============================================================================
// File Processing Tests
// ============================================================================

#[test]
fn test_cli_structure_mode() {
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
        .arg("--mode")
        .arg("structure")
        .assert()
        .success()
        .stdout(predicate::str::contains("function add"))
        .stdout(predicate::str::contains("{ /* ... */ }"))
        .stdout(predicate::str::contains("return a + b").not());
}

#[test]
fn test_cli_signatures_mode() {
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
        .arg("--mode")
        .arg("signatures")
        .assert()
        .success()
        .stdout(predicate::str::contains("function add(a: number, b: number): number"))
        .stdout(predicate::str::contains("return").not());
}

#[test]
fn test_cli_types_mode() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(
        &file_path,
        "type UserId = string;\nfunction foo() { return 42; }",
    )
    .unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--mode")
        .arg("types")
        .assert()
        .success()
        .stdout(predicate::str::contains("type UserId"))
        .stdout(predicate::str::contains("function foo").not());
}

#[test]
fn test_cli_full_mode() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    let content = "function add(a: number, b: number): number { return a + b; }";
    fs::write(&file_path, content).unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--mode")
        .arg("full")
        .assert()
        .success()
        .stdout(predicate::str::contains(content));
}

// ============================================================================
// Language Detection Tests
// ============================================================================

#[test]
fn test_cli_auto_detect_typescript() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function test() { }").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success();
}

#[test]
fn test_cli_auto_detect_python() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.py");
    fs::write(&file_path, "def test(): pass").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success();
}

#[test]
fn test_cli_auto_detect_rust() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.rs");
    fs::write(&file_path, "fn test() {}").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success();
}

// ============================================================================
// Stdin Tests
// ============================================================================

#[test]
fn test_cli_stdin_with_language() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg("--language")
        .arg("typescript")
        .write_stdin("function test() { return 42; }")
        .assert()
        .success()
        .stdout(predicate::str::contains("function test"));
}

#[test]
fn test_cli_stdin_without_language_fails() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .write_stdin("function test() {}")
        .assert()
        .failure()
        .stderr(predicate::str::contains("requires --language flag"));
}

// ============================================================================
// Error Handling Tests
// ============================================================================

#[test]
fn test_cli_nonexistent_file() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("nonexistent.ts")
        .assert()
        .failure()
        .stderr(predicate::str::contains("No such file"));
}

#[test]
fn test_cli_unsupported_extension() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.xyz");
    fs::write(&file_path, "some code").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("Unsupported language"));
}

#[test]
fn test_cli_invalid_mode() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function test() {}").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--mode")
        .arg("invalid")
        .assert()
        .failure();
}

// ============================================================================
// Multi-Language Tests
// ============================================================================

#[test]
fn test_cli_all_languages_structure() {
    let temp_dir = TempDir::new().unwrap();

    // TypeScript
    let ts_file = temp_dir.path().join("test.ts");
    fs::write(&ts_file, "function test() { return 42; }").unwrap();
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&ts_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("{ /* ... */ }"));

    // Python
    let py_file = temp_dir.path().join("test.py");
    fs::write(&py_file, "def test():\n    return 42").unwrap();
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&py_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("{ /* ... */ }"));

    // Rust
    let rs_file = temp_dir.path().join("test.rs");
    fs::write(&rs_file, "fn test() { 42 }").unwrap();
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&rs_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("{ /* ... */ }"));

    // Go
    let go_file = temp_dir.path().join("test.go");
    fs::write(&go_file, "func test() int { return 42 }").unwrap();
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&go_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("{ /* ... */ }"));

    // Java
    let java_file = temp_dir.path().join("Test.java");
    fs::write(&java_file, "class Test { int test() { return 42; } }").unwrap();
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&java_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("{ /* ... */ }"));
}

// ============================================================================
// Edge Case Tests
// ============================================================================

#[test]
fn test_cli_empty_file() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("empty.ts");
    fs::write(&file_path, "").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn test_cli_unicode_content() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function greet() { return \"你好 🎉\"; }").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("function greet"));
}

#[test]
fn test_cli_malformed_syntax() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("broken.ts");
    fs::write(&file_path, "function broken(() { { { {").unwrap();

    // tree-sitter is error-tolerant, should not crash
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success();
}

// ============================================================================
// Language Flag Tests
// ============================================================================

#[test]
fn test_cli_explicit_language_override() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "function test() { return 42; }").unwrap();

    // Force TypeScript parsing despite .txt extension
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--language")
        .arg("typescript")
        .assert()
        .success()
        .stdout(predicate::str::contains("function test"));
}
