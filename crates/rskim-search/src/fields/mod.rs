//! Field classifier implementations for mapping source regions to [`SearchField`] values.
//!
//! Two classification paths:
//! - **Tree-sitter languages** → [`TreeSitterClassifier`] (implements `FieldClassifier` trait)
//! - **Serde-based languages** (JSON, YAML, TOML, Markdown) → standalone `classify_*_fields()` functions
//!
//! The dual-path design exists because `FieldClassifier` takes `&tree_sitter::Node`,
//! which doesn't exist for serde-parsed formats.

pub mod markdown_fields;
pub mod serde_fields;
pub mod tree_sitter_fields;

use std::ops::Range;

use rskim_core::Language;

use crate::{FieldClassifier, SearchField};
use tree_sitter_fields::TreeSitterClassifier;

/// Get a tree-sitter field classifier for the given language.
///
/// Returns `None` for serde-based languages (JSON, YAML, TOML) and Markdown,
/// which use [`classify_serde_fields`] instead.
pub fn for_language(language: Language) -> Option<Box<dyn FieldClassifier>> {
    TreeSitterClassifier::for_language(language)
        .map(|c| Box::new(c) as Box<dyn FieldClassifier>)
}

/// Classify fields for serde-based and non-tree-sitter languages.
///
/// Returns `(byte_range, SearchField)` pairs identifying semantically
/// meaningful regions of the source text.
pub fn classify_serde_fields(
    source: &str,
    language: Language,
) -> crate::Result<Vec<(Range<usize>, SearchField)>> {
    match language {
        Language::Json => serde_fields::classify_json_fields(source),
        Language::Yaml => serde_fields::classify_yaml_fields(source),
        Language::Toml => serde_fields::classify_toml_fields(source),
        Language::Markdown => markdown_fields::classify_markdown_fields(source),
        _ => Ok(vec![]),
    }
}
