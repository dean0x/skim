//! Query execution — search the index and format results.
//!
//! # Data flow
//!
//! 1. Check for `index.skidx` — auto-build on cold start.
//! 2. Check staleness (git HEAD) — rebuild if stale.
//! 3. Open `NgramIndexReader`, wrap in `QueryEngine`.
//! 4. Execute the query, get `Vec<SearchResult>` with `FileId`s.
//! 5. Load `FileManifest`, map `FileId → path` via `sorted_paths()`.
//! 6. For each result, verify substring membership + extract snippet (single read,
//!    AD-355-1).
//! 7. Truncate to `--limit` LAST — after verification drops non-matching candidates
//!    (AD-355-2).
//! 8. Return `QueryOutput`.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use rskim_search::{
    CompositeWeights, FileId, IndexStats, NgramIndexReader, QueryEngine, SearchLayer, SearchQuery,
    SearchResult, StructuralMetrics, count_query_word_tokens, intersect_and_rank, is_single_token,
    merge_layer_scores, recompose_with_lexical,
};

use super::manifest::FileManifest;
use super::snippet::{SnippetOutcome, VerifyMode, extract_snippet_and_verify};
use super::staleness::auto_refresh_if_stale;
use super::types::{QueryConfig, QueryOutput, ResolvedResult};

// ============================================================================
// Candidate-pool sizing (AD-355-2, AD-356-1)
// ============================================================================
//
// The three paths (pure-lexical, compound text+AST, blast-radius) each size
// their candidate pool before the verify-then-truncate-LAST step.  All three
// pools are defined here in one place so the "how wide must the pre-verify pool
// be" decision has a single reason to change.
//
// `candidate_pool(limit, k)` returns `max(limit * k, CANDIDATE_POOL_FLOOR)` so
// every path uses the same floor policy.  Calibrating K per-path is tracked in
// #361 per ADR-003 (grounded measurements before changing).
//
// Current values:
//   LEXICAL_CANDIDATE_POOL_K = 5  (pure-lexical, with floor)
//   BLAST_CANDIDATE_POOL_K   = 10 (blast-radius composite UNION, with floor)
//
// The compound text+AST path (run_compound_query) no longer uses a K multiplier.
// AD-356-1: the lexical candidate pool is the AST-matched FileId set itself.
// See run_compound_query for the full rationale.

/// Shared floor for `candidate_pool`: every widened pool has at least this many
/// slots so small `--limit` values do not starve the verify step.
const CANDIDATE_POOL_FLOOR: usize = 100;

/// Pool multiplier K for pure-lexical and AST standalone candidate pools.
///
/// AD-374-3: promoted to `pub(super)` module level so `ast.rs` can reuse this
/// constant for the AST verify gate pool — single definition, no divergent
/// AST-local fork (see #361 and ADR-003). Value = 5; unmeasured heuristic,
/// tracked under #361 per ADR-003.
pub(super) const LEXICAL_CANDIDATE_POOL_K: usize = 5;

/// Compute the pre-verify candidate pool size for a given path K multiplier.
///
/// Returns `limit.saturating_mul(k).max(CANDIDATE_POOL_FLOOR)`.
///
/// Used by the pure-lexical, blast-radius, and AST standalone paths.
/// The compound text+AST path sizes its pool to the AST set directly (AD-356-1,
/// see run_compound_query).
///
/// AD-374-3: promoted to `pub(super)` so `ast.rs` (sibling module) can reuse the
/// same definition instead of forking a second divergent AST-local pool heuristic.
/// Single definition avoids the exact divergence #361 warns about.
#[inline]
pub(super) fn candidate_pool(limit: usize, k: usize) -> usize {
    limit.saturating_mul(k).max(CANDIDATE_POOL_FLOOR)
}

// ============================================================================
// Inert-`--weights` notice (#377, AD-377-2)
// ============================================================================

/// Notice emitted when `--weights` is supplied to a path that ignores the WHOLE
/// flag (#377, PF-006: a documented flag must never be *silently* inert).
///
/// Fires on the pure-lexical, standalone-`--ast`, temporal-only and
/// blast-radius-only paths — none of which run a weighted RRF, so no component
/// of `--weights` affects the result. The wording is deliberately unconditional
/// ("had no effect") because on these paths that is literally true.
///
/// **Single source of truth (PF-008).** Both `execute_query_with_manifest`
/// (pure-lexical) and the two standalone dispatch arms in `mod.rs` (standalone
/// `--ast`, temporal-only/blast-only) emit *this exact string* via
/// [`weights_inert_notice`], so AC7 and AC8 assert the identical substring and
/// the sites cannot silently drift. It names *both* composite paths so it stays
/// in sync with `print_help` (AC10 / PF-008 doc-drift guard).
pub(super) const WEIGHTS_FULLY_INERT_NOTICE: &str = "skim search: note: --weights only tunes ranking on the --blast-radius and \
     text+--ast composite paths; the supplied --weights had no effect on this query.";

/// Notice emitted on the compound text+`--ast` (± `--blast-radius`) path when the
/// user supplied a NON-ZERO temporal weight (#377, AD-377-2).
///
/// On this path `intersect_and_rank` fuses ONLY the lexical and ast rank terms,
/// so the lexical and ast weights *did* affect ranking — only the temporal
/// component was inert. The blocking-review fix (#377): the message must NOT
/// claim the whole flag "had no effect" here (that is factually wrong on the
/// compound path); it scopes the inert claim to the temporal component alone.
pub(super) const WEIGHTS_TEMPORAL_INERT_NOTICE: &str = "skim search: note: the temporal component of --weights had no effect — on a \
     text+--ast query only the lexical and ast weights tune ranking (the AST \
     intersection fuses no temporal signal). The lexical and ast weights were applied.";

/// Decide whether (and which) inert-`--weights` notice should fire for the chosen
/// path. Returns `Some(notice)` when the user supplied `--weights` *and* at least
/// one supplied component is inert on the path selected by
/// `(has_text, has_ast, has_blast)`; otherwise `None`.
///
/// This is the pure, side-effect-free decision seam (AD-377-2): callers turn a
/// `Some` into a guarded `eprintln!`. Keeping the policy here (not inline at the
/// `eprintln!`) lets unit tests in both `query_tests.rs` and `ast_tests.rs`
/// assert the matrix directly without capturing process stderr.
///
/// # Inert-layer matrix (AD-377-2)
///
/// | Path                                          | Honored layers         | verdict                                       |
/// |-----------------------------------------------|------------------------|-----------------------------------------------|
/// | text + `--ast` (± blast)                      | lexical, ast           | temporal-inert notice iff `temporal != 0.0`   |
/// | text + `--blast-radius` (no `--ast`)          | lexical, ast, temporal | never inert (all 3 active) → `None`           |
/// | pure-lexical / standalone-AST / temporal-only | none                   | fully-inert notice (any weights)              |
///
/// The temporal component is genuinely unused by `intersect_and_rank` (it fuses
/// only `weights.lexical` + `weights.ast`), so on every compound `--ast` path a
/// non-zero `temporal` is a no-op — hence the `temporal != 0.0` predicate rather
/// than a blanket notice (AC3/AC4a fire; AC5's `0,0,0` and AC4's `*,*,0.0` stay
/// quiet because no temporal contribution was requested).
#[must_use]
pub(super) fn weights_inert_notice(
    weights: Option<rskim_search::CompositeWeights6>,
    has_text: bool,
    has_ast: bool,
    has_blast: bool,
) -> Option<&'static str> {
    // No flag supplied → nothing to warn about (AC2 back-compat: `None` is silent).
    let weights = weights?;

    if has_text && has_ast {
        // Compound text+--ast path (with or without --blast-radius): lexical+ast
        // are honored by intersect_and_rank; temporal is inert.  Only warn when
        // the user actually asked for a temporal contribution (temporal != 0.0),
        // and scope the notice to the temporal component (blocking-review fix #2).
        return (weights.temporal != 0.0).then_some(WEIGHTS_TEMPORAL_INERT_NOTICE);
    }

    if has_text && has_blast {
        // Blast-radius composite path (no --ast): all three layers are honored by
        // run_blast_radius_composite_query → nothing is inert.
        return None;
    }

    // Everything else — pure-lexical, standalone --ast, temporal-only,
    // blast-radius-only — runs no weighted RRF, so the whole flag is inert.
    Some(WEIGHTS_FULLY_INERT_NOTICE)
}

// ============================================================================
// AST coverage notice seam (AD-405-4 / C1 / C4)
// ============================================================================

/// Common stderr prefix for the AST size-coverage notice (AD-405-4).
///
/// **Single source of truth (PF-008 / AD-405-4).**  Every emitting site
/// prints a string that starts with this prefix so unit tests can assert the
/// identical prefix without capturing process stderr.  The full message is
/// generated by [`ast_coverage_notice`], which returns `None` on a clean
/// corpus so callers do not need to gate on `is_clean()` themselves.
///
/// ## Notice cadence (D-4)
///
/// The notice fires at exactly these sites and nowhere else:
/// - Explicit builds (`--build`, `--rebuild`, `--update`) via `IndexResult`.
/// - Standalone `--ast` dispatch site (run_ast_standalone).
/// - Compound text+`--ast` dispatch site.
/// - `--stats` (embedded in the text stats block; JSON uses the key directly).
/// - The very first (NoIndex) self-heal build on a pure-lexical query.
///
/// **Silent** on: pure-lexical queries against an existing index; incremental
/// self-heals (`HeadChanged` / `WorkingTreeChanged`) — these are the common
/// case and emitting would contradict the "pure-lexical carries no AST caveat"
/// promise (AC-405-8).
pub(super) const AST_COVERAGE_PREFIX: &str = "skim search: note: ";

/// Produce the AST size-coverage notice string, or `None` on a clean corpus.
///
/// Returns a full `Option<String>` (not `&'static str`) because the message
/// embeds per-invocation counts from `coverage`.  Callers turn a `Some` into a
/// guarded `eprintln!` — they never write the notice themselves, keeping the
/// wording in one place (PF-008 / AD-405-4).
///
/// The returned string ALWAYS starts with [`AST_COVERAGE_PREFIX`]; unit tests
/// assert the prefix from the constant to avoid re-deriving the literal.
#[must_use]
pub(super) fn ast_coverage_notice(coverage: &rskim_search::AstCoverage) -> Option<String> {
    if coverage.is_clean() {
        return None;
    }

    // Build the "By language:" clause from excluded_by_lang.
    // PF-012: BTreeMap gives stable key order → deterministic notice text.
    let by_lang = coverage
        .excluded_by_lang
        .iter()
        .map(|(lang, count)| format!("{lang} {count}"))
        .collect::<Vec<_>>()
        .join(", ");

    let cap_mib = rskim_core::AST_SIZE_LIMIT_DEFAULT / (1024 * 1024);
    let n = coverage.size_excluded_files;
    let u = coverage.undetermined_files;

    let mut parts: Vec<String> = Vec::new();
    if n > 0 {
        let lang_clause = if by_lang.is_empty() {
            String::new()
        } else {
            format!(" By language: {by_lang}.")
        };
        parts.push(format!(
            "{n} file(s) exceed the structural (AST) size cap ({cap_mib} MiB) and are \
             NOT searchable with --ast.{lang_clause}"
        ));
    }
    if u > 0 {
        parts.push(format!(
            "{u} file(s) have an unknown size and could not be classified for AST indexing."
        ));
    }

    let body = parts.join(" ");
    Some(format!(
        "{AST_COVERAGE_PREFIX}{body} They remain fully text-searchable. \
         Run `skim search --stats --json` and read `ast_coverage.excluded` for the sample."
    ))
}

/// Emit the AST coverage advisory notice to stderr when coverage is non-clean.
///
/// Encapsulates the recurring `if let Some(notice) = ast_coverage_notice(cov) { eprintln!(...) }`
/// pattern shared across all five notice-cadence sites (run_build, run_update,
/// run_query compound path, run_ast_standalone, and the auto_refresh inner
/// branch of execute_query_with_manifest) so a future wording or routing change
/// touches exactly one function.  No-op on a clean corpus (delegates to
/// [`ast_coverage_notice`] which returns `None` when `is_clean()` is true).
pub(super) fn emit_ast_coverage_notice(coverage: &rskim_search::AstCoverage) {
    if let Some(notice) = ast_coverage_notice(coverage) {
        eprintln!("{notice}");
    }
}

// ============================================================================
// Verify-mode selection (AD-393-5, AD-403-1)
// ============================================================================

/// AD-403-1: Select the [`VerifyMode`] for a query from `--phrase` / `--near` flags.
///
/// Exhaustive tuple match on `(phrase, near)` — no combination falls through to
/// a winner branch.  Called on every query path (pure-lexical, compound text+AST,
/// blast-radius).  Single definition prevents the three call sites from drifting
/// apart when a new `VerifyMode` variant is added.
///
/// | phrase | near    | VerifyMode      | Semantic                              |
/// |--------|---------|-----------------|---------------------------------------|
/// | false  | None    | Substring       | trigram containment (default)         |
/// | false  | Some(n) | Near(n)         | unordered total span ≤ n             |
/// | true   | None    | Phrase          | ordered, consecutive positions        |
/// | true   | Some(n) | PhraseNear(n)   | ordered, total span ≤ n (AD-403-2)   |
pub(super) fn verify_mode_for(phrase: bool, near: Option<u32>) -> VerifyMode {
    match (phrase, near) {
        (false, None) => VerifyMode::Substring,
        (false, Some(n)) => VerifyMode::Near(n),
        (true, None) => VerifyMode::Phrase,
        (true, Some(n)) => VerifyMode::PhraseNear(n),
    }
}

// ============================================================================
// Positional-flag notices (AD-403-5, AD-403-6)
// ============================================================================

/// AD-403-5: Returns `Some(notice)` iff at least one positional flag was
/// supplied AND there is no text query (has_text = `!text.trim().is_empty()`).
///
/// A SINGLE pre-dispatch guard in `mod.rs` (above `match flags.action`) emits
/// this notice so EVERY arm — action arms (`--build`, `--rebuild`, `--stats`,
/// etc.), standalone `--ast`, standalone temporal/blast, and the bare help arm
/// — is covered.  Avoids per-arm silent drops (PF-006 class elimination).
///
/// Names only the flags actually supplied.  Notice is plain text on stderr on
/// every path including `--json` — stdout stays byte-identical; exit 0.
#[must_use]
pub(super) fn positional_inert_notice(
    phrase: bool,
    near: Option<u32>,
    has_text: bool,
) -> Option<String> {
    // Nothing to warn about when text is present — flags are honored on that path.
    if has_text {
        return None;
    }
    match (phrase, near) {
        (true, Some(n)) => Some(format!(
            "skim search: note: --phrase and --near {n} have no effect without a text query."
        )),
        (true, None) => {
            Some("skim search: note: --phrase has no effect without a text query.".to_string())
        }
        (false, Some(n)) => Some(format!(
            "skim search: note: --near {n} has no effect without a text query."
        )),
        (false, None) => None,
    }
}

/// AD-403-6: Returns `Some(notice)` when a text query is present and `--near N`
/// is structurally degenerate (extends the AD-393-9 precedent):
///
/// - Single-word query: `--near` cannot constrain anything — there is only one
///   word token so proximity to "other words" is vacuous.
/// - `N < word_count - 1`: k distinct strictly-ascending positions span at least
///   k−1 word tokens, so no assignment can satisfy the window.  Silent zero
///   results would be confusing; a stderr notice follows ADR-001 ("fail loud,
///   never silently").
///
/// Not a hard error — exit code stays 0.  `text` is the trimmed query string.
#[must_use]
pub(super) fn near_diagnostic_notice(near: Option<u32>, text: &str) -> Option<String> {
    let n = near?;
    // Use the authoritative tokenizer (count_query_word_tokens / collect_word_spans /
    // D10 / is_word_byte) so the word count matches what the positional predicates
    // see.  Punctuation acts as a separator: "foo::bar" is 2 tokens, not 1.
    let word_count = count_query_word_tokens(text);
    if word_count == 0 {
        return None; // no text to diagnose (covered by positional_inert_notice)
    }
    if word_count == 1 {
        return Some(format!(
            "skim search: note: --near {n} has no effect on a single-word query \
             (there are no other words to be near)."
        ));
    }
    // k distinct strictly-ascending word-token positions span at least k-1 ordinals.
    if (n as usize) < word_count - 1 {
        return Some(format!(
            "skim search: note: --near {n} cannot match a {word_count}-word query: \
             {word_count} distinct word tokens span at least {} positions.",
            word_count - 1,
        ));
    }
    None
}

// ============================================================================
// Query execution
// ============================================================================

/// Execute a search query against the index.
///
/// Handles auto-build on cold start and staleness refresh transparently.
/// This is the canonical interface used by `query_tests.rs` and `ast_tests.rs`.
/// Production dispatch in `mod.rs` calls [`execute_query_with_manifest`] directly
/// to thread a pre-loaded manifest and avoid a redundant refresh on the combined
/// text+`--ast` path.
///
/// # Errors
///
/// Returns `Err` on I/O failures or if the index is corrupt.
// Used by query_tests.rs and ast_tests.rs (both #[cfg(test)] callers); the
// production path in mod.rs calls execute_query_with_manifest directly.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn execute_query(
    config: &QueryConfig,
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<QueryOutput> {
    execute_query_with_manifest(config, None, analytics)
}

/// Execute a search query, optionally reusing a pre-loaded manifest.
///
/// `pre_loaded_manifest` may be `Some` when the caller has already called
/// `auto_refresh_if_stale` (e.g. the combined text+`--ast` path in `run_query`
/// refreshes before opening the AST engine and passes the resulting manifest
/// here to avoid a redundant disk load). When `None`, the function calls
/// `auto_refresh_if_stale` itself — this is the pure-lexical (no `--ast`) path.
///
/// # Errors
///
/// Returns `Err` on I/O failures or if the index is corrupt.
pub(super) fn execute_query_with_manifest(
    config: &QueryConfig,
    pre_loaded_manifest: Option<FileManifest>,
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<QueryOutput> {
    let start = Instant::now();

    // Empty query short-circuits before any I/O.
    if config.text.is_empty() {
        return Ok(QueryOutput {
            query: config.text.clone(),
            total: 0,
            has_more: false,
            verify_mode: None,
            results: vec![],
            duration_ms: start.elapsed().as_millis() as u64,
            index_stats: None,
            ast_coverage: None,
        });
    }
    // Compute the verify-mode label once for all QueryOutput sites in this function.
    // AD-403-7: absent (None) for Substring so the JSON field is skipped for default
    // callers, preserving byte-identity.
    let vm_label = verify_mode_for(config.phrase, config.near).json_label();

    // AD-377-2 / PF-006: warn (once, on stderr) when `--weights` was supplied to a
    // path that ignores some or all of it.  This entry point handles the
    // pure-lexical, text+--ast, and text+--blast-radius dispatch below; the two
    // standalone arms in mod.rs (standalone --ast, temporal-only/blast-only) emit
    // the *same* fully-inert notice via the same helper.  Always stderr — never
    // touches stdout — so JSON output stays byte-identical and parseable (AC9).
    // Guarded eprintln! off the hot path (AC12).  `has_text` is true here (the
    // empty-query short-circuit above already returned), so only the text+--ast
    // and text+--blast shapes can suppress/scope the notice.
    if let Some(notice) = weights_inert_notice(
        config.composite_weights,
        /* has_text */ true,
        config.ast_scored.is_some(),
        config.blast_radius_paths.is_some(),
    ) {
        eprintln!("{notice}");
    }

    let cache_dir = &config.cache_dir;
    let root = &config.root;

    // Ensure the index is built and current.  When the caller already refreshed
    // (combined text+--ast path), reuse the manifest they provide to avoid a
    // redundant check_staleness + FileManifest::load on an already-current index.
    // Pure-lexical path (no --ast): refreshes here exactly once.
    let manifest = match pre_loaded_manifest {
        Some(m) => m,
        None => {
            let (outcome, m) = auto_refresh_if_stale(root, cache_dir, analytics)?;
            // AC-405-7 / AC-405-8: emit AST coverage notice ONLY when a first-time
            // (NoIndex) build fires on the pure-lexical query path (D-4 cadence).
            // Incremental self-heals (HeadChanged / WorkingTreeChanged) must be
            // silent — they are the common case and emitting would contradict the
            // "pure-lexical carries no AST caveat" promise (AC-405-8).
            // `outcome.is_first_build()` distinguishes the two via RefreshOutcome.
            if outcome.is_first_build() {
                emit_ast_coverage_notice(&m.ast_coverage());
            }
            m
        }
    };

    // Open the reader.
    let reader = NgramIndexReader::open(cache_dir)?;
    let stats = reader.stats();
    let engine = QueryEngine::new(Box::new(reader));

    // Hoist sorted_paths() so it is computed once and reused for both the
    // file_filter construction and the path-resolution step below.
    let sorted = manifest.sorted_paths();

    // Build the FileId allowlist from blast-radius paths.
    // Used for blast-radius-only and blast+AST paths.
    let blast_file_ids: Option<HashSet<FileId>> = config
        .blast_radius_paths
        .as_ref()
        .map(|allowed_paths| super::temporal::paths_to_file_ids(&sorted, allowed_paths));

    // ── Compound text+AST path (#198, #356) ──────────────────────────────────
    //
    // When `ast_scored` is Some, run the compound text+AST intersection:
    //
    //   1. Restrict the lexical engine to the AST FileId set via file_filter
    //      (AD-356-1) so `raw_lex` is exactly AST ∩ lexical-present.
    //      Optionally intersect with blast-radius (if set).
    //   2. Size sq.limit to the candidate set (AD-356-2) so the reader's own
    //      .take(limit) does not truncate before intersect_and_rank sees the
    //      complete pool.
    //   3. Run intersect_and_rank: HashMap join + weighted RRF fusion.
    //   4. Recompose: carry the lexical SearchResult (snippet + line_range)
    //      with the composite RRF score replacing the raw lexical score.
    //   5. Truncate to --limit LAST (rank-then-truncate-LAST invariant).
    //
    // Structural refinement (depth-based via AstIndexReader) is not yet threaded
    // through the CLI layer — the AstIndexReader is opened in mod.rs and dropped
    // before execute_query_with_manifest is called.  Wiring it through is tracked
    // in #290 (thread AstIndexReader / pre-fetched FileId→StructuralMetrics map
    // into QueryConfig / execute_query_with_manifest to close this seam).
    // For 4a the structural lookup is a no-op; the RRF fusion of lexical+AST rank
    // alone replaces the old file_filter gate (#198).
    if let Some(ref ast_scored_vec) = config.ast_scored {
        return run_compound_query(
            config,
            ast_scored_vec,
            blast_file_ids,
            QueryContext {
                engine: &engine,
                sorted: &sorted,
                root,
                manifest: &manifest,
                stats,
                start,
            },
        );
    }
    // ── End compound text+AST path ────────────────────────────────────────────

    // ── Composite UNION blast-radius path (#200) ──────────────────────────────
    //
    // When blast_radius_paths is set AND there is no AST filter, replace the old
    // file_filter (set-intersection) approach with UNION re-ranking via composite
    // weighted RRF:
    //
    //   1. Fetch a WIDER lexical pool (limit * BLAST_CANDIDATE_POOL_K) WITHOUT a
    //      file_filter so text-only matches outside the co-change partner set
    //      are still present in the lexical ranked list.
    //   2. Build a temporal ranked list from the co-change partner set:
    //      each partner gets an equal score of 1.0 so they all contribute rank
    //      terms to the RRF fusion.
    //   3. Run merge_layer_scores over [lexical, temporal] with the composite
    //      weights from config or the default profile.
    //   4. Recompose: carry the lexical SearchResult (snippet + line_range)
    //      for files that appear in the lexical pool.  Files present ONLY in the
    //      temporal list (co-change-only) get a stub result with the fused score.
    //   5. Truncate to --limit LAST (rank-then-truncate-LAST invariant).
    //
    // UNION semantics (AC12): a co-change partner absent from the lexical list
    // is still returned, ranked by its temporal RRF term alone (lexical absent →
    // contributes 0 under graceful absence).
    //
    // AC11 (temporal source): temporal ranked list is built from blast_radius_paths
    // which is resolved from TemporalDb::cochanges_for_file — the same store the
    // CLI blast-radius used before #200.  This satisfies AC11 (source identity).
    if config.blast_radius_paths.is_some() {
        return run_blast_radius_composite_query(
            config,
            &blast_file_ids,
            QueryContext {
                engine: &engine,
                sorted: &sorted,
                root,
                manifest: &manifest,
                stats,
                start,
            },
        );
    }
    // ── End composite UNION blast-radius path ─────────────────────────────────

    // ── Pure-lexical path (no blast-radius, no AST) ──────────────────────────
    //
    // AD-372-3 / AD-355-2: Two sub-paths depending on query shape.
    //
    // **Exact-symbol path** (`is_single_token` == true, ≥3 bytes):
    //   The reader's `search_exact_intersection` generates an AND-intersection
    //   candidate set that is grep-exact and limit/size-independent: every file
    //   containing the literal token is in the candidate set regardless of file
    //   size or pool size.  `sq.limit = None` (no LEXICAL_CANDIDATE_POOL_K
    //   widening) so the complete intersection reaches
    //   `resolve_paths_and_snippets_verified`.  The reader still ranks the
    //   intersection internally (AD-372-6: raw occurrence-count,
    //   length-norm-free), and offset is applied after ranking.
    //
    // **Multi-word / default path** (`is_single_token` == false):
    //   The existing UNION/BM25F path still uses LEXICAL_CANDIDATE_POOL_K×limit
    //   widening (AD-355-2) because BM25F rank is approximate and the definer
    //   file may appear past rank N in the UNION ordering.
    //
    // AD-355-4: Dropping non-matching candidates is a relevance gate, NOT an
    // output elision/cap under #317 "compress-never-truncate".  No
    // `elision_marker` is needed.
    // AD-374-3: the pool K constant lives at module level (pub(super)) so the AST
    // standalone path in ast.rs can reuse it without a divergent fork. The
    // multi-word path below references that module constant directly (no local
    // shadow); the value is unchanged (5), so pure-lexical behavior is identical.
    let exact_symbol = is_single_token(&config.text);
    // v5 positional search (#392 / #380 Phase 2): --phrase / --near queries need
    // the FULL ranked candidate set for the same verify-then-truncate-LAST reason
    // as the exact-symbol path (AD-372-3), so they share its branch below.
    let positional = config.phrase || config.near.is_some();

    let (raw_results, pool_was_capped) = if exact_symbol || positional {
        // AD-372-3 / RESOLVED Decision 3 (extended to the positional path, #392):
        // sq.limit = None: reader returns the FULL ranked intersection so that the
        // post-verify skip (below) operates on the verified set, not the pre-verify
        // intersection.  Applying offset inside the reader (pre-verify) would shift
        // page boundaries when stale/incidental-overlap files are removed by the
        // verify step — page-2 could silently omit a file that was at rank-1 after
        // verification.  Setting sq.offset = None (reader default) keeps the full
        // ranked intersection intact for the post-verify pagination below.
        //
        // pool_was_capped = false: sq.limit = None means the reader returns the
        // complete intersection — no external pool cap exists on this path, so
        // has_more is never conservatively inflated by a pool-cap signal.
        let mut sq = SearchQuery::new(config.text.clone());
        sq.limit = None;
        // sq.offset is intentionally left as None (== reader default 0): offset is
        // applied AFTER verification in resolve_paths_and_snippets_verified below,
        // matching RESOLVED Decision 3 and the multi-word path contract.
        sq.phrase = config.phrase;
        sq.near = config.near;
        sq.lang = config.lang; // D17 / AC16: --lang honored on positional + exact paths
        (engine.search(&sq)?, false)
    } else {
        // Multi-word / default: widen pool via LEXICAL_CANDIDATE_POOL_K (AD-355-2).
        // phrase/near are false/None here by construction (positional is false);
        // forwarded for clarity/symmetry with the branch above.
        //
        // AD-404-4 additive widening: pool = candidate_pool(limit, K) + offset.
        // At offset 0: pool == candidate_pool(limit, K) — zero regression.
        // NEVER the multiplicative candidate_pool(depth(), K) form (D-2).
        // saturating_add (not `+`) guards a hostile `--offset` near usize::MAX from
        // overflowing (applies PF-004, matches Page::depth()); byte-identical for
        // realistic offsets, clamps only at the usize::MAX ceiling.
        let pool_limit = candidate_pool(config.page().limit(), LEXICAL_CANDIDATE_POOL_K)
            .saturating_add(config.page().offset());
        let mut sq = SearchQuery::new(config.text.clone());
        sq.limit = Some(pool_limit);
        sq.phrase = config.phrase;
        sq.near = config.near;
        sq.lang = config.lang; // D17 / AC16: --lang honored on BM25F UNION path
        let results = engine.search(&sq)?;
        // AD-404-11 / D-5 pool-cap signal (cross-path consistency with AST path):
        // if the reader returned exactly pool_limit candidates it filled the pool to
        // the ceiling — there may be additional qualifying files beyond it that the
        // verify gate never sees.  Set pool_was_capped conservatively so
        // resolve_paths_and_snippets_verified can report has_more = true even when
        // the probe-then-truncate check finds exactly `limit` verified rows.
        // Direction is safe: a false positive (says "more" when there are none)
        // prompts one redundant paginate; a false negative silently drops results.
        let capped = results.len() == pool_limit;
        (results, capped)
    };

    // Resolve snippets, verify with the correct predicate, then truncate to --limit LAST.
    //
    // AD-355-2 / AD-355-4 / AD-372-3 / RESOLVED Decision 3:
    // verify-then-truncate-LAST invariant.  Offset is applied HERE (post-verify)
    // on BOTH the exact-symbol and multi-word paths so that page boundaries are
    // consistent regardless of how many pre-verify candidates are dropped.
    //
    // AD-393-5: select the predicate based on mode. Phrase/Near eliminate
    // trigram-containment false positives at the CLI gate (the reader is recall-
    // oriented, not the correctness authority — AD-393-1).
    let effective_offset = config.offset.unwrap_or(0);
    let (results, has_more) = resolve_paths_and_snippets_verified(
        &raw_results,
        &sorted,
        root,
        &manifest,
        SnippetVerifyParams {
            query: &config.text,
            layers_matched: &[],
            limit: config.limit,
            offset: effective_offset,
            verify_mode: verify_mode_for(config.phrase, config.near),
            pool_was_capped,
        },
    );

    let total = results.len();
    let duration_ms = start.elapsed().as_millis() as u64;

    Ok(QueryOutput {
        query: config.text.clone(),
        total,
        has_more,
        verify_mode: vm_label,
        results,
        duration_ms,
        index_stats: Some(stats),
        ast_coverage: None,
    })
}

/// Execution context threaded through the compound query path.
///
/// Bundles the read-only index state that is computed once in
/// [`execute_query_with_manifest`] and forwarded to [`run_compound_query`].
/// This eliminates the >5 positional argument count and makes the caller
/// site self-documenting.
struct QueryContext<'a> {
    engine: &'a QueryEngine,
    sorted: &'a [&'a str],
    root: &'a Path,
    manifest: &'a FileManifest,
    stats: IndexStats,
    start: Instant,
}

/// Execute the compound text+AST query branch (#198, #356).
///
/// Restricts the lexical engine to the AST-matched FileId set (AD-356-1),
/// sizes `sq.limit` to the candidate set (AD-356-2), runs
/// `intersect_and_rank` (HashMap join + weighted RRF fusion), recomposes the
/// results with lexical snippets, and returns a [`QueryOutput`].
///
/// Extracted from [`execute_query_with_manifest`] to give each path a
/// single-responsibility scope and eliminate the duplicated `QueryOutput`
/// construction tail.
///
/// # Errors
///
/// Returns `Err` when the lexical engine search fails.
fn run_compound_query(
    config: &super::types::QueryConfig,
    ast_scored_vec: &[(FileId, f64)],
    blast_file_ids: Option<HashSet<FileId>>,
    ctx: QueryContext<'_>,
) -> anyhow::Result<QueryOutput> {
    // Note: config.limit >= 1 is enforced by parse_limit_value (mod.rs) which
    // rejects --limit 0 with a CLI error, so limit:0 is unreachable in production.
    // The debug_assert below documents the invariant (not a runtime safety net).
    debug_assert!(
        config.limit >= 1,
        "config.limit must be >= 1 (CLI guarantee via parse_limit_value; see mod.rs)"
    );
    // AD-403-7: compute once for all QueryOutput sites in this function.
    let vm_label = verify_mode_for(config.phrase, config.near).json_label();

    // Build the AST FileId set once for O(1) membership tests below.
    let ast_fid_set: HashSet<FileId> = ast_scored_vec.iter().map(|&(fid, _)| fid).collect();

    // Early-out: empty AST set → no intersection possible → return empty output.
    // Correctness guard (AC12): an empty file_filter causes the reader to score
    // zero files regardless of sq.limit.
    if ast_fid_set.is_empty() {
        return Ok(QueryOutput {
            query: config.text.clone(),
            total: 0,
            has_more: false,
            verify_mode: vm_label,
            results: vec![],
            duration_ms: ctx.start.elapsed().as_millis() as u64,
            index_stats: Some(ctx.stats),
            ast_coverage: None,
        });
    }

    // Compute the lexical file_filter and pool size (AD-356-1 / AD-356-2).
    //
    // AD-356-1: restrict the lexical engine to the AST-matched FileId set so
    // `raw_lex` is exactly AST ∩ lexical-present — no qualifying file can fall
    // beyond a `limit * K` cliff. We removed CANDIDATE_POOL_K (ADR-003: the
    // correct fix is to eliminate the cap, not retune it).
    //
    // AD-356-2: size sq.limit to the candidate set, NOT None. The reader's
    // `unwrap_or(20)` default would silently re-cap at 20 and reintroduce #356
    // for AST sets > 20. filter_set.len() >= 1 is guaranteed by the two early-out
    // guards above (ast_fid_set.is_empty() and filter_set.is_empty()), so no
    // .max(1) is needed here.
    let filter_set: HashSet<FileId> = match blast_file_ids {
        // blast ∩ AST: O(blast) with O(1) membership test in ast_fid_set.
        Some(ref blast) => blast
            .iter()
            .filter(|id| ast_fid_set.contains(*id))
            .copied()
            .collect(),
        None => ast_fid_set,
    };
    // Disjoint blast∩AST early-out: blast and AST sets are non-empty but share
    // no files.  Symmetric with the ast_fid_set.is_empty() guard above — both
    // guards prevent unnecessary reader/intersect work and make the intent
    // explicit rather than relying on reader side-effect semantics (#356, ADR-003).
    if filter_set.is_empty() {
        return Ok(QueryOutput {
            query: config.text.clone(),
            total: 0,
            has_more: false,
            verify_mode: vm_label,
            results: vec![],
            duration_ms: ctx.start.elapsed().as_millis() as u64,
            index_stats: Some(ctx.stats),
            ast_coverage: None,
        });
    }
    // AD-356-2: size sq.limit to the candidate set.  filter_set.len() >= 1 is
    // guaranteed by the early-out above, so .max(1) is now a compile-time
    // no-op kept for clarity.
    // AD-393-7: thread phrase/near through the compound-AST path so that
    // `skim search "fn main" --ast nested-loop --phrase` correctly narrows
    // the AST candidate set using positional alignment (not just BM25F recall).
    let mut sq = SearchQuery::new(config.text.clone());
    sq.limit = Some(filter_set.len());
    sq.file_filter = Some(filter_set);
    sq.phrase = config.phrase;
    sq.near = config.near;
    sq.lang = config.lang; // D17 / AC16: --lang honored on compound AST path

    let raw_lex = ctx.engine.search(&sq)?;

    // Compound intersect + RRF fusion (pure, no I/O, closures only).
    //
    // WAVE 4a — structural seam is a no-op:
    // The structural_lookup and avg_max_depth parameters exist to enable
    // depth-based AST re-ranking (AC2/AC12), but on this production path both
    // are placeholders deferred to #290.  As a result every entry's depth_key
    // is 0.0 and the AST decorate-sort reduces to pure ast_score-DESC order.
    // The shipped Wave 4a ranking is lexical-rank + AST-score-rank RRF only.
    //
    // AD-377-1 / PF-006: honor caller-supplied `--weights` on the compound
    // text+--ast path, identical to the blast path (run_blast_radius_composite_query).
    // Before #377 this hardcoded `CompositeWeights::default()`, silently ignoring
    // `--weights` here while accepting it without error — exactly the silent-inert
    // bug #377 fixes.  Only `lexical` + `ast` are consumed by intersect_and_rank;
    // a supplied non-zero `temporal` is still inert (the user is told via the
    // temporal-scoped notice in execute_query_with_manifest, AD-377-2).
    //
    // AD-377-4: `--weights 0,0,0` is a deliberate "no ranking signal" request on
    // this path — intersect_and_rank then scores every intersected file 0.0 and the
    // FileId-ASC tiebreaker orders them, so the compound path returns the FULL
    // intersection at score 0.0 (AC5).  This DIVERGES intentionally from the blast
    // path, where all-zero weights collapse the RRF UNION to an empty result; the
    // divergence is documented at both sites and merge_layer_scores is left unchanged.
    let composite_weights = config
        .composite_weights
        .unwrap_or_else(CompositeWeights::with_six_signal_defaults);
    let ranked = intersect_and_rank(
        &raw_lex,
        ast_scored_vec,
        // AD-405-12: `--blast-radius`/`--weights` need no AST-coverage caveat
        // today because this seam returns `None` for every file (no structural
        // metric contribution).  Once #290 wires the real AstIndexReader here,
        // an AST-excluded file (size > 1 MiB) will contribute a zero structural
        // term and be silently down-ranked — same defect class as pre-#405 recall
        // gaps.  #290 MUST inherit the ast_size_limit() contract and emit
        // ast_coverage accordingly.
        |_: FileId| -> Option<StructuralMetrics> { None }, // structural seam — placeholder until #290
        0.0_f32,                                           // avg_max_depth — placeholder until #290
        composite_weights,
    );

    // Recompose: carry lexical SearchResult (snippet + line_range), replace score.
    // NOTE: recompose_with_lexical operates on the FULL `ranked` list (AST-set sized,
    // AD-356-1), not a pre-truncated slice — this preserves the AD-355-2
    // verify-then-truncate-LAST invariant.  We MUST NOT truncate to config.limit here;
    // if we did, and the top `limit` RRF slots were occupied by incidental-overlap junk,
    // the real definer at slot limit+1 would be dropped before verification could keep
    // it and the junk is removed.  Truncation happens LAST in
    // resolve_paths_and_snippets_verified (after verification filters non-matching
    // candidates), matching the pure-lexical and blast-radius paths.
    let recomposed = recompose_with_lexical(&ranked, &raw_lex);

    // AC-F6: text+AST compound path → layers_matched = ["lexical","ast"] (stable order).
    //
    // AD-355-2/AD-355-4: verify substring membership over the FULL recomposed list,
    // then truncate to --limit LAST.
    //
    // AD-356-3: No output::elision_marker here. The pool == the full AST∩lexical
    // set (AD-356-1), so the only truncation is the user's --limit — honored
    // display semantics, not a hidden performance cap. Emitting an elision notice
    // would fire on every --limit < result_count, which is normal truncation, not
    // elision. Compress-never-truncate (CLAUDE.md) is satisfied: no internal cap.
    //
    // Verification drops non-matching candidates (relevance gate, not a #317 cap);
    // truncation to config.limit happens inside resolve_paths_and_snippets_verified
    // as the final step.
    // AD-372-3 / PF-006: thread config.offset into the compound path so that
    // `skim search "foo" --ast try-catch --offset 10` paginates correctly.
    // The RRF recomposition does NOT apply offset; pagination is handled here,
    // post-verify, as on the pure-lexical path.
    // AD-393-7: same VerifyMode as the pure-lexical path so that phrase/near
    // correctness carries through the compound text+--ast dispatch.
    let effective_offset = config.offset.unwrap_or(0);
    let (results, has_more) = resolve_paths_and_snippets_verified(
        &recomposed,
        ctx.sorted,
        ctx.root,
        ctx.manifest,
        SnippetVerifyParams {
            query: &config.text,
            layers_matched: &["lexical", "ast"],
            limit: config.limit,
            offset: effective_offset,
            verify_mode: verify_mode_for(config.phrase, config.near),
            // Pool = exact AST∩lexical set (AD-356-1): the reader's file_filter
            // restricts candidates to exactly the AST-matched FileId set, so the
            // pool is tight and cannot be capped by an external K-multiplier.
            pool_was_capped: false,
        },
    );
    let total = results.len();
    let duration_ms = ctx.start.elapsed().as_millis() as u64;
    Ok(QueryOutput {
        query: config.text.clone(),
        total,
        has_more,
        verify_mode: vm_label,
        results,
        duration_ms,
        index_stats: Some(ctx.stats),
        ast_coverage: None,
    })
}

/// Execute the composite UNION blast-radius re-ranking path (#200).
///
/// Fuses the lexical ranked list and the temporal co-change ranked list into
/// a single composite ranking via weighted RRF (UNION semantics):
///
/// - Files present ONLY in the lexical list: contribute their lexical rank term.
/// - Files present ONLY in the temporal (co-change) list: contribute their
///   temporal rank term alone (graceful absence = 0 from the lexical layer).
///   These are co-change-only files that the text query did not match — they
///   APPEAR in UNION mode (AC12) but would be ABSENT under old filter mode.
/// - Files present in BOTH: accumulate both rank terms (AC2 multi-layer bonus).
///
/// The output score field carries the fused RRF value, NOT a BM25F magnitude
/// (AC14: score is documented as composite fused RRF in the doc comment below
/// and in the `ResolvedResult::score` field doc).
///
/// # Temporal ranked list construction (AC11 source identity)
///
/// The temporal ranked list is built from `blast_paths` (already resolved from
/// `TemporalDb::cochanges_for_file` — the same SQLite source the CLI used
/// before #200).  Each co-change partner path is assigned an equal score of
/// `1.0` (uniform temporal rank input) and converted to `FileId` via the
/// manifest's `sorted_paths`.  The Jaccard-value-aware ranking within the
/// temporal list is not preserved here; the RRF framework uses rank, not
/// magnitude, so the order within the temporal list only matters when there
/// are many co-change partners.  Improvement tracked for follow-up: use the
/// Jaccard score as the raw temporal score for better rank ordering (#200+).
fn run_blast_radius_composite_query(
    config: &super::types::QueryConfig,
    blast_file_ids: &Option<HashSet<FileId>>,
    ctx: QueryContext<'_>,
) -> anyhow::Result<QueryOutput> {
    // AD-403-7: compute once for all QueryOutput sites in this function.
    let vm_label = verify_mode_for(config.phrase, config.near).json_label();

    // Effective weights: use caller-supplied override or the six-signal #200 profile.
    let weights = config
        .composite_weights
        .unwrap_or_else(CompositeWeights::with_six_signal_defaults);

    // Step 1: fetch a WIDE lexical ranked list WITHOUT a file_filter.
    //
    // The UNION contract requires ranking the complete candidate set (all files
    // that appear in *either* the lexical or temporal list) before truncation.
    // Applying a bare `config.limit` pre-cap here would silently drop co-change
    // partners whose lexical rank exceeds the cap, violating the rank-then-
    // truncate-LAST invariant.
    //
    // We set sq.limit = Some(K × limit).max(100) on EVERY path — trigram-scored
    // and short-query fallback alike.  The reader's `unwrap_or(20)` default is
    // never reached on this path because we always pass Some(N>=100).
    //
    // K=10: generous multiple of limit so RRF fusion still sees enough candidates
    // for the co-change-UNION to work correctly even if many lexical hits fail
    // verification.  The worst-case file reads are O(K × limit) per query; on
    // the AD-355-7 short-query fallback the candidate set is still bounded to
    // Some(K × limit).max(100) before the verify step.  Calibrating K for large
    // corpora is tracked in #361.
    // AD-393-12: in positional mode (--phrase / --near), use sq.limit = None to
    // get the full ranked intersection (same as the pure-lexical exact-symbol path)
    // so the verify step operates on all phrase/near-aligned candidates.  In the
    // default (substring) mode keep the K×limit widened pool.
    let positional = config.phrase || config.near.is_some();
    const BLAST_CANDIDATE_POOL_K: usize = 10;
    let mut sq = SearchQuery::new(config.text.clone());
    sq.limit = if positional {
        None // full intersection for phrase/near — verify-then-truncate-LAST (AD-393-12)
    } else {
        // AD-404-13 / AC-404-13: BLAST_CANDIDATE_POOL_K=10 is load-bearing (pinned by
        // AC-404-13) — do NOT replace with bare config.page().depth() (pool-shrink
        // regression). Additive form: candidate_pool(limit, K) + offset (D-2).
        // saturating_add (not `+`) guards a hostile `--offset` near usize::MAX from
        // overflowing (applies PF-004, matches Page::depth()); byte-identical for
        // realistic offsets, clamps only at the usize::MAX ceiling.
        Some(
            candidate_pool(config.page().limit(), BLAST_CANDIDATE_POOL_K)
                .saturating_add(config.page().offset()),
        )
    };
    // AD-393-12: thread phrase/near through the blast-radius SearchQuery so the
    // reader's search_positional path is exercised (not just BM25F recall).
    sq.phrase = config.phrase;
    sq.near = config.near;
    sq.lang = config.lang; // D17 / AC16: --lang honored on blast-radius path
    // No file_filter: UNION mode requires the full lexical ranked list.
    let raw_lex = ctx.engine.search(&sq)?;
    // AD-393-12: select the verify predicate for the blast path.
    let blast_verify_mode = verify_mode_for(config.phrase, config.near);

    // Step 2: build the temporal ranked list from blast_paths.
    // Each co-change partner path → FileId (via sorted_paths index).
    // Score = 1.0 (uniform; RRF uses rank not magnitude, so this suffices).
    // The target file itself is included in blast_paths by resolve_blast_radius_paths.
    // When blast_file_ids is None (temporal DB absent), degrades to lexical-only ranking.
    let mut temporal_layer: Vec<(FileId, f64)> = blast_file_ids
        .as_ref()
        .map(|ids| ids.iter().map(|&fid| (fid, 1.0)).collect())
        .unwrap_or_default();
    // Sort by FileId for deterministic rank assignment within the layer.
    // All have equal scores, so the sort order determines their temporal ranks.
    temporal_layer.sort_unstable_by_key(|&(fid, _)| fid.0);

    // Step 3: lexical ranked list from raw_lex (already sorted DESC by score).
    let lexical_layer: Vec<(FileId, f64)> = raw_lex.iter().map(|r| (r.file_id, r.score)).collect();

    // Step 4: N-signal RRF UNION merge.
    // The blast-radius path fuses only the lexical and co-change (temporal)
    // signals, so only those two layers are constructed here. The `ast` weight
    // (0.3 by default) and the extended signals (import_graph, dir_proximity,
    // structural_coupling — all 0.0 by default per ADR-003) have no layer to
    // apply to on this path; wiring the full text+AST+temporal compound dispatch
    // is tracked in #339.
    let layers: &[(Vec<(FileId, f64)>, f64)] = &[
        (lexical_layer, weights.lexical),
        (temporal_layer, weights.temporal),
    ];
    let ranked = merge_layer_scores(layers);

    // Step 5: rank the full UNION set, then apply verification + truncation LAST.
    //
    // AD-355-2: do NOT truncate before verification.  The UNION contract requires
    // all candidates to be ranked before any are dropped.  After verification the
    // count is capped at --limit.
    //
    // AD-355-4: dropping a lexical-hit candidate that fails substring verification
    // is a relevance gate, not a #317 output cap.  No elision_marker needed.
    let lex_map: HashMap<FileId, &SearchResult> = raw_lex.iter().map(|r| (r.file_id, r)).collect();

    // Step 6: recompose results with verification for lexical-hit candidates.
    //
    // For files present in the lexical pool: read snippet + verify substring
    // membership in a SINGLE file read via extract_snippet_and_verify (AD-355-1).
    // Drop the candidate if verification fails.
    //
    // For co-change-only files (absent from lexical pool): no file content is
    // available here; these are pure temporal results that the text query did not
    // match — include them unconditionally (AC12, UNION mode).
    // AD-372-3 / PF-006: thread config.offset into the blast-radius path so that
    // `skim search "foo" --blast-radius src/x.rs --offset 10` paginates correctly.
    // Applied post-verify (`.skip` before `.take`), consistent with the pure-lexical
    // and compound paths.
    let effective_offset = config.offset.unwrap_or(0);
    // AD-404-11 / D-5: collect limit+1 results to detect has_more without a second
    // pass; truncated to config.limit below after the has_more check.
    let mut results: Vec<super::types::ResolvedResult> = ranked
        .iter()
        .filter_map(|&(fid, composite_score)| {
            let path = ctx.sorted.get(fid.0 as usize)?;
            let manifest_entry = ctx.manifest.lookup(path);

            if let Some(&lex_result) = lex_map.get(&fid) {
                // File has a lexical hit: verify and extract snippet in one read
                // (AD-355-1 — no second I/O).
                // AD-393-12: use blast_verify_mode (Phrase/Near/Substring) so
                // trigram-containment false positives are eliminated on this path too.
                let mut r = lex_result.clone();
                r.score = composite_score;

                let (snippet_outcome, verified) = extract_snippet_and_verify(
                    ctx.root,
                    path,
                    &r.match_positions,
                    manifest_entry,
                    &config.text,
                    blast_verify_mode,
                );

                // Drop lexical-hit candidates that do not pass the predicate.
                // Stale files produce verified=false and are dropped — positions
                // may be wrong and we cannot confirm content without re-reading.
                if !verified {
                    return None;
                }

                let (line_number, line_range, snippet, stale) = decode_snippet(snippet_outcome);
                Some(super::types::ResolvedResult {
                    path: path.to_string(),
                    score: composite_score,
                    field: r.field.name().to_string(),
                    line_number,
                    line_range,
                    snippet,
                    stale,
                    match_positions: r.match_positions.clone(),
                    temporal: None,
                    layers_matched: vec![],
                })
            } else if positional {
                // AD-393-12: co-change peer gate — in positional mode (--phrase/--near),
                // co-change-only files (absent from the lang-filtered raw_lex pool) are
                // dropped unconditionally.
                //
                // Why this is safe and correct:
                //   • raw_lex is built with sq.lang = config.lang, so any file that
                //     truly contains the phrase AND matches the lang filter is already
                //     in the lexical pool and handled by the first branch above.
                //   • A co-change-only peer that contains the phrase but has a wrong
                //     language would be a false positive violating --lang semantics.
                //   • A co-change-only peer that does NOT contain the phrase must be
                //     excluded by the token-exact predicate regardless.
                //   • Therefore any file that should appear in a positional result set
                //     is reachable via the lexical path; the co-change-only path adds
                //     no recall in positional mode.
                //
                // Prior implementation read the file and ran the predicate, but because
                // the --lang filter is not applied at the manifest-lookup level, a wrong-
                // language peer that happened to contain the phrase would be included,
                // violating D17 / AC16.  The unconditional drop is both simpler and
                // correct (AD-393-12; review finding medium/architecture 2026-07-04).
                None
            } else {
                // Co-change-only file: no lexical hit → no snippet (AC12, UNION mode).
                // These files appear because their temporal rank contributes to the
                // fused score even though the text query did not match them.
                // No file content is read here; include unconditionally (substring mode).
                Some(super::types::ResolvedResult {
                    path: path.to_string(),
                    score: composite_score,
                    field: "co_change_partner".to_string(),
                    line_number: None,
                    line_range: None,
                    snippet: None,
                    stale: false,
                    match_positions: vec![],
                    temporal: None,
                    layers_matched: vec![],
                })
            }
        })
        // AD-355-2 / AD-372-3: apply offset then truncate LAST — after verification
        // removes non-matching candidates (consistent with pure-lexical path).
        // Probe one extra (config.limit + 1) to detect has_more without a second
        // pass; truncated to config.limit below (AD-404-11).
        .skip(effective_offset)
        .take(config.limit.saturating_add(1))
        .collect();
    let has_more = results.len() > config.limit;
    results.truncate(config.limit);

    let total = results.len();
    let duration_ms = ctx.start.elapsed().as_millis() as u64;
    Ok(QueryOutput {
        query: config.text.clone(),
        total,
        has_more,
        verify_mode: vm_label,
        results,
        duration_ms,
        index_stats: Some(ctx.stats),
        ast_coverage: None,
    })
}

/// Decode a `SnippetOutcome` into the 4-tuple used by `ResolvedResult`.
fn decode_snippet(
    outcome: SnippetOutcome,
) -> (
    Option<u32>,
    Option<std::ops::Range<usize>>,
    Option<super::types::SnippetContext>,
    bool,
) {
    match outcome {
        SnippetOutcome::Ok {
            match_line,
            line_range,
            context,
        } => (Some(match_line), Some(line_range), Some(context), false),
        SnippetOutcome::Stale => (None, None, None, true),
        SnippetOutcome::Unavailable => (None, None, None, false),
    }
}

/// Output-shaping parameters for [`resolve_paths_and_snippets_verified`].
///
/// Groups the query string, layer attribution, pagination, and verify-mode fields
/// so the function signature stays within the seven-argument Clippy limit.
struct SnippetVerifyParams<'a> {
    /// The literal query text used for AND-token verification (AD-355-3).
    query: &'a str,
    /// Layer names that contributed signal to these results (e.g. `["lexical"]`
    /// or `["lexical", "ast"]`).  Forwarded verbatim into each [`ResolvedResult`].
    layers_matched: &'a [&'static str],
    /// Maximum results to return (applied LAST, after verification).
    limit: usize,
    /// Number of verified results to skip before collecting (AD-372-3).
    offset: usize,
    /// Predicate to apply for correctness verification (AD-393-5).
    ///
    /// - `Substring`: pre-#393 default — each whitespace token must appear as a
    ///   substring.
    /// - `Phrase` / `Near(n)`: token-exact phrase or proximity predicate
    ///   (`phrase_tokens_present` / `near_tokens_present`).  Eliminates
    ///   trigram-containment false positives, e.g. `encode_length varint_writer`
    ///   must NOT match when the query is `encode varint`.
    verify_mode: VerifyMode,
    /// Conservative pool-cap fallback for has_more (AD-404-11 / D-5).
    ///
    /// When `true`, has_more is set to `true` even if the probe-then-truncate
    /// check finds exactly `limit` verified results — because the pre-verify pool
    /// was filled to its ceiling and there may be additional qualifying candidates
    /// beyond it that the verify gate never saw.  Mirrors the AST path's
    /// `pool_was_capped` logic (ast.rs line `pool_was_capped = pooled.len() == window
    /// && raw_count > window`), closing the cross-path consistency gap.
    ///
    /// Always `false` on the exact-symbol / positional path (sq.limit = None →
    /// full intersection, no ceiling) and on the compound text+AST path (pool is
    /// the exact AST-intersection set, no K-multiplier cap).  Only non-trivially
    /// `true` on the multi-word BM25F path when the reader fills its pool to
    /// `candidate_pool(limit, K) + offset`.
    pool_was_capped: bool,
}

/// Map `FileId`s to paths, extract snippets, **verify substring membership**,
/// and truncate to `limit` — all in one pass with a single file read per result.
///
/// # Design (AD-355-1 / AD-355-2 / AD-355-3 / AD-355-4)
///
/// Candidate-then-verify: the caller fetches a **wider** candidate pool
/// (`LEXICAL_CANDIDATE_POOL_K × limit`) so the definer file is not truncated
/// before verification.  This fn then:
///
/// 1. Reads each file once via [`extract_snippet_and_verify`] — no second I/O.
/// 2. Drops any candidate whose file content does not contain the literal query
///    as an AND-of-whitespace-tokens (case-sensitive; see AD-355-3).
/// 3. Truncates to `limit` LAST — after verification, not before (AD-355-2).
///
/// Dropping non-matching candidates is a **relevance gate**, not a #317 output
/// cap.  No `elision_marker` is needed (AD-355-4).
///
/// # Fan-out bound (AD-355-2 / #361)
///
/// The worst-case file-read count equals `raw_results.len()`, which is bounded
/// by the caller's `sq.limit` — itself bounded to:
///   - Pure-lexical exact-symbol path: `|intersection|` (AND of query trigram posting
///     lists; `sq.limit = None` so the full intersection reaches verify). Bounded by
///     posting list sizes, not corpus size (AD-372-2 superset invariant).
///   - Pure-lexical multi-word path:   `max(limit × LEXICAL_CANDIDATE_POOL_K, 100)` = `max(5N, 100)`.
///   - Blast-radius path:              `max(limit × BLAST_CANDIDATE_POOL_K, 100)`  = `max(10N, 100)`.
///   - Compound text+AST:              `|ast_set|` (exact AST match count; no K multiplier, AD-356-1).
///
/// The fan-out is therefore O(K × limit) file reads — bounded for any fixed K and
/// user-supplied `--limit`.  Calibrating K for large corpora is tracked in #361.
fn resolve_paths_and_snippets_verified(
    raw_results: &[SearchResult],
    sorted_paths: &[&str],
    root: &Path,
    manifest: &FileManifest,
    params: SnippetVerifyParams<'_>,
) -> (Vec<ResolvedResult>, bool) {
    let SnippetVerifyParams {
        query,
        layers_matched,
        limit,
        offset,
        verify_mode,
        pool_was_capped,
    } = params;
    // AD-404-11 / D-5: probe one extra result beyond the page boundary so we can
    // report `has_more` without a full second pass over the verified set.
    // `.take(limit + 1)` reads at most one file past the page; if the probe item
    // exists, `has_more = true` and we truncate back to `limit`.
    let mut probe: Vec<ResolvedResult> = raw_results
        .iter()
        .filter_map(|r| {
            let path = sorted_paths.get(r.file_id.0 as usize)?;
            let manifest_entry = manifest.lookup(path);

            // Read file once; verify with the correct predicate and extract snippet
            // in one call (AD-355-1 / AD-393-5). verify_mode selects:
            // Substring (pre-#393 default), Phrase (exact token order), or Near(n).
            let (outcome, verified) = extract_snippet_and_verify(
                root,
                path,
                &r.match_positions,
                manifest_entry,
                query,
                verify_mode,
            );

            // Drop candidates that do not pass the predicate.
            // Stale files produce verified=false and are also dropped — we
            // cannot confirm their content matches without re-reading.
            if !verified {
                return None;
            }

            let (line_number, line_range, snippet, stale) = decode_snippet(outcome);

            Some(ResolvedResult {
                path: path.to_string(),
                score: r.score,
                field: r.field.name().to_string(),
                line_number,
                line_range,
                snippet,
                stale,
                match_positions: r.match_positions.clone(),
                temporal: None,
                layers_matched: layers_matched.to_vec(),
            })
        })
        // AD-355-2 / AD-372-3: apply offset then truncate LAST — after verification
        // removes non-matching candidates.
        // Probe one extra (limit.saturating_add(1)) to detect has_more without a
        // second pass; truncated to `limit` below (AD-404-11).
        .skip(offset)
        .take(limit.saturating_add(1))
        .collect();
    // AD-404-11 / D-5: probe-then-truncate has_more check.
    // `probe.len() > limit` fires when the probe item (rank offset+limit+1) exists
    // in the verified set.  `pool_was_capped` is the conservative pool-cap fallback:
    // when the pre-verify pool was filled to its ceiling there may be additional
    // qualifying candidates beyond it that verification never read — so has_more is
    // set true even when the probe check alone would produce false.  Mirrors the
    // AST path (`has_more = pre_page_len > page.depth() || pool_was_capped`).
    let has_more = probe.len() > limit || pool_was_capped;
    probe.truncate(limit);
    (probe, has_more)
}

// ============================================================================
// Output formatters
// ============================================================================

/// Build an optional temporal annotation suffix for a single result line.
///
/// Examples:
/// - hotspot only  → `"  hotspot: 0.950"`
/// - risk only     → `"  risk: 0.800"`
/// - both          → `"  hotspot: 0.950  risk: 0.800"`
/// - neither       → `""`
fn temporal_annotation_tag(temporal: Option<&super::types::TemporalAnnotation>) -> String {
    let Some(t) = temporal else {
        return String::new();
    };
    let parts: Vec<String> = [
        t.hotspot_score.map(|hs| format!("hotspot: {hs:.3}")),
        t.risk_score.map(|rs| format!("risk: {rs:.3}")),
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() {
        return String::new();
    }
    format!("  {}", parts.join("  "))
}

/// Format query results as human-readable text to `w`.
///
/// Format per result:
/// ```text
/// src/auth/middleware.rs:42  [function_signature]  score: 12.34  hotspot: 0.950
///   41│ /// Validates JWT token
///   42│ pub fn authenticate(req: &Request) -> Result<Claims> {
///   43│     let header = req.header("Authorization")
/// ```
pub(super) fn format_text_output(
    output: &QueryOutput,
    w: &mut impl std::io::Write,
) -> anyhow::Result<()> {
    if output.results.is_empty() {
        writeln!(w, "no results for {:?}", output.query)?;
        return Ok(());
    }

    for r in &output.results {
        let line_info = r.line_number.map(|ln| format!(":{ln}")).unwrap_or_default();
        let stale_tag = if r.stale { "  [stale]" } else { "" };

        // Compose optional temporal annotation suffix: "  hotspot: 0.95  risk: 0.80"
        let temporal_tag = temporal_annotation_tag(r.temporal.as_ref());

        writeln!(
            w,
            "{}{}  [{}]  score: {:.2}{}{}",
            r.path, line_info, r.field, r.score, stale_tag, temporal_tag
        )?;

        if let Some(ctx) = &r.snippet {
            for line in &ctx.lines {
                let marker = if line.is_match { ">" } else { " " };
                writeln!(w, "  {}  {:>4}│ {}", marker, line.line_number, line.content)?;
            }
        }
        writeln!(w)?;
    }

    // AD-412-4: Echo the effective query in the non-empty human summary so a
    // silently-mangled query can never masquerade as a successful search.
    // Using {:?} matches the empty-branch quoting convention (`no results for {:?}`)
    // for uniform escaping. JSON already carries `query`; this echo is text-only
    // and does NOT alter JSON output.
    writeln!(
        w,
        "{} result(s) for {:?} in {}ms",
        output.total, output.query, output.duration_ms
    )?;

    Ok(())
}

/// Format query results as a JSON object to `w`.
pub(super) fn format_json_output(
    output: &QueryOutput,
    w: &mut impl std::io::Write,
) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(output)?;
    writeln!(w, "{json}")?;
    Ok(())
}

// ============================================================================
// Tests (co-located in query_tests.rs)
// ============================================================================

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;
