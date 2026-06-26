//! Markdown integration tests for CLI
//!
//! Tests markdown header extraction with various modes.

use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;
mod common;

// ============================================================================
// Structure Mode Tests (H1-H3)
// ============================================================================

#[test]
fn test_markdown_structure_mode_h1_to_h3() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.md");
    fs::write(
        &file_path,
        r#"# Main Title

Some intro paragraph.

## Section One

Content for section one.

### Subsection 1.1

More details.

#### Level 4 Header

This should NOT appear in structure mode.

##### Level 5 Header

This should also NOT appear.

###### Level 6 Header

This should also NOT appear.
"#,
    )
    .unwrap();

    let output = common::skim()
        .arg(&file_path)
        .arg("--mode")
        .arg("structure")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();

    // Should contain H1-H3
    assert!(stdout.contains("# Main Title"));
    assert!(stdout.contains("## Section One"));
    assert!(stdout.contains("### Subsection 1.1"));

    // Should NOT contain H4-H6
    assert!(!stdout.contains("#### Level 4"));
    assert!(!stdout.contains("##### Level 5"));
    assert!(!stdout.contains("###### Level 6"));

    // Should NOT contain body content
    assert!(!stdout.contains("Some intro paragraph"));
    assert!(!stdout.contains("Content for section"));
}

#[test]
fn test_markdown_structure_mode_auto_detect() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("README.md");
    fs::write(
        &file_path,
        r#"# Project Title

## Installation

### Prerequisites

#### Detailed Steps
"#,
    )
    .unwrap();

    common::skim()
        .arg(&file_path)
        // No --mode specified, should default to structure
        .assert()
        .success()
        .stdout(predicate::str::contains("# Project Title"))
        .stdout(predicate::str::contains("## Installation"))
        .stdout(predicate::str::contains("### Prerequisites"))
        .stdout(predicate::str::contains("#### Detailed Steps").not());
}

// ============================================================================
// Signatures Mode Tests (H1-H6, All Headers)
// ============================================================================

#[test]
fn test_markdown_signatures_mode_all_headers() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.md");
    fs::write(
        &file_path,
        r#"# H1 Header

## H2 Header

### H3 Header

#### H4 Header

##### H5 Header

###### H6 Header

Some body text that should not appear.
"#,
    )
    .unwrap();

    let output = common::skim()
        .arg(&file_path)
        .arg("--mode")
        .arg("signatures")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();

    // Should contain ALL headers (H1-H6)
    assert!(stdout.contains("# H1 Header"));
    assert!(stdout.contains("## H2 Header"));
    assert!(stdout.contains("### H3 Header"));
    assert!(stdout.contains("#### H4 Header"));
    assert!(stdout.contains("##### H5 Header"));
    assert!(stdout.contains("###### H6 Header"));

    // Should NOT contain body text
    assert!(!stdout.contains("Some body text"));
}

// ============================================================================
// Types Mode Tests (Same as Signatures for Markdown)
// ============================================================================

#[test]
fn test_markdown_types_mode() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.md");
    fs::write(
        &file_path,
        r#"# Documentation

#### Deep Section

###### Very Deep
"#,
    )
    .unwrap();

    let output = common::skim()
        .arg(&file_path)
        .arg("--mode")
        .arg("types")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();

    // Types mode should extract ALL headers (same as signatures)
    assert!(stdout.contains("# Documentation"));
    assert!(stdout.contains("#### Deep Section"));
    assert!(stdout.contains("###### Very Deep"));
}

// ============================================================================
// Setext Header Tests (Underlined Headers)
// ============================================================================

#[test]
fn test_markdown_setext_headers() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.md");
    fs::write(
        &file_path,
        r#"Main Title
==========

Some content here.

Subtitle
--------

More content.
"#,
    )
    .unwrap();

    let output = common::skim()
        .arg(&file_path)
        .arg("--mode")
        .arg("structure")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();

    // Should contain both setext headers
    assert!(stdout.contains("Main Title"));
    assert!(stdout.contains("Subtitle"));

    // Should NOT contain body content
    assert!(!stdout.contains("Some content"));
    assert!(!stdout.contains("More content"));
}

// ============================================================================
// Mixed Header Styles
// ============================================================================

#[test]
fn test_markdown_mixed_header_styles() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.md");
    fs::write(
        &file_path,
        r#"ATX Style H1
============

## ATX Style H2

### ATX Style H3

Setext Style H2
---------------
"#,
    )
    .unwrap();

    let output = common::skim()
        .arg(&file_path)
        .arg("--mode")
        .arg("signatures")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();

    // Should handle both ATX and setext styles
    assert!(stdout.contains("ATX Style H1"));
    assert!(stdout.contains("## ATX Style H2"));
    assert!(stdout.contains("### ATX Style H3"));
    assert!(stdout.contains("Setext Style H2"));
}

// ============================================================================
// Full Mode (No Transformation)
// ============================================================================

#[test]
fn test_markdown_full_mode() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.md");
    let content = r#"# Title

Body content here.

## Section

More body content.
"#;
    fs::write(&file_path, content).unwrap();

    common::skim()
        .arg(&file_path)
        .arg("--mode")
        .arg("full")
        .assert()
        .success()
        .stdout(predicate::str::contains("# Title"))
        .stdout(predicate::str::contains("Body content here"))
        .stdout(predicate::str::contains("## Section"))
        .stdout(predicate::str::contains("More body content"));
}

// ============================================================================
// Document Order (Fix 1 — heading sort regression guard)
// ============================================================================

/// Headings must appear in document (top-to-bottom) order in the output.
///
/// Before the document-order sort fix, the LIFO DFS stack emitted siblings
/// in reverse order, so `## Beta` appeared before `## Alpha`.  This test
/// is the regression guard: it verifies that the structure-mode and
/// signatures-mode outputs list headings in ascending source-line order.
#[test]
fn test_markdown_headings_in_document_order() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("order.md");
    fs::write(
        &file_path,
        "# Section Alpha\n\nSome content.\n\n## Sub Beta\n\nMore content.\n\n## Sub Gamma\n\nFinal.\n",
    )
    .unwrap();

    for mode in &["structure", "signatures", "types"] {
        let output = common::skim()
            .arg(&file_path)
            .arg("--mode")
            .arg(mode)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();

        let stdout = String::from_utf8(output).unwrap();

        // All three headings must be present
        assert!(
            stdout.contains("Alpha"),
            "[{mode}] Alpha heading missing from output: {stdout}"
        );
        assert!(
            stdout.contains("Beta"),
            "[{mode}] Beta heading missing from output: {stdout}"
        );
        assert!(
            stdout.contains("Gamma"),
            "[{mode}] Gamma heading missing from output: {stdout}"
        );

        // Alpha (line 1) must appear before Beta (line 5) and Gamma (line 9)
        let pos_alpha = stdout.find("Alpha").expect("Alpha must be present");
        let pos_beta = stdout.find("Beta").expect("Beta must be present");
        let pos_gamma = stdout.find("Gamma").expect("Gamma must be present");

        assert!(
            pos_alpha < pos_beta,
            "[{mode}] Alpha (pos {pos_alpha}) must precede Beta (pos {pos_beta})"
        );
        assert!(
            pos_beta < pos_gamma,
            "[{mode}] Beta (pos {pos_beta}) must precede Gamma (pos {pos_gamma})"
        );
    }
}

// ============================================================================
// Edge Cases
// ============================================================================

#[test]
fn test_markdown_empty_file() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("empty.md");
    fs::write(&file_path, "").unwrap();

    common::skim().arg(&file_path).assert().success();
}

#[test]
fn test_markdown_no_headers() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.md");
    fs::write(&file_path, "Just some plain text without any headers.").unwrap();

    let output = common::skim()
        .arg(&file_path)
        .arg("--mode")
        .arg("structure")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    // Should return empty or no content (no headers to extract)
    assert!(!stdout.contains("plain text"));
}

#[test]
fn test_markdown_extension_variations() {
    let temp_dir = TempDir::new().unwrap();

    // Test both .md and .markdown extensions
    for ext in &["md", "markdown"] {
        let file_path = temp_dir.path().join(format!("test.{}", ext));
        fs::write(&file_path, "# Test Header").unwrap();

        common::skim()
            .arg(&file_path)
            .assert()
            .success()
            .stdout(predicate::str::contains("# Test Header"));
    }
}
