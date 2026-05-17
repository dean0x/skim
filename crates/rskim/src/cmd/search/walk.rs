//! Project-root discovery and recursive file walking for the index builder.
//!
//! # File cap
//!
//! `walk_and_read` stops after `max_files` files have been accepted. Skipped
//! files (unsupported language, too large, non-UTF8) do not count toward the cap.
//!
//! # Skip conditions (in order checked)
//!
//! | Condition | Threshold |
//! |-----------|-----------|
//! | Unsupported language | `Language::from_path()` returns `None` |
//! | File too large | > 5 MB (`metadata.len()`) |
//! | Non-UTF8 | `read_to_string()` returns `Err` |
//! | Minified | avg line > 500 bytes in first 8 KB (tree-sitter langs only) |
//! | Cap reached | `max_files` exceeded |

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use ignore::WalkBuilder;
use rskim_core::Language;
use sha2::{Digest, Sha256};

use super::types::{ReadFile, SkipReason};

// ============================================================================
// Constants
// ============================================================================

/// Maximum file size accepted for indexing (5 MiB).
const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

// Compile-time guard: MAX_FILE_BYTES must fit in a usize so the pre-allocation
// in open_and_read (`size as usize`) is sound on every supported platform.
const _: () = assert!(
    MAX_FILE_BYTES <= usize::MAX as u64,
    "MAX_FILE_BYTES exceeds usize::MAX — update the cast in open_and_read"
);

/// Number of bytes inspected when checking for minified files.
const MINIFY_PROBE_BYTES: usize = 8192;

/// Average line length (bytes) above which a file is considered minified.
const MINIFY_AVG_LINE_BYTES: usize = 500;

/// Maximum number of ancestors to traverse when looking for a `.git` root.
/// 256 ancestors is far beyond any real filesystem depth.
const MAX_ANCESTORS: usize = 256;

// ============================================================================
// Typed read outcome
// ============================================================================

/// Strongly-typed result of [`open_and_read`].
///
/// Using an enum instead of `io::Error` avoids string-matching on error messages
/// to distinguish the "too large" case from genuine I/O failures.  The caller
/// matches on variants and never inspects error message text.
enum ReadOutcome {
    /// File read successfully.
    Content(String),
    /// File content is not valid UTF-8.
    NonUtf8,
    /// File size (from the open file handle's metadata) exceeds [`MAX_FILE_BYTES`].
    TooLarge(u64),
    /// Any other I/O error (permission denied, broken pipe, etc.).
    Io(std::io::Error),
}

// ============================================================================
// Project root discovery
// ============================================================================

/// Walk up from `start` looking for a `.git` directory.
///
/// Returns the first ancestor that contains `.git/`, or `start` itself if none
/// is found (fallback: treat the provided directory as the root).
///
/// # Errors
///
/// Returns `Err` if `start` cannot be canonicalized.
pub(super) fn discover_project_root(start: &Path) -> anyhow::Result<PathBuf> {
    let canonical = start
        .canonicalize()
        .with_context(|| format!("failed to canonicalize path: {}", start.display()))?;

    let mut current = canonical.as_path();
    for _ in 0..MAX_ANCESTORS {
        if current.join(".git").exists() {
            return Ok(current.to_path_buf());
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }

    // No .git found — fall back to the canonical form of the provided path.
    Ok(canonical)
}

// ============================================================================
// File walking
// ============================================================================

/// Walk `root` recursively, read each source file, compute its SHA-256, and
/// return the list of [`ReadFile`]s along with collected [`SkipReason`]s.
///
/// The walker respects `.gitignore` and other ignore files, skips hidden
/// directories, and does not follow symbolic links.
///
/// # Ordering
///
/// Files are returned in lexicographic path order (from `sort_by_file_path`).
///
/// # Errors
///
/// Returns `Err` only for fatal walker setup errors. Per-file read errors are
/// collected as [`SkipReason::ReadError`] and returned in the skipped list.
pub(super) fn walk_and_read(
    root: &Path,
    max_files: usize,
) -> anyhow::Result<(Vec<ReadFile>, Vec<SkipReason>)> {
    // Pre-allocate based on max_files (capped at 4096) to avoid repeated
    // reallocation on large repos.  Skipped entries are typically far fewer
    // than accepted files, so 256 is a conservative but sufficient default.
    let mut files: Vec<ReadFile> = Vec::with_capacity(max_files.min(4096));
    let mut skipped: Vec<SkipReason> = Vec::with_capacity(256);

    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(true) // skip hidden files/dirs
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .ignore(true)
        .parents(true)
        .require_git(false)
        .follow_links(false)
        .sort_by_file_path(|a, b| a.cmp(b));

    for entry_result in builder.build() {
        // Stop once we've hit the cap.
        if files.len() >= max_files {
            skipped.push(SkipReason::CapReached);
            break;
        }

        let entry = match entry_result {
            Ok(e) => e,
            Err(err) => {
                // Extract the real path from the ignore::Error::WithPath
                // variant instead of parsing the error message string.
                let path = match &err {
                    ignore::Error::WithPath { path, .. } => path.clone(),
                    _ => PathBuf::new(),
                };
                skipped.push(SkipReason::ReadError {
                    path,
                    error: err.to_string(),
                });
                continue;
            }
        };

        // Only process regular files.
        let file_type = match entry.file_type() {
            Some(ft) => ft,
            None => continue,
        };
        if !file_type.is_file() {
            continue;
        }

        let abs_path = entry.path();

        // --- Unsupported language ---
        let lang = match Language::from_path(abs_path) {
            Some(l) => l,
            None => {
                skipped.push(SkipReason::UnsupportedLanguage(abs_path.to_path_buf()));
                continue;
            }
        };

        // --- Fast size pre-screen using DirEntry cached metadata (issue #2) ---
        // entry.metadata() avoids an extra stat(2) syscall on 50 K-file repos.
        // If it fails we fall through and let the open() path handle the error.
        if let Ok(meta) = entry.metadata() {
            let size = meta.len();
            if size > MAX_FILE_BYTES {
                skipped.push(SkipReason::TooLarge {
                    path: abs_path.to_path_buf(),
                    size,
                });
                continue;
            }
        }

        // --- Open, size-check on handle, read (fixes TOCTOU race, issue #1) ---
        // Open the file first so that the metadata check and the read operate
        // on the same inode.  Pre-allocate the buffer to the known size so
        // read_to_string does at most one allocation.
        let content = match open_and_read(abs_path) {
            ReadOutcome::Content(c) => c,
            ReadOutcome::NonUtf8 => {
                skipped.push(SkipReason::NonUtf8(abs_path.to_path_buf()));
                continue;
            }
            ReadOutcome::TooLarge(size) => {
                // File grew past the limit between the pre-screen and open.
                skipped.push(SkipReason::TooLarge {
                    path: abs_path.to_path_buf(),
                    size,
                });
                continue;
            }
            ReadOutcome::Io(e) => {
                skipped.push(SkipReason::ReadError {
                    path: abs_path.to_path_buf(),
                    error: e.to_string(),
                });
                continue;
            }
        };

        // --- Minification check (tree-sitter languages only) ---
        if is_tree_sitter_language(lang) && is_minified(&content) {
            skipped.push(SkipReason::Minified(abs_path.to_path_buf()));
            continue;
        }

        // --- Compute SHA-256 ---
        let sha256 = sha256_hex(content.as_bytes());

        // --- Build relative path ---
        let rel_path = abs_path
            .strip_prefix(root)
            .unwrap_or(abs_path)
            .to_path_buf();

        files.push(ReadFile {
            rel_path,
            lang,
            content,
            sha256,
        });
    }

    Ok((files, skipped))
}

// ============================================================================
// Private helpers
// ============================================================================

/// Open `path`, verify its on-disk size via the file handle (not a separate
/// `stat(2)` call), then read it into a `String`.
///
/// Using the file handle for both the metadata check and the read prevents the
/// TOCTOU race where a file could be swapped between the size check and the
/// actual read.
///
/// Returns a [`ReadOutcome`] variant rather than an `io::Error` so that the
/// caller can match on typed cases without inspecting error message text.
fn open_and_read(path: &Path) -> ReadOutcome {
    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return ReadOutcome::Io(e),
    };
    let meta = match file.metadata() {
        Ok(m) => m,
        Err(e) => return ReadOutcome::Io(e),
    };
    let size = meta.len();
    if size > MAX_FILE_BYTES {
        return ReadOutcome::TooLarge(size);
    }
    // Pre-size the buffer to avoid reallocation; +1 so read_to_string can
    // detect EOF without an extra allocation.
    // Safety: MAX_FILE_BYTES <= usize::MAX is guaranteed by the compile-time
    // assertion above, so this cast is sound.
    let mut content = String::with_capacity((size as usize).saturating_add(1));
    match file.read_to_string(&mut content) {
        Ok(_) => ReadOutcome::Content(content),
        // read_to_string returns InvalidData for non-UTF-8 content.
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => ReadOutcome::NonUtf8,
        Err(e) => ReadOutcome::Io(e),
    }
}

/// Returns `true` if `lang` uses tree-sitter for parsing.
///
/// Non-tree-sitter languages (JSON, YAML, TOML) are excluded from the minify
/// check because their format makes long lines normal (e.g. minified JSON).
fn is_tree_sitter_language(lang: Language) -> bool {
    !matches!(lang, Language::Json | Language::Yaml | Language::Toml)
}

/// Returns `true` if the content appears minified.
///
/// Minification heuristic: probe the first [`MINIFY_PROBE_BYTES`] bytes. If
/// they contain no newlines, or the average bytes-per-line exceeds
/// [`MINIFY_AVG_LINE_BYTES`], the file is considered minified.
fn is_minified(content: &str) -> bool {
    let probe_len = content.len().min(MINIFY_PROBE_BYTES);
    // probe_len <= content.len(), so the slice is always in-bounds.
    let probe = &content.as_bytes()[..probe_len];
    let newline_count = probe.iter().filter(|&&b| b == b'\n').count();
    if newline_count == 0 {
        return probe.len() > MINIFY_AVG_LINE_BYTES;
    }
    probe.len() / newline_count > MINIFY_AVG_LINE_BYTES
}

/// Compute the SHA-256 of `data` and return it as a 64-character lowercase hex string.
///
/// Uses a const nibble lookup table instead of `write!` format calls to avoid
/// per-byte `fmt::Write` overhead on the hot path (called once per indexed file).
pub(super) fn sha256_hex(data: &[u8]) -> String {
    const NIBBLES: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(data);
    let mut hex = vec![0u8; 64];
    for (i, byte) in digest.iter().enumerate() {
        hex[i * 2] = NIBBLES[(byte >> 4) as usize];
        hex[i * 2 + 1] = NIBBLES[(byte & 0x0f) as usize];
    }
    // SAFETY: NIBBLES contains only ASCII hex characters, so hex is always valid UTF-8.
    unsafe { String::from_utf8_unchecked(hex) }
}

// ============================================================================
// Tests (co-located in walk_tests.rs)
// ============================================================================

#[cfg(test)]
#[path = "walk_tests.rs"]
mod tests;
