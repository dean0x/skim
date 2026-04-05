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
