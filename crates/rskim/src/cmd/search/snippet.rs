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
#[derive(Debug, Clone)]
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
    /// - `match_line`: 1-indexed line number of the **first** match position
    ///   (as `u32` for display formatting).
    /// - `line_range`: 1-indexed exclusive-end range spanning **all** match
    ///   positions (may differ from `match_line` when the first position is not
    ///   the minimum-line position across all positions).
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

/// Return `true` iff every whitespace-delimited token in `query` appears as
/// a case-sensitive substring of `content`.
///
/// # Design (AD-355-1)
///
/// This is a thin re-export of [`rskim_search::query_substring_present`], which
/// is defined in `rskim-search/src/types.rs` so that the rskim-bench harness can
/// use the **same predicate** to filter raw `reader.search()` output.  Both the
/// CLI verify gate (`extract_snippet_and_verify`) and the bench AC1/AC4 guard
/// measure over the identical verified surface — ensuring bench precision metrics
/// reflect the same correctness criterion users see.
///
/// # Match semantics (AD-355-3)
///
/// - **Case-sensitive**: code identifiers are case-sensitive; defaulting to
///   case-sensitive prevents false positives (e.g. `Foo` matching `foo`).
/// - **AND-of-whitespace-tokens**: a multi-word query `"foo bar"` requires both
///   `"foo"` and `"bar"` to appear as substrings somewhere in the file.  This
///   matches code-search ergonomics where users expect conjunctive multi-word
///   queries.
/// - **Short queries (< 3 bytes)**: the trigram reader returns all indexed files as
///   score-0 candidates for queries shorter than 3 bytes (`extract_query_ngrams`
///   returns `[]` for `len < 3`; see AD-355-7).  Verification is the only
///   correctness gate in that path — this fn filters those candidates down to
///   files that actually contain the literal query string.
///
/// This fn is pure (no I/O, no side effects) — see `snippet_tests.rs` for
/// unit tests and `rskim-search/src/types.rs` for the canonical definition.
pub(super) fn query_substring_present(content: &str, query: &str) -> bool {
    rskim_search::query_substring_present(content, query)
}

/// Extract a snippet and simultaneously verify that `query` is present in the
/// file content — reading the file exactly once (no second I/O).
///
/// Returns the normal [`SnippetOutcome`] PLUS a boolean:
/// - `true`  — the file content passes the predicate selected by `verify_mode`.
/// - `false` — the file was not read (Stale / Unavailable) or the predicate
///   failed.  The caller should drop this candidate from the verified result set.
///
/// # Design (AD-355-1 / AD-393-5)
///
/// Verification is co-located with snippet extraction so the file bytes are
/// read only once. `verify_mode` selects the correctness predicate:
/// - `Substring` → `query_substring_present` (pre-#393 default)
/// - `Phrase`    → `rskim_search::phrase_tokens_present` (exact ordered tokens)
/// - `Near(n)`   → `rskim_search::near_tokens_present` (within n word-token positions)
///
/// In Phrase/Near mode, if the predicate returns `Some(range)`, the snippet
/// is re-anchored to the first byte of `range` instead of `match_positions[0].start`
/// (AD-393-6: trigram-containment positions are approximate; predicate-returned
/// range is exact).
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

    // AD-393-5: Dispatch the correct predicate and capture the anchor range
    // for Phrase/Near re-anchoring (AD-393-6).
    let (verified, anchor_range): (bool, Option<Range<usize>>) =
        run_verify_predicate_with_range(text, query, &verify_mode);

    // AD-355-7 / D13: short-query fallback candidates (and all-short Phrase/Near
    // fallback) arrive with empty positions. Cannot compute a meaningful snippet
    // without a byte offset; return Unavailable. `verified` still gates inclusion.
    if match_positions.is_empty() && anchor_range.is_none() {
        return (SnippetOutcome::Unavailable, verified);
    }

    // AD-393-6: In Phrase/Near mode, re-anchor the snippet from the predicate's
    // returned exact occurrence range, not the approximate trigram-containment
    // `match_positions[0].start`. For Substring mode (anchor_range=None) or when
    // positions are available, fall back to match_positions[0].start.
    let anchor_start = anchor_range
        .as_ref()
        .map(|r| r.start)
        .or_else(|| match_positions.first().map(|r| r.start));

    let Some(anchor_start) = anchor_start else {
        // Neither predicate range nor match_positions — can't place a snippet.
        return (SnippetOutcome::Unavailable, verified);
    };

    let match_line = rskim_search::byte_offset_to_line(&content, anchor_start) as u32;

    let line_range = if let Some(ref ar) = anchor_range {
        // AD-393-6: compute line_range from the exact predicate occurrence range.
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
fn run_verify_predicate(text: &str, query: &str, mode: &VerifyMode) -> bool {
    match mode {
        VerifyMode::Substring => query_substring_present(text, query),
        VerifyMode::Phrase => rskim_search::phrase_tokens_present(text, query).is_some(),
        VerifyMode::Near(n) => rskim_search::near_tokens_present(text, query, *n).is_some(),
    }
}

/// Run the verify predicate and return `(verified, anchor_range)`.
///
/// For `Phrase` / `Near`, the anchor range is the exact byte span of the match
/// returned by the predicate — used to re-anchor the snippet (AD-393-6).
/// For `Substring` the range is always `None` (no position information available).
fn run_verify_predicate_with_range(
    text: &str,
    query: &str,
    mode: &VerifyMode,
) -> (bool, Option<Range<usize>>) {
    match mode {
        VerifyMode::Substring => (query_substring_present(text, query), None),
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
