//! CLI tests for --last-lines flag
//!
//! Tests the --last-lines flag through the skim binary.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;
mod common;

/// Get a command for the skim binary
fn skim_cmd() -> Command {
    let mut cmd = common::skim();
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
        line_count <= 4,
        "Output should have at most 4 lines (3 content + 1 marker), got {}: {:?}",
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
        line_count <= 4,
        "Output should have at most 4 lines in structure mode (3 content + 1 marker), got {}: {:?}",
        line_count,
        stdout,
    );
}

#[test]
fn test_last_lines_with_pseudo_mode() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.py");
    // Use a file with substantial bodies so the B5 elision-hint overhead (≈10 tokens
    // appended to every truncation marker) is absorbed by genuine body savings.
    // A tiny pass-only file has almost no savings, causing the guardrail to fire and
    // emit raw (bypassing --last-lines entirely).
    std::fs::write(
        &file,
        concat!(
            "import os\n",
            "import sys\n",
            "import json\n",
            "\n",
            "def process_items(items, config):\n",
            "    result = []\n",
            "    for item in items:\n",
            "        if item.get('active'):\n",
            "            value = item['value'] * config.get('multiplier', 1)\n",
            "            result.append(value)\n",
            "    return result\n",
            "\n",
            "def validate_config(config):\n",
            "    required = ['name', 'version', 'items']\n",
            "    for key in required:\n",
            "        if key not in config:\n",
            "            return False\n",
            "    return True\n",
            "\n",
            "def transform_data(data, config):\n",
            "    if not validate_config(config):\n",
            "        return None\n",
            "    processed = process_items(data.get('items', []), config)\n",
            "    return json.dumps(processed)\n",
        ),
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
        line_count <= 5,
        "Output should have at most 5 lines in pseudo mode (4 content + 1 marker), got {}: {:?}",
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

    // Each file section in multi-file output gets a header line (// file.ts)
    // followed by the per-file output. Verify that per-file content respects the
    // last-lines limit by checking each section individually.
    //
    // Split on "\n// " (newline-anchored) to avoid splitting on inline comment
    // lines inside file content. Prepend "\n" so the first header is also
    // preceded by a newline and the split pattern matches uniformly.
    let normalized = format!("\n{stdout}");
    let sections: Vec<&str> = normalized
        .split("\n// ")
        .filter(|s| !s.is_empty())
        .collect();
    assert!(
        sections.len() >= 2,
        "Should have at least 2 file sections in glob output, got {}: {:?}",
        sections.len(),
        stdout,
    );

    for section in &sections {
        // Each section starts with "filename.ts\n" header, then content lines.
        // Trailing empty lines are file separators, not content, so trim them.
        let content_lines: Vec<&str> = section
            .lines()
            .skip(1) // skip the header line (e.g., "file1.ts")
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

// ============================================================================
// ADR-016 N=1 carve-out for --last-lines (reliability-5)
//
// The ADR-016 N=1 carve-out states that spending the only slot on the marker
// returns a view with no code, which violates the no-silent-loss rule.  For
// `--max-lines 1` this carve-out was already present.  For `--last-lines 1`
// it was missing: `simple_last_line_truncate_with_start` computed
// `start = total - 0 = total` (zero content lines), emitting only the marker.
//
// Post-fix: N=1 for `--last-lines` emits 1 content line (the last line) + 1
// marker = 2 lines total, mirroring the head carve-out exactly.
// ============================================================================

fn fixture_path_ll(relative: &str) -> std::path::PathBuf {
    let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop();
    path.pop();
    path.join("tests/fixtures").join(relative)
}

/// Pins reliability-5: `--last-lines 1 --mode=full` must yield ≥1 non-marker
/// content line.
///
/// Pre-fix: `simple_last_line_truncate_with_start` computed `start = total`
/// for n=1, leaving zero content lines and emitting only the elision marker.
/// The N=1 carve-out that `--max-lines 1` already had was not applied to the
/// tail, so `tail -1` rewrites (→ `--mode=full --last-lines 1`) were broken.
///
/// mixed_priority.ts is 43 source lines; expected: 1 marker + 1 content line.
#[test]
fn last_lines_1_n1_carve_out_mode_full() {
    let fixture = fixture_path_ll("typescript/mixed_priority.ts");

    let output = skim_cmd()
        .arg(fixture.to_str().unwrap())
        .arg("--mode=full")
        .arg("--last-lines")
        .arg("1")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success(), "should succeed");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();

    // N=1 carve-out: 1 marker + 1 content line = 2 lines total.
    // Pre-fix yielded only the marker (1 line, 0 content).
    assert!(
        lines.iter().any(|l| !l.contains("lines above")),
        "--last-lines 1 N=1 carve-out: at least one non-marker content line required;\
         pre-fix returned only the elision marker with zero code.\nGot:\n{}",
        stdout,
    );
    assert!(
        stdout.contains("lines above"),
        "marker must be present even with N=1 (ADR-016 loss disclosure):\n{}",
        stdout,
    );
    // Marker must disclose the omitted count in source-space (ADR-017):
    // 43 source lines − 1 kept = 42.
    assert!(
        stdout.contains("42"),
        "marker must disclose 42 omitted source lines (source-space count):\n{}",
        stdout,
    );
}

/// Pins reliability-5 on the structure path.  Same invariants as the full-mode
/// test above, but exercises the core transform → guardrail path.
#[test]
fn last_lines_1_n1_carve_out_mode_structure() {
    let fixture = fixture_path_ll("typescript/mixed_priority.ts");

    let output = skim_cmd()
        .arg(fixture.to_str().unwrap())
        .arg("--mode=structure")
        .arg("--last-lines")
        .arg("1")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success(), "should succeed");

    let stdout = String::from_utf8(output.stdout).unwrap();

    assert!(
        stdout.lines().any(|l| !l.contains("lines above")),
        "--last-lines 1 --mode=structure: at least one non-marker content line required:\n{}",
        stdout,
    );
    assert!(
        stdout.contains("lines above"),
        "marker must be present (ADR-016 disclosure):\n{}",
        stdout,
    );
}
