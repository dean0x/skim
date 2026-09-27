//! Independent ground-truth oracles for the search scoreboard
//! (`rskim-bench`'s `scoreboard` binary, #203).
//!
//! An oracle here answers "what should skim return?" from the source text and
//! the real tree-sitter grammars alone. The crate depends on no `rskim-*`
//! crate, so nothing in skim's search stack (linearization, n-grams, the
//! query engine, re-parse verification) can reach an answer at compile time;
//! `tests/independence.rs` keeps the manifest that way.
//!
//! - [`structural`] — the structural oracle for `skim search --ast <pattern>`
//!   (#541): one hand-written tree-sitter query per (pattern, language), the
//!   nested-loop intent oracles, and the oracle's own AST language table and
//!   size cap.
//!
//! skim's pattern catalog is not an input: the scoreboard reads it in
//! `rskim-bench` and crosses it with this crate's registry by pattern name.

pub mod structural;
