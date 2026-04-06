//! Data-driven field classifier for tree-sitter languages.
//!
//! Uses `FxHashMap` lookup tables (O(1)) instead of match statements.
//! Static data tables live in [`super::tree_sitter_tables`]; this module
//! contains only the classifier struct and its construction logic (~35 lines).
//!
//! Identifiers are classified only in declaration contexts to avoid
//! indexing every variable reference.
//!
//! # Caching
//!
//! [`TreeSitterClassifier::for_language`] returns a shared `&'static` reference.
//! Each language's classifier is built once via `OnceLock` and reused across
//! all subsequent calls, eliminating per-call `FxHashMap` allocation.

use std::sync::OnceLock;

use rustc_hash::FxHashMap;

use rskim_core::Language;

use crate::{FieldClassifier, SearchField};

use super::tree_sitter_tables as tables;

// ============================================================================
// Classifier implementation
// ============================================================================

/// Data-driven field classifier using per-language lookup tables.
///
/// Two-level classification:
/// 1. Container nodes (function_declaration, struct_item, etc.) → direct field_map lookup
/// 2. Identifier nodes → parent-context lookup via declaration_parents table
pub struct TreeSitterClassifier {
    /// Direct node-kind → SearchField mapping for container nodes.
    field_map: FxHashMap<&'static str, SearchField>,
    /// Parent-kind → SearchField for identifier nodes in declaration contexts.
    declaration_parents: FxHashMap<&'static str, SearchField>,
}

impl TreeSitterClassifier {
    /// Return a shared reference to the classifier for the given language.
    ///
    /// The classifier is built once per language and cached for the process
    /// lifetime. Returns `None` for serde-based languages (JSON, YAML, TOML)
    /// and Markdown, which use `classify_serde_fields` instead.
    pub fn for_language(language: Language) -> Option<&'static Self> {
        macro_rules! cached {
            ($id:ident, $field_map:expr, $parents:expr) => {{
                static $id: OnceLock<TreeSitterClassifier> = OnceLock::new();
                $id.get_or_init(|| Self::build($field_map, $parents))
            }};
        }

        let classifier = match language {
            Language::Json | Language::Yaml | Language::Toml | Language::Markdown => {
                return None;
            }
            Language::TypeScript => cached!(TS, tables::TS_FIELD_MAP, tables::TS_DECL_PARENTS),
            Language::JavaScript => cached!(JS, tables::JS_FIELD_MAP, tables::JS_DECL_PARENTS),
            Language::Python => cached!(PY, tables::PY_FIELD_MAP, tables::PY_DECL_PARENTS),
            Language::Rust => cached!(RS, tables::RS_FIELD_MAP, tables::RS_DECL_PARENTS),
            Language::Go => cached!(GO, tables::GO_FIELD_MAP, tables::GO_DECL_PARENTS),
            Language::Java => cached!(JAVA, tables::JAVA_FIELD_MAP, tables::JAVA_DECL_PARENTS),
            Language::C => cached!(C, tables::C_FIELD_MAP, tables::C_DECL_PARENTS),
            Language::Cpp => cached!(CPP, tables::CPP_FIELD_MAP, tables::CPP_DECL_PARENTS),
            Language::CSharp => cached!(CS, tables::CS_FIELD_MAP, tables::CS_DECL_PARENTS),
            Language::Ruby => cached!(RB, tables::RB_FIELD_MAP, tables::RB_DECL_PARENTS),
            Language::Kotlin => cached!(KT, tables::KT_FIELD_MAP, tables::KT_DECL_PARENTS),
            Language::Swift => cached!(SWIFT, tables::SWIFT_FIELD_MAP, tables::SWIFT_DECL_PARENTS),
            Language::Sql => cached!(SQL, tables::SQL_FIELD_MAP, tables::SQL_DECL_PARENTS),
        };

        Some(classifier)
    }

    fn build(
        field_pairs: &'static [(&'static str, SearchField)],
        parent_pairs: &'static [(&'static str, SearchField)],
    ) -> Self {
        Self {
            field_map: field_pairs.iter().copied().collect(),
            declaration_parents: parent_pairs.iter().copied().collect(),
        }
    }
}

/// Forward `FieldClassifier` through a shared reference so that
/// `Box::new(&'static classifier)` can coerce to `Box<dyn FieldClassifier>`.
impl FieldClassifier for &TreeSitterClassifier {
    fn classify_node(&self, node: &tree_sitter::Node<'_>, source: &str) -> Option<SearchField> {
        (**self).classify_node(node, source)
    }
}

impl FieldClassifier for TreeSitterClassifier {
    /// Classify a tree-sitter node into a search field.
    ///
    /// Classification logic:
    /// 1. Check `field_map` for a direct node kind match → return if found.
    /// 2. If node kind is `"identifier"`, `"type_identifier"`, or `"property_identifier"`:
    ///    - Check parent node's kind against `declaration_parents`.
    ///    - Only classify identifiers in declaration contexts.
    /// 3. Otherwise return `None`.
    fn classify_node(&self, node: &tree_sitter::Node<'_>, _source: &str) -> Option<SearchField> {
        let kind = node.kind();

        // Step 1: direct container node lookup.
        if let Some(&field) = self.field_map.get(kind) {
            return Some(field);
        }

        // Step 2: identifier in declaration context.
        if matches!(
            kind,
            "identifier" | "type_identifier" | "property_identifier"
        ) {
            if let Some(parent) = node.parent() {
                if let Some(&field) = self.declaration_parents.get(parent.kind()) {
                    return Some(field);
                }
            }
        }

        None
    }
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_languages_return_none() {
        assert!(TreeSitterClassifier::for_language(Language::Json).is_none());
        assert!(TreeSitterClassifier::for_language(Language::Yaml).is_none());
        assert!(TreeSitterClassifier::for_language(Language::Toml).is_none());
        assert!(TreeSitterClassifier::for_language(Language::Markdown).is_none());
    }

    #[test]
    fn tree_sitter_languages_return_some() {
        let langs = [
            Language::TypeScript,
            Language::JavaScript,
            Language::Python,
            Language::Rust,
            Language::Go,
            Language::Java,
            Language::C,
            Language::Cpp,
            Language::CSharp,
            Language::Ruby,
            Language::Kotlin,
            Language::Swift,
            Language::Sql,
        ];
        for lang in langs {
            assert!(
                TreeSitterClassifier::for_language(lang).is_some(),
                "Expected Some for {lang:?}"
            );
        }
    }

    #[test]
    fn same_language_returns_same_pointer() {
        // Caching guarantee: two calls return the same static reference.
        let a = TreeSitterClassifier::for_language(Language::Rust).unwrap();
        let b = TreeSitterClassifier::for_language(Language::Rust).unwrap();
        assert!(
            std::ptr::eq(a, b),
            "for_language(Rust) must return the same cached pointer on every call"
        );
    }

    #[test]
    fn rust_field_map_contains_expected_entries() {
        let classifier =
            TreeSitterClassifier::for_language(Language::Rust).expect("Rust classifier must exist");
        assert_eq!(
            classifier.field_map.get("struct_item"),
            Some(&SearchField::TypeDefinition)
        );
        assert_eq!(
            classifier.field_map.get("function_item"),
            Some(&SearchField::FunctionSignature)
        );
        assert_eq!(
            classifier.field_map.get("use_declaration"),
            Some(&SearchField::ImportExport)
        );
    }

    #[test]
    fn typescript_declaration_parents_contains_expected_entries() {
        let classifier = TreeSitterClassifier::for_language(Language::TypeScript)
            .expect("TypeScript classifier must exist");
        assert_eq!(
            classifier.declaration_parents.get("function_declaration"),
            Some(&SearchField::SymbolName)
        );
        assert_eq!(
            classifier.declaration_parents.get("import_specifier"),
            Some(&SearchField::ImportExport)
        );
    }
}
