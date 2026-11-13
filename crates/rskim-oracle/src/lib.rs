//! Independent ground-truth oracles for the search scoreboard
//! (`rskim-bench`'s `scoreboard` binary, #203).
//!
//! An oracle here answers "what should skim return?" from the source text and
//! the real tree-sitter grammars alone. The crate depends on no `rskim-*`
//! crate, so nothing in skim's search stack (linearization, n-grams, the
//! query engine, re-parse verification) can reach an answer at compile time;
//! `tests/independence.rs` keeps the manifest that way.
//!
//! - [`roles`] — the role oracle for names (#540): where a name is defined,
//!   called and imported, from role queries written from the grammars.
//!
//! - [`structural`] — the oracle language table and parser scratch space that
//!   [`roles`] uses. It also holds the structural oracle of the removed
//!   `skim search --ast <pattern>` flag (#541): one hand-written tree-sitter
//!   query per (pattern, language) and the nested-loop intent oracles. Nothing
//!   scores those queries now (see the module's `Status` section).

pub mod roles;
pub mod structural;
