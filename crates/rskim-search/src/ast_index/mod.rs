//! AST structural indexing: CST linearization and n-gram encoding for
//! structural code search.
//!
//! This module converts tree-sitter CSTs into compact depth-encoded node-type
//! sequences. Each node in the pre-order traversal is represented as a
//! `LinearNode { kind_id, depth }` pair, enabling downstream n-gram extraction
//! over structural AST patterns without retaining the full tree.
//!
//! The `ngram` sub-module provides [`AstBigram`] and [`AstTrigram`] newtypes
//! for packing node-kind ID pairs into compact integer keys, along with
//! vocabulary helpers and IDF weight lookup functions.
//!
//! # Usage
//!
//! ```rust,ignore
//! use rskim_search::ast_index::{
//!     AstBigram, AstTrigram, LinearNode, LinearizeResult, NodeKindId,
//!     ast_bigram_idf, ast_trigram_idf, linearize_source,
//!     vocab_len, vocab_lookup, vocab_resolve, DEFAULT_AST_WEIGHT,
//! };
//! use rskim_core::Language;
//!
//! let result = linearize_source("fn main() {}", Language::Rust).unwrap();
//! println!("{} nodes", result.node_count);
//!
//! let parent = vocab_lookup("function_item").unwrap();
//! let child = vocab_lookup("block").unwrap();
//! let bigram = AstBigram::encode(parent, child);
//! let weight = ast_bigram_idf(Language::Rust, bigram);
//! ```

mod extract;
mod linearize;
mod ngram;
pub mod patterns;
mod store;
pub(crate) mod structural;

// ============================================================================
// Shared type alias
// ============================================================================

/// Compact numeric ID for a tree-sitter node kind string.
///
/// Indexes into [`crate::ast_weights::NODE_KIND_VOCABULARY`]. `0` is the
/// sentinel for unknown kinds (maps to `""`).
///
/// Defined here (the shared parent module) so that both `linearize` and
/// `ngram` reference the same canonical definition rather than each working
/// with a raw `u16`.
pub type NodeKindId = u16;

pub use extract::{
    AstBigramEntry, AstNgramSet, AstTrigramEntry, extract_ast_ngrams,
    extract_ast_ngrams_with_metrics, extract_ast_ngrams_with_weights,
};
pub use linearize::{LinearNode, LinearizeResult, linearize_source};
pub use ngram::{
    AstBigram, AstTrigram, DEFAULT_AST_WEIGHT, ast_bigram_idf, ast_trigram_idf, vocab_len,
    vocab_lookup, vocab_resolve,
};
pub use patterns::{Pattern, PatternCategory, all_patterns, lookup_pattern, pattern_to_query_set};
pub use store::{AstFileMetaEntry, AstIndexBuilder, AstIndexReader, AstPosting};
pub use structural::StructuralMetrics;
