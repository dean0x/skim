//! Language-specific AST symbol extractors.
//!
//! Each extractor walks the tree-sitter AST for its language and yields named
//! symbols (function names, type names, import paths) as `ExtractedSymbol`
//! values.
//!
//! # Design
//!
//! Extractors use tree-sitter directly (not rskim-core) because the Language
//! abstraction in rskim-core keeps `to_tree_sitter()` crate-private. We depend
//! on the grammar crates from the workspace directly.

use std::ops::Range;
use std::path::{Path, PathBuf};

use rskim_search::SearchField;

pub mod go;
pub mod python;
pub mod rust_lang;

/// A named symbol extracted from a source file.
#[derive(Debug, Clone)]
pub struct ExtractedSymbol {
    /// The symbol name (e.g. function name, type name, import path segment).
    pub name: String,
    /// Path of the file this symbol was extracted from.
    pub file_path: PathBuf,
    /// The search field category for this symbol.
    pub field: SearchField,
    /// Byte range of the symbol name within the source.
    pub byte_range: Range<usize>,
}

/// Dispatch to the correct language extractor.
///
/// Returns an empty Vec for unsupported languages rather than an error —
/// benchmark will simply skip those files.
pub fn extract_symbols(
    path: &Path,
    content: &str,
    language: rskim_core::Language,
) -> Vec<ExtractedSymbol> {
    match language {
        rskim_core::Language::Rust => rust_lang::extract(path, content),
        rskim_core::Language::Python => python::extract(path, content),
        rskim_core::Language::Go => go::extract(path, content),
        _ => vec![],
    }
}
