//! Integration tests for serde-based and Markdown field classification.
//!
//! Validates:
//! - JSON: top-level keys → TypeDefinition, nested keys → SymbolName, string values → StringLiteral
//! - YAML: top-level keys → TypeDefinition, comments → Comment, nested → SymbolName
//! - TOML: section headers → TypeDefinition, keys → SymbolName, comments → Comment
//! - Markdown: headings → TypeDefinition, code blocks → FunctionBody, links → ImportExport
//! - Empty / malformed inputs → empty vec (graceful degradation)

use rskim_core::Language;
use rskim_search::fields::classify_serde_fields;
use rskim_search::fields::serde_fields::{classify_json_fields, classify_toml_fields, classify_yaml_fields};
use rskim_search::SearchField;

// ============================================================================
// JSON
// ============================================================================

#[test]
fn json_empty_object_returns_empty() {
    let result = classify_serde_fields("{}", Language::Json).expect("no error");
    assert!(result.is_empty());
}

#[test]
fn json_empty_string_returns_empty() {
    let result = classify_serde_fields("", Language::Json).expect("no error");
    assert!(result.is_empty());
}

#[test]
fn json_malformed_returns_empty_not_error() {
    let result = classify_serde_fields("{bad json}", Language::Json).expect("should not error");
    assert!(result.is_empty(), "malformed JSON must degrade gracefully");
}

#[test]
fn json_top_level_key_is_type_definition() {
    let source = r#"{"database_url": "postgres://localhost/db"}"#;
    let result = classify_serde_fields(source, Language::Json).expect("no error");
    let has_type_def = result
        .iter()
        .any(|(_, f)| *f == SearchField::TypeDefinition);
    assert!(
        has_type_def,
        "top-level JSON key must be TypeDefinition; got {result:?}"
    );
}

#[test]
fn json_string_value_is_string_literal() {
    let source = r#"{"name": "alice"}"#;
    let result = classify_serde_fields(source, Language::Json).expect("no error");
    let has_str = result.iter().any(|(_, f)| *f == SearchField::StringLiteral);
    assert!(
        has_str,
        "string value must be StringLiteral; got {result:?}"
    );
}

#[test]
fn json_nested_key_is_symbol_name() {
    let source = r#"{"server": {"host": "localhost"}}"#;
    let result = classify_serde_fields(source, Language::Json).expect("no error");
    let has_symbol = result.iter().any(|(_, f)| *f == SearchField::SymbolName);
    assert!(
        has_symbol,
        "nested JSON key must be SymbolName; got {result:?}"
    );
}

#[test]
fn json_config_fixture() {
    // Use the checked-in fixture for realistic validation.
    let source = include_str!("../../../tests/fixtures/search/config.json");
    let result = classify_serde_fields(source, Language::Json).expect("no error");
    assert!(
        !result.is_empty(),
        "config.json fixture must produce non-empty classification"
    );

    // Should have top-level TypeDefinition keys (database_url, redis_url, server, etc.)
    let has_type_def = result
        .iter()
        .any(|(_, f)| *f == SearchField::TypeDefinition);
    assert!(has_type_def);
}

// ============================================================================
// YAML
// ============================================================================

#[test]
fn yaml_empty_string_returns_empty() {
    let result = classify_serde_fields("", Language::Yaml).expect("no error");
    assert!(result.is_empty());
}

#[test]
fn yaml_top_level_key_is_type_definition() {
    let source = "name: alice\n";
    let result = classify_serde_fields(source, Language::Yaml).expect("no error");
    let has_type_def = result
        .iter()
        .any(|(_, f)| *f == SearchField::TypeDefinition);
    assert!(
        has_type_def,
        "top-level YAML key must be TypeDefinition; got {result:?}"
    );
}

#[test]
fn yaml_nested_key_is_symbol_name() {
    let source = "server:\n  host: localhost\n";
    let result = classify_serde_fields(source, Language::Yaml).expect("no error");
    let has_symbol = result.iter().any(|(_, f)| *f == SearchField::SymbolName);
    assert!(
        has_symbol,
        "nested YAML key must be SymbolName; got {result:?}"
    );
}

#[test]
fn yaml_comment_is_comment() {
    let source = "# this is a comment\nname: test\n";
    let result = classify_serde_fields(source, Language::Yaml).expect("no error");
    let has_comment = result.iter().any(|(_, f)| *f == SearchField::Comment);
    assert!(has_comment, "YAML comment must be Comment; got {result:?}");
}

#[test]
fn yaml_deploy_fixture() {
    let source = include_str!("../../../tests/fixtures/search/deploy.yaml");
    let result = classify_serde_fields(source, Language::Yaml).expect("no error");
    assert!(
        !result.is_empty(),
        "deploy.yaml fixture must produce non-empty classification"
    );
}

// ============================================================================
// TOML
// ============================================================================

#[test]
fn toml_empty_string_returns_empty() {
    let result = classify_serde_fields("", Language::Toml).expect("no error");
    assert!(result.is_empty());
}

#[test]
fn toml_section_header_is_type_definition() {
    let source = "[package]\nname = \"skim\"\n";
    let result = classify_serde_fields(source, Language::Toml).expect("no error");
    let has_type_def = result
        .iter()
        .any(|(_, f)| *f == SearchField::TypeDefinition);
    assert!(
        has_type_def,
        "[section] must be TypeDefinition; got {result:?}"
    );
}

#[test]
fn toml_key_is_symbol_name() {
    let source = "name = \"skim\"\n";
    let result = classify_serde_fields(source, Language::Toml).expect("no error");
    let has_symbol = result.iter().any(|(_, f)| *f == SearchField::SymbolName);
    assert!(has_symbol, "TOML key must be SymbolName; got {result:?}");
}

#[test]
fn toml_comment_is_comment() {
    let source = "# a comment\nname = \"x\"\n";
    let result = classify_serde_fields(source, Language::Toml).expect("no error");
    let has_comment = result.iter().any(|(_, f)| *f == SearchField::Comment);
    assert!(
        has_comment,
        "TOML # comment must be Comment; got {result:?}"
    );
}

#[test]
fn toml_string_value_is_string_literal() {
    let source = "name = \"skim\"\n";
    let result = classify_serde_fields(source, Language::Toml).expect("no error");
    let has_str = result.iter().any(|(_, f)| *f == SearchField::StringLiteral);
    assert!(
        has_str,
        "TOML string value must be StringLiteral; got {result:?}"
    );
}

// ============================================================================
// Markdown
// ============================================================================

#[test]
fn markdown_empty_string_returns_empty() {
    let result = classify_serde_fields("", Language::Markdown).expect("no error");
    assert!(result.is_empty());
}

#[test]
fn markdown_h1_is_type_definition() {
    let source = "# Title\n";
    let result = classify_serde_fields(source, Language::Markdown).expect("no error");
    let has_type_def = result
        .iter()
        .any(|(_, f)| *f == SearchField::TypeDefinition);
    assert!(
        has_type_def,
        "H1 heading must be TypeDefinition; got {result:?}"
    );
}

#[test]
fn markdown_code_block_is_function_body() {
    let source = "```rust\nfn main() {}\n```\n";
    let result = classify_serde_fields(source, Language::Markdown).expect("no error");
    let has_fn_body = result.iter().any(|(_, f)| *f == SearchField::FunctionBody);
    assert!(
        has_fn_body,
        "code block must be FunctionBody; got {result:?}"
    );
}

#[test]
fn markdown_link_is_import_export() {
    let source = "Check [here](https://example.com) for details.\n";
    let result = classify_serde_fields(source, Language::Markdown).expect("no error");
    let has_import = result.iter().any(|(_, f)| *f == SearchField::ImportExport);
    assert!(
        has_import,
        "markdown link must be ImportExport; got {result:?}"
    );
}

#[test]
fn markdown_prose_is_comment() {
    let source = "This is ordinary prose without any links.\n";
    let result = classify_serde_fields(source, Language::Markdown).expect("no error");
    let has_comment = result.iter().any(|(_, f)| *f == SearchField::Comment);
    assert!(has_comment, "prose must be Comment; got {result:?}");
}

#[test]
fn markdown_readme_fixture() {
    let source = include_str!("../../../tests/fixtures/search/README.md");
    let result = classify_serde_fields(source, Language::Markdown).expect("no error");
    assert!(
        !result.is_empty(),
        "README.md fixture must produce non-empty classification"
    );
}

// ============================================================================
// Non-serde languages return empty vec
// ============================================================================

#[test]
fn non_serde_language_returns_empty() {
    // TypeScript is a tree-sitter language — classify_serde_fields returns empty.
    let result =
        classify_serde_fields("function foo() {}", Language::TypeScript).expect("no error");
    assert!(
        result.is_empty(),
        "tree-sitter language must return empty from classify_serde_fields"
    );
}

// ============================================================================
// Byte ranges are valid
// ============================================================================

#[test]
fn json_byte_ranges_within_source_bounds() {
    let source = r#"{"key": "value", "nested": {"inner": "x"}}"#;
    let result = classify_serde_fields(source, Language::Json).expect("no error");
    for (range, _) in &result {
        assert!(
            range.end <= source.len(),
            "range {:?} exceeds source length {}",
            range,
            source.len()
        );
    }
}

#[test]
fn yaml_byte_ranges_within_source_bounds() {
    let source = "name: alice\nserver:\n  host: localhost\n";
    let result = classify_serde_fields(source, Language::Yaml).expect("no error");
    for (range, _) in &result {
        assert!(
            range.end <= source.len(),
            "range {:?} exceeds source length {}",
            range,
            source.len()
        );
    }
}

#[test]
fn toml_byte_ranges_within_source_bounds() {
    let source = "[package]\nname = \"skim\"\nversion = \"1.0\"\n";
    let result = classify_serde_fields(source, Language::Toml).expect("no error");
    for (range, _) in &result {
        assert!(
            range.end <= source.len(),
            "range {:?} exceeds source length {}",
            range,
            source.len()
        );
    }
}

// ============================================================================
// Low-level function tests (previously inline in serde_fields.rs)
// These exercise classify_{json,yaml,toml}_fields directly to cover edge cases
// not reachable through the public classify_serde_fields wrapper.
// ============================================================================

// ---- JSON low-level ----

#[test]
fn json_low_level_empty_object_is_empty() {
    let result = classify_json_fields("{}").expect("should succeed");
    assert!(result.is_empty());
}

#[test]
fn json_low_level_malformed_returns_empty() {
    let result = classify_json_fields("{not valid json").expect("should succeed");
    assert!(result.is_empty());
}

#[test]
fn json_low_level_top_level_key_is_type_definition() {
    let source = r#"{"name": "alice"}"#;
    let result = classify_json_fields(source).expect("should succeed");
    let has_type_def = result
        .iter()
        .any(|(_, f)| *f == SearchField::TypeDefinition);
    assert!(has_type_def, "top-level JSON key should be TypeDefinition");
}

#[test]
fn json_low_level_string_value_is_string_literal() {
    let source = r#"{"name": "alice"}"#;
    let result = classify_json_fields(source).expect("should succeed");
    let has_str_lit = result.iter().any(|(_, f)| *f == SearchField::StringLiteral);
    assert!(has_str_lit, "string value should be StringLiteral");
}

#[test]
fn json_low_level_duplicate_string_values_get_distinct_offsets() {
    // Two keys with the same string value. Both occurrences should be found
    // and their ranges should be distinct (i.e. the second search picks up
    // the second occurrence, not the first again).
    let source = r#"{"a": "x", "b": "x"}"#;
    let result = classify_json_fields(source).expect("should succeed");
    let string_lits: Vec<_> = result
        .iter()
        .filter(|(_, f)| *f == SearchField::StringLiteral)
        .collect();
    // Expect two StringLiteral spans for the two "x" values.
    assert_eq!(string_lits.len(), 2, "should find both \"x\" occurrences");
    // They must be at different offsets.
    assert_ne!(string_lits[0].0.start, string_lits[1].0.start);
}

#[test]
fn json_low_level_deeply_nested_does_not_panic() {
    // Build JSON nested 100 levels deep — beyond MAX_JSON_DEPTH (64).
    // The classifier should return without stack overflow.
    let mut s = String::new();
    for _ in 0..100 {
        s.push_str(r#"{"k": "#);
    }
    s.push('1');
    for _ in 0..100 {
        s.push('}');
    }
    // We just need it not to panic; any result is acceptable.
    let _result = classify_json_fields(&s).expect("should succeed");
}

// ---- YAML low-level ----

#[test]
fn yaml_low_level_empty_string_is_empty() {
    let result = classify_yaml_fields("").expect("should succeed");
    assert!(result.is_empty());
}

#[test]
fn yaml_low_level_comment_line_is_comment() {
    let source = "# this is a comment\nname: alice\n";
    let result = classify_yaml_fields(source).expect("should succeed");
    let has_comment = result.iter().any(|(_, f)| *f == SearchField::Comment);
    assert!(has_comment, "comment line should be Comment");
}

#[test]
fn yaml_low_level_top_level_key_is_type_definition() {
    let source = "name: alice\n";
    let result = classify_yaml_fields(source).expect("should succeed");
    let has_type_def = result
        .iter()
        .any(|(_, f)| *f == SearchField::TypeDefinition);
    assert!(has_type_def, "top-level YAML key should be TypeDefinition");
}

#[test]
fn yaml_low_level_nested_key_is_symbol_name() {
    let source = "server:\n  host: localhost\n";
    let result = classify_yaml_fields(source).expect("should succeed");
    let has_symbol = result.iter().any(|(_, f)| *f == SearchField::SymbolName);
    assert!(has_symbol, "nested YAML key should be SymbolName");
}

#[test]
fn yaml_low_level_crlf_byte_ranges_within_bounds() {
    // CRLF-terminated YAML. Each line separator is 2 bytes (\r\n).
    // Without newline_len, byte_offset drifts +1 per line causing
    // spans to exceed source.len() on the second line onwards.
    let source = "name: alice\r\nage: 30\r\n";
    let result = classify_yaml_fields(source).expect("should succeed");
    assert!(!result.is_empty(), "expected spans for CRLF YAML source");
    for (range, _) in &result {
        assert!(
            range.end <= source.len(),
            "YAML range {:?} out of bounds for source len {}",
            range,
            source.len()
        );
    }
}

// ---- TOML low-level ----

#[test]
fn toml_low_level_empty_string_is_empty() {
    let result = classify_toml_fields("").expect("should succeed");
    assert!(result.is_empty());
}

#[test]
fn toml_low_level_section_header_is_type_definition() {
    let source = "[package]\nname = \"skim\"\n";
    let result = classify_toml_fields(source).expect("should succeed");
    let has_type_def = result
        .iter()
        .any(|(_, f)| *f == SearchField::TypeDefinition);
    assert!(has_type_def, "[section] should be TypeDefinition");
}

#[test]
fn toml_low_level_key_is_symbol_name() {
    let source = "name = \"skim\"\n";
    let result = classify_toml_fields(source).expect("should succeed");
    let has_symbol = result.iter().any(|(_, f)| *f == SearchField::SymbolName);
    assert!(has_symbol, "TOML key should be SymbolName");
}

#[test]
fn toml_low_level_comment_is_comment() {
    let source = "# a comment\nname = \"x\"\n";
    let result = classify_toml_fields(source).expect("should succeed");
    let has_comment = result.iter().any(|(_, f)| *f == SearchField::Comment);
    assert!(has_comment, "# comment should be Comment");
}

#[test]
fn toml_low_level_crlf_byte_ranges_within_bounds() {
    // CRLF-terminated TOML. Without newline_len, offset drifts +1 per line.
    let source = "[pkg]\r\nname = \"x\"\r\n";
    let result = classify_toml_fields(source).expect("should succeed");
    assert!(!result.is_empty(), "expected spans for CRLF TOML source");
    for (range, _) in &result {
        assert!(
            range.end <= source.len(),
            "TOML range {:?} out of bounds for source len {}",
            range,
            source.len()
        );
    }
}
