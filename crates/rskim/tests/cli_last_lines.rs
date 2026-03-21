//! CLI tests for --last-lines flag
//!
//! Tests the --last-lines flag through the skim binary.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

/// Get a command for the skim binary
fn skim_cmd() -> Command {
    Command::cargo_bin("skim").unwrap()
}

#[test]
fn test_last_lines_basic() {
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
        .arg("--last-lines")
        .arg("3")
        .arg("--mode=full")
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
    // Should contain truncation marker
    assert!(
        stdout.contains("lines above"),
        "Should contain 'lines above' marker: {:?}",
        stdout
    );
}

#[test]
fn test_last_lines_larger_than_file_unchanged() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("small.ts");
    let content = "const x = 1;\nconst y = 2;\n";
    std::fs::write(&file, content).unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--last-lines")
        .arg("100")
        .arg("--mode=full")
        .arg("--no-cache")
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout, content,
        "When --last-lines exceeds file length, output should be unchanged"
    );
}

#[test]
fn test_last_lines_mutual_exclusion_with_max_lines() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    std::fs::write(&file, "const x = 1;").unwrap();

    skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--last-lines")
        .arg("5")
        .arg("--max-lines")
        .arg("5")
        .assert()
        .failure()
        .stderr(predicate::str::contains("mutually exclusive"));
}

#[test]
fn test_last_lines_zero_rejected() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    std::fs::write(&file, "const x = 1;").unwrap();

    skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--last-lines")
        .arg("0")
        .assert()
        .failure()
        .stderr(predicate::str::contains("--last-lines must be at least 1"));
}

#[test]
fn test_last_lines_with_structure_mode() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    std::fs::write(
        &file,
        "import { a } from 'a';\n\
         import { b } from 'b';\n\
         type Foo = string;\n\
         interface Bar { x: number; y: string; }\n\
         function hello(): void { console.log('hello'); }\n\
         function world(): void { console.log('world'); }\n\
         function third(): void { console.log('third'); }\n\
         export { hello, world, third };\n",
    )
    .unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--last-lines")
        .arg("3")
        .arg("--mode=structure")
        .arg("--no-cache")
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let line_count = stdout.lines().count();
    assert!(
        line_count <= 3,
        "Output should have at most 3 lines in structure mode, got {}: {:?}",
        line_count,
        stdout,
    );
}

#[test]
fn test_last_lines_with_pseudo_mode() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.py");
    std::fs::write(
        &file,
        "import os\nimport sys\ndef foo():\n    pass\ndef bar():\n    pass\ndef baz():\n    pass\n",
    )
    .unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--last-lines")
        .arg("4")
        .arg("--mode=pseudo")
        .arg("--no-cache")
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let line_count = stdout.lines().count();
    assert!(
        line_count <= 4,
        "Output should have at most 4 lines in pseudo mode, got {}: {:?}",
        line_count,
        stdout,
    );
}
