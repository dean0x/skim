//! Data-driven field classifier for tree-sitter languages.
//!
//! Uses `FxHashMap` lookup tables (O(1)) instead of match statements.
//! Each language defines static slices of `(node_kind, SearchField)` pairs.
//! Identifiers are classified only in declaration contexts to avoid
//! indexing every variable reference.

use rskim_core::Language;

use crate::{FieldClassifier, SearchField};

/// Data-driven field classifier using per-language lookup tables.
///
/// Two-level classification:
/// 1. Container nodes (function_declaration, struct_item, etc.) → direct field_map lookup
/// 2. Identifier nodes → parent-context lookup via declaration_parents table
pub struct TreeSitterClassifier {
    _private: (), // placeholder — Phase 1c fills in FxHashMap fields
}

impl TreeSitterClassifier {
    /// Create a classifier for the given language.
    ///
    /// Returns `None` for serde-based languages (JSON, YAML, TOML) and Markdown.
    pub fn for_language(language: Language) -> Option<Self> {
        match language {
            Language::Json | Language::Yaml | Language::Toml | Language::Markdown => None,
            _ => {
                // Phase 1c: populate field_map and declaration_parents per language
                Some(Self { _private: () })
            }
        }
    }
}

impl FieldClassifier for TreeSitterClassifier {
    fn classify_node(
        &self,
        _node: &tree_sitter::Node<'_>,
        _source: &str,
    ) -> Option<SearchField> {
        // Phase 1c: implement data-table driven classification
        None
    }
}
