//! Dependency-free degraded-state vocabulary for `skim search` temporal flags.
//!
//! # Purpose
//!
//! This module is a leaf in the dependency graph: it imports nothing from
//! other `cmd/search/` submodules (only from the workspace library crates and
//! the parent module's constants).  By extracting the degraded-state types here,
//! the former cycle
//!
//! ```text
//! temporal_build → temporal → staleness → temporal_state → temporal_build
//! ```
//!
//! is broken: `temporal_build.rs` now imports from this leaf, while `temporal.rs`
//! re-exports from here so its existing callers (mod.rs, ast.rs, etc.) are unaffected.
//!
//! # SSOT guarantee (AD-414-1)
//!
//! Every degraded-state notice text is produced by [`degraded_notice`] via
//! [`DegradedReason::full_message`].  No other production file under
//! `cmd/search/` may contain cause substrings — enforced by
//! `t19b_no_cause_substring_outside_the_builder` in `temporal_tests.rs`.

use serde::Serialize;

// ============================================================================
// DegradedReason
// ============================================================================

/// AD-414-25: the remedy for an empty temporal DB on a **shallow** clone.
///
/// `skim search --rebuild` cannot help here — the commits simply are not in the
/// clone, so a rebuild re-derives the same zero rows.  Unshallowing has to come
/// first.  Shared by [`DegradedReason::full_message`] (human-readable notice)
/// and [`DegradedReason::remediation_for`] (`DegradedJson.remediation`) so the
/// two can never disagree.
const SHALLOW_EMPTY_REMEDIATION: &str =
    "run 'git fetch --unshallow' (or use a full clone), then 'skim search --rebuild'";

/// AD-414-15: the ordering below is the **§2.3 spec precedence** — the
/// conceptual priority a machine consumer assigns to each state when reporting.
/// The probe order inside [`super::temporal::open_temporal_state`] differs:
/// `RepositoryMismatch` is checked after `TemporalDb::open` succeeds (it must
/// read `meta.git_toplevel`), even though it ranks before `missing`/`empty` in
/// this table.  See that function's doc for the implementation order and the
/// reason it differs.
///
/// Each variant represents the **state** a user or agent can recognise, never
/// the mechanism that detected it.  The reason string is part of the `degraded`
/// JSON contract (OD-A) and is fixed before implementation to avoid breaking
/// adopters on rename.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DegradedReason {
    /// Directory is not tracked by git (no ancestor `.git`).
    NotGitRepo,
    /// git dir found but HEAD could not be resolved to a commit SHA
    /// (unborn branch, unsupported ref backend — #481).
    HeadUnresolved,
    /// DB was built for a different repository root (AD-413-16).
    RepositoryMismatch,
    /// git repo present and HEAD resolves, but `temporal.db` does not exist.
    Missing,
    /// `temporal.db` exists but is structurally corrupt (SQLITE_NOTADB/CORRUPT).
    Corrupt,
    /// `temporal.db` was written by a newer schema version than supported.
    UnsupportedVersion,
    /// `temporal.db` exists but could not be opened for any other reason.
    Unreadable,
    /// DB open and readable but zero rows match the requested dimension.
    Empty,
    /// DB readable and rows exist, but none of the matched results carries a
    /// temporal score for the requested dimension (all new/uncommitted files).
    NoRankedRows,
    /// Rows were computed but the build-time ghost filter excluded all of them
    /// (files not present on disk at the indexed root).  `detail` holds the
    /// pre-filter count as a decimal string (e.g. `"42"`).
    GhostFilter,
}

impl DegradedReason {
    /// Machine-readable reason code for JSON output (OD-A).
    pub(super) fn as_json_str(self) -> &'static str {
        match self {
            Self::NotGitRepo => "not_git_repo",
            Self::HeadUnresolved => "head_unresolved",
            Self::RepositoryMismatch => "repository_mismatch",
            Self::Missing => "missing",
            Self::Corrupt => "corrupt",
            Self::UnsupportedVersion => "unsupported_version",
            Self::Unreadable => "unreadable",
            Self::Empty => "empty",
            Self::NoRankedRows => "no_ranked_rows",
            Self::GhostFilter => "ghost_filter",
        }
    }

    /// The §2.3 normative cause text in isolation; [`Self::full_message`] delegates
    /// to this method to build the production notice.
    ///
    /// `detail` carries reason-specific context set by [`super::temporal::open_temporal_state`]
    /// or the zero-coverage path (e.g. path pair for `RepositoryMismatch`,
    /// formatted count for `NoRankedRows`/`GhostFilter`, error text for `Unreadable`).
    ///
    /// For `Empty`, `detail == "shallow"` signals that `meta.is_shallow` is set;
    /// absent row → `detail` is empty → no shallow suffix.
    ///
    /// For `GhostFilter`, `detail` is the pre-filter row count as a decimal string.
    ///
    /// This is the §2.3 normative cause table in isolation.  [`Self::full_message`]
    /// delegates to this method (cause + remediation), so `cause` is an active
    /// production call site (via [`degraded_notice`]).  AC-19(a)/T-19(a) pins
    /// every variant-by-variant.
    pub(super) fn cause(self, detail: &str) -> String {
        match self {
            // Legacy constants used verbatim (AC-19 byte-identical guards).
            Self::NotGitRepo => super::NO_TEMPORAL_DATA_MSG.to_string(),
            Self::HeadUnresolved => super::HEAD_UNRESOLVED_TEMPORAL_MSG.to_string(),
            Self::RepositoryMismatch => {
                format!("{} {}", super::SUBDIR_ROOT_TEMPORAL_MSG, detail)
            }
            // §2.3 normative table (AD-414-15 / AC-2).
            Self::Missing => "temporal.db is not present in the index cache".to_string(),
            Self::Corrupt => "temporal.db is corrupt (not a database)".to_string(),
            Self::UnsupportedVersion => {
                format!("temporal.db was written by a newer skim ({detail})")
            }
            Self::Unreadable => {
                if detail.is_empty() {
                    "temporal.db could not be opened".to_string()
                } else {
                    format!("temporal.db could not be opened ({detail})")
                }
            }
            Self::Empty => {
                let base = "temporal data is empty (0 rows) \u{2014} this repository has no \
                            commit history skim can analyse";
                if detail.contains("shallow") {
                    format!("{base}; a shallow clone is the usual cause")
                } else {
                    base.to_string()
                }
            }
            Self::NoRankedRows => detail.to_string(),
            Self::GhostFilter => format!(
                "temporal data built 0 rows — all {detail} computed rows were excluded \
                 by the on-disk ghost filter (files not present on disk at the indexed root)"
            ),
        }
    }

    /// Build the `NoRankedRows` detail string from coverage counters.
    ///
    /// Low-level SSOT for the "0 of N results have temporal data" text; called by
    /// [`super::temporal::DegradedReason::no_ranked_rows`], the higher-level builder
    /// that constructs the full [`TemporalUnavailable`] (Finding [medium/complexity]).
    /// `cause()` returns `detail` verbatim for `NoRankedRows`.
    /// Enforced by the `"0 of "` and `"results have temporal data"` entries in
    /// `cause_substrings_for_guard` (t19b guard).
    pub(super) fn no_ranked_rows_detail(total: usize, lookup_errors: usize) -> String {
        if lookup_errors > 0 {
            format!(
                "0 of {total} results have temporal data \
                 ({lookup_errors} temporal lookups failed)"
            )
        } else {
            format!("0 of {total} results have temporal data")
        }
    }

    /// SSOT for the `"schema version {found}, this build supports {supported}"` detail
    /// string (§2.3 normative table, RD-4).
    ///
    /// Both open paths (`temporal::open_temporal_state` and
    /// `temporal_build::open_or_discard_temporal_db`) call this instead of formatting
    /// the literal independently, so the two sites cannot drift from each other.
    pub(super) fn unsupported_version_detail(found: i64, supported: i64) -> String {
        format!("schema version {found}, this build supports {supported}")
    }

    /// Detail-independent remediation advice.
    ///
    /// Every emit site must call [`Self::remediation_for`] instead: it is the
    /// detail-aware wrapper that selects [`SHALLOW_EMPTY_REMEDIATION`] for a
    /// shallow-clone `Empty`.  This method carries the base table so the two
    /// cannot drift.
    fn remediation(self) -> &'static str {
        match self {
            // Embedded in NO_TEMPORAL_DATA_MSG; repeated separately for JSON consumers.
            Self::NotGitRepo => "run 'skim search' on a git repo to auto-populate",
            Self::HeadUnresolved => "commit at least one file to initialise the branch HEAD",
            Self::RepositoryMismatch => {
                "run 'skim search --rebuild --root <this root>' to re-anchor it"
            }
            Self::Missing => "run 'skim search --update' to build it",
            Self::Corrupt => "run 'skim search --rebuild' to discard and rebuild it",
            Self::UnsupportedVersion => "upgrade skim; skim will not overwrite a newer database",
            Self::Unreadable => "run 'skim search --rebuild'",
            Self::Empty => "run 'skim search --rebuild'",
            Self::NoRankedRows => {
                "commit the matched files, or run 'skim search --update' after committing"
            }
            Self::GhostFilter => "run 'skim search --rebuild' to rebuild with the current file set",
        }
    }

    /// AD-414-25 (F-C2-01): detail-aware remediation — the value that goes into
    /// `DegradedJson.remediation`.
    ///
    /// The `Empty` cause has two distinct remedies and `--rebuild` is the wrong
    /// one for a shallow clone: rebuilding re-derives the same zero rows because
    /// the history is not present locally.  When `meta.is_shallow` is set — the
    /// `detail == "shallow"` signal produced by
    /// [`super::temporal::empty_temporal_state`] at query time and by
    /// `temporal_build::zero_row_notice` at build time — the remediation names
    /// `git fetch --unshallow` first, matching the tail
    /// [`Self::full_message`] already appends to the human-readable notice.
    ///
    /// Both fields are therefore built from [`SHALLOW_EMPTY_REMEDIATION`], so a
    /// `DegradedJson` can never advise something its own `message` contradicts
    /// (AD-414-1 SSOT).
    pub(super) fn remediation_for(self, detail: &str) -> &'static str {
        match self {
            Self::Empty if detail.contains("shallow") => SHALLOW_EMPTY_REMEDIATION,
            _ => self.remediation(),
        }
    }

    /// Complete human-readable message (cause + embedded remediation).
    ///
    /// Used by [`degraded_notice`].  Delegates to [`Self::cause`] + `"; "` +
    /// [`Self::remediation`] for all variants, with two exceptions:
    ///
    /// - `NotGitRepo` and `HeadUnresolved`: the legacy constants are returned
    ///   verbatim (AC-19 byte-identical assertions); appending remediation would
    ///   break adopters that match on the constant text.
    /// - `Empty` with `detail == "shallow"`: the remediation is the unshallow-
    ///   specific advice rather than the generic "skim search --rebuild" that
    ///   [`Self::remediation`] returns; see §2.3 conditional (AC-2).
    fn full_message(self, detail: &str) -> String {
        let cause = self.cause(detail);
        match self {
            // AC-19: legacy constants returned verbatim — do NOT append remediation.
            Self::NotGitRepo | Self::HeadUnresolved => cause,
            // §2.3 shallow-Empty: remediation differs from Self::remediation().
            // AD-414-25: both this tail and `DegradedJson.remediation` read the
            // same constant, so the JSON element cannot advise `--rebuild` while
            // its own message advises `git fetch --unshallow`.
            Self::Empty if detail.contains("shallow") => {
                format!("{cause}; {SHALLOW_EMPTY_REMEDIATION}")
            }
            // All other variants: cause + "; " + remediation().
            _ => format!("{cause}; {}", self.remediation()),
        }
    }
}

// ============================================================================
// TemporalUnavailable
// ============================================================================

/// Why the temporal database is not available for a query.
///
/// Returned by [`super::temporal::open_temporal_state`] (the new single funnel).
/// Carries `reason` (the typed classification, AD-414-15) and `detail`
/// (reason-specific context — path pair for `RepositoryMismatch`, error text for
/// `Corrupt`, etc.).
#[derive(Debug)]
pub(super) struct TemporalUnavailable {
    pub(super) reason: DegradedReason,
    /// Reason-specific context string; may be empty for stateless variants.
    pub(super) detail: String,
}

// ============================================================================
// DegradedJson
// ============================================================================

/// Machine-readable representation of a single degradation signal (OD-A).
///
/// Serialised as an element of `QueryOutput.degraded` (a `Vec<DegradedJson>`
/// with `skip_serializing_if = "Vec::is_empty"`) so the JSON key is absent on
/// healthy queries (AD-414-12).
#[derive(Debug, Clone, Serialize)]
pub(super) struct DegradedJson {
    /// Subsystem that emits this signal (always `"temporal"` in this ticket).
    pub subsystem: &'static str,
    /// Machine-readable reason code (`DegradedReason::as_json_str`).
    pub reason: &'static str,
    /// The user flag that requested temporal ranking, in bare form (e.g. `"hot"`,
    /// `"blast-radius"`).  No `--` prefix — use [`super::types::TemporalSort::json_name`]
    /// to obtain the correct value; `flag_name()` is for human-readable message text.
    pub requested: &'static str,
    /// The ranking actually served (e.g. `"lexical"`, `"ast"`).
    pub applied: &'static str,
    /// Human-readable notice (identical to what was printed to stderr).
    pub message: String,
    /// Machine-readable remediation advice.
    pub remediation: &'static str,
}

// ============================================================================
// Fallback
// ============================================================================

/// How the search degrades when temporal data is unavailable (AD-414-1).
///
/// Passed to [`degraded_notice`] to communicate which ranking was served
/// instead and to tailor remediation advice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fallback {
    /// Pure-lexical BM25F order is served.
    Lexical,
    /// Raw AST structural ranking is served without temporal enrichment.
    Ast,
    /// No fallback is possible (standalone temporal arm — no results served).
    NoResults,
}

// ============================================================================
// degraded_notice (SSOT — AD-414-1)
// ============================================================================

/// AD-414-1: single source of truth for every degraded-state notice,
/// generalising the documented `--ast` contract (warn on stderr, keep the
/// upstream order, exit 0).  Loud when skim cannot self-fix, cause-specific,
/// and always carries a remediation.  This is the ONLY builder of degraded-state
/// notice text: every emit site formats the string it returns rather than
/// composing its own (enforced by `t19b_no_cause_substring_outside_the_builder`).
///
/// When `flag` is non-empty, appends a fallback-specific tail explaining which
/// flag was not applied and what order was served instead.  When `flag` is
/// empty (standalone temporal arm, `--hot`/`--cold`/`--risky` without a text
/// query), returns the base message verbatim so legacy byte-identical
/// assertions remain valid.
pub(super) fn degraded_notice(u: &TemporalUnavailable, flag: &str, fallback: Fallback) -> String {
    let base = u.reason.full_message(&u.detail);
    if flag.is_empty() {
        base
    } else {
        let tail = match fallback {
            Fallback::Lexical => {
                format!("; {flag} not applied — results are in lexical relevance order")
            }
            Fallback::Ast => {
                format!("; {flag} not applied — results are in raw AST match order")
            }
            Fallback::NoResults => format!("; no {flag} data to rank"),
        };
        format!("{base}{tail}")
    }
}

// ============================================================================
// blast_radius_degraded_msg
// ============================================================================

/// AC-7 / AC-19(b): compute the human-readable message for a `--blast-radius`
/// degradation notice.
///
/// `NotGitRepo` keeps the legacy composition format byte-identical to the
/// pre-refactor message (AC-19 byte-identical guard).  All other reasons use
/// [`degraded_notice`] with `flag = "--blast-radius"` and [`Fallback::Lexical`].
///
/// Shared by two call sites that must emit the same string:
/// - [`super::temporal::resolve_blast_radius_paths`] — prints to stderr.
/// - `mod.rs::run_query` — writes to `DegradedJson.message` (must match stderr).
pub(super) fn blast_radius_degraded_msg(u: &TemporalUnavailable) -> String {
    if u.reason == DegradedReason::NotGitRepo {
        format!(
            "no temporal data for --blast-radius — {}",
            super::NO_TEMPORAL_DATA_MSG
        )
    } else {
        degraded_notice(u, "--blast-radius", Fallback::Lexical)
    }
}

// ============================================================================
// Test support
// ============================================================================

/// The distinctive cause-text prefixes for the t19b SSOT guard.
///
/// Co-located with the builder so the list stays in sync automatically: when a
/// new [`DegradedReason`] variant is added, the developer adds its prefix here
/// and the guard test picks it up without a separate edit.
///
/// `NotGitRepo` and `HeadUnresolved` are intentionally absent: their causes ARE
/// the shared `NO_TEMPORAL_DATA_MSG` / `HEAD_UNRESOLVED_TEMPORAL_MSG` constants
/// declared in `mod.rs`, which legitimately appear at several emit sites
/// (AC-19/AC-20).
#[cfg(test)]
pub(super) fn cause_substrings_for_guard() -> &'static [&'static str] {
    &[
        "temporal.db is not present in the index cache",
        "temporal.db is corrupt (not a database)",
        "temporal.db was written by a newer skim",
        "temporal.db could not be opened",
        "temporal data is empty (0 rows)",
        "temporal data built 0 rows",
        // NoRankedRows: "0 of N results have temporal data …"
        // Two overlapping substrings guard the full phrase so renaming one part
        // fails the t19b test even if the other is coincidentally preserved.
        "0 of ",
        "results have temporal data",
    ]
}
