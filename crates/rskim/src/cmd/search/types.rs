//! Shared types for the `skim search index` pipeline.
//!
//! All types here are pure data — no I/O, no side effects.

use std::path::PathBuf;
use std::time::Duration;

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
// Per-file read result
// ============================================================================

/// A successfully read file ready for classification.
#[derive(Debug)]
pub(super) struct ReadFile {
    /// Path relative to the project root.
    pub rel_path: PathBuf,
    /// Detected source language.
    pub lang: rskim_core::Language,
    /// Full file content as UTF-8 string.
    pub content: String,
    /// Hex-encoded SHA-256 of `content` (lowercase, 64 chars).
    pub sha256: String,
}
