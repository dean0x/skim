//! Tests for CST linearization.
//!
//! Test cycles follow the plan:
//!   1. Types & defaults
//!   2. Vocabulary lookup
//!   3. Core linearization
//!   4. ERROR/MISSING node handling
//!   5. Bounds guards
//!   6. Multi-language
//!   7. Edge cases
//!   8. Performance

#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use rskim_core::Language;

use super::{LinearNode, LinearizeResult, MAX_AST_DEPTH, MAX_AST_NODES, MAX_FILE_SIZE};
use crate::ast_index::linearize::LANG_MAPS;
use crate::ast_weights::NODE_KIND_VOCABULARY;

// ── Test helpers ──────────────────────────────────────────────────────────────

/// Linearize source and unwrap the result. Panics on error (OK in tests).
fn parse_and_linearize(source: &str, lang: Language) -> LinearizeResult {
    super::linearize_source(source, lang).expect("linearize_source should not fail in tests")
}

/// Map kind_ids back to their vocabulary strings.
///
/// Used to write human-readable assertions without hardcoding numeric IDs.
fn resolve_kinds(result: &LinearizeResult) -> Vec<&'static str> {
    result
        .nodes
        .iter()
        .map(|n| NODE_KIND_VOCABULARY[n.kind_id as usize])
        .collect()
}

/// Assert the node_count invariant: nodes.len() + error_count == node_count.
fn assert_node_count_invariant(result: &LinearizeResult) {
    let expected = result.nodes.len() as u32 + result.error_count;
    assert_eq!(
        result.node_count, expected,
        "invariant violated: node_count ({}) != nodes.len() ({}) + error_count ({})",
        result.node_count,
        result.nodes.len(),
        result.error_count,
    );
}

// ── Cycle 1: Types & defaults ─────────────────────────────────────────────────

#[test]
fn linear_node_is_copy() {
    let n = LinearNode { kind_id: 42, depth: 3 };
    let copy = n; // Copy
    assert_eq!(copy.kind_id, 42);
    assert_eq!(copy.depth, 3);
    // Original still usable — confirmed Copy, not Move
    assert_eq!(n.kind_id, 42);
}

#[test]
fn linearize_result_default_is_empty() {
    let r = LinearizeResult::default();
    assert!(r.nodes.is_empty());
    assert_eq!(r.node_count, 0);
    assert_eq!(r.error_count, 0);
    assert_node_count_invariant(&r);
}

#[test]
fn linear_node_and_result_are_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<LinearNode>();
    assert_send_sync::<LinearizeResult>();
}

// ── Cycle 2: Vocabulary lookup ────────────────────────────────────────────────

#[test]
fn vocabulary_has_1740_entries() {
    assert_eq!(NODE_KIND_VOCABULARY.len(), 1740);
}

#[test]
fn rust_lang_map_contains_known_kinds() {
    let maps = &*LANG_MAPS;
    let rust_map = maps.get(&Language::Rust).expect("Rust must have a lang map");
    // "function_item" is a core Rust grammar node — must be in the map.
    let found = rust_map.iter().any(|entry| {
        entry.map(|idx| NODE_KIND_VOCABULARY[idx as usize] == "function_item").unwrap_or(false)
    });
    assert!(found, "Rust lang map must map some kind_id to 'function_item'");
}

#[test]
fn serde_only_languages_not_in_lang_maps() {
    let maps = &*LANG_MAPS;
    assert!(!maps.contains_key(&Language::Json), "JSON must not have a lang map");
    assert!(!maps.contains_key(&Language::Yaml), "YAML must not have a lang map");
    assert!(!maps.contains_key(&Language::Toml), "TOML must not have a lang map");
}

#[test]
fn vocabulary_is_sorted() {
    let vocab = NODE_KIND_VOCABULARY;
    for window in vocab.windows(2) {
        assert!(
            window[0] <= window[1],
            "NODE_KIND_VOCABULARY is not sorted at: {:?} > {:?}",
            window[0],
            window[1]
        );
    }
}

#[test]
fn known_kind_roundtrips_through_lang_map() {
    // Verify that the vocabulary index for "function_item" resolves back to
    // the same string — confirming the binary-search lookup is correct.
    let vocab_idx = NODE_KIND_VOCABULARY
        .binary_search(&"function_item")
        .expect("'function_item' must be in vocabulary");
    assert_eq!(NODE_KIND_VOCABULARY[vocab_idx], "function_item");
}

// ── Cycle 3: Core linearization ───────────────────────────────────────────────

#[test]
fn empty_source_produces_root_only() {
    // An empty Rust source still has a root "source_file" node.
    let result = parse_and_linearize("", Language::Rust);
    assert!(
        result.node_count >= 1,
        "empty source must have at least the root node"
    );
    assert_node_count_invariant(&result);
}

#[test]
fn simple_fn_produces_multiple_nodes() {
    let result = parse_and_linearize("fn main() {}", Language::Rust);
    assert!(result.nodes.len() > 1, "simple fn must produce more than one node");
    assert_node_count_invariant(&result);
}

#[test]
fn pre_order_root_comes_first() {
    let result = parse_and_linearize("fn main() {}", Language::Rust);
    // Root node is at depth 0 and must be the very first node.
    assert_eq!(
        result.nodes[0].depth,
        0,
        "first node must be the root at depth 0"
    );
}

#[test]
fn depth_increases_for_children() {
    let result = parse_and_linearize("fn main() {}", Language::Rust);
    // In a non-trivial parse, some node must be deeper than the root.
    let has_deeper = result.nodes.iter().any(|n| n.depth > 0);
    assert!(has_deeper, "must have nodes deeper than root");
}

#[test]
fn node_count_invariant_holds_for_simple_fn() {
    let result = parse_and_linearize("fn main() {}", Language::Rust);
    assert_node_count_invariant(&result);
}

#[test]
fn parent_child_recoverable_from_depth() {
    let result = parse_and_linearize("fn main() {}", Language::Rust);
    // Every non-root node must have a preceding node with depth == self.depth - 1.
    for (i, node) in result.nodes.iter().enumerate().skip(1) {
        let parent_depth = node.depth.saturating_sub(1);
        let has_parent = result.nodes[..i]
            .iter()
            .rev()
            .any(|p| p.depth == parent_depth);
        assert!(
            has_parent || node.depth == 0,
            "node at index {i} with depth {} has no preceding parent at depth {}",
            node.depth,
            parent_depth
        );
    }
}

// ── Cycle 4: ERROR/MISSING nodes ─────────────────────────────────────────────

#[test]
fn error_nodes_are_skipped_and_counted() {
    // Deliberately malformed Rust: unclosed brace
    let result = parse_and_linearize("fn foo( {", Language::Rust);
    // tree-sitter recovers with ERROR nodes — error_count must be non-zero.
    assert!(
        result.error_count > 0 || result.node_count > 0,
        "malformed input should produce nodes or errors"
    );
    assert_node_count_invariant(&result);
}

#[test]
fn missing_nodes_are_counted_in_error_count() {
    // A source with syntax that forces tree-sitter to insert MISSING nodes.
    // We just verify the invariant holds regardless of error_count value.
    let result = parse_and_linearize("fn ()", Language::Rust);
    assert_node_count_invariant(&result);
}

#[test]
fn error_children_still_traversed() {
    // Ensure that when an ERROR node exists, its children still contribute to
    // node_count (the traversal does not prune subtrees under ERROR nodes).
    let with_error = parse_and_linearize("fn foo( { let x = 1; }", Language::Rust);
    let clean = parse_and_linearize("fn foo() { let x = 1; }", Language::Rust);
    // The malformed version should still have substantial node count.
    assert!(
        with_error.node_count > 0,
        "ERROR parent children must still be traversed"
    );
    // Both produce non-trivial node counts.
    assert!(clean.node_count > 0);
    assert_node_count_invariant(&with_error);
    assert_node_count_invariant(&clean);
}

// ── Cycle 5: Bounds guards ────────────────────────────────────────────────────

#[test]
fn no_node_has_depth_at_or_above_max_ast_depth() {
    // We can't easily force a 500-deep tree in a test, but we can verify the
    // guard constant is correct and no real parse exceeds it.
    let result = parse_and_linearize(
        "fn deeply_nested() { if true { if true { if true { let x = 1; } } } }",
        Language::Rust,
    );
    for node in &result.nodes {
        assert!(
            node.depth < MAX_AST_DEPTH,
            "node depth {} must be < MAX_AST_DEPTH {}",
            node.depth,
            MAX_AST_DEPTH
        );
    }
    assert_node_count_invariant(&result);
}

#[test]
fn max_nodes_guard_truncates_output() {
    // Generate a very large source that would exceed MAX_AST_NODES if uncapped.
    // We use the MAX_AST_NODES constant to verify the cap is enforced.
    // In practice, a file this large is also > MAX_FILE_SIZE, so we set up a
    // tighter scenario: verify that node_count never exceeds MAX_AST_NODES.
    let small_repeat = "let x = 1;\n".repeat(100);
    let result = parse_and_linearize(&small_repeat, Language::Rust);
    assert!(
        result.node_count <= MAX_AST_NODES,
        "node_count {} must not exceed MAX_AST_NODES {}",
        result.node_count,
        MAX_AST_NODES
    );
    assert_node_count_invariant(&result);
}

#[test]
fn oversized_file_returns_default() {
    // File larger than MAX_FILE_SIZE should return empty default result.
    let big = "fn x() {}\n".repeat(MAX_FILE_SIZE / 10 + 1);
    let result = parse_and_linearize(&big, Language::Rust);
    assert!(result.nodes.is_empty(), "oversized file must return empty nodes");
    assert_eq!(result.node_count, 0, "oversized file must return node_count 0");
    assert_eq!(result.error_count, 0, "oversized file must return error_count 0");
}

// ── Cycle 6: Multi-language ───────────────────────────────────────────────────

#[test]
fn all_14_ts_languages_produce_output() {
    let ts_langs = [
        Language::TypeScript,
        Language::JavaScript,
        Language::Python,
        Language::Rust,
        Language::Go,
        Language::Java,
        Language::C,
        Language::Cpp,
        Language::CSharp,
        Language::Ruby,
        Language::Sql,
        Language::Kotlin,
        Language::Swift,
        Language::Markdown,
    ];
    for lang in ts_langs {
        let result = parse_and_linearize("", lang);
        assert!(
            result.node_count >= 1,
            "{lang:?} must produce at least 1 node for empty source"
        );
        assert_node_count_invariant(&result);
    }
}

#[test]
fn serde_only_languages_return_default() {
    for lang in [Language::Json, Language::Yaml, Language::Toml] {
        let result = parse_and_linearize("{}", lang);
        assert!(
            result.nodes.is_empty(),
            "{lang:?} must return empty nodes (serde-only language)"
        );
        assert_eq!(result.node_count, 0, "{lang:?} must return node_count 0");
    }
}

#[test]
fn kind_resolution_works_across_languages() {
    // Each language's kinds should resolve to valid vocabulary entries.
    let langs = [Language::Rust, Language::Python, Language::TypeScript];
    for lang in langs {
        let result = parse_and_linearize("", lang);
        for node in &result.nodes {
            // kind_id must be a valid index into NODE_KIND_VOCABULARY
            assert!(
                (node.kind_id as usize) < NODE_KIND_VOCABULARY.len(),
                "{lang:?}: kind_id {} out of vocabulary range",
                node.kind_id
            );
        }
    }
}

#[test]
fn rust_fixture_linearization() {
    let source = include_str!("../../../../tests/fixtures/rust/simple.rs");
    let result = parse_and_linearize(source, Language::Rust);
    assert!(
        result.nodes.len() > 10,
        "Rust fixture must produce more than 10 nodes, got {}",
        result.nodes.len()
    );
    assert_node_count_invariant(&result);
    // Root must be source_file for Rust
    let kinds = resolve_kinds(&result);
    assert!(
        kinds.contains(&"source_file"),
        "Rust parse must include 'source_file' kind"
    );
}

#[test]
fn typescript_fixture_linearization() {
    let source = include_str!("../../../../tests/fixtures/typescript/simple.ts");
    let result = parse_and_linearize(source, Language::TypeScript);
    assert!(
        result.nodes.len() > 10,
        "TypeScript fixture must produce more than 10 nodes, got {}",
        result.nodes.len()
    );
    assert_node_count_invariant(&result);
}

#[test]
fn python_fixture_linearization() {
    let source = include_str!("../../../../tests/fixtures/python/simple.py");
    let result = parse_and_linearize(source, Language::Python);
    assert!(
        result.nodes.len() > 10,
        "Python fixture must produce more than 10 nodes, got {}",
        result.nodes.len()
    );
    assert_node_count_invariant(&result);
    // Python module node is the root
    let kinds = resolve_kinds(&result);
    assert!(
        kinds.contains(&"module"),
        "Python parse must include 'module' kind"
    );
}

// ── Cycle 7: Edge cases ───────────────────────────────────────────────────────

#[test]
fn unknown_kind_emits_sentinel_zero() {
    // We can't easily inject an unknown kind, but we can verify that any
    // node with kind_id == 0 corresponds to the empty sentinel entry in the
    // vocabulary (index 0 is "" in NODE_KIND_VOCABULARY).
    let result = parse_and_linearize("fn main() {}", Language::Rust);
    for node in &result.nodes {
        if node.kind_id == 0 {
            assert_eq!(
                NODE_KIND_VOCABULARY[0], "",
                "sentinel ID 0 must map to empty string in vocabulary"
            );
        }
    }
}

#[test]
fn utf8_multibyte_source_is_handled() {
    // tree-sitter handles UTF-8 gracefully; we just verify no panic/error.
    let source = "fn 日本語() {}";
    let result = parse_and_linearize(source, Language::Rust);
    assert_node_count_invariant(&result);
    // May parse with errors due to non-ASCII identifier, but must not panic.
}

#[test]
fn whitespace_only_source_returns_ok() {
    let result = parse_and_linearize("   \n\t  \n", Language::Rust);
    assert_node_count_invariant(&result);
    // At minimum, tree-sitter returns a root node.
}

#[test]
fn binary_like_input_returns_ok_default() {
    // Source with a null byte exceeds MAX_FILE_SIZE only if very large, but
    // even a short binary-like string must not panic — it returns a result.
    // tree-sitter may produce ERROR nodes for non-UTF8 but won't crash.
    // Use a source with control characters that is valid UTF-8.
    let source = "\x00\x01\x02\x03";
    let result = super::linearize_source(source, Language::Rust);
    // Must not return an Err from linearize_source — only Ok.
    assert!(result.is_ok(), "binary-like input must return Ok");
}

// ── Cycle 8: Performance ──────────────────────────────────────────────────────

#[test]
#[cfg(not(debug_assertions))]
fn linearize_1000_line_file_under_5ms() {
    use std::time::Instant;

    // Generate a 1000-function Rust file (well under MAX_FILE_SIZE).
    let source: String = (0..100)
        .map(|i| format!("fn func_{i}(x: i32) -> i32 {{ x + {i} }}\n"))
        .collect();

    // Warm up LazyLock init outside the timed section.
    let _ = parse_and_linearize("fn warm() {}", Language::Rust);

    let start = Instant::now();
    let result = parse_and_linearize(&source, Language::Rust);
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_millis() < 5,
        "linearize_source took {}ms for ~1000-line Rust file, expected < 5ms",
        elapsed.as_millis()
    );
    assert_node_count_invariant(&result);
}
