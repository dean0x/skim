//! Benchmark harness for `skim search`: the search scoreboard.
//!
//! # Modules
//!
//! - [`metrics`]  — Mean Reciprocal Rank
//! - [`scoreboard`] — search scoreboard: end-to-end retrieval-quality gate (#203)

pub mod metrics;
pub mod scoreboard;
