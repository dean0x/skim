//! Temporal query helpers for `skim search` temporal flags.
//!
//! # Responsibilities
//!
//! - Path normalization for `--blast-radius` (cross-platform, repo-relative).
//! - `TemporalDb` open/check helpers.
//! - Standalone temporal dispatch (`--hot`, `--cold`, `--risky`, `--blast-radius`).
//! - Combined text+temporal enrichment (`apply_temporal_enrichment`).
//! - Output formatting for standalone temporal queries.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;

use serde::Serialize;

use rskim_search::{FileId, HotspotRow, RiskRow, SearchError, TemporalDb};

// Re-export the degraded-state vocabulary from the dependency-free leaf module.
// This keeps `temporal::DegradedReason` etc. accessible to mod.rs / ast.rs while
// breaking the temporal_build → temporal → staleness → temporal_state → temporal_build
// cycle (temporal_build now imports directly from super::degraded).
pub(super) use super::degraded::{
    DegradedJson, DegradedReason, Fallback, TemporalUnavailable, blast_radius_degraded_msg,
    degraded_notice,
};

#[cfg(test)]
pub(super) use super::degraded::cause_substrings_for_guard;

use super::staleness::{AnchorState, HeadState};
use super::types::{BlastRadiusStrengths, Page, ResolvedResult, TemporalAnnotation, TemporalSort};

// ============================================================================
// Blast-radius constants
// ============================================================================

/// Sentinel score for the blast-radius target file in the temporal layer.
///
/// AD-409-3: `SEED_STRENGTH = 2.0` is a **finite** sentinel strictly greater than the
/// Jaccard maximum of 1.0. It is the resolved decision Option A from ticket #409:
/// the blast-radius target (seed) always ranks **first** in the temporal layer.
/// `merge_layer_scores` sorts each layer by score DESC (AD-409-4), placing the
/// seed at rank 1 because `2.0 > max_jaccard(1.0)`.
///
/// Options B (sentinel == 1.0, indistinguishable from a perfect Jaccard match) and
/// C (seed excluded from the temporal layer entirely) were rejected.
pub(super) const SEED_STRENGTH: f64 = 2.0;

/// Stderr notice emitted when the blast-radius target file is absent from the
/// lexical manifest.
///
/// Exported as a `pub(super)` constant so intra-crate tests assert against a
/// single source of truth (consistent with `WEIGHTS_FULLY_INERT_NOTICE` and
/// `WEIGHTS_TEMPORAL_INERT_NOTICE` in `query.rs`).
pub(super) const BLAST_RADIUS_SEED_UNINDEXED_NOTICE: &str = "skim search: note: blast-radius target file not found in the indexed manifest \
     (excluded from scoring)";

/// Static suffix of the partial-drop notice emitted when co-change partners
/// are absent from the manifest.
///
/// Intra-crate tests can reference this const to build the expected substring
/// without duplicating the wording.  The full line is:
/// `"skim search: note: {dropped} of {partner_count} {BLAST_RADIUS_PARTNER_NOT_FOUND}"`.
pub(super) const BLAST_RADIUS_PARTNER_NOT_FOUND: &str =
    "co-change partners not found in the indexed manifest (excluded from scoring)";

/// Fallback score assigned to Jaccard values that are non-finite or outside the
/// mathematical Jaccard range of `[0.0, 1.0]`.
///
/// Any value outside `[0.0, 1.0]` arriving from `temporal.db` is corrupt data —
/// Jaccard similarity is mathematically confined to that interval.  Clamping such
/// values here, at the trust boundary in [`cochange_partner_strengths`], ensures
/// that no corrupt row can impersonate the seed sentinel (`SEED_STRENGTH = 2.0`)
/// or outrank it (`1e308 > 2.0`).  The floor is strictly below
/// `rskim_search::MIN_COCHANGE_JACCARD` (0.10), so out-of-range-clamped files
/// rank after every valid co-change partner in the temporal layer.
const NON_FINITE_JACCARD_FLOOR: f64 = 0.0;

// ============================================================================
// Path normalization
// ============================================================================

/// Normalize a user-provided file path to repo-root-relative form.
///
/// Algorithm:
/// 1. If absolute, use as-is. If relative, try joining to `project_root`
///    first; fall back to CWD when the root-relative path doesn't exist.
/// 2. Canonicalize (resolve symlinks, normalize `../`).
/// 3. Strip `project_root` prefix → repo-relative.
/// 4. Replace `\\` with `/` for Windows cross-platform consistency.
///
/// The root-first resolution makes `--blast-radius src/foo.rs` work correctly
/// when the user's CWD is the repo root or any subdirectory thereof.
///
/// # Errors
///
/// Returns an error when the path is outside the repository root or cannot
/// be canonicalized.
pub(super) fn normalize_blast_radius_path(
    raw: &str,
    project_root: &Path,
) -> anyhow::Result<String> {
    let p = std::path::Path::new(raw);

    // Resolve to an absolute path, trying existence in order:
    // 1. project-root-relative (most common for `--blast-radius src/foo.rs`)
    // 2. CWD-relative (user is in a subdirectory of the repo)
    // 3. Neither exists → bail with a clear "not found" error.
    //
    // The existence check happens before canonicalization so that missing files
    // produce "blast-radius file not found: <path>" instead of the confusing
    // "outside the project root" message that canonicalize() fallback would yield.
    let abs = if p.is_absolute() {
        // Absolute paths: check existence directly before proceeding.
        if !p.exists() {
            anyhow::bail!("blast-radius file not found: {}", raw);
        }
        p.to_path_buf()
    } else {
        // Prefer project-root-relative resolution so that `src/foo.rs` works
        // regardless of the user's CWD within the repo.
        let root_relative = project_root.join(p);
        if root_relative.exists() {
            root_relative
        } else {
            // Fallback: CWD-relative (e.g. user is in a subdirectory).
            // If current_dir() fails (deleted temp dir in tests, unusual in
            // production), treat it as "not found" rather than propagating a
            // confusing OS error.
            let cwd_relative = std::env::current_dir()
                .ok()
                .map(|cwd| cwd.join(p))
                .filter(|candidate| candidate.exists());

            match cwd_relative {
                Some(path) => path,
                None => anyhow::bail!("blast-radius file not found: {}", raw),
            }
        }
    };

    // Canonicalize — resolves `..` and symlinks.
    // Fallback to the raw path if canonicalize fails (e.g. race: file deleted
    // between the existence check above and this call).
    let canonical = abs.canonicalize().unwrap_or_else(|e| {
        if crate::debug::is_debug_enabled() {
            eprintln!(
                "skim search: canonicalize failed for {:?}: {e} — using raw path",
                abs
            );
        }
        abs.clone()
    });

    // Canonicalize the project root too for fair comparison.
    let canonical_root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());

    // Strip the root prefix.
    let rel = canonical
        .strip_prefix(&canonical_root)
        .map_err(|_| {
            anyhow::anyhow!(
                "path {:?} is outside the project root {:?}",
                raw,
                canonical_root
            )
        })?
        .to_string_lossy()
        .replace('\\', "/");

    // Strip leading `./` if present (edge case on some platforms).
    //
    // NOTE (#373 scope): this extra `strip_prefix("./")` step is intentional and
    // is NOT consolidated into `walk::normalize_rel_path`.  That helper only
    // covers the manifest-key/assignment/cache-lookup triple (the walk sort key
    // plus the `path_key` bindings in `index.rs` `consume`/`read_and_classify`);
    // it does not carry the `./` strip.  Combining the two would change
    // `--blast-radius ./foo/bar.rs` lookup behavior and widen the regression
    // blast-radius beyond #373's narrow scope.
    let normalized = rel.strip_prefix("./").unwrap_or(&rel).to_string();

    Ok(normalized)
}

/// Result of attempting to open the temporal database for a query.
///
/// Returned by [`open_temporal_state`].  A dedicated enum rather than a
/// `Result`: "cannot serve temporal ranking" is an ordinary, exit-0 outcome that
/// every arm must handle, not an error to propagate with `?`.
#[derive(Debug)]
pub(super) enum TemporalOpen {
    /// DB is open and anchored to the same repository — ready to serve.
    Open(TemporalDb),
    /// DB cannot be served; `reason` and `detail` describe why.
    Unavailable(TemporalUnavailable),
}

impl DegradedJson {
    /// Construct a `DegradedJson` whose `message` is produced by [`degraded_notice`].
    ///
    /// This is the single constructor for all standard degraded-state JSON elements,
    /// ensuring `DegradedJson.message` is always identical to what is printed to
    /// stderr via the same `degraded_notice` call (AD-414-1 SSOT enforcement,
    /// Finding [medium/architecture]).
    ///
    /// * `requested` — bare flag name (e.g. `"hot"`, `"blast-radius"`) per AC-4 /
    ///   RD-5 (use [`TemporalSort::json_name`], not [`TemporalSort::flag_name`]).
    /// * `applied` — ranking actually served.  Per-arm values (F2 / AD-414-19;
    ///   corrected P3-2 / 2026-09-03):
    ///   - `"lexical"` — text-query arms only: `run_query` (including text +
    ///     `--blast-radius`); lexical BM25F order was served (or would have been,
    ///     for NoRankedRows).  Standalone `--blast-radius` (no text query) reports
    ///     `"none"`, not `"lexical"`, because no lexical ranking was computed.
    ///   - `"ast"` — reserved for #483 (standalone `--ast` degraded path); not
    ///     emitted by any call site in this release.
    ///   - `"none"` — standalone temporal arm (`run_temporal_standalone`); no
    ///     text query means no result set, so no ranking was served at all.
    /// * `flag` — `--`-prefixed flag for the human-readable tail (passed to
    ///   [`degraded_notice`]); pass `""` for the standalone temporal arm so
    ///   `degraded_notice` returns the base message without a tail.
    /// * `fallback` — controls the tail description in [`degraded_notice`].
    pub(super) fn new(
        u: &TemporalUnavailable,
        requested: &'static str,
        applied: &'static str,
        flag: &str,
        fallback: Fallback,
    ) -> Self {
        let message = degraded_notice(u, flag, fallback);
        DegradedJson {
            subsystem: "temporal",
            reason: u.reason.as_json_str(),
            requested,
            applied,
            message,
            // AD-414-25: detail-aware so a shallow-clone `Empty` advises
            // `git fetch --unshallow` rather than a rebuild that cannot help.
            remediation: u.reason.remediation_for(&u.detail),
        }
    }

    /// Construct a `DegradedJson` for blast-radius degradation.
    ///
    /// Uses [`blast_radius_degraded_msg`] as the message source so the JSON
    /// element matches the stderr notice emitted by [`resolve_blast_radius_paths`]
    /// (AC-7 / AC-19(b) byte-identical contract for `NotGitRepo`).
    ///
    /// All non-`NotGitRepo` reasons delegate to `degraded_notice` with
    /// `flag = "--blast-radius"` and [`Fallback::Lexical`], same as a
    /// `DegradedJson::new` call with those arguments would.
    pub(super) fn for_blast_radius(u: &TemporalUnavailable) -> Self {
        let message = blast_radius_degraded_msg(u);
        DegradedJson {
            subsystem: "temporal",
            reason: u.reason.as_json_str(),
            requested: "blast-radius",
            applied: "lexical",
            message,
            // AD-414-25: detail-aware so a shallow-clone `Empty` advises
            // `git fetch --unshallow` rather than a rebuild that cannot help.
            remediation: u.reason.remediation_for(&u.detail),
        }
    }
}

/// Coverage counters returned by [`apply_temporal_enrichment`] and
/// [`enrich_ast_results`] after the annotate pass (AD-414-13).
#[derive(Debug, Clone, Copy)]
pub(super) struct TemporalCoverage {
    /// Results that received a non-sentinel score for the requested dimension.
    pub ranked: usize,
    /// Total results passed to the enrichment function.
    pub total: usize,
    /// Per-file DB lookup failures during the annotate pass (E-16).
    pub lookup_errors: usize,
}

impl DegradedReason {
    /// Single builder for a `NoRankedRows` [`TemporalUnavailable`].
    ///
    /// Consolidates the `detail`-build + struct-construction that were duplicated
    /// at `ast.rs` (`run_ast_standalone`) and `mod.rs` (`run_query`) into one SSOT,
    /// so changing the wording of the zero-coverage notice requires editing one call
    /// chain only (Finding [medium/complexity]).
    ///
    /// `TemporalCoverage` is defined in this module; the low-level string builder
    /// `no_ranked_rows_detail` stays in `degraded.rs` (leaf module) and is called
    /// here rather than at the two former call sites.
    pub(super) fn no_ranked_rows(cov: TemporalCoverage) -> TemporalUnavailable {
        TemporalUnavailable {
            reason: Self::NoRankedRows,
            detail: Self::no_ranked_rows_detail(cov.total, cov.lookup_errors),
        }
    }
}

// ============================================================================
// DB open / state resolution
// ============================================================================

/// AD-414-1 / AD-414-15: single funnel for all temporal DB access.
///
/// Classification order (normative per AD-414-15):
/// not_git_repo → head_unresolved → [file present?] → corrupt/unsupported_version/
/// unreadable → repository_mismatch → open.
///
/// Note: `RepositoryMismatch` requires a successfully opened DB (it reads the
/// `meta.git_toplevel` row), so it is probed AFTER `TemporalDb::open` succeeds,
/// even though it ranks BEFORE `missing`/`empty` in the §2.3 precedence table.
/// An absent `git_toplevel` row is adopt-and-record, never a refusal (AD-413-16).
pub(super) fn open_temporal_state(root: &Path, cache_dir: &Path, head: &HeadState) -> TemporalOpen {
    let db_path = cache_dir.join("temporal.db");
    if !db_path.exists() {
        let reason = match head {
            HeadState::NotARepo => DegradedReason::NotGitRepo,
            HeadState::Unresolved => DegradedReason::HeadUnresolved,
            HeadState::Resolved(_) => DegradedReason::Missing,
        };
        return TemporalOpen::Unavailable(TemporalUnavailable {
            reason,
            detail: String::new(),
        });
    }
    match TemporalDb::open(&db_path) {
        Ok(db) => {
            // AD-413-16: check anchor via the already-open connection (no second SQLite
            // open; `anchor_state_on_db` reads META_GIT_TOPLEVEL from `db`).
            if let AnchorState::Differs { recorded, live } =
                super::staleness::anchor_state_on_db(&db, root)
            {
                return TemporalOpen::Unavailable(TemporalUnavailable {
                    reason: DegradedReason::RepositoryMismatch,
                    detail: format!("(recorded: {recorded:?}, live: {live:?})"),
                });
            }
            TemporalOpen::Open(db)
        }
        Err(SearchError::DatabaseCorrupt(m)) => TemporalOpen::Unavailable(TemporalUnavailable {
            reason: DegradedReason::Corrupt,
            detail: m,
        }),
        Err(SearchError::UnsupportedSchemaVersion { found, supported }) => {
            TemporalOpen::Unavailable(TemporalUnavailable {
                reason: DegradedReason::UnsupportedVersion,
                detail: DegradedReason::unsupported_version_detail(found, supported),
            })
        }
        Err(other) => TemporalOpen::Unavailable(TemporalUnavailable {
            reason: DegradedReason::Unreadable,
            detail: other.to_string(),
        }),
    }
}

/// AD-414-4: probe whether the requested temporal dimension has any rows.
///
/// Uses `top_hotspots(1)` for Hot/Cold, `top_risks(1)` for Risky — one probe
/// instead of N per-file lookups.  A query `Err` is treated as empty.
/// **Never used to probe `cochange`**; cochange emptiness never borrows the
/// shallow/empty wording (G-3).
pub(super) fn dimension_is_empty(db: &TemporalDb, sort: TemporalSort) -> bool {
    match sort {
        TemporalSort::Hot | TemporalSort::Cold => {
            db.top_hotspots(1).map_or(true, |rows| rows.is_empty())
        }
        TemporalSort::Risky => db.top_risks(1).map_or(true, |rows| rows.is_empty()),
    }
}

/// Variant of [`open_temporal_state`] that folds the emptiness probe for a
/// temporal-sort dimension (Finding [medium/complexity]).
///
/// When `sort` is `Some(s)`, opens the DB and additionally calls
/// [`dimension_is_empty`].  An open-but-empty DB is returned as
/// `Unavailable { reason: DegradedReason::Empty, detail: "" }`, so every
/// caller sees a two-way enum (Open-and-ready / Unavailable) rather than the
/// three-way shape (Open-non-empty / Open-empty / Unavailable) that was
/// formerly duplicated across all temporal call sites.
///
/// When `sort` is `None`, the call is identical to [`open_temporal_state`]:
/// no emptiness probe is made.  This is correct for blast-radius-only paths —
/// cochange emptiness never borrows the shallow/empty wording (G-3).
pub(super) fn open_temporal_state_for(
    root: &Path,
    cache_dir: &Path,
    head: &HeadState,
    sort: Option<TemporalSort>,
) -> TemporalOpen {
    match open_temporal_state(root, cache_dir, head) {
        TemporalOpen::Open(db) => {
            if let Some(s) = sort
                && dimension_is_empty(&db, s)
            {
                return TemporalOpen::Unavailable(empty_temporal_state(&db));
            }
            TemporalOpen::Open(db)
        }
        unavail => unavail,
    }
}

/// AD-414-24: the single query-time producer of `DegradedReason::Empty`.
///
/// Every arm that has to report an empty temporal DB calls this rather than
/// constructing the struct inline, so the three call sites — the sort-dimension
/// probe in [`open_temporal_state_for`], the composite blast-radius arm in
/// [`resolve_blast_radius_paths`], and the standalone blast-radius arm in
/// `mod.rs::run_temporal_standalone` — cannot drift from one another.
///
/// # AD-414-25 (F-C2-01): the shallow-clone branch
///
/// The build-time zero-row notice has always distinguished the two ways a
/// temporal DB ends up with no rows — a repository that genuinely has no
/// analysable history, versus a `--depth N` clone whose history was never
/// fetched — and names `git fetch --unshallow` for the second.  The query-time
/// notice did not: it advised `skim search --rebuild`, which on a still-shallow
/// clone rebuilds the same zero rows.  `sync()` records
/// [`rskim_search::META_IS_SHALLOW`] on every build (AD-414-14), so the flag is
/// read here from the connection the caller already holds.
///
/// The `"shallow"` detail string is the same signal
/// `temporal_build::zero_row_notice` emits, so both notices flow through the one
/// [`DegradedReason::full_message`] branch (AD-414-1 SSOT).  A `meta` read
/// failure or an absent row (pre-AD-414-14 DBs) degrades to the non-shallow
/// wording — never to a fabricated shallow claim.
pub(super) fn empty_temporal_state(db: &TemporalDb) -> TemporalUnavailable {
    let is_shallow = db
        .get_meta(rskim_search::META_IS_SHALLOW)
        .ok()
        .flatten()
        .is_some_and(|v| v.trim() == "1");
    TemporalUnavailable {
        reason: DegradedReason::Empty,
        detail: if is_shallow {
            "shallow".to_string()
        } else {
            String::new()
        },
    }
}

// ============================================================================
// Bounded re-sort window
// ============================================================================

/// Compute the bounded candidate window for a temporal re-sort.
///
/// `limit * 5`, clamped to at least 100, mirroring the original inline bound in
/// `query_standalone`.  Callers fetch this many candidates (in raw ranked order)
/// before enriching + re-sorting by temporal score, then truncate to `limit`.
/// This keeps per-file DB lookups bounded (`O(window)`, not `O(all matches)` —
/// AC-P1) while ensuring a temporally-hot file that ranks beyond `limit` in raw
/// order can still surface after the re-sort (AC-F4).
///
/// `saturating_mul` guards a hostile `--limit` near `usize::MAX` from overflowing.
pub(super) fn resort_window(limit: usize) -> usize {
    limit.saturating_mul(5).max(100)
}

// ============================================================================
// Blast-radius → FileId resolution (shared helper)
// ============================================================================

/// Convert a blast-radius path-to-Jaccard map to the corresponding `FileId`s.
///
/// AD-409-7: Delegates to [`paths_to_scored_file_ids`] and discards the score
/// component, so membership parity between the filter arm (lexical / AST) and
/// the composite ranking arm holds by construction — one algorithm, one copy of
/// the notice-dispatch logic.  After the manifest scan, emits at most two stderr
/// lines: "matched 0 indexed files" when nothing resolved; otherwise
/// [`emit_seed_unindexed_notice`] when the seed is absent from the manifest, and
/// [`emit_partial_drop_notice`] when one or more co-change partners are absent.
/// Each notice fires at most once per query.  Exit code stays 0.
/// No `--json` key is added here (tracked in #483).
///
/// Accepts a `&[&str]` slice (from `manifest.sorted_paths()`) so that callers
/// which already hold the slice can pass it directly without a second allocation.
///
/// This function is the membership-only twin of [`paths_to_scored_file_ids`].
/// Its two real call sites are the compound text+AST branch in `query.rs`
/// (which needs only the `FileId` set, not Jaccard scores) and
/// [`resolve_blast_radius_file_ids`].
pub(super) fn paths_to_file_ids(
    sorted_paths: &[&str],
    allowed_paths: &BlastRadiusStrengths,
) -> HashSet<FileId> {
    paths_to_scored_file_ids(sorted_paths, allowed_paths)
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

/// Emit the one-line stderr notice for the "nothing resolved" case: the
/// blast-radius allowlist is non-empty but not one of its paths has a `FileId`
/// in the manifest.
///
/// Called by [`paths_to_scored_file_ids`] (the single implementation of the
/// manifest scan) when the blast-radius allowlist is non-empty but zero entries
/// resolved.  Reported separately from the partial-drop notice because a
/// "N−1 of N−1 partners not found" line is less informative than naming both
/// the allowlist size and the index size when nothing at all resolved.
fn emit_no_indexed_files_notice(allowlist_len: usize, indexed_file_count: usize) {
    eprintln!(
        "skim search: note: blast-radius filter matched 0 indexed files \
         (allowed {allowlist_len} paths, index has {indexed_file_count} files)"
    );
}

/// Emit a one-line stderr notice when the blast-radius target file itself is absent
/// from the lexical manifest (e.g. the file was deleted from disk while
/// `temporal.db` still records it, or the file is outside the indexed subtree).
///
/// AD-409-7: the seed is always excluded from the partner-drop arithmetic — emitting
/// a separate notice keeps the partner count truthful ("N of M partners" never
/// silently inflates by 1 when the seed is also absent).  Callers suppress the
/// partner-drop notice independently when zero partners were actually dropped.
///
/// Emits [`BLAST_RADIUS_SEED_UNINDEXED_NOTICE`] verbatim so intra-crate tests
/// can assert against the constant rather than a duplicated string literal.
fn emit_seed_unindexed_notice() {
    eprintln!("{BLAST_RADIUS_SEED_UNINDEXED_NOTICE}");
}

/// Emit a one-line stderr notice when co-change partner paths are absent from
/// the indexed manifest.
///
/// Called by [`paths_to_scored_file_ids`] (the single implementation of the
/// manifest scan) after the scan completes.  The notice is suppressed when
/// `dropped == 0`.  Callers report the fully-unresolved case (`found == 0`)
/// through [`emit_no_indexed_files_notice`] instead.
///
/// `partner_count` is the number of partner slots in the allowlist (total entries
/// minus the seed slot when a seed is present, or total entries when there is no
/// seed); `partners_found` is the count of resolved entries whose path is not the
/// seed path (incremented once per partner in the scan loop, never derived from
/// totals after the fact).  The dropped count is therefore
/// `partner_count − partners_found`, computed with one `saturating_sub` (PF-004).
///
/// AC `ac409_4_unindexed_partner_omission_is_disclosed` verifies the exact
/// message wording end-to-end.  Uses [`BLAST_RADIUS_PARTNER_NOT_FOUND`] as the
/// static suffix so intra-crate tests can reference the constant.
fn emit_partial_drop_notice(partner_count: usize, partners_found: usize) {
    let dropped = partner_count.saturating_sub(partners_found);
    if dropped > 0 {
        eprintln!(
            "skim search: note: {dropped} of {partner_count} {}",
            BLAST_RADIUS_PARTNER_NOT_FOUND
        );
    }
}

/// Convert a blast-radius path-to-Jaccard map to a scored `(FileId, f64)` layer
/// for the temporal RRF pass inside `run_blast_radius_composite_query`.
///
/// AD-409-2: This is the scored twin of [`paths_to_file_ids`] — both agree on
/// *membership* (every path that has a `FileId` in the manifest appears in both
/// outputs) and differ only in what they return: a `HashSet<FileId>` for the
/// membership-only filter path vs. a `Vec<(FileId, f64)>` scored layer for the
/// composite ranking path.  The blast-radius target carries `SEED_STRENGTH = 2.0`
/// (> max Jaccard of 1.0) so it always occupies temporal rank 1 after
/// `merge_layer_scores`' per-layer total sort.  Each co-change partner carries
/// its actual Jaccard co-change strength as read from `TemporalDb::cochanges_for_file`.
///
/// **AC-15 finiteness**: a partner whose stored Jaccard is NaN or ±∞ (the DB
/// should not produce this, but is guarded defensively) is mapped to
/// `NON_FINITE_JACCARD_FLOOR` (0.0), which is strictly below
/// `rskim_search::MIN_COCHANGE_JACCARD` (0.10).  The file still appears in the
/// output, ranked below every partner with a finite Jaccard ≥ 0.10, and the
/// fallback neither panics nor propagates NaN into the RRF denominator.
///
/// **AD-409-7 partial-drop notice**: after the manifest scan, emits **at most
/// two** stderr lines — "matched 0 indexed files" when nothing resolved; otherwise
/// [`emit_seed_unindexed_notice`] when the seed is absent from the manifest, and
/// [`emit_partial_drop_notice`] when one or more co-change partners are absent.
/// Both notices are suppressed when their respective conditions are not met.
/// [`paths_to_file_ids`] delegates to this function and discards the score
/// component, so the two blast-radius arms always agree on membership and notice
/// text by construction (AD-409-7; AC-7).  Exit code stays 0; no `--json` key
/// added (tracked in #483).
///
/// Applies PF-004 widening (`u32::try_from(idx)`) — never `as u32`.
pub(super) fn paths_to_scored_file_ids(
    sorted_paths: &[&str],
    allowed_paths: &BlastRadiusStrengths,
) -> Vec<(FileId, f64)> {
    // AD-409-5: ONE bounded pass over the manifest slice.  Each path is looked
    // up in the allowlist map exactly once; a `(FileId, score)` pair is emitted
    // only when the path is present.  The pass is bounded by
    // `sorted_paths.len()` — the indexed file count, which is capped by
    // `COUPLING_MAX_FILES` at index-build time so no explicit per-loop bound
    // is needed here.
    //
    // AD-409-7: Pre-compute the seed path exactly once so that seed identity is
    // carried explicitly (as `Option<&str>`) rather than inferred per-entry by
    // float comparison.  `cochange_partner_strengths` guarantees that only the
    // explicit `allowed_paths.insert(normalized, SEED_STRENGTH)` call produces
    // a value of 2.0; DB-sourced rows are clamped to [0.0, 1.0] at the trust
    // boundary.  When `seed_path` is `None` — a seedless allowlist such as a
    // test helper whose values are all 1.0 — the seed-unindexed notice is
    // suppressed entirely, preventing a false user-visible disclosure.
    // AD-409-9: an empty allowlist is a sentinel, never a scan.  Two producers
    // reach here with one: the `AnchorDiffers` `Filtered { allow: empty }` arm
    // (mismatch notice already emitted) and the zero-partner seed suppression in
    // `resolve_blast_radius_paths` ("no co-change data for X" already emitted).
    // Both have disclosed the situation; running the manifest scan would only add
    // a third, less informative "matched 0 indexed files (allowed 0 paths, …)"
    // line.  Return before any notice so exactly one explanation reaches stderr.
    if allowed_paths.is_empty() {
        return Vec::new();
    }
    let seed_path: Option<&str> = allowed_paths
        .iter()
        .find_map(|(k, &v)| (v == SEED_STRENGTH).then_some(k.as_str()));
    // `partner_count` is the number of partner slots: total entries minus the
    // seed slot when a seed is present, or all entries when there is no seed.
    let partner_count = if seed_path.is_some() {
        allowed_paths.len().saturating_sub(1)
    } else {
        allowed_paths.len()
    };
    let mut scored: Vec<(FileId, f64)> = Vec::with_capacity(allowed_paths.len());
    let mut seed_resolved = false;
    let mut partners_found: usize = 0;
    for (idx, path) in sorted_paths.iter().enumerate() {
        if let Some(&jaccard) = allowed_paths.get(*path) {
            // PF-004: widen idx (usize) to u32 before constructing FileId.
            // The file cap (50 000) guarantees no overflow, but `try_from`
            // makes the widening explicit and safe by construction.
            if let Ok(id) = u32::try_from(idx) {
                // AC-15: NON_FINITE_JACCARD_FLOOR guards any residual non-finite
                // value that survived into the allowlist (belt-and-suspenders).
                // The primary guard is the Jaccard clamping in
                // `cochange_partner_strengths`; SEED_STRENGTH (2.0) itself is
                // finite and passes the is_finite() check correctly.
                let safe_score = if jaccard.is_finite() {
                    jaccard
                } else {
                    NON_FINITE_JACCARD_FLOOR
                };
                scored.push((FileId(id), safe_score));
                // Identify the seed by path, not by value, so that a seedless
                // allowlist (seed_path == None) never sets seed_resolved = true
                // and never triggers a false seed-unindexed notice.
                if seed_path == Some(*path) {
                    seed_resolved = true;
                } else {
                    // Count resolved co-change partners (excludes the seed path).
                    partners_found += 1;
                }
            }
        }
    }
    // AD-409-7: emit notices for the two disclosure conditions.
    // `scored.is_empty()` is the fully-unresolved case; the partner-drop
    // arithmetic is only meaningful once at least one entry resolved.
    // Stderr only; no --json key; no degraded element (tracked in #483).
    if scored.is_empty() {
        emit_no_indexed_files_notice(allowed_paths.len(), sorted_paths.len());
    } else {
        // Only emit the seed-unindexed notice when a seed was intended
        // (`seed_path.is_some()`) but its path was absent from the manifest.
        // Suppressing it when seed_path is None prevents a false positive on
        // seedless allowlists (e.g. AC-24 guard tests with all-1.0 values).
        if seed_path.is_some() && !seed_resolved {
            emit_seed_unindexed_notice();
        }
        emit_partial_drop_notice(partner_count, partners_found);
    }
    scored
}

/// Resolution of a `--blast-radius` request.
///
/// Replaces the former `(Option<HashSet<String>>, Option<TemporalUnavailable>)` tuple
/// to make illegal states unrepresentable (the old `(Some, Some)` was unreachable but
/// representable) and to give `RepositoryMismatch` a machine-readable signal (AC-7):
/// it previously returned `(Some(empty), None)` so callers' `output.degraded` never
/// received an entry, producing the silent-degradation class #414 exists to eliminate.
///
/// Variants map to the former tuple encoding as follows, for callers that need both
/// the path filter and the degraded signal:
/// - `NotRequested`          → paths = None,        degraded = None
/// - `Allowed(paths)`        → paths = Some(paths), degraded = None
/// - `Filtered { allow, .. }`→ paths = Some(allow), degraded = Some(...)
/// - `Degraded(u)`           → paths = None,        degraded = Some(u)
#[derive(Debug)]
pub(super) enum BlastRadiusResolution {
    /// `--blast-radius` was not requested; skip filter entirely.
    NotRequested,
    /// Resolved successfully; `allow` maps each co-change partner path to its
    /// Jaccard score, plus the blast-radius target keyed to [`SEED_STRENGTH`]
    /// (2.0 > max Jaccard 1.0).  Downstream callers build the temporal RRF layer
    /// with per-partner scores so stronger co-change relationships rank higher
    /// (see [`super::query::run_blast_radius_composite_query`] and AD-409-2).
    /// The target always ranks first in the temporal layer.
    Allowed(BlastRadiusStrengths),
    /// Resolved but the DB is degraded: `allow` is the effective path filter
    /// (empty for `RepositoryMismatch`) and `degraded` carries the reason so
    /// callers can push a `DegradedJson` entry to `output.degraded` (AC-7).
    Filtered {
        allow: BlastRadiusStrengths,
        degraded: TemporalUnavailable,
    },
    /// Fully degraded: temporal DB is unavailable; blast-radius cannot be applied.
    Degraded(TemporalUnavailable),
}

/// Resolve a `--blast-radius` raw path to the set of co-change partner paths.
///
/// Shared core for both `resolve_blast_radius_file_ids` (standalone AST path, and
/// text + blast-radius path in `mod.rs`).  Returns a
/// [`BlastRadiusResolution`] that encodes whether blast-radius was requested, resolved
/// successfully, or degraded — and in the `RepositoryMismatch` case, carries BOTH an
/// empty allowlist (zero results) AND the degraded reason for `output.degraded`.
///
/// `head` is the [`HeadState`] already resolved by the caller (Finding 2 fix:
/// returned by `auto_refresh_if_stale` so it need not be re-derived here).
/// It is passed to [`open_temporal_state`] to classify DB-absent cases (AD-414-15).
///
/// # Errors
///
/// Returns `Err` only when path normalization fails (outside-repo or missing file).
pub(super) fn resolve_blast_radius_paths(
    blast_radius: Option<&str>,
    root: &Path,
    cache_dir: &Path,
    json: bool,
    head: &HeadState,
) -> anyhow::Result<BlastRadiusResolution> {
    let Some(raw_path) = blast_radius else {
        return Ok(BlastRadiusResolution::NotRequested);
    };

    // AD-414-1 / AD-414-15: open_temporal_state is the single funnel for all temporal
    // DB access.  RepositoryMismatch → wrong co-change data would be served; every other
    // Unavailable variant → degrade gracefully.  Both arms emit a reason-specific human-
    // readable string via degraded_notice (AD-414-1).
    let db = match open_temporal_state(root, cache_dir, head) {
        TemporalOpen::Open(db) => db,
        TemporalOpen::Unavailable(u) => {
            // AC-7 / AC-19(b): see blast_radius_degraded_msg for the
            // NotGitRepo legacy-format contract and the two-site agreement.
            let msg = blast_radius_degraded_msg(&u);
            if json {
                let envelope = serde_json::json!({ "warning": msg });
                eprintln!("{}", serde_json::to_string(&envelope)?);
            } else {
                eprintln!("skim search: {msg}");
            }
            // AD-413-16 / PF-016: RepositoryMismatch means the DB belongs to a
            // different repository.  Returning None for the allowlist would overload
            // the "not requested" sentinel — callers' .map() would yield None,
            // bypassing the file filter entirely and serving the full unfiltered
            // index.  Filtered with an empty allowlist forces zero results on all three
            // blast-radius call sites (AC-7: also carries the degraded reason so
            // output.degraded receives an entry — previously silent).
            if u.reason == DegradedReason::RepositoryMismatch {
                return Ok(BlastRadiusResolution::Filtered {
                    allow: HashMap::new(),
                    degraded: u,
                });
            }
            // Return the unavailable reason so the caller can push DegradedJson.
            return Ok(BlastRadiusResolution::Degraded(u));
        }
    };

    let normalized = normalize_blast_radius_path(raw_path, root)?;
    let partners = db.cochanges_for_file(&normalized)?;
    if partners.is_empty() {
        // T-7/AC-7: distinguish "DB is entirely empty" from "DB has data but not for
        // this file".  Only emit the Empty degraded notice when the hotspot table is
        // also empty — confirming the DB truly has no temporal data at all.  When the
        // DB has hotspot rows but no co-change for this specific file, emit the
        // non-degraded "no co-change data" message instead and proceed (blast-radius
        // still applies the {target_file}-only filter rather than disappearing).
        //
        // PF-006 guard: a DB with co-change data but empty hotspot (possible in tests)
        // reaches this branch only when the queried file has no co-change rows — the
        // non-empty-partners path above handles it correctly without any emptiness
        // check.
        if dimension_is_empty(&db, TemporalSort::Hot) {
            // AD-414-25: shallow-aware Empty (see `empty_temporal_state`).
            let u = empty_temporal_state(&db);
            let msg = degraded_notice(&u, "--blast-radius", Fallback::Lexical);
            eprintln!("skim search: {msg}");
            return Ok(BlastRadiusResolution::Degraded(u));
        }
        eprintln!("skim search: no co-change data for {raw_path:?}");
    }
    let mut allowed_paths = cochange_partner_strengths(&partners, &normalized);
    // Include the target file itself so queries like `skim search auth --blast-radius src/auth.rs`
    // surface matches within the target file in addition to its co-change partners.
    // SEED_STRENGTH (2.0 > max Jaccard 1.0) ensures the target ranks first in the temporal layer.
    //
    // AD-409-9 (F-C1-01): the seed is injected ONLY when the target actually has
    // a co-change relation — i.e. at least one cochange row exists for it.  A
    // blast radius IS a co-change relation; with zero partners there is no
    // relation to rank, so seeding the temporal layer with the target alone
    // manufactured a ranking out of nothing: `skim search <gibberish>
    // --blast-radius solo.rs` returned solo.rs as its own `co_change_partner` at
    // the bare RRF sentinel score (temporal weight / 61), immediately after
    // stderr had said "no co-change data for solo.rs".  #409's AC-20 forbids
    // exactly that ("MUST NOT fabricate any ranking"); AC-2's seed-first rule
    // (Option A, AD-409-3) is scoped to targets that HAVE partners and is
    // unchanged here.
    //
    // The empty allowlist that results is the "blast radius contributes nothing"
    // sentinel already understood downstream: `blast_temporal_layer` early-outs
    // to `empty_output` (ADR-009) and `paths_to_scored_file_ids` returns an empty
    // layer, so every blast-radius arm reports zero results — matching what the
    // standalone `--blast-radius` arm already returns for the same target.
    if !partners.is_empty() {
        allowed_paths.insert(normalized, SEED_STRENGTH);
    }
    Ok(BlastRadiusResolution::Allowed(allowed_paths))
}

/// Resolve a `--blast-radius` raw path to the set of matching `FileId`s.
///
/// Unified resolver used by every blast-radius call site:
/// - `run_ast_standalone` caller in `mod.rs` (standalone `--ast --blast-radius`)
/// - `execute_query_with_manifest` blast-radius arm (query.rs, via `paths_to_file_ids`)
/// - `resolve_blast_radius_file_ids` (mod.rs, text + blast-radius composite path)
///
/// Algorithm:
/// 1. If `blast_radius` is `None`, return `Ok(None)` immediately.
/// 2. Open `temporal.db` under `cache_dir`.  If absent/corrupt/empty, emit the
///    degraded notice and return `Ok(None)`.
/// 3. Normalize the raw path to repo-relative form.
/// 4. Look up co-change partners (with Jaccard scores), add the target with `SEED_STRENGTH`.
/// 5. Convert the path map to `FileId`s via `paths_to_file_ids`.
/// 6. Return `Ok(Some(file_ids))`.
///
/// # Errors
///
/// Returns `Err` only when path normalization fails (outside-repo or missing file).
pub(super) fn resolve_blast_radius_file_ids(
    blast_radius: Option<&str>,
    root: &Path,
    cache_dir: &Path,
    sorted_paths: &[&str],
    json: bool,
    head: &HeadState,
) -> anyhow::Result<Option<HashSet<FileId>>> {
    let allowed_paths = match resolve_blast_radius_paths(blast_radius, root, cache_dir, json, head)?
    {
        BlastRadiusResolution::NotRequested => return Ok(None),
        BlastRadiusResolution::Allowed(paths) => paths,
        BlastRadiusResolution::Filtered { allow, .. } => allow,
        BlastRadiusResolution::Degraded(_) => return Ok(None),
    };
    let file_ids = paths_to_file_ids(sorted_paths, &allowed_paths);
    Ok(Some(file_ids))
}

/// Check whether the temporal database is stale compared to the current git HEAD.
///
/// Returns `Some(warning_message)` when the stored HEAD differs from the
/// current HEAD, `None` when current or when the staleness check cannot be
/// performed (missing git, non-git repo, missing meta key).
///
/// # Usage note (Decision O-B)
///
/// This function is no longer called on the production query path —
/// `auto_refresh_if_stale` in `staleness.rs` guarantees freshness before any
/// query executes, making this staleness warning dead code on the happy path.
/// It is retained for test use only (AC6 discriminating assertion in
/// `temporal_build_tests.rs`).
#[cfg(test)]
pub(super) fn check_temporal_staleness(db: &TemporalDb, project_root: &Path) -> Option<String> {
    let stored_head = db.get_meta(rskim_search::META_GIT_HEAD).ok().flatten()?;

    let current_head = read_git_head(project_root)?;
    if stored_head.trim() != current_head.trim() {
        Some(format!(
            "skim search: temporal data is stale (stored: {}, current: {}). \
             Run 'skim search' on this repo to auto-refresh.",
            stored_head.get(..7).unwrap_or(&stored_head),
            current_head.get(..7).unwrap_or(&current_head),
        ))
    } else {
        None
    }
}

/// Read the current git HEAD SHA from the project root.
///
/// Spawns `git rev-parse HEAD` with a 5-second timeout. Returns `None` on
/// timeout, spawn failure, non-zero exit, or non-git directory.
///
/// The timeout prevents indefinite hangs on network-mounted repos or
/// corrupted `.git` directories. The staleness check is advisory, so
/// timing out is safe — the caller degrades gracefully.
///
/// Only compiled in test builds — see `check_temporal_staleness` doc.
#[cfg(test)]
fn read_git_head(root: &Path) -> Option<String> {
    use std::sync::mpsc;
    use std::time::Duration;

    const TIMEOUT: Duration = Duration::from_secs(5);

    let child = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("rev-parse")
        .arg("HEAD")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    let child_id = child.id();
    let (tx, rx) = mpsc::channel::<Option<String>>();

    std::thread::spawn(move || {
        let result = child.wait_with_output().ok().and_then(|out| {
            if out.status.success() {
                Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
            } else {
                None
            }
        });
        let _ = tx.send(result);
    });

    match rx.recv_timeout(TIMEOUT) {
        Ok(result) => result,
        Err(_timeout) => {
            // Kill the subprocess so it doesn't linger after we give up.
            #[cfg(unix)]
            {
                // SAFETY: kill(2) is always safe to call with a valid pid.
                unsafe {
                    libc::kill(child_id as libc::pid_t, libc::SIGKILL);
                }
            }
            #[cfg(not(unix))]
            {
                let _ = std::process::Command::new("taskkill")
                    .args(["/F", "/PID", &child_id.to_string()])
                    .status();
            }
            None
        }
    }
}

// ============================================================================
// Private helpers
// ============================================================================

/// Given a co-change row, return the path of the file that is NOT `target`.
///
/// Co-change pairs are stored with the lexically smaller path in `file_a`. This
/// helper resolves both directions so callers don't need to repeat the pattern.
fn cochange_partner<'a>(row: &'a rskim_search::CochangeRow, target: &str) -> &'a str {
    if row.file_a == target {
        &row.file_b
    } else {
        &row.file_a
    }
}

/// Extract the map of partner paths to their co-change Jaccard scores from a slice of
/// co-change rows.
///
/// AD-409-1: Returns a [`BlastRadiusStrengths`] map of partner paths to their Jaccard
/// co-change scores. Both `file_a`→`file_b` and `file_b`→`file_a` directions are
/// resolved via `cochange_partner`, preserving the Jaccard value for each partner.
/// The `target` file itself is NOT included — callers add it separately with
/// [`SEED_STRENGTH`] when needed.
///
/// **Trust-boundary Jaccard clamping (security):** Jaccard is mathematically
/// confined to `[0.0, 1.0]`.  Any finite value outside that range arriving from
/// `temporal.db` is corrupt data.  Such values — including a corrupt row carrying
/// exactly `SEED_STRENGTH` (2.0) — are mapped to [`NON_FINITE_JACCARD_FLOOR`]
/// here, at the single point where partner rows cross the DB trust boundary.
/// This guarantees that only the explicit `allowed_paths.insert(normalized,
/// SEED_STRENGTH)` call performed by the caller can produce a sentinel value of
/// 2.0 in the allowlist; no corrupt co-change row can impersonate the seed.
pub(super) fn cochange_partner_strengths(
    partners: &[rskim_search::CochangeRow],
    target: &str,
) -> BlastRadiusStrengths {
    partners
        .iter()
        .map(|p| {
            let j = p.jaccard;
            // Clamp to the mathematical Jaccard range [0.0, 1.0].  Non-finite
            // values and out-of-range values (including a corrupt 2.0 that would
            // impersonate SEED_STRENGTH) are both mapped to the floor.
            let safe = if j.is_finite() && (0.0..=1.0).contains(&j) {
                j
            } else {
                NON_FINITE_JACCARD_FLOOR
            };
            (cochange_partner(p, target).to_string(), safe)
        })
        .collect()
}

// ============================================================================
// Standalone temporal query
// ============================================================================

/// Output variants from a standalone temporal query.
#[derive(Debug)]
pub(super) enum TemporalQueryOutput {
    /// Top hotspot files (--hot).
    Hotspots(Vec<HotspotRow>),
    /// Top coldspot files (--cold).
    Coldspots(Vec<HotspotRow>),
    /// Top risky files (--risky).
    Risks(Vec<RiskRow>),
    /// Co-change partners of a target file (--blast-radius).
    Cochanges {
        target: String,
        partners: Vec<rskim_search::CochangeRow>,
    },
}

impl TemporalQueryOutput {
    /// Number of results in the current page.
    ///
    /// Used by the bounded-page-notice in `run_temporal_standalone` (AD-404-8)
    /// to report how many results were shown before emitting the "more exist"
    /// hint on stderr.
    pub(super) fn result_count(&self) -> usize {
        match self {
            Self::Hotspots(rows) => rows.len(),
            Self::Coldspots(rows) => rows.len(),
            Self::Risks(rows) => rows.len(),
            Self::Cochanges { partners, .. } => partners.len(),
        }
    }
}

/// Execute a standalone temporal query (no text query).
///
/// - `sort`: optional sort mode (Hot, Cold, Risky).
/// - `blast_radius`: optional file path for co-change partner lookup.
/// - `page`: pagination cursor (limit + offset); AD-404 standalone paths fix.
/// - `db`: open temporal database.
/// - `project_root`: needed for path normalization of `blast_radius`.
///
/// Returns `(output, has_more)` where `has_more` is the sound pagination
/// terminator (AD-404-11 / D-5): true when more results exist beyond this page.
///
/// # Errors
///
/// Returns an error if path normalization fails or the database query fails.
pub(super) fn query_standalone(
    sort: Option<TemporalSort>,
    blast_radius: Option<&str>,
    page: Page,
    db: &TemporalDb,
    project_root: &Path,
) -> anyhow::Result<(TemporalQueryOutput, bool)> {
    if let Some(raw_path) = blast_radius {
        let normalized = normalize_blast_radius_path(raw_path, project_root)?;
        let mut partners = db.cochanges_for_file(&normalized)?;

        if let Some(sort_mode) = sort {
            // AD-404-7: temporal re-sort window is fixed at resort_window(page.limit()),
            // NOT resort_window(page.depth()). Offset-independent so pages are provably
            // disjoint with no duplicate/miss defect; see D-2 user sign-off 2026-07-15.
            let window = resort_window(page.limit());
            let window_capped = partners.len() > window;
            partners.truncate(window);
            resort_partners_by_temporal(&mut partners, sort_mode, &normalized, db)?;
            let pre_page_len = partners.len();
            page.apply(&mut partners);
            // has_more: either the temporal window was capped (AD-404-8 bounded-page
            // notice seam) or there are more verified rows within the window than
            // the current page consumes (AD-404-11).
            let has_more = window_capped || pre_page_len > page.depth();
            return Ok((
                TemporalQueryOutput::Cochanges {
                    target: normalized,
                    partners,
                },
                has_more,
            ));
        }

        // No sort: cochanges returned in Jaccard DESC order from DB (all partners,
        // no internal truncation). Apply page directly.
        let total_before = partners.len();
        page.apply(&mut partners);
        let has_more = total_before > page.depth();
        return Ok((
            TemporalQueryOutput::Cochanges {
                target: normalized,
                partners,
            },
            has_more,
        ));
    }

    // No blast-radius — pure temporal sort.
    // Over-fetch page.depth() + 1 rows so we can detect whether more results
    // exist beyond the current page (the "+1 sentinel" trick: if we get back
    // exactly depth+1 rows, has_more is true; fewer rows means this is the last page).
    let fetch_limit = page.depth().saturating_add(1);

    match sort {
        Some(TemporalSort::Hot) | None => {
            let (rows, has_more) = paginate_sentinel(page, db.top_hotspots(fetch_limit)?);
            Ok((TemporalQueryOutput::Hotspots(rows), has_more))
        }
        Some(TemporalSort::Cold) => {
            let (rows, has_more) = paginate_sentinel(page, db.top_coldspots(fetch_limit)?);
            Ok((TemporalQueryOutput::Coldspots(rows), has_more))
        }
        Some(TemporalSort::Risky) => {
            let (rows, has_more) = paginate_sentinel(page, db.top_risks(fetch_limit)?);
            Ok((TemporalQueryOutput::Risks(rows), has_more))
        }
    }
}

/// Detect pagination end and apply skip+take on a sentinel-over-fetched row vec.
///
/// The caller must have fetched `page.depth().saturating_add(1)` rows; the
/// extra element acts as a sentinel: `has_more` is true iff the DB returned it.
/// `page.apply` drops the sentinel by draining `offset` rows then truncating to
/// `limit`, so a separate `truncate(depth)` call is not needed.
fn paginate_sentinel<T>(page: Page, mut rows: Vec<T>) -> (Vec<T>, bool) {
    let has_more = rows.len() > page.depth();
    page.apply(&mut rows);
    (rows, has_more)
}

/// Re-sort blast-radius partners by temporal score using per-file lookups.
///
/// Callers MUST pre-truncate `partners` to a reasonable window before calling
/// this function to bound the number of per-file DB queries.
///
/// Uses `hotspot_for_file` / `risk_for_file` for each partner individually,
/// avoiding bulk table loads. Absent entries sort last (score 0.0).
///
/// # Errors
///
/// Returns an error if any per-file DB query fails.
fn resort_partners_by_temporal(
    partners: &mut Vec<rskim_search::CochangeRow>,
    sort_mode: TemporalSort,
    normalized: &str,
    db: &TemporalDb,
) -> anyhow::Result<()> {
    // Compute scores eagerly into a parallel Vec — one entry per partner.
    // Scores are keyed by position so we can sort an index Vec without
    // touching `partners` until the final permutation step.
    let scores: Vec<f64> = partners
        .iter()
        .map(|row| -> anyhow::Result<f64> {
            let partner = cochange_partner(row, normalized);
            match sort_mode {
                TemporalSort::Hot | TemporalSort::Cold => Ok(db
                    .hotspot_for_file(partner)?
                    .map(|h| h.score)
                    .unwrap_or(0.0)),
                TemporalSort::Risky => Ok(db
                    .risk_for_file(partner)?
                    .map(|r| r.risk_score)
                    .unwrap_or(0.0)),
            }
        })
        .collect::<anyhow::Result<_>>()?;

    // Sort an index Vec by score, then apply the permutation to `partners`.
    let mut indices: Vec<usize> = (0..partners.len()).collect();
    indices.sort_by(|&a, &b| {
        if sort_mode == TemporalSort::Cold {
            scores[a].partial_cmp(&scores[b])
        } else {
            scores[b].partial_cmp(&scores[a])
        }
        .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Apply permutation: collect in sorted order, then replace `partners`.
    *partners = indices.into_iter().map(|i| partners[i].clone()).collect();
    Ok(())
}

// ============================================================================
// Output formatters
// ============================================================================

/// Format the bounded-page stderr notice emitted whenever `has_more=true`.
///
/// ## AD-404-8: bounded-page-notice emission site
///
/// Shared across all search dispatch paths — not limited to standalone temporal
/// queries.  Current callers:
///
/// * `run_temporal_standalone` (temporal-only: `--hot`/`--cold`/`--risky`) —
///   capped at the temporal ranking window or more rows exist in the DB.
/// * `mod.rs` text+temporal arm — mirrors standalone, fires after the combined
///   lexical+temporal result set is paged.
/// * `mod.rs` pure-text arm — no temporal window; fires when the verified
///   candidate pool exceeds the requested page (AD-404-11 / D-5).
/// * `ast.rs` `--ast`+temporal arm — fires after `page.apply()` when the
///   pre-page pool exceeded the current page depth.
///
/// Goes to stderr (#377 seam, PF-006) so `--json` stdout stays byte-identical.
///
/// `n` is the count of results in the current page, used so agents see exactly
/// how many they received before the "more results exist" hint.
pub(super) fn bounded_page_notice(n: usize, offset: usize, limit: usize) -> String {
    format!(
        "skim search: showing {n} result(s) (offset {offset}, limit {limit}); \
         more results exist. \
         Use --offset {} to page forward or increase --limit.",
        offset.saturating_add(limit)
    )
}

/// Return the empty-result message for a standalone temporal query page.
///
/// - At `offset 0`: "No {kind} data available." — the data source appears empty.
/// - At `offset > 0`: "No {kind} results at offset N (try a smaller --offset)."
///   — the page is exhausted, not the data source.
///
/// Used by `format_temporal_text` for the three typed arms (hotspot / coldspot /
/// risk).  The co-change arm handles its own empty message because it includes a
/// `target` path in both forms.
fn page_empty_msg(kind: &str, page: Page) -> String {
    if page.offset() > 0 {
        format!(
            "No {kind} results at offset {} (try a smaller --offset).",
            page.offset()
        )
    } else {
        format!("No {kind} data available.")
    }
}

/// Return the 1-indexed range label for a temporal result page header.
///
/// - At `offset 0`: `first_page` (backward-compatible with golden fixtures,
///   e.g. "top 5" or "5 files").
/// - At `offset > 0`: "items {first}–{last}" using saturating arithmetic (PF-004).
///
/// `n` is the number of rows in the current page; it must equal `first_page`'s
/// embedded count when `offset == 0` (no check — callers are responsible).
fn page_range_label(first_page: &str, n: usize, page: Page) -> String {
    if page.offset() == 0 {
        first_page.to_string()
    } else {
        format!(
            "items {}–{}",
            page.offset().saturating_add(1),
            page.offset().saturating_add(n)
        )
    }
}

/// Format a standalone temporal query result as human-readable text.
///
/// ## AC-404-10: page-aware headers and empty messages
///
/// `page` is required so that:
/// - Headers say "top N" only at offset 0 (backward-compatible with golden
///   fixtures); at offset > 0 they show `offset+1 .. offset+count` instead.
/// - An empty result page at offset > 0 says "no more results at this offset"
///   rather than the misleading "No hotspot data available." message (which
///   implies the data source is empty, not that the page is exhausted).
pub(super) fn format_temporal_text(
    output: &TemporalQueryOutput,
    page: Page,
    w: &mut impl Write,
) -> anyhow::Result<()> {
    match output {
        TemporalQueryOutput::Hotspots(rows) => {
            if rows.is_empty() {
                writeln!(w, "{}", page_empty_msg("hotspot", page))?;
                return Ok(());
            }
            // Single newline after header (writeln! already appends \n; no
            // extra \n in the format string — that would insert a blank line).
            //
            // At offset 0: "top N" (backward-compatible with golden fixtures).
            // At offset > 0: 1-indexed range via page_range_label (PF-004 saturating).
            let range = page_range_label(&format!("top {}", rows.len()), rows.len(), page);
            writeln!(w, "Hotspots ({range}, 90-day decay):")?;
            writeln!(w, "  Score  30d  90d  Path")?;
            writeln!(w, "  ─────  ───  ───  ────────────────────────────────")?;
            for r in rows {
                writeln!(
                    w,
                    "  {:.3}   {:>4} {:>4}  {}",
                    r.score, r.changes_30d, r.changes_90d, r.file_path
                )?;
            }
        }
        TemporalQueryOutput::Coldspots(rows) => {
            if rows.is_empty() {
                writeln!(w, "{}", page_empty_msg("coldspot", page))?;
                return Ok(());
            }
            let range = page_range_label(&format!("top {}", rows.len()), rows.len(), page);
            writeln!(w, "Coldspots ({range}, least active):")?;
            writeln!(w, "  Score  30d  90d  Path")?;
            writeln!(w, "  ─────  ───  ───  ────────────────────────────────")?;
            for r in rows {
                writeln!(
                    w,
                    "  {:.3}   {:>4} {:>4}  {}",
                    r.score, r.changes_30d, r.changes_90d, r.file_path
                )?;
            }
        }
        TemporalQueryOutput::Risks(rows) => {
            if rows.is_empty() {
                writeln!(w, "{}", page_empty_msg("risk", page))?;
                return Ok(());
            }
            let range = page_range_label(&format!("top {}", rows.len()), rows.len(), page);
            writeln!(w, "Risk hotspots ({range}):\n")?;
            writeln!(w, "  Risk   Fix%   Fixes  Total  Path")?;
            writeln!(
                w,
                "  ─────  ─────  ─────  ─────  ────────────────────────────────"
            )?;
            for r in rows {
                writeln!(
                    w,
                    "  {:.3}  {:>5.1}%  {:>5}  {:>5}  {}",
                    r.risk_score,
                    r.fix_density * 100.0,
                    r.fix_commits,
                    r.total_commits,
                    r.file_path
                )?;
            }
        }
        TemporalQueryOutput::Cochanges { target, partners } => {
            if partners.is_empty() {
                // Co-change empty message includes the target path in both forms;
                // inlined here because the target variable is only in scope here.
                if page.offset() > 0 {
                    writeln!(
                        w,
                        "No co-change results for {target:?} at offset {} (try a smaller --offset).",
                        page.offset()
                    )?;
                } else {
                    writeln!(w, "No co-change data for {target:?}.")?;
                }
                return Ok(());
            }
            let range =
                page_range_label(&format!("{} files", partners.len()), partners.len(), page);
            writeln!(w, "Co-change partners of {} ({range}):\n", target)?;
            writeln!(w, "  Jaccard  Count  Path")?;
            writeln!(w, "  ───────  ─────  ────────────────────────────────")?;
            for p in partners {
                let partner = cochange_partner(p, target);
                writeln!(w, "  {:.3}    {:>5}  {}", p.jaccard, p.count, partner)?;
            }
        }
    }
    Ok(())
}

// ============================================================================
// JSON serialization types
// ============================================================================

/// A single hotspot/coldspot entry in standalone JSON output.
#[derive(Serialize)]
struct HotspotJsonRow<'a> {
    path: &'a str,
    hotspot_score: f64,
    changes_30d: u32,
    changes_90d: u32,
}

/// A single risk entry in standalone JSON output.
#[derive(Serialize)]
struct RiskJsonRow<'a> {
    path: &'a str,
    risk_score: f64,
    fix_density: f64,
    fix_commits: u32,
    total_commits: u32,
}

/// A single co-change partner entry in standalone JSON output.
#[derive(Serialize)]
struct CochangeJsonRow<'a> {
    path: &'a str,
    jaccard: f64,
    count: u32,
}

/// Top-level envelope for hotspot/coldspot standalone JSON.
///
/// `has_more` is absent when false (additive, back-compat; AD-404-11).
#[derive(Serialize)]
struct HotColdJson<'a> {
    mode: &'a str,
    total: usize,
    /// Sound pagination terminator; absent when false (AD-404-11 / D-5).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    has_more: bool,
    results: Vec<HotspotJsonRow<'a>>,
}

/// Top-level envelope for risk standalone JSON.
///
/// `has_more` is absent when false (additive, back-compat; AD-404-11).
#[derive(Serialize)]
struct RiskyJson<'a> {
    mode: &'a str,
    total: usize,
    /// Sound pagination terminator; absent when false (AD-404-11 / D-5).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    has_more: bool,
    results: Vec<RiskJsonRow<'a>>,
}

/// Top-level envelope for blast-radius standalone JSON.
///
/// `has_more` is absent when false (additive, back-compat; AD-404-11).
#[derive(Serialize)]
struct BlastRadiusJson<'a> {
    mode: &'a str,
    target: &'a str,
    total: usize,
    /// Sound pagination terminator; absent when false (AD-404-11 / D-5).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    has_more: bool,
    results: Vec<CochangeJsonRow<'a>>,
}

/// Serialize a hotspot/coldspot row slice to JSON and write it.
fn write_hotcold_json(
    mode: &str,
    rows: &[HotspotRow],
    has_more: bool,
    w: &mut impl Write,
) -> anyhow::Result<()> {
    let envelope = HotColdJson {
        mode,
        total: rows.len(),
        has_more,
        results: rows
            .iter()
            .map(|r| HotspotJsonRow {
                path: &r.file_path,
                hotspot_score: r.score,
                changes_30d: r.changes_30d,
                changes_90d: r.changes_90d,
            })
            .collect(),
    };
    writeln!(w, "{}", serde_json::to_string_pretty(&envelope)?)?;
    Ok(())
}

/// Format a standalone temporal query result as JSON (AD-404-11).
///
/// `has_more`: sound pagination terminator — true when more results exist
/// beyond the current page. Absent from JSON when false (additive, back-compat).
///
/// Uses `#[derive(Serialize)]` typed structs so field names are defined in one
/// place, preventing the hand-built `serde_json::json!()` approach from drifting
/// independently.
pub(super) fn format_temporal_json(
    output: &TemporalQueryOutput,
    has_more: bool,
    w: &mut impl Write,
) -> anyhow::Result<()> {
    match output {
        TemporalQueryOutput::Hotspots(rows) => write_hotcold_json("hot", rows, has_more, w)?,
        TemporalQueryOutput::Coldspots(rows) => write_hotcold_json("cold", rows, has_more, w)?,
        TemporalQueryOutput::Risks(rows) => {
            let envelope = RiskyJson {
                mode: "risky",
                total: rows.len(),
                has_more,
                results: rows
                    .iter()
                    .map(|r| RiskJsonRow {
                        path: &r.file_path,
                        risk_score: r.risk_score,
                        fix_density: r.fix_density,
                        fix_commits: r.fix_commits,
                        total_commits: r.total_commits,
                    })
                    .collect(),
            };
            writeln!(w, "{}", serde_json::to_string_pretty(&envelope)?)?;
        }
        TemporalQueryOutput::Cochanges { target, partners } => {
            let envelope = BlastRadiusJson {
                mode: "blast-radius",
                target,
                total: partners.len(),
                has_more,
                results: partners
                    .iter()
                    .map(|p| CochangeJsonRow {
                        path: cochange_partner(p, target),
                        jaccard: p.jaccard,
                        count: p.count,
                    })
                    .collect(),
            };
            writeln!(w, "{}", serde_json::to_string_pretty(&envelope)?)?;
        }
    }
    Ok(())
}

// ============================================================================
// Combined text+temporal enrichment (Step 10)
// ============================================================================

/// Compare two hotspot/coldspot scores for a Hot-or-Cold sort.
///
/// - `Hot` → descending (higher score first).
/// - `Cold` → ascending (lower score first).
///
/// Extracted to eliminate the byte-identical comparator body that previously
/// appeared in both `apply_temporal_enrichment` (text+temporal path) and
/// `enrich_ast_results` (standalone AST path).  Path-ASC tiebreak is applied
/// by the caller via `.then_with(|| a.path.cmp(&b.path))`.
#[inline]
fn hotcold_score_cmp(score_a: f64, score_b: f64, sort: TemporalSort) -> std::cmp::Ordering {
    if sort == TemporalSort::Hot {
        score_b
            .partial_cmp(&score_a)
            .unwrap_or(std::cmp::Ordering::Equal)
    } else {
        score_a
            .partial_cmp(&score_b)
            .unwrap_or(std::cmp::Ordering::Equal)
    }
}

/// AD-414-13: pure post-annotate counter for the zero-coverage skip.
///
/// Called between the annotate pass and the `sort_by` inside
/// [`apply_temporal_enrichment`].  Returns `TemporalCoverage` with `lookup_errors`
/// set to 0; callers patch in the actual error count from the annotate return value.
///
/// The predicate reads whichever score field the requested dimension uses:
/// - `Hot` / `Cold` → `hotspot_score`
/// - `Risky` → `risk_score`
///
/// A file is "ranked" when its score is `Some(_)` (annotated by the DB lookup).
/// Files that were absent from the DB keep `None` (sentinel `-1.0` in the sort
/// comparator) and do NOT count as ranked.
///
/// Unit-testable without a DB: supply pre-annotated results and a sort direction.
///
/// Only called from tests (the production path uses the inline counter inside
/// [`enrich_temporal_generic`] to avoid a second pass).
#[cfg(test)]
pub(super) fn ranked_row_count(results: &[ResolvedResult], sort: TemporalSort) -> TemporalCoverage {
    let total = results.len();
    let ranked = results
        .iter()
        .filter(|r| match sort {
            TemporalSort::Hot | TemporalSort::Cold => {
                r.temporal.as_ref().and_then(|t| t.hotspot_score).is_some()
            }
            TemporalSort::Risky => r.temporal.as_ref().and_then(|t| t.risk_score).is_some(),
        })
        .count();
    TemporalCoverage {
        ranked,
        total,
        lookup_errors: 0,
    }
}

// ============================================================================
// TemporalTarget trait — generic enrichment over ResolvedResult and AstResult
// ============================================================================

/// Common accessor/mutator interface for types that can receive temporal enrichment.
///
/// Implemented by [`ResolvedResult`] (lexical path) and [`rskim_search::AstResult`]
/// (AST path), enabling one generic implementation of the annotate loop and sort —
/// eliminating the four near-identical `annotate_*` functions and the duplicated
/// score-closure + `sort_by` pair (Finding C / AD-414).
///
/// The trait is private to this module; public callers use the typed wrappers
/// [`apply_temporal_enrichment`] and [`enrich_ast_results`].
trait TemporalTarget {
    fn path(&self) -> &str;
    fn hotspot_score(&self) -> Option<f64>;
    fn risk_score(&self) -> Option<f64>;
    fn set_hotspot(&mut self, score: f64, changes_30d: u32, changes_90d: u32);
    fn set_risk(&mut self, risk_score: f64, fix_density: f64);
}

impl TemporalTarget for ResolvedResult {
    fn path(&self) -> &str {
        &self.path
    }
    fn hotspot_score(&self) -> Option<f64> {
        self.temporal.as_ref().and_then(|t| t.hotspot_score)
    }
    fn risk_score(&self) -> Option<f64> {
        self.temporal.as_ref().and_then(|t| t.risk_score)
    }
    fn set_hotspot(&mut self, score: f64, changes_30d: u32, changes_90d: u32) {
        self.temporal = Some(TemporalAnnotation {
            hotspot_score: Some(score),
            changes_30d: Some(changes_30d),
            changes_90d: Some(changes_90d),
            ..Default::default()
        });
    }
    fn set_risk(&mut self, risk_score: f64, fix_density: f64) {
        self.temporal = Some(TemporalAnnotation {
            risk_score: Some(risk_score),
            fix_density: Some(fix_density),
            ..Default::default()
        });
    }
}

impl TemporalTarget for rskim_search::AstResult {
    fn path(&self) -> &str {
        &self.path
    }
    fn hotspot_score(&self) -> Option<f64> {
        self.temporal.as_ref().and_then(|t| t.hotspot_score)
    }
    fn risk_score(&self) -> Option<f64> {
        self.temporal.as_ref().and_then(|t| t.risk_score)
    }
    fn set_hotspot(&mut self, score: f64, changes_30d: u32, changes_90d: u32) {
        self.temporal = Some(rskim_search::TemporalAnnotation {
            hotspot_score: Some(score),
            changes_30d: Some(changes_30d),
            changes_90d: Some(changes_90d),
            ..Default::default()
        });
    }
    fn set_risk(&mut self, risk_score: f64, fix_density: f64) {
        self.temporal = Some(rskim_search::TemporalAnnotation {
            risk_score: Some(risk_score),
            fix_density: Some(fix_density),
            ..Default::default()
        });
    }
}

/// Generic annotate pass: one DB query per result (O(N)).
///
/// On lookup failure emits a warning and leaves the row unannotated.
/// Returns the number of per-file lookup failures (E-16).
/// The default `--limit` of 20 keeps the O(N) cost negligible; at
/// `--limit 1000` this is 1000 queries — acceptable for an interactive CLI.
fn annotate_hotspots_generic<T: TemporalTarget>(results: &mut [T], db: &TemporalDb) -> usize {
    let mut lookup_errors: usize = 0;
    for result in results.iter_mut() {
        match db.hotspot_for_file(result.path()) {
            Ok(Some(row)) => result.set_hotspot(row.score, row.changes_30d, row.changes_90d),
            Ok(None) => {} // File not in temporal DB — leave unannotated.
            Err(e) => {
                eprintln!("skim search: temporal enrichment warning: {e}");
                lookup_errors += 1;
            }
        }
    }
    lookup_errors
}

/// Generic annotate pass for risk scores; see [`annotate_hotspots_generic`].
fn annotate_risks_generic<T: TemporalTarget>(results: &mut [T], db: &TemporalDb) -> usize {
    let mut lookup_errors: usize = 0;
    for result in results.iter_mut() {
        match db.risk_for_file(result.path()) {
            Ok(Some(row)) => result.set_risk(row.risk_score, row.fix_density),
            Ok(None) => {} // File not in temporal DB — leave unannotated.
            Err(e) => {
                eprintln!("skim search: temporal enrichment warning: {e}");
                lookup_errors += 1;
            }
        }
    }
    lookup_errors
}

/// Sort `results` by the requested temporal dimension (AD-414-13 contract).
///
/// Tiebreak: score DESC (Hot) / ASC (Cold) / DESC (Risky), then `path` ASC —
/// unified total order matching SQL (resolution 8).
fn sort_by_temporal<T: TemporalTarget>(results: &mut [T], sort: TemporalSort) {
    match sort {
        TemporalSort::Hot | TemporalSort::Cold => {
            results.sort_by(|a, b| {
                hotcold_score_cmp(
                    a.hotspot_score().unwrap_or(-1.0),
                    b.hotspot_score().unwrap_or(-1.0),
                    sort,
                )
                .then_with(|| a.path().cmp(b.path()))
            });
        }
        TemporalSort::Risky => {
            results.sort_by(|a, b| {
                b.risk_score()
                    .unwrap_or(-1.0)
                    .partial_cmp(&a.risk_score().unwrap_or(-1.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.path().cmp(b.path()))
            });
        }
    }
}

/// Generic annotate + sort pass shared by both query paths.
///
/// - Annotates `results` with hotspot or risk data from `db`.
/// - AD-414-13: skips the `sort_by` when `ranked == 0` — preserves upstream order.
/// - Returns `TemporalCoverage { ranked, total, lookup_errors }`.
fn enrich_temporal_generic<T: TemporalTarget>(
    results: &mut [T],
    sort: TemporalSort,
    db: &TemporalDb,
) -> TemporalCoverage {
    let lookup_errors = match sort {
        TemporalSort::Hot | TemporalSort::Cold => annotate_hotspots_generic(results, db),
        TemporalSort::Risky => annotate_risks_generic(results, db),
    };
    let ranked = results
        .iter()
        .filter(|r| match sort {
            TemporalSort::Hot | TemporalSort::Cold => r.hotspot_score().is_some(),
            TemporalSort::Risky => r.risk_score().is_some(),
        })
        .count();
    // AD-414-13: skip the re-sort when zero matched files carry a row for the
    // requested dimension — degenerate sort onto the -1.0 sentinel carries no
    // information; leave results in upstream order instead.
    if ranked > 0 {
        sort_by_temporal(results, sort);
    }
    TemporalCoverage {
        ranked,
        total: results.len(),
        lookup_errors,
    }
}

// ============================================================================
// Combined text+temporal enrichment (Step 10)
// ============================================================================

/// Annotate and re-sort text search results with temporal data.
///
/// - For `Hot`: annotate with hotspot scores, sort descending. Files absent
///   from temporal DB sort last (by path for determinism).
/// - For `Cold`: annotate with hotspot scores, sort ascending. Files absent
///   sort first (score `-1.0` sentinel).
/// - For `Risky`: annotate with risk scores, sort descending. Files absent
///   sort last.
///
/// Uses per-file lookups (`hotspot_for_file` / `risk_for_file`) to avoid
/// bulk table loads when annotating a small result set.
///
/// Graceful degradation: if a per-file DB query fails, the result is left
/// unannotated and a warning is emitted; other results are still annotated.
///
/// **AD-414-13 zero-coverage skip**: when `ranked == 0` after annotation, the
/// `sort_by` is **not** applied — the upstream lexical order is preserved rather
/// than re-sorting every result onto the `-1.0` sentinel and emitting path-ASC.
/// Partial coverage (`ranked >= 1`) runs the sort unchanged.
///
/// Returns `TemporalCoverage { ranked, total, lookup_errors }` so callers can
/// detect zero-coverage and emit the `NoRankedRows` degraded notice.
pub(super) fn apply_temporal_enrichment(
    results: &mut [ResolvedResult],
    sort: TemporalSort,
    db: &TemporalDb,
) -> anyhow::Result<TemporalCoverage> {
    Ok(enrich_temporal_generic(results, sort, db))
}

// ============================================================================
// Standalone-AST temporal enrichment (full-CLI integration)
// ============================================================================

/// Annotate and re-sort standalone `--ast` results with temporal data.
///
/// The AST analogue of [`apply_temporal_enrichment`]: applies the **identical**
/// ordering contract (via [`enrich_temporal_generic`]) — absent files sort last
/// (score sentinel `-1.0`) and equal temporal scores tie-break by `path.cmp` —
/// so the two query paths expose one observable sort behaviour (design decision 4
/// / AC-A2).  AD-414-13 zero-coverage skip applies: `ranked == 0` → sort skipped.
///
/// Callers MUST pre-truncate `results` to the bounded re-sort window
/// ([`resort_window`]) before calling so per-file DB lookups stay bounded (AC-P1).
/// Returns `TemporalCoverage { ranked, total, lookup_errors }`.
pub(super) fn enrich_ast_results(
    results: &mut [rskim_search::AstResult],
    sort: TemporalSort,
    db: &TemporalDb,
) -> TemporalCoverage {
    enrich_temporal_generic(results, sort, db)
}

// ============================================================================
// Tests (co-located)
// ============================================================================

#[cfg(test)]
#[path = "temporal_tests.rs"]
mod tests;
