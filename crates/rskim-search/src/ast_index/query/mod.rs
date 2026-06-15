//! AST Structural Pattern Query Engine — Wave 3f (#197), Wave 4 perf (#286).
//!
//! Answers named-pattern and containment queries with OR-union additive BM25
//! ranking. Exposes a Wave-4 intersection hook (`search_ast`) and a
//! Wave-3g [`SearchLayer`] adapter.
//!
//! # Wave 4 performance changes (#286)
//!
//! - **P1 (partial decode)**: `score_postings` calls `file_lang_and_node_count`
//!   instead of `file_meta`, decoding only `lang_id` + `node_count` (5 bytes)
//!   rather than the full 15-byte record.
//! - **P2 (scalar IDF cache)**: The `last_lang`/`last_idf` scalar cache
//!   introduced post-#284 already collapses O(postings) IDF lookups to
//!   O(distinct-langs-in-run).  The mixed-language bench confirms no thrash.
//!   Closed-by-#284-refactor; no `LANG_COUNT` constant introduced (ADR-003).
//! - **P3 (capacity sizing)**: `run_ngram_set` starts at `CAPACITY_FLOOR` and
//!   calls `scores.reserve(n)` before processing each posting list of length
//!   `n`, growing the map at most once per n-gram instead of pre-allocating
//!   `file_count()`.  Solves both the over-allocation (broad queries) and the
//!   empty-first-list under-sizing (AC7) cases.
//! - **P4 (lang filter fold-in)**: `run_ngram_set` accepts an optional
//!   `lang_filter`; when set, each posting is skipped before insertion if its
//!   `lang_id` does not match, eliminating the second per-file `file_meta`
//!   decode loop that previously ran in `SearchLayer::search`.
//!
//! # Module structure (A1/CX2, #287)
//!
//! - [`parse`] — `AstQuery` enum, `parse_ast_query()`, and parsing helpers.
//! - [`engine`] — `AstQueryEngine` and `SearchLayer` adapter.
//! - [`scoring`] — `ScoringCtx`, BM25 helpers, IDF memoization, `LiteMeta`.
//! - [`adapter`] — `AstPostingSource` trait and its `AstIndexReader` impl.

mod adapter;
mod engine;
mod parse;
mod scoring;

// Re-export the public API surface that `ast_index/mod.rs` and `lib.rs` expect.
// AC-3: this set must be byte-identical to the pre-split `pub use query::{...}`
// block in ast_index/mod.rs (lines 64-66).
pub use adapter::AstPostingSource;
pub use engine::AstQueryEngine;
pub use parse::{AstQuery, parse_ast_query};
pub use scoring::{AST_BM25_B, AST_BM25_K1};

// Re-exports used by the test module via `use super::*`. These are already
// public in the crate; bringing them into this module's namespace lets the
// `#[path]`-included `query_tests.rs` reach them without additional imports
// (mirrors the original flat `query.rs` where they were in scope as `use super::{...}`).
// None of these leak new symbols into the external API (AC-5): they were already
// re-exported at `crate::ast_index::*`.
#[cfg(test)]
pub use crate::ast_index::AstIndexReader;
#[cfg(test)]
pub use crate::types::SearchLayer;

#[cfg(test)]
#[path = "../query_tests.rs"]
mod tests;
