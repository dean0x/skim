//! AST structural indexing: CST linearization for n-gram extraction.
//!
//! This module converts tree-sitter CSTs into compact depth-encoded node-type
//! sequences. Each node in the pre-order traversal is represented as a
//! `LinearNode { kind_id, depth }` pair, enabling downstream n-gram extraction
//! over structural AST patterns without retaining the full tree.
//!
//! # Usage
//!
//! ```rust,ignore
//! use rskim_search::ast_index::{LinearNode, LinearizeResult, linearize_source};
//! use rskim_core::Language;
//!
//! let result = linearize_source("fn main() {}", Language::Rust).unwrap();
//! println!("{} nodes", result.node_count);
//! ```

mod linearize;
pub use linearize::{LinearNode, LinearizeResult, linearize_source};
