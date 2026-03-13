//! CLI tests for --max-lines flag
//!
//! Tests the --max-lines flag through the skim binary.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::PathBuf;
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

#[test]
fn test_max_lines_python_class_priority_over_functions() {
    // Python class_definition should be priority 5 (type system),
    // so classes appear before standalone functions when budget is tight.
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.py");
    std::fs::write(
        &file,
        "import os\n\n\
         def create_user(name: str) -> None:\n    pass\n\n\
         class User:\n    def __init__(self, name: str):\n        self.name = name\n",
    )
    .unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--max-lines")
        .arg("5")
        .arg("--no-cache")
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("class"),
        "Python class should be kept over standalone function with tight budget: {:?}",
        stdout,
    );
}

// ============================================================================
// Non-contiguous span / marker budget tests (issues #24, #25)
//
// These tests exercise the full CLI pipeline (parse -> transform -> truncate)
// on real fixture files that produce non-contiguous selected spans when
// truncated. This validates that the marker budget accounting correctly
// reserves lines for omission markers between gaps.
// ============================================================================

/// Resolve path to a test fixture file relative to the workspace root
fn fixture_path(relative: &str) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crates/rskim -> workspace root
    path.pop();
    path.pop();
    path.join("tests/fixtures").join(relative)
}

#[test]
fn test_max_lines_noncontiguous_spans_fixture() {
    // mixed_priority.ts has types and interfaces interspersed with functions
    // and variables. Under a tight --max-lines budget, the truncation engine
    // selects high-priority spans (types, interfaces) and drops lower-priority
    // ones (functions, variables), producing non-contiguous gaps that require
    // omission markers. This test validates the fix from issues #24/#25:
    // the marker budget must be accounted for so output never exceeds max_lines.
    let fixture = fixture_path("typescript/mixed_priority.ts");
    assert!(fixture.exists(), "Fixture file should exist: {:?}", fixture);

    for budget in [5, 8, 10, 15] {
        let output = skim_cmd()
            .arg(fixture.to_str().unwrap())
            .arg("--mode=structure")
            .arg("--max-lines")
            .arg(budget.to_string())
            .arg("--no-cache")
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "Should succeed with --max-lines={}: stderr={:?}",
            budget,
            String::from_utf8_lossy(&output.stderr),
        );

        let stdout = String::from_utf8(output.stdout).unwrap();
        let line_count = stdout.lines().count();
        assert!(
            line_count <= budget,
            "Output must not exceed --max-lines={}, got {} lines:\n{}",
            budget,
            line_count,
            stdout,
        );
    }
}

#[test]
fn test_max_lines_noncontiguous_spans_contain_markers() {
    // With a tight budget on mixed_priority.ts, the output should contain
    // omission markers between the non-contiguous selected spans.
    let fixture = fixture_path("typescript/mixed_priority.ts");

    let output = skim_cmd()
        .arg(fixture.to_str().unwrap())
        .arg("--mode=structure")
        .arg("--max-lines")
        .arg("10")
        .arg("--no-cache")
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();

    // The full structure output of mixed_priority.ts is ~17 lines (with blank
    // lines). With budget=10, truncation must drop some spans and insert
    // omission markers in the gaps.
    assert!(
        stdout.contains("// ... (truncated)"),
        "Non-contiguous truncation should produce omission markers:\n{}",
        stdout,
    );
}

#[test]
fn test_max_lines_noncontiguous_spans_preserve_high_priority() {
    // Types and interfaces should be preserved over functions when budget
    // is tight, because they have higher priority scores (5 vs 4).
    let fixture = fixture_path("typescript/mixed_priority.ts");

    let output = skim_cmd()
        .arg(fixture.to_str().unwrap())
        .arg("--mode=structure")
        .arg("--max-lines")
        .arg("10")
        .arg("--no-cache")
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Type aliases have priority 5, should be kept
    assert!(
        stdout.contains("type UserId"),
        "Type aliases (priority 5) should be preserved under tight budget:\n{}",
        stdout,
    );

    // Interfaces have priority 5, should be kept (at least one)
    assert!(
        stdout.contains("interface"),
        "Interfaces (priority 5) should be preserved under tight budget:\n{}",
        stdout,
    );
}

#[test]
fn test_max_lines_noncontiguous_spans_rust_fixture() {
    // Verify the same non-contiguous marker behavior works for Rust fixtures.
    // mixed_priority.rs has type aliases, enums, traits, structs, impls,
    // and functions -- a rich mix of priority levels.
    let fixture = fixture_path("rust/mixed_priority.rs");
    assert!(fixture.exists(), "Fixture file should exist: {:?}", fixture);

    for budget in [5, 10, 15] {
        let output = skim_cmd()
            .arg(fixture.to_str().unwrap())
            .arg("--mode=structure")
            .arg("--max-lines")
            .arg(budget.to_string())
            .arg("--no-cache")
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "Rust fixture should succeed with --max-lines={}: stderr={:?}",
            budget,
            String::from_utf8_lossy(&output.stderr),
        );

        let stdout = String::from_utf8(output.stdout).unwrap();
        let line_count = stdout.lines().count();
        assert!(
            line_count <= budget,
            "Rust output must not exceed --max-lines={}, got {} lines:\n{}",
            budget,
            line_count,
            stdout,
        );
    }
}
