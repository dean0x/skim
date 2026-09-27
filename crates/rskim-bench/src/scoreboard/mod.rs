//! Search scoreboard — the end-to-end retrieval-quality gate for
//! `skim search` (#203; the required search gate per SEARCH-ADR-007's
//! 2026-09-25 amendment).
//!
//! The scoreboard drives the release `skim` binary as a subprocess against
//! pinned real-world corpora and checks its output against oracles that are
//! independent of skim's own code. The structural oracle is its own crate,
//! `rskim-oracle`, which depends on no `rskim-*` crate (its
//! `tests/independence.rs` checks the manifest), so that independence holds
//! at compile time. Everything in this module is independent by convention
//! only, because `rskim-bench` depends on `rskim-search` and `rskim-core`:
//! nothing that scores skim imports `rskim_search::query_substring_present`,
//! `rskim_core::Language`, or an `rskim-search` tokenizer. Two modules read
//! skim's code on purpose, and neither scores it: [`catalog`] reads skim's
//! pattern catalog once and hands its projection to everything that needs
//! it, and [`golden_gen`] proposes golden entries for human review, passing
//! `rskim_core::Language` to the symbol extractor as a dispatch key only.
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
//! - [`golden_gen`] — `golden-gen`: candidate `[[ident]]` entries and, with
//!   `--ast`, candidate `[[ast]]` entries from the structural oracle and the
//!   gate's own `--ast` pattern calls (a reviewed proposal; uses
//!   `rskim_core::Language` only as the symbol extractor's dispatch key,
//!   never scores skim).
//! - [`catalog`] — skim's `--ast` pattern catalog (the one read of
//!   `rskim_search::all_patterns`, name / exact / example only) crossed with
//!   the structural oracle's coverage. The structural oracle itself (#541:
//!   tree-sitter queries encoding each catalog description, the nested-loop
//!   intent oracles, the oracle's own AST language table and size cap) is
//!   the `rskim-oracle` crate, `rskim_oracle::structural`.
//! - [`structural_metrics`] — scores skim's `--ast` answers against the
//!   structural oracle: rows split by language, the `structural.*` HARD
//!   checks, the per-entry measurements, `uncovered_patterns`.
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
//! - [`fmt`] — value formatting the scoring and report modules share
//!   (4-decimal rounding, path samples); a leaf, so they never import one
//!   another for it.
//! - [`pipeline`] — one run end to end; the `scoreboard` binary
//!   (`src/bin/scoreboard.rs`) is a thin CLI over it.
//!
//! Exit codes: `0` pass, `1` gate failure, `2` harness error (never reported
//! as a regression).

pub mod baseline;
pub mod catalog;
pub mod corpus;
pub mod fmt;
pub mod gate;
pub mod golden;
pub mod golden_gen;
pub mod metrics;
pub mod oracle;
pub mod pipeline;
pub mod report;
pub mod runner;
pub mod structural_metrics;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_support;
pub mod types;
pub mod universe;

use std::path::Path;

/// Hard bound on a pagination sweep: `--offset 0, L, 2L, …` stops when
/// `has_more` is false or after this many pages. Golden integrity bounds each
/// pagination entry's full count by `min(limits) × (MAX_PAGES − 1)` with the
/// same constant.
pub const MAX_PAGES: u32 = 64;

/// Read an optional data file (`known_failures.toml`, `baseline.json`):
/// `None` when it does not exist.
///
/// # Errors
///
/// Any read failure other than "not found".
pub(crate) fn read_optional(path: &Path) -> anyhow::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(raw) => Ok(Some(raw)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::anyhow!(e).context(format!("reading {}", path.display()))),
    }
}
