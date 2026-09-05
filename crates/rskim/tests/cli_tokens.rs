//! CLI integration tests for token counting and --show-stats flag
//!
//! Tests token reduction statistics output

use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;
mod common;

#[test]
fn test_show_stats_single_file() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(
        &file_path,
        "function test(a: number, b: number): number {\n    return a + b;\n}",
    )
    .unwrap();

    common::skim()
        .arg(&file_path)
        .arg("--show-stats")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("tokens"))
        .stderr(predicate::str::contains("→"))
        .stderr(predicate::str::contains("%"));
}

#[test]
fn test_show_stats_multiple_files() {
    let temp_dir = TempDir::new().unwrap();

    fs::write(
        temp_dir.path().join("file1.ts"),
        "function a(x: number) { return x * 2; }",
    )
    .unwrap();
    fs::write(
        temp_dir.path().join("file2.ts"),
        "function b(y: string) { return y.toUpperCase(); }",
    )
    .unwrap();
    fs::write(
        temp_dir.path().join("file3.ts"),
        "function c(z: boolean) { return !z; }",
    )
    .unwrap();

    // Stats should show aggregated counts for multiple files
    common::skim()
        .arg("*.ts")
        .arg("--show-stats")
        .current_dir(temp_dir.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("file(s)"));
}

#[test]
fn test_show_stats_with_structure_mode() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(
        &file_path,
        "function longFunction() {\n    const x = 1;\n    const y = 2;\n    return x + y;\n}",
    )
    .unwrap();

    common::skim()
        .arg(&file_path)
        .arg("--mode=structure")
        .arg("--show-stats")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("tokens"));
}

#[test]
fn test_show_stats_with_signatures_mode() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(
        &file_path,
        "function add(a: number, b: number): number { return a + b; }",
    )
    .unwrap();

    common::skim()
        .arg(&file_path)
        .arg("--mode=signatures")
        .arg("--show-stats")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("tokens"));
}

#[test]
fn test_show_stats_with_full_mode() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function test() { return 42; }").unwrap();

    // Full mode should show 0% reduction (or 100% of original)
    common::skim()
        .arg(&file_path)
        .arg("--mode=full")
        .arg("--show-stats")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"));
}

#[test]
fn test_show_stats_with_stdin() {
    common::skim()
        .arg("-")
        .arg("--language=typescript")
        .arg("--show-stats")
        .write_stdin("function test(x: number): number { return x * 2; }")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("tokens"));
}

#[test]
fn test_no_stats_by_default() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function test() { return 42; }").unwrap();

    // Without --show-stats, stderr must not contain token-stats output.
    // The ADR-011 class-1 lossy-view marker may appear unconditionally when the
    // served view differs from raw — that is expected and is not stats output.
    // It is spelled "[skim] <class> view: …", where the class names the mode
    // (e.g. "structure view: bodies removed"), so the guard below matches the
    // marker's shape rather than one mode's wording.
    let output = common::skim()
        .arg(&file_path)
        .assert()
        .success()
        .get_output()
        .stderr
        .clone();

    let stderr_str = String::from_utf8_lossy(&output);
    // Token-stats output (requires --show-stats) must not appear.
    assert!(
        !stderr_str.contains("tokens \u{2192}"),
        "Token stats should not appear without --show-stats flag"
    );
    // Guard: any [skim] line on stderr must be the ADR-011 class-1 lossy-view
    // marker, not unexpected noise from an unrelated code path.
    for line in stderr_str.lines() {
        if line.contains("[skim]") {
            assert!(
                line.contains(" view:"),
                "unexpected [skim] line on stderr (not the ADR-011 lossy-view marker): {line:?}"
            );
        }
    }
}

#[test]
fn test_show_stats_format_contains_reduction_percentage() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(
        &file_path,
        "function calculate(a: number, b: number): number {\n    \
         const sum = a + b;\n    \
         const product = a * b;\n    \
         return sum + product;\n\
         }",
    )
    .unwrap();

    let output = common::skim()
        .arg(&file_path)
        .arg("--mode=structure")
        .arg("--show-stats")
        .assert()
        .success()
        .get_output()
        .stderr
        .clone();

    let stderr_str = String::from_utf8_lossy(&output);

    // Stats should include token counts and reduction percentage
    assert!(stderr_str.contains("tokens"), "Should show token count");
    assert!(stderr_str.contains("→"), "Should show arrow separator");
    assert!(stderr_str.contains("%"), "Should show percentage");
}

#[test]
fn test_show_stats_with_python() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.py");
    fs::write(
        &file_path,
        "def calculate_sum(a: int, b: int) -> int:\n    result = a + b\n    return result",
    )
    .unwrap();

    common::skim()
        .arg(&file_path)
        .arg("--show-stats")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("tokens"));
}

#[test]
fn test_show_stats_with_rust() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.rs");
    fs::write(
        &file_path,
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}",
    )
    .unwrap();

    common::skim()
        .arg(&file_path)
        .arg("--show-stats")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("tokens"));
}

#[test]
fn test_show_stats_with_glob_and_no_header() {
    let temp_dir = TempDir::new().unwrap();

    fs::write(temp_dir.path().join("a.ts"), "function a() {}").unwrap();
    fs::write(temp_dir.path().join("b.ts"), "function b() {}").unwrap();

    // Stats should still work with --no-header flag
    common::skim()
        .arg("*.ts")
        .arg("--no-header")
        .arg("--show-stats")
        .current_dir(temp_dir.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("file(s)"));
}

#[test]
fn test_show_stats_empty_file() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("empty.ts");
    fs::write(&file_path, "").unwrap();

    // Empty file should still work with --show-stats (likely 0 → 0 tokens)
    common::skim()
        .arg(&file_path)
        .arg("--show-stats")
        .assert()
        .success();
}

/// `--tokens N` on an extension-less file must honour the budget rather than
/// silently emit the whole file (consistency-15 / ADR-016).
///
/// The unknown-language path previously called `passthrough_with_truncation`
/// with `max_lines` and `last_lines` only, discarding the token budget.  After
/// the fix, a binary search finds the largest head prefix that fits within N
/// tokens and emits it with an elision marker on stdout.
#[test]
fn test_tokens_budget_honoured_for_unknown_language_file() {
    let temp_dir = TempDir::new().unwrap();
    // Write an extension-less file that is long enough to exceed a tiny budget.
    // 50 lines × ~10 tokens each ≈ 500 tokens — well above a budget of 5.
    let mut content = String::new();
    for i in 0..50 {
        content.push_str(&format!("line number {i} with some extra words to pad token count\n"));
    }
    // No extension so language detection falls back to the unknown-language path.
    let file_path = temp_dir.path().join("datafile");
    fs::write(&file_path, &content).unwrap();

    let output = common::skim()
        .arg(&file_path)
        .arg("--tokens")
        .arg("5")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8_lossy(&output);

    // The output must NOT be the entire file (50 lines).  If the budget were
    // silently ignored we would get all 50 lines; after the fix the output is
    // truncated to fit within the budget.
    let line_count = stdout.lines().count();
    assert!(
        line_count < 50,
        "--tokens 5 on an unknown-language file must truncate output; got {line_count} lines"
    );

    // An elision marker must be present so the reader knows content was dropped
    // (ADR-011 class 1 / #317: compress, never truncate silently).
    assert!(
        stdout.contains("lines truncated") || stdout.contains("line truncated"),
        "--tokens budget truncation must emit an elision marker; got: {stdout:?}"
    );
}
