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
    Ok(u32, SnippetContext),
    /// File has changed since indexing (mtime mismatch) — positions may be stale.
    Stale,
    /// File deleted, unreadable, empty positions, or non-UTF8.
    Unavailable,
}

// ============================================================================
// Byte-offset → line number
// ============================================================================

/// Map a byte offset within `content` to a 1-indexed line number.
///
/// Counts newlines in `content[..offset]`. Returns `1` for offset `0` or
/// any offset in an empty file.
pub(super) fn byte_offset_to_line(content: &[u8], offset: usize) -> u32 {
    let safe_offset = offset.min(content.len());
    let newlines = content[..safe_offset]
        .iter()
        .filter(|&&b| b == b'\n')
        .count();
    (newlines as u32).saturating_add(1)
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
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len() as u32;

    if total_lines == 0 {
        return Vec::new();
    }

    // Clamp to [1, total_lines]
    let match_line = match_line.max(1).min(total_lines);

    let start = match_line.saturating_sub(context).max(1);
    let end = (match_line + context).min(total_lines);

    (start..=end)
        .map(|ln| {
            let idx = (ln - 1) as usize;
            SnippetLine {
                line_number: ln,
                content: lines[idx].to_string(),
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
/// - `SnippetOutcome::Ok(line, ctx)` on success.
/// - `SnippetOutcome::Stale` when the file's mtime differs from manifest (changed since indexing).
/// - `SnippetOutcome::Unavailable` when positions are empty, file is deleted/unreadable, or non-UTF8.
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

    // Mtime guard: if the manifest recorded an mtime and it doesn't match
    // the file's current mtime, the file has changed — positions are stale.
    if let Some(stored_mtime) = manifest_entry.and_then(|e| e.mtime) {
        let current_mtime = std::fs::metadata(&abs_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        if current_mtime != Some(stored_mtime) {
            return SnippetOutcome::Stale;
        }
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

    // Use the first match position to locate the match line.
    let first_match_start = match_positions[0].start;
    let match_line = byte_offset_to_line(&content, first_match_start);

    let ctx_lines = extract_context_window(text, match_line, DEFAULT_CONTEXT);
    if ctx_lines.is_empty() {
        return SnippetOutcome::Unavailable;
    }

    SnippetOutcome::Ok(match_line, SnippetContext { lines: ctx_lines })
}

// ============================================================================
// Tests (co-located in snippet_tests.rs)
// ============================================================================

#[cfg(test)]
#[path = "snippet_tests.rs"]
mod tests;
