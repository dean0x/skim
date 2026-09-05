//! RED integration tests: cascade must not serve empty stdout when every
//! escalated mode produces "" for a Rust file that has only `fn` items.
//!
//! Defect (measured at HEAD with target/debug/skim):
//!   `skim <file> --tokens 25` exits 0 with ZERO bytes on stdout and no marker
//!   when the file contains only `fn` items and no type declarations.
//!
//! Root cause (`crates/rskim/src/cascade.rs` ~:123–147):
//!   The cascade escalated through modes and accepted an escalated output of ""
//!   because `count_tokens_or_max("") == 0 <= budget`; only `Ok(None)` triggered
//!   `continue`. The fix (9f0be23) treats empty escalated output as "does not
//!   satisfy", continues the cascade, and falls back to line truncation with an
//!   elision marker containing the word `truncated` when every mode is empty.
//!
//! RED at 9f0be23^; GREEN since 9f0be23.

use tempfile::TempDir;

mod common;

/// Rust source that contains ONLY `fn` items — no type declarations.
///
/// In `types` mode skim produces "" for this content, which the current cascade
/// incorrectly accepts as satisfying the token budget.
const ONLY_FN_FIXTURE: &str = r#"fn alpha(x: i32) -> i32 {
    let y = x * 2;
    y + 1
}

fn beta(s: &str) -> usize {
    s.len()
}

fn gamma() {
    println!("hello");
}

fn delta(a: u8, b: u8) -> u8 {
    a.wrapping_add(b)
}
"#;

/// Build a sandboxed skim command with analytics and passthrough disabled.
fn skim_cmd() -> assert_cmd::Command {
    let mut cmd = common::skim();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd
}

// ============================================================================
// RED tests
// ============================================================================

/// When every escalated mode produces "" for a fn-only Rust file, the cascade
/// MUST NOT serve empty stdout. It must fall back to line truncation and emit a
/// marker containing the word `truncated`.
///
/// Today this fails because cascade.rs accepts the empty `types`-mode output.
#[test]
fn tokens_budget_never_serves_empty_stdout_when_escalated_mode_is_empty() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("no_types.rs");
    std::fs::write(&file, ONLY_FN_FIXTURE).unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--tokens")
        .arg("25")
        .arg("--no-cache")
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();

    assert!(
        output.status.success(),
        "skim should exit 0 even when every mode is empty. stderr: {stderr:?}",
    );

    assert!(
        !stdout.is_empty(),
        "stdout must not be empty when the cascade exhausts all modes \
         (should fall back to line-truncation elision marker). \
         stderr: {stderr:?}",
    );

    assert!(
        stdout.contains("truncated"),
        "stdout must contain 'truncated' elision marker, got: {stdout:?}. stderr: {stderr:?}",
    );
}

/// Even a budget of 5 tokens must not silently produce empty output for a
/// fn-only Rust file. The cascade must surface a `truncated` marker.
///
/// Today this fails for the same reason as the 25-token test above.
#[test]
fn tokens_budget_tiny_budget_still_discloses_with_marker() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("no_types.rs");
    std::fs::write(&file, ONLY_FN_FIXTURE).unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--tokens")
        .arg("5")
        .arg("--no-cache")
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();

    assert!(
        output.status.success(),
        "skim should exit 0 even with a budget of 5 tokens. stderr: {stderr:?}",
    );

    assert!(
        !stdout.is_empty(),
        "stdout must not be empty for a 5-token budget on a fn-only Rust file \
         (should fall back to line-truncation elision marker). \
         stderr: {stderr:?}",
    );

    assert!(
        stdout.contains("truncated"),
        "stdout must contain 'truncated' elision marker for a 5-token budget, \
         got: {stdout:?}. stderr: {stderr:?}",
    );
}

/// ADR-016 / ADR-011 class 1: when the token budget is too tight to include
/// the `SKIM_PASSTHROUGH=1` remedy hint inline on stdout (compact marker form),
/// the CLI must emit the remedy unconditionally on stderr.
///
/// Observed with `target/debug/skim <no_types.rs> --tokens 3` (debug binary
/// predates the empty-mode guard and this fix):
///   stdout: (empty — the old cascade accepted the 0-token empty types output)
///   stderr: "[skim] token budget: escalated from structure to types mode (0 tokens)"
///           "[skim] structure view: bodies removed — SKIM_PASSTHROUGH=1 for raw output"
///   → no truncation remedy emitted on stderr
///
/// Expected after fix: stdout carries the compact elision marker (count only,
/// no inline hint); stderr carries the exact remedy line.
#[test]
fn tokens_tiny_budget_compact_marker_puts_remedy_on_stderr() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("no_types.rs");
    std::fs::write(&file, ONLY_FN_FIXTURE).unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--tokens")
        .arg("3")
        .arg("--no-cache")
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();

    assert!(
        output.status.success(),
        "skim must exit 0 with a tiny token budget. stderr: {stderr:?}",
    );

    // Compact form: the count must appear on stdout, but the hint must not
    // (the budget is too tight to include it inline).
    assert!(
        stdout.contains("truncated)"),
        "stdout must contain the compact elision marker (count), got: {stdout:?}. \
         stderr: {stderr:?}",
    );
    assert!(
        !stdout.contains("SKIM_PASSTHROUGH=1"),
        "stdout must NOT contain the inline hint in compact form (budget too tight), \
         got: {stdout:?}",
    );

    // ADR-016 channel split: the remedy must appear on stderr so the reader
    // always sees SKIM_PASSTHROUGH=1 regardless of how tight the budget is.
    assert!(
        stderr.contains(
            "[skim] output truncated to the --tokens budget \
             \u{2014} SKIM_PASSTHROUGH=1 for full output"
        ),
        "stderr must carry the ADR-016 remedy notice, got: {stderr:?}",
    );
}

/// A Rust file containing only `//` comment lines (no fn/type declarations).
/// With a token budget of 20 the cascade exhausts all structural modes (all
/// yield ""), falls back to the raw source, and must serve either the raw
/// comment lines or an elision marker — never empty stdout.
#[test]
fn tokens_budget_comment_only_file_serves_raw_or_truncated_content() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("comments_only.rs");
    let content = "// line 01\n// line 02\n// line 03\n// line 04\n// line 05\n\
                   // line 06\n// line 07\n// line 08\n// line 09\n// line 10\n\
                   // line 11\n// line 12\n";
    std::fs::write(&file, content).unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--tokens")
        .arg("20")
        .arg("--no-cache")
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();

    assert!(
        output.status.success(),
        "skim should exit 0 for a comment-only Rust file. stderr: {stderr:?}",
    );

    assert!(
        !stdout.is_empty(),
        "stdout must not be empty for a comment-only file — \
         cascade must serve raw or truncated content. stderr: {stderr:?}",
    );

    assert!(
        stdout.contains("truncated") || stdout.contains("// comment") || stdout.contains("// line"),
        "stdout must contain either an elision marker or comment lines, got: {stdout:?}. \
         stderr: {stderr:?}",
    );
}

/// A file whose entire content is whitespace (newlines and spaces) should
/// produce empty stdout, exit 0, and carry no `Error` on stderr.
///
/// The cascade must recognise that the source is empty/whitespace and return
/// an empty result with no elision marker (nothing was elided).
#[test]
fn tokens_budget_whitespace_only_file_is_empty_and_succeeds() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("whitespace_only.ts");
    std::fs::write(&file, "   \n  \n\n   \n").unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--tokens")
        .arg("100")
        .arg("--no-cache")
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();

    assert!(
        output.status.success(),
        "skim should exit 0 for a whitespace-only file. stderr: {stderr:?}",
    );

    assert!(
        stdout.chars().all(|c| c.is_whitespace()),
        "stdout must contain no non-whitespace bytes for a whitespace-only file, \
         got: {stdout:?}",
    );

    assert!(
        !stderr.contains("Error"),
        "stderr must not contain 'Error' for a whitespace-only file, got: {stderr:?}",
    );
}
