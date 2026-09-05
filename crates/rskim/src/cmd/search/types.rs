//! Shared types for the `skim search index` pipeline.
//!
//! All types here are pure data — no I/O, no side effects.

use std::ops::Range;
use std::path::PathBuf;
use std::time::Duration;

use rskim_search::SearchField;
use serde::Serialize;

// ============================================================================
// Pagination value type (AD-404-1/2/3)
// ============================================================================

/// A pagination cursor that bundles `limit` and `offset` into one value.
///
/// ## AD-404-1: why a struct, not two loose `usize` arguments
///
/// The P1 root cause (mod.rs:201-212 passing `limit` but silently never
/// threading `offset`) happened because `run_ast_standalone` accepted
/// `limit: usize` as a positional argument — a positional argument can be
/// forgotten in a function call whereas a struct field cannot be omitted when
/// the struct literal is constructed.  `Page` does NOT make omission a compile
/// error — `Page::new(flags.limit, None)` compiles fine — but it expresses
/// pagination as an indivisible concept so every call site must visibly choose
/// `Page::first(limit)` (offset=0) or `Page::new(limit, flags.offset)`, making
/// the intent clear at every dispatch site.  The real guard is the dispatch-level
/// argv test (`cli_search_offset.rs`).
///
/// ## AD-404-2: depth() arithmetic
///
/// `depth() = limit.saturating_add(offset)` is the minimum candidates the
/// pre-verify pool must hold.  `saturating_add` guards a hostile
/// `--offset near usize::MAX` from overflowing (applies PF-004: widen before
/// adding an offset).  Zero-regression property: when offset==0, depth()==limit,
/// so every raw-order widening is a provable no-op for existing behavior.
///
/// ## AD-404-3: apply() ownership and position
///
/// `apply` is a consuming drain-then-truncate that implements the canonical
/// `skip(offset) -> take(limit)` sequence.  It must run AFTER any temporal
/// re-sort and AFTER the verify gate, never before — the comment at each call
/// site cites this rule.  It takes `&mut Vec<T>` (not `Vec<T>`) so callers keep
/// their named binding and can inspect the result immediately after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Page {
    limit: usize,
    offset: usize,
}

impl Page {
    /// Construct from a `limit` and an optional `offset`.
    ///
    /// `None` offset is treated as 0 (the default when the flag is absent).
    pub(super) fn new(limit: usize, offset: Option<usize>) -> Self {
        Self {
            limit,
            offset: offset.unwrap_or(0),
        }
    }

    /// Construct a first-page cursor (`offset = 0`).
    ///
    /// Use at call sites where `--offset` is intentionally absent or ignored
    /// (e.g. unit tests that predate pagination).  Avoids the
    /// `Page::new(limit, None)` form so grep can distinguish
    /// "intentionally no offset" from "accidentally forgot offset".
    ///
    /// `#[allow(dead_code)]`: this function is a test helper called only from
    /// `#[cfg(test)]` modules (`ast_tests.rs`, `temporal_tests.rs`).  The bin
    /// crate's dead_code lint fires because no non-test code calls it; the
    /// attribute silences the false positive while keeping the function available
    /// to all test modules that need it.
    #[allow(dead_code)]
    pub(super) fn first(limit: usize) -> Self {
        Self { limit, offset: 0 }
    }

    /// The maximum number of results to return.
    #[must_use]
    #[inline]
    pub(super) fn limit(self) -> usize {
        self.limit
    }

    /// The number of verified results to skip before collecting.
    #[must_use]
    #[inline]
    pub(super) fn offset(self) -> usize {
        self.offset
    }

    /// Minimum pre-verify pool size to guarantee this page is fully fillable.
    ///
    /// `limit.saturating_add(offset)` — safe against `offset` near `usize::MAX`
    /// (PF-004 / AD-404-2).  When offset==0, depth()==limit (zero-regression at
    /// all existing call sites).
    #[must_use]
    #[inline]
    pub(super) fn depth(self) -> usize {
        self.limit.saturating_add(self.offset)
    }

    /// Apply skip-then-take to `rows`: drain the first `offset` elements, then
    /// truncate to `limit`.  Position is load-bearing: call AFTER verify gate
    /// and AFTER any temporal re-sort.  See AD-404-3.
    pub(super) fn apply<T>(self, rows: &mut Vec<T>) {
        if self.offset > 0 {
            let skip = self.offset.min(rows.len());
            rows.drain(..skip);
        }
        rows.truncate(self.limit);
    }
}

impl QueryConfig {
    /// Return the pagination cursor implied by this config's `limit` and `offset`.
    ///
    /// This is the canonical way to derive a `Page` from a `QueryConfig` rather
    /// than repeating `Page::new(config.limit, config.offset)` at each call site.
    /// Upstream tickets (#403, #405) MUST call `config.page()` rather than
    /// constructing their own `Page` so that any change to `QueryConfig`'s offset
    /// handling propagates automatically.
    ///
    /// ## AD-404-4: additive pool widening
    ///
    /// The candidate pool passed to the lexical engine MUST be computed as
    /// `candidate_pool(page.limit(), K).saturating_add(page.offset())` — the
    /// additive form (not the multiplicative `candidate_pool(page.depth(), K)`
    /// form, which is D-2 / Decision 2 violation).  The call sites in `query.rs`
    /// cite this decision; this accessor ensures callers consistently derive the
    /// same `limit` and `offset` values that drive the pool calculation.
    #[must_use]
    pub(super) fn page(&self) -> Page {
        Page::new(self.limit, self.offset)
    }
}

// ============================================================================
// Temporal query types (Issue #189)
// ============================================================================

/// Sort mode for temporal queries. Mutually exclusive with each other.
///
/// When combined with a text query, the sort is applied to the text results.
/// When used standalone (no query text), produces a ranked list from temporal DB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TemporalSort {
    /// Sort by hotspot score descending (most active files first).
    Hot,
    /// Sort by hotspot score ascending (least active files first).
    Cold,
    /// Sort by fix_density descending (most bug-prone files first).
    Risky,
}

impl TemporalSort {
    /// Human-readable flag name for use in error messages and `degraded_notice` calls.
    ///
    /// Returns the `--`-prefixed form (e.g. `"--hot"`).  Use [`Self::json_name`]
    /// for `DegradedJson.requested` where the plan contract (AC-4/AC-7) requires
    /// the bare form.
    pub(super) fn flag_name(self) -> &'static str {
        match self {
            Self::Hot => "--hot",
            Self::Cold => "--cold",
            Self::Risky => "--risky",
        }
    }

    /// Bare name for `DegradedJson.requested` (AC-4 / AC-7: no `--` prefix).
    ///
    /// `flag_name()` keeps the `--`-prefixed form for message text and
    /// `degraded_notice` calls where the dashed form is correct.
    pub(super) fn json_name(self) -> &'static str {
        match self {
            Self::Hot => "hot",
            Self::Cold => "cold",
            Self::Risky => "risky",
        }
    }
}

/// Temporal annotation attached to a resolved search result.
///
/// Fields are `None` when the file is not present in the temporal database.
#[derive(Debug, Clone, Serialize, Default)]
pub(super) struct TemporalAnnotation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hotspot_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_density: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cochange_jaccard: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changes_30d: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changes_90d: Option<u32>,
}

// ============================================================================
// Snippet types
// ============================================================================

/// A single line in a snippet context window.
#[derive(Debug, Clone, Serialize)]
pub(super) struct SnippetLine {
    /// 1-indexed line number in the original source file.
    pub line_number: u32,
    /// Raw text of the line (no trailing newline).
    pub content: String,
    /// `true` for the primary match line; `false` for context lines.
    pub is_match: bool,
}

/// A window of source lines surrounding a search match.
#[derive(Debug, Clone, Serialize)]
pub(super) struct SnippetContext {
    /// Lines in the context window, ordered by line number.
    pub lines: Vec<SnippetLine>,
}

// ============================================================================
// Query types
// ============================================================================

/// Path-to-Jaccard map type used by [`QueryConfig::blast_radius_paths`].
///
/// AD-409-1: The map carries co-change partner path → Jaccard score. The
/// blast-radius target (seed) is also present in this map, keyed to
/// [`super::temporal::SEED_STRENGTH`] — a finite sentinel strictly greater than
/// the Jaccard maximum of 1.0 — so it always ranks first in the temporal layer
/// after the ranking fix in ticket #409.
pub(super) type BlastRadiusStrengths = std::collections::HashMap<String, f64>;

/// Configuration for a query execution run.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(super) struct QueryConfig {
    /// The raw query text.
    pub text: String,
    /// Maximum number of results to return (default: 20).
    pub limit: usize,
    /// Number of results to skip before returning (for pagination).
    ///
    /// `None` means no offset (equivalent to 0). Used on the pure-lexical exact-symbol
    /// path (AD-372-3) and threaded through to `resolve_paths_and_snippets_verified`.
    pub offset: Option<usize>,
    /// When `true`, output JSON instead of human-readable text.
    pub json: bool,
    /// Project root (absolute path).
    pub root: PathBuf,
    /// Cache directory containing the index files.
    pub cache_dir: PathBuf,
    /// Optional map of allowed file paths to co-change Jaccard strengths (blast-radius
    /// pre-filter).
    ///
    /// AD-409-1: When `Some`, keys are repo-relative paths; values are co-change
    /// Jaccard scores for partner paths, or [`super::temporal::SEED_STRENGTH`] (2.0)
    /// for the blast-radius target itself. The seed's score exceeds the Jaccard maximum
    /// of 1.0, ensuring the target always ranks first in the temporal layer.
    ///
    /// When `Some`, only files whose repo-relative path is in this map are scored. The
    /// filter is applied inside the search engine (before LIMIT) so that the limit
    /// applies to the filtered result set rather than being wasted on files that would
    /// be discarded.
    ///
    /// In the UNION composite path (#200), this map drives the temporal ranked list:
    /// each path's Jaccard score (or the seed sentinel) is the raw temporal score,
    /// merged with the lexical results via weighted RRF (UNION semantics).
    pub blast_radius_paths: Option<BlastRadiusStrengths>,
    /// Optional scored AST results from a structural pattern query (#198).
    ///
    /// When `Some`, carries `Vec<(FileId, f64)>` sorted ASC by FileId (the
    /// frozen Wave-4 contract from #287).  The compound intersector in
    /// `execute_query_with_manifest` uses these scores for weighted-RRF fusion
    /// with the lexical results (replaces the old lossy HashSet gate from #199).
    ///
    /// `None` means "no AST filter" — pure-lexical path (all existing callers
    /// compile unchanged because they initialize this field explicitly).
    pub ast_scored: Option<Vec<(rskim_search::FileId, f64)>>,
    /// Optional composite weights for the weighted-ranking query paths (#200, #377).
    ///
    /// When `Some`, the weighted RRF paths use these ratios instead of the default
    /// six-signal profile.  AD-377-1/AD-377-3 — applied on BOTH composite paths:
    /// - `--blast-radius` UNION re-ranking (`run_blast_radius_composite_query`):
    ///   lexical + ast + temporal all weighted.
    /// - text+`--ast` intersection (`run_compound_query`): lexical + ast weighted;
    ///   the temporal weight is INERT here because `intersect_and_rank` fuses only
    ///   the lexical and ast rank terms.  Supplying a non-zero temporal weight on a
    ///   `--ast` path triggers the temporal-scoped inert-weights stderr notice
    ///   (AD-377-2).
    ///
    /// `None` → use `CompositeWeights6::with_six_signal_defaults()` when composite
    /// ranking is active.  AD-377-4: on the compound path `Some(0,0,0)` is a valid
    /// "no ranking signal" request that returns the full intersection at score 0.0
    /// (FileId-ASC), diverging intentionally from the blast path's empty result.
    pub composite_weights: Option<rskim_search::CompositeWeights6>,
    /// v5 positional: require contiguous ordered phrase (`--phrase`).
    pub phrase: bool,
    /// v5 positional: max word-token distance (`--near N`).
    pub near: Option<u32>,
    /// Optional language filter for `--lang <name>` (e.g. `--lang rust`).
    ///
    /// When `Some`, only files of this language are returned.  Threaded into
    /// every [`rskim_search::SearchQuery`] construction site in `query.rs` so
    /// the reader's `lang_filter` path is exercised on ALL search paths
    /// (exact-symbol, BM25F UNION, positional phrase/near, compound AST, and
    /// blast-radius).  `None` means no language restriction (all files).
    pub lang: Option<rskim_core::Language>,
}

/// A search result with the file path resolved and snippet extracted.
#[derive(Debug, Serialize)]
#[allow(dead_code)]
pub(super) struct ResolvedResult {
    /// Repo-relative path (forward slashes, no leading `.`).
    pub path: String,
    /// Relevance score (higher is better).
    ///
    /// Semantics depend on the active query path:
    /// - **Plain lexical / AST path** (no `--blast-radius`): BM25F magnitude from
    ///   the lexical ranking layer.
    /// - **Composite UNION blast-radius path** (`--blast-radius` with composite
    ///   ranking active, #200): fused weighted-RRF score —
    ///   `Σᵢ wᵢ / (RRF_K + rankᵢ(file))`.  This is a small positive number
    ///   (typically well below 1.0) and is NOT a BM25F magnitude.  Consumers
    ///   reading this field as BM25F on the composite path will silently
    ///   misinterpret it.
    ///
    /// The `field` value `"co_change_partner"` indicates a co-change-only result
    /// whose score is the temporal RRF term alone (no lexical component).
    pub score: f64,
    /// Name of the AST field type (e.g. `"function_signature"`).
    pub field: String,
    /// 1-indexed line number of the primary match within the file.
    pub line_number: Option<u32>,
    /// 1-indexed, exclusive-end line range spanned by all match positions.
    ///
    /// `None` when snippet extraction is unavailable (stale, deleted, or non-UTF8).
    /// Populated from [`rskim_search::compute_line_range`] during snippet extraction.
    /// Serialises as `{"start": N, "end": M}` in `--format json` output.
    pub line_range: Option<Range<usize>>,
    /// Source context window surrounding the match.
    pub snippet: Option<SnippetContext>,
    /// `true` when the file has changed since indexing (mtime mismatch or deleted).
    pub stale: bool,
    /// Byte-position ranges within the file content where query terms appear.
    ///
    /// **Reader-internal — do not use for anchoring (AD-396-7 / #422).**
    /// These are trigram byte positions emitted by the index reader as ranking /
    /// TF signals; they are NOT guaranteed to coincide with full query-token
    /// occurrences.  `line_number` and `line_range` are derived from
    /// `rskim_search::substring_first_anchor` (content-derived, AD-396-1) and
    /// are the authoritative anchor source.  A future #422 pass will populate
    /// this field with semantically-true reader-level positions.
    #[serde(skip)]
    pub match_positions: Vec<Range<usize>>,
    /// Optional temporal data for this file, populated when temporal flags are active.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temporal: Option<TemporalAnnotation>,
    /// Layers that contributed non-zero signal for this file (#201).
    ///
    /// Absent on degraded rows (pure-lexical path without `--ast`), present on
    /// compound paths.  Uses `skip_serializing_if` so existing pure-lexical JSON
    /// consumers see no new key (additive, back-compat with pre-#201 schema).
    ///
    /// `["lexical","ast"]` for text+`--ast` intersection (AC-F6).
    /// `["lexical"]` for pure-lexical or blast-radius paths (no AST layer).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub layers_matched: Vec<&'static str>,
}

/// Output produced by a query execution run.
#[derive(Debug, Serialize)]
pub(super) struct QueryOutput {
    /// The original query text.
    pub query: String,
    /// Total number of results returned (≤ limit).
    pub total: usize,
    /// Sound pagination terminator — true when more results exist beyond the
    /// current page or the candidate pool was capped (AD-404-11 / D-5).
    ///
    /// Replaces the unsound `results.len() < limit` heuristic. Absent when
    /// false (additive, back-compat; `#[serde(skip_serializing_if)]`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub has_more: bool,
    /// AD-403-7: Verification mode applied to candidate files on this query.
    ///
    /// Absent (via `skip_serializing_if`) when the mode is the default Substring
    /// — preserves byte-identical JSON for all callers that do not use positional
    /// flags.  When `--phrase`, `--near N`, or `--phrase --near N` is active,
    /// carries `"phrase"`, `"near"`, or `"phrase_near"` respectively.
    ///
    /// Derived from `verify_mode_for` on the query path.  Informational /
    /// diagnostic — not a search parameter.  D-5 sign-off 2026-07-15.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify_mode: Option<&'static str>,
    /// Resolved and enriched results.
    pub results: Vec<ResolvedResult>,
    /// Wall-clock duration of the query in milliseconds.
    pub duration_ms: u64,
    /// Index statistics (included when available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_stats: Option<rskim_search::IndexStats>,
    /// AST size-coverage for the query invocation (D-5 / AD-405-9).
    ///
    /// Present ONLY on `--ast` paths (standalone and compound).  Absent on
    /// pure-lexical queries (`None` → key omitted from JSON via
    /// `skip_serializing_if`).  When present and not clean, the coverage
    /// object carries the counts and bounded excluded-file sample; when
    /// `is_clean()` the field is set to `None` (omit from JSON entirely).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ast_coverage: Option<rskim_search::AstCoverage>,
    /// AD-414-5 / AD-414-12: degradation signals emitted on this query.
    ///
    /// An array of objects — one per subsystem that could not deliver the
    /// requested ranking.  **Absent from JSON when empty** (additive,
    /// back-compat; `#[serde(skip_serializing_if)]`).  Consumers that check
    /// for the key can detect degraded queries without parsing stderr.
    ///
    /// The array is `pub` so callers outside `query.rs` (e.g. `mod.rs`
    /// enrichment arms) can push signals after `execute_query_with_manifest`
    /// returns.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub degraded: Vec<super::temporal::DegradedJson>,
}

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for an index build run.
#[derive(Debug, Clone)]
pub(super) struct IndexConfig {
    /// The project root to index (absolute, canonical path).
    pub root: PathBuf,
    /// Maximum number of files to index before stopping.
    /// `None` uses the default cap of 50,000.
    pub max_files: Option<usize>,
    /// When `true`, skip the manifest cache and re-classify every file.
    pub force: bool,
    /// Optional override for the cache directory (used in tests).
    /// When `None`, the default `~/.cache/skim/search/<hash>/` is used.
    pub cache_dir_override: Option<PathBuf>,
}

impl IndexConfig {
    /// Default maximum files per index run.
    pub const DEFAULT_MAX_FILES: usize = 50_000;

    /// Returns the effective file cap.
    #[must_use]
    pub fn effective_max_files(&self) -> usize {
        self.max_files.unwrap_or(Self::DEFAULT_MAX_FILES)
    }
}

// ============================================================================
// Results
// ============================================================================

/// Summary statistics produced after an index build completes.
#[derive(Debug)]
pub(super) struct IndexResult {
    /// Number of files successfully indexed.
    pub file_count: u32,
    /// Number of files skipped (unsupported, too large, non-UTF8, etc.).
    pub skipped: u32,
    /// Bounded, stable-key-sorted sample of skip reasons for display
    /// (AD-395-6 / PF-012).  Capped at `MAX_SKIP_REASONS`; sorted by path
    /// string ascending with `CapReached` last.
    pub skip_sample: Vec<SkipReason>,
    /// Number of files whose field_map was reused from the manifest cache
    /// (lexical cache hits).
    pub cache_hits: u32,
    /// Number of files whose AST n-grams were served from `ast_index.skcache`
    /// (AST cache hits — extraction skipped).
    pub ast_cache_hits: u32,
    /// Number of files whose AST n-grams were freshly extracted (AST cache
    /// misses — `derive_ast_entry` was called).
    pub ast_reextracted: u32,
    /// Wall-clock duration of the build.
    pub duration: Duration,
    /// AST size-coverage computed from the manifest after the build (#405).
    ///
    /// Derived from the manifest before `save()` — zero extra I/O (AC-405-12).
    /// Not `Serialize` (in-memory only; the struct is never written as JSON
    /// directly — emission sites use it to compute a notice or pass it to
    /// `build_stats_json` / `format_ast_json`).
    pub ast_coverage: rskim_search::AstCoverage,
}

// ============================================================================
// Skip reasons
// ============================================================================

/// Why a file was excluded from the index.
///
/// The `Display` impl produces a short user-facing one-liner suitable for the
/// stderr sample emitted by `run_build` (AD-395-6).
#[derive(Debug, Clone)]
pub(super) enum SkipReason {
    /// File is larger than the 5 MB threshold.
    TooLarge { path: PathBuf, size: u64 },
    /// File content is not valid UTF-8.
    NonUtf8(PathBuf),
    /// File appears to be minified (both signals required — see AD-395-1):
    /// content.len() >= 64 KiB AND the first 8 KiB probe is effectively
    /// single-line (newline_count <= 1).  Carries the measured avg line bytes
    /// for the user-facing message.
    Minified {
        path: PathBuf,
        avg_line_bytes: usize,
    },
    /// No supported [`rskim_core::Language`] maps to this file's extension.
    UnsupportedLanguage(PathBuf),
    /// I/O error while reading the file.
    ReadError { path: PathBuf, error: String },
    /// File cap reached — no further files will be indexed.
    CapReached,
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkipReason::Minified {
                path,
                avg_line_bytes,
            } => write!(
                f,
                "skipped {}: minified (avg line {avg_line_bytes} > 500 bytes)",
                path.display()
            ),
            SkipReason::NonUtf8(path) => {
                write!(f, "skipped {}: not valid UTF-8", path.display())
            }
            SkipReason::TooLarge { path, size } => {
                write!(f, "skipped {}: too large ({size} bytes)", path.display())
            }
            SkipReason::ReadError { path, error } => {
                write!(f, "skipped {}: read error: {error}", path.display())
            }
            SkipReason::UnsupportedLanguage(path) => {
                write!(f, "skipped {}: unsupported language", path.display())
            }
            SkipReason::CapReached => write!(f, "file cap reached"),
        }
    }
}

impl SkipReason {
    /// Returns the path associated with this skip reason, if any.
    ///
    /// Used for stable-key sorting of the skip sample (AD-395-6 / PF-012).
    /// `CapReached` has no path and sorts last.
    pub(super) fn sort_key(&self) -> Option<&std::path::Path> {
        match self {
            SkipReason::Minified { path, .. }
            | SkipReason::NonUtf8(path)
            | SkipReason::TooLarge { path, .. }
            | SkipReason::ReadError { path, .. }
            | SkipReason::UnsupportedLanguage(path) => Some(path.as_path()),
            SkipReason::CapReached => None,
        }
    }

    /// Discriminant for binary persistence in the manifest skip section.
    ///
    /// Only DETERMINISTIC skips (Minified / NonUtf8 / TooLarge) are persisted;
    /// ReadError and UnsupportedLanguage fall through un-persisted (OD-395-4).
    /// `None` means this skip reason is NOT persisted.
    pub(super) fn persist_discriminant(&self) -> Option<PersistedSkipReason> {
        match self {
            SkipReason::Minified { .. } => Some(PersistedSkipReason::Minified),
            SkipReason::NonUtf8(_) => Some(PersistedSkipReason::NonUtf8),
            SkipReason::TooLarge { .. } => Some(PersistedSkipReason::TooLarge),
            SkipReason::ReadError { .. }
            | SkipReason::UnsupportedLanguage(_)
            | SkipReason::CapReached => None,
        }
    }
}

// ============================================================================
// Persisted skip reason (manifest v5 wire format)
// ============================================================================

/// The set of skip reasons that are durably persisted in the v5 manifest skip
/// section (AD-395-4 / OD-395-4).
///
/// Only DETERMINISTIC reasons are persisted.  `ReadError` and
/// `UnsupportedLanguage` are intentionally absent — they must NOT be persisted
/// because a transiently-unreadable or temporarily-unsupported file would
/// otherwise be excluded from recall until its mtime/size changes.
///
/// `to_u8` / `from_u8` are the single source of truth for the on-disk wire
/// format; `decode_skip_entry` in manifest.rs calls `from_u8` and propagates
/// `None` as a manifest-reject (AD-380-3 / AC-5), catching corrupt/forged bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum PersistedSkipReason {
    Minified = 1,
    NonUtf8 = 2,
    TooLarge = 3,
}

impl PersistedSkipReason {
    /// Encode to the one-byte manifest wire format.
    pub(super) fn to_u8(self) -> u8 {
        self as u8
    }

    /// Decode from the one-byte manifest wire format.
    ///
    /// Returns `None` for any byte outside the known domain `{1, 2, 3}` so
    /// that `decode_skip_entry` can reject corrupt or forged manifests at the
    /// boundary (AD-380-3 / AC-5).  Adding a new variant requires extending
    /// this match — the exhaustive check prevents silent omissions.
    pub(super) fn from_u8(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Minified),
            2 => Some(Self::NonUtf8),
            3 => Some(Self::TooLarge),
            _ => None,
        }
    }

    /// Human-readable label used by `--stats` text and JSON output.
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Minified => "minified",
            Self::NonUtf8 => "non_utf8",
            Self::TooLarge => "too_large",
        }
    }
}

// ============================================================================
// Persisted skip entry (manifest v5)
// ============================================================================

/// A content-skipped file persisted in the v5 manifest skip section (AD-395-4).
///
/// Only DETERMINISTIC skips (Minified / NonUtf8 / TooLarge) are stored here;
/// transient ReadErrors fall through un-persisted (OD-395-4). The persisted
/// `(path, mtime, size, reason)` tuple is used by `scan_working_tree` to
/// reconcile walked files against the skip-set without re-reading content
/// (AD-395-5 loop-killer; respects AD-379/AC15 metadata-only invariant).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SkippedEntry {
    /// Repo-relative normalized path (same normalization as manifest keys).
    pub path: String,
    /// File modification time at skip time. `None` when the platform does not
    /// expose mtime.
    pub mtime: Option<u64>,
    /// File size in bytes at skip time. `None` when the platform does not
    /// expose size.
    pub size: Option<u64>,
    /// Typed skip reason.  Use [`PersistedSkipReason`] methods for wire
    /// encoding and label output instead of matching on raw integers.
    pub reason: PersistedSkipReason,
}

impl SkippedEntry {
    /// Reason label for `--stats` text and JSON output, delegated to
    /// [`PersistedSkipReason::label`] — single source of truth.
    pub(super) fn reason_label(&self) -> &'static str {
        self.reason.label()
    }
}

// ============================================================================
// Producer skip aggregate returned via JoinHandle (AD-395-2)
// ============================================================================

/// Aggregates collected by the producer thread and returned via `JoinHandle`
/// (single-threaded producer needs no Mutex — AD-395-2).
pub(super) struct ProducerSkips {
    /// Exact, uncapped count of every producer-phase skip (AD-395-2).
    ///
    /// Incremented once per `Err(reason)` in the producer loop, independent
    /// of `sample` (which is capped at `MAX_SKIP_REASONS`).  Use this for
    /// `IndexResult.skipped` and the "...and N more" arithmetic so that a
    /// repo with >10 000 content-skipped files still reports the true total
    /// rather than the bounded sample length.
    pub skipped_total: usize,
    /// Bounded display sample (capped at `MAX_SKIP_REASONS` from walk.rs).
    pub sample: Vec<SkipReason>,
    /// Full set of DETERMINISTIC content-skips for manifest persistence.
    ///
    /// Only Minified / NonUtf8 / TooLarge entries appear here (OD-395-4).
    /// Capped at `MAX_MANIFEST_ENTRIES` (naturally <= max_files since every
    /// skip was a previously accepted WalkEntry).
    pub skip_set: Vec<SkippedEntry>,
}

// ============================================================================
// Streaming pipeline types
// ============================================================================

/// A directory entry produced by [`super::walk::walk_metadata`].
///
/// Contains only metadata — no file content. The streaming producer reads
/// content on demand, decoupling the walk from the read phase.
#[derive(Debug)]
pub(super) struct WalkEntry {
    /// Absolute path to the file.
    pub abs_path: PathBuf,
    /// Path relative to the project root.
    pub rel_path: PathBuf,
    /// Detected source language.
    pub lang: rskim_core::Language,
    /// File modification time as seconds since UNIX_EPOCH.
    ///
    /// `None` when the platform does not expose mtime or the syscall fails.
    pub mtime: Option<u64>,
    /// File size in bytes captured from the walker's metadata.
    ///
    /// `None` when the platform does not expose size or the syscall fails.
    /// Recorded in the manifest (AD-379-2) so working-tree staleness can compare
    /// both mtime AND size against the current on-disk file.
    pub size: Option<u64>,
}

/// A fully processed file ready for indexing, produced by the streaming producer.
///
/// Content is held here until the consumer calls `add_file_classified` and then
/// drops it — limiting peak memory to (channel capacity × average file size).
#[derive(Debug)]
pub(super) struct ProcessedFile {
    /// Path relative to the project root (used as the manifest key).
    pub rel_path: PathBuf,
    /// Detected source language.
    pub lang: rskim_core::Language,
    /// Full file content as UTF-8.
    pub content: String,
    /// Hex-encoded SHA-256 of `content` (64 lowercase hex chars).
    pub sha256: String,
    /// File modification time forwarded from [`WalkEntry`].
    pub mtime: Option<u64>,
    /// File size in bytes forwarded from [`WalkEntry`] (AD-379-2).
    pub size: Option<u64>,
    /// Pre-computed or cache-reused field map.
    pub field_map: Vec<(Range<usize>, SearchField)>,
    /// `true` when field_map was reused from the manifest cache (no classify call).
    pub cache_hit: bool,
    /// Cached AST n-gram payload from `ast_index.skcache`, when the file's
    /// content SHA matched a prior build's entry.
    ///
    /// `Some(entry)` → consumer uses payload directly (no `derive_ast_entry` call).
    /// `None`         → consumer calls `derive_ast_entry` and records the result.
    ///
    /// A `Some` here DOES NOT imply `cache_hit == true`: if the lexical field_map
    /// was a miss but the AST payload was already cached from a different build
    /// path, both are tracked independently.
    pub ast_cached: Option<rskim_search::CachedAstEntry>,
}

// ============================================================================
// Per-file read result (retained for tests via walk_and_read)
// ============================================================================

/// A successfully read file — produced by the test-only [`super::walk::walk_and_read`].
///
/// In production the streaming pipeline uses [`WalkEntry`] + [`ProcessedFile`]
/// instead. This type is kept for the walk unit tests which exercise the
/// integrated walk-and-read code path.
#[cfg(test)]
#[derive(Debug)]
pub(super) struct ReadFile {
    /// Path relative to the project root.
    pub rel_path: PathBuf,
    /// Detected source language.
    pub lang: rskim_core::Language,
    /// Full file content as UTF-8 string.
    pub content: String,
    /// File modification time as seconds since UNIX_EPOCH.
    ///
    /// `None` when the platform does not expose mtime or the syscall fails.
    /// Only used as a fast pre-screening hint; SHA-256 remains the correctness
    /// guarantee for cache invalidation.
    pub mtime: Option<u64>,
}

// ============================================================================
// Tests (co-located in types_tests.rs)
// ============================================================================

#[cfg(test)]
#[path = "types_tests.rs"]
mod tests;
