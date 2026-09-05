//! Golden byte snapshots for the full (mode × truncation-bound) matrix.
//!
//! # Purpose
//!
//! These tests capture TODAY'S output — including any known bugs — so that
//! future changes that alter truncation behaviour show up as snapshot diffs
//! rather than silent regressions.  They are NOT behavioural assertions; they
//! document the current state.  When a fix deliberately changes output, run
//! `INSTA_UPDATE=always cargo nextest run -p rskim-core` and commit the new
//! snapshots alongside the fix.
//!
//! # What these goldens can and cannot show
//!
//! An unchanged snapshot proves only that output did not move — never that the
//! output is correct, since a golden records whatever the code produced when it
//! was blessed, bugs included, and re-blessing the whole set is one environment
//! variable away.  Do not cite "no snapshots moved" as evidence that a change is
//! correct; that argument needs assertions which state the expected contract
//! (see `truncation_markers.rs`, which pins ADR-016 line bounds directly).
//! These goldens answer a narrower question: did anything change that nobody
//! intended to change?
//!
//! # Coverage
//!
//! 5 languages × 6 modes × 4 bound variants, minus the combinations that are
//! not generated: 76 snapshot cells.
//! Not all cells produce distinct output (e.g. Full mode ignores max_lines);
//! the value comes from the matrix completeness, not the distinctness.
//!
//! Languages: Rust, Python, Go, TypeScript, Markdown
//! Modes:     Structure, Signatures, Types, Minimal, Pseudo, Full
//! Bounds:    unbounded | max_lines=15 | last_lines=10 | max_lines=5

#![allow(clippy::unwrap_used, clippy::expect_used)]

use rskim_core::{Language, Mode, TransformConfig};

// ---------------------------------------------------------------------------
// Fixture sources (included at compile time so fixtures are always in sync)
// ---------------------------------------------------------------------------

const RUST_SIMPLE: &str = include_str!("../../../tests/fixtures/rust/simple.rs");
const PYTHON_SIMPLE: &str = include_str!("../../../tests/fixtures/python/simple.py");
const GO_SIMPLE: &str = include_str!("../../../tests/fixtures/go/simple.go");
const TS_SIMPLE: &str = include_str!("../../../tests/fixtures/typescript/simple.ts");
const MD_SIMPLE: &str = include_str!("../../../tests/fixtures/markdown/simple.md");

// Additional fixtures for broader coverage.
const PYTHON_COMMENTS: &str = include_str!("../../../tests/fixtures/python/comments.py");
const RUST_COMMENTS: &str = include_str!("../../../tests/fixtures/rust/comments.rs");
const GO_COMMENTS: &str = include_str!("../../../tests/fixtures/go/comments.go");
const TS_COMMENTS: &str = include_str!("../../../tests/fixtures/typescript/comments.ts");

// ---------------------------------------------------------------------------
// Helper — run transform and panic with the full output on any error
// ---------------------------------------------------------------------------

fn snap(source: &str, language: Language, config: TransformConfig) -> String {
    rskim_core::transform_with_config(source, language, &config)
        .expect("transform must succeed for fixture inputs")
}

// ---------------------------------------------------------------------------
// Rust — simple.rs
// ---------------------------------------------------------------------------

#[test]
fn rust_simple_structure_unbounded() {
    insta::assert_snapshot!(snap(
        RUST_SIMPLE,
        Language::Rust,
        TransformConfig::with_mode(Mode::Structure)
    ));
}

#[test]
fn rust_simple_signatures_unbounded() {
    insta::assert_snapshot!(snap(
        RUST_SIMPLE,
        Language::Rust,
        TransformConfig::with_mode(Mode::Signatures)
    ));
}

#[test]
fn rust_simple_types_unbounded() {
    insta::assert_snapshot!(snap(
        RUST_SIMPLE,
        Language::Rust,
        TransformConfig::with_mode(Mode::Types)
    ));
}

#[test]
fn rust_simple_minimal_unbounded() {
    insta::assert_snapshot!(snap(
        RUST_SIMPLE,
        Language::Rust,
        TransformConfig::with_mode(Mode::Minimal)
    ));
}

#[test]
fn rust_simple_pseudo_unbounded() {
    insta::assert_snapshot!(snap(
        RUST_SIMPLE,
        Language::Rust,
        TransformConfig::with_mode(Mode::Pseudo)
    ));
}

#[test]
fn rust_simple_full_unbounded() {
    insta::assert_snapshot!(snap(
        RUST_SIMPLE,
        Language::Rust,
        TransformConfig::with_mode(Mode::Full)
    ));
}

#[test]
fn rust_simple_structure_max15() {
    insta::assert_snapshot!(snap(
        RUST_SIMPLE,
        Language::Rust,
        TransformConfig::with_mode(Mode::Structure).with_max_lines(15)
    ));
}

#[test]
fn rust_simple_pseudo_max15() {
    insta::assert_snapshot!(snap(
        RUST_SIMPLE,
        Language::Rust,
        TransformConfig::with_mode(Mode::Pseudo).with_max_lines(15)
    ));
}

#[test]
fn rust_simple_structure_last10() {
    insta::assert_snapshot!(snap(
        RUST_SIMPLE,
        Language::Rust,
        TransformConfig::with_mode(Mode::Structure).with_last_lines(10)
    ));
}

#[test]
fn rust_simple_pseudo_last10() {
    insta::assert_snapshot!(snap(
        RUST_SIMPLE,
        Language::Rust,
        TransformConfig::with_mode(Mode::Pseudo).with_last_lines(10)
    ));
}

#[test]
fn rust_simple_structure_max5() {
    insta::assert_snapshot!(snap(
        RUST_SIMPLE,
        Language::Rust,
        TransformConfig::with_mode(Mode::Structure).with_max_lines(5)
    ));
}

#[test]
fn rust_simple_pseudo_max5() {
    insta::assert_snapshot!(snap(
        RUST_SIMPLE,
        Language::Rust,
        TransformConfig::with_mode(Mode::Pseudo).with_max_lines(5)
    ));
}

// Additional Rust fixture
#[test]
fn rust_comments_structure_unbounded() {
    insta::assert_snapshot!(snap(
        RUST_COMMENTS,
        Language::Rust,
        TransformConfig::with_mode(Mode::Structure)
    ));
}

#[test]
fn rust_comments_pseudo_unbounded() {
    insta::assert_snapshot!(snap(
        RUST_COMMENTS,
        Language::Rust,
        TransformConfig::with_mode(Mode::Pseudo)
    ));
}

#[test]
fn rust_comments_structure_max15() {
    insta::assert_snapshot!(snap(
        RUST_COMMENTS,
        Language::Rust,
        TransformConfig::with_mode(Mode::Structure).with_max_lines(15)
    ));
}

#[test]
fn rust_comments_pseudo_max5() {
    insta::assert_snapshot!(snap(
        RUST_COMMENTS,
        Language::Rust,
        TransformConfig::with_mode(Mode::Pseudo).with_max_lines(5)
    ));
}

// ---------------------------------------------------------------------------
// Python — simple.py
// ---------------------------------------------------------------------------

#[test]
fn python_simple_structure_unbounded() {
    insta::assert_snapshot!(snap(
        PYTHON_SIMPLE,
        Language::Python,
        TransformConfig::with_mode(Mode::Structure)
    ));
}

#[test]
fn python_simple_signatures_unbounded() {
    insta::assert_snapshot!(snap(
        PYTHON_SIMPLE,
        Language::Python,
        TransformConfig::with_mode(Mode::Signatures)
    ));
}

#[test]
fn python_simple_types_unbounded() {
    insta::assert_snapshot!(snap(
        PYTHON_SIMPLE,
        Language::Python,
        TransformConfig::with_mode(Mode::Types)
    ));
}

#[test]
fn python_simple_minimal_unbounded() {
    insta::assert_snapshot!(snap(
        PYTHON_SIMPLE,
        Language::Python,
        TransformConfig::with_mode(Mode::Minimal)
    ));
}

#[test]
fn python_simple_pseudo_unbounded() {
    insta::assert_snapshot!(snap(
        PYTHON_SIMPLE,
        Language::Python,
        TransformConfig::with_mode(Mode::Pseudo)
    ));
}

#[test]
fn python_simple_full_unbounded() {
    insta::assert_snapshot!(snap(
        PYTHON_SIMPLE,
        Language::Python,
        TransformConfig::with_mode(Mode::Full)
    ));
}

#[test]
fn python_simple_structure_max15() {
    insta::assert_snapshot!(snap(
        PYTHON_SIMPLE,
        Language::Python,
        TransformConfig::with_mode(Mode::Structure).with_max_lines(15)
    ));
}

#[test]
fn python_simple_pseudo_max15() {
    insta::assert_snapshot!(snap(
        PYTHON_SIMPLE,
        Language::Python,
        TransformConfig::with_mode(Mode::Pseudo).with_max_lines(15)
    ));
}

#[test]
fn python_simple_structure_last10() {
    insta::assert_snapshot!(snap(
        PYTHON_SIMPLE,
        Language::Python,
        TransformConfig::with_mode(Mode::Structure).with_last_lines(10)
    ));
}

#[test]
fn python_simple_pseudo_last10() {
    insta::assert_snapshot!(snap(
        PYTHON_SIMPLE,
        Language::Python,
        TransformConfig::with_mode(Mode::Pseudo).with_last_lines(10)
    ));
}

#[test]
fn python_simple_structure_max5() {
    insta::assert_snapshot!(snap(
        PYTHON_SIMPLE,
        Language::Python,
        TransformConfig::with_mode(Mode::Structure).with_max_lines(5)
    ));
}

#[test]
fn python_simple_pseudo_max5() {
    insta::assert_snapshot!(snap(
        PYTHON_SIMPLE,
        Language::Python,
        TransformConfig::with_mode(Mode::Pseudo).with_max_lines(5)
    ));
}

// Python comments fixture
#[test]
fn python_comments_structure_unbounded() {
    insta::assert_snapshot!(snap(
        PYTHON_COMMENTS,
        Language::Python,
        TransformConfig::with_mode(Mode::Structure)
    ));
}

#[test]
fn python_comments_pseudo_unbounded() {
    insta::assert_snapshot!(snap(
        PYTHON_COMMENTS,
        Language::Python,
        TransformConfig::with_mode(Mode::Pseudo)
    ));
}

#[test]
fn python_comments_structure_max15() {
    insta::assert_snapshot!(snap(
        PYTHON_COMMENTS,
        Language::Python,
        TransformConfig::with_mode(Mode::Structure).with_max_lines(15)
    ));
}

#[test]
fn python_comments_pseudo_max5() {
    insta::assert_snapshot!(snap(
        PYTHON_COMMENTS,
        Language::Python,
        TransformConfig::with_mode(Mode::Pseudo).with_max_lines(5)
    ));
}

// ---------------------------------------------------------------------------
// Go — simple.go
// ---------------------------------------------------------------------------

#[test]
fn go_simple_structure_unbounded() {
    insta::assert_snapshot!(snap(
        GO_SIMPLE,
        Language::Go,
        TransformConfig::with_mode(Mode::Structure)
    ));
}

#[test]
fn go_simple_signatures_unbounded() {
    insta::assert_snapshot!(snap(
        GO_SIMPLE,
        Language::Go,
        TransformConfig::with_mode(Mode::Signatures)
    ));
}

#[test]
fn go_simple_types_unbounded() {
    insta::assert_snapshot!(snap(
        GO_SIMPLE,
        Language::Go,
        TransformConfig::with_mode(Mode::Types)
    ));
}

#[test]
fn go_simple_minimal_unbounded() {
    insta::assert_snapshot!(snap(
        GO_SIMPLE,
        Language::Go,
        TransformConfig::with_mode(Mode::Minimal)
    ));
}

#[test]
fn go_simple_pseudo_unbounded() {
    insta::assert_snapshot!(snap(
        GO_SIMPLE,
        Language::Go,
        TransformConfig::with_mode(Mode::Pseudo)
    ));
}

#[test]
fn go_simple_full_unbounded() {
    insta::assert_snapshot!(snap(
        GO_SIMPLE,
        Language::Go,
        TransformConfig::with_mode(Mode::Full)
    ));
}

#[test]
fn go_simple_structure_max15() {
    insta::assert_snapshot!(snap(
        GO_SIMPLE,
        Language::Go,
        TransformConfig::with_mode(Mode::Structure).with_max_lines(15)
    ));
}

#[test]
fn go_simple_pseudo_max15() {
    insta::assert_snapshot!(snap(
        GO_SIMPLE,
        Language::Go,
        TransformConfig::with_mode(Mode::Pseudo).with_max_lines(15)
    ));
}

#[test]
fn go_simple_structure_last10() {
    insta::assert_snapshot!(snap(
        GO_SIMPLE,
        Language::Go,
        TransformConfig::with_mode(Mode::Structure).with_last_lines(10)
    ));
}

#[test]
fn go_simple_pseudo_last10() {
    insta::assert_snapshot!(snap(
        GO_SIMPLE,
        Language::Go,
        TransformConfig::with_mode(Mode::Pseudo).with_last_lines(10)
    ));
}

#[test]
fn go_simple_structure_max5() {
    insta::assert_snapshot!(snap(
        GO_SIMPLE,
        Language::Go,
        TransformConfig::with_mode(Mode::Structure).with_max_lines(5)
    ));
}

#[test]
fn go_simple_pseudo_max5() {
    insta::assert_snapshot!(snap(
        GO_SIMPLE,
        Language::Go,
        TransformConfig::with_mode(Mode::Pseudo).with_max_lines(5)
    ));
}

// Go comments fixture
#[test]
fn go_comments_structure_unbounded() {
    insta::assert_snapshot!(snap(
        GO_COMMENTS,
        Language::Go,
        TransformConfig::with_mode(Mode::Structure)
    ));
}

#[test]
fn go_comments_pseudo_unbounded() {
    insta::assert_snapshot!(snap(
        GO_COMMENTS,
        Language::Go,
        TransformConfig::with_mode(Mode::Pseudo)
    ));
}

#[test]
fn go_comments_structure_max15() {
    insta::assert_snapshot!(snap(
        GO_COMMENTS,
        Language::Go,
        TransformConfig::with_mode(Mode::Structure).with_max_lines(15)
    ));
}

#[test]
fn go_comments_pseudo_max5() {
    insta::assert_snapshot!(snap(
        GO_COMMENTS,
        Language::Go,
        TransformConfig::with_mode(Mode::Pseudo).with_max_lines(5)
    ));
}

// ---------------------------------------------------------------------------
// TypeScript — simple.ts
// ---------------------------------------------------------------------------

#[test]
fn ts_simple_structure_unbounded() {
    insta::assert_snapshot!(snap(
        TS_SIMPLE,
        Language::TypeScript,
        TransformConfig::with_mode(Mode::Structure)
    ));
}

#[test]
fn ts_simple_signatures_unbounded() {
    insta::assert_snapshot!(snap(
        TS_SIMPLE,
        Language::TypeScript,
        TransformConfig::with_mode(Mode::Signatures)
    ));
}

#[test]
fn ts_simple_types_unbounded() {
    insta::assert_snapshot!(snap(
        TS_SIMPLE,
        Language::TypeScript,
        TransformConfig::with_mode(Mode::Types)
    ));
}

#[test]
fn ts_simple_minimal_unbounded() {
    insta::assert_snapshot!(snap(
        TS_SIMPLE,
        Language::TypeScript,
        TransformConfig::with_mode(Mode::Minimal)
    ));
}

#[test]
fn ts_simple_pseudo_unbounded() {
    insta::assert_snapshot!(snap(
        TS_SIMPLE,
        Language::TypeScript,
        TransformConfig::with_mode(Mode::Pseudo)
    ));
}

#[test]
fn ts_simple_full_unbounded() {
    insta::assert_snapshot!(snap(
        TS_SIMPLE,
        Language::TypeScript,
        TransformConfig::with_mode(Mode::Full)
    ));
}

#[test]
fn ts_simple_structure_max15() {
    insta::assert_snapshot!(snap(
        TS_SIMPLE,
        Language::TypeScript,
        TransformConfig::with_mode(Mode::Structure).with_max_lines(15)
    ));
}

#[test]
fn ts_simple_pseudo_max15() {
    insta::assert_snapshot!(snap(
        TS_SIMPLE,
        Language::TypeScript,
        TransformConfig::with_mode(Mode::Pseudo).with_max_lines(15)
    ));
}

#[test]
fn ts_simple_structure_last10() {
    insta::assert_snapshot!(snap(
        TS_SIMPLE,
        Language::TypeScript,
        TransformConfig::with_mode(Mode::Structure).with_last_lines(10)
    ));
}

#[test]
fn ts_simple_pseudo_last10() {
    insta::assert_snapshot!(snap(
        TS_SIMPLE,
        Language::TypeScript,
        TransformConfig::with_mode(Mode::Pseudo).with_last_lines(10)
    ));
}

#[test]
fn ts_simple_structure_max5() {
    insta::assert_snapshot!(snap(
        TS_SIMPLE,
        Language::TypeScript,
        TransformConfig::with_mode(Mode::Structure).with_max_lines(5)
    ));
}

#[test]
fn ts_simple_pseudo_max5() {
    insta::assert_snapshot!(snap(
        TS_SIMPLE,
        Language::TypeScript,
        TransformConfig::with_mode(Mode::Pseudo).with_max_lines(5)
    ));
}

// TypeScript comments fixture
#[test]
fn ts_comments_structure_unbounded() {
    insta::assert_snapshot!(snap(
        TS_COMMENTS,
        Language::TypeScript,
        TransformConfig::with_mode(Mode::Structure)
    ));
}

#[test]
fn ts_comments_pseudo_unbounded() {
    insta::assert_snapshot!(snap(
        TS_COMMENTS,
        Language::TypeScript,
        TransformConfig::with_mode(Mode::Pseudo)
    ));
}

#[test]
fn ts_comments_structure_max15() {
    insta::assert_snapshot!(snap(
        TS_COMMENTS,
        Language::TypeScript,
        TransformConfig::with_mode(Mode::Structure).with_max_lines(15)
    ));
}

#[test]
fn ts_comments_pseudo_max5() {
    insta::assert_snapshot!(snap(
        TS_COMMENTS,
        Language::TypeScript,
        TransformConfig::with_mode(Mode::Pseudo).with_max_lines(5)
    ));
}

// ---------------------------------------------------------------------------
// Markdown — simple.md
// ---------------------------------------------------------------------------

#[test]
fn md_simple_structure_unbounded() {
    insta::assert_snapshot!(snap(
        MD_SIMPLE,
        Language::Markdown,
        TransformConfig::with_mode(Mode::Structure)
    ));
}

#[test]
fn md_simple_signatures_unbounded() {
    insta::assert_snapshot!(snap(
        MD_SIMPLE,
        Language::Markdown,
        TransformConfig::with_mode(Mode::Signatures)
    ));
}

#[test]
fn md_simple_types_unbounded() {
    insta::assert_snapshot!(snap(
        MD_SIMPLE,
        Language::Markdown,
        TransformConfig::with_mode(Mode::Types)
    ));
}

#[test]
fn md_simple_minimal_unbounded() {
    insta::assert_snapshot!(snap(
        MD_SIMPLE,
        Language::Markdown,
        TransformConfig::with_mode(Mode::Minimal)
    ));
}

#[test]
fn md_simple_pseudo_unbounded() {
    insta::assert_snapshot!(snap(
        MD_SIMPLE,
        Language::Markdown,
        TransformConfig::with_mode(Mode::Pseudo)
    ));
}

#[test]
fn md_simple_full_unbounded() {
    insta::assert_snapshot!(snap(
        MD_SIMPLE,
        Language::Markdown,
        TransformConfig::with_mode(Mode::Full)
    ));
}

#[test]
fn md_simple_structure_max15() {
    insta::assert_snapshot!(snap(
        MD_SIMPLE,
        Language::Markdown,
        TransformConfig::with_mode(Mode::Structure).with_max_lines(15)
    ));
}

#[test]
fn md_simple_pseudo_max15() {
    insta::assert_snapshot!(snap(
        MD_SIMPLE,
        Language::Markdown,
        TransformConfig::with_mode(Mode::Pseudo).with_max_lines(15)
    ));
}

#[test]
fn md_simple_structure_last10() {
    insta::assert_snapshot!(snap(
        MD_SIMPLE,
        Language::Markdown,
        TransformConfig::with_mode(Mode::Structure).with_last_lines(10)
    ));
}

#[test]
fn md_simple_pseudo_last10() {
    insta::assert_snapshot!(snap(
        MD_SIMPLE,
        Language::Markdown,
        TransformConfig::with_mode(Mode::Pseudo).with_last_lines(10)
    ));
}

#[test]
fn md_simple_structure_max5() {
    insta::assert_snapshot!(snap(
        MD_SIMPLE,
        Language::Markdown,
        TransformConfig::with_mode(Mode::Structure).with_max_lines(5)
    ));
}

#[test]
fn md_simple_pseudo_max5() {
    insta::assert_snapshot!(snap(
        MD_SIMPLE,
        Language::Markdown,
        TransformConfig::with_mode(Mode::Pseudo).with_max_lines(5)
    ));
}
