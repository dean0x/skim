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
/// [`extract_snippet_and_verify`] with a sentinel query that is always present,
/// so there is a single read/guard/decode path (no duplication of the stat/mtime/
/// size/read pipeline).
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
    // The sentinel "" is safe here because `_verified` is unused: the defense-
    // in-depth change in `query_substring_present` (Finding 15) means "" now
    // returns false, but since this function ignores verified, behavior is
    // unchanged.  This eliminates the previous copy-paste of the
    // stat/mtime/size/read/decode pipeline (Finding 10 / DRY fix).
    let (outcome, _verified) =
        extract_snippet_and_verify(root, rel_path, match_positions, manifest_entry, "");
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
/// Exact-match verification lives here — not in the core reader — because:
/// - The `NgramIndexReader` / `QueryEngine` pipeline has **no access to file
///   content**; it operates only on byte-offset postings from the inverted index.
/// - `extract_snippet` is the **only call site** where file bytes already exist
///   at query time.  Verification piggy-backs on that existing read at zero
///   additional I/O cost.
/// - This makes the predicate a **candidate-then-verify** gate: the trigram index
///   generates candidates cheaply; this fn drops non-matching ones.
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
/// This fn is pure (no I/O, no side effects) so it can be unit-tested in
/// isolation — see `snippet_tests.rs`.
pub(super) fn query_substring_present(content: &str, query: &str) -> bool {
    // Split on whitespace; require every non-empty token to appear in content.
    //
    // Defense-in-depth (Finding 15 / #355 cycle-2): if the token set is empty
    // (empty query OR whitespace-only query), `.all()` would return vacuously true
    // and let all candidates through.  To guard against a future caller that skips
    // the is_empty()/trim() guards, we make the empty-token case explicit here:
    // an empty/whitespace-only query is treated as "no match" (false).
    //
    // The CLI dispatch (mod.rs:97) already rejects whitespace-only queries before
    // reaching this function, and execute_query_with_manifest short-circuits on
    // empty queries.  This local guard is an additional safety net so the predicate's
    // safety property is self-contained — removing a caller guard cannot silently
    // re-open the bug.
    let mut tokens = query.split_whitespace().peekable();
    if tokens.peek().is_none() {
        // Empty or whitespace-only query: treat as "not present" (no content
        // matches a non-existent query term).
        return false;
    }
    tokens.all(|token| content.contains(token))
}

/// Extract a snippet and simultaneously verify that `query` is present in the
/// file content — reading the file exactly once (no second I/O).
///
/// Returns the normal [`SnippetOutcome`] PLUS a boolean:
/// - `true`  — the file content passes `query_substring_present(text, query)`.
/// - `false` — the file was not read (Stale / Unavailable) or the query is
///   absent.  The caller should drop this candidate from the verified
///   result set.
///
/// # Design (AD-355-1)
///
/// Verification is co-located with snippet extraction so the file bytes are
/// read only once.  The `query_substring_present` fn is the pure predicate; this
/// wrapper applies it at the single on-disk-read call site.
pub(super) fn extract_snippet_and_verify(
    root: &Path,
    rel_path: &str,
    match_positions: &[Range<usize>],
    manifest_entry: Option<&ManifestEntry>,
    query: &str,
) -> (SnippetOutcome, bool) {
    // AD-355-7: empty match_positions is valid for short-query fallback candidates
    // (1–2 byte queries that cannot produce trigrams).  In that case the ngram
    // reader returns all indexed files with empty positions; we still need to read
    // the file and run query_substring_present to decide whether to keep the result.
    // We skip the early-return here and fall through to the I/O+verify path.
    // For the normal (ngram-scored) path, positions are non-empty and the snippet
    // extraction below will succeed as before.
    //
    // When positions ARE empty we return SnippetOutcome::Unavailable (no context
    // window can be computed without a position), but verified may be true or false
    // depending on whether the file contains the literal query string.

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
    // (Finding 20 / #355 cycle-2).
    //
    // For large files we do a bounded verification read: read at most
    // MAX_VERIFY_SCAN_BYTES of the file and run query_substring_present on that
    // prefix.  This preserves pre-#355 behaviour (large files were returned
    // snippet-less; verification is new but correct) while keeping the I/O
    // cost bounded.  A query match that spans byte offset >MAX_VERIFY_SCAN_BYTES
    // will produce a false negative (file dropped from results) — that is the
    // accepted trade-off for files well beyond the size limit.
    const MAX_SNIPPET_FILE_BYTES: u64 = 5 * 1024 * 1024;
    const MAX_VERIFY_SCAN_BYTES: usize = 5 * 1024 * 1024; // same bound as snippet limit
    let file_size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    if file_size > MAX_SNIPPET_FILE_BYTES {
        use std::io::Read;
        let mut buf = vec![0u8; MAX_VERIFY_SCAN_BYTES];
        let n = std::fs::File::open(&abs_path)
            .ok()
            .and_then(|mut f| f.read(&mut buf).ok())
            .unwrap_or(0);
        let prefix = &buf[..n];
        let verified = std::str::from_utf8(prefix)
            .map(|text| query_substring_present(text, query))
            .unwrap_or(false);
        return (SnippetOutcome::Unavailable, verified);
    }

    // Read file content — single I/O operation shared by snippet extraction
    // and substring verification (AD-355-1: no second file read).
    let content = match std::fs::read(&abs_path) {
        Ok(c) => c,
        Err(_) => return (SnippetOutcome::Unavailable, false),
    };
    let text = match std::str::from_utf8(&content) {
        Ok(t) => t,
        Err(_) => return (SnippetOutcome::Unavailable, false),
    };

    // Substring verification — pure, no I/O (AD-355-1 / AD-355-3).
    let verified = query_substring_present(text, query);

    // AD-355-7: short-query fallback candidates arrive with no positions.  We
    // cannot compute a meaningful snippet without a byte offset, so return
    // Unavailable (snippet will be None in the result).  The `verified` flag
    // still controls whether the candidate survives the relevance gate.
    if match_positions.is_empty() {
        return (SnippetOutcome::Unavailable, verified);
    }

    let match_line = rskim_search::byte_offset_to_line(&content, match_positions[0].start) as u32;
    let line_range = rskim_search::compute_line_range(&content, match_positions);
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
// Tests (co-located in snippet_tests.rs)
// ============================================================================

#[cfg(test)]
#[path = "snippet_tests.rs"]
mod tests;
