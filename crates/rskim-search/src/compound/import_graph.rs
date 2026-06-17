//! Import-graph signal for composite ranking (#200).
//!
//! # Overview
//!
//! Extracts import/use/require edges from source files for TypeScript, Python,
//! Rust, and Go (AC6), building a directed edge set keyed by source path.
//! The import graph signal scores a (source, target) pair positively when
//! the source file imports the target file.
//!
//! # Algorithm
//!
//! For each source file, regex-extract the set of module specifiers from
//! import statements, then resolve specifiers to candidate FileIds via a
//! caller-supplied `path_map` (repo-relative path → FileId).  The edge score
//! for a (source, target) pair is `1.0` when an edge exists, `0.0` otherwise.
//!
//! # Limitations (open: #200 follow-up)
//!
//! - Resolution is path-based with simple heuristics (try exact path, then
//!   `<spec>.ext`, `<spec>/index.ext`).  Complex module systems (TypeScript
//!   path aliases, Python packages with `__init__.py`, Rust `mod.rs`) are
//!   partially handled but not complete.
//! - Only relative imports are resolved to FileIds; bare module names (stdlib,
//!   npm packages) are ignored (contribute 0 to the graph).
//! - Cross-crate Rust imports (`use other_crate::...`) are not resolved.
//!
//! # Performance
//!
//! Extraction is O(lines × pattern-length) per file; graph building is O(F)
//! over the file set.  Edge lookup is O(1) via the pre-built HashMap.  No
//! I/O inside the scoring function — callers pre-build the graph.
//!
//! # AC6 scope
//!
//! Supported languages for extraction:
//! - **TypeScript / JavaScript**: `import ... from "..."`, `require("...")`
//! - **Python**: `from ... import ...`, `import ...`
//! - **Rust**: `use ...::{...}` (intra-crate module references)
//! - **Go**: `import "..."`, `import (...)` blocks

use std::collections::{HashMap, HashSet};

use crate::types::FileId;

// ============================================================================
// Import graph
// ============================================================================

/// A directed import edge set: source FileId → set of target FileIds.
///
/// Built once per index build (or query execution) and reused for all
/// (source, target) scoring calls.
#[derive(Debug, Default)]
pub struct ImportGraph {
    /// edges[source_file_id] = set of target file IDs that source imports.
    edges: HashMap<FileId, HashSet<FileId>>,
}

impl ImportGraph {
    /// Build an import graph from a map of file contents.
    ///
    /// # Arguments
    ///
    /// * `files` — iterator of `(repo_relative_path, language, content)`.
    ///   `language` is a hint for which extractor to apply.
    /// * `path_to_id` — mapping from repo-relative path to `FileId`.
    ///   Used to resolve import specifiers to target `FileId`s.
    ///
    /// Files whose specifiers do not resolve to any known FileId are silently
    /// skipped (the import target is outside the index — stdlib, vendor, etc.).
    pub fn build<'a, I>(files: I, path_to_id: &HashMap<String, FileId>) -> Self
    where
        I: IntoIterator<Item = (&'a str, ImportLanguage, &'a str)>,
    {
        let mut edges: HashMap<FileId, HashSet<FileId>> = HashMap::new();
        for (path, lang, content) in files {
            let Some(&source_id) = path_to_id.get(path) else {
                continue;
            };
            let specifiers = extract_import_specifiers(content, lang);
            let targets = resolve_specifiers(&specifiers, path, path_to_id);
            if !targets.is_empty() {
                edges.insert(source_id, targets);
            }
        }
        Self { edges }
    }

    /// Score the (source, target) pair.
    ///
    /// Returns `1.0` when `source` imports `target`; `0.0` otherwise.
    ///
    /// This is an O(1) lookup (two HashMap lookups: outer by source, inner by
    /// target).  No per-call allocation.
    #[must_use]
    pub fn score(&self, source: FileId, target: FileId) -> f64 {
        if self
            .edges
            .get(&source)
            .is_some_and(|targets| targets.contains(&target))
        {
            1.0
        } else {
            0.0
        }
    }

    /// Returns the number of source files with at least one outgoing edge.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.edges.values().map(|s| s.len()).sum()
    }

    /// Produce a `Vec<(FileId, f64)>` layer suitable for `merge_layer_scores`,
    /// scoring all files reachable from `source` with 1.0.
    ///
    /// Returns an empty Vec when `source` has no outgoing edges.
    #[must_use]
    pub fn outgoing_as_layer(&self, source: FileId) -> Vec<(FileId, f64)> {
        match self.edges.get(&source) {
            None => vec![],
            Some(targets) => {
                let mut layer: Vec<(FileId, f64)> = targets.iter().map(|&t| (t, 1.0)).collect();
                // Deterministic order: sort by FileId ASC.
                layer.sort_unstable_by_key(|&(fid, _)| fid.0);
                layer
            }
        }
    }
}

// ============================================================================
// Language hint
// ============================================================================

/// Language identifier for import extraction.
///
/// Only the languages required by AC6 are listed here.  Unsupported languages
/// produce an empty specifier set (silent, no extraction).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportLanguage {
    /// TypeScript or JavaScript source.
    TypeScript,
    /// Python source.
    Python,
    /// Rust source.
    Rust,
    /// Go source.
    Go,
    /// Language not supported for import extraction (silently returns empty).
    Other,
}

// ============================================================================
// Extraction helpers (pure, no I/O)
// ============================================================================

/// Extract raw import specifier strings from `content` for the given language.
///
/// Returns a `Vec` of specifier strings (e.g. `"./utils"`, `"../auth.rs"`).
/// Only relative specifiers that could resolve to in-repo files are expected
/// to produce non-zero scores after path resolution.  Bare module names (e.g.
/// `"react"`, `"std::io"`) are included but will typically fail to resolve.
///
/// The function is pure (no I/O) and may produce false positives from
/// import-like patterns in comments or strings — acceptable for a ranking
/// signal where precision matters more than recall.
fn extract_import_specifiers(content: &str, lang: ImportLanguage) -> Vec<String> {
    match lang {
        ImportLanguage::TypeScript => extract_ts_specifiers(content),
        ImportLanguage::Python => extract_py_specifiers(content),
        ImportLanguage::Rust => extract_rs_specifiers(content),
        ImportLanguage::Go => extract_go_specifiers(content),
        ImportLanguage::Other => vec![],
    }
}

/// Extract TypeScript/JavaScript import specifiers.
///
/// Patterns matched (line-by-line):
/// - `import ... from "..."` / `import ... from '...'`
/// - `require("...")` / `require('...')`
/// - `import("...")` / `import('...')`  (dynamic imports)
fn extract_ts_specifiers(content: &str) -> Vec<String> {
    let mut specs = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        // Static import: `import ... from "..."`.
        if let Some(spec) = extract_from_clause(line, "from") {
            specs.push(spec);
        }
        // require() / import(): extract the string literal argument.
        for prefix in &["require(", "import("] {
            if let Some(inner) = extract_call_string_arg(line, prefix) {
                specs.push(inner);
            }
        }
    }
    specs
}

/// Extract Python import specifiers.
///
/// Patterns matched:
/// - `from <module> import ...` → module path (e.g. `.auth`, `..utils`)
/// - `import <module>` → module name (bare; only intra-package `.` prefixes resolve)
fn extract_py_specifiers(content: &str) -> Vec<String> {
    let mut specs = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("from ") {
            // `from <module> import ...`
            if let Some(module) = rest.split_whitespace().next() {
                specs.push(module.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("import ") {
            // `import <module>` or `import <module>, <module2>`
            for part in rest.split(',') {
                let module = part.split_whitespace().next().unwrap_or("");
                if !module.is_empty() {
                    specs.push(module.to_string());
                }
            }
        }
    }
    specs
}

/// Extract Rust `use` paths.
///
/// Patterns matched:
/// - `use <path>;` or `use <path> as ...;`
/// - Lines starting with `use ` (after `pub`/visibility modifiers stripped).
///
/// Only `super::` and `crate::` prefixes are retained as candidates; these
/// are intra-crate references that might correspond to sibling files.
fn extract_rs_specifiers(content: &str) -> Vec<String> {
    let mut specs = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        // Strip pub/pub(crate)/pub(super) visibility modifiers.
        let line = line
            .trim_start_matches("pub")
            .trim()
            .trim_start_matches("(crate)")
            .trim()
            .trim_start_matches("(super)")
            .trim();
        if let Some(rest) = line.strip_prefix("use ") {
            // Take the path up to `;` or whitespace.
            let path = rest
                .split([';', ' '])
                .next()
                .unwrap_or("")
                .trim_end_matches(';');
            if !path.is_empty() {
                specs.push(path.to_string());
            }
        }
    }
    specs
}

/// Extract Go import specifiers.
///
/// Patterns matched:
/// - `import "..."` (single-import)
/// - Lines inside `import (...)` blocks containing a quoted string
fn extract_go_specifiers(content: &str) -> Vec<String> {
    let mut specs = Vec::new();
    let mut in_import_block = false;

    for line in content.lines() {
        let line = line.trim();
        if line == "import (" {
            in_import_block = true;
            continue;
        }
        if in_import_block && line == ")" {
            in_import_block = false;
            continue;
        }
        if in_import_block {
            // Line inside the import block: `"pkg/path"` or `alias "pkg/path"`.
            if let Some(spec) = extract_quoted_string(line) {
                specs.push(spec);
            }
        } else if let Some(rest) = line.strip_prefix("import ") {
            // Single-line `import "pkg/path"`.
            if let Some(spec) = extract_quoted_string(rest.trim()) {
                specs.push(spec);
            }
        }
    }
    specs
}

// ============================================================================
// String-literal extraction helpers
// ============================================================================

/// Extract the specifier from a `from "..."` or `from '...'` clause.
fn extract_from_clause(line: &str, keyword: &str) -> Option<String> {
    // Find `from ` or ` from ` preceded by some content.
    let idx = line
        .find(&format!("{keyword} "))
        .or_else(|| line.find(&format!(" {keyword} ")))?;
    let after = &line[idx + keyword.len() + 1..].trim_start();
    extract_quoted_string(after)
}

/// Extract the string argument from a call like `require("...")` or `import("...")`.
fn extract_call_string_arg(line: &str, prefix: &str) -> Option<String> {
    let idx = line.find(prefix)?;
    let after = &line[idx + prefix.len()..];
    extract_quoted_string(after)
}

/// Extract the content of the first quoted string (`"..."` or `'...'`) in `s`.
fn extract_quoted_string(s: &str) -> Option<String> {
    let s = s.trim();
    for quote in ['"', '\'', '`'] {
        if let Some(inner) = s.strip_prefix(quote)
            && let Some(end) = inner.find(quote)
        {
            let spec = &inner[..end];
            if !spec.is_empty() {
                return Some(spec.to_string());
            }
        }
    }
    None
}

// ============================================================================
// Path resolution
// ============================================================================

/// Resolve a slice of import specifiers to a set of target `FileId`s.
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
fn resolve_specifiers(
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
fn candidate_paths(spec: &str, source_dir: &str) -> Vec<String> {
    let mut candidates = Vec::new();

    // Extensions to try (in priority order).
    const EXTENSIONS: &[&str] = &[".ts", ".tsx", ".js", ".jsx", ".py", ".rs", ".go"];
    const INDEX_FILES: &[&str] = &["/index.ts", "/index.tsx", "/index.js"];

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
    } else {
        // Bare or stdlib: use as-is.
        let base = spec.replace('.', "/");
        add_extension_candidates(&base, EXTENSIONS, INDEX_FILES, &mut candidates);
    }

    candidates
}

/// Append extension-based candidate paths for a base path into `out`.
fn add_extension_candidates(
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
fn normalize_path(path: &str) -> String {
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "import_graph_tests.rs"]
mod tests;
