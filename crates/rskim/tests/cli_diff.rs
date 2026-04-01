//! Integration tests for `skim git diff` AST-aware pipeline (#103)
//!
//! Uses temporary git repos to test the full end-to-end flow:
//! create repo -> add files -> commit -> modify -> run `skim git diff`.

use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;

/// Run a git command in the given directory, asserting it succeeds.
///
/// Panics with stderr output if the command exits with a non-zero status.
fn git(repo_path: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(repo_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Initialize a bare git repo with user config (no initial commit).
fn init_repo(dir: &TempDir) {
    let repo_path = dir.path();
    git(repo_path, &["init", "--initial-branch=main"]);
    git(repo_path, &["config", "user.email", "test@test.com"]);
    git(repo_path, &["config", "user.name", "Test"]);
}

/// Create a temporary git repo with an initial commit containing the given file.
fn setup_repo(filename: &str, initial_content: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    let repo_path = dir.path();

    init_repo(&dir);

    // Create subdirectories if needed
    let file_path = repo_path.join(filename);
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent).unwrap();
    }

    // Write initial file and commit
    fs::write(&file_path, initial_content).unwrap();
    git(repo_path, &["add", "."]);
    git(repo_path, &["commit", "-m", "initial commit"]);

    dir
}

/// Create a temporary git repo with an initial commit containing multiple files.
fn setup_repo_multi(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().unwrap();
    let repo_path = dir.path();

    init_repo(&dir);

    for (filename, content) in files {
        let file_path = repo_path.join(filename);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&file_path, content).unwrap();
    }

    git(repo_path, &["add", "."]);
    git(repo_path, &["commit", "-m", "initial commit"]);

    dir
}

/// Run `skim git diff` with additional args in the given directory.
fn run_skim_diff(dir: &TempDir, extra_args: &[&str]) -> assert_cmd::assert::Assert {
    let mut cmd = Command::cargo_bin("skim").unwrap();
    cmd.current_dir(dir.path());
    cmd.args(["git", "diff"]);
    cmd.args(extra_args);
    cmd.assert()
}

// ============================================================================
// Working tree diff tests
// ============================================================================

#[test]
fn test_diff_working_tree_typescript() {
    let initial = r#"import { Request } from 'express';

export function greet(name: string): string {
  return `Hello, ${name}!`;
}

export function farewell(name: string): string {
  return `Goodbye, ${name}!`;
}
"#;

    let modified = r#"import { Request } from 'express';

export function greet(name: string, title?: string): string {
  const prefix = title ? `${title} ` : '';
  return `Hello, ${prefix}${name}!`;
}

export function farewell(name: string): string {
  return `Goodbye, ${name}!`;
}
"#;

    let dir = setup_repo("src/greet.ts", initial);
    fs::write(dir.path().join("src/greet.ts"), modified).unwrap();

    let assert = run_skim_diff(&dir, &[]);
    let output = assert.success().get_output().stdout.clone();
    let stdout = String::from_utf8(output).unwrap();

    // Should reference the changed file
    assert!(
        stdout.contains("greet.ts"),
        "expected file path in output, got:\n{stdout}"
    );

    // AST-aware output should contain +/- markers for changed lines
    assert!(
        stdout.contains('+') || stdout.contains('-'),
        "expected +/- patch markers in AST-aware output, got:\n{stdout}"
    );

    // Should contain the changed function name (AST node detection)
    assert!(
        stdout.contains("greet"),
        "expected changed function 'greet' in output, got:\n{stdout}"
    );

    // Should show the signature change (title parameter added)
    assert!(
        stdout.contains("title"),
        "expected new parameter 'title' in diff output, got:\n{stdout}"
    );
}

#[test]
fn test_diff_no_changes() {
    let content = "function hello() { return 'hi'; }\n";
    let dir = setup_repo("src/hello.ts", content);

    // No modifications -> should print "No changes" to stderr
    let assert = run_skim_diff(&dir, &[]);
    assert
        .success()
        .stderr(predicates::str::contains("No changes"));
}

#[test]
fn test_diff_new_file_unstaged() {
    let initial = "function old() {}\n";
    let dir = setup_repo("old.ts", initial);

    // Create a new file but don't add it
    fs::write(dir.path().join("new.ts"), "function newFn() {}\n").unwrap();

    // Working tree diff doesn't show untracked files
    let assert = run_skim_diff(&dir, &[]);
    assert
        .success()
        .stderr(predicates::str::contains("No changes"));
}

// ============================================================================
// Staged diff tests
// ============================================================================

#[test]
fn test_diff_staged() {
    let initial = "export const VERSION = '1.0';\n";
    let modified = "export const VERSION = '2.0';\n";

    let dir = setup_repo("version.ts", initial);
    fs::write(dir.path().join("version.ts"), modified).unwrap();

    // Stage the change
    git(dir.path(), &["add", "version.ts"]);

    let assert = run_skim_diff(&dir, &["--cached"]);
    let output = assert.success().get_output().stdout.clone();
    let stdout = String::from_utf8(output).unwrap();

    assert!(
        stdout.contains("version.ts"),
        "expected file path in staged output, got:\n{stdout}"
    );
    // Should contain patch markers showing the version change
    assert!(
        stdout.contains("2.0") || stdout.contains("VERSION"),
        "expected version change in staged diff output, got:\n{stdout}"
    );
}

// ============================================================================
// Passthrough flag tests
// ============================================================================

#[test]
fn test_diff_stat_passthrough() {
    let initial = "const x = 1;\n";
    let modified = "const x = 2;\n";

    let dir = setup_repo("main.ts", initial);
    fs::write(dir.path().join("main.ts"), modified).unwrap();

    // --stat should pass through to git directly
    let assert = run_skim_diff(&dir, &["--stat"]);
    assert
        .success()
        .stdout(predicates::str::contains("main.ts"));
}

#[test]
fn test_diff_name_only_passthrough() {
    let initial = "const x = 1;\n";
    let modified = "const x = 2;\n";

    let dir = setup_repo("main.ts", initial);
    fs::write(dir.path().join("main.ts"), modified).unwrap();

    // --name-only should pass through to git directly
    let assert = run_skim_diff(&dir, &["--name-only"]);
    assert
        .success()
        .stdout(predicates::str::contains("main.ts"));
}

// ============================================================================
// Unsupported language fallback
// ============================================================================

#[test]
fn test_diff_unsupported_language_falls_back_to_raw() {
    let initial = "Hello world\n";
    let modified = "Hello modified world\n";

    let dir = setup_repo("readme.txt", initial);
    fs::write(dir.path().join("readme.txt"), modified).unwrap();

    // .txt is unsupported -> should fall back to raw diff hunks
    let assert = run_skim_diff(&dir, &[]);
    assert
        .success()
        .stdout(predicates::str::contains("readme.txt"));
}

// ============================================================================
// JSON output / serialization
// ============================================================================

#[test]
fn test_diff_rust_file() {
    let initial = r#"fn main() {
    println!("hello");
}

fn helper() -> i32 {
    42
}
"#;

    let modified = r#"fn main() {
    println!("hello world");
    eprintln!("debug");
}

fn helper() -> i32 {
    42
}
"#;

    let dir = setup_repo("src/main.rs", initial);
    fs::write(dir.path().join("src/main.rs"), modified).unwrap();

    let assert = run_skim_diff(&dir, &[]);
    assert
        .success()
        .stdout(predicates::str::contains("main.rs"))
        .stdout(predicates::str::contains("modified"));
}

// ============================================================================
// Multiple files changed
// ============================================================================

#[test]
fn test_diff_multiple_files() {
    let dir = setup_repo_multi(&[
        ("src/a.ts", "export function a() { return 1; }\n"),
        ("src/b.ts", "export function b() { return 2; }\n"),
    ]);

    // Modify both files
    fs::write(
        dir.path().join("src/a.ts"),
        "export function a() { return 10; }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("src/b.ts"),
        "export function b() { return 20; }\n",
    )
    .unwrap();

    let assert = run_skim_diff(&dir, &[]);
    assert
        .success()
        .stdout(predicates::str::contains("a.ts"))
        .stdout(predicates::str::contains("b.ts"));
}

// ============================================================================
// --mode structure (Gap 1)
// ============================================================================

#[test]
fn test_diff_mode_structure() {
    let initial = r#"import { Request } from 'express';

export function greet(name: string): string {
  return `Hello, ${name}!`;
}

export function farewell(name: string): string {
  return `Goodbye, ${name}!`;
}
"#;

    let modified = r#"import { Request } from 'express';

export function greet(name: string, title?: string): string {
  const prefix = title ? `${title} ` : '';
  return `Hello, ${prefix}${name}!`;
}

export function farewell(name: string): string {
  return `Goodbye, ${name}!`;
}
"#;

    let dir = setup_repo("src/greet.ts", initial);
    fs::write(dir.path().join("src/greet.ts"), modified).unwrap();

    // --mode structure should show changed nodes AND unchanged nodes as signatures
    let assert = run_skim_diff(&dir, &["--mode", "structure"]);
    let output = assert.success().get_output().stdout.clone();
    let stdout = String::from_utf8(output).unwrap();

    // Should contain the changed function
    assert!(
        stdout.contains("greet"),
        "expected 'greet' in output, got:\n{stdout}"
    );

    // Should contain some reference to farewell (as structure/signature context)
    assert!(
        stdout.contains("farewell"),
        "expected unchanged 'farewell' to appear in structure mode, got:\n{stdout}"
    );
}

// ============================================================================
// --mode full (Gap 1)
// ============================================================================

#[test]
fn test_diff_mode_full() {
    let initial = r#"export function greet(name: string): string {
  return `Hello, ${name}!`;
}

export function farewell(name: string): string {
  return `Goodbye, ${name}!`;
}
"#;

    let modified = r#"export function greet(name: string, title?: string): string {
  const prefix = title ? `${title} ` : '';
  return `Hello, ${prefix}${name}!`;
}

export function farewell(name: string): string {
  return `Goodbye, ${name}!`;
}
"#;

    let dir = setup_repo("src/greet.ts", initial);
    fs::write(dir.path().join("src/greet.ts"), modified).unwrap();

    // --mode full should show changed nodes AND unchanged nodes in full
    let assert = run_skim_diff(&dir, &["--mode", "full"]);
    let output = assert.success().get_output().stdout.clone();
    let stdout = String::from_utf8(output).unwrap();

    // Should contain the changed function
    assert!(
        stdout.contains("greet"),
        "expected 'greet' in output, got:\n{stdout}"
    );

    // Should contain the unchanged farewell function in full (including body)
    assert!(
        stdout.contains("farewell") && stdout.contains("Goodbye"),
        "expected unchanged 'farewell' with full body in full mode, got:\n{stdout}"
    );
}

// ============================================================================
// --json output (Gap 2)
// ============================================================================

#[test]
fn test_diff_json_output() {
    let initial = "const x = 1;\n";
    let modified = "const x = 2;\n";

    let dir = setup_repo("main.ts", initial);
    fs::write(dir.path().join("main.ts"), modified).unwrap();

    let assert = run_skim_diff(&dir, &["--json"]);
    let output = assert.success().get_output().stdout.clone();
    let stdout = String::from_utf8(output).unwrap();

    // Should be valid JSON
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("expected valid JSON output");

    // Should have files_changed field
    assert!(
        parsed.get("files_changed").is_some(),
        "expected 'files_changed' in JSON output"
    );

    // Should have files array
    assert!(
        parsed.get("files").is_some(),
        "expected 'files' in JSON output"
    );

    // File should be main.ts
    let files = parsed["files"].as_array().unwrap();
    assert!(!files.is_empty());
    assert_eq!(files[0]["path"], "main.ts");
}

// ============================================================================
// --show-stats (Gap 5)
// ============================================================================

#[test]
fn test_diff_show_stats() {
    let initial = "const x = 1;\n";
    let modified = "const x = 2;\n";

    let dir = setup_repo("main.ts", initial);
    fs::write(dir.path().join("main.ts"), modified).unwrap();

    let assert = run_skim_diff(&dir, &["--show-stats"]);
    assert.success().stderr(predicates::str::contains("tokens"));
}

// ============================================================================
// --name-status passthrough (Gap 5)
// ============================================================================

#[test]
fn test_diff_name_status_passthrough() {
    let initial = "const x = 1;\n";
    let modified = "const x = 2;\n";

    let dir = setup_repo("main.ts", initial);
    fs::write(dir.path().join("main.ts"), modified).unwrap();

    // --name-status should pass through to git directly
    let assert = run_skim_diff(&dir, &["--name-status"]);
    let output = assert.success().get_output().stdout.clone();
    let stdout = String::from_utf8(output).unwrap();

    // --name-status output starts with M/A/D followed by file path
    assert!(
        stdout.contains("main.ts"),
        "expected 'main.ts' in --name-status output"
    );
}

// ============================================================================
// --check passthrough (Gap 5)
// ============================================================================

#[test]
fn test_diff_check_passthrough() {
    let initial = "const x = 1;\n";
    let modified = "const x = 2;\n";

    let dir = setup_repo("main.ts", initial);
    fs::write(dir.path().join("main.ts"), modified).unwrap();

    // --check should pass through to git directly and succeed
    let assert = run_skim_diff(&dir, &["--check"]);
    assert.success();
}
