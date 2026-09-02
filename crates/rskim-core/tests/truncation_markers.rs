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

/// Transform helper — mirrors snap() in truncation_golden.rs.
fn xform(source: &str, language: Language, config: TransformConfig) -> String {
    rskim_core::transform_with_config(source, language, &config)
        .expect("transform must succeed for fixture inputs")
}

/// Count non-marker content lines.
#[allow(dead_code)] // retained for tests that assert content-vs-marker composition
fn content_lines(output: &str) -> Vec<&str> {
    output
        .lines()
        .filter(|l| {
            // A truncation marker contains "truncated" or "lines above" (last_lines variant).
            !l.contains("truncated") && !l.contains("lines above")
        })
        .collect()
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

/// `--max-lines N` = at most N lines TOTAL, marker included (b5507ad / ADR-002).
/// It backs the `head -N` rewrite, and `head -N` emits at most N lines — a bound
/// the tool can exceed is not a bound. So a truncating run emits N-1 content
/// lines plus one elision marker.
#[test]
fn test_pseudo_max_lines_n_emits_n_total_lines_rust() {
    // RUST_SIMPLE has 34 source lines; pseudo output has more than 20 lines.
    let n = 20_usize;
    let out = xform(
        RUST_SIMPLE,
        Language::Rust,
        TransformConfig::with_mode(Mode::Pseudo).with_max_lines(n),
    );
    assert!(
        out.lines().count() <= n,
        "pseudo + --max-lines={n} must not exceed {n} lines total (got {}).\n\
         Full output:\n{out}",
        out.lines().count()
    );
    assert!(
        out.contains("truncated"),
        "pseudo + --max-lines={n} elided content and must disclose it.\n\
         Full output:\n{out}"
    );
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
        assert!(
            out.lines().count() <= n,
            "pseudo + --max-lines={n} must not exceed {n} lines total (got {}).\n\
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
        assert!(
            out.lines().count() <= n,
            "minimal + --max-lines={n} must not exceed {n} lines total (got {}).\n\
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
