//! CLI tests for --last-lines flag
//!
//! Tests the --last-lines flag through the skim binary.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

/// Get a command for the skim binary
fn skim_cmd() -> Command {
    let mut cmd = Command::cargo_bin("skim").unwrap();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd
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

#[test]
fn test_last_lines_with_glob_pattern() {
    let dir = TempDir::new().unwrap();

    // Create two multi-line TypeScript files
    fs::write(
        dir.path().join("file1.ts"),
        "type A = string;\ntype B = number;\nfunction foo(): void {}\nfunction bar(): void {}\nconst x = 1;\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("file2.ts"),
        "type C = boolean;\ntype D = string;\nfunction baz(): void {}\nfunction qux(): void {}\nconst y = 2;\n",
    )
    .unwrap();

    let output = skim_cmd()
        .arg("*.ts")
        .arg("--last-lines")
        .arg("3")
        .arg("--mode=full")
        .arg("--no-cache")
        .current_dir(dir.path())
        .output()
        .unwrap();

    let stderr = String::from_utf8(output.stderr.clone()).unwrap_or_default();
    assert!(
        output.status.success(),
        "Glob with --last-lines should succeed. stderr: {:?}",
        stderr,
    );

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Each file section in multi-file output gets a header line (// === file.ts ===)
    // followed by the per-file output. Verify that per-file content respects the
    // last-lines limit by checking each section individually.
    let sections: Vec<&str> = stdout.split("// === ").filter(|s| !s.is_empty()).collect();
    assert!(
        sections.len() >= 2,
        "Should have at least 2 file sections in glob output, got {}: {:?}",
        sections.len(),
        stdout,
    );

    for section in &sections {
        // Each section starts with "filename.ts ===\n" header, then content lines.
        // Trailing empty lines are file separators, not content, so trim them.
        let content_lines: Vec<&str> = section
            .lines()
            .skip(1) // skip the header line (e.g., "file1.ts ===")
            .collect::<Vec<_>>();
        let content_count = content_lines
            .iter()
            .rev()
            .skip_while(|l| l.is_empty())
            .count();
        assert!(
            content_count <= 3,
            "Each file section should have at most 3 content lines, got {}: {:?}",
            content_count,
            section,
        );
    }
}
