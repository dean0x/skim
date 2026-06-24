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
/// the snippet-extraction logic in isolation.
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

    let abs_path = root.join(rel_path);

    // Single stat(2) call shared by both the mtime guard and the size guard below.
    let meta = std::fs::metadata(&abs_path).ok();

    // Mtime guard: if the manifest recorded an mtime and it doesn't match
    // the file's current mtime, the file has changed — positions are stale.
    if let Some(stored_mtime) = manifest_entry.and_then(|e| e.mtime) {
        let current_mtime = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        if current_mtime != Some(stored_mtime) {
            return SnippetOutcome::Stale;
        }
    }

    // Size guard: reject files larger than 5 MB to match the index-build cap and
    // bound peak memory when 20 results are resolved simultaneously.
    const MAX_SNIPPET_FILE_BYTES: u64 = 5 * 1024 * 1024;
    let file_size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    if file_size > MAX_SNIPPET_FILE_BYTES {
        return SnippetOutcome::Unavailable;
    }

    // Read file content.
    let content = match std::fs::read(&abs_path) {
        Ok(c) => c,
        Err(_) => return SnippetOutcome::Unavailable,
    };
    let text = match std::str::from_utf8(&content) {
        Ok(t) => t,
        Err(_) => return SnippetOutcome::Unavailable,
    };

    let match_line = rskim_search::byte_offset_to_line(&content, match_positions[0].start) as u32;

    let line_range = rskim_search::compute_line_range(&content, match_positions);

    let ctx_lines = extract_context_window(text, match_line, DEFAULT_CONTEXT);
    if ctx_lines.is_empty() {
        return SnippetOutcome::Unavailable;
    }

    SnippetOutcome::Ok {
        match_line,
        line_range,
        context: SnippetContext { lines: ctx_lines },
    }
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
/// - This makes the predicate a **candidate-then-verify** gate: the bigram index
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
/// - **Single-token / sub-2-byte queries**: the bigram reader already returns an
///   empty candidate set for queries shorter than 2 bytes
///   (`extract_query_ngrams` returns `[]` for `len < 2`).  Verification of a
///   sub-2-byte query over a non-empty candidate list is therefore moot in
///   practice, but this fn handles it correctly: the token must appear as a
///   substring.
///
/// This fn is pure (no I/O, no side effects) so it can be unit-tested in
/// isolation — see `snippet_tests.rs`.
pub(super) fn query_substring_present(content: &str, query: &str) -> bool {
    // Split on whitespace; require every non-empty token to appear in content.
    // An empty query (no tokens after splitting) is vacuously true — callers
    // already short-circuit on empty queries before building the candidate list.
    query
        .split_whitespace()
        .all(|token| content.contains(token))
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
    if match_positions.is_empty() {
        return (SnippetOutcome::Unavailable, false);
    }

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
    const MAX_SNIPPET_FILE_BYTES: u64 = 5 * 1024 * 1024;
    let file_size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    if file_size > MAX_SNIPPET_FILE_BYTES {
        return (SnippetOutcome::Unavailable, false);
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
