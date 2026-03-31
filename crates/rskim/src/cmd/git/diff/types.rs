//! Type definitions for the AST-aware diff pipeline.

use rskim_core::Language;

use super::DiffMode;
use crate::output::canonical::DiffFileStatus;

/// A single hunk from a unified diff.
#[derive(Debug, Clone)]
pub(super) struct DiffHunk<'a> {
    /// Start line in the old file (1-indexed).
    /// Used in tests and for hunk-to-node overlap calculations.
    #[allow(dead_code)]
    pub old_start: usize,
    /// Number of lines removed from old file.
    /// Used in tests for validating hunk parsing.
    #[allow(dead_code)]
    pub old_count: usize,
    /// Start line in the new file (1-indexed)
    pub new_start: usize,
    /// Number of lines added in new file
    pub new_count: usize,
    /// Raw patch lines (including `+`, `-`, and context ` ` prefixes).
    /// Borrows from the raw diff output string, which outlives all consumers.
    pub patch_lines: Vec<&'a str>,
}

/// Parsed representation of a single file in a unified diff.
#[derive(Debug, Clone)]
pub(super) struct FileDiff<'a> {
    /// File path (new path for renames/adds, old path for deletes)
    pub path: String,
    /// Original path for renames (old name)
    pub old_path: Option<String>,
    /// File status
    pub status: DiffFileStatus,
    /// Hunks of changed lines
    pub hunks: Vec<DiffHunk<'a>>,
}

/// Metadata collected from extended diff headers (new/deleted/renamed/binary).
pub(super) struct FileMetadata {
    pub is_binary: bool,
    pub is_new: bool,
    pub is_deleted: bool,
    pub is_renamed: bool,
    pub rename_from: Option<String>,
    pub file_minus: String,
    pub file_plus: String,
}

/// A resolved AST node range, with optional parent context for nested nodes.
#[derive(Debug, Clone)]
pub(super) struct ChangedNodeRange {
    /// Start line of this node (1-indexed).
    pub start: usize,
    /// End line of this node (1-indexed).
    pub end: usize,
    /// If this node is a child of a container (class/struct/impl), store the
    /// parent's first line (declaration header) and last line (closing brace).
    pub parent_context: Option<ParentContext>,
}

/// Stores the declaration line and closing brace of a container node.
#[derive(Debug, Clone)]
pub(super) struct ParentContext {
    /// The first line of the parent (declaration header), 1-indexed.
    pub header_line: usize,
    /// The last line of the parent (closing brace), 1-indexed.
    pub close_line: usize,
}

/// Shared context for mode-aware rendering functions.
///
/// Groups the parameters that are threaded through the rendering call chain
/// to stay within clippy's 7-argument limit.
pub(super) struct ModeRenderContext<'a> {
    pub changed_ranges: &'a [ChangedNodeRange],
    pub hunks: &'a [DiffHunk<'a>],
    pub source_lines: &'a [&'a str],
    pub source: &'a str,
    pub lang: Language,
    pub diff_mode: DiffMode,
}
