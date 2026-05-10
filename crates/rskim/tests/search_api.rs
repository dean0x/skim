//! Integration tests exercising the rskim-search public API.
//!
//! These tests establish a real integration point between the rskim binary crate
//! and the rskim-search library. They validate that rskim-search types can be
//! constructed and used from a downstream consumer, catching API breakage before
//! the dependency is re-added as a runtime dependency.
//!
//! # Scope
//!
//! These are pure Rust tests (no CLI invocation). They exercise type construction,
//! trait implementations, and serialization — the surface area a future CLI
//! integration layer would consume.

use rskim_search::{
    FieldClassifier, FileId, IndexStats, LayerBuilder, NodeInfo, SearchError, SearchField,
    SearchLayer, SearchQuery, SearchResult, TemporalFlags,
};

// ============================================================================
// FileId
// ============================================================================

#[test]
fn file_id_constructs_and_compares() {
    let a = FileId(0);
    let b = FileId(1);
    assert_eq!(a, FileId(0));
    assert_ne!(a, b);
    assert!(a < b);
}

// ============================================================================
// SearchQuery
// ============================================================================

#[test]
fn search_query_new_has_no_filters() {
    let q = SearchQuery::new("impl Iterator");
    assert_eq!(q.text, "impl Iterator");
    assert!(q.lang.is_none());
    assert!(q.ast_pattern.is_none());
    assert!(q.temporal_flags.is_none());
    assert!(q.limit.is_none());
    assert!(q.offset.is_none());
}

#[test]
fn search_query_with_all_filters() {
    let q = SearchQuery {
        text: "fn search".to_string(),
        lang: Some(rskim_core::Language::Rust),
        ast_pattern: Some("function_item".to_string()),
        temporal_flags: Some(TemporalFlags {
            modified_within_days: Some(30),
        }),
        limit: Some(10),
        offset: Some(0),
    };
    assert_eq!(q.lang, Some(rskim_core::Language::Rust));
    assert_eq!(q.limit, Some(10));
    let tf = q.temporal_flags.unwrap();
    assert_eq!(tf.modified_within_days, Some(30));
}

// ============================================================================
// SearchField
// ============================================================================

#[test]
fn search_field_name_is_stable() {
    assert_eq!(SearchField::TypeDefinition.name(), "type_definition");
    assert_eq!(SearchField::FunctionSignature.name(), "function_signature");
    assert_eq!(SearchField::FunctionBody.name(), "function_body");
    assert_eq!(SearchField::SymbolName.name(), "symbol_name");
    assert_eq!(SearchField::ImportExport.name(), "import_export");
    assert_eq!(SearchField::Comment.name(), "comment");
    assert_eq!(SearchField::StringLiteral.name(), "string_literal");
    assert_eq!(SearchField::Other.name(), "other");
}

// ============================================================================
// SearchError
// ============================================================================

#[test]
fn search_error_invalid_query_display() {
    let e = SearchError::InvalidQuery("empty string".to_string());
    assert_eq!(format!("{e}"), "Invalid query: empty string");
}

#[test]
fn search_error_index_corrupted_display() {
    let e = SearchError::IndexCorrupted("bad block".to_string());
    assert_eq!(format!("{e}"), "Index corrupted: bad block");
}

#[test]
fn search_error_file_not_found_display() {
    let e = SearchError::FileNotFound(FileId(99));
    assert_eq!(format!("{e}"), "File not found in index: 99");
}

#[test]
fn search_error_from_core_error() {
    let core_err = rskim_core::SkimError::ParseError("bad parse".into());
    let e = SearchError::from(core_err);
    let display = format!("{e}");
    assert!(
        display.contains("bad parse"),
        "Core error message should propagate, got: {display}"
    );
}

// ============================================================================
// IndexStats
// ============================================================================

#[test]
fn index_stats_fields_accessible() {
    let stats = IndexStats {
        file_count: 5,
        total_ngrams: 1_000,
        index_size_bytes: 4096,
        last_updated: Some(1_700_000_000),
    };
    assert_eq!(stats.file_count, 5);
    assert_eq!(stats.total_ngrams, 1_000);
    assert_eq!(stats.index_size_bytes, 4096);
    assert_eq!(stats.last_updated, Some(1_700_000_000));
}

// ============================================================================
// NodeInfo + FieldClassifier trait
// ============================================================================

/// Concrete FieldClassifier that a downstream consumer can implement.
/// Validates the trait's API contract is usable without tree-sitter.
struct SimpleClassifier;

impl FieldClassifier for SimpleClassifier {
    fn classify(&self, node: &NodeInfo, _source: &str) -> SearchField {
        match node.kind {
            "function_item" | "function_definition" => SearchField::FunctionSignature,
            "struct_item" | "class_definition" => SearchField::TypeDefinition,
            "use_declaration" | "import_statement" => SearchField::ImportExport,
            "line_comment" | "block_comment" => SearchField::Comment,
            "string_literal" => SearchField::StringLiteral,
            _ => SearchField::Other,
        }
    }
}

#[test]
fn field_classifier_can_be_implemented_without_tree_sitter() {
    let classifier = SimpleClassifier;

    let fn_node = NodeInfo {
        kind: "function_item",
        byte_range: 0..40,
        named_child_count: 3,
    };
    assert_eq!(
        classifier.classify(&fn_node, "fn foo() -> u32 { 1 }"),
        SearchField::FunctionSignature
    );

    let struct_node = NodeInfo {
        kind: "struct_item",
        byte_range: 0..20,
        named_child_count: 1,
    };
    assert_eq!(
        classifier.classify(&struct_node, "struct Foo;"),
        SearchField::TypeDefinition
    );

    let unknown = NodeInfo {
        kind: "attribute_item",
        byte_range: 0..10,
        named_child_count: 0,
    };
    assert_eq!(
        classifier.classify(&unknown, "#[cfg(test)]"),
        SearchField::Other
    );
}

// ============================================================================
// SearchLayer + LayerBuilder traits (object-safety checks)
// ============================================================================

/// Validates SearchLayer is object-safe: a Box<dyn SearchLayer> can be formed.
/// This guards against accidental breaking of the object-safety contract.
fn _assert_search_layer_is_object_safe(_layer: Box<dyn SearchLayer>) {}

/// Validates LayerBuilder object-safety for the add_file method (build has
/// `where Self: Sized` so it's excluded from the vtable, as intended).
fn _assert_layer_builder_add_file_is_dyn_safe(_builder: &mut dyn LayerBuilder) {}

// ============================================================================
// SearchResult construction
// ============================================================================

#[test]
fn search_result_fields_accessible() {
    let result = SearchResult {
        file_id: FileId(3),
        score: 0.75,
        line_range: 5..15,
        match_positions: vec![0..4, 10..14],
        field: SearchField::FunctionBody,
        snippet: Some("fn process(x: u32)".to_string()),
    };
    assert_eq!(result.file_id, FileId(3));
    assert!((result.score - 0.75).abs() < f64::EPSILON);
    assert_eq!(result.line_range, 5..15);
    assert_eq!(result.match_positions.len(), 2);
    assert_eq!(result.field, SearchField::FunctionBody);
    assert_eq!(result.snippet.as_deref(), Some("fn process(x: u32)"));
}
