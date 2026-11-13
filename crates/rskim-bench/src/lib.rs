//! Benchmark harness for `skim search`: the search scoreboard.
//!
//! # Modules
//!
//! - [`extract`]  — language-specific AST symbol extractors (`golden-gen`)
//! - [`metrics`]  — Mean Reciprocal Rank
//! - [`scoreboard`] — search scoreboard: end-to-end retrieval-quality gate (#203)

pub mod extract;
pub mod metrics;
pub mod scoreboard;
