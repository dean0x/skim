//! Type definitions for the AST-aware diff pipeline.

use super::DiffMode;
use crate::output::canonical::DiffFileStatus;

/// A single hunk from a unified diff.
///
/// DESIGN NOTE (AD-6): visibility widened to `pub(in crate::cmd::git)` so that
/// `show.rs` can pass `DiffHunk` slices into `render_diff_file` directly, reusing
/// the diff pipeline without duplicating parsing logic.
#[derive(Debug, Clone)]
pub(in crate::cmd::git) struct DiffHunk<'a> {
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
///
/// DESIGN NOTE (AD-6): visibility widened to `pub(in crate::cmd::git)` so that
/// `show.rs` can iterate over `FileDiff` entries returned by `parse_unified_diff`
/// without requiring a parallel data model in the show handler.
#[derive(Debug, Clone)]
pub(in crate::cmd::git) struct FileDiff<'a> {
    /// File path (new path for renames/adds, old path for deletes)
    pub path: String,
    /// Original path for renames (old name)
    pub old_path: Option<String>,
    /// File status
    pub status: DiffFileStatus,
    /// Hunks of changed lines
    pub hunks: Vec<DiffHunk<'a>>,
}

/// The kind of change recorded in extended diff headers.
///
/// Encodes the mutually-exclusive file states that the boolean flags
/// `is_new`, `is_deleted`, `is_renamed`, and `is_binary` previously
/// represented. Using an enum makes illegal combinations unrepresentable
/// at the type level.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) enum FileChange {
    /// Regular modification — the default when no special header is seen.
    #[default]
    Modified,
    /// `new file mode` header present.
    New,
    /// `deleted file mode` header present.
    Deleted,
    /// `rename from` / `rename to` headers present.
    Renamed {
        /// Source path from `rename from <path>`, if the header was present.
        from: Option<String>,
    },
    /// `Binary files … differ` line present.
    Binary,
}

/// Metadata collected from extended diff headers (new/deleted/renamed/binary).
pub(super) struct FileMetadata {
    /// The kind of change — replaces the old `is_new`/`is_deleted`/`is_renamed`/`is_binary` booleans.
    pub change: FileChange,
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
/// Groups the parameters that are threaded through the rendering call chain.
/// The tree-sitter `Parser` is passed separately as `&mut` (cannot be shared
/// via an immutable context).
pub(super) struct ModeRenderContext<'a> {
    pub changed_ranges: &'a [ChangedNodeRange],
    pub hunks: &'a [DiffHunk<'a>],
    pub source_lines: &'a [&'a str],
    pub source: &'a str,
    pub diff_mode: DiffMode,
}
