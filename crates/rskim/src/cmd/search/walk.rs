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
use std::path::{Path, PathBuf};

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
/// Returns [`std::io::Error`] if `start` cannot be canonicalized.
pub(super) fn discover_project_root(start: &Path) -> std::io::Result<PathBuf> {
    let canonical = start.canonicalize()?;

    let mut current = canonical.as_path();
    loop {
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
/// Returns [`std::io::Error`] only for fatal walker setup errors. Per-file
/// read errors are collected as [`SkipReason::ReadError`] and returned in the
/// skipped list.
pub(super) fn walk_and_read(
    root: &Path,
    max_files: usize,
) -> std::io::Result<(Vec<ReadFile>, Vec<SkipReason>)> {
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
                skipped.push(SkipReason::ReadError {
                    path: PathBuf::from(err.to_string()),
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

        // --- File too large ---
        let metadata = match fs::metadata(abs_path) {
            Ok(m) => m,
            Err(e) => {
                skipped.push(SkipReason::ReadError {
                    path: abs_path.to_path_buf(),
                    error: e.to_string(),
                });
                continue;
            }
        };
        if metadata.len() > MAX_FILE_BYTES {
            skipped.push(SkipReason::TooLarge {
                path: abs_path.to_path_buf(),
                size: metadata.len(),
            });
            continue;
        }

        // --- Read content (catches non-UTF8) ---
        let content = match fs::read_to_string(abs_path) {
            Ok(c) => c,
            Err(_) => {
                skipped.push(SkipReason::NonUtf8(abs_path.to_path_buf()));
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

/// Returns `true` if `lang` uses tree-sitter for parsing.
///
/// Non-tree-sitter languages (JSON, YAML, TOML) are excluded from the minify
/// check because their format makes long lines normal (e.g. minified JSON).
fn is_tree_sitter_language(lang: Language) -> bool {
    !matches!(
        lang,
        Language::Json | Language::Yaml | Language::Toml
    )
}

/// Returns `true` if the content appears minified.
///
/// Minification heuristic: probe the first [`MINIFY_PROBE_BYTES`] bytes. If
/// they contain no newlines, or the average bytes-per-line exceeds
/// [`MINIFY_AVG_LINE_BYTES`], the file is considered minified.
fn is_minified(content: &str) -> bool {
    let probe = &content[..content.len().min(MINIFY_PROBE_BYTES)];
    let newline_count = probe.bytes().filter(|&b| b == b'\n').count();
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
