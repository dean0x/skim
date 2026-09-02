//! Temporal query helpers for `skim search` temporal flags.
//!
//! # Responsibilities
//!
//! - Path normalization for `--blast-radius` (cross-platform, repo-relative).
//! - `TemporalDb` open/check helpers.
//! - Standalone temporal dispatch (`--hot`, `--cold`, `--risky`, `--blast-radius`).
//! - Combined text+temporal enrichment (`apply_temporal_enrichment`).
//! - Output formatting for standalone temporal queries.

use std::collections::HashSet;
use std::io::Write;
use std::path::Path;

use rskim_search::{FileId, HotspotRow, RiskRow, SearchError, TemporalDb};
use serde::Serialize;

use super::staleness::{AnchorState, HeadState};
use super::types::{Page, ResolvedResult, TemporalAnnotation, TemporalSort};

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

// ============================================================================
// Degraded-state vocabulary (AD-414-15, AD-414-1)
// ============================================================================

/// AD-414-15: classification order is NORMATIVE:
/// not_git_repo → head_unresolved → repository_mismatch → missing
/// → corrupt/unsupported_version/unreadable → empty → no_ranked_rows.
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
        }
    }

    /// Cause substring for the degraded-state notice and `DegradedJson`.
    ///
    /// `detail` carries reason-specific context set by [`open_temporal_state`]
    /// or the zero-coverage path (e.g. path pair for `RepositoryMismatch`,
    /// formatted count for `NoRankedRows`, error text for `Unreadable`).
    ///
    /// For `Empty`, `detail == "shallow"` signals that `meta.is_shallow` is set
    /// (written by Step 10 / Phase C2); absent row → `detail` is empty → no
    /// shallow suffix.
    ///
    /// Reserved for Phase C1 `DegradedJson` wiring — not yet used at call sites.
    #[allow(dead_code)]
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
                let base = "temporal data is empty (0 rows) - this repository has no \
                            commit history skim can analyse";
                if detail.contains("shallow") {
                    format!("{base}; a shallow clone is the usual cause")
                } else {
                    base.to_string()
                }
            }
            Self::NoRankedRows => detail.to_string(),
        }
    }

    /// Actionable remediation advice for `DegradedJson.remediation`.
    ///
    /// For `Empty` the is_shallow variant ("run 'git fetch --unshallow'…") is
    /// wired by Phase C1 via `DegradedJson`; this returns the non-shallow
    /// default, which is sufficient for the Phase B2 notice path.
    pub(super) fn remediation(self) -> &'static str {
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
        }
    }

    /// Complete human-readable message (cause + embedded remediation).
    ///
    /// Used by [`degraded_notice`].  `NotGitRepo` and `HeadUnresolved` return
    /// the legacy constant verbatim so AC-19 byte-identical assertions pass.
    /// `Empty` implements the §2.3 is_shallow conditional via `detail == "shallow"`
    /// (written by Phase C2); absent → non-shallow path (AC-2).
    fn full_message(self, detail: &str) -> String {
        match self {
            Self::NotGitRepo => super::NO_TEMPORAL_DATA_MSG.to_string(),
            Self::HeadUnresolved => super::HEAD_UNRESOLVED_TEMPORAL_MSG.to_string(),
            Self::RepositoryMismatch => format!(
                "{} {}; run 'skim search --rebuild --root <this root>' to re-anchor it",
                super::SUBDIR_ROOT_TEMPORAL_MSG,
                detail,
            ),
            Self::Missing => "temporal.db is not present in the index cache; \
                 run 'skim search --update' to build it"
                .to_string(),
            Self::Corrupt => "temporal.db is corrupt (not a database); \
                 run 'skim search --rebuild' to discard and rebuild it"
                .to_string(),
            Self::UnsupportedVersion => format!(
                "temporal.db was written by a newer skim ({detail}); \
                 upgrade skim; skim will not overwrite a newer database"
            ),
            Self::Unreadable => {
                if detail.is_empty() {
                    "temporal.db could not be opened; run 'skim search --rebuild'".to_string()
                } else {
                    format!(
                        "temporal.db could not be opened ({detail}); \
                         run 'skim search --rebuild'"
                    )
                }
            }
            Self::Empty => {
                let base = "temporal data is empty (0 rows) - this repository has no \
                            commit history skim can analyse";
                if detail.contains("shallow") {
                    format!(
                        "{base}; a shallow clone is the usual cause; \
                         run 'git fetch --unshallow' (or use a full clone), \
                         then 'skim search --rebuild'"
                    )
                } else {
                    format!("{base}; run 'skim search --rebuild'")
                }
            }
            Self::NoRankedRows => format!(
                "{detail}; commit the matched files, \
                 or run 'skim search --update' after committing"
            ),
        }
    }
}

/// Why the temporal database is not available for a query.
///
/// Returned by [`open_temporal_state`] (the new single funnel).  Carries
/// `reason` (the typed classification, AD-414-15) and `detail` (reason-specific
/// context — path pair for `RepositoryMismatch`, error text for `Corrupt`, etc.).
#[derive(Debug)]
pub(super) struct TemporalUnavailable {
    pub(super) reason: DegradedReason,
    /// Reason-specific context string; may be empty for stateless variants.
    pub(super) detail: String,
}

/// Result of attempting to open the temporal database for a query.
///
/// Returned by [`open_temporal_state`]; replaces the old `open_temporal_db_for`
/// `Result<TemporalDb, TemporalUnavailable>` pair.
#[derive(Debug)]
pub(super) enum TemporalOpen {
    /// DB is open and anchored to the same repository — ready to serve.
    Open(TemporalDb),
    /// DB cannot be served; `reason` and `detail` describe why.
    Unavailable(TemporalUnavailable),
}

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
    /// The user flag that requested temporal ranking (e.g. `"--hot"`).
    /// Empty for composite arms that do not map to a single flag.
    pub requested: String,
    /// The ranking actually served (e.g. `"lexical"`, `"ast"`).
    pub applied: &'static str,
    /// Human-readable notice (identical to what was printed to stderr).
    pub message: String,
    /// Machine-readable remediation advice.
    pub remediation: &'static str,
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

/// AD-414-1: single source of truth for every degraded-state notice,
/// generalising the documented `--ast` contract (warn on stderr, keep the
/// upstream order, exit 0).  Loud when skim cannot self-fix, cause-specific,
/// and always carries a remediation.  Subsumes #413's interim
/// `mod.rs::temporal_unavailable_msg`, which this PR deletes: one selector,
/// one line, no duplicate SSOT survives the wave.
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
                // §2.3 normative table: "schema version {found}, this build supports {supported}"
                detail: format!("schema version {found}, this build supports {supported}"),
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

/// Convert a set of repo-relative path strings to the corresponding `FileId`s.
///
/// Iterates the pre-computed `sorted_paths` slice once, collecting `FileId`s for
/// every path in `allowed_paths`.  Applies PF-004 widening (`u32::try_from(idx)`)
/// — never `as u32`.  Emits a one-line stderr warning when the result set is empty
/// (the blast-radius paths are not indexed), so callers do not have to repeat the
/// check.
///
/// Accepts a `&[&str]` slice (from `manifest.sorted_paths()`) so that callers
/// which already hold the slice can pass it directly without a second allocation.
///
/// This function is the single source of truth for the path→FileId conversion
/// used by all three blast-radius call sites (ast.rs standalone, query.rs lexical
/// filter, and mod.rs resolve_blast_radius_filter).
pub(super) fn paths_to_file_ids(
    sorted_paths: &[&str],
    allowed_paths: &HashSet<String>,
) -> HashSet<FileId> {
    let mut file_ids = HashSet::new();
    for (idx, path) in sorted_paths.iter().enumerate() {
        if allowed_paths.contains(*path) {
            // PF-004: widen idx (usize) to u32 before constructing FileId.
            // The file cap (50 000) guarantees no overflow, but `try_from`
            // makes the widening explicit and safe by construction.
            if let Ok(id) = u32::try_from(idx) {
                file_ids.insert(FileId(id));
            }
        }
    }
    if file_ids.is_empty() {
        eprintln!(
            "skim search: blast-radius filter matched 0 indexed files \
             (allowed {} paths, index has {} files)",
            allowed_paths.len(),
            sorted_paths.len()
        );
    }
    file_ids
}

/// Resolve a `--blast-radius` raw path to the set of co-change partner paths.
///
/// Shared core for both `resolve_blast_radius_file_ids` (standalone AST path) and
/// `resolve_blast_radius_filter` (text-query path in `mod.rs`).  Returns the set of
/// repo-relative path strings that the blast-radius filter should allow, including
/// the target file itself, plus an optional [`TemporalUnavailable`] when blast-radius
/// could not be applied so the caller can push a `DegradedJson` entry (AD-414-12).
///
/// `head` is the [`HeadState`] already resolved by the caller (Finding 2 fix:
/// returned by `auto_refresh_if_stale` so it need not be re-derived here).
/// It is passed to [`open_temporal_state`] to classify DB-absent cases (AD-414-15).
///
/// Returns:
/// - `Ok((None, None))` when `blast_radius` is `None` (not requested).
/// - `Ok((None, Some(u)))` when temporal data is absent/unreadable/empty.
///   The caller uses `u` to push a `DegradedJson` entry to `output.degraded`.
/// - `Ok((Some(empty_set), None))` when the DB belongs to a different repository
///   (`RepositoryMismatch`).  The empty allowlist forces zero results on all three
///   blast-radius call sites, matching the standalone arm which also serves zero rows
///   on a mismatch.  Returning `(None, _)` would overload the "not requested" sentinel
///   (PF-016 absence-overloading class, AD-413-16).
/// - `Ok((Some(paths), None))` when blast-radius resolved successfully.
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
) -> anyhow::Result<(
    Option<std::collections::HashSet<String>>,
    Option<TemporalUnavailable>,
)> {
    let Some(raw_path) = blast_radius else {
        return Ok((None, None));
    };

    // AD-414-1 / AD-414-15: open_temporal_state is the single funnel for all temporal
    // DB access.  RepositoryMismatch → wrong co-change data would be served; every other
    // Unavailable variant → degrade gracefully.  Both arms emit a reason-specific human-
    // readable string via degraded_notice (AD-414-1).
    let db = match open_temporal_state(root, cache_dir, head) {
        TemporalOpen::Open(db) => db,
        TemporalOpen::Unavailable(u) => {
            // AC-7 / AC-19(b): NotGitRepo keeps the legacy composition format
            // byte-identical to the pre-refactor message.  All other reasons
            // route through degraded_notice with flag="--blast-radius" and
            // Fallback::Lexical so the notice reads "--blast-radius not applied"
            // (T-7/AC-7 requirement) and no doubled phrase is produced.
            let msg = if u.reason == DegradedReason::NotGitRepo {
                format!(
                    "no temporal data for --blast-radius — {}",
                    super::NO_TEMPORAL_DATA_MSG
                )
            } else {
                degraded_notice(&u, "--blast-radius", Fallback::Lexical)
            };
            if json {
                let envelope = serde_json::json!({ "warning": msg });
                eprintln!("{}", serde_json::to_string(&envelope)?);
            } else {
                eprintln!("skim search: {msg}");
            }
            // AD-413-16 / PF-016: RepositoryMismatch means the DB belongs to a
            // different repository.  Returning (None, _) would overload the
            // "not requested" sentinel — callers' .map() would yield None,
            // bypassing the file filter entirely and serving the full unfiltered
            // index.  Return an empty allowlist instead so every blast-radius
            // call site (AST arm, lexical arm, standalone arm) agrees: wrong
            // repo → zero results, not all results.
            if u.reason == DegradedReason::RepositoryMismatch {
                return Ok((Some(std::collections::HashSet::new()), None));
            }
            // Return the unavailable reason so the caller can push DegradedJson.
            return Ok((None, Some(u)));
        }
    };

    // T-7/AC-7: detect an empty temporal DB (valid schema, zero hotspot rows).
    // A valid-but-empty DB reaches this arm because open_temporal_state returns
    // Open(db) — the Empty classification is a derived state checked here via
    // dimension_is_empty, matching the pattern used by the temporal-sort arm in
    // run_query and the standalone arm in run_temporal_standalone (AD-414-4).
    if dimension_is_empty(&db, TemporalSort::Hot) {
        let u = TemporalUnavailable {
            reason: DegradedReason::Empty,
            detail: String::new(),
        };
        let msg = degraded_notice(&u, "--blast-radius", Fallback::Lexical);
        eprintln!("skim search: {msg}");
        return Ok((None, Some(u)));
    }

    let normalized = normalize_blast_radius_path(raw_path, root)?;
    let partners = db.cochanges_for_file(&normalized)?;
    if partners.is_empty() {
        eprintln!("skim search: no co-change data for {raw_path:?}");
    }
    let mut allowed_paths = cochange_partner_paths(&partners, &normalized);
    // Include the target file itself so queries like `skim search auth --blast-radius src/auth.rs`
    // surface matches within the target file in addition to its co-change partners.
    allowed_paths.insert(normalized);
    Ok((Some(allowed_paths), None))
}

/// Resolve a `--blast-radius` raw path to the set of matching `FileId`s.
///
/// Unified resolver used by every blast-radius call site:
/// - `run_ast_standalone` caller in `mod.rs` (standalone `--ast --blast-radius`)
/// - `execute_query_with_manifest` blast-radius arm (query.rs, via `paths_to_file_ids`)
/// - `resolve_blast_radius_filter` (mod.rs, text + blast-radius)
///
/// Algorithm:
/// 1. If `blast_radius` is `None`, return `Ok(None)` immediately.
/// 2. Open `temporal.db` under `cache_dir`.  If absent/corrupt/empty, emit the
///    degraded notice and return `Ok(None)`.
/// 3. Normalize the raw path to repo-relative form.
/// 4. Look up co-change partners, add the target file itself.
/// 5. Convert the path set to `FileId`s via `paths_to_file_ids`.
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
    let (allowed_paths_opt, _degraded) =
        resolve_blast_radius_paths(blast_radius, root, cache_dir, json, head)?;
    let Some(allowed_paths) = allowed_paths_opt else {
        return Ok(None);
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

/// Extract the set of partner paths from a slice of co-change rows.
///
/// Uses `cochange_partner` to resolve both `file_a`/`file_b` directions. The
/// `target` file itself is NOT included — callers add it separately when needed.
pub(super) fn cochange_partner_paths(
    partners: &[rskim_search::CochangeRow],
    target: &str,
) -> std::collections::HashSet<String> {
    partners
        .iter()
        .map(|p| cochange_partner(p, target).to_string())
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
    let lookup_errors = match sort {
        TemporalSort::Hot | TemporalSort::Cold => annotate_hotspots(results, db),
        TemporalSort::Risky => annotate_risks(results, db),
    };
    let mut cov = ranked_row_count(results, sort);
    cov.lookup_errors = lookup_errors;

    // AD-414-13: skip the re-sort when zero matched files carry a row for the
    // requested dimension.  With all comparator keys at the `-1.0` sentinel the
    // sort degenerates to path-ASC and carries no information from the dimension
    // the user asked for.  Leave results in upstream lexical order instead.
    if cov.ranked > 0 {
        match sort {
            TemporalSort::Hot | TemporalSort::Cold => {
                let hotspot_score = |r: &ResolvedResult| {
                    r.temporal
                        .as_ref()
                        .and_then(|t| t.hotspot_score)
                        .unwrap_or(-1.0)
                };
                // Tiebreak: score DESC (Hot) or ASC (Cold), then file_path ASC
                // unconditionally — unified total order matching SQL (resolution 8).
                results.sort_by(|a, b| {
                    hotcold_score_cmp(hotspot_score(a), hotspot_score(b), sort)
                        .then_with(|| a.path.cmp(&b.path))
                });
            }
            TemporalSort::Risky => {
                let risk_score = |r: &ResolvedResult| {
                    r.temporal
                        .as_ref()
                        .and_then(|t| t.risk_score)
                        .unwrap_or(-1.0)
                };
                results.sort_by(|a, b| {
                    risk_score(b)
                        .partial_cmp(&risk_score(a))
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.path.cmp(&b.path))
                });
            }
        }
    }
    Ok(cov)
}

/// Annotate results with hotspot data using per-file lookups.
///
/// Performs one DB query per result (O(N)). The default `--limit` of 20 keeps
/// this negligible. At `--limit 1000` this becomes 1000 queries — acceptable
/// for an interactive CLI but not for batch workloads.
///
/// On lookup failure, emits a warning and leaves that result unannotated.
/// Returns the number of per-file lookup failures (E-16).
fn annotate_hotspots(results: &mut [ResolvedResult], db: &TemporalDb) -> usize {
    let mut lookup_errors: usize = 0;
    for result in results.iter_mut() {
        match db.hotspot_for_file(&result.path) {
            Ok(Some(row)) => {
                result.temporal = Some(TemporalAnnotation {
                    hotspot_score: Some(row.score),
                    changes_30d: Some(row.changes_30d),
                    changes_90d: Some(row.changes_90d),
                    ..Default::default()
                });
            }
            Ok(None) => {} // File not in temporal DB — leave unannotated.
            Err(e) => {
                eprintln!("skim search: temporal enrichment warning: {e}");
                lookup_errors += 1;
            }
        }
    }
    lookup_errors
}

/// Annotate results with risk data using per-file lookups.
///
/// Performs one DB query per result (O(N)). See [`annotate_hotspots`] for the
/// complexity note.
///
/// On lookup failure, emits a warning and leaves that result unannotated.
/// Returns the number of per-file lookup failures (E-16).
fn annotate_risks(results: &mut [ResolvedResult], db: &TemporalDb) -> usize {
    let mut lookup_errors: usize = 0;
    for result in results.iter_mut() {
        match db.risk_for_file(&result.path) {
            Ok(Some(row)) => {
                result.temporal = Some(TemporalAnnotation {
                    risk_score: Some(row.risk_score),
                    fix_density: Some(row.fix_density),
                    ..Default::default()
                });
            }
            Ok(None) => {} // File not in temporal DB — leave unannotated.
            Err(e) => {
                eprintln!("skim search: temporal enrichment warning: {e}");
                lookup_errors += 1;
            }
        }
    }
    lookup_errors
}

// ============================================================================
// Standalone-AST temporal enrichment (full-CLI integration)
// ============================================================================

/// Annotate and re-sort standalone `--ast` results with temporal data.
///
/// The AST analogue of [`apply_temporal_enrichment`]: it applies the **identical**
/// ordering contract — absent files sort last (score sentinel `-1.0`) and equal
/// temporal scores tie-break by `path.cmp` — so the two query paths expose one
/// observable sort behaviour (design decision 4 / AC-A2).
/// This sentinel ordering governs when `ranked >= 1`; at zero coverage both arms
/// skip the re-sort entirely, leaving the upstream order intact (AD-414-13).
///
/// It operates on [`rskim_search::AstResult`] and writes the library-side
/// [`rskim_search::TemporalAnnotation`].  The small mirror (rather than a shared
/// generic) is deliberate: the two row types carry different annotation structs,
/// and a trait abstraction would add more indirection than the duplication saves.
///
/// Callers MUST pre-truncate `results` to the bounded re-sort window
/// ([`resort_window`]) before calling so per-file DB lookups stay bounded (AC-P1).
/// Returns `TemporalCoverage { ranked, total, lookup_errors }`.
///
/// **AD-414-13 zero-coverage skip**: when `ranked == 0` after annotation the
/// `sort_by` is NOT applied — raw-AST order is preserved rather than re-sorting
/// every result onto the `-1.0` sentinel.  The caller receives a
/// `TemporalCoverage` with `ranked == 0` and may act on it (e.g. emit a
/// degraded notice); the specific caller action is determined at call sites.
pub(super) fn enrich_ast_results(
    results: &mut [rskim_search::AstResult],
    sort: TemporalSort,
    db: &TemporalDb,
) -> TemporalCoverage {
    let total = results.len();
    let lookup_errors = match sort {
        TemporalSort::Hot | TemporalSort::Cold => annotate_ast_hotspots(results, db),
        TemporalSort::Risky => annotate_ast_risks(results, db),
    };
    // Count ranked entries with the same predicate as ranked_row_count (AD-414-13).
    let ranked = results
        .iter()
        .filter(|r| match sort {
            TemporalSort::Hot | TemporalSort::Cold => {
                r.temporal.as_ref().and_then(|t| t.hotspot_score).is_some()
            }
            TemporalSort::Risky => r.temporal.as_ref().and_then(|t| t.risk_score).is_some(),
        })
        .count();
    // AD-414-13: only sort when at least one file carries a row for this dimension.
    if ranked > 0 {
        match sort {
            TemporalSort::Hot | TemporalSort::Cold => {
                let hotspot_score = |r: &rskim_search::AstResult| {
                    r.temporal
                        .as_ref()
                        .and_then(|t| t.hotspot_score)
                        .unwrap_or(-1.0)
                };
                // Tiebreak: score DESC (Hot) or ASC (Cold), then file_path ASC
                // unconditionally — unified total order matching SQL (resolution 8, AD-7).
                results.sort_by(|a, b| {
                    hotcold_score_cmp(hotspot_score(a), hotspot_score(b), sort)
                        .then_with(|| a.path.cmp(&b.path))
                });
            }
            TemporalSort::Risky => {
                let risk_score = |r: &rskim_search::AstResult| {
                    r.temporal
                        .as_ref()
                        .and_then(|t| t.risk_score)
                        .unwrap_or(-1.0)
                };
                results.sort_by(|a, b| {
                    risk_score(b)
                        .partial_cmp(&risk_score(a))
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.path.cmp(&b.path))
                });
            }
        }
    }
    TemporalCoverage {
        ranked,
        total,
        lookup_errors,
    }
}

/// Annotate `AstResult`s with hotspot data via per-file lookups (one DB query
/// per result). On lookup failure, emits a warning and leaves the row unannotated.
/// Returns the number of per-file lookup failures (E-16).
fn annotate_ast_hotspots(results: &mut [rskim_search::AstResult], db: &TemporalDb) -> usize {
    let mut lookup_errors: usize = 0;
    for result in results.iter_mut() {
        match db.hotspot_for_file(&result.path) {
            Ok(Some(row)) => {
                result.temporal = Some(rskim_search::TemporalAnnotation {
                    hotspot_score: Some(row.score),
                    changes_30d: Some(row.changes_30d),
                    changes_90d: Some(row.changes_90d),
                    ..Default::default()
                });
            }
            Ok(None) => {} // File not in temporal DB — leave unannotated.
            Err(e) => {
                eprintln!("skim search: temporal enrichment warning: {e}");
                lookup_errors += 1;
            }
        }
    }
    lookup_errors
}

/// Annotate `AstResult`s with risk data via per-file lookups (one DB query per
/// result). On lookup failure, emits a warning and leaves the row unannotated.
/// Returns the number of per-file lookup failures (E-16).
fn annotate_ast_risks(results: &mut [rskim_search::AstResult], db: &TemporalDb) -> usize {
    let mut lookup_errors: usize = 0;
    for result in results.iter_mut() {
        match db.risk_for_file(&result.path) {
            Ok(Some(row)) => {
                result.temporal = Some(rskim_search::TemporalAnnotation {
                    risk_score: Some(row.risk_score),
                    fix_density: Some(row.fix_density),
                    ..Default::default()
                });
            }
            Ok(None) => {} // File not in temporal DB — leave unannotated.
            Err(e) => {
                eprintln!("skim search: temporal enrichment warning: {e}");
                lookup_errors += 1;
            }
        }
    }
    lookup_errors
}

// ============================================================================
// Tests (co-located)
// ============================================================================

#[cfg(test)]
#[path = "temporal_tests.rs"]
mod tests;
