//! CLI tests for --max-lines flag
//!
//! Tests the --max-lines flag through the skim binary.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

/// Get a command for the skim binary
fn skim_cmd() -> Command {
    Command::cargo_bin("skim").unwrap()
}

#[test]
fn test_max_lines_flag_basic() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    std::fs::write(
        &file,
        "import { foo } from 'bar';\n\
         type UserId = string;\n\
         function hello(name: string): string { return `Hi ${name}`; }\n\
         function world(): void { console.log('world'); }\n\
         const x = 1;\n",
    )
    .unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--max-lines")
        .arg("3")
        .arg("--no-cache")
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let line_count = stdout.lines().count();
    assert!(
        line_count <= 3,
        "Output should have at most 3 lines, got {}: {:?}",
        line_count,
        stdout,
    );
}

#[test]
fn test_max_lines_zero_rejected() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    std::fs::write(&file, "function foo() {}").unwrap();

    skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--max-lines")
        .arg("0")
        .assert()
        .failure()
        .stderr(predicate::str::contains("--max-lines must be at least 1"));
}

#[test]
fn test_max_lines_with_stdin() {
    skim_cmd()
        .arg("-")
        .arg("-l")
        .arg("typescript")
        .arg("--max-lines")
        .arg("2")
        .write_stdin(
            "type A = string;\n\
             type B = number;\n\
             function foo(): void { return; }\n\
             function bar(): void { return; }\n",
        )
        .assert()
        .success()
        .stdout(predicate::function(|s: &str| s.lines().count() <= 2));
}

#[test]
fn test_max_lines_no_truncation_for_small_files() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("small.ts");
    std::fs::write(
        &file,
        "function add(a: number, b: number) { return a + b; }\n",
    )
    .unwrap();

    // File output has fewer lines than max_lines, so no truncation
    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--max-lines")
        .arg("100")
        .arg("--no-cache")
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    // Should not contain truncation markers
    assert!(
        !stdout.contains("(truncated)"),
        "Small file should not be truncated: {:?}",
        stdout,
    );
}

#[test]
fn test_max_lines_composable_with_mode() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    std::fs::write(
        &file,
        "type A = string;\n\
         type B = number;\n\
         type C = boolean;\n\
         function foo(): void { return; }\n\
         function bar(): void { return; }\n\
         function baz(): void { return; }\n",
    )
    .unwrap();

    // Test with --mode=signatures --max-lines 3
    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--mode=signatures")
        .arg("--max-lines")
        .arg("3")
        .arg("--no-cache")
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let line_count = stdout.lines().count();
    assert!(
        line_count <= 3,
        "Signatures + max_lines=3 should produce at most 3 lines, got {}: {:?}",
        line_count,
        stdout,
    );
}

#[test]
fn test_max_lines_glob_per_file() {
    let dir = TempDir::new().unwrap();

    // Create two files
    let file1 = dir.path().join("file1.ts");
    std::fs::write(
        &file1,
        "type A = string;\ntype B = number;\nfunction foo(): void {}\nfunction bar(): void {}\n",
    )
    .unwrap();

    let file2 = dir.path().join("file2.ts");
    std::fs::write(
        &file2,
        "type C = boolean;\ntype D = string;\nfunction baz(): void {}\nfunction qux(): void {}\n",
    )
    .unwrap();

    // Use relative glob by setting current_dir to the temp directory
    let output = skim_cmd()
        .arg("*.ts")
        .arg("--max-lines")
        .arg("3")
        .arg("--no-cache")
        .current_dir(dir.path())
        .output()
        .unwrap();

    let stderr = String::from_utf8(output.stderr.clone()).unwrap_or_default();

    // Should succeed
    assert!(
        output.status.success(),
        "Glob with max-lines should succeed. stderr: {:?}",
        stderr,
    );
}

#[test]
fn test_max_lines_without_flag_returns_full_output() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    let source = "import { foo } from 'bar';\n\
         type UserId = string;\n\
         function hello(name: string): string { return `Hi ${name}`; }\n\
         function world(): void { console.log('world'); }\n";
    std::fs::write(&file, source).unwrap();

    // Without --max-lines
    let output_full = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--no-cache")
        .output()
        .unwrap();

    let stdout_full = String::from_utf8(output_full.stdout).unwrap();

    // Should not contain truncation markers
    assert!(
        !stdout_full.contains("(truncated)"),
        "Without --max-lines, output should not be truncated: {:?}",
        stdout_full,
    );
}

#[test]
fn test_max_lines_show_stats_interaction() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    std::fs::write(
        &file,
        "type A = string;\nfunction foo(): void { return; }\nfunction bar(): void { return; }\n",
    )
    .unwrap();

    // --max-lines with --show-stats should both work
    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--max-lines")
        .arg("2")
        .arg("--show-stats")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "max-lines + show-stats should succeed"
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    let line_count = stdout.lines().count();
    assert!(
        line_count <= 2,
        "Output should have at most 2 lines, got {}: {:?}",
        line_count,
        stdout,
    );
}
