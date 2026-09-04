//! CLI tests for --max-lines flag
//!
//! Tests the --max-lines flag through the skim binary.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::PathBuf;
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
    // Use a file with substantial function bodies so the B5 elision-hint overhead
    // (≈10 tokens appended to every truncation marker) is absorbed by genuine body
    // savings. A 3-line file with return-only bodies compresses to roughly the same
    // token count as the marker alone, causing the guardrail to fire and emit raw
    // (bypassing --max-lines entirely).
    std::fs::write(
        &file,
        concat!(
            "type A = string;\n",
            "function foo(): void {\n",
            "    const x = Math.random();\n",
            "    const y = Math.floor(x * 100);\n",
            "    console.log('Result:', y);\n",
            "    return;\n",
            "}\n",
            "function bar(): void {\n",
            "    const items = [1, 2, 3, 4, 5];\n",
            "    const sum = items.reduce((a, b) => a + b, 0);\n",
            "    console.log('Sum:', sum);\n",
            "    return;\n",
            "}\n",
        ),
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
    // ADR-001 outranks priority selection on inputs this small. The bounded
    // structure view needs two elision markers to show the class, and those
    // markers cost more than the raw source they replace — so the net-savings
    // guard serves raw, and raw truncated to 5 lines is the head of the file.
    //
    // This is a real consequence of making elision markers honest: a marker has
    // a byte cost, and on a short file that cost tips ADR-001 toward raw, which
    // has no notion of node priority. The bound still holds and the elision is
    // still disclosed — what is lost is compression, not fidelity.
    //
    // PF-027: the fixture is deliberately NOT enlarged to make compression pay.
    // Resizing an input until a guard agrees is a silent revert of the guard.
    assert_eq!(
        stdout.lines().count(),
        5,
        "--max-lines 5 must hold regardless of which view the guard selects: {:?}",
        stdout,
    );
    assert!(
        stdout.contains("truncated"),
        "elision must be disclosed even when the guard serves raw: {:?}",
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
    // Markers include counts: "// ... (N lines truncated)" or
    // "// ... (1 line truncated)". The word "truncated" is always present.
    assert!(
        stdout.contains("truncated"),
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

// ============================================================================
// ADR-016 N=1 carve-out regression tests (reliability-1 / reliability-3 /
// consistency-7 / complexity-2)
//
// Pre-fix, the outer passthrough_with_truncation in process.rs re-applied the
// line bound over output that rskim-core had already bounded. For N=1 this was
// fatal: core emits [content_line, marker] (2 lines); outer pass saw 2 > 1 and
// kept only the marker (0 content lines). Fixed by delegating post-guardrail
// bounds to core via enforce_line_bounds and only firing that helper when the
// guardrail actually triggered (raw was served; core produced no inner bound).
// ============================================================================

/// Pins reliability-1: `--max-lines 1 --mode=full` must yield ≥1 non-marker
/// content line. Pre-fix the double-application zeroed the content, leaving
/// only the elision marker (ADR-016 N=1 carve-out was undone).
///
/// mixed_priority.ts is 43 source lines; expected: 1 content line + marker
/// disclosing 42 omitted lines (source-space count, ADR-017).
#[test]
fn max_lines_1_n1_carve_out_mode_full() {
    let fixture = fixture_path("typescript/mixed_priority.ts");

    let output = skim_cmd()
        .arg(fixture.to_str().unwrap())
        .arg("--mode=full")
        .arg("--max-lines")
        .arg("1")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success(), "should succeed");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();

    // N=1 carve-out: 1 content line + 1 marker = 2 lines total
    // (serving only the marker with zero code violates the carve-out).
    assert!(
        lines.iter().any(|l| !l.contains("truncated")),
        "--max-lines 1 N=1 carve-out: at least one non-marker content line required;\
         pre-fix double-application served zero content lines.\nGot:\n{}",
        stdout,
    );
    assert!(
        stdout.contains("truncated"),
        "marker must be present even with N=1 (ADR-016 loss disclosure):\n{}",
        stdout,
    );
    // Marker count must be in source-space (ADR-017): 43 source lines − 1 kept = 42.
    assert!(
        stdout.contains("42"),
        "marker must disclose 42 omitted source lines (not output-space count):\n{}",
        stdout,
    );
}

/// Pins reliability-1 on the structure path. Same invariants as the full-mode
/// test above, but exercises the core transform path (no-guardrail) and, when
/// the ADR-001 guard fires and serves raw, the enforce_line_bounds path.
#[test]
fn max_lines_1_n1_carve_out_mode_structure() {
    let fixture = fixture_path("typescript/mixed_priority.ts");

    let output = skim_cmd()
        .arg(fixture.to_str().unwrap())
        .arg("--mode=structure")
        .arg("--max-lines")
        .arg("1")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success(), "should succeed");

    let stdout = String::from_utf8(output.stdout).unwrap();

    assert!(
        stdout.lines().any(|l| !l.contains("truncated")),
        "--max-lines 1 --mode=structure: at least one non-marker content line required:\n{}",
        stdout,
    );
    assert!(
        stdout.contains("truncated"),
        "marker must be present (ADR-016 disclosure):\n{}",
        stdout,
    );
}

/// Pins consistency-7: the stdin path must honour --max-lines 1 with the N=1
/// carve-out. Pre-fix, process_stdin had no post-guardrail line-bound
/// enforcement at all, so the guardrail-raw path silently skipped truncation.
#[test]
fn max_lines_1_n1_carve_out_stdin() {
    // 10-line TypeScript file via stdin. Every line is a distinct declaration
    // so the structure view does not compress significantly, causing the
    // ADR-001 guardrail to fire and serve raw — exercising the enforce_line_bounds
    // path inside process_stdin.
    let source = "type A = string;\n\
                  type B = number;\n\
                  type C = boolean;\n\
                  type D = never;\n\
                  type E = unknown;\n\
                  type F = object;\n\
                  type G = symbol;\n\
                  type H = bigint;\n\
                  type I = null;\n\
                  type J = undefined;\n";

    let output = skim_cmd()
        .arg("-")
        .arg("-l")
        .arg("typescript")
        .arg("--max-lines")
        .arg("1")
        .write_stdin(source)
        .output()
        .unwrap();

    assert!(output.status.success(), "stdin --max-lines 1 should succeed");

    let stdout = String::from_utf8(output.stdout).unwrap();

    assert!(
        stdout.lines().any(|l| !l.contains("truncated")),
        "stdin --max-lines 1 N=1 carve-out: at least one non-marker content line required:\n{}",
        stdout,
    );
    assert!(
        stdout.contains("truncated"),
        "stdin --max-lines 1 must disclose the elision:\n{}",
        stdout,
    );
}

/// Pins reliability-3: the guardrail-served-raw path must not cut inside a
/// multi-line template literal. Pre-fix, the outer passthrough_with_truncation
/// was literal-blind and would naïvely slice at the computed keep-index,
/// leaking interior literal lines into the output.
///
/// The fixture is a short 7-line TypeScript file where the template literal
/// spans lines 2-6. With --mode=structure the structure output ≈ raw (all
/// const declarations are module-level structure), so the ADR-001 guardrail
/// fires and serves raw — exercising the enforce_line_bounds → simple_line_truncate
/// (literal-aware) path.
///
/// With --max-lines 4, the naïve cut at line 3 falls inside the literal.
/// The literal-aware pull-back retreats to line 1, producing 1 content line
/// + marker, never any interior literal text.
#[test]
fn max_lines_literal_aware_pull_back() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lit.ts");
    // Lines:
    //   1: const a = 1;
    //   2: const msg = `
    //   3:   interior text
    //   4:   still inside
    //   5:   last interior
    //   6: `;
    //   7: const b = 2;
    // Naïve keep=N-1=3 includes lines 1-3; line 3 is inside the literal.
    // Literal-aware: pull back to line 1 (last complete line before literal).
    std::fs::write(
        &file,
        "const a = 1;\n\
         const msg = `\n\
           interior text\n\
           still inside\n\
           last interior\n\
         `;\n\
         const b = 2;\n",
    )
    .unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--mode=structure")
        .arg("--max-lines")
        .arg("4")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success(), "should succeed");

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Interior literal text must NOT appear: a literal-blind cut at line 3
    // would leak "interior text" or "still inside" into the output.
    assert!(
        !stdout.contains("interior text"),
        "literal-aware pull-back must not cut inside a template literal:\n{}",
        stdout,
    );
    assert!(
        !stdout.contains("still inside"),
        "literal-aware pull-back must not cut inside a template literal:\n{}",
        stdout,
    );
    // The elision must still be disclosed — pull-back must not silently expand
    // the output past the budget either.
    assert!(
        stdout.contains("truncated"),
        "pull-back must disclose the elision (ADR-016 / ADR-011 class 1):\n{}",
        stdout,
    );
}

/// Pins complexity-2: `--last-lines N -n` must annotate output lines with
/// their true source positions, not with identity labels 1..N.
///
/// Pre-fix, apply_line_numbers checked `guardrail_triggered` before
/// `computed_map`; the identity map (1..N) was applied when the guardrail
/// served raw, producing wrong line numbers for tail-window views.
/// After the fix, computed_map (which carries the correct start offset from
/// simple_last_line_truncate_with_start) takes priority.
///
/// The fixture is an 8-line type-only TypeScript file. Structure mode keeps
/// all type aliases (they ARE the structure), so output ≈ raw and the
/// ADR-001 guardrail fires — serving raw and triggering the enforce_line_bounds
/// path where the bug lived.
///
/// With --last-lines 3, the retained lines are source lines 6, 7, 8.
/// Correct annotation: "6\ttype Zeta = null;" etc.
/// Wrong annotation (pre-fix identity map): "1\ttype Zeta = null;" etc.
#[test]
fn last_lines_line_numbers_source_positions() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("types.ts");
    // 8-line file: all type aliases. Structure mode emits all of them
    // (type aliases are the structure), so compressed ≈ raw → ADR-001
    // guardrail fires → enforce_line_bounds path is exercised.
    //
    // Source lines 6, 7, 8 are retained by --last-lines 3:
    //   6: type Zeta = null;
    //   7: type Eta = undefined;
    //   8: type Theta = object;
    std::fs::write(
        &file,
        "type Alpha = string;\n\
         type Beta = number;\n\
         type Gamma = boolean;\n\
         type Delta = never;\n\
         type Epsilon = unknown;\n\
         type Zeta = null;\n\
         type Eta = undefined;\n\
         type Theta = object;\n",
    )
    .unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--mode=structure")
        .arg("--last-lines")
        .arg("3")
        .arg("-n")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success(), "should succeed");

    let stdout = String::from_utf8(output.stdout).unwrap();

    // Correct: source positions 6/7/8 annotate the tail lines.
    // The tab-separated format is "{source_line}\t{content}" (format.rs AC-18).
    assert!(
        stdout.contains("6\ttype") || stdout.contains("7\ttype") || stdout.contains("8\ttype"),
        "--last-lines 3 -n must show source positions (6/7/8), not identity 1/2/3:\n{}",
        stdout,
    );
    // Pre-fix identity map labelled 1, 2, 3 — none of those must appear.
    assert!(
        !stdout.contains("1\ttype") && !stdout.contains("2\ttype") && !stdout.contains("3\ttype"),
        "identity map (1/2/3) detected — source positions not applied (complexity-2 regression):\n{}",
        stdout,
    );
}
