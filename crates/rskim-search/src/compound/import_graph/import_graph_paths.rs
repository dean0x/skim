//! Path-resolution helpers for the import-graph signal.
//!
//! Factored out of `import_graph.rs` to keep each file within the 400-line
//! AC16 guard.  All items are `pub(super)` so they remain private to the
//! `import_graph` module.

use std::collections::{HashMap, HashSet};

use crate::types::FileId;

/// File extensions to try (in priority order) when resolving a module path.
const EXTENSIONS: &[&str] = &[".ts", ".tsx", ".js", ".jsx", ".py", ".rs", ".go"];
/// Index-file suffixes to try for directory-style imports.
const INDEX_FILES: &[&str] = &["/index.ts", "/index.tsx", "/index.js"];

// ============================================================================
// Path resolution
// ============================================================================

/// Resolve a slice of import specifiers to a set of target [`FileId`]s.
///
/// For each specifier, tries the following candidates in order:
/// 1. Exact: specifier as-is (relative to `source_path`'s directory).
/// 2. With `.ts`, `.js`, `.py`, `.rs`, `.go` extensions appended.
/// 3. With `/index.ts`, `/index.js` appended (for index files).
///
/// Relative specifiers (starting with `./` or `../`) are resolved relative
/// to the directory of `source_path`.  Absolute or bare specifiers are tried
/// as repo-relative paths directly.
///
/// Rust `use` paths (e.g. `crate::cmd::search`) are converted to
/// `cmd/search.rs` and `cmd/search/mod.rs` candidates.
pub(super) fn resolve_specifiers(
    specifiers: &[String],
    source_path: &str,
    path_to_id: &HashMap<String, FileId>,
) -> HashSet<FileId> {
    let source_dir = source_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let mut targets = HashSet::new();

    for spec in specifiers {
        let candidates = candidate_paths(spec, source_dir);
        for candidate in candidates {
            if let Some(&fid) = path_to_id.get(&candidate) {
                targets.insert(fid);
                break; // First match wins per specifier.
            }
        }
    }

    targets
}

/// Generate candidate repo-relative paths from a specifier.
pub(super) fn candidate_paths(spec: &str, source_dir: &str) -> Vec<String> {
    let mut candidates = Vec::new();

    // Determine the base path and generate candidates.
    if spec.starts_with("./") || spec.starts_with("../") {
        // Standard relative specifier: resolve relative to source directory.
        let joined = if source_dir.is_empty() {
            spec.to_string()
        } else {
            format!("{source_dir}/{spec}")
        };
        let base = normalize_path(&joined);
        add_extension_candidates(&base, EXTENSIONS, INDEX_FILES, &mut candidates);
    } else if spec.starts_with('.') && !spec.starts_with("..") {
        // Python relative import: `.module` → same directory as source.
        // `.utils` from `src/main.py` resolves to `src/utils.py`.
        let module_name = spec.trim_start_matches('.');
        if module_name.is_empty() {
            // `from . import foo` — current package directory (no single file to resolve).
            return candidates;
        }
        let base = if source_dir.is_empty() {
            module_name.replace('.', "/")
        } else {
            format!("{}/{}", source_dir, module_name.replace('.', "/"))
        };
        let base = normalize_path(&base);
        add_extension_candidates(&base, EXTENSIONS, INDEX_FILES, &mut candidates);
    } else if spec.starts_with("crate::") || spec.starts_with("super::") {
        // Rust intra-crate path — delegate to keep this dispatcher flat.
        candidates = rust_candidates(spec, source_dir);
    } else {
        // Bare or stdlib: use as-is.
        let base = spec.replace('.', "/");
        add_extension_candidates(&base, EXTENSIONS, INDEX_FILES, &mut candidates);
    }

    candidates
}

/// Generate candidate paths for a Rust intra-crate `use` specifier
/// (`crate::…` or `super::…`).
///
/// Factored out of [`candidate_paths`] so that dispatcher stays flat; the
/// `crate::` and `super::` forms each build their own candidate set using the
/// shared [`normalize_path`] + [`add_extension_candidates`] helpers.
fn rust_candidates(spec: &str, source_dir: &str) -> Vec<String> {
    let mut candidates = Vec::new();

    // Rust intra-crate path: convert `::` segments to `/`.
    // `crate::cmd::search` → `cmd/search` (without crate:: prefix) → try `src/cmd/search.rs`.
    let without_prefix = if let Some(rest) = spec.strip_prefix("crate::") {
        rest
    } else {
        // `super::foo` from `src/bar/mod.rs` → `src/foo`
        let parts = spec.trim_start_matches("super::").replace("::", "/");
        let base = if source_dir.is_empty() {
            parts
        } else {
            // Go up one level from the source file's directory.
            let parent = source_dir.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
            if parent.is_empty() {
                parts
            } else {
                format!("{parent}/{parts}")
            }
        };
        let base = normalize_path(&base);
        add_extension_candidates(&base, EXTENSIONS, INDEX_FILES, &mut candidates);
        candidates.push(format!("{base}/mod.rs"));
        return candidates;
    };
    // Try with and without a `src/` prefix since Rust crates typically live in `src/`.
    let rel_path = without_prefix.replace("::", "/");
    // Candidate 1: direct path (e.g., for flat workspace layout).
    add_extension_candidates(&rel_path, EXTENSIONS, INDEX_FILES, &mut candidates);
    candidates.push(format!("{rel_path}/mod.rs"));
    // Candidate 2: under `src/` prefix (most common Rust layout).
    let src_path = format!("src/{rel_path}");
    add_extension_candidates(&src_path, EXTENSIONS, INDEX_FILES, &mut candidates);
    candidates.push(format!("{src_path}/mod.rs"));

    candidates
}

/// Append extension-based candidate paths for a base path into `out`.
pub(super) fn add_extension_candidates(
    base: &str,
    extensions: &[&str],
    index_files: &[&str],
    out: &mut Vec<String>,
) {
    // Exact match first.
    out.push(base.to_string());
    // With each extension.
    for ext in extensions {
        out.push(format!("{base}{ext}"));
    }
    // Index files.
    for idx in index_files {
        out.push(format!("{base}{idx}"));
    }
}

/// Normalize a path by resolving `.` and `..` components.
///
/// Simple implementation: split on `/`, process components.
/// Does not follow symlinks (pure string manipulation).
pub(super) fn normalize_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {} // Skip empty and current-dir segments.
            ".." => {
                parts.pop(); // Go up one level (saturating at root).
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}
