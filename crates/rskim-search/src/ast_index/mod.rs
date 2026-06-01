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

mod linearize;
mod ngram;

pub use linearize::{LinearNode, LinearizeResult, linearize_source};
pub use ngram::{
    AstBigram, AstTrigram, DEFAULT_AST_WEIGHT, NodeKindId, ast_bigram_idf, ast_trigram_idf,
    vocab_len, vocab_lookup, vocab_resolve,
};
