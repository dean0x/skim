//! Tests for format-specific field classifiers (JSON, YAML, TOML, Markdown).
//!
//! Written in TDD order: tests were written before production code.
//! Tests cover the contract invariants plus format-specific field mapping.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ops::Range;

use super::super::classifier::classify_source;
use super::super::field::SearchField;

use super::serde_fields::{classify_json, classify_toml, classify_yaml};

// ============================================================================
// Contract helpers
// ============================================================================

/// Assert that ranges cover the full source length without gaps or overlaps.
fn assert_contiguous(ranges: &[(Range<usize>, SearchField)], len: usize) {
    if len == 0 {
        assert!(
            ranges.is_empty(),
            "empty source should produce empty ranges, got: {ranges:?}"
        );
        return;
    }
    let mut expected = 0usize;
    for (range, _) in ranges {
        assert_eq!(
            range.start, expected,
            "gap at {expected}: range starts at {}; full ranges: {ranges:?}",
            range.start
        );
        assert!(
            range.end > range.start,
            "range must be non-empty; got {range:?}"
        );
        expected = range.end;
    }
    assert_eq!(
        expected, len,
        "ranges do not cover full source: covered {expected}, source len {len}; ranges: {ranges:?}"
    );
}

/// Assert that the sum of range lengths equals source_len.
fn assert_field_lengths_sum(ranges: &[(Range<usize>, SearchField)], len: usize) {
    let total: usize = ranges.iter().map(|(r, _)| r.end - r.start).sum();
    assert_eq!(
        total, len,
        "sum of range lengths {total} != source len {len}"
    );
}

/// Returns true if any range has the given field.
fn has_field(ranges: &[(Range<usize>, SearchField)], field: SearchField) -> bool {
    ranges.iter().any(|(_, f)| *f == field)
}

/// Collect the source text slices for all ranges matching the given field.
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

// ============================================================================
// JSON tests
// ============================================================================

/// F-JSON-01: simple object — the key is not a string, the value is a
/// StringLiteral.
#[test]
fn f_json_01_key_is_not_a_string_value_is_a_string_literal() {
    let source = r#"{"name": "skim"}"#;
    let ranges = classify_json(source);
    assert_contiguous(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::StringLiteral),
        vec![r#""skim""#],
        "ranges: {ranges:?}"
    );
}

/// F-JSON-02: keys of nested objects are not strings.
#[test]
fn f_json_02_keys_of_nested_objects_are_not_strings() {
    let source = r#"{"deps": {"serde": "1.0"}}"#;
    let ranges = classify_json(source);
    assert_contiguous(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::StringLiteral),
        vec![r#""1.0""#],
        "ranges: {ranges:?}"
    );
}

/// F-JSON-04: a value with escaped quotes is one StringLiteral.
#[test]
fn f_json_04_escaped_quotes_stay_in_one_string() {
    let source = r#"{"key": "val with \"quotes\""}"#;
    let ranges = classify_json(source);
    assert_contiguous(&ranges, source.len());
    assert_field_lengths_sum(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::StringLiteral),
        vec![r#""val with \"quotes\"""#],
        "ranges: {ranges:?}"
    );
}

/// F-JSON-05: empty object → one Other range.
#[test]
fn f_json_05_empty_object_all_other() {
    let source = "{}";
    let ranges = classify_json(source);
    assert_eq!(ranges, vec![(0..2, SearchField::Other)]);
}

/// F-JSON-06: deep nesting — only the innermost value is a string.
#[test]
fn f_json_06_deep_nesting_only_the_value_is_a_string() {
    let source = r#"{"a":{"b":{"c":"deep"}}}"#;
    let ranges = classify_json(source);
    assert_contiguous(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::StringLiteral),
        vec![r#""deep""#],
        "ranges: {ranges:?}"
    );
}

/// F-JSON-07: array at root — keys inside the objects are not strings, the
/// strings of the array are.
#[test]
fn f_json_07_array_at_root_keys_are_not_strings() {
    let source = r#"[{"id": "x"}, "y"]"#;
    let ranges = classify_json(source);
    assert_contiguous(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::StringLiteral),
        vec![r#""x""#, r#""y""#],
        "ranges: {ranges:?}"
    );
}

/// F-JSON-08: numbers, booleans and null are not strings.
#[test]
fn f_json_08_non_string_values_are_other() {
    let source = r#"{"key": 42, "flag": true, "none": null}"#;
    let ranges = classify_json(source);
    assert_eq!(ranges, vec![(0..source.len(), SearchField::Other)]);
}

/// F-JSON-09: JSON nested beyond MAX_JSON_DEPTH (1024) does not panic and
/// produces contiguous output covering the full source.
///
/// Exercises the asymmetry guard in the `}` handler: braces beyond the cap
/// must not pop entries from shallower scopes.
#[test]
fn f_json_09_depth_beyond_max_json_depth_cap() {
    // Build a JSON object nested to 1025 levels (one past MAX_JSON_DEPTH).
    let depth = 1025usize;
    let mut source = String::with_capacity(depth * 2 + 20);
    // Opening braces with a key at the innermost level.
    for _ in 0..depth {
        source.push('{');
    }
    source.push_str(r#""k":"v""#);
    for _ in 0..depth {
        source.push('}');
    }

    let ranges = classify_json(&source);
    // Must not panic and output must be contiguous.
    assert_contiguous(&ranges, source.len());
    assert_field_lengths_sum(&ranges, source.len());
    // Output must not be empty (at minimum the key/value strings are classified).
    assert!(
        !ranges.is_empty(),
        "ranges must not be empty for deeply nested JSON; ranges: {ranges:?}"
    );
}

/// F-JSON-10: every string of an array inside an object is a value, not a key.
///
/// A comma inside an array must not put the enclosing object back to "expect a
/// key": that gave the second and later strings of an array the key field.
#[test]
fn f_json_10_every_string_of_an_array_in_an_object_is_a_string_literal() {
    let source = r#"{"files": ["a", "b", "c"], "next": "d"}"#;
    let ranges = classify_json(source);
    assert_contiguous(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::StringLiteral),
        vec![r#""a""#, r#""b""#, r#""c""#, r#""d""#],
        "ranges: {ranges:?}"
    );
}

/// F-JSON-11: arrays nested in arrays and objects in arrays keep their own
/// key and value positions.
#[test]
fn f_json_11_nested_arrays_and_objects_in_arrays_keep_key_and_value_positions() {
    let source = r#"{"a": [["x", "y"], {"k": "v", "l": ["w", "z"]}], "b": "u"}"#;
    let ranges = classify_json(source);
    assert_contiguous(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::StringLiteral),
        vec![r#""x""#, r#""y""#, r#""v""#, r#""w""#, r#""z""#, r#""u""#],
        "keys must not be string literals; ranges: {ranges:?}"
    );
}

// ============================================================================
// YAML tests
// ============================================================================

/// F-YAML-01: keys at any indent are not strings or comments.
#[test]
fn f_yaml_01_keys_are_other() {
    let source = "database:\n  host: localhost\n";
    let ranges = classify_yaml(source);
    assert_eq!(ranges, vec![(0..source.len(), SearchField::Other)]);
}

/// F-YAML-02: a comment line is a Comment.
#[test]
fn f_yaml_02_comment_line_is_comment() {
    let source = "# comment\nkey: val\n";
    let ranges = classify_yaml(source);
    assert_contiguous(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::Comment),
        vec!["# comment\n"],
        "ranges: {ranges:?}"
    );
}

/// F-YAML-03: a quoted value is a StringLiteral, its key is not.
#[test]
fn f_yaml_03_quoted_string_value_is_string_literal() {
    let source = "name: \"skim\"\n";
    let ranges = classify_yaml(source);
    assert_contiguous(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::StringLiteral),
        vec!["\"skim\""],
        "ranges: {ranges:?}"
    );
}

/// F-YAML-04: multi-level nesting — only the quoted value is a string.
#[test]
fn f_yaml_04_multi_level_nesting() {
    let source = "a:\n  b:\n    c: \"val\"\n";
    let ranges = classify_yaml(source);
    assert_contiguous(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::StringLiteral),
        vec!["\"val\""],
        "ranges: {ranges:?}"
    );
}

/// F-YAML-05: multi-document separators handled gracefully.
#[test]
fn f_yaml_05_multi_doc_separators() {
    let source = "---\ntitle: test\n---\nother: val\n";
    let ranges = classify_yaml(source);
    assert_contiguous(&ranges, source.len());
    assert_field_lengths_sum(&ranges, source.len());
}

/// F-YAML-06: the value of a key in a list item is read after the `- `.
#[test]
fn f_yaml_06_list_item_key_value_is_a_string() {
    let source = "items:\n  - name: \"foo\"\n";
    let ranges = classify_yaml(source);
    assert_contiguous(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::StringLiteral),
        vec!["\"foo\""],
        "ranges: {ranges:?}"
    );
}

/// F-YAML-07: quoted string value StringLiteral range excludes the trailing newline byte.
///
/// The scanner explicitly trims `\n` (and `\r` for CRLF) so the newline is not
/// boosted with StringLiteral weight in BM25F scoring.
#[test]
fn f_yaml_07_quoted_string_excludes_trailing_newline() {
    let source = "key: \"value\"\n";
    let ranges = classify_yaml(source);
    assert_contiguous(&ranges, source.len());

    // Find the StringLiteral range.
    let str_ranges: Vec<_> = ranges
        .iter()
        .filter(|(_, f)| *f == SearchField::StringLiteral)
        .collect();
    assert!(
        !str_ranges.is_empty(),
        "expected a StringLiteral range; ranges: {ranges:?}"
    );

    for (r, _) in &str_ranges {
        let last_byte = source.as_bytes()[r.end - 1];
        assert_ne!(
            last_byte,
            b'\n',
            "StringLiteral range must not end with \\n; range: {r:?}, text: {:?}",
            &source[r.clone()]
        );
        assert_ne!(
            last_byte,
            b'\r',
            "StringLiteral range must not end with \\r; range: {r:?}, text: {:?}",
            &source[r.clone()]
        );
    }
}

// ============================================================================
// TOML tests
// ============================================================================

/// F-TOML-01: a header and a key are not strings, the value is a StringLiteral.
#[test]
fn f_toml_01_section_header_key_value() {
    let source = "[package]\nname = \"skim\"\n";
    let ranges = classify_toml(source);
    assert_contiguous(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::StringLiteral),
        vec!["\"skim\""],
        "ranges: {ranges:?}"
    );
}

/// F-TOML-02: an array header is not a string.
#[test]
fn f_toml_02_array_header_is_other() {
    let source = "[[bin]]\nname = \"skim\"\n";
    let ranges = classify_toml(source);
    assert_contiguous(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::StringLiteral),
        vec!["\"skim\""],
        "ranges: {ranges:?}"
    );
    assert!(
        !has_field(&ranges, SearchField::Comment),
        "ranges: {ranges:?}"
    );
}

/// F-TOML-03: a comment line is a Comment.
#[test]
fn f_toml_03_comment_line() {
    let source = "# Config\n[db]\nurl = \"pg\"\n";
    let ranges = classify_toml(source);
    assert_contiguous(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::Comment),
        vec!["# Config"],
        "ranges: {ranges:?}"
    );
}

/// F-TOML-04: an inline comment after a value is a Comment.
#[test]
fn f_toml_04_inline_comment() {
    let source = "[db]\nurl = \"pg\" # inline\n";
    let ranges = classify_toml(source);
    assert_contiguous(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::Comment),
        vec!["# inline"],
        "ranges: {ranges:?}"
    );
}

/// F-TOML-05: a dotted key is not a string.
#[test]
fn f_toml_05_dotted_key_is_other() {
    let source = "a.b.c = \"val\"\n";
    let ranges = classify_toml(source);
    assert_contiguous(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::StringLiteral),
        vec!["\"val\""],
        "ranges: {ranges:?}"
    );
}

/// F-TOML-06: a long string value with a URL is one StringLiteral.
#[test]
fn f_toml_06_url_value_is_one_string() {
    let source = "[database]\ndatabase_url = \"postgres://localhost/mydb\"\n";
    let ranges = classify_toml(source);
    assert_contiguous(&ranges, source.len());
    assert_field_lengths_sum(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::StringLiteral),
        vec!["\"postgres://localhost/mydb\""],
        "ranges: {ranges:?}"
    );
}

/// F-TOML-07: triple-double-quote basic multi-line string is StringLiteral.
#[test]
fn f_toml_07_triple_double_quote_is_string_literal() {
    let source = "desc = \"\"\"\nline one\nline two\n\"\"\"\n";
    let ranges = classify_toml(source);
    assert_contiguous(&ranges, source.len());
    assert_field_lengths_sum(&ranges, source.len());

    let str_texts = field_text(source, &ranges, SearchField::StringLiteral);
    assert!(
        str_texts.iter().any(|t| t.contains("line one")),
        "triple-double-quote value should be StringLiteral; str texts: {str_texts:?}; ranges: {ranges:?}"
    );
}

/// F-TOML-08: triple-single-quote literal multi-line string is StringLiteral.
#[test]
fn f_toml_08_triple_single_quote_is_string_literal() {
    let source = "desc = '''\nline one\nline two\n'''\n";
    let ranges = classify_toml(source);
    assert_contiguous(&ranges, source.len());
    assert_field_lengths_sum(&ranges, source.len());

    let str_texts = field_text(source, &ranges, SearchField::StringLiteral);
    assert!(
        str_texts.iter().any(|t| t.contains("line one")),
        "triple-single-quote value should be StringLiteral; str texts: {str_texts:?}; ranges: {ranges:?}"
    );
}

/// F-TOML-09: triple-double-quote string with embedded quotes inside.
#[test]
fn f_toml_09_triple_double_quote_with_embedded_quotes() {
    // A triple-double-quoted string may contain `"` and `""` without terminating.
    let source = "msg = \"\"\"\nHe said \"hello\" to me.\n\"\"\"\n";
    let ranges = classify_toml(source);
    assert_contiguous(&ranges, source.len());
    assert_field_lengths_sum(&ranges, source.len());

    let str_texts = field_text(source, &ranges, SearchField::StringLiteral);
    assert!(
        str_texts.iter().any(|t| t.contains("hello")),
        "embedded quotes inside triple-double-quote should remain StringLiteral; str texts: {str_texts:?}; ranges: {ranges:?}"
    );
}

/// F-TOML-10: triple-single-quote string with embedded backslash (treated literally, not as escape).
#[test]
fn f_toml_10_triple_single_quote_backslash_literal() {
    // In TOML literal strings (single-quoted), backslash is NOT an escape.
    // This also holds for triple-single-quoted strings.
    let source = "path = '''\nC:\\Users\\skim\n'''\n";
    let ranges = classify_toml(source);
    assert_contiguous(&ranges, source.len());
    assert_field_lengths_sum(&ranges, source.len());

    let str_texts = field_text(source, &ranges, SearchField::StringLiteral);
    assert!(
        str_texts.iter().any(|t| t.contains("skim")),
        "backslash in triple-single-quote should be literal, content still StringLiteral; str texts: {str_texts:?}; ranges: {ranges:?}"
    );
}

/// F-TOML-11: a quoted TOML key containing `=` is not a string, and its `=` is
/// not the separator.
///
/// `find_toml_eq_sign` skips over `=` inside double-quoted key strings.
/// This test exercises that backslash-escape-aware path.
#[test]
fn f_toml_11_escaped_eq_in_quoted_key() {
    // TOML spec: quoted keys may contain any characters including `=`.
    // The `=` inside the quotes must NOT be treated as the key-value separator.
    let source = "\"path=here\" = \"value\"\n";
    let ranges = classify_toml(source);
    assert_contiguous(&ranges, source.len());
    assert_field_lengths_sum(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::StringLiteral),
        vec!["\"value\""],
        "ranges: {ranges:?}"
    );
}

/// F-TOML-12: a backslash before a line end stays inside its line.
///
/// A basic string whose last byte before the line end is a backslash must not
/// carry the scan past the line end: that put the inline-comment scan out of
/// range and stopped the whole scan. Each input here is a file that holds the
/// bad line.
#[test]
fn f_toml_12_backslash_before_a_line_end_does_not_leave_the_line() {
    for source in [
        "a = \"x\\\n\"\n",
        "a = \"\\\n",
        "a = \"x\\\nb = 1\n",
        "a = \"x\\\r\n",
        "a = \"x\\",
    ] {
        let ranges = classify_toml(source);
        assert_contiguous(&ranges, source.len());
        assert_field_lengths_sum(&ranges, source.len());
    }
}

/// F-TOML-13: the line after a line that ends in a backslash is classified on
/// its own: its string is a string, its comment is a comment.
#[test]
fn f_toml_13_the_line_after_a_backslash_line_end_is_classified_on_its_own() {
    let source = "a = \"x\\\nb = \"y\" # note\n";
    let ranges = classify_toml(source);
    assert_contiguous(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::StringLiteral),
        vec!["\"x\\", "\"y\""],
        "ranges: {ranges:?}"
    );
    assert_eq!(
        field_text(source, &ranges, SearchField::Comment),
        vec!["# note"],
        "ranges: {ranges:?}"
    );
}

// ============================================================================
// Markdown tests
// ============================================================================

/// Parse `source` as Markdown and classify its tree.
fn classify_markdown(source: &str) -> rskim_core::Result<Vec<(Range<usize>, SearchField)>> {
    let mut parser = rskim_core::Parser::new(rskim_core::Language::Markdown)?;
    let tree = parser.parse(source)?;
    Ok(super::markdown::classify_markdown(&tree, source))
}

/// F-MD-01: body text is a Comment, a heading is not.
#[test]
fn f_md_01_body_text_is_comment_heading_is_not() {
    let source = "# Title\n\nBody text.\n";
    let ranges = classify_markdown(source).unwrap();
    assert_contiguous(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::Comment),
        vec!["Body text.\n"],
        "ranges: {ranges:?}"
    );
}

/// F-MD-02: ATX headings and link reference definitions are neither prose nor
/// code.
#[test]
fn f_md_02_headings_and_link_reference_definitions_are_other() {
    let source = "# H1\n\n### H3\n\n#### H4\n\n[ref]: https://example.com\n";
    let ranges = classify_markdown(source).unwrap();
    assert_contiguous(&ranges, source.len());

    assert!(
        !has_field(&ranges, SearchField::Comment) && !has_field(&ranges, SearchField::Code),
        "ranges: {ranges:?}"
    );
}

/// F-MD-03: a fenced code block is Code.
#[test]
fn f_md_03_fenced_code_block_is_code() {
    let source = "# T\n\n```rust\nfn f(){}\n```\n";
    let ranges = classify_markdown(source).unwrap();
    assert_contiguous(&ranges, source.len());

    assert!(
        field_text(source, &ranges, SearchField::Code)
            .iter()
            .any(|t| t.contains("fn f(){}")),
        "ranges: {ranges:?}"
    );
}

/// F-MD-04: a fenced code block in a list item is Code, not the Comment of the
/// list item around it.
#[test]
fn f_md_04_code_block_in_a_list_item_is_code_not_comment() {
    let source = "- item\n\n  ```\n  call();\n  ```\n";
    let ranges = classify_markdown(source).unwrap();
    assert_contiguous(&ranges, source.len());

    let at = source.find("call();").unwrap();
    let field = ranges
        .iter()
        .find(|(range, _)| range.contains(&at))
        .map(|(_, field)| *field);
    assert_eq!(field, Some(SearchField::Code), "ranges: {ranges:?}");
}

/// F-MD-05: a block quote is a Comment.
#[test]
fn f_md_05_blockquote_is_comment() {
    let source = "# T\n\n> quote text\n";
    let ranges = classify_markdown(source).unwrap();
    assert_contiguous(&ranges, source.len());

    assert!(
        has_field(&ranges, SearchField::Comment),
        "blockquote should be Comment; ranges: {ranges:?}"
    );
}

/// F-MD-06: list items are Comment.
#[test]
fn f_md_06_list_is_comment() {
    let source = "# T\n\n- item 1\n- item 2\n";
    let ranges = classify_markdown(source).unwrap();
    assert_contiguous(&ranges, source.len());

    assert!(
        has_field(&ranges, SearchField::Comment),
        "list should be Comment; ranges: {ranges:?}"
    );
}

// ============================================================================
// Contract tests (apply to all serde scanners)
// ============================================================================

/// C-01: JSON output is sorted.
#[test]
fn c_01_json_sorted() {
    let source = r#"{"a":"1","b":"2","c":"3"}"#;
    let ranges = classify_json(source);
    for i in 0..ranges.len().saturating_sub(1) {
        assert!(
            ranges[i].0.start <= ranges[i + 1].0.start,
            "ranges not sorted at index {i}; ranges: {ranges:?}"
        );
    }
}

/// C-02: YAML output is non-overlapping.
#[test]
fn c_02_yaml_non_overlapping() {
    let source = "a:\n  b: c\nd: e\n";
    let ranges = classify_yaml(source);
    for i in 0..ranges.len().saturating_sub(1) {
        assert_eq!(
            ranges[i].0.end,
            ranges[i + 1].0.start,
            "overlap or gap at index {i}; ranges: {ranges:?}"
        );
    }
}

/// C-03: TOML output is contiguous.
#[test]
fn c_03_toml_contiguous() {
    let source = "[pkg]\nname = \"x\"\nversion = \"1.0\"\n";
    let ranges = classify_toml(source);
    assert_contiguous(&ranges, source.len());
}

/// C-04: empty JSON source → empty Vec.
#[test]
fn c_04_empty_json_empty_vec() {
    let ranges = classify_json("");
    assert!(
        ranges.is_empty(),
        "empty JSON source should return empty Vec; got: {ranges:?}"
    );
}

/// C-05: empty YAML source → empty Vec.
#[test]
fn c_05_empty_yaml_empty_vec() {
    let ranges = classify_yaml("");
    assert!(
        ranges.is_empty(),
        "empty YAML source should return empty Vec; got: {ranges:?}"
    );
}

/// C-06: empty TOML source → empty Vec.
#[test]
fn c_06_empty_toml_empty_vec() {
    let ranges = classify_toml("");
    assert!(
        ranges.is_empty(),
        "empty TOML source should return empty Vec; got: {ranges:?}"
    );
}

/// C-07: malformed JSON does not panic, returns valid contiguous output.
#[test]
fn c_07_malformed_json_no_panic() {
    let source = "{bad json!!}";
    let ranges = classify_json(source);
    assert_contiguous(&ranges, source.len());
    assert_field_lengths_sum(&ranges, source.len());
}

/// C-08: malformed YAML does not panic, returns valid contiguous output.
#[test]
fn c_08_malformed_yaml_no_panic() {
    let source = ": this is weird\n  :\n";
    let ranges = classify_yaml(source);
    assert_contiguous(&ranges, source.len());
    assert_field_lengths_sum(&ranges, source.len());
}

/// C-09: malformed TOML does not panic, returns valid contiguous output.
#[test]
fn c_09_malformed_toml_no_panic() {
    let source = "[invalid\nkey = = = value\n";
    let ranges = classify_toml(source);
    assert_contiguous(&ranges, source.len());
    assert_field_lengths_sum(&ranges, source.len());
}

/// C-10: scanners are infallible (classify_json/yaml/toml return Vec, not Result).
/// This is a compile-time property, verified by calling them without error handling.
#[test]
fn c_10_scanners_infallible() {
    let _j = classify_json("{}");
    let _y = classify_yaml("key: val\n");
    let _t = classify_toml("[s]\nk = \"v\"\n");
    // If they compiled and ran without errors, the test passes.
}

// ============================================================================
// Integration tests (through classify_source dispatch)
// ============================================================================

/// I-01: classify_source dispatches JSON to the format-specific scanner.
#[test]
fn i_01_classify_source_json_dispatch() {
    let source = r#"{"name": "skim"}"#;
    let ranges = classify_source(source, rskim_core::Language::Json).unwrap();
    assert_contiguous(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::StringLiteral),
        vec![r#""skim""#],
        "ranges: {ranges:?}"
    );
}

/// I-02: classify_source dispatches YAML to the format-specific scanner.
#[test]
fn i_02_classify_source_yaml_dispatch() {
    let source = "host: \"localhost\"\n";
    let ranges = classify_source(source, rskim_core::Language::Yaml).unwrap();
    assert_contiguous(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::StringLiteral),
        vec!["\"localhost\""],
        "ranges: {ranges:?}"
    );
}

/// I-03: classify_source dispatches TOML to the format-specific scanner.
#[test]
fn i_03_classify_source_toml_dispatch() {
    let source = "[package]\nname = \"skim\"\n";
    let ranges = classify_source(source, rskim_core::Language::Toml).unwrap();
    assert_contiguous(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::StringLiteral),
        vec!["\"skim\""],
        "ranges: {ranges:?}"
    );
}

/// I-04: classify_source dispatches Markdown to the Markdown classifier.
#[test]
fn i_04_classify_source_markdown_dispatch() {
    let source = "# Title\n\nBody.\n";
    let ranges = classify_source(source, rskim_core::Language::Markdown).unwrap();
    assert_contiguous(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::Comment),
        vec!["Body.\n"],
        "ranges: {ranges:?}"
    );
}

/// I-05: classify_source takes the tree-sitter path for a code language.
#[test]
fn i_05_classify_source_rust_unchanged() {
    let source = "fn f() { let s = \"x\"; } // note";
    let ranges = classify_source(source, rskim_core::Language::Rust).unwrap();
    assert_contiguous(&ranges, source.len());

    assert_eq!(
        field_text(source, &ranges, SearchField::StringLiteral),
        vec!["\"x\""],
        "ranges: {ranges:?}"
    );
    assert_eq!(
        field_text(source, &ranges, SearchField::Comment),
        vec!["// note"],
        "ranges: {ranges:?}"
    );
}
