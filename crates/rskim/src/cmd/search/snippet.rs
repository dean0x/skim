//! Snippet extraction — pull source context around a match position.
//!
//! # Design
//!
//! - Pure file I/O: open the file, compute the line from a byte offset, extract
//!   N lines of context.
//! - Mtime guard: if the manifest entry records an mtime and the file's mtime
//!   differs, the file has changed since indexing — return `None` (stale).
//! - Error-tolerant: deleted or unreadable files return `None` rather than
//!   propagating errors.
//! - No allocation of the entire file when not needed: we read the file once and
//!   work with the string content directly.

use std::ops::Range;
use std::path::Path;

use super::manifest::ManifestEntry;
use super::types::{SnippetContext, SnippetLine};

/// Default number of context lines above and below the match.
pub(super) const DEFAULT_CONTEXT: u32 = 3;

/// Files larger than this byte limit are not fully read for snippet extraction;
/// instead only the first `MAX_VERIFY_SCAN_BYTES` are read for verification.
const MAX_SNIPPET_FILE_BYTES: u64 = 5 * 1024 * 1024;

/// Maximum bytes to read when verifying a large file (> `MAX_SNIPPET_FILE_BYTES`).
///
/// Matches `MAX_SNIPPET_FILE_BYTES` so the two bounds are consistently defined
/// in one place.  A genuine query match starting after this offset will produce
/// a false-negative verification — accepted trade-off documented in the function
/// body.
const MAX_VERIFY_SCAN_BYTES: usize = 5 * 1024 * 1024;

/// How to verify a candidate file (AD-393-5).
///
/// The reader is a recall-oriented candidate generator; the CLI predicate is the
/// correctness authority (AD-393-1). `VerifyMode` threads the correct predicate
/// through the single file-read in `extract_snippet_and_verify` so no second I/O
/// is needed.
///
/// - `Substring`: the pre-#393 default — any whitespace-delimited token must
///   appear as a substring of the file content.
/// - `Phrase`: all query words must appear in exact contiguous order as word-tokens
///   (uses `rskim_search::phrase_tokens_present`).
/// - `Near(n)`: all query words must appear within `n` word-token positions of each
///   other, in any order (uses `rskim_search::near_tokens_present`).
#[derive(Debug, Clone, Copy)]
pub(super) enum VerifyMode {
    /// Substring (trigram-intersection) verification — default pre-#393 mode.
    Substring,
    /// Exact phrase: words in contiguous order, no gaps (AD-393-3).
    Phrase,
    /// Proximity: all words within `n` word-token positions (AD-393-4).
    Near(u32),
}

/// Outcome of attempting to extract a snippet.
#[derive(Debug)]
pub(super) enum SnippetOutcome {
    /// Successfully extracted a snippet.
    ///
    /// - `match_line`: 1-indexed line number of the content-derived anchor
    ///   (as `u32` for display formatting).
    /// - `line_range`: for Substring / Phrase / Near paths with a content-derived
    ///   anchor (AD-396-1 / AD-393-6), this is the single anchor line
    ///   `{match_line, match_line+1}` — not a multi-position span.
    ///   For the `extract_snippet` test-sentinel path (empty query), this falls
    ///   back to the span of all `match_positions` for backwards-compat.
    /// - `context`: surrounding source lines.
    Ok {
        match_line: u32,
        line_range: std::ops::Range<usize>,
        context: SnippetContext,
    },
    /// File has changed since indexing (mtime mismatch) — positions may be stale.
    Stale,
    /// File deleted, unreadable, empty positions, or non-UTF8.
    Unavailable,
}

// ============================================================================
// Context window extraction
// ============================================================================

/// Extract a context window of `context` lines above and below `match_line`.
///
/// `match_line` is 1-indexed. The window is clamped to the file boundaries
/// (no negative line numbers, no lines past EOF).
///
/// The match line has `is_match = true`; all other lines have `is_match = false`.
pub(super) fn extract_context_window(
    content: &str,
    match_line: u32,
    context: u32,
) -> Vec<SnippetLine> {
    let line_count = content.lines().count();
    let total_lines = u32::try_from(line_count).unwrap_or(u32::MAX);

    if total_lines == 0 {
        return Vec::new();
    }

    // Clamp to [1, total_lines]
    let match_line = match_line.max(1).min(total_lines);

    let start = match_line.saturating_sub(context).max(1);
    let end = match_line.saturating_add(context).min(total_lines);

    // Collect only the window lines — skip lines before the window, take only
    // what is needed, avoiding a full-file allocation for large files.
    let skip = (start - 1) as usize;
    let take = (end - start + 1) as usize;
    content
        .lines()
        .enumerate()
        .skip(skip)
        .take(take)
        .map(|(idx, line_text)| {
            let ln = (idx + 1) as u32;
            SnippetLine {
                line_number: ln,
                content: line_text.to_string(),
                is_match: ln == match_line,
            }
        })
        .collect()
}

// ============================================================================
// Full snippet extraction
// ============================================================================

/// Extract a snippet for a search result.
///
/// Returns:
/// - `SnippetOutcome::Ok(line, line_range, ctx)` on success.
/// - `SnippetOutcome::Stale` when the file's mtime differs from manifest (changed since indexing).
/// - `SnippetOutcome::Unavailable` when positions are empty, file is deleted/unreadable, or non-UTF8.
///
/// Production paths use [`extract_snippet_and_verify`] to read the file once
/// and check substring membership simultaneously.  This fn is kept for testing
/// the snippet-extraction logic in isolation.  It delegates to
/// [`extract_snippet_and_verify`] with an empty sentinel query (`""`) whose
/// verification result is discarded — the file is read exactly once through the
/// shared stat/mtime/size/read/decode pipeline (no duplication; DRY, AD-355-1).
/// The empty sentinel is safe because `_verified` is ignored: per
/// `query_substring_present`, an empty query returns `false` (see types.rs unit
/// tests), but since this fn discards the flag the behaviour is unchanged.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn extract_snippet(
    root: &Path,
    rel_path: &str,
    match_positions: &[Range<usize>],
    manifest_entry: Option<&ManifestEntry>,
) -> SnippetOutcome {
    if match_positions.is_empty() {
        return SnippetOutcome::Unavailable;
    }
    // Delegate to extract_snippet_and_verify with an empty sentinel query.
    // `_verified` is discarded — this function is only used for tests that
    // exercise the snippet-extraction path in isolation, not the verify gate.
    // The sentinel "" is safe because `_verified` is unused: per AD-355-1
    // `query_substring_present("", _) == false`, but since this fn ignores the
    // verified flag, that has no effect.  Single shared read/stat/decode path
    // (AD-355-1: no second I/O, no copy-paste of the pipeline).
    let (outcome, _verified) = extract_snippet_and_verify(
        root,
        rel_path,
        match_positions,
        manifest_entry,
        "",
        VerifyMode::Substring,
    );
    outcome
}

// ============================================================================
// Exact-match verification (AD-355-1)
// ============================================================================

/// Extract a snippet and simultaneously verify that `query` is present in the
/// file content — reading the file exactly once (no second I/O).
///
/// Returns the normal [`SnippetOutcome`] PLUS a boolean:
/// - `true`  — the file content passes the predicate selected by `verify_mode`.
/// - `false` — the file was not read (Stale / Unavailable) or the predicate
///   failed.  The caller should drop this candidate from the verified result set.
///
/// # Design (AD-355-1 / AD-393-5 / AD-396-1)
///
/// Verification is co-located with snippet extraction so the file bytes are
/// read only once. `verify_mode` selects the correctness predicate:
/// - `Substring` → `rskim_search::substring_first_anchor` (AD-396-1: content-
///   derived anchor replacing the prior trigram-position fallback; returns both
///   the verified flag and the anchor range for re-anchoring, AD-393-6)
/// - `Phrase`    → `rskim_search::phrase_tokens_present` (exact ordered tokens)
/// - `Near(n)`   → `rskim_search::near_tokens_present` (within n word-token positions)
///
/// For Phrase/Near/Substring, if the predicate returns `Some(range)`, the snippet
/// is re-anchored to `range.start` instead of the approximate trigram-containment
/// `match_positions[0].start` (AD-393-6 / AD-396-1).
///
/// # AD-396-2 — Tiered anchor-selection rule (Substring path)
///
/// The Substring anchor is computed by `rskim_search::substring_first_anchor`,
/// which implements a two-tier policy:
/// - **Tier 1**: earliest line containing ALL query tokens simultaneously
///   (grep-AND parity, strongest semantic anchor for agent consumers).
/// - **Tier 2** (no Tier-1 line): first occurrence of the RAREST token
///   (highest max-trigram IDF weight from `TRIGRAM_WEIGHTS`; tie-breaks:
///   longest token, then earliest occurrence).
pub(super) fn extract_snippet_and_verify(
    root: &Path,
    rel_path: &str,
    match_positions: &[Range<usize>],
    manifest_entry: Option<&ManifestEntry>,
    query: &str,
    verify_mode: VerifyMode,
) -> (SnippetOutcome, bool) {
    // AD-355-7: empty match_positions is valid for short-query fallback candidates
    // (1–2 byte queries that cannot produce trigrams).  In that case the ngram
    // reader returns all indexed files with empty positions; we still need to read
    // the file and run the predicate to decide whether to keep the result.
    // We skip the early-return here and fall through to the I/O+verify path.
    // For the normal (ngram-scored) path, positions are non-empty and the snippet
    // extraction below will succeed as before.
    //
    // D13 (AD-393-1): the same applies for Phrase/Near with all-short words
    // (search_positional short_query_fallback also emits empty positions).

    let abs_path = root.join(rel_path);

    // Single stat(2) call shared by both the mtime guard and the size guard below.
    let meta = std::fs::metadata(&abs_path).ok();

    // Mtime guard.
    if let Some(stored_mtime) = manifest_entry.and_then(|e| e.mtime) {
        let current_mtime = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        if current_mtime != Some(stored_mtime) {
            // Stale: positions may be wrong; cannot verify. Drop from verified set.
            return (SnippetOutcome::Stale, false);
        }
    }

    // Size guard.
    //
    // Files exceeding MAX_SNIPPET_FILE_BYTES cannot produce a snippet (the context
    // window would allocate the entire file).  However, we MUST NOT conflate
    // "too large to snippet" with "failed verification" — a large UTF-8 source file
    // that genuinely CONTAINS the query must survive as a snippet-less result
    // (AD-355-4: large-file verify path, verified in #355 cycle-2).
    //
    // For large files we do a bounded verification read: read at most
    // MAX_VERIFY_SCAN_BYTES of the file and run the predicate on that prefix.
    // This preserves pre-#355 behaviour (large files were returned snippet-less;
    // verification is new but correct) while keeping the I/O cost bounded.  A
    // query match that spans byte offset >MAX_VERIFY_SCAN_BYTES will produce a
    // false negative (file dropped from results) — accepted trade-off documented
    // here (AD-393-10: same bounded-scan applies to Phrase/Near predicates).
    let file_size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    if file_size > MAX_SNIPPET_FILE_BYTES {
        // Bounded verification read for large files.
        //
        // Fixes (F1/security + F6/performance): the previous code allocated a
        // 5 MiB zero-filled buffer unconditionally and then overwrote it with a
        // single `f.read(&mut buf)` call, which is permitted to return fewer bytes
        // than requested — producing nondeterministic scan windows that can miss
        // genuine matches.
        //
        // Instead:
        // (a) Size the buffer to `min(file_size, MAX_VERIFY_SCAN_BYTES)` to avoid
        //     the full 5 MiB alloc+memset for files just over the 5 MiB threshold.
        // (b) Use `Read::take(...).read_to_end(&mut buf)` which drains the full
        //     intended prefix (up to the cap) in a loop, giving deterministic
        //     behaviour.
        use std::io::Read;
        let needed = (file_size as usize).min(MAX_VERIFY_SCAN_BYTES);
        let mut buf = Vec::with_capacity(needed);
        let ok = std::fs::File::open(&abs_path)
            .ok()
            .and_then(|f| f.take(needed as u64).read_to_end(&mut buf).ok())
            .is_some();
        // AD-393-10: dispatch the correct predicate for large-file bounded scan.
        let verified = if ok {
            std::str::from_utf8(&buf)
                .map(|text| run_verify_predicate(text, query, &verify_mode))
                .unwrap_or(false)
        } else {
            false
        };
        return (SnippetOutcome::Unavailable, verified);
    }

    // Read file content — single I/O operation shared by snippet extraction
    // and verification (AD-355-1: no second file read).
    let content = match std::fs::read(&abs_path) {
        Ok(c) => c,
        Err(_) => return (SnippetOutcome::Unavailable, false),
    };
    let text = match std::str::from_utf8(&content) {
        Ok(t) => t,
        Err(_) => return (SnippetOutcome::Unavailable, false),
    };

    // AD-393-5 / AD-396-1: Dispatch the correct predicate and capture the
    // anchor range for re-anchoring (AD-393-6 / AD-396-2).
    let (verified, anchor_range): (bool, Option<Range<usize>>) =
        run_verify_predicate_with_range(text, query, &verify_mode);

    // AD-396-5: Short-query Substring scope boundary.
    // For Substring mode with empty match_positions (the <3-byte fallback,
    // AD-355-7 / AD-372-4), null out anchor_range so the guard below returns
    // Unavailable.  This preserves current short-query behaviour (verified but
    // snippet-less) while `verified` still gates inclusion.  Only the Substring
    // path is affected — Phrase/Near short-word fallback keeps its predicate
    // anchor to avoid a #393 regression.
    let anchor_range = if matches!(verify_mode, VerifyMode::Substring) && match_positions.is_empty()
    {
        None
    } else {
        anchor_range
    };

    // AD-355-7 / D13: short-query fallback candidates (and all-short Phrase/Near
    // fallback) arrive with empty positions. Cannot compute a meaningful snippet
    // without a byte offset; return Unavailable. `verified` still gates inclusion.
    if match_positions.is_empty() && anchor_range.is_none() {
        return (SnippetOutcome::Unavailable, verified);
    }

    // AD-393-6 / AD-396-1: Re-anchor the snippet from the predicate's returned
    // exact occurrence range. For all verify modes (Substring via AD-396-1,
    // Phrase/Near via AD-393-6), anchor_range now carries the content-derived
    // anchor; match_positions is the fallback only when anchor_range is None.
    //
    // NOTE: The CLI no longer treats reader-emitted trigram positions
    // (match_positions) as query-match locations for anchoring.  They remain
    // reader-internal ranking/TF signals only.  Anchor provenance is now always
    // content-derived (file bytes already read once, AD-355-1).
    let anchor_start = anchor_range
        .as_ref()
        .map(|r| r.start)
        .or_else(|| match_positions.first().map(|r| r.start));

    let Some(anchor_start) = anchor_start else {
        // Neither predicate range nor match_positions — can't place a snippet.
        return (SnippetOutcome::Unavailable, verified);
    };

    let match_line = rskim_search::byte_offset_to_line(&content, anchor_start) as u32;

    // AD-396-6: dev-time invariant — when verified, the anchor line must contain
    // ≥1 query token. `!verified` short-circuits for the extract_snippet sentinel
    // (query="", verified=false) and any non-verified path. When verified=true,
    // all three predicates return None/false for empty queries, so the inner block
    // is always reached and the ADR-007 invariant is checked. Compiled out of
    // --release; the test suite is the production correctness gate (PF-007).
    debug_assert!(
        !verified || {
            let idx = (match_line as usize).saturating_sub(1);
            let anchor_line = text.lines().nth(idx).unwrap_or("");
            query
                .split_whitespace()
                .any(|tok| anchor_line.contains(tok))
        },
        "AD-396-6: anchor line {} contains no query token from {:?}",
        match_line,
        query
    );

    let line_range = if let Some(ref ar) = anchor_range {
        // AD-393-6 / AD-396-2: compute line_range from the content-derived anchor
        // range — single anchor line {n, n+1} (AD-393-6 precedent).
        rskim_search::compute_line_range(&content, std::slice::from_ref(ar))
    } else {
        rskim_search::compute_line_range(&content, match_positions)
    };

    let ctx_lines = extract_context_window(text, match_line, DEFAULT_CONTEXT);

    if ctx_lines.is_empty() {
        return (SnippetOutcome::Unavailable, verified);
    }

    (
        SnippetOutcome::Ok {
            match_line,
            line_range,
            context: SnippetContext { lines: ctx_lines },
        },
        verified,
    )
}

// ============================================================================
// Internal predicate helpers (AD-393-5)
// ============================================================================

/// Run the verify predicate for `mode` and return a `bool`. Used by the
/// large-file bounded-scan path where re-anchoring is not needed (no snippet).
///
/// For `Substring`, calls `rskim_search::query_substring_present` directly
/// (single-pass boolean) rather than the full `substring_first_anchor` which
/// computes a tiered anchor only to have it discarded by the large-file caller.
/// For `Phrase`/`Near`, delegates to `run_verify_predicate_with_range` (they are
/// already single-pass and share the same code path as the snippet branch).
fn run_verify_predicate(text: &str, query: &str, mode: &VerifyMode) -> bool {
    match mode {
        VerifyMode::Substring => rskim_search::query_substring_present(text, query),
        _ => run_verify_predicate_with_range(text, query, mode).0,
    }
}

/// Run the verify predicate and return `(verified, anchor_range)`.
///
/// For all modes, the anchor range is the content-derived byte span of the first
/// match — used to re-anchor the snippet (AD-393-6 / AD-396-1):
/// - `Substring` → `rskim_search::substring_first_anchor` (AD-396-1): returns
///   the content-derived anchor for the tiered policy (AD-396-2); `is_some()`
///   is logically equivalent to `query_substring_present` (AD-396-3).
/// - `Phrase`    → `rskim_search::phrase_tokens_present` (exact ordered tokens).
/// - `Near(n)`   → `rskim_search::near_tokens_present` (within n positions).
fn run_verify_predicate_with_range(
    text: &str,
    query: &str,
    mode: &VerifyMode,
) -> (bool, Option<Range<usize>>) {
    match mode {
        VerifyMode::Substring => {
            // AD-396-1: content-derived anchor for Substring path.
            // AD-396-2: tiered anchor-selection rule — see substring_first_anchor
            // in rskim-search/src/types.rs for the full Tier 1 / Tier 2 policy.
            // AD-396-3: is_some() == query_substring_present() (equivalence gate).
            let anchor = rskim_search::substring_first_anchor(text, query);
            (anchor.is_some(), anchor)
        }
        VerifyMode::Phrase => {
            let opt = rskim_search::phrase_tokens_present(text, query);
            (opt.is_some(), opt)
        }
        VerifyMode::Near(n) => {
            let opt = rskim_search::near_tokens_present(text, query, *n);
            (opt.is_some(), opt)
        }
    }
}

// ============================================================================
// Tests (co-located in snippet_tests.rs)
// ============================================================================

#[cfg(test)]
#[path = "snippet_tests.rs"]
mod tests;
