//! Tests for classify_tree(), classify_data_format() and build_field_ranges().
//!
//! The role rules read only the comment and string fields (`Labels::role`).
//! The comment and string kinds of each language, and the role a word gets in
//! them, are tested in `roles_tests.rs`. The tests here hold the invariants of
//! the range list and the parts of the classifier below the role rules.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::ops::Range;

use super::*;

// -----------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------

/// Assert that ranges cover the full source length without gaps or overlaps.
fn assert_contiguous(ranges: &[(Range<usize>, SearchField)], source_len: usize) {
    if source_len == 0 {
        assert!(
            ranges.is_empty(),
            "empty source should produce empty ranges"
        );
        return;
    }
    let mut expected = 0usize;
    for (range, _) in ranges {
        assert_eq!(
            range.start, expected,
            "gap at {expected}: range starts at {}",
            range.start
        );
        assert!(range.end > range.start, "range must be non-empty");
        expected = range.end;
    }
    assert_eq!(
        expected, source_len,
        "ranges do not cover full source: covered {expected}, source len {source_len}"
    );
}

/// Assert that field_lengths_sum equals doc_length.
fn assert_field_lengths_sum(ranges: &[(Range<usize>, SearchField)], source_len: usize) {
    let total: usize = ranges.iter().map(|(r, _)| r.end - r.start).sum();
    assert_eq!(
        total, source_len,
        "sum of range lengths {total} != source len {source_len}"
    );
}

/// The source text of each range that has `field`, in order.
fn field_text<'a>(
    source: &'a str,
    ranges: &[(Range<usize>, SearchField)],
    field: SearchField,
) -> Vec<&'a str> {
    ranges
        .iter()
        .filter(|(_, f)| *f == field)
        .map(|(r, _)| &source[r.clone()])
        .collect()
}

// -----------------------------------------------------------------------
// Empty source
// -----------------------------------------------------------------------

#[test]
fn test_empty_source_returns_empty() {
    let ranges = classify_source("", rskim_core::Language::Rust).unwrap();
    assert!(ranges.is_empty(), "empty source should return empty vec");
}

// -----------------------------------------------------------------------
// Dispatch
// -----------------------------------------------------------------------

/// The data formats have no tree: they take the source alone. Every other
/// language takes a tree.
#[test]
fn the_data_formats_and_no_other_language_take_the_source_alone() {
    for lang in [Language::Json, Language::Yaml, Language::Toml] {
        assert!(
            classify_data_format("a", lang).is_some(),
            "{lang:?} has no tree"
        );
    }
    for lang in [Language::Rust, Language::Markdown, Language::Sql] {
        assert!(
            classify_data_format("a", lang).is_none(),
            "{lang:?} has a tree"
        );
    }
}

/// A Markdown tree takes the Markdown classifier: the generic path has no
/// kind for a paragraph, so a paragraph that is a comment shows which path ran.
#[test]
fn test_markdown_tree_takes_the_markdown_classifier() {
    let source = "#### Deep\n\nBody.\n";
    let ranges = classify_source(source, rskim_core::Language::Markdown).unwrap();
    assert_contiguous(&ranges, source.len());
    assert_eq!(
        field_text(source, &ranges, SearchField::Comment),
        vec!["Body.\n"],
        "ranges: {ranges:?}"
    );
}

// -----------------------------------------------------------------------
// Invariants of the range list
// -----------------------------------------------------------------------

#[test]
fn test_non_overlapping_invariant() {
    let source = "struct Foo { fn bar() {} } // note\n";
    let ranges = classify_source(source, rskim_core::Language::Rust).unwrap();
    // Verify no two ranges overlap.
    for i in 0..ranges.len().saturating_sub(1) {
        assert_eq!(
            ranges[i].0.end,
            ranges[i + 1].0.start,
            "ranges overlap or gap between index {i} and {}: {:?} vs {:?}",
            i + 1,
            ranges[i],
            ranges[i + 1]
        );
    }
}

#[test]
fn test_field_lengths_sum_multi_language() {
    let cases: &[(&str, rskim_core::Language)] = &[
        ("def f(x): return x", rskim_core::Language::Python),
        ("fn x() {}", rskim_core::Language::Rust),
        (
            "function y() { return 1; }",
            rskim_core::Language::JavaScript,
        ),
    ];
    for (source, lang) in cases {
        let ranges = classify_source(source, *lang).unwrap();
        assert_contiguous(&ranges, source.len());
        assert_field_lengths_sum(&ranges, source.len());
    }
}

// -----------------------------------------------------------------------
// Strings and comments
// -----------------------------------------------------------------------

#[test]
fn a_string_and_a_comment_of_a_function_are_their_own_ranges() {
    let source = "fn main() { let s = \"text\"; /* note */ }";
    let ranges = classify_source(source, rskim_core::Language::Rust).unwrap();
    assert_contiguous(&ranges, source.len());
    assert_eq!(
        field_text(source, &ranges, SearchField::StringLiteral),
        vec!["\"text\""],
        "ranges: {ranges:?}"
    );
    assert_eq!(
        field_text(source, &ranges, SearchField::Comment),
        vec!["/* note */"],
        "ranges: {ranges:?}"
    );
}

/// A SQL `literal` is a string, a number, a boolean or null. Only the string
/// has a quote.
#[test]
fn a_sql_literal_is_a_string_only_when_it_has_a_quote() {
    let source = "SELECT 'text', 42, 4.5, TRUE, NULL, $$body$$;\n";
    let ranges = classify_source(source, rskim_core::Language::Sql).unwrap();
    assert_contiguous(&ranges, source.len());
    assert_eq!(
        field_text(source, &ranges, SearchField::StringLiteral),
        vec!["'text'", "$$body$$"],
        "ranges: {ranges:?}"
    );
}

// -----------------------------------------------------------------------
// build_field_ranges: the sweep over nested node ranges
// -----------------------------------------------------------------------

#[test]
fn the_innermost_node_wins_each_byte_and_gaps_are_other() {
    use SearchField::{Code, Comment, StringLiteral};
    // Pre-order: an outer body over 2..20, a string in it over 5..8, a comment
    // in the string's place with the same range (the later node is deeper), and
    // a second string after a gap.
    let ranges = vec![
        (2..20, Code),
        (5..8, StringLiteral),
        (5..8, Comment),
        (12..15, StringLiteral),
    ];

    let built = build_field_ranges(ranges, 24);

    assert_eq!(
        built,
        vec![
            (0..2, SearchField::Other),
            (2..5, Code),
            (5..8, Comment),
            (8..12, Code),
            (12..15, StringLiteral),
            (15..20, Code),
            (20..24, SearchField::Other),
        ]
    );
}

/// The code in the `${…}` of a template string is not part of the string.
#[test]
fn a_name_inside_a_string_takes_the_code_field_not_the_string_field() {
    let source = "const s = `a ${name} b`;\n";
    let ranges = classify_source(source, rskim_core::Language::JavaScript).unwrap();
    assert_contiguous(&ranges, source.len());
    assert!(
        field_text(source, &ranges, SearchField::Code).contains(&"name"),
        "ranges: {ranges:?}"
    );
    assert!(
        !field_text(source, &ranges, SearchField::StringLiteral).contains(&"name"),
        "ranges: {ranges:?}"
    );
}

#[test]
fn sibling_nodes_with_the_same_field_merge_and_a_child_past_its_parent_is_cut() {
    use SearchField::{Code, Comment};
    let built = build_field_ranges(vec![(0..4, Code), (4..8, Code), (6..12, Comment)], 10);
    // The comment child starts inside the second body and ends after it: it is
    // cut at the parent's end.
    assert_eq!(
        built,
        vec![(0..6, Code), (6..8, Comment), (8..10, SearchField::Other)]
    );
    assert_contiguous(&built, 10);
}

#[test]
fn no_ranges_give_one_other_range() {
    assert_eq!(
        build_field_ranges(Vec::new(), 7),
        vec![(0..7, SearchField::Other)]
    );
}
