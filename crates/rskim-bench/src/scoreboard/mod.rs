//! Search scoreboard — the end-to-end retrieval-quality gate for
//! `skim search` (#203; the required search gate per SEARCH-ADR-007's
//! 2026-09-25 amendment).
//!
//! The scoreboard drives the release `skim` binary as a subprocess against
//! pinned real-world corpora and checks its output against oracles that are
//! independent of skim's own code. The role oracle is its own crate,
//! `rskim-oracle`, which depends on no `rskim-*` crate
//! (its `tests/independence.rs` checks the manifest), so that independence
//! holds at compile time. This crate depends on neither `rskim` nor `rskim-core`:
//! nothing that scores skim imports a skim tokenizer, classifier or file
//! walker. Every rule that must agree with skim (the word-character class,
//! the `-t` type table, the `-g` glob rules, the left-out reasons) is carried
//! as the oracle's own copy. [`golden_gen`] proposes golden entries for human
//! review from the role oracle, and never scores skim.
//!
//! # Modules
//!
//! - [`corpus`] — `corpora.toml` and the [`corpus::CorpusSource`] seam
//!   (production: `rskim_research::clone::ensure_pinned_history_clone`).
//! - [`universe`] — the oracle's text universe (every tracked text file plus
//!   the untracked files the ignore files allow) and its left-out
//!   breakdown, mirroring the engine's file walk.
//! - [`oracle`] — ground-truth predicates (word set, file type, glob,
//!   whole word, line) and the in-process baselines (alphabetical,
//!   occurrence-count, simulated `rg -n -F`).
//! - [`golden`] — golden-set schema, query flags, loading, integrity.
//! - [`golden_gen`] — `golden-gen`: candidate `[[ident]]` entries from the
//!   role oracle (a reviewed proposal; never scores skim).
//! - [`types`] — what a skim invocation returns (rows, pages, occurrences)
//!   and the HARD-check vocabulary.
//! - [`runner`] — runs the skim CLI as a sandboxed, time-bounded subprocess:
//!   full lists, pagination sweeps, prefix lists, text runs and the
//!   `[[stakes]]` calls.
//! - [`metrics`] — which checks run on which entry ([`metrics::plan`]), the
//!   pure HARD checks, and the RATCHET measurements and aggregates.
//! - [`gate`] — the `known_failures.toml` ledger (XFAIL / XPASS) and the
//!   gate verdict against `baseline.json`.
//! - [`baseline`] — `baseline.json` and `bless`.
//! - [`report`] — `report.json` (deterministic apart from `latency`) and
//!   `report.md` (the step summary).
//! - [`fmt`] — value formatting shared across the scoreboard (4-decimal
//!   rounding, path samples, the `entry` / `entries` noun); a leaf, so its
//!   users never import one another for it.
//! - [`pipeline`] — one run end to end; the `scoreboard` binary
//!   (`src/bin/scoreboard.rs`) is a thin CLI over it.
//!
//! Exit codes: `0` pass, `1` gate failure, `2` harness error (never reported
//! as a regression).

pub mod baseline;
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
