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
//!
//! The `walk_ast` helper eliminates the parser-setup / tree-walk boilerplate
//! that was duplicated across all three language modules.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    pub file_path: Arc<PathBuf>,
    /// The search field category for this symbol.
    pub field: SearchField,
    /// Byte range of the symbol name within the source.
    pub byte_range: Range<usize>,
}

/// Walk a tree-sitter AST and collect extracted symbols using a pre-created parser.
///
/// Accepts a mutable reference to an already-configured `Parser` so that
/// callers processing many files can reuse a single parser instance instead
/// of allocating a new one per call.
///
/// Returns an empty `Vec` if the content cannot be parsed.
fn walk_ast_with_parser<F>(
    parser: &mut tree_sitter::Parser,
    content: &str,
    mut visit: F,
) -> Vec<ExtractedSymbol>
where
    F: FnMut(tree_sitter::Node<'_>, &[u8], &mut Vec<ExtractedSymbol>),
{
    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return vec![],
    };

    let bytes = content.as_bytes();
    let root = tree.root_node();
    let mut cursor = root.walk();
    let mut symbols = Vec::new();

    walk_nodes(root, &mut cursor, bytes, &mut symbols, &mut visit, 0);
    symbols
}

/// Walk a tree-sitter AST and collect extracted symbols.
///
/// Handles: parser creation, language setup, parsing, root cursor, and recursive
/// traversal. Language modules provide only the node-level visitor logic.
///
/// Returns an empty `Vec` if the parser cannot be configured or the content
/// cannot be parsed.
pub(crate) fn walk_ast<F>(
    content: &str,
    ts_language: tree_sitter::Language,
    visit: F,
) -> Vec<ExtractedSymbol>
where
    F: FnMut(tree_sitter::Node<'_>, &[u8], &mut Vec<ExtractedSymbol>),
{
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&ts_language).is_err() {
        return vec![];
    }
    walk_ast_with_parser(&mut parser, content, visit)
}

/// Maximum recursion depth for `walk_nodes`. Prevents stack overflow on
/// pathological or deeply-nested external repo files.
const MAX_WALK_DEPTH: usize = 256;

/// Recursive node visitor used by `walk_ast`.
fn walk_nodes<F>(
    node: tree_sitter::Node<'_>,
    cursor: &mut tree_sitter::TreeCursor<'_>,
    bytes: &[u8],
    symbols: &mut Vec<ExtractedSymbol>,
    visit: &mut F,
    depth: usize,
) where
    F: FnMut(tree_sitter::Node<'_>, &[u8], &mut Vec<ExtractedSymbol>),
{
    if depth > MAX_WALK_DEPTH {
        return;
    }

    visit(node, bytes, symbols);

    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            walk_nodes(child, cursor, bytes, symbols, visit, depth + 1);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
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
