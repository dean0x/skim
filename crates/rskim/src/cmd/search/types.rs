//! Shared types for the `skim search index` pipeline.
//!
//! All types here are pure data — no I/O, no side effects.

use std::ops::Range;
use std::path::PathBuf;
use std::time::Duration;

use rskim_search::SearchField;

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct IndexResult {
    /// Number of files successfully indexed.
    pub file_count: u32,
    /// Number of files skipped (unsupported, too large, non-UTF8, etc.).
    pub skipped: u32,
    /// Number of files whose field_map was reused from the manifest cache.
    pub cache_hits: u32,
    /// Wall-clock duration of the build.
    pub duration: Duration,
}

// ============================================================================
// Skip reasons
// ============================================================================

/// Why a file was excluded from the index.
#[derive(Debug)]
#[allow(dead_code)] // Fields are for diagnostic/debug output via {:?}
pub(super) enum SkipReason {
    /// File is larger than the 5 MB threshold.
    TooLarge { path: PathBuf, size: u64 },
    /// File content is not valid UTF-8.
    NonUtf8(PathBuf),
    /// File appears to be minified (average line length > 500 bytes
    /// in the first 8 KB, tree-sitter languages only).
    Minified(PathBuf),
    /// No supported [`rskim_core::Language`] maps to this file's extension.
    UnsupportedLanguage(PathBuf),
    /// I/O error while reading the file.
    ReadError { path: PathBuf, error: String },
    /// File cap reached — no further files will be indexed.
    CapReached,
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
    /// Pre-computed or cache-reused field map.
    pub field_map: Vec<(Range<usize>, SearchField)>,
    /// `true` when field_map was reused from the manifest cache (no classify call).
    pub cache_hit: bool,
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
