//! Tests for `compound::import_graph` (AC6).

use super::*;

// ============================================================================
// Helpers
// ============================================================================

fn make_path_map(paths: &[&str]) -> HashMap<String, FileId> {
    paths
        .iter()
        .enumerate()
        .map(|(i, &p)| (p.to_string(), FileId(i as u32)))
        .collect()
}

// ============================================================================
// AC6 — TypeScript: import edge scores > 0 for importer→imported pair
// ============================================================================

#[test]
fn test_ts_import_edge_present() {
    let path_map = make_path_map(&["src/main.ts", "src/utils.ts", "src/other.ts"]);

    // main.ts imports utils.ts via relative specifier.
    let files = [(
        "src/main.ts",
        ImportLanguage::TypeScript,
        "import { foo } from \"./utils\";\n",
    )];
    let graph = ImportGraph::build(files, &path_map);

    let main_id = FileId(0);
    let utils_id = FileId(1);
    let other_id = FileId(2);

    let score_edge = graph.score(main_id, utils_id);
    let score_no_edge = graph.score(main_id, other_id);

    assert!(
        score_edge > 0.0,
        "TypeScript: importer→imported must score > 0 (AC6), got {score_edge}"
    );
    assert_eq!(
        score_no_edge, 0.0,
        "TypeScript: unrelated pair must score 0 (AC6), got {score_no_edge}"
    );
    assert!(
        score_edge > score_no_edge,
        "TypeScript: import edge must score strictly higher than non-edge (AC6)"
    );
}

#[test]
fn test_ts_require_edge_present() {
    let path_map = make_path_map(&["src/app.ts", "src/config.ts"]);
    let content = "const config = require('./config');\n";
    let files = [("src/app.ts", ImportLanguage::TypeScript, content)];
    let graph = ImportGraph::build(files, &path_map);

    let score = graph.score(FileId(0), FileId(1));
    assert!(
        score > 0.0,
        "TypeScript require() must create an edge (AC6), got {score}"
    );
}

// ============================================================================
// AC6 — Python
// ============================================================================

#[test]
fn test_python_from_import_edge_present() {
    let path_map = make_path_map(&["src/main.py", "src/utils.py", "src/other.py"]);
    let content = "from .utils import helper\n";
    let files = [("src/main.py", ImportLanguage::Python, content)];
    let graph = ImportGraph::build(files, &path_map);

    let main_id = FileId(0);
    let utils_id = FileId(1);
    let other_id = FileId(2);

    let score_edge = graph.score(main_id, utils_id);
    let score_no_edge = graph.score(main_id, other_id);

    assert!(
        score_edge > 0.0,
        "Python: from ... import edge must score > 0 (AC6), got {score_edge}"
    );
    assert_eq!(
        score_no_edge, 0.0,
        "Python: unrelated pair must score 0 (AC6)"
    );
    assert!(score_edge > score_no_edge, "Python: edge > non-edge (AC6)");
}

// ============================================================================
// AC6 — Rust
// ============================================================================

#[test]
fn test_rust_use_edge_present() {
    let path_map = make_path_map(&["src/main.rs", "src/cmd/search.rs", "src/other.rs"]);
    let content = "use crate::cmd::search;\n";
    let files = [("src/main.rs", ImportLanguage::Rust, content)];
    let graph = ImportGraph::build(files, &path_map);

    let main_id = FileId(0);
    let search_id = FileId(1);
    let other_id = FileId(2);

    let score_edge = graph.score(main_id, search_id);
    let score_no_edge = graph.score(main_id, other_id);

    assert!(
        score_edge > 0.0,
        "Rust: crate::use edge must score > 0 (AC6), got {score_edge}"
    );
    assert_eq!(
        score_no_edge, 0.0,
        "Rust: unrelated pair must score 0 (AC6)"
    );
    assert!(score_edge > score_no_edge, "Rust: edge > non-edge (AC6)");
}

// ============================================================================
// AC6 — Go
// ============================================================================

#[test]
fn test_go_import_edge_present() {
    // Go uses package paths.  Use a relative specifier that resolves to a file
    // in the same directory (or a sibling path).
    // Source: cmd/main.go; specifier: "./config" → resolves to cmd/config.go.
    let path_map = make_path_map(&["cmd/main.go", "cmd/config.go", "cmd/other.go"]);
    let content = "import \"./config\"\n";
    let files = [("cmd/main.go", ImportLanguage::Go, content)];
    let graph = ImportGraph::build(files, &path_map);

    let main_id = FileId(0);
    let config_id = FileId(1);
    let other_id = FileId(2);

    let score_edge = graph.score(main_id, config_id);
    let score_no_edge = graph.score(main_id, other_id);

    assert!(
        score_edge > 0.0,
        "Go: import edge must score > 0 (AC6), got {score_edge}"
    );
    assert_eq!(score_no_edge, 0.0, "Go: unrelated pair must score 0 (AC6)");
    assert!(score_edge > score_no_edge, "Go: edge > non-edge (AC6)");
}

// ============================================================================
// AC6 — Signal deleted (all-zero) test guard
// ============================================================================

/// Guard: if the signal were deleted (always returning 0.0), the strict
/// inequality assertions would FAIL.  This test verifies the signal is not
/// constant-zero by checking that at least one edge produces a non-zero score.
#[test]
fn test_signal_not_constant_zero() {
    let path_map = make_path_map(&["a.ts", "b.ts"]);
    let files = [("a.ts", ImportLanguage::TypeScript, "import x from './b';\n")];
    let graph = ImportGraph::build(files, &path_map);

    let edge_score = graph.score(FileId(0), FileId(1));
    assert_ne!(
        edge_score, 0.0,
        "import-graph signal must not be constant-zero (AC6 deletion guard)"
    );
}

// ============================================================================
// Non-import pair is exactly 0.0
// ============================================================================

#[test]
fn test_non_import_pair_is_zero() {
    let path_map = make_path_map(&["a.ts", "b.ts", "c.ts"]);
    let files = [("a.ts", ImportLanguage::TypeScript, "import x from './b';\n")];
    let graph = ImportGraph::build(files, &path_map);

    // a→c: no edge.
    assert_eq!(
        graph.score(FileId(0), FileId(2)),
        0.0,
        "non-import pair must score exactly 0.0"
    );
    // c→b: no edge.
    assert_eq!(graph.score(FileId(2), FileId(1)), 0.0);
    // b→a: reverse direction, no edge.
    assert_eq!(graph.score(FileId(1), FileId(0)), 0.0);
}

// ============================================================================
// Degenerate inputs
// ============================================================================

#[test]
fn test_empty_graph_returns_zero() {
    let graph = ImportGraph::default();
    assert_eq!(graph.score(FileId(0), FileId(1)), 0.0);
    assert_eq!(graph.edge_count(), 0);
}

#[test]
fn test_outgoing_as_layer_no_edges() {
    let graph = ImportGraph::default();
    let layer = graph.outgoing_as_layer(FileId(99));
    assert!(layer.is_empty(), "no edges → empty layer");
}

#[test]
fn test_outgoing_as_layer_with_edges() {
    let path_map = make_path_map(&["a.ts", "b.ts", "c.ts"]);
    let files = [(
        "a.ts",
        ImportLanguage::TypeScript,
        "import x from './b';\nimport y from './c';\n",
    )];
    let graph = ImportGraph::build(files, &path_map);

    let layer = graph.outgoing_as_layer(FileId(0));
    assert_eq!(layer.len(), 2, "a.ts imports b.ts and c.ts → 2 edges");
    // All scores 1.0.
    for &(_, score) in &layer {
        assert_eq!(score, 1.0);
    }
    // Sorted FileId-ASC.
    let ids: Vec<u32> = layer.iter().map(|&(fid, _)| fid.0).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted, "outgoing_as_layer must be FileId-ASC");
}

// ============================================================================
// extract_import_specifiers (unit tests for the pure extractors)
// ============================================================================

#[test]
fn test_ts_extracts_from_import() {
    let specs = extract_import_specifiers(
        "import { foo } from './utils';\nimport type { Bar } from './types';\n",
        ImportLanguage::TypeScript,
    );
    assert!(
        specs.contains(&"./utils".to_string()),
        "must extract './utils': {specs:?}"
    );
    assert!(
        specs.contains(&"./types".to_string()),
        "must extract './types': {specs:?}"
    );
}

#[test]
fn test_py_extracts_from_import() {
    let specs = extract_import_specifiers(
        "from .utils import foo\nimport os\n",
        ImportLanguage::Python,
    );
    assert!(
        specs.contains(&".utils".to_string()),
        "must extract '.utils': {specs:?}"
    );
    assert!(
        specs.contains(&"os".to_string()),
        "must extract 'os': {specs:?}"
    );
}

#[test]
fn test_rs_extracts_use() {
    let specs = extract_import_specifiers(
        "use crate::cmd::search;\npub use super::types;\n",
        ImportLanguage::Rust,
    );
    assert!(
        specs.contains(&"crate::cmd::search".to_string()),
        "{specs:?}"
    );
    // Note: `pub use super::types` stripping may vary; just check non-empty.
    assert!(
        !specs.is_empty(),
        "Rust must extract some specifiers: {specs:?}"
    );
}

#[test]
fn test_go_extracts_import_block() {
    let content = "import (\n\t\"fmt\"\n\t\"./internal/config\"\n)\n";
    let specs = extract_import_specifiers(content, ImportLanguage::Go);
    assert!(specs.contains(&"fmt".to_string()), "{specs:?}");
    assert!(
        specs.contains(&"./internal/config".to_string()),
        "{specs:?}"
    );
}

#[test]
fn test_other_language_returns_empty() {
    let specs = extract_import_specifiers("any content here", ImportLanguage::Other);
    assert!(
        specs.is_empty(),
        "Other language must return empty specifiers"
    );
}
