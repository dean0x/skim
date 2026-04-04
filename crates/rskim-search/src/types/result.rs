//! Search result types: `SearchResult`, `MatchSpan`, `LineRange`, and `IndexStats`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::query::SearchField;

/// Byte-offset span within a source file.
///
/// Both `start` and `end` are byte offsets into the original UTF-8 source.
/// The span is half-open: `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchSpan {
    /// Start byte offset (inclusive).
    pub start: u32,
    /// End byte offset (exclusive).
    pub end: u32,
}

impl MatchSpan {
    /// Create a new span from start and end byte offsets.
    pub fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// Return the length of the span in bytes.
    pub fn len(self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    /// Return `true` if the span has zero length.
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
}

/// 1-indexed, inclusive line range within a source file.
///
/// Both `start` and `end` are 1-based line numbers.
/// A single-line range has `start == end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineRange {
    /// First line (1-indexed, inclusive).
    pub start: u32,
    /// Last line (1-indexed, inclusive).
    pub end: u32,
}

impl LineRange {
    /// Create a new line range from start and end line numbers.
    pub fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }
}

/// A single result from a search query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Path to the file containing the match.
    pub file_path: PathBuf,
    /// Line range of the matched region (1-indexed, inclusive).
    pub line_range: LineRange,
    /// Relevance score (higher is better; not normalized across layers).
    pub score: f32,
    /// The semantic field in which the match was found.
    pub matched_field: SearchField,
    /// A short excerpt of the matching source region.
    pub snippet: String,
    /// Byte-offset positions of the matched terms within `snippet`.
    pub match_positions: Vec<MatchSpan>,
}

/// Runtime statistics for a search index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexStats {
    /// Total number of files indexed.
    pub file_count: u64,
    /// Total number of n-grams stored in the index.
    pub total_ngrams: u64,
    /// On-disk size of the index in bytes.
    pub index_size_bytes: u64,
    /// Unix timestamp (seconds) of the last index update.
    pub last_updated: u64,
    /// Serialization format version for forward/backward compatibility.
    pub format_version: u32,
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_match_span_len_and_empty() {
        let span = MatchSpan::new(10, 20);
        assert_eq!(span.len(), 10);
        assert!(!span.is_empty());

        let zero = MatchSpan::new(5, 5);
        assert_eq!(zero.len(), 0);
        assert!(zero.is_empty());

        // Saturating subtraction: start > end should not panic
        let inverted = MatchSpan::new(20, 10);
        assert_eq!(inverted.len(), 0);
        assert!(inverted.is_empty());
    }

    #[test]
    fn test_line_range_construction() {
        let r = LineRange::new(1, 5);
        assert_eq!(r.start, 1);
        assert_eq!(r.end, 5);
    }
}
