//! Search scoreboard — the end-to-end retrieval-quality gate for
//! `skim search` (#203; the required search gate per ADR-007's 2026-09-25
//! amendment).
//!
//! The scoreboard drives the release `skim` binary as a subprocess against
//! pinned real-world corpora and checks its output against oracles that are
//! independent of skim's own code: nothing under this module imports
//! `rskim_search::query_substring_present`, `rskim_core::Language`, or an
//! `rskim-search` tokenizer.
//!
//! # Modules
//!
//! - [`corpus`] — `corpora.toml` and the [`corpus::CorpusSource`] seam
//!   (production: `rskim_research::clone::ensure_pinned_history_clone`).
//! - [`universe`] — the oracle's file universe, skip breakdown, and coverage
//!   universe, mirroring the CLI walker.
//! - [`oracle`] — ground-truth predicates (and / phrase / near / pnear /
//!   lang) and the in-process baselines (alphabetical, occurrence-count,
//!   simulated `rg -n -F`).
//! - [`golden`] — golden-set schema, query flags, loading, integrity.
//! - [`types`] — what a skim invocation returns (rows, pages, stats) and the
//!   HARD-check vocabulary.
//!
//! The runner, metrics, gate, baseline, report and `scoreboard` binary build
//! on these in later phases.

pub mod corpus;
pub mod golden;
pub mod oracle;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_support;
pub mod types;
pub mod universe;

/// Hard bound on a pagination sweep: `--offset 0, L, 2L, …` stops when
/// `has_more` is false or after this many pages. Golden integrity bounds each
/// pagination entry's full count by `min(limits) × (MAX_PAGES − 1)` with the
/// same constant.
pub const MAX_PAGES: u32 = 64;
