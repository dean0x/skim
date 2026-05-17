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
use std::io::{self, Read as _};
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

/// Number of bytes inspected when checking for minified files.
const MINIFY_PROBE_BYTES: usize = 8192;

/// Average line length (bytes) above which a file is considered minified.
const MINIFY_AVG_LINE_BYTES: usize = 500;

/// Maximum number of ancestors to traverse when looking for a `.git` root.
/// 256 ancestors is far beyond any real filesystem depth.
const MAX_ANCESTORS: usize = 256;

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
    let mut files: Vec<ReadFile> = Vec::new();
    let mut skipped: Vec<SkipReason> = Vec::new();

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
            Ok(c) => c,
            Err(e) => {
                // Distinguish non-UTF-8 content from other I/O errors (issue #3).
                if e.kind() == io::ErrorKind::InvalidData {
                    skipped.push(SkipReason::NonUtf8(abs_path.to_path_buf()));
                } else if e.kind() == io::ErrorKind::Other
                    && e.to_string().contains("too large")
                {
                    // File grew past the limit between the pre-screen and open.
                    skipped.push(SkipReason::TooLarge {
                        path: abs_path.to_path_buf(),
                        size: MAX_FILE_BYTES + 1,
                    });
                } else {
                    skipped.push(SkipReason::ReadError {
                        path: abs_path.to_path_buf(),
                        error: e.to_string(),
                    });
                }
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
/// # Errors
///
/// - `ErrorKind::InvalidData` — file content is not valid UTF-8
/// - `ErrorKind::Other` with message "too large" — file exceeds [`MAX_FILE_BYTES`]
/// - Other `io::Error` kinds — permission denied, I/O error, etc.
fn open_and_read(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let meta = file.metadata()?;
    let size = meta.len();
    if size > MAX_FILE_BYTES {
        return Err(io::Error::other("too large"));
    }
    // Pre-size the buffer to avoid reallocation; +1 so read_to_string can
    // detect EOF without an extra allocation.
    let mut content = String::with_capacity((size as usize).saturating_add(1));
    file.read_to_string(&mut content)?;
    Ok(content)
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
    let probe = content
        .as_bytes()
        .get(..probe_len)
        .unwrap_or(content.as_bytes());
    let newline_count = probe.iter().filter(|&&b| b == b'\n').count();
    if newline_count == 0 {
        return probe.len() > MINIFY_AVG_LINE_BYTES;
    }
    probe.len() / newline_count > MINIFY_AVG_LINE_BYTES
}

/// Compute the SHA-256 of `data` and return it as a 64-character lowercase hex string.
pub(super) fn sha256_hex(data: &[u8]) -> String {
    use std::fmt::Write;
    let digest = Sha256::digest(data);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        // write! to a String is infallible — unwrap is safe here.
        write!(hex, "{byte:02x}").unwrap();
    }
    hex
}

// ============================================================================
// Tests (co-located in walk_tests.rs)
// ============================================================================

#[cfg(test)]
#[path = "walk_tests.rs"]
mod tests;
