//! RED tests for truncation marker defects — Phase E (#317, ADR-011).
//!
//! These tests assert the CORRECT post-fix behaviour. Before the fixes land
//! they should all FAIL; after E2-E5 they should all PASS. Commit E1 adds
//! them in a failing state adjacent to E2 so no standalone broken revision
//! exists in the history.
//!
//! # Covered defects
//!
//! (a) Missing marker on pseudo/minimal + `--max-lines` (P2/P4, #317).
//! (b) Countless marker on the AST multi-span path (structure/signatures/types).
//! (c) Silent total-loss when `--tokens` budget is too small even for the marker.
//! (d) Off-by-one: pseudo/minimal `--max-lines=N` emits `N-1` content lines
//!     instead of `N` (E4, "marker is line N+1").

#![allow(clippy::unwrap_used, clippy::expect_used)]

use rskim_core::{Language, Mode, TransformConfig, truncate_to_token_budget};

// ---------------------------------------------------------------------------
// Fixtures — same files used by truncation_golden.rs
// ---------------------------------------------------------------------------

const RUST_SIMPLE: &str = include_str!("../../../tests/fixtures/rust/simple.rs");
const PYTHON_SIMPLE: &str = include_str!("../../../tests/fixtures/python/simple.py");
const GO_SIMPLE: &str = include_str!("../../../tests/fixtures/go/simple.go");

// architecture-5 / rust-4 / reliability-8 fixtures
const PYTHON_CLOSE_REOPEN: &str =
    include_str!("../../../tests/fixtures/python/close_reopen_literal.py");
const SQL_CLOSE_REOPEN: &str = include_str!("../../../tests/fixtures/sql/close_reopen_literal.sql");
const TS_CLOSE_REOPEN: &str =
    include_str!("../../../tests/fixtures/typescript/close_reopen_literal.ts");
const PYTHON_DEGENERATE: &str =
    include_str!("../../../tests/fixtures/python/degenerate_literal.py");

/// Transform helper — mirrors snap() in truncation_golden.rs.
fn xform(source: &str, language: Language, config: TransformConfig) -> String {
    rskim_core::transform_with_config(source, language, &config)
        .expect("transform must succeed for fixture inputs")
}

// ============================================================================
// (a) Missing marker on pseudo/minimal + --max-lines
// ============================================================================

/// Defect: pseudo + --max-lines emits 0 markers (P2/P4).
/// Fix: emit one trailing elision marker with an accurate count.
#[test]
fn test_pseudo_max_lines_emits_trailing_marker_rust() {
    let out = xform(
        RUST_SIMPLE,
        Language::Rust,
        TransformConfig::with_mode(Mode::Pseudo).with_max_lines(5),
    );
    assert!(
        out.lines().any(|l| l.contains("truncated")),
        "pseudo + --max-lines must emit a truncation marker.\n\
         Got (no marker):\n{out}"
    );
}

#[test]
fn test_pseudo_max_lines_emits_trailing_marker_python() {
    let out = xform(
        PYTHON_SIMPLE,
        Language::Python,
        TransformConfig::with_mode(Mode::Pseudo).with_max_lines(5),
    );
    assert!(
        out.lines().any(|l| l.contains("truncated")),
        "pseudo + --max-lines must emit a truncation marker (Python).\n\
         Got:\n{out}"
    );
}

#[test]
fn test_pseudo_max_lines_emits_trailing_marker_go() {
    let out = xform(
        GO_SIMPLE,
        Language::Go,
        TransformConfig::with_mode(Mode::Pseudo).with_max_lines(5),
    );
    assert!(
        out.lines().any(|l| l.contains("truncated")),
        "pseudo + --max-lines must emit a truncation marker (Go).\n\
         Got:\n{out}"
    );
}

#[test]
fn test_minimal_max_lines_emits_trailing_marker() {
    let out = xform(
        RUST_SIMPLE,
        Language::Rust,
        TransformConfig::with_mode(Mode::Minimal).with_max_lines(5),
    );
    assert!(
        out.lines().any(|l| l.contains("truncated")),
        "minimal + --max-lines must emit a truncation marker.\n\
         Got:\n{out}"
    );
}

// ============================================================================
// (b) Countless marker on AST multi-span path
// ============================================================================

/// Structure mode's `// ... (truncated)` gap marker must carry a count.
/// Currently it emits "// ... (truncated)" with no number; after E2 it must
/// include digits so agents know how much is missing.
#[test]
fn test_structure_max_lines_gap_marker_has_count() {
    let out = xform(
        RUST_SIMPLE,
        Language::Rust,
        TransformConfig::with_mode(Mode::Structure).with_max_lines(5),
    );
    // Every marker line that contains "truncated" must also contain at least one digit.
    let marker_lines: Vec<&str> = out
        .lines()
        .filter(|l| l.contains("truncated") || l.contains("lines above"))
        .collect();
    assert!(
        !marker_lines.is_empty(),
        "structure + --max-lines must produce at least one marker: {out}"
    );
    for marker in &marker_lines {
        assert!(
            marker.chars().any(|c| c.is_ascii_digit()),
            "marker must contain a line count; got: {marker:?}\nFull output:\n{out}"
        );
    }
}

#[test]
fn test_signatures_max_lines_gap_marker_has_count() {
    let out = xform(
        RUST_SIMPLE,
        Language::Rust,
        TransformConfig::with_mode(Mode::Signatures).with_max_lines(3),
    );
    let marker_lines: Vec<&str> = out
        .lines()
        .filter(|l| l.contains("truncated") || l.contains("lines above"))
        .collect();
    assert!(
        !marker_lines.is_empty(),
        "signatures + --max-lines must produce at least one marker: {out}"
    );
    for marker in &marker_lines {
        assert!(
            marker.chars().any(|c| c.is_ascii_digit()),
            "marker must contain a line count; got: {marker:?}\nFull output:\n{out}"
        );
    }
}

// ============================================================================
// (c) Silent total-loss when --tokens budget is too small even for the marker
// ============================================================================

/// `truncate_to_token_budget` must NEVER return an empty string (#317).
/// When even the marker exceeds the budget, it must still emit the marker
/// (fail loud, never silent).
#[test]
fn test_token_budget_extreme_small_never_returns_empty_string() {
    // Budget = 1 token, but the marker "// ... (3 lines truncated)" is ~5 tokens.
    // Before the fix: returns Ok("") — silent total data loss.
    let text = "line one\nline two\nline three\n";
    let result = truncate_to_token_budget(
        text,
        Language::Rust,
        1,
        |s: &str| s.split_whitespace().count(),
        None,
        None,
        None,
    )
    .expect("truncation must not error");
    assert!(
        !result.is_empty(),
        "truncate_to_token_budget must never return an empty string (silent loss);\n\
         got empty output for input:\n{text}"
    );
    // The emitted token must be the marker.
    assert!(
        result.contains("truncated"),
        "the non-empty output must be (or contain) the truncation marker;\n\
         got: {result:?}"
    );
}

#[test]
fn test_token_budget_zero_budget_never_returns_empty() {
    let text = "word1\nword2\nword3\n";
    let result = truncate_to_token_budget(
        text,
        Language::Python,
        0,
        |s: &str| s.split_whitespace().count(),
        None,
        None,
        None,
    )
    .expect("truncation must not error");
    assert!(
        !result.is_empty(),
        "budget=0 must still emit the truncation marker, not empty string;\n\
         got: {result:?}"
    );
}

// ============================================================================
// (d) Off-by-one: pseudo --max-lines=N must emit N content lines (not N-1)
// ============================================================================

/// `--max-lines N` = at most N lines TOTAL, marker included (ADR-016).
/// It backs the `head -N` rewrite, and `head -N` emits at most N lines — a bound
/// the tool can exceed is not a bound. So a truncating run emits N-1 content
/// lines plus one elision marker.
///
/// These are LINE-BASED modes (pseudo/minimal), where `simple_line_truncate`
/// emits exactly N lines once truncation fires. The assertion is therefore an
/// equality, not an upper bound: `<=` alone is one-sided and is satisfied by an
/// over-truncating regression that returns 2 lines under `--max-lines 20`
/// (PF-025 rule 9). AST multi-span modes legitimately emit fewer than N and
/// keep `<=` elsewhere in this file.
#[test]
fn test_pseudo_max_lines_n_emits_n_total_lines_rust() {
    // RUST_SIMPLE has 34 source lines; pseudo output has more than 20 lines.
    let n = 20_usize;
    let out = xform(
        RUST_SIMPLE,
        Language::Rust,
        TransformConfig::with_mode(Mode::Pseudo).with_max_lines(n),
    );
    let total_pseudo = xform(
        RUST_SIMPLE,
        Language::Rust,
        TransformConfig::with_mode(Mode::Pseudo),
    );
    if total_pseudo.lines().count() > n {
        assert_eq!(
            out.lines().count(),
            n,
            "ADR-016: pseudo + --max-lines={n} must emit exactly {n} lines TOTAL \
             when truncating (got {}).\n\
             Full output:\n{out}",
            out.lines().count()
        );
        assert!(
            out.contains("truncated"),
            "pseudo + --max-lines={n} elided content and must disclose it.\n\
             Full output:\n{out}"
        );
    }
}

#[test]
fn test_pseudo_max_lines_n_emits_n_total_lines_go() {
    // GO_SIMPLE pseudo output has 15+ lines.
    let n = 5_usize;
    let out = xform(
        GO_SIMPLE,
        Language::Go,
        TransformConfig::with_mode(Mode::Pseudo).with_max_lines(n),
    );
    let total_pseudo = xform(
        GO_SIMPLE,
        Language::Go,
        TransformConfig::with_mode(Mode::Pseudo),
    );
    // Only run the assertion when the pseudo output exceeds the budget.
    if total_pseudo.lines().count() > n {
        assert_eq!(
            out.lines().count(),
            n,
            "ADR-016: pseudo + --max-lines={n} must emit exactly {n} lines TOTAL \
             when truncating (got {}).\n\
             Full output:\n{out}",
            out.lines().count()
        );
        assert!(
            out.contains("truncated"),
            "pseudo + --max-lines={n} elided content and must disclose it.\n\
             Full output:\n{out}"
        );
    }
}

#[test]
fn test_minimal_max_lines_n_emits_n_total_lines() {
    let n = 5_usize;
    let out = xform(
        RUST_SIMPLE,
        Language::Rust,
        TransformConfig::with_mode(Mode::Minimal).with_max_lines(n),
    );
    let total_minimal = xform(
        RUST_SIMPLE,
        Language::Rust,
        TransformConfig::with_mode(Mode::Minimal),
    );
    if total_minimal.lines().count() > n {
        assert_eq!(
            out.lines().count(),
            n,
            "ADR-016: minimal + --max-lines={n} must emit exactly {n} lines TOTAL \
             when truncating (got {}).\n\
             Full output:\n{out}",
            out.lines().count()
        );
        assert!(
            out.contains("truncated"),
            "minimal + --max-lines={n} elided content and must disclose it.\n\
             Full output:\n{out}"
        );
    }
}

// ============================================================================
// Marker accuracy: the stated count must match reality
// ============================================================================

/// When pseudo + --max-lines truncates, the stated count in the marker must
/// be a positive integer and must not exceed the source line count.
#[test]
fn test_pseudo_max_lines_marker_count_is_positive_and_plausible() {
    let n = 5_usize;
    let source_lines = RUST_SIMPLE.lines().count();
    let out = xform(
        RUST_SIMPLE,
        Language::Rust,
        TransformConfig::with_mode(Mode::Pseudo).with_max_lines(n),
    );
    let marker = out
        .lines()
        .find(|l| l.contains("truncated"))
        .expect("must contain a truncation marker after fix");

    // Extract the first run of digits from the marker.
    let count_str: String = marker.chars().filter(|c| c.is_ascii_digit()).collect();
    assert!(
        !count_str.is_empty(),
        "marker must contain a digit; got: {marker:?}"
    );
    let stated: usize = count_str.parse().unwrap_or(0);
    assert!(
        stated > 0,
        "stated count must be > 0; got {stated} in {marker:?}"
    );
    assert!(
        stated <= source_lines,
        "stated count ({stated}) must not exceed source line count ({source_lines}); \
         marker: {marker:?}"
    );
}
// ============================================================================
// (e) Literal boundaries — the cut must not land inside a string literal (#511)
// ============================================================================

/// 200 lines of filler around two template literals: lines 38-44 (in reach of a
/// `--max-lines` cut) and lines 160-167 (in reach of a `--last-lines` window).
const TS_MULTILINE_LITERAL: &str =
    include_str!("../../../tests/fixtures/typescript/multiline_literal.ts");

/// End-to-end through `transform_with_config`: `--max-lines 40` used to keep
/// line 38's opening backtick with no closer, so the elision marker — and every
/// line an agent read after it — was the tail of a template literal.
///
/// Measured at e48f977 (`skim … --mode full --max-lines 40`): 40 lines,
/// 1 backtick, `// ... (161 lines truncated)`. Required: the window pulls back
/// to line 37, so the output is 38 lines with balanced backticks and a marker
/// counting the 163 source lines the agent cannot see.
#[test]
fn test_max_lines_does_not_cut_inside_template_literal() {
    let out = xform(
        TS_MULTILINE_LITERAL,
        Language::TypeScript,
        TransformConfig::with_mode(Mode::Full).with_max_lines(40),
    );

    let backticks = out.bytes().filter(|byte| *byte == b'`').count();
    assert_eq!(
        backticks % 2,
        0,
        "--max-lines must not leave a template literal open ({backticks} backticks):\n{out}"
    );
    assert!(
        out.lines().count() <= 40,
        "--max-lines 40 is a bound; got {} lines",
        out.lines().count()
    );
    assert_eq!(
        out.lines().next_back().unwrap(),
        "// ... (163 lines truncated)",
        "the marker counts from the pulled-back window:\n{out}"
    );
}

/// PF-019 / #511: the `-n` labels come from a line map that
/// `transform_passthrough_with_line_map` rebuilt arithmetically from `n`
/// (`start_line = source_line_count - (n - 1) + 1`). Once #511 can move the
/// `--last-lines` window forward — out of a multi-line literal — that
/// arithmetic no longer describes the window the truncator produced, and every
/// retained line is labelled with the wrong source line. The map must be
/// derived from the truncator's own start.
///
/// Measured at 9058273 (`skim … --mode full --last-lines 40 -n`): the window
/// starts at source line 162, mid-literal, and is labelled `162` — self-
/// consistent, but the window is wrong. Required: the window starts at source
/// line 168 (past the literal's closer) and the labels follow it there.
#[test]
fn test_last_lines_line_map_follows_the_moved_window() {
    let config = TransformConfig::with_mode(Mode::Full)
        .with_last_lines(40)
        .with_line_numbers(true);
    let (out, _has_errors, line_map, _degraded) =
        rskim_core::transform_with_line_map(TS_MULTILINE_LITERAL, Language::TypeScript, &config)
            .expect("transform must succeed for fixture inputs");

    let map = line_map.expect("line_numbers = true must yield a map");
    let out_lines: Vec<&str> = out.lines().collect();
    let source_lines: Vec<&str> = TS_MULTILINE_LITERAL.lines().collect();

    assert_eq!(map.len(), out_lines.len(), "one label per output line");
    assert_eq!(map[0], 0, "the marker line carries no annotation");

    // The invariant, wherever the window ends up: a labelled line must BE the
    // source line its label names.
    for (output_line, label) in out_lines.iter().zip(&map).skip(1) {
        let labelled = label.checked_sub(1).and_then(|i| source_lines.get(i));
        assert_eq!(
            labelled.copied(),
            Some(*output_line),
            "output line labelled {label} is not source line {label}"
        );
    }

    // ...and the window is the one #511 produces: it begins after the tail
    // literal's closer on line 167, not inside the literal on line 162.
    assert_eq!(map[1], 168, "the window must begin at source line 168");
    assert_eq!(map.last().copied(), Some(200));
}

// ============================================================================
// architecture-5: bounded fixpoint pull-back for close-and-reopen literals
// ============================================================================

/// Python: a line that closes one `"""` and immediately opens another means
/// a single snap leaves the new last retained line inside the PREVIOUS literal.
/// The fixpoint loop must keep snapping until the cut lands in clean state.
///
/// Fixture layout (0-based lines):
///   0: # Clean line A
///   1: """                   ← opens literal A
///   2: Literal A body
///   3: """ + """             ← closes A, opens B on same line
///   4: Literal B body
///   5: """                   ← closes B
///   6: # Clean line B
///   7: x = 1
///
/// With `--max-lines 6` (`content_lines = 5`, `last_retained = 4`):
///   Iteration 1: `open_after(4) = Some(3)` → snap to `content_lines = 3`
///   Iteration 2: `open_after(2) = Some(1)` → snap to `content_lines = 1`
///   Iteration 3: `open_after(0) = None`    → break
/// Result: 1 content line + marker. No unterminated `"""`.
#[test]
fn test_max_lines_fixpoint_close_reopen_python() {
    let out = xform(
        PYTHON_CLOSE_REOPEN,
        Language::Python,
        TransformConfig::with_mode(Mode::Full).with_max_lines(6),
    );
    // No unpaired triple-quote in the output (even count of `"""`)
    let triple_quote_count = out.matches("\"\"\"").count();
    assert_eq!(
        triple_quote_count % 2,
        0,
        "--max-lines must not leave a triple-quote literal open ({triple_quote_count} occurrences):\n{out}"
    );
    // At most 6 total lines (marker included)
    assert!(
        out.lines().count() <= 6,
        "--max-lines 6 must bound total lines; got {}:\n{out}",
        out.lines().count()
    );
}

/// SQL: same close-and-reopen pattern with single-quoted string literals.
#[test]
fn test_max_lines_fixpoint_close_reopen_sql() {
    let out = xform(
        SQL_CLOSE_REOPEN,
        Language::Sql,
        TransformConfig::with_mode(Mode::Full).with_max_lines(5),
    );
    // No unpaired single-quote — even count of non-escaped `'`
    // (SQL uses '' doubling to escape, so count of `'` modulo 2 == 0 means balanced)
    let sq_count = out.matches('\'').count();
    assert_eq!(
        sq_count % 2,
        0,
        "--max-lines must not leave an SQL literal open ({sq_count} single-quotes):\n{out}"
    );
    assert!(
        out.lines().count() <= 5,
        "--max-lines 5 must bound total lines; got {}:\n{out}",
        out.lines().count()
    );
}

/// TypeScript: same close-and-reopen pattern with backtick template literals.
#[test]
fn test_max_lines_fixpoint_close_reopen_typescript() {
    let out = xform(
        TS_CLOSE_REOPEN,
        Language::TypeScript,
        TransformConfig::with_mode(Mode::Full).with_max_lines(6),
    );
    // No unpaired backtick
    let backtick_count = out.bytes().filter(|b| *b == b'`').count();
    assert_eq!(
        backtick_count % 2,
        0,
        "--max-lines must not leave a template literal open ({backtick_count} backticks):\n{out}"
    );
    assert!(
        out.lines().count() <= 6,
        "--max-lines 6 must bound total lines; got {}:\n{out}",
        out.lines().count()
    );
}

// ============================================================================
// rust-4: snap-to-zero must not drop all content when best > 0
// ============================================================================

/// Python top-of-file docstring (`"""` on line 0): previously `truncate_to_token_budget`
/// would set `best = 0` (unguarded snap), producing a marker-only output with zero
/// content lines. The fix (rust-4) guards `if open > 0` so only snaps that leave at
/// least one content line are applied; when `open == 0`, `best` stays unchanged.
///
/// Budget calibration for PYTHON_DEGENERATE (45 lines, `"""` on line 0):
///   word_count(`"""`)                           = 1
///   word_count(`# ... (44 lines truncated)`)    = 5  ("# ... (44 lines truncated)")
///   1-line candidate total                       = 6
///   Budget = 6 → binary search picks best = 1 (fits exactly).
///   open_after(0) = Some(0): the triple-quote on line 0 opens a literal that covers
///   the entire docstring body. OLD code snapped best to 0 (zero content, marker only).
///   FIX: `open == 0` guard keeps best = 1; the opening `"""` line survives.
#[test]
fn test_token_budget_top_of_file_docstring_keeps_content() {
    let word_count = |s: &str| -> usize { s.split_whitespace().count() };
    // Budget = 6: exactly covers the 1-line candidate (1 content + 5-word marker).
    // This forces best = 1 from the binary search and then triggers the snap check
    // (open_after(0) = Some(0)), exercising the rust-4 guard directly.
    let result = truncate_to_token_budget(
        PYTHON_DEGENERATE,
        Language::Python,
        6,
        word_count,
        None,
        None,
        None,
    )
    .expect("truncation must not error");
    // With the rust-4 fix, the triple-quote opening line must survive.
    assert!(
        result.starts_with("\"\"\""),
        "rust-4: the opening triple-quote line must survive the snap-to-zero guard;\n\
         got: {result:?}"
    );
    // The marker must report truncated lines (44 lines elided).
    assert!(
        result.contains("44 lines"),
        "rust-4: marker must report 44 truncated lines (source_total=45, kept=1);\n\
         got: {result:?}"
    );
    // Output is at most 3 lines (1 content + 1 marker + optional trailing newline).
    assert!(
        result.lines().count() <= 3,
        "rust-4: output must be at most 3 lines; got {}:\n{result}",
        result.lines().count()
    );
}

// ============================================================================
// reliability-8: source_line_count makes the elision count source-accurate
// ============================================================================

/// `truncate_to_token_budget` accepts `source_line_count` and uses it for the
/// marker's omitted-line count. When `text` has already been through `--max-lines`
/// it contains a synthetic marker line; without `source_line_count` the count
/// would be measured in output space (wrong). With `source_line_count = Some(k)`
/// the marker reports `k - best` (source-space count, correct).
#[test]
fn test_token_budget_source_line_count_is_used_in_marker() {
    let word_count = |s: &str| -> usize { s.split_whitespace().count() };
    // Simulate --max-lines 2 applied to a 6-line Python file:
    // 1 content line + 1 marker ("# ... (5 lines truncated)")
    let bounded_text = "x = 1\n# ... (5 lines truncated)\n";
    let source_lines = 6usize;

    // Budget forces truncation: marker line alone has several tokens.
    // With source_line_count = None, count comes from bounded_text.lines() = 2.
    let result_none = truncate_to_token_budget(
        bounded_text,
        Language::Python,
        1, // budget smaller than the content + marker → compact marker
        word_count,
        None,
        None,
        None,
    )
    .expect("truncation must not error");

    // With source_line_count = Some(6), count is 6 - best (source-accurate).
    let result_some = truncate_to_token_budget(
        bounded_text,
        Language::Python,
        1, // same budget
        word_count,
        None,
        None,
        Some(source_lines),
    )
    .expect("truncation must not error");

    // None-path: marker counts from bounded_text (2 lines) → says "2 lines truncated"
    assert!(
        result_none.contains("2 lines truncated"),
        "reliability-8: without source_line_count the marker is in output space (2);\n\
         got: {result_none:?}"
    );
    // Some-path: marker counts from source_line_count (6) → says "6 lines truncated"
    assert!(
        result_some.contains("6 lines truncated"),
        "reliability-8: with source_line_count=6 the marker must say 6 lines;\n\
         got: {result_some:?}"
    );
}

// ============================================================================
// rust-9: cut_inside_side pass-through for already-converted ElidedSide variants
// ============================================================================

/// Verify that `--max-lines` applied twice does not relabel an existing
/// `TruncatedInsideFence` / `TruncatedInsideLiteral` marker as the wrong variant.
/// (The underlying fix is in `cut_inside_side`'s `(already, _) => already` arm.)
///
/// This is a round-trip property test: apply `--max-lines` to a Markdown file
/// that *is* a single fenced block, check the marker says "fence" not "literal".
#[test]
fn test_cut_inside_side_fence_not_relabelled_as_literal() {
    // A Markdown file that is entirely one fenced code block — cutting anywhere
    // inside it should produce `TruncatedInsideFence`, not `TruncatedInsideLiteral`.
    let markdown = "```rust\nfn foo() {}\nfn bar() {}\nfn baz() {}\n```\n";
    let out = xform(
        markdown,
        Language::Markdown,
        TransformConfig::with_mode(Mode::Full).with_max_lines(3),
    );
    // A cut inside the fence must say "code fence", not "string literal".
    if out.contains("cut inside") {
        assert!(
            out.contains("code fence"),
            "rust-9: cut inside Markdown fence must say 'code fence', not 'string literal':\n{out}"
        );
        assert!(
            !out.contains("string literal"),
            "rust-9: Markdown fence marker must not say 'string literal':\n{out}"
        );
    }
    // If no cut-inside message, the output simply ends before the fence closes —
    // that is also correct (the fence may fit within max_lines).
}

// ============================================================================
// (f) Source-space elision counts on the AST multi-span path (ADR-011
//     2026-09-24 amendment)
// ============================================================================
//
// The span-based modes (structure, signatures, types) used to build their
// elision markers from the TRANSFORMED line count -- a fact about a text the
// reader never asked for and cannot see. The fix gives each NodeSpan a
// `source_range` and states every marker count in SOURCE-line space.
//
// The `structure` half of that fix is covered by the truncation_golden matrix.
// The `signatures` and `types` halves are NOT: no signatures or types golden in
// that matrix is long enough to truncate at all, so both producers shipped with
// zero executing coverage. Under this repo's no-compiler rule for agents
// (PF-034) untested code is exactly where defects survive, so the two tests
// below drive those producers directly with hand-computed expectations.

/// Extract the elided line count from an elision marker line.
///
/// Every spelling `rskim_core::elision_marker_line` produces is
/// `<prefix> ... (<N> line|lines <side>)[ — <hint>]<suffix>`, where `<side>` is
/// one of SIX values -- and the `above` half of that list is what the previous
/// version of this helper did not know:
///
/// | side text                                | produced by                    |
/// |------------------------------------------|--------------------------------|
/// | `truncated`                              | `--max-lines` / `--tokens`     |
/// | `above`                                  | `--last-lines`                 |
/// | `truncated; cut inside a string literal` | `--max-lines`, #511 fail-safe  |
/// | `truncated; cut inside a code fence`     | idem, Markdown                 |
/// | `above; cut inside a string literal`     | `--last-lines`, #511 fail-safe |
/// | `above; cut inside a code fence`         | idem, Markdown                 |
///
/// Requiring the substring `truncated` therefore returned `None` for all three
/// `above` forms, and [`split_markers_and_content`] counted those markers as
/// EMITTED CONTENT -- an off-by-one in the accounting identity for any caller
/// that reached a `--last-lines` output, silently, in the direction that makes a
/// broken view look correct. The test is now on the side text's first word, which
/// is `truncated` or `above` for every one of the six.
///
/// Returns `None` for ordinary content lines, which is how callers separate
/// emitted content from markers.
fn marker_count(line: &str) -> Option<usize> {
    let after = line.split_once("... (")?.1;
    let (num, rest) = after.split_once(' ')?;
    // Guard against a content line that merely happens to contain "... (".
    // `rest` is `line <side>)…` or `lines <side>)…`; anything else is content.
    let side = rest
        .strip_prefix("lines ")
        .or_else(|| rest.strip_prefix("line "))?;
    if !side.starts_with("truncated") && !side.starts_with("above") {
        return None;
    }
    num.parse::<usize>().ok()
}

/// Split an output into (sum of marker counts, number of emitted content lines).
/// Blank lines count as emitted content -- they are lines the reader can see.
fn split_markers_and_content(out: &str) -> (usize, usize) {
    let mut marker_total = 0usize;
    let mut emitted = 0usize;
    for line in out.lines() {
        match marker_count(line) {
            Some(n) => marker_total += n,
            None => emitted += 1,
        }
    }
    (marker_total, emitted)
}

/// 8 TypeScript functions, each 3 code lines followed by 1 blank line.
///
/// Source layout (0-indexed rows): `fN` starts at row `4*(N-1)`, so
/// f1@0, f2@4, f3@8, f4@12, f5@16, f6@20, f7@24, f8@28 -- 32 source lines.
/// Signatures mode emits one line per function (the body is excluded), so the
/// transformed output is 8 lines and each span shows exactly 1 source line.
fn ts_eight_functions() -> String {
    (1..=8)
        .map(|i| format!("function f{i}(a: number): number {{\n  return a + {i};\n}}\n\n"))
        .collect()
}

/// 6 TypeScript interfaces, each 3 code lines followed by 3 blank lines.
///
/// Source layout (0-indexed rows): `IN` starts at row `6*(N-1)`, so
/// I1@0, I2@6, I3@12, I4@18, I5@24, I6@30 -- 36 source lines.
/// Types mode emits each interface verbatim (3 lines) joined by "\n\n", so the
/// transformed output is 6*3 + 5 synthetic separators = 23 lines. The 3-blank
/// gutter makes the SOURCE gap between interfaces (3 lines) differ from the
/// TRANSFORMED gap (1 synthetic separator), which is what makes this fixture
/// able to tell the two coordinate spaces apart.
fn ts_six_interfaces() -> String {
    (1..=6)
        .map(|i| format!("interface I{i} {{\n  a: number;\n}}\n\n\n\n"))
        .collect()
}

/// 4 TypeScript interfaces, each 3 code lines, with NO gutter between them.
///
/// The discriminating sibling of [`ts_six_interfaces`]: that fixture's 3-blank
/// gutter forces every SOURCE gap to 3, which is what lets it tell the two
/// coordinate spaces apart -- and also what makes it structurally unable to
/// observe the case where the source gap is ZERO.
///
/// Source layout (0-indexed rows): `IN` starts at row `3*(N-1)`, so I1@0, I2@3,
/// I3@6, I4@9 -- 12 source lines, every definition source-ADJACENT to the next.
/// Types mode still joins with "\n\n", so the transformed output is 4*3 + 3
/// synthetic separators = 15 lines: a 1-line TRANSFORMED gap sitting over a
/// 0-line SOURCE gap, at every join.
fn ts_adjacent_interfaces() -> String {
    (1..=4)
        .map(|i| format!("interface I{i} {{\n  a: number;\n}}\n"))
        .collect()
}

/// signatures + --max-lines: the trailing marker counts SOURCE lines.
///
/// Derivation (by hand -- no snapshot, no tooling):
///   * 8 spans, all `function_declaration` (priority 4), transformed 0..1 .. 7..8.
///   * Greedy at max_lines=5 selects spans 0-4 (5 lines); count_markers adds the
///     trailing marker, so 5+1 > 5 and the trim loop drops the highest-position
///     span (span 4). Selection settles on spans 0-3.
///   * 4 content lines are emitted; last_source_end lands on f4's source range
///     end = 13.
///   * Trailing marker = source_total(32) - 13 = 19.
///
/// Pre-fix this marker read `lines.len() - last_end` = 8 - 4 = 4 -- an
/// output-space count, understating the hidden source lines by nearly 5x.
#[test]
fn signatures_max_lines_marker_is_in_source_space() {
    let source = ts_eight_functions();
    assert_eq!(
        source.lines().count(),
        32,
        "fixture must have exactly 32 source lines"
    );

    let out = xform(
        &source,
        Language::TypeScript,
        TransformConfig::with_mode(Mode::Signatures).with_max_lines(5),
    );
    let lines: Vec<&str> = out.lines().collect();

    assert_eq!(
        lines.len(),
        5,
        "ADR-016: --max-lines 5 must yield 5 lines total, marker included.\nGot:\n{out}"
    );
    assert!(
        lines[0].contains("function f1") && lines[3].contains("function f4"),
        "the four highest-position-surviving signatures must be f1..f4.\nGot:\n{out}"
    );
    assert!(
        !out.contains("function f5"),
        "f5 was trimmed and must not appear.\nGot:\n{out}"
    );
    assert!(
        out.contains("// ... (19 lines truncated)"),
        "signatures trailing marker must count SOURCE lines: source_total(32) \
         - last_source_end(13) = 19.\nGot:\n{out}"
    );
    assert!(
        !out.contains("(4 lines truncated)"),
        "4 is the pre-fix OUTPUT-space count (8 output lines - 4 emitted); its \
         presence means the signatures producer never migrated to \
         NodeSpan::with_source.\nGot:\n{out}"
    );
}

/// types + --max-lines: BOTH gap markers and the trailing marker count SOURCE
/// lines, and every source line is accounted for exactly once.
///
/// Derivation (by hand):
///   * 6 spans, all `interface_declaration` (priority 5). Transformed ranges
///     0..3, 4..7, 8..11, 12..15, 16..19, 20..23 -- non-contiguous because types
///     mode inserts one synthetic blank separator between defs.
///   * Greedy at max_lines=12 selects I1-I4 (12 lines); count_markers finds 3
///     gaps + 1 trailing = 4, so 12+4 > 12 and the trim loop drops I4.
///     Selection settles on I1, I2, I3.
///   * Source ranges are 0..3, 6..9, 12..15. Gap markers are therefore
///     6-3 = 3 and 12-9 = 3; the trailing marker is 36-15 = 21.
///
/// Pre-fix those same three markers read 1, 1 and 12: the gaps counted the
/// single SYNTHETIC separator line (which exists in no source file at all) and
/// the tail counted transformed lines.
#[test]
fn types_max_lines_gap_and_trailing_markers_are_in_source_space() {
    let source = ts_six_interfaces();
    assert_eq!(
        source.lines().count(),
        36,
        "fixture must have exactly 36 source lines"
    );

    let out = xform(
        &source,
        Language::TypeScript,
        TransformConfig::with_mode(Mode::Types).with_max_lines(12),
    );

    assert_eq!(
        out.lines().count(),
        12,
        "ADR-016: --max-lines 12 must yield 12 lines total, markers included.\nGot:\n{out}"
    );

    let gap_markers = out
        .lines()
        .filter(|l| l.contains("(3 lines truncated)"))
        .count();
    assert_eq!(
        gap_markers, 2,
        "both gap markers must count the 3 SOURCE lines between interfaces, not \
         the 1 synthetic separator line.\nGot:\n{out}"
    );
    assert!(
        out.contains("// ... (21 lines truncated)"),
        "types trailing marker must count SOURCE lines: source_total(36) \
         - last_source_end(15) = 21.\nGot:\n{out}"
    );
    assert!(
        !out.contains("(1 line truncated)"),
        "'1 line truncated' is the pre-fix gap value -- the width of the synthetic \
         separator that types mode inserts, which corresponds to no source line \
         whatsoever.\nGot:\n{out}"
    );

    // The accounting identity holds exactly for this case: types spans are
    // separated in transformed space, so every gap fires a marker and no hidden
    // source line escapes disclosure.
    let (marker_total, emitted) = split_markers_and_content(&out);
    let reconstructed = marker_total + emitted;
    assert_eq!(
        reconstructed, 36,
        "every source line must be either SHOWN or counted in a marker.\n  \
         reconstructed = {reconstructed} (markers {marker_total} + emitted {emitted})\n  \
         expected      = 36 source lines\nGot:\n{out}"
    );
}

/// types + --max-lines over SOURCE-ADJACENT definitions: no marker may claim that
/// zero lines are missing.
///
/// This is the case [`types_max_lines_gap_and_trailing_markers_are_in_source_space`]
/// structurally cannot see. Its fixture's 3-blank gutter makes every source gap 3,
/// so the source count and the transformed count are both non-zero and the bug
/// hides. Remove the gutter and the two disagree at the only value that matters:
///
///   * spans transformed `0..3, 4..7, 8..11, 12..15` (the `\n\n` join inserts a
///     synthetic separator that belongs to NO source line), source
///     `0..3, 3..6, 6..9, 9..12`.
///   * the transformed gap is 1 at every join; the SOURCE gap is 0 at every join.
///
/// Pre-fix, presence was decided in transformed space (`start > last_end`) while
/// the count was already in source space, so the builder emitted
/// `// ... (0 lines truncated) — SKIM_PASSTHROUGH=1 for full output` twice: an
/// ADR-011 class-1 disclosure that discloses nothing, spending 2 of the 12 lines
/// ADR-016 allows on a claim of zero. This is the shape PF-033 rule 4 names -- a
/// count that is right about a gap the predicate got wrong -- and it is a
/// REGRESSION, not an inherited wart: before the counts moved to source space
/// those two markers read `(1 line truncated)`, which was wrong about the space
/// but at least disclosed the separator it had counted.
///
/// The accounting identity cannot catch it on its own (9 emitted + 0 + 0 + 3 = 12
/// reconciles perfectly), which is why the zero-claim assertion is stated
/// directly.
#[test]
fn types_max_lines_never_claims_zero_lines_truncated() {
    let source = ts_adjacent_interfaces();
    assert_eq!(
        source.lines().count(),
        12,
        "fixture must have exactly 12 source lines"
    );

    let out = xform(
        &source,
        Language::TypeScript,
        TransformConfig::with_mode(Mode::Types)
            .with_max_lines(12)
            .with_elision_hint("SKIM_PASSTHROUGH=1 for full output"),
    );

    assert!(
        !out.contains("(0 line"),
        "a marker that discloses nothing is an ADR-016 budget line spent on a \
         false claim: presence must follow the SOURCE count, not the transformed \
         gap.\nGot:\n{out}"
    );
    assert_eq!(
        out.lines().count(),
        12,
        "ADR-016: --max-lines 12 must still yield at most 12 lines total, markers \
         included.\nGot:\n{out}"
    );
    assert!(
        out.contains("interface I1") && out.contains("interface I3"),
        "suppressing the empty markers must return their budget lines to CONTENT, \
         not strand them.\nGot:\n{out}"
    );

    // The identity is the independent check on the count that survives: whatever
    // the trim loop settles on, every source line is either shown or counted.
    let (marker_total, emitted) = split_markers_and_content(&out);
    let reconstructed = marker_total + emitted;
    assert_eq!(
        reconstructed, 12,
        "every source line must be either SHOWN or counted in a marker.\n  \
         reconstructed = {reconstructed} (markers {marker_total} + emitted {emitted})\n  \
         expected      = 12 source lines\nGot:\n{out}"
    );
}

/// signatures + --max-lines: the source lines the view discloses NOWHERE, pinned
/// to the exact amount so the gap is tracked rather than invisible.
///
/// This is the self-retiring exclusion for the second half of the coordinate
/// migration, in the same shape as `ACCOUNTING_EXCLUSIONS` below: an explicit
/// named number rather than a tolerance band, so fixing the defect turns THIS
/// test red and forces whoever fixed it to delete the entry.
///
/// # The gap
///
/// `transform_signatures_with_spans_and_line_map` joins signatures with `"\n"` and
/// advances `current_output_line += line_count` with no separator, so consecutive
/// spans are CONTIGUOUS in transformed space. The builder's gap predicate asks
/// `start > last_end`, which is therefore never true for signatures, and the
/// function bodies between two selected signatures are shown nowhere and counted
/// in no marker. Measured on this fixture at `--max-lines 5`: 4 signatures
/// emitted, trailing marker 19, so `4 + 19 = 23` of 32 source lines are accounted
/// for and 9 (the bodies and blanks of f1-f4: rows 1-3, 5-7, 9-11) are not.
///
/// # Why it is tracked and not yet fixed
///
/// Firing the gap marker on a SOURCE gap that the transformed cursor cannot see is
/// the right fix, and it is not landable in isolation: `extract_markdown_headers_with_spans`
/// builds spans that are contiguous in exactly the same way AND understate their
/// rendered output (each header text carries a trailing newline, so 6 headers
/// render 13 output lines while their spans claim 8 -- see the
/// `md_simple_structure_max5` entry in `ACCOUNTING_EXCLUSIONS`). Under the source
/// gap predicate, `md_simple_structure_max5` fires three new gap markers and its
/// view collapses to one header, one blank line and two markers inside the same
/// 5-line budget. The markdown producer has to be repaired first; the two defects
/// share one code path but not one root cause.
#[test]
fn signatures_max_lines_undisclosed_source_lines_are_pinned() {
    // source_line_count - (markers + emitted), as measured today.
    const UNDISCLOSED: usize = 9;
    let documented = UNDISCLOSED;

    let source = ts_eight_functions();
    let source_total = source.lines().count();
    assert_eq!(
        source_total, 32,
        "fixture must have exactly 32 source lines"
    );

    let out = xform(
        &source,
        Language::TypeScript,
        TransformConfig::with_mode(Mode::Signatures).with_max_lines(5),
    );
    let (marker_total, emitted) = split_markers_and_content(&out);
    let reconstructed = marker_total + emitted;
    let shortfall = source_total.saturating_sub(reconstructed);

    assert_eq!(
        shortfall, documented,
        "documented shortfall no longer matches. If you made the gap marker fire \
         on a SOURCE gap the transformed cursor cannot see, delete this test and \
         assert the identity instead. If you did not, the accounting regressed.\n  \
         reconstructed  = {reconstructed} (markers {marker_total} + emitted {emitted})\n  \
         expected       = {source_total} source lines\n  \
         documented gap = {documented}\nGot:\n{out}"
    );
}

// ----------------------------------------------------------------------------
// The accounting identity, pinned across the whole structure golden matrix
// ----------------------------------------------------------------------------
//
// INVARIANT: when --max-lines truncates, every line of the user's source file
// is either SHOWN in the output or counted inside an elision marker. Formally
//
//     sum(marker counts) + emitted output lines == source line count
//
// This is the property the source-space fix establishes, and it is strictly
// stronger than "the numbers changed as expected": a golden records whatever
// the code produced when it was blessed, bugs included, whereas this identity
// states the contract independently of any blessed output.
//
// `emitted` counts blank lines too -- a blank line in the output is a line the
// reader can see.

const TS_SIMPLE: &str = include_str!("../../../tests/fixtures/typescript/simple.ts");
const MD_SIMPLE: &str = include_str!("../../../tests/fixtures/markdown/simple.md");
const RUST_COMMENTS: &str = include_str!("../../../tests/fixtures/rust/comments.rs");
const PYTHON_COMMENTS: &str = include_str!("../../../tests/fixtures/python/comments.py");
const GO_COMMENTS: &str = include_str!("../../../tests/fixtures/go/comments.go");
const TS_COMMENTS: &str = include_str!("../../../tests/fixtures/typescript/comments.ts");

struct StructureCase {
    /// Matches the truncation_golden snapshot name, so a failure here points
    /// straight at the golden that covers the same cell.
    name: &'static str,
    source: &'static str,
    language: Language,
    max_lines: usize,
}

/// Every (fixture x bound) cell of the structure golden matrix that actually
/// truncates. Cells that fit inside their bound emit no marker and are outside
/// this invariant's scope.
const STRUCTURE_CASES: &[StructureCase] = &[
    StructureCase {
        name: "rust_simple_structure_max5",
        source: RUST_SIMPLE,
        language: Language::Rust,
        max_lines: 5,
    },
    StructureCase {
        name: "rust_simple_structure_max15",
        source: RUST_SIMPLE,
        language: Language::Rust,
        max_lines: 15,
    },
    StructureCase {
        name: "rust_comments_structure_max15",
        source: RUST_COMMENTS,
        language: Language::Rust,
        max_lines: 15,
    },
    StructureCase {
        name: "ts_simple_structure_max5",
        source: TS_SIMPLE,
        language: Language::TypeScript,
        max_lines: 5,
    },
    StructureCase {
        name: "ts_comments_structure_max15",
        source: TS_COMMENTS,
        language: Language::TypeScript,
        max_lines: 15,
    },
    StructureCase {
        name: "go_simple_structure_max5",
        source: GO_SIMPLE,
        language: Language::Go,
        max_lines: 5,
    },
    StructureCase {
        name: "go_simple_structure_max15",
        source: GO_SIMPLE,
        language: Language::Go,
        max_lines: 15,
    },
    StructureCase {
        name: "go_comments_structure_max15",
        source: GO_COMMENTS,
        language: Language::Go,
        max_lines: 15,
    },
    StructureCase {
        name: "python_simple_structure_max5",
        source: PYTHON_SIMPLE,
        language: Language::Python,
        max_lines: 5,
    },
    StructureCase {
        name: "python_simple_structure_max15",
        source: PYTHON_SIMPLE,
        language: Language::Python,
        max_lines: 15,
    },
    StructureCase {
        name: "python_comments_structure_max15",
        source: PYTHON_COMMENTS,
        language: Language::Python,
        max_lines: 15,
    },
    StructureCase {
        name: "md_simple_structure_max5",
        source: MD_SIMPLE,
        language: Language::Markdown,
        max_lines: 5,
    },
];

/// Cells that do NOT yet satisfy the identity, each with its cause and the exact
/// shortfall measured today.
///
/// This is deliberately an explicit, named list rather than a tolerance band: a
/// tolerance would also swallow the next regression. Each entry is pinned by
/// `structure_accounting_exclusions_still_fail_by_documented_amount` below, so
/// when either defect is fixed THAT test fails and forces whoever fixed it to
/// delete the entry -- at which point the main assertion tightens over the cell
/// automatically. Neither defect is fixable without changing which lines are
/// shown or whether a marker appears, which is why they are tracked separately.
struct AccountingExclusion {
    name: &'static str,
    /// source_line_count - (sum of markers + emitted lines), as measured today.
    shortfall: usize,
    cause: &'static str,
}

const ACCOUNTING_EXCLUSIONS: &[AccountingExclusion] = &[
    AccountingExclusion {
        name: "md_simple_structure_max5",
        shortfall: 21,
        cause: "extract_markdown_headers_with_spans understates transformed_range: each \
                header_text carries a trailing newline, so texts.join(\"\\n\") renders 13 \
                output lines for 6 headers whose spans claim only 8. The spans therefore \
                claim to cover 4 headers when only 2 are on screen, and the source-space \
                cursor runs ahead of what the reader actually sees.",
    },
    AccountingExclusion {
        name: "ts_simple_structure_max5",
        shortfall: 3,
        cause: "marker PRESENCE is still decided in transformed space. The last selected \
                span ends at the output's final line, so `last_end < lines.len()` is false \
                and no trailing marker fires -- even though source lines 11-13 (the greet \
                function body) are hidden. The count that would have been emitted is \
                correct; the marker simply never fires.",
    },
];

fn structure_accounting(case: &StructureCase) -> (usize, usize, usize) {
    let out = xform(
        case.source,
        case.language,
        TransformConfig::with_mode(Mode::Structure).with_max_lines(case.max_lines),
    );
    let (marker_total, emitted) = split_markers_and_content(&out);
    (marker_total, emitted, case.source.lines().count())
}

/// Every structure cell that truncates accounts for every source line exactly.
#[test]
fn structure_truncation_accounts_for_every_source_line() {
    for case in STRUCTURE_CASES {
        if ACCOUNTING_EXCLUSIONS.iter().any(|e| e.name == case.name) {
            continue;
        }
        let (marker_total, emitted, source_total) = structure_accounting(case);
        let reconstructed = marker_total + emitted;
        let difference = (reconstructed as i64) - (source_total as i64);
        let name = case.name;
        assert_eq!(
            reconstructed, source_total,
            "{name}: source-space accounting is broken. Every source line must be either \
             SHOWN in the output or counted inside an elision marker.\n  \
             reconstructed = {reconstructed} (markers {marker_total} + emitted {emitted})\n  \
             expected      = {source_total} source lines\n  \
             difference    = {difference}"
        );
    }
}

/// Guard: an exclusion name that matches no case would silently exclude nothing
/// (or, after a rename, silently exclude the wrong thing).
#[test]
fn structure_accounting_exclusions_name_real_cases() {
    for excl in ACCOUNTING_EXCLUSIONS {
        let name = excl.name;
        assert!(
            STRUCTURE_CASES.iter().any(|c| c.name == name),
            "exclusion {name:?} names no case in STRUCTURE_CASES -- a typo or a stale \
             rename would make the exclusion a no-op"
        );
    }
}

/// Guard: each excluded cell must STILL fail by exactly its documented amount.
///
/// This is what makes the exclusion list self-retiring. Fix either underlying
/// defect and this test goes red, naming the entry to delete; the main
/// assertion above then covers the cell with no further edit.
#[test]
fn structure_accounting_exclusions_still_fail_by_documented_amount() {
    for excl in ACCOUNTING_EXCLUSIONS {
        let Some(case) = STRUCTURE_CASES.iter().find(|c| c.name == excl.name) else {
            continue; // covered by structure_accounting_exclusions_name_real_cases
        };
        let (marker_total, emitted, source_total) = structure_accounting(case);
        let reconstructed = marker_total + emitted;
        let shortfall = source_total.saturating_sub(reconstructed);
        let difference = (reconstructed as i64) - (source_total as i64);
        let name = case.name;
        let documented = excl.shortfall;
        let cause = excl.cause;
        assert_eq!(
            shortfall, documented,
            "{name}: documented shortfall no longer matches. If you FIXED the underlying \
             defect, delete this entry from ACCOUNTING_EXCLUSIONS -- \
             structure_truncation_accounts_for_every_source_line will then cover this \
             cell. If you did not, the accounting regressed.\n  \
             reconstructed  = {reconstructed} (markers {marker_total} + emitted {emitted})\n  \
             expected       = {source_total} source lines\n  \
             difference     = {difference}\n  \
             documented gap = {documented}\n  \
             cause          = {cause}"
        );
    }
}

// ----------------------------------------------------------------------------
// `--last-lines`: the tail marker's POSITIONAL claim, pinned per golden cell
// ----------------------------------------------------------------------------
//
// # Why this table exists (testing-03)
//
// `git show --stat 8ee640b` -- the commit that moved the `--last-lines` elision
// count into source space -- lists `types.rs` plus four snapshots and NO test
// source, while its `--max-lines` sibling `efac056` added the invariants above.
// The goldens cannot stand in for them: truncation_golden.rs says so in its own
// header ("capture TODAY'S output -- including any known bugs ... They are NOT
// behavioural assertions", "Do not cite 'no snapshots moved' as evidence that a
// change is correct"), and two of the five values it blessed were wrong.
//
// # Why the claim asserted here is POSITIONAL and not the accounting identity
//
// The tail marker reads `(N lines ABOVE)`. That is a statement about a position,
// not a budget: N is the number of source lines that lie above the first source
// line the window shows. The accounting identity `markers + emitted == source`
// is NOT equivalent to it and must not be substituted for it, in either
// direction:
//
//   * it passes on a wrong value -- `md_simple_structure_last10` satisfies
//     28 + 9 == 37 while the first line it shows is source line 9, so the true
//     count is 8. An identity-only test would have blessed the defect.
//   * it FAILS on the right value -- `python_simple_structure_last10` correctly
//     reports 10 above, and 10 + 9 == 19 against 21 source lines. The missing 2
//     are lines the mode collapses INSIDE the retained window (`greet_user`'s
//     body). The tail path has no gap markers at all, so nothing discloses them;
//     that is the `--last-lines` mirror of the signatures gap pinned by
//     `signatures_max_lines_undisclosed_source_lines_are_pinned`, and it is
//     recorded per case in `undisclosed_inside_window` rather than hidden inside
//     a marker count that happens to absorb it.
//
// So: assert the positional claim, and pin the internal shortfall explicitly.

struct LastLinesCase {
    /// Matches the truncation_golden snapshot name, so a failure here points
    /// straight at the golden that covers the same cell.
    name: &'static str,
    source: &'static str,
    language: Language,
    mode: Mode,
    last_lines: usize,
    /// 1-indexed SOURCE line of the first line the retained window shows,
    /// derived by hand from the fixture and the mode's unbounded golden.
    first_shown_source_line: usize,
    /// Source lines inside the retained window that the mode collapses and no
    /// marker mentions. `0` where the window is verbatim.
    undisclosed_inside_window: usize,
}

/// Every `*_last10` golden cell that actually truncates.
///
/// `ts_simple_structure_last10` is absent on purpose: TypeScript's structure
/// output is 8 lines, so `--last-lines 10` does not truncate and emits no marker.
const LAST_LINES_CASES: &[LastLinesCase] = &[
    // Structure output is 28 lines; the window opens on output line 19, the `}`
    // closing `type Computer interface` at source line 26.
    LastLinesCase {
        name: "go_simple_structure_last10",
        source: GO_SIMPLE,
        language: Language::Go,
        mode: Mode::Structure,
        last_lines: 10,
        first_shown_source_line: 26,
        undisclosed_inside_window: 0,
    },
    // Pseudo output is 34 lines -- one per source line -- so the window is the
    // verbatim tail from source line 26.
    LastLinesCase {
        name: "go_simple_pseudo_last10",
        source: GO_SIMPLE,
        language: Language::Go,
        mode: Mode::Pseudo,
        last_lines: 10,
        first_shown_source_line: 26,
        undisclosed_inside_window: 0,
    },
    // Structure output is 26 lines; the window opens on `pub trait Compute {`,
    // source line 26.
    LastLinesCase {
        name: "rust_simple_structure_last10",
        source: RUST_SIMPLE,
        language: Language::Rust,
        mode: Mode::Structure,
        last_lines: 10,
        first_shown_source_line: 26,
        undisclosed_inside_window: 0,
    },
    LastLinesCase {
        name: "rust_simple_pseudo_last10",
        source: RUST_SIMPLE,
        language: Language::Rust,
        mode: Mode::Pseudo,
        last_lines: 10,
        first_shown_source_line: 26,
        undisclosed_inside_window: 0,
    },
    // Structure output is 17 lines; the window opens on output line 8,
    // `def greet_user(name: str) -> str:` at source line 11. Source lines
    // 12-14, 18 and 21 are collapsed to ` {...}` placeholders inside the
    // window: 11 source lines in, 9 output lines out, so 2 are undisclosed.
    LastLinesCase {
        name: "python_simple_structure_last10",
        source: PYTHON_SIMPLE,
        language: Language::Python,
        mode: Mode::Structure,
        last_lines: 10,
        first_shown_source_line: 11,
        undisclosed_inside_window: 2,
    },
    // Pseudo output is 21 lines -- one per source line -- window from line 13.
    LastLinesCase {
        name: "python_simple_pseudo_last10",
        source: PYTHON_SIMPLE,
        language: Language::Python,
        mode: Mode::Pseudo,
        last_lines: 10,
        first_shown_source_line: 13,
        undisclosed_inside_window: 0,
    },
    // Pseudo output is 13 lines -- one per source line -- window from line 5
    // (a blank line, which is still a line the reader sees).
    LastLinesCase {
        name: "ts_simple_pseudo_last10",
        source: TS_SIMPLE,
        language: Language::TypeScript,
        mode: Mode::Pseudo,
        last_lines: 10,
        first_shown_source_line: 5,
        undisclosed_inside_window: 0,
    },
    // Markdown Pseudo is the identity passthrough: 37 output lines, window from
    // source line 29 (`Setext Style H1`).
    LastLinesCase {
        name: "md_simple_pseudo_last10",
        source: MD_SIMPLE,
        language: Language::Markdown,
        mode: Mode::Pseudo,
        last_lines: 10,
        first_shown_source_line: 29,
        undisclosed_inside_window: 0,
    },
    // Structure output is 13 lines; the window opens on `### Subsection 1.1`,
    // source line 9. Source lines 10-37 minus the 9 shown leave 20 undisclosed
    // inside the window. EXCLUDED below -- the marker states 28, not 8.
    LastLinesCase {
        name: "md_simple_structure_last10",
        source: MD_SIMPLE,
        language: Language::Markdown,
        mode: Mode::Structure,
        last_lines: 10,
        first_shown_source_line: 9,
        undisclosed_inside_window: 20,
    },
];

/// Cells whose tail marker is still positionally wrong, each with the value it
/// actually states and why. Same self-retiring contract as
/// `ACCOUNTING_EXCLUSIONS`: fix the cause and
/// `last_lines_exclusions_still_fail_by_documented_amount` goes red, naming the
/// entry to delete, after which the main assertion covers the cell unchanged.
struct LastLinesExclusion {
    name: &'static str,
    /// The count the marker states today.
    actual_above: usize,
    cause: &'static str,
}

const LAST_LINES_EXCLUSIONS: &[LastLinesExclusion] = &[LastLinesExclusion {
    name: "md_simple_structure_last10",
    actual_above: 28,
    cause: "the count is resolved from the transform's line map, and markdown's is \
            the one that cannot answer: extract_markdown_headers_with_spans sets \
            line_count from `text.lines().count()` while each header text carries a \
            trailing newline, so 6 headers render 13 output lines but push only 8 map \
            entries (1 per ATX heading, 2 per setext). The window opens at output \
            line 4, and map[4] is the FIFTH entry -- source line 29, the setext H1 -- \
            rather than source line 9. 29-1 = 28 happens to equal the pre-fix \
            output-space arithmetic, so this cell is byte-unchanged; it is wrong for \
            a different reason now. Same producer defect as the \
            `md_simple_structure_max5` entry in ACCOUNTING_EXCLUSIONS, and it must be \
            fixed in structure.rs, not here.",
}];

fn last_lines_accounting(case: &LastLinesCase) -> (usize, usize, usize) {
    let out = xform(
        case.source,
        case.language,
        TransformConfig::with_mode(case.mode).with_last_lines(case.last_lines),
    );
    let (marker_total, emitted) = split_markers_and_content(&out);
    (marker_total, emitted, case.source.lines().count())
}

/// The tail marker states exactly the number of source lines above the window.
///
/// This is the assertion whose absence let 8ee640b bless two wrong values.
#[test]
fn last_lines_marker_counts_the_source_lines_above_the_window() {
    for case in LAST_LINES_CASES {
        if LAST_LINES_EXCLUSIONS.iter().any(|e| e.name == case.name) {
            continue;
        }
        let (marker_total, _emitted, _source_total) = last_lines_accounting(case);
        let expected = case.first_shown_source_line.saturating_sub(1);
        let name = case.name;
        let first = case.first_shown_source_line;
        assert_eq!(
            marker_total, expected,
            "{name}: `(N lines above)` is a POSITIONAL claim. The window's first \
             line is source line {first}, so exactly {expected} source lines lie \
             above it.\n  \
             stated   = {marker_total}\n  \
             expected = {expected}\n  \
             A value equal to `source_total - (retained output lines)` means the \
             count is being finished in output space again (PF-033 rule 4)."
        );
    }
}

/// ADR-016 tail mirror: `--last-lines N` yields N lines total, marker included.
#[test]
fn last_lines_emits_n_lines_total_marker_included() {
    for case in LAST_LINES_CASES {
        let out = xform(
            case.source,
            case.language,
            TransformConfig::with_mode(case.mode).with_last_lines(case.last_lines),
        );
        let total = out.lines().count();
        let name = case.name;
        let n = case.last_lines;
        assert_eq!(
            total, n,
            "{name}: ADR-016 -- --last-lines {n} must yield {n} lines total, the \
             leading marker included.\nGot {total}:\n{out}"
        );
    }
}

/// Every source line is SHOWN, counted in the tail marker, or named in the
/// case's `undisclosed_inside_window`.
///
/// The third bucket is the tail path's missing inline disclosure, and pinning it
/// is what keeps it from being absorbed silently into the marker count (which is
/// exactly how `python_simple_structure_last10` came to read 12).
#[test]
fn last_lines_accounts_for_every_source_line_or_names_the_gap() {
    for case in LAST_LINES_CASES {
        if LAST_LINES_EXCLUSIONS.iter().any(|e| e.name == case.name) {
            continue;
        }
        let (marker_total, emitted, source_total) = last_lines_accounting(case);
        let reconstructed = marker_total + emitted + case.undisclosed_inside_window;
        let name = case.name;
        let inside = case.undisclosed_inside_window;
        assert_eq!(
            reconstructed, source_total,
            "{name}: every source line must be SHOWN, counted in the tail marker, \
             or named in undisclosed_inside_window.\n  \
             reconstructed = {reconstructed} (markers {marker_total} + emitted \
             {emitted} + undisclosed-inside {inside})\n  \
             expected      = {source_total} source lines"
        );
    }
}

/// Guard: an exclusion name that matches no case would silently exclude nothing.
#[test]
fn last_lines_exclusions_name_real_cases() {
    for excl in LAST_LINES_EXCLUSIONS {
        let name = excl.name;
        assert!(
            LAST_LINES_CASES.iter().any(|c| c.name == name),
            "exclusion {name:?} names no case in LAST_LINES_CASES -- a typo or a \
             stale rename would make the exclusion a no-op"
        );
    }
}

/// Guard: each excluded cell must STILL state exactly its documented value.
#[test]
fn last_lines_exclusions_still_fail_by_documented_amount() {
    for excl in LAST_LINES_EXCLUSIONS {
        let Some(case) = LAST_LINES_CASES.iter().find(|c| c.name == excl.name) else {
            continue; // covered by last_lines_exclusions_name_real_cases
        };
        let (marker_total, _emitted, _source_total) = last_lines_accounting(case);
        let name = case.name;
        let documented = excl.actual_above;
        let truth = case.first_shown_source_line.saturating_sub(1);
        let cause = excl.cause;
        assert_eq!(
            marker_total, documented,
            "{name}: documented value no longer matches. If you FIXED the \
             underlying defect the marker now states {truth}; delete this entry \
             and `last_lines_marker_counts_the_source_lines_above_the_window` \
             will cover the cell. If you did not, the count regressed.\n  \
             stated     = {marker_total}\n  \
             documented = {documented}\n  \
             truth      = {truth}\n  \
             cause      = {cause}"
        );
    }
}

// ----------------------------------------------------------------------------
// A bounded pseudo view must not spend its budget on blank lines
// ----------------------------------------------------------------------------
//
// # The defect
//
// Stripping a module-level comment removes whole lines, so the blank line above
// the comment ends up adjacent to the blank line below it. Neither was written
// next to the other -- the run is removal residue, and since #476 preserved the
// module header in every language it sits directly under that header: the first
// thing a bounded view spends its budget on. `--max-lines 5` over
// `tests/fixtures/typescript/comments.ts` spent TWO of its four content slots on
// blank lines and put no body on screen at all.
//
// # Why "at most one blank" and not "at least one non-comment line"
//
// The review that found this asked for the stronger assertion -- that a bounded
// pseudo view show at least one non-blank, non-comment line. That assertion is
// UNREACHABLE for these fixtures, and writing it would pin a fiction: the first
// body content in `comments.ts` is a six-line JSDoc block that pseudo preserves
// by contract (ADR-007/ADR-008), so `export function add(...)` is the seventh
// line of the body and no blank-line policy fits it into four slots. Recovering
// the slot is the whole of what the fix can do; what it must never do is spend
// the recovered slot on another blank.
//
// So this pins the defect that actually occurred -- budget spent on blanks --
// and pins it as a BOUND (<= 1) rather than an equality, because one blank line
// is the separation the header is entitled to and a fixture whose header run is
// already a single blank has nothing to fold.

/// A five-line pseudo view spends at most one of its slots on a blank line.
///
/// Marker lines are excluded from the count: the trailing elision marker is the
/// disclosure, not content the reader could have had instead.
#[test]
fn bounded_pseudo_view_spends_at_most_one_slot_on_blank_lines() {
    for (name, source, language) in [
        ("ts_comments", TS_COMMENTS, Language::TypeScript),
        ("rust_comments", RUST_COMMENTS, Language::Rust),
        ("python_comments", PYTHON_COMMENTS, Language::Python),
        ("go_comments", GO_COMMENTS, Language::Go),
    ] {
        let out = xform(
            source,
            language,
            TransformConfig::with_mode(Mode::Pseudo).with_max_lines(5),
        );
        let blanks = out
            .lines()
            .filter(|l| !l.contains("truncated"))
            .filter(|l| l.trim().is_empty())
            .count();
        assert!(
            blanks <= 1,
            "{name}: a 5-line pseudo view may spend at most one of its slots on a \
             blank line, spent {blanks}:\n{out}"
        );
    }
}
