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
pub(super) mod tree_sitter_tables;

// ============================================================================
// Shared line-scanning helpers
// ============================================================================

/// Returns the byte width of the newline sequence that follows the character at
/// `end_of_line` in `source`.
///
/// `str::lines()` strips both `\n` and `\r\n`, and the last line of a file may
/// have no trailing newline at all. Unconditionally adding 1 would cause the
/// byte offset to drift by 1 for every CRLF line. This function inspects the
/// raw bytes to return the correct separator width (0, 1, or 2).
#[inline]
pub(super) fn newline_len(source: &str, end_of_line: usize) -> usize {
    let bytes = source.as_bytes();
    match bytes.get(end_of_line) {
        Some(&b'\r') => {
            if bytes.get(end_of_line + 1) == Some(&b'\n') {
                2
            } else {
                1
            }
        }
        Some(&b'\n') => 1,
        _ => 0, // no trailing newline (last line of file)
    }
}

use std::ops::Range;

use rskim_core::Language;

use crate::{FieldClassifier, SearchField};
use tree_sitter_fields::TreeSitterClassifier;

/// Get a tree-sitter field classifier for the given language.
///
/// Returns `None` for serde-based languages (JSON, YAML, TOML) and Markdown,
/// which use [`classify_serde_fields`] instead.
///
/// The returned classifier is backed by a static `OnceLock` cache — no heap
/// allocation occurs on subsequent calls for the same language.
pub fn for_language(language: Language) -> Option<Box<dyn FieldClassifier>> {
    TreeSitterClassifier::for_language(language).map(|c| Box::new(c) as Box<dyn FieldClassifier>)
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
