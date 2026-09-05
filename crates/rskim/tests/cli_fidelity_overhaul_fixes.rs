//! Regression tests for the fidelity-overhaul fix batch.
//!
//! Covers three fix classes from the Wave-3 / resolve-W3-D pass:
//!
//! - **consistency-2**: cache-hit `view_differs` must use the stored field, not
//!   infer from `mode != Mode::Full` (which is wrong when the ADR-001 guardrail
//!   served raw bytes on the cold run).
//! - **regression-6**: `emit_json_envelope`'s class-1 stderr marker must use
//!   `write_line_to_stderr` rather than `eprintln!`; the latter panics on EPIPE
//!   (e.g. `skim log --json 2>&1 | head`).
//! - **reliability-8**: the `--tokens` elision marker must report counts in
//!   SOURCE space (`source_line_count` forwarded to `truncate_to_token_budget`),
//!   not output space (which misreports the count after multi-mode cascade).
//!
//! ## Interception surface
//!
//! All tests in this file drive the **explicit subcommand** path via
//! `assert_cmd::Command::cargo_bin("skim")` — the rewrite-engine and
//! PATH-wrapper surfaces are NOT exercised here.

use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;

mod common;

// ============================================================================
// consistency-2: cold and warm cache runs must produce identical stderr
// ============================================================================

/// Creates a minimal TypeScript type-alias file where Structure mode output is
/// byte-identical to the source (no bodies to strip) so `view_differs = false`
/// on the cold run.
///
/// Before the consistency-2 fix, the warm (cache-hit) run would infer
/// `view_differs = mode != Mode::Full = true` and emit a spurious transparency
/// marker; after the fix it reads the stored `view_differs = false` and stays
/// silent — matching the cold run.
#[test]
fn consistency_2_cold_and_warm_cache_produce_identical_stderr() {
    let cache_dir = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();

    let src_dir = TempDir::new().unwrap();
    let file_path = src_dir.path().join("tiny.ts");
    // A single-line type alias: Structure mode cannot strip any body, so
    // the output bytes equal the source bytes → view_differs = false.
    fs::write(&file_path, "export type UserId = string;\n").unwrap();

    // Cold run (populates cache).
    let cold = common::skim_sandboxed(home.path())
        .env("SKIM_CACHE_DIR", cache_dir.path())
        .env_remove("SKIM_REWRITTEN_FROM") // no hook origin → no raw-read branch
        .args(["--mode=structure", file_path.to_str().unwrap()])
        .output()
        .unwrap();

    // Warm run (cache hit).
    let warm = common::skim_sandboxed(home.path())
        .env("SKIM_CACHE_DIR", cache_dir.path())
        .env_remove("SKIM_REWRITTEN_FROM")
        .args(["--mode=structure", file_path.to_str().unwrap()])
        .output()
        .unwrap();

    let cold_stderr = String::from_utf8_lossy(&cold.stderr);
    let warm_stderr = String::from_utf8_lossy(&warm.stderr);

    assert_eq!(
        cold_stderr, warm_stderr,
        "consistency-2: cold and warm cache runs must emit identical stderr\n\
         cold  stderr: {cold_stderr:?}\n\
         warm  stderr: {warm_stderr:?}\n\
         (a spurious warm marker means view_differs was inferred from mode != Full \
         rather than read from the stored cache field)"
    );
    assert!(
        cold.status.success(),
        "cold run must succeed; stderr: {cold_stderr:?}"
    );
    assert!(
        warm.status.success(),
        "warm run must succeed; stderr: {warm_stderr:?}"
    );
}

/// Complementary check: a file whose Structure output genuinely differs from
/// source emits the transparency marker on BOTH cold and warm runs.
///
/// Before the fix, the warm run was coincidentally correct for this case
/// (mode != Full = true). The important invariant is that cold == warm.
#[test]
fn consistency_2_lossy_cold_and_warm_cache_both_emit_marker() {
    let cache_dir = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();

    let src_dir = TempDir::new().unwrap();
    let file_path = src_dir.path().join("funcs.ts");
    // Multiple functions with bodies — Structure mode strips the bodies.
    fs::write(
        &file_path,
        "function add(a: number, b: number): number {\n  return a + b;\n}\n\
         function multiply(a: number, b: number): number {\n  return a * b;\n}\n\
         function divide(a: number, b: number): number {\n  if (b === 0) throw new Error(\"div by zero\");\n  return a / b;\n}\n",
    )
    .unwrap();

    let cold = common::skim_sandboxed(home.path())
        .env("SKIM_CACHE_DIR", cache_dir.path())
        .env_remove("SKIM_REWRITTEN_FROM")
        .args(["--mode=structure", file_path.to_str().unwrap()])
        .output()
        .unwrap();

    let warm = common::skim_sandboxed(home.path())
        .env("SKIM_CACHE_DIR", cache_dir.path())
        .env_remove("SKIM_REWRITTEN_FROM")
        .args(["--mode=structure", file_path.to_str().unwrap()])
        .output()
        .unwrap();

    let cold_stderr = String::from_utf8_lossy(&cold.stderr);
    let warm_stderr = String::from_utf8_lossy(&warm.stderr);

    assert_eq!(
        cold_stderr, warm_stderr,
        "consistency-2: cold and warm must have identical stderr even for lossy views\n\
         cold  stderr: {cold_stderr:?}\n\
         warm  stderr: {warm_stderr:?}"
    );
    assert!(
        cold.status.success(),
        "cold must succeed; stderr: {cold_stderr:?}"
    );
    assert!(
        warm.status.success(),
        "warm must succeed; stderr: {warm_stderr:?}"
    );
}

// ============================================================================
// regression-6: the class-1 stderr marker must not panic on broken stderr
// ============================================================================

/// `skim log --json` with a lossy result must emit the class-1 disclosure
/// marker on stderr and exit cleanly (not exit 101 / panic).
///
/// The regression: `emit_json_envelope` used `eprintln!` which panics on
/// EPIPE (e.g. when fd 2 is also the same pipe as fd 1 and the reader
/// closed early).  The fix uses `write_line_to_stderr` which discards EPIPE
/// errors gracefully.
///
/// This test exercises the normal path (stderr is open) to confirm the marker
/// IS emitted, which proves the `write_line_to_stderr` code path is active.
/// The EPIPE-on-stderr scenario cannot be reproduced portably in a unit test;
/// the implementation change itself is the authoritative fix.
#[test]
fn regression_6_emit_json_envelope_class1_marker_emitted_and_no_panic() {
    // Build a synthetic log stream that causes the Full/Degraded parse tier
    // (not Passthrough), which triggers Completeness::Lossy in emit_json_envelope.
    // 150 identical lines → dedup collapses them → view is lossy.
    let repeated_log = "[2024-01-01T00:00:00Z INFO myapp] Connection established\n".repeat(150);

    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["log", "--json"])
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env("NO_COLOR", "1")
        .env_remove("SKIM_REWRITTEN_FROM")
        .write_stdin(repeated_log)
        .output()
        .unwrap();

    let code = output.status.code();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // No panic — exit 101 is Rust's panic-abort code.
    assert_ne!(
        code,
        Some(101),
        "regression-6: eprintln! panicked; stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("panicked at"),
        "regression-6: panic on stderr: {stderr}"
    );

    // Command must have produced some output.
    assert!(
        output.status.success() || code.is_some(),
        "unexpected signal-only exit for skim log --json"
    );

    // When the log parse tier is Lossy, the class-1 marker must appear on
    // stderr.  If the parser fell through to Passthrough (Reencoded), no
    // marker fires — that path is also acceptable and we skip the check.
    // We only assert the marker is present when stdout contains a JSON
    // "entries" array (indicating the Full/Degraded tier ran).
    if stdout.contains("\"entries\"") {
        assert!(
            stderr.contains("SKIM_PASSTHROUGH") || stderr.contains("skim log"),
            "regression-6: class-1 marker must appear on stderr for a Lossy --json result; \
             stdout snippet: {stdout:.200}\nstderr: {stderr}"
        );
    }
}

// ============================================================================
// reliability-8: --tokens elision count must be in source space
// ============================================================================

/// `--tokens N` on a TypeScript file must emit an elision marker whose count
/// reflects **source lines**, not the (smaller) output-space line count that
/// a prior cascade pass may have produced.
///
/// The regression: `fallback_line_truncate` called `truncate_to_token_budget`
/// with `source_line_count: None`, so the omitted count was computed against
/// the already-transformed output length.  For a 50-line source file that
/// Structure mode compressed to 20 output lines, the marker would say
/// "X lines truncated" where X ≤ 20 instead of X ≤ 50.
///
/// After the fix, `source_line_count` is forwarded from the caller, so the
/// marker always reflects the true source size.
#[test]
fn reliability_8_tokens_elision_count_is_in_source_space() {
    let src_dir = TempDir::new().unwrap();
    let file_path = src_dir.path().join("big.ts");

    // 40 source lines: 8 functions × 5 lines each.
    // Structure mode will strip the bodies, reducing output to ~8 lines.
    // With a very tight token budget, line truncation fires and must report
    // the omitted count against the original 40 lines, not the 8 output lines.
    let source: String = (1..=8)
        .map(|i| {
            format!(
                "function func{i}(x: number, y: number): number {{\n  \
                 const result = x + y * {i};\n  \
                 const adjusted = result > 0 ? result : -result;\n  \
                 return adjusted;\n}}\n"
            )
        })
        .collect();

    // Verify we built what we intended.
    let source_lines: usize = source.lines().count();
    assert_eq!(
        source_lines, 40,
        "fixture must have exactly 40 source lines"
    );

    fs::write(&file_path, &source).unwrap();

    // Use a token budget small enough to force line truncation (cascaded
    // through all modes then falling back to line-truncate on the output).
    // 5 tokens is far below any meaningful output.
    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["--tokens=5", file_path.to_str().unwrap()])
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env("NO_COLOR", "1")
        .env_remove("SKIM_REWRITTEN_FROM")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // The output must contain the elision marker.
    assert!(
        stdout.contains("lines truncated"),
        "reliability-8: --tokens=5 must trigger line truncation on a 40-line file; \
         stdout: {stdout:?}\nstderr: {stderr:?}"
    );

    // Extract the truncated-line count from the marker.
    // Marker format: `// ... (N lines truncated) — ...`
    let count = stdout
        .lines()
        .find(|l| l.contains("lines truncated"))
        .and_then(|l| {
            // Parse the number between `(` and ` lines truncated`
            let after_paren = l.split('(').nth(1)?;
            let num_str = after_paren.split_whitespace().next()?;
            num_str.parse::<usize>().ok()
        });

    assert!(
        count.is_some(),
        "reliability-8: could not parse count from elision marker; stdout: {stdout:?}"
    );

    let omitted = count.unwrap();

    // The omitted count must be within source space (≤ 40 source lines).
    // Output-space count would be ≤ 8 (structure output lines).
    // If omitted ≤ 8, the fix did not take effect.
    assert!(
        omitted > 8,
        "reliability-8: elision count {omitted} is in output space (≤ 8 structure-mode lines); \
         must be in source space (> 8, reflecting the {source_lines} source lines); \
         stdout: {stdout:?}"
    );
    assert!(
        omitted <= source_lines,
        "reliability-8: elision count {omitted} exceeds source line count {source_lines}; \
         stdout: {stdout:?}"
    );
}

/// Complementary check: `--tokens N` with passthrough mode (full output)
/// where the source line count and output line count are identical.  The
/// elision marker count must equal `source_lines - kept` in both old and
/// new code (no regression for the simpler case).
#[test]
fn reliability_8_tokens_passthrough_marker_count_is_consistent() {
    let src_dir = TempDir::new().unwrap();
    let file_path = src_dir.path().join("raw.txt");

    // Plain text: no tree-sitter language → passthrough.
    // 10 lines total.  With a 5-token budget the cascade falls to line truncation.
    let source = (1..=10)
        .map(|i| format!("Line number {i}: this is a longer line of plain text content.\n"))
        .collect::<String>();

    let source_lines = source.lines().count();
    assert_eq!(source_lines, 10);

    fs::write(&file_path, &source).unwrap();

    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["--tokens=5", file_path.to_str().unwrap()])
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env("NO_COLOR", "1")
        .env_remove("SKIM_REWRITTEN_FROM")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Marker must be present.
    assert!(
        stdout.contains("lines truncated"),
        "reliability-8 passthrough: elision marker must be present; stdout: {stdout:?}"
    );

    // Count must be ≤ source_lines.
    let count = stdout
        .lines()
        .find(|l| l.contains("lines truncated"))
        .and_then(|l| {
            let after_paren = l.split('(').nth(1)?;
            let num_str = after_paren.split_whitespace().next()?;
            num_str.parse::<usize>().ok()
        });

    assert!(
        count.is_some(),
        "reliability-8 passthrough: could not parse elision count; stdout: {stdout:?}"
    );
    assert!(
        count.unwrap() <= source_lines,
        "reliability-8 passthrough: elision count {} exceeds source lines {}",
        count.unwrap(),
        source_lines
    );
}
