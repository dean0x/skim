//! Multi-layer result intersection and composite re-ranking.
//!
//! # Algorithm (Wave 4a, #198)
//!
//! 1. **Intersect** lexical and AST results by `FileId` via HashMap-based join.
//!    The lexical layer is indexed into a `HashMap<FileId, rank>` (O(n)); the AST
//!    layer (arriving FileId-ASC by contract) is iterated once with O(1) lookup
//!    per entry, for O(n+m) overall.
//! 2. **Fuse** via weighted Reciprocal Rank Fusion (RRF):
//!    `score(d) = Σᵢ wᵢ / (RRF_K + rankᵢ(d))`
//!    where `rankᵢ(d)` is d's 1-based position in layer i's DESC-sorted list
//!    and a layer in which d is absent contributes 0 (graceful absence).
//! 3. **Sort** DESC by fused score, then FileId-ASC as a deterministic tiebreaker.
//!
//! # Design invariants (cross-ticket, shared with #200)
//!
//! * `RRF_K ≈ 60` — the IR-standard damping constant (Cormack et al., SIGIR 2009).
//! * Equal-weight default — both layers contribute identically.
//! * Scale-free — only rank, never raw score magnitude, drives fusion.
//! * NaN-safe — denominator `RRF_K + rank` is always strictly positive.
//! * Deterministic tiebreaker — FileId-ASC for equal composite scores.
//! * Pure / I/O-free — all lookups are injected via `impl Fn` closures; no
//!   reader, DB, or filesystem access inside this module.
//!
//! # Structural refinement status (Wave 4a)
//!
//! The `structural_lookup` closure and `avg_max_depth` parameter are the
//! extension seam for depth-based AST re-ranking, planned in AC2/AC12 and
//! deferred to #290.  **On the production CLI path in Wave 4a, the caller
//! (`run_compound_query`) always passes `|_| None` and `avg_max_depth = 0.0`**,
//! so every entry's `depth_key` evaluates to `0.0 / 1.0 = 0.0`.  The AST
//! decorate-sort therefore reduces to pure `ast_score`-DESC order — depth
//! re-ranking is not live in production.  The seam is exercised only by
//! unit tests that inject real closures; those tests validate the logic in
//! isolation against the #290 implementation milestone.
//!
//! Shipped composite ranking in Wave 4a: **lexical-rank + AST-score-rank RRF only**.
//!
//! # AC7 parameter note
//!
//! The original plan AC7 specified an additional `impl Fn(FileId) -> Option<f64>`
//! temporal multiplier parameter.  This parameter was dropped in the Cross-Plan
//! Amendment because the `#202` gate blocks the combined `--ast + temporal` path
//! entirely in Wave 4a, making a temporal multiplier unreachable and therefore
//! baseless per ADR-003.  The omission is intentional and traceable to the amendment;
//! #202 lifting it is the prerequisite for adding it.
//!
//! # Lexical candidate pool and completeness
//!
//! The production caller fetches a **wider** lexical pool of `limit * 4` candidates
//! (no lexical `file_filter` on the text+AST path) and intersects with the full AST
//! set.  A file that is in both AST and lexical sets but ranks beyond position
//! `limit * 4` in the unfiltered lexical ranking will not appear in the output.
//! The K=4 multiplier is a heuristic with no measured corpus basis; it is documented
//! here for the #290 follow-up to calibrate.  This is an intentional trade-off: the
//! old `file_filter` gate guaranteed completeness within the AST set but precluded
//! composite ranking; the new wider-pool approach enables composite ranking at the
//! cost of a bounded completeness gap.
//!
//! # Extension points for #200
//!
//! `CompositeWeights` holds the per-layer weights so #200 can add temporal and
//! graph signal ranked lists into the same RRF without changing the fusion
//! kernel.  The equal-weight two-layer default defined here is the seed that
//! #200 extends additively.
//!
//! # References
//!
//! G. V. Cormack, C. L. A. Clarke, and S. Buettcher. Reciprocal rank fusion
//! outperforms condorcet and individual rank learning methods. *Proc. SIGIR
//! 2009*, pp. 758–759.  `RRF_K = 60` is the constant from that paper.

use std::collections::HashMap;

use crate::ast_index::StructuralMetrics;
use crate::types::{FileId, Result, SearchError, SearchResult};

// ============================================================================
// Constants
// ============================================================================

/// RRF damping constant.
///
/// `k ≈ 60` is the value from the original Cormack et al. SIGIR 2009 paper.
/// It prevents high-ranked results from dominating by capping the maximum
/// contribution of rank-1 to `w / (60 + 1)`.  Do NOT change this without
/// a measured lift on a real corpus.
pub const RRF_K: f64 = 60.0;

/// Weight for the lexical (BM25F) layer in the two-signal RRF blend.
///
/// Equal-weight default; #200 extends `CompositeWeights` with additional
/// signals.  Must be non-negative.
pub const WEIGHT_LEXICAL: f64 = 1.0;

/// Weight for the AST structural layer in the two-signal RRF blend.
///
/// Equal-weight default; see [`WEIGHT_LEXICAL`].
pub const WEIGHT_AST: f64 = 1.0;

// ============================================================================
// Weight container (extensible by #200)
// ============================================================================

/// Per-signal fusion weights for weighted RRF.
///
/// #198 defines the two-signal (lexical + AST) blend.  #200 extends this
/// struct additively to N signals — temporal, import-graph, dir-proximity, and
/// structural-coupling are added here as the canonical extension, not a separate
/// struct.  The equal-weight default for the #198 two-signal path uses
/// `WEIGHT_LEXICAL = 1.0` / `WEIGHT_AST = 1.0`; the six-signal profile (from
/// `WEIGHT6_*`) is captured in the `with_six_signal_defaults()` constructor.
///
/// # Extension by #200
///
/// The four new fields (`temporal`, `import_graph`, `dir_proximity`,
/// `structural_coupling`) default to `0.0` per ADR-003 — each will be promoted
/// to a non-zero value after a measured relative-lift benchmark confirms positive
/// marginal lift on the same corpus in the same run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompositeWeights {
    /// Weight for the lexical (BM25F) ranked list.
    pub lexical: f64,
    /// Weight for the AST structural ranked list.
    pub ast: f64,
    /// Weight for the temporal co-change Jaccard ranked list (default 0.0 for
    /// the two-signal #198 path; 0.2 in the six-signal #200 profile).
    pub temporal: f64,
    /// Weight for the import-graph signal (default 0.0 — ADR-003 gated).
    pub import_graph: f64,
    /// Weight for the directory-proximity signal (default 0.0 — ADR-003 gated).
    pub dir_proximity: f64,
    /// Weight for the structural-coupling signal (default 0.0 — ADR-003 gated).
    pub structural_coupling: f64,
}

impl Default for CompositeWeights {
    /// Six-signal (#200) default profile.
    ///
    /// Returns `lexical = 0.5`, `ast = 0.3`, `temporal = 0.2`, all extended
    /// signals `0.0`.  This is the canonical #200 starting profile as specified
    /// in AC1.
    ///
    /// The #198 two-signal equal-weight profile (`lexical = 1.0`, `ast = 1.0`)
    /// is available via the `WEIGHT_LEXICAL` / `WEIGHT_AST` constants for
    /// callers that need the legacy two-signal path explicitly.
    fn default() -> Self {
        Self::with_six_signal_defaults()
    }
}

impl CompositeWeights {
    /// Six-signal (#200) default profile.
    ///
    /// Returns the canonical #200 starting weights:
    /// `lexical = 0.5`, `ast = 0.3`, `temporal = 0.2`, extended `0.0`.
    /// Extended signals will be promoted from `0.0` after measured relative-lift
    /// benchmarks confirm positive marginal lift (ADR-003).
    ///
    /// These literal values mirror `WEIGHT6_*` constants in `compound::weights`.
    /// They are inlined here to avoid a circular dependency
    /// (`intersection` → `weights` → `intersection`).
    #[must_use]
    pub fn with_six_signal_defaults() -> Self {
        Self {
            lexical: 0.5,
            ast: 0.3,
            temporal: 0.2,
            import_graph: 0.0,
            dir_proximity: 0.0,
            structural_coupling: 0.0,
        }
    }

    /// Validate that all weights are finite and non-negative.
    ///
    /// Returns `Ok(())` when all six weights satisfy:
    /// - Not NaN (`w.is_nan()` is false)
    /// - Not infinite (`w.is_infinite()` is false)
    /// - Non-negative (`w >= 0.0`)
    ///
    /// Returns `Err(SearchError::InvalidQuery(...))` for the first invalid
    /// weight encountered.  This function never panics (engineering rule:
    /// Result, never throw in business logic).
    ///
    /// # Example
    ///
    /// ```
    /// # use rskim_search::compound::CompositeWeights;
    /// assert!(CompositeWeights::with_six_signal_defaults().validate().is_ok());
    ///
    /// let bad = CompositeWeights { lexical: -0.5, ..Default::default() };
    /// assert!(bad.validate().is_err());
    /// ```
    pub fn validate(&self) -> Result<()> {
        let fields = [
            ("lexical", self.lexical),
            ("ast", self.ast),
            ("temporal", self.temporal),
            ("import_graph", self.import_graph),
            ("dir_proximity", self.dir_proximity),
            ("structural_coupling", self.structural_coupling),
        ];
        for (name, w) in fields {
            if w.is_nan() {
                return Err(SearchError::InvalidQuery(format!(
                    "weight '{name}' is NaN — all weights must be finite and non-negative"
                )));
            }
            if w.is_infinite() {
                return Err(SearchError::InvalidQuery(format!(
                    "weight '{name}' is infinite — all weights must be finite and non-negative"
                )));
            }
            if w < 0.0 {
                return Err(SearchError::InvalidQuery(format!(
                    "weight '{name}' is negative ({w}) — all weights must be >= 0.0"
                )));
            }
        }
        Ok(())
    }

    /// Parse a comma-separated weights string `"l,a,t"` into a `CompositeWeights`.
    ///
    /// Accepts exactly 3 values: lexical, ast, temporal.  Extended-signal weights
    /// (import_graph, dir_proximity, structural_coupling) remain at their defaults
    /// (all 0.0) — they are not user-configurable until benchmark lift is measured
    /// (applies ADR-003).
    ///
    /// Returns `Err` when the string does not contain exactly 3 comma-separated
    /// values, or any value fails to parse as a finite non-negative f64.
    ///
    /// # Example
    ///
    /// ```
    /// # use rskim_search::compound::CompositeWeights;
    /// let w = CompositeWeights::parse_weights_flag("0.5,0.3,0.2").unwrap();
    /// assert_eq!(w.lexical, 0.5);
    /// assert_eq!(w.ast, 0.3);
    /// assert_eq!(w.temporal, 0.2);
    /// ```
    pub fn parse_weights_flag(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.split(',').collect();
        if parts.len() != 3 {
            return Err(SearchError::InvalidQuery(format!(
                "--weights requires exactly 3 comma-separated values (lexical,ast,temporal), got: {s:?}"
            )));
        }
        let mut vals = [0.0f64; 3];
        for (i, part) in parts.iter().enumerate() {
            let v: f64 = part.trim().parse().map_err(|_| {
                SearchError::InvalidQuery(format!(
                    "--weights value {part:?} is not a valid number (field {})",
                    ["lexical", "ast", "temporal"][i]
                ))
            })?;
            vals[i] = v;
        }
        let candidate = Self {
            lexical: vals[0],
            ast: vals[1],
            temporal: vals[2],
            import_graph: 0.0,
            dir_proximity: 0.0,
            structural_coupling: 0.0,
        };
        candidate.validate()?;
        Ok(candidate)
    }
}

// ============================================================================
// Core intersection + RRF fusion
// ============================================================================

/// Intersect and composite-rank lexical and AST search results.
///
/// # Inputs
///
/// * `lexical_scored` — results from the lexical layer, **sorted DESC by
///   score** (higher = better). FileIds need not be unique or sorted by FileId.
/// * `ast_scored` — results from the AST layer, **sorted ASC by FileId**,
///   unique FileIds, all scores > 0 (the frozen Wave-4 contract from #287).
/// * `structural_lookup` — pure closure: `FileId → Option<StructuralMetrics>`.
///   Used to refine the AST-layer rank by `max_depth` (depth-only in 4a;
///   richer metrics are available in v2 but grounded baselines for branch_count
///   etc. are deferred per ADR-003/ADR-004).  The closure must perform **no
///   I/O** — callers pre-fetch metrics before calling this function.
/// * `avg_max_depth` — corpus average max CST depth, from
///   `AstIndexReader::avg_max_depth()`.  Used as the ordering key baseline so
///   the structural refinement is grounded (avoids ADR-003 baseless magic).
///   **Wave 4a**: the production caller passes `0.0` (structural seam deferred
///   to #290); depth re-ranking is not live until #290 wires a real lookup.
/// * `weights` — per-signal RRF weights; use [`CompositeWeights::default()`]
///   for the equal-weight blend.
///
/// # Invariants enforced
///
/// * Only files present in **both** layers are returned (intersection gate).
/// * Empty intersection → returns an empty `Vec` (infallible; this function
///   never returns `Result` — see AC13).
/// * `u16` structural metrics are widened via `u32::from` / `f64::from` before
///   any arithmetic (avoids PF-004 overflow).
/// * The fused score denominator `RRF_K + rank` is always positive → NaN-safe.
/// * Equal composite scores are broken by FileId-ASC (deterministic, AC10).
/// * Stale FileId skew (FileId in AST not in lexical manifest) → silent drop,
///   consistent with `resolve_paths_and_snippets` pattern.
///
/// # Returns
///
/// `Vec<(FileId, f64)>` sorted DESC by composite score, then FileId-ASC.
/// The caller is responsible for resolving FileIds to paths and extracting
/// snippets from the lexical `SearchResult`s (AC11: carry the lexical result,
/// replace `.score` with the composite score).
///
/// # Note: structural refinement in Wave 4a
///
/// The `structural_lookup` / `avg_max_depth` seam is live only in unit tests
/// that inject real closures.  The production caller always injects `|_| None`
/// and `0.0`, so the shipped Wave 4a ranking is lexical-rank + AST-score-rank
/// RRF only.  Depth-aware ranking is wired in #290.
///
/// # Doc: limit semantics
///
/// This function receives the **full** un-pre-limited candidate sets from both
/// layers. Truncation to `--limit` happens in the caller after composite ranking
/// (rank-then-truncate-LAST invariant; see Amendment in plan).
#[must_use]
pub fn intersect_and_rank(
    lexical_scored: &[SearchResult],
    ast_scored: &[(FileId, f64)],
    structural_lookup: impl Fn(FileId) -> Option<StructuralMetrics>,
    avg_max_depth: f32,
    weights: CompositeWeights,
) -> Vec<(FileId, f64)> {
    if lexical_scored.is_empty() || ast_scored.is_empty() {
        return vec![];
    }

    // Enforce input contracts with debug assertions so callers catch violations
    // early in development without paying in release builds.
    // Contract 1: AST list must be unique FileIds.
    debug_assert!(
        {
            let mut seen = std::collections::HashSet::new();
            ast_scored.iter().all(|(fid, _)| seen.insert(*fid))
        },
        "intersect_and_rank: ast_scored must contain unique FileIds"
    );
    // Contract 2: AST list must be sorted FileId-ASC.
    debug_assert!(
        ast_scored.windows(2).all(|w| w[0].0 <= w[1].0),
        "intersect_and_rank: ast_scored must be sorted FileId-ASC"
    );
    // Contract 3: All AST scores must be > 0.
    debug_assert!(
        ast_scored.iter().all(|(_, s)| *s > 0.0),
        "intersect_and_rank: all ast_scored scores must be > 0"
    );

    // --- Step 1: build rank maps from each layer ---

    // Lexical rank map: FileId → 1-based rank position in DESC-score order.
    // The lexical layer arrives pre-sorted DESC; index + 1 = rank.
    let lexical_rank: HashMap<FileId, usize> = lexical_scored
        .iter()
        .enumerate()
        .map(|(i, r)| (r.file_id, i + 1))
        .collect();

    // AST layer with optional structural refinement.
    // The AST scored list arrives FileId-ASC; we need to build a DESC-score
    // ordering (rank 1 = most structurally complex) to feed RRF.
    //
    // Structural refinement (4a, depth-only per ADR-003/ADR-004):
    // Sort the AST results DESC by (depth_ordering_key, ast_score) where
    //   depth_ordering_key = max_depth / (1 + avg_max_depth)
    // using a grounded baseline (`avg_max_depth` from the stored v2 header).
    // Because RRF consumes rank, not magnitude, no corpus divisor is strictly
    // needed — we only need a stable relative ordering.  Using the normalised
    // depth as the primary sort key and ast_score as tiebreaker achieves this
    // without arbitrary thresholds (ADR-003).
    // PF-004: widen u16→u32→f64 BEFORE any arithmetic.
    //
    // Decorate-sort-undecorate: precompute depth key once per entry (O(m) closure
    // calls), then sort on the precomputed key.  Calling `structural_lookup`
    // inside `sort_unstable_by` would invoke it O(m log m) times — 2× per
    // comparison — which wastes work and causes quadratic behaviour for non-trivial
    // lookups once AstIndexReader is threaded in (#290).
    let avg_depth_f64 = f64::from(avg_max_depth);

    // Decorated: (depth_key, ast_score, FileId) for sort.  We keep ast_score
    // in the tuple so the tiebreaker has access to it without a second lookup.
    let mut ast_decorated: Vec<(f64, f64, FileId)> = ast_scored
        .iter()
        .map(|&(fid, score)| {
            let depth = structural_lookup(fid)
                .map(|m| f64::from(u32::from(m.max_depth)))
                .unwrap_or(0.0);
            let key = depth / (1.0 + avg_depth_f64);
            (key, score, fid)
        })
        .collect();

    // Sort DESC by (depth_key, ast_score); FileId-ASC as deterministic tiebreaker.
    // Use total_cmp throughout for NaN-safe, clippy-idiomatic fully-ordered f64 keys.
    ast_decorated.sort_unstable_by(|&(ak, as_, af), &(bk, bs, bf)| {
        bk.total_cmp(&ak)
            .then(bs.total_cmp(&as_))
            .then(af.0.cmp(&bf.0))
    });

    // AST rank map: FileId → 1-based rank (after structural refinement).
    let ast_rank: HashMap<FileId, usize> = ast_decorated
        .iter()
        .enumerate()
        .map(|(i, &(_, _, fid))| (fid, i + 1))
        .collect();

    // --- Step 2: HashMap-based intersection ---
    //
    // `ast_scored` arrives FileId-ASC (frozen contract). `lexical_rank` is a
    // HashMap so each AST entry gets an O(1) membership test; the loop is O(m).
    // Total complexity including Step 1 (O(n) map build) is O(n+m).

    let mut candidates: Vec<(FileId, f64)> = ast_scored
        .iter()
        .filter_map(|&(fid, _)| {
            // Only files in BOTH layers (intersection gate).
            let rank_lex = *lexical_rank.get(&fid)?;
            let rank_ast = *ast_rank.get(&fid)?;
            // Weighted RRF: Σᵢ wᵢ / (RRF_K + rankᵢ)
            // Denominator is always positive (RRF_K = 60, rank ≥ 1) → NaN-safe.
            let score = weights.lexical / (RRF_K + rank_lex as f64)
                + weights.ast / (RRF_K + rank_ast as f64);
            Some((fid, score))
        })
        .collect();

    // --- Step 3: sort DESC by composite score, FileId-ASC tiebreaker (AC10) ---
    candidates.sort_unstable_by(|&(a_fid, a_score), &(b_fid, b_score)| {
        b_score.total_cmp(&a_score).then(a_fid.0.cmp(&b_fid.0))
    });

    candidates
}

// ============================================================================
// Snippet-preserving result recomposition (AC11)
// ============================================================================

/// Recompose lexical `SearchResult`s with composite RRF scores.
///
/// For files in the intersection, carries the lexical `SearchResult` (with its
/// snippet and line_range) and replaces `.score` with the composite RRF score.
/// This preserves snippet + line-number data from the lexical layer (AC11).
///
/// Results are returned in the order defined by `ranked` (DESC by composite
/// score, FileId-ASC tiebreaker from [`intersect_and_rank`]).
///
/// Stale FileId skew (FileId in `ranked` not found in `lexical_scored`) →
/// silent drop, consistent with `resolve_paths_and_snippets`.
///
/// # Performance note
///
/// This function builds a second `HashMap<FileId, &SearchResult>` over
/// `lexical_scored`.  [`intersect_and_rank`] already builds a
/// `HashMap<FileId, rank>` over the same slice.  Across the public API boundary
/// the two builds are necessary (each function is independently callable).
///
/// **Caller contract** — the production path in `run_compound_query` intentionally
/// passes the **full untruncated** `ranked` slice (up to `limit × CANDIDATE_POOL_K`
/// entries) rather than a pre-truncated `limit`-element slice.  This is required by
/// the AD-355-2 verify-then-truncate-LAST invariant: pre-truncating before
/// verification could silently discard the real definer if it lands below the
/// `limit`-th rank slot but above the `limit × K`-th slot.  The accepted cost is
/// up to K×limit `SearchResult` clones rather than `limit` (bounded, one-time).
/// A caller that verifies candidates itself (or does not need the verify-last
/// guarantee) may pre-truncate for cheaper clone work.
/// Consolidating the two maps into a shared data structure is tracked in #290.
#[must_use]
pub fn recompose_with_lexical(
    ranked: &[(FileId, f64)],
    lexical_scored: &[SearchResult],
) -> Vec<SearchResult> {
    // Build a map FileId → &SearchResult for O(1) lookup.
    let lex_map: HashMap<FileId, &SearchResult> =
        lexical_scored.iter().map(|r| (r.file_id, r)).collect();

    ranked
        .iter()
        .filter_map(|&(fid, composite_score)| {
            let lex = lex_map.get(&fid)?;
            let mut result = (*lex).clone();
            result.score = composite_score;
            Some(result)
        })
        .collect()
}

// ============================================================================
// Tests (co-located in intersection_tests.rs, per repo convention)
// ============================================================================

#[cfg(test)]
#[path = "intersection_tests.rs"]
mod tests;
