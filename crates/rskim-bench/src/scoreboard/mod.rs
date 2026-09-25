//! Search scoreboard — the end-to-end retrieval-quality gate for
//! `skim search` (#203; the required search gate per ADR-007's 2026-09-25
//! amendment).
//!
//! The scoreboard drives the release `skim` binary as a subprocess against
//! pinned real-world corpora and checks its output against oracles that are
//! independent of skim's own code: nothing that scores skim imports
//! `rskim_search::query_substring_present`, `rskim_core::Language`, or an
//! `rskim-search` tokenizer. The one `rskim_core::Language` user is
//! [`golden_gen`], which proposes golden entries for human review and passes
//! the enum to the symbol extractor as a dispatch key only.
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
//! - [`golden_gen`] — `golden-gen`: candidate `[[ident]]` entries (a
//!   reviewed proposal; uses `rskim_core::Language` only as the symbol
//!   extractor's dispatch key, never scores skim).
//! - [`types`] — what a skim invocation returns (rows, pages, stats) and the
//!   HARD-check vocabulary.
//! - [`runner`] — runs the skim CLI as a sandboxed, time-bounded subprocess:
//!   build, stats, full lists, pagination sweeps, prefix lists, text runs.
//! - [`metrics`] — which checks run on which entry ([`metrics::plan`]), the
//!   pure HARD checks, and the RATCHET measurements and aggregates.
//! - [`gate`] — the `known_failures.toml` ledger (XFAIL / XPASS) and the
//!   gate verdict against `baseline.json`.
//! - [`baseline`] — `baseline.json` and `bless`.
//! - [`report`] — `report.json` (deterministic apart from `latency`) and
//!   `report.md` (the step summary).
//! - [`pipeline`] — one run end to end; the `scoreboard` binary
//!   (`src/bin/scoreboard.rs`) is a thin CLI over it.
//!
//! Exit codes: `0` pass, `1` gate failure, `2` harness error (never reported
//! as a regression).

pub mod baseline;
pub mod corpus;
pub mod gate;
pub mod golden;
pub mod golden_gen;
pub mod metrics;
pub mod oracle;
pub mod pipeline;
pub mod report;
pub mod runner;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_support;
pub mod types;
pub mod universe;

/// Hard bound on a pagination sweep: `--offset 0, L, 2L, …` stops when
/// `has_more` is false or after this many pages. Golden integrity bounds each
/// pagination entry's full count by `min(limits) × (MAX_PAGES − 1)` with the
/// same constant.
pub const MAX_PAGES: u32 = 64;
