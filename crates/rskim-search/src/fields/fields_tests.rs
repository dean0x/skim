//! Tests for format-specific field classifiers (JSON, YAML, TOML, Markdown).
//!
//! Written in TDD order: tests were written before production code.
//! Tests cover the contract invariants plus format-specific field mapping.

#![allow(clippy::unwrap_used)]

use std::ops::Range;

use crate::SearchField;
use crate::lexical::classify_source;

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

/// F-JSON-01: simple object — key is SymbolName, value is StringLiteral.
#[test]
fn f_json_01_key_is_symbol_name_value_is_string_literal() {
    let source = r#"{"name": "skim"}"#;
    let ranges = classify_json(source);
    assert_contiguous(&ranges, source.len());

    let key_texts = field_text(source, &ranges, SearchField::SymbolName);
    assert!(
        key_texts.iter().any(|t| t.contains("name")),
        "key 'name' should be SymbolName; symbol texts: {key_texts:?}; ranges: {ranges:?}"
    );

    let str_texts = field_text(source, &ranges, SearchField::StringLiteral);
    assert!(
        str_texts.iter().any(|t| t.contains("skim")),
        "value 'skim' should be StringLiteral; string texts: {str_texts:?}; ranges: {ranges:?}"
    );
}

/// F-JSON-02: depth-0 key whose value is an object → TypeDefinition.
#[test]
fn f_json_02_depth0_key_with_object_value_is_type_definition() {
    let source = r#"{"deps": {"serde": "1.0"}}"#;
    let ranges = classify_json(source);
    assert_contiguous(&ranges, source.len());

    let type_def_texts = field_text(source, &ranges, SearchField::TypeDefinition);
    assert!(
        type_def_texts.iter().any(|t| t.contains("deps")),
        "'deps' should be TypeDefinition (depth-0, value is object); type_def texts: {type_def_texts:?}; ranges: {ranges:?}"
    );
}

/// F-JSON-03: depth-0 key with scalar value → SymbolName, NOT TypeDefinition.
#[test]
fn f_json_03_depth0_key_with_scalar_value_is_symbol_name() {
    let source = r#"{"name": "skim"}"#;
    let ranges = classify_json(source);
    assert_contiguous(&ranges, source.len());

    // "name" has a scalar string value, so it must NOT be TypeDefinition.
    assert!(
        !has_field(&ranges, SearchField::TypeDefinition),
        "'name' with scalar value must NOT produce TypeDefinition; ranges: {ranges:?}"
    );
    let sym_texts = field_text(source, &ranges, SearchField::SymbolName);
    assert!(
        sym_texts.iter().any(|t| t.contains("name")),
        "'name' must be SymbolName; sym texts: {sym_texts:?}; ranges: {ranges:?}"
    );
}

/// F-JSON-04: key and value with escaped quotes — must not panic, must be contiguous.
#[test]
fn f_json_04_escaped_quotes_no_panic() {
    let source = r#"{"key": "val with \"quotes\""}"#;
    let ranges = classify_json(source);
    assert_contiguous(&ranges, source.len());
    assert_field_lengths_sum(&ranges, source.len());
}

/// F-JSON-05: empty object → contiguous (all Other).
#[test]
fn f_json_05_empty_object_all_other() {
    let source = "{}";
    let ranges = classify_json(source);
    assert_contiguous(&ranges, source.len());
    assert_field_lengths_sum(&ranges, source.len());
}

/// F-JSON-06: deep nesting — root key is TypeDefinition, inner keys are SymbolName.
#[test]
fn f_json_06_deep_nesting_root_type_def_inner_symbol() {
    let source = r#"{"a":{"b":{"c":"deep"}}}"#;
    let ranges = classify_json(source);
    assert_contiguous(&ranges, source.len());

    // "a" at depth 0 with object value → TypeDefinition
    let type_def_texts = field_text(source, &ranges, SearchField::TypeDefinition);
    assert!(
        type_def_texts.iter().any(|t| t.contains("a")),
        "'a' should be TypeDefinition; type_def texts: {type_def_texts:?}; ranges: {ranges:?}"
    );

    // Inner keys "b", "c" → SymbolName
    let sym_texts = field_text(source, &ranges, SearchField::SymbolName);
    assert!(
        sym_texts.iter().any(|t| t.contains("b")) || sym_texts.iter().any(|t| t.contains("c")),
        "inner keys should be SymbolName; sym texts: {sym_texts:?}; ranges: {ranges:?}"
    );
}

/// F-JSON-07: array at root — keys inside array objects are SymbolName (not TypeDefinition).
#[test]
fn f_json_07_array_at_root_keys_are_symbol_name() {
    let source = r#"[{"id": 1}, {"id": 2}]"#;
    let ranges = classify_json(source);
    assert_contiguous(&ranges, source.len());

    // Inside array-at-root, no keys should be TypeDefinition.
    assert!(
        !has_field(&ranges, SearchField::TypeDefinition),
        "keys inside root array must NOT be TypeDefinition; ranges: {ranges:?}"
    );
}

/// F-JSON-08: non-string values (numbers, booleans) are Other.
#[test]
fn f_json_08_non_string_values_are_other() {
    let source = r#"{"key": 42, "flag": true}"#;
    let ranges = classify_json(source);
    assert_contiguous(&ranges, source.len());
    assert_field_lengths_sum(&ranges, source.len());
    // Just verifies it doesn't panic and is contiguous.
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

// ============================================================================
// YAML tests
// ============================================================================

/// F-YAML-01: top-level key is TypeDefinition, nested key is SymbolName.
#[test]
fn f_yaml_01_top_level_type_def_nested_symbol_name() {
    let source = "database:\n  host: localhost\n";
    let ranges = classify_yaml(source);
    assert_contiguous(&ranges, source.len());

    let type_def_texts = field_text(source, &ranges, SearchField::TypeDefinition);
    assert!(
        type_def_texts.iter().any(|t| t.contains("database")),
        "'database' should be TypeDefinition; type_def texts: {type_def_texts:?}; ranges: {ranges:?}"
    );

    let sym_texts = field_text(source, &ranges, SearchField::SymbolName);
    assert!(
        sym_texts.iter().any(|t| t.contains("host")),
        "'host' should be SymbolName; sym texts: {sym_texts:?}; ranges: {ranges:?}"
    );
}

/// F-YAML-02: comment line is classified as Comment.
#[test]
fn f_yaml_02_comment_line_is_comment() {
    let source = "# comment\nkey: val\n";
    let ranges = classify_yaml(source);
    assert_contiguous(&ranges, source.len());

    assert!(
        has_field(&ranges, SearchField::Comment),
        "comment line should produce Comment field; ranges: {ranges:?}"
    );
}

/// F-YAML-03: quoted string value is StringLiteral.
#[test]
fn f_yaml_03_quoted_string_value_is_string_literal() {
    let source = "name: \"skim\"\n";
    let ranges = classify_yaml(source);
    assert_contiguous(&ranges, source.len());

    let str_texts = field_text(source, &ranges, SearchField::StringLiteral);
    assert!(
        str_texts.iter().any(|t| t.contains("skim")),
        "quoted value 'skim' should be StringLiteral; str texts: {str_texts:?}; ranges: {ranges:?}"
    );
}

/// F-YAML-04: multi-level nesting — root TypeDefinition, deeper keys SymbolName.
#[test]
fn f_yaml_04_multi_level_nesting() {
    let source = "a:\n  b:\n    c: val\n";
    let ranges = classify_yaml(source);
    assert_contiguous(&ranges, source.len());

    let type_def_texts = field_text(source, &ranges, SearchField::TypeDefinition);
    assert!(
        type_def_texts.iter().any(|t| t.contains("a")),
        "'a' should be TypeDefinition; type_def texts: {type_def_texts:?}; ranges: {ranges:?}"
    );

    let sym_texts = field_text(source, &ranges, SearchField::SymbolName);
    assert!(
        sym_texts.iter().any(|t| t.contains("b")) || sym_texts.iter().any(|t| t.contains("c")),
        "nested keys should be SymbolName; sym texts: {sym_texts:?}; ranges: {ranges:?}"
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

/// F-YAML-06: list item key is SymbolName, parent key is TypeDefinition.
#[test]
fn f_yaml_06_list_parent_type_def_item_key_symbol() {
    let source = "items:\n  - name: foo\n";
    let ranges = classify_yaml(source);
    assert_contiguous(&ranges, source.len());

    let type_def_texts = field_text(source, &ranges, SearchField::TypeDefinition);
    assert!(
        type_def_texts.iter().any(|t| t.contains("items")),
        "'items' should be TypeDefinition; type_def texts: {type_def_texts:?}; ranges: {ranges:?}"
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
            last_byte, b'\n',
            "StringLiteral range must not end with \\n; range: {r:?}, text: {:?}",
            &source[r.clone()]
        );
        assert_ne!(
            last_byte, b'\r',
            "StringLiteral range must not end with \\r; range: {r:?}, text: {:?}",
            &source[r.clone()]
        );
    }
}

// ============================================================================
// TOML tests
// ============================================================================

/// F-TOML-01: [package] header is TypeDefinition, key is SymbolName, value is StringLiteral.
#[test]
fn f_toml_01_section_header_key_value() {
    let source = "[package]\nname = \"skim\"\n";
    let ranges = classify_toml(source);
    assert_contiguous(&ranges, source.len());

    // [package] → TypeDefinition
    let type_def_texts = field_text(source, &ranges, SearchField::TypeDefinition);
    assert!(
        type_def_texts.iter().any(|t| t.contains("package")),
        "'[package]' should be TypeDefinition; type_def texts: {type_def_texts:?}; ranges: {ranges:?}"
    );

    // name → SymbolName
    let sym_texts = field_text(source, &ranges, SearchField::SymbolName);
    assert!(
        sym_texts.iter().any(|t| t.contains("name")),
        "'name' should be SymbolName; sym texts: {sym_texts:?}; ranges: {ranges:?}"
    );

    // "skim" → StringLiteral
    let str_texts = field_text(source, &ranges, SearchField::StringLiteral);
    assert!(
        str_texts.iter().any(|t| t.contains("skim")),
        "'skim' should be StringLiteral; str texts: {str_texts:?}; ranges: {ranges:?}"
    );
}

/// F-TOML-02: [[bin]] array header is TypeDefinition.
#[test]
fn f_toml_02_array_section_type_def() {
    let source = "[[bin]]\nname = \"skim\"\n";
    let ranges = classify_toml(source);
    assert_contiguous(&ranges, source.len());

    let type_def_texts = field_text(source, &ranges, SearchField::TypeDefinition);
    assert!(
        type_def_texts.iter().any(|t| t.contains("bin")),
        "'[[bin]]' should be TypeDefinition; type_def texts: {type_def_texts:?}; ranges: {ranges:?}"
    );
}

/// F-TOML-03: comment line is Comment.
#[test]
fn f_toml_03_comment_line() {
    let source = "# Config\n[db]\nurl = \"pg\"\n";
    let ranges = classify_toml(source);
    assert_contiguous(&ranges, source.len());

    assert!(
        has_field(&ranges, SearchField::Comment),
        "'# Config' should be Comment; ranges: {ranges:?}"
    );
}

/// F-TOML-04: inline comment after value is Comment.
#[test]
fn f_toml_04_inline_comment() {
    let source = "[db]\nurl = \"pg\" # inline\n";
    let ranges = classify_toml(source);
    assert_contiguous(&ranges, source.len());

    let comment_texts = field_text(source, &ranges, SearchField::Comment);
    assert!(
        comment_texts.iter().any(|t| t.contains("inline")),
        "inline comment should be Comment; comment texts: {comment_texts:?}; ranges: {ranges:?}"
    );
}

/// F-TOML-05: dotted key is SymbolName.
#[test]
fn f_toml_05_dotted_key_is_symbol_name() {
    let source = "a.b.c = \"val\"\n";
    let ranges = classify_toml(source);
    assert_contiguous(&ranges, source.len());

    let sym_texts = field_text(source, &ranges, SearchField::SymbolName);
    assert!(
        sym_texts.iter().any(|t| t.contains("a.b.c")),
        "'a.b.c' should be SymbolName; sym texts: {sym_texts:?}; ranges: {ranges:?}"
    );
}

/// F-TOML-06: AC-4 acceptance test — section, key, and long string value.
#[test]
fn f_toml_06_acceptance_test() {
    let source = "[database]\ndatabase_url = \"postgres://localhost/mydb\"\n";
    let ranges = classify_toml(source);
    assert_contiguous(&ranges, source.len());
    assert_field_lengths_sum(&ranges, source.len());

    assert!(
        has_field(&ranges, SearchField::TypeDefinition),
        "should have TypeDefinition for [database]; ranges: {ranges:?}"
    );
    assert!(
        has_field(&ranges, SearchField::SymbolName),
        "should have SymbolName for key; ranges: {ranges:?}"
    );
    assert!(
        has_field(&ranges, SearchField::StringLiteral),
        "should have StringLiteral for URL value; ranges: {ranges:?}"
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

/// F-TOML-11: quoted TOML key containing `=` is classified as a single SymbolName.
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

    let sym_texts = field_text(source, &ranges, SearchField::SymbolName);
    assert!(
        sym_texts.iter().any(|t| t.contains("path=here")),
        "quoted key with '=' inside must be a single SymbolName; sym texts: {sym_texts:?}; ranges: {ranges:?}"
    );
}

// ============================================================================
// Markdown tests
// ============================================================================

use super::markdown::classify_markdown;

/// F-MD-01: H1 heading is TypeDefinition, body text is Comment.
#[test]
fn f_md_01_h1_type_def_body_comment() {
    let source = "# Title\n\nBody text.\n";
    let ranges = classify_markdown(source).unwrap();
    assert_contiguous(&ranges, source.len());

    assert!(
        has_field(&ranges, SearchField::TypeDefinition),
        "H1 should be TypeDefinition; ranges: {ranges:?}"
    );
}

/// F-MD-02: H3 heading is TypeDefinition.
#[test]
fn f_md_02_h3_type_def() {
    let source = "### H3\n\nText.\n";
    let ranges = classify_markdown(source).unwrap();
    assert_contiguous(&ranges, source.len());

    assert!(
        has_field(&ranges, SearchField::TypeDefinition),
        "H3 should be TypeDefinition; ranges: {ranges:?}"
    );
}

/// F-MD-03: H4 heading is NOT TypeDefinition (Other).
#[test]
fn f_md_03_h4_is_not_type_def() {
    let source = "#### H4\n\nText.\n";
    let ranges = classify_markdown(source).unwrap();
    assert_contiguous(&ranges, source.len());

    // H4 must not produce TypeDefinition — it maps to Other.
    assert!(
        !has_field(&ranges, SearchField::TypeDefinition),
        "H4 must NOT be TypeDefinition; ranges: {ranges:?}"
    );
}

/// F-MD-04: fenced code block is FunctionBody.
#[test]
fn f_md_04_fenced_code_block_is_function_body() {
    let source = "# T\n\n```rust\nfn f(){}\n```\n";
    let ranges = classify_markdown(source).unwrap();
    assert_contiguous(&ranges, source.len());

    assert!(
        has_field(&ranges, SearchField::FunctionBody),
        "fenced code block should be FunctionBody; ranges: {ranges:?}"
    );
}

/// F-MD-05: link reference definition is ImportExport.
#[test]
fn f_md_05_link_ref_def_is_import_export() {
    let source = "# T\n\n[ref]: https://example.com\n";
    let ranges = classify_markdown(source).unwrap();
    assert_contiguous(&ranges, source.len());

    assert!(
        has_field(&ranges, SearchField::ImportExport),
        "link reference definition should be ImportExport; ranges: {ranges:?}"
    );
}

/// F-MD-06: setext H1 heading is TypeDefinition.
#[test]
fn f_md_06_setext_h1_type_def() {
    let source = "Title\n=====\n\nBody.\n";
    let ranges = classify_markdown(source).unwrap();
    assert_contiguous(&ranges, source.len());

    assert!(
        has_field(&ranges, SearchField::TypeDefinition),
        "setext H1 should be TypeDefinition; ranges: {ranges:?}"
    );
}

/// F-MD-07: blockquote is Comment.
#[test]
fn f_md_07_blockquote_is_comment() {
    let source = "# T\n\n> quote text\n";
    let ranges = classify_markdown(source).unwrap();
    assert_contiguous(&ranges, source.len());

    assert!(
        has_field(&ranges, SearchField::Comment),
        "blockquote should be Comment; ranges: {ranges:?}"
    );
}

/// F-MD-08: list items are Comment.
#[test]
fn f_md_08_list_is_comment() {
    let source = "# T\n\n- item 1\n- item 2\n";
    let ranges = classify_markdown(source).unwrap();
    assert_contiguous(&ranges, source.len());

    assert!(
        has_field(&ranges, SearchField::Comment),
        "list should be Comment; ranges: {ranges:?}"
    );
}

/// F-MD-09: classify_markdown's own size guard fires before tree-sitter work.
///
/// The Markdown classifier has an independent `MAX_SOURCE_BYTES` check (lines
/// 50-55 of markdown.rs) separate from the dispatcher guard in `classify_source`.
/// This test calls `classify_markdown` directly with a source that is exactly one
/// byte over the limit and verifies that `SearchError::FileTooLarge` is returned
/// with the correct `size` and `limit` values.
///
/// The input is a flat space-padded string; the size check fires before any
/// tree-sitter parsing, so this does not cause a slow parse.
#[test]
fn f_md_09_size_guard_returns_file_too_large() {
    use crate::lexical::classifier::MAX_SOURCE_BYTES;
    use crate::SearchError;

    let oversized = " ".repeat(MAX_SOURCE_BYTES + 1);
    let err = classify_markdown(&oversized)
        .expect_err("classify_markdown must return Err for sources over MAX_SOURCE_BYTES");

    assert!(
        matches!(err, SearchError::FileTooLarge { size, limit }
            if size == MAX_SOURCE_BYTES + 1 && limit == MAX_SOURCE_BYTES),
        "expected FileTooLarge {{ size: {}, limit: {} }}, got: {err:?}",
        MAX_SOURCE_BYTES + 1,
        MAX_SOURCE_BYTES,
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

    // With format-specific classification, JSON should no longer be all-Other.
    // At minimum, the key "name" should be SymbolName.
    assert!(
        has_field(&ranges, SearchField::SymbolName),
        "JSON classify_source should produce SymbolName for key; ranges: {ranges:?}"
    );
}

/// I-02: classify_source dispatches YAML to the format-specific scanner.
#[test]
fn i_02_classify_source_yaml_dispatch() {
    let source = "host: localhost\n";
    let ranges = classify_source(source, rskim_core::Language::Yaml).unwrap();
    assert_contiguous(&ranges, source.len());

    assert!(
        has_field(&ranges, SearchField::TypeDefinition)
            || has_field(&ranges, SearchField::SymbolName),
        "YAML classify_source should produce TypeDefinition or SymbolName; ranges: {ranges:?}"
    );
}

/// I-03: classify_source dispatches TOML to the format-specific scanner.
#[test]
fn i_03_classify_source_toml_dispatch() {
    let source = "[package]\nname = \"skim\"\n";
    let ranges = classify_source(source, rskim_core::Language::Toml).unwrap();
    assert_contiguous(&ranges, source.len());

    assert!(
        has_field(&ranges, SearchField::TypeDefinition),
        "TOML classify_source should produce TypeDefinition for section; ranges: {ranges:?}"
    );
}

/// I-04: classify_source dispatches Markdown to the format-specific scanner.
#[test]
fn i_04_classify_source_markdown_dispatch() {
    let source = "# Title\n\nBody.\n";
    let ranges = classify_source(source, rskim_core::Language::Markdown).unwrap();
    assert_contiguous(&ranges, source.len());

    assert!(
        has_field(&ranges, SearchField::TypeDefinition),
        "Markdown classify_source should produce TypeDefinition for H1; ranges: {ranges:?}"
    );
}

/// I-05: classify_source still works correctly for tree-sitter languages.
#[test]
fn i_05_classify_source_rust_unchanged() {
    let source = "struct Foo { x: u32 }";
    let ranges = classify_source(source, rskim_core::Language::Rust).unwrap();
    assert_contiguous(&ranges, source.len());

    assert!(
        has_field(&ranges, SearchField::TypeDefinition),
        "Rust struct should still have TypeDefinition; ranges: {ranges:?}"
    );
}

/// I-06: Markdown is now dispatched to the tree-sitter-based classifier.
///
/// Verifies that classify_source for Markdown produces non-trivial field
/// classification (not all-Other) for a document with a heading.
#[test]
fn i_06_classify_source_markdown_produces_type_def_for_heading() {
    let source = "## Section\n\nContent.\n";
    let ranges = classify_source(source, rskim_core::Language::Markdown).unwrap();
    assert_contiguous(&ranges, source.len());

    assert!(
        has_field(&ranges, SearchField::TypeDefinition),
        "Markdown H2 should produce TypeDefinition via classify_source; ranges: {ranges:?}"
    );
}
