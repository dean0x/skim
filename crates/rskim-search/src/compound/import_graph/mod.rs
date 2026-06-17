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

// Path-resolution helpers (factored out to satisfy the AC16 ≤400-line guard).
mod import_graph_paths;
use import_graph_paths::resolve_specifiers;

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
// Tests
// ============================================================================

#[cfg(test)]
#[path = "import_graph_tests.rs"]
mod tests;
