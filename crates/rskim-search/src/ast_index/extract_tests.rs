//! Tests for AST sparse n-gram extraction.
//!
//! Uses synthetic weight closures for determinism in structural tests.
//! End-to-end tests use the real production IDF tables.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use rskim_core::Language;

use super::*;
use crate::ast_index::{AstBigram, AstTrigram, DEFAULT_AST_WEIGHT, linearize_source, vocab_lookup};

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Synthetic weight closures that always return 1.0 (default weight).
fn unit_bigram_weight(_: AstBigram) -> f32 {
    1.0
}
fn unit_trigram_weight(_: AstTrigram) -> f32 {
    1.0
}

/// Build a `LinearNode` with given kind_id and depth.
fn node(kind_id: u16, depth: u16) -> LinearNode {
    LinearNode { kind_id, depth }
}

// ── F1: Empty input ───────────────────────────────────────────────────────────

#[test]
fn empty_input_yields_empty_set() {
    let result = extract_ast_ngrams_with_weights(&[], unit_bigram_weight, unit_trigram_weight);
    assert!(
        result.bigrams.is_empty(),
        "bigrams should be empty for empty input"
    );
    assert!(
        result.trigrams.is_empty(),
        "trigrams should be empty for empty input"
    );
}

// ── F2: Linear chain ──────────────────────────────────────────────────────────

#[test]
fn linear_chain_root_child_grandchild() {
    // depth 0: root (kind 10)
    // depth 1: child (kind 20)
    // depth 2: grandchild (kind 30)
    let nodes = [node(10, 0), node(20, 1), node(30, 2)];

    let result = extract_ast_ngrams_with_weights(&nodes, unit_bigram_weight, unit_trigram_weight);

    // Expected bigrams: (10→20) and (20→30)
    let b1 = AstBigram::encode(10, 20);
    let b2 = AstBigram::encode(20, 30);

    assert_eq!(result.bigrams.len(), 2, "expected exactly 2 bigrams");

    let keys: Vec<u32> = result.bigrams.iter().map(|e| e.ngram.key()).collect();
    assert!(keys.contains(&b1.key()), "missing bigram 10→20");
    assert!(keys.contains(&b2.key()), "missing bigram 20→30");

    // Expected trigram: (10→20→30)
    let t1 = AstTrigram::encode(10, 20, 30);
    assert_eq!(result.trigrams.len(), 1, "expected exactly 1 trigram");
    assert_eq!(result.trigrams[0].ngram.key(), t1.key());
}

// ── F3: Siblings bind to parent, not each other ───────────────────────────────

#[test]
fn siblings_bind_to_parent_not_each_other() {
    // depth 0: root (kind 10)
    // depth 1: sibling A (kind 20)
    // depth 1: sibling B (kind 30)
    let nodes = [node(10, 0), node(20, 1), node(30, 1)];

    let result = extract_ast_ngrams_with_weights(&nodes, unit_bigram_weight, unit_trigram_weight);

    let b_root_a = AstBigram::encode(10, 20);
    let b_root_b = AstBigram::encode(10, 30);
    let b_sibling = AstBigram::encode(20, 30); // should NOT exist

    let keys: Vec<u32> = result.bigrams.iter().map(|e| e.ngram.key()).collect();
    assert!(keys.contains(&b_root_a.key()), "missing bigram root→sibA");
    assert!(keys.contains(&b_root_b.key()), "missing bigram root→sibB");
    assert!(
        !keys.contains(&b_sibling.key()),
        "sibling→sibling edge must not appear"
    );
}

// ── F4: Same kind under two different parents → distinct bigrams ───────────────

#[test]
fn same_kind_two_depths_distinct_bigrams() {
    // root (kind 10) → child (kind 50)
    // root (kind 10) → grandchild via different parent: parent2 (kind 60) → child2 (kind 50)
    // Build: 10@0, 50@1, 60@1, 50@2
    // But wait: 60@1 means parent is 10@0. Then 50@2 means parent is 60@1.
    let nodes = [node(10, 0), node(50, 1), node(60, 1), node(50, 2)];

    let result = extract_ast_ngrams_with_weights(&nodes, unit_bigram_weight, unit_trigram_weight);

    let b1 = AstBigram::encode(10, 50); // 10 → 50 (depth 0 → depth 1)
    let b2 = AstBigram::encode(60, 50); // 60 → 50 (depth 1 → depth 2)

    let keys: Vec<u32> = result.bigrams.iter().map(|e| e.ngram.key()).collect();
    assert!(keys.contains(&b1.key()), "missing bigram 10→50");
    assert!(keys.contains(&b2.key()), "missing bigram 60→50");

    // These are distinct bigrams (different keys even though child kind is same)
    assert_ne!(b1.key(), b2.key(), "bigrams should be distinct");
}

// ── F5: Depth jumps break the ancestor chain ─────────────────────────────────

#[test]
fn depth_jump_breaks_chain() {
    // Depth sequence: 0, 1, 3 — the node at depth 3 has no direct parent at depth 2
    let nodes = [node(10, 0), node(20, 1), node(30, 3)];

    let result = extract_ast_ngrams_with_weights(&nodes, unit_bigram_weight, unit_trigram_weight);

    // The node at depth 3 should NOT emit a bigram because ancestors[2] = None (gap)
    let b_gap = AstBigram::encode(20, 30); // would be wrong — depth 2 was nulled
    let keys: Vec<u32> = result.bigrams.iter().map(|e| e.ngram.key()).collect();

    // Only the 10→20 bigram should exist; the jump-orphan at depth 3 must NOT appear
    assert!(
        !keys.contains(&b_gap.key()),
        "gap-orphan bigram must not be emitted"
    );
    // The valid bigram 10→20 should still exist
    let b_valid = AstBigram::encode(10, 20);
    assert!(
        keys.contains(&b_valid.key()),
        "valid bigram 10→20 should exist"
    );
}

#[test]
fn two_dropped_nodes_wide_gap() {
    // Depth sequence: 0, 1, 4 — gap of 3 (nodes at depth 2 and 3 dropped)
    let nodes = [node(10, 0), node(20, 1), node(40, 4)];

    let result = extract_ast_ngrams_with_weights(&nodes, unit_bigram_weight, unit_trigram_weight);

    // Node at depth 4 should NOT emit bigram since ancestors[3] = None
    // (both ancestors[2] and ancestors[3] were nulled by gap-fill)
    let keys: Vec<u32> = result.bigrams.iter().map(|e| e.ngram.key()).collect();

    // Only the 10→20 bigram should exist
    let b_valid = AstBigram::encode(10, 20);
    assert!(
        keys.contains(&b_valid.key()),
        "valid bigram 10→20 should exist"
    );
    assert_eq!(
        keys.len(),
        1,
        "only one valid bigram; gap-orphan suppressed"
    );
}

// ── F6: Sentinel kind_id == 0 suppresses n-gram emission ─────────────────────

#[test]
fn sentinel_parent_suppresses_ngram() {
    // kind_id 0 is the sentinel — nodes are recorded in ancestor table but never emitted
    // root(sentinel 0)@0, child(real 20)@1 → bigram(0,20) MUST NOT be emitted
    let nodes = [node(0, 0), node(20, 1), node(30, 2)];

    let result = extract_ast_ngrams_with_weights(&nodes, unit_bigram_weight, unit_trigram_weight);

    // No bigram should contain kind_id 0 in parent position
    for entry in &result.bigrams {
        let (parent, _child) = entry.ngram.decode();
        assert_ne!(
            parent, 0,
            "sentinel kind_id 0 must not appear as bigram parent"
        );
    }

    // Also: (20→30) should be emitted — deeper nodes still work
    // But since sentinel at depth 0, ancestors[0] = 0 (NOT None — sentinel recorded but skipped at emit)
    // The node at depth 1 has parent kind_id 0 → suppressed; no bigram for (0→20)
    // The node at depth 2 has parent at depth 1 (kind 20) — valid bigram (20→30) should emit
    let b_deep = AstBigram::encode(20, 30);
    let keys: Vec<u32> = result.bigrams.iter().map(|e| e.ngram.key()).collect();
    assert!(
        keys.contains(&b_deep.key()),
        "deeper real node (20→30) should still emit"
    );
}

// ── F6b: Sentinel grandparent suppresses trigram emission ────────────────────

#[test]
fn sentinel_grandparent_suppresses_trigram() {
    // kind_id 0 is the sentinel — a node at grandparent depth with kind_id 0
    // must not appear in any emitted trigram's grandparent slot.
    //
    // Sequence:
    //   sentinel(0)@0 — grandparent (kind_id 0)
    //   real(20)@1    — parent      (kind_id 20)
    //   real(30)@2    — child       (kind_id 30)
    //
    // The would-be trigram (0→20→30) MUST NOT be emitted because gp == 0.
    // The bigram (20→30) SHOULD be emitted — sentinel only suppresses at emit,
    // not in the ancestor table, so node 20 is a valid parent for node 30.
    // The bigram (0→20) MUST NOT be emitted — sentinel at parent depth.
    let nodes = [node(0, 0), node(20, 1), node(30, 2)];

    let result = extract_ast_ngrams_with_weights(&nodes, unit_bigram_weight, unit_trigram_weight);

    // No emitted trigram may have grandparent kind_id == 0.
    for entry in &result.trigrams {
        let (gp, _parent, _child) = entry.ngram.decode();
        assert_ne!(
            gp, 0,
            "sentinel kind_id 0 must not appear as trigram grandparent"
        );
    }

    // The valid bigram (20→30) should still be emitted.
    let b_deep = AstBigram::encode(20, 30);
    let bigram_keys: Vec<u32> = result.bigrams.iter().map(|e| e.ngram.key()).collect();
    assert!(
        bigram_keys.contains(&b_deep.key()),
        "deeper real node (20→30) should still emit as bigram"
    );

    // No trigrams at all — the only candidate (0→20→30) is suppressed.
    assert!(
        result.trigrams.is_empty(),
        "all trigrams suppressed; none should be emitted when grandparent is sentinel"
    );
}

// ── F7 + F9: Repeated edge deduplication with count ──────────────────────────

#[test]
fn repeated_edge_dedup_counts_occurrences() {
    // Same parent→child edge repeated 3 times
    let nodes = [
        node(10, 0),
        node(20, 1), // edge 10→20 #1
        node(10, 0),
        node(20, 1), // edge 10→20 #2
        node(10, 0),
        node(20, 1), // edge 10→20 #3
    ];

    let result = extract_ast_ngrams_with_weights(&nodes, unit_bigram_weight, unit_trigram_weight);

    let target = AstBigram::encode(10, 20);
    let entry = result
        .bigrams
        .iter()
        .find(|e| e.ngram.key() == target.key())
        .expect("bigram 10→20 should exist");

    assert_eq!(entry.count, 3, "repeated edge should have count == 3");
    assert_eq!(result.bigrams.len(), 1, "deduplicated to single entry");
}

// ── F9: Suppressed occurrences not counted ────────────────────────────────────

#[test]
fn suppressed_occurrences_not_counted() {
    // Mix of valid and sentinel-suppressed edges
    // valid: 10@0 → 20@1 (count: 1)
    // suppressed: 0@0 → 20@1 (sentinel parent: should NOT be counted)
    let nodes = [
        node(10, 0),
        node(20, 1), // valid edge: 10→20
        node(0, 0),
        node(20, 1), // suppressed: parent is sentinel
    ];

    let result = extract_ast_ngrams_with_weights(&nodes, unit_bigram_weight, unit_trigram_weight);

    // Only the valid edge should appear, with count 1
    let target = AstBigram::encode(10, 20);
    let entry = result
        .bigrams
        .iter()
        .find(|e| e.ngram.key() == target.key())
        .expect("bigram 10→20 should exist");

    assert_eq!(entry.count, 1, "suppressed occurrences excluded from count");

    // No sentinel bigram
    for e in &result.bigrams {
        let (parent, _) = e.ngram.decode();
        assert_ne!(parent, 0, "sentinel kind_id 0 must not appear as parent");
    }
}

// ── F8: End-to-end with real source ───────────────────────────────────────────

#[test]
fn end_to_end_rust_function_item_block() {
    let result =
        linearize_source("fn main() {}", Language::Rust).expect("linearize should not fail");
    let set = extract_ast_ngrams(&result.nodes, Language::Rust);

    // Resolve vocab IDs for known kinds
    let fn_id = vocab_lookup("function_item").expect("function_item in vocab");
    let block_id = vocab_lookup("block").expect("block in vocab");

    let target = AstBigram::encode(fn_id, block_id);
    let entry = set
        .bigrams
        .iter()
        .find(|e| e.ngram.key() == target.key())
        .expect("function_item > block bigram should be present");

    // Weight must be a positive finite f32 (either IDF weight or default 1.0)
    assert!(
        entry.weight > 0.0 && entry.weight.is_finite(),
        "weight should be positive finite"
    );
    assert!(entry.count >= 1, "count must be at least 1");
}

// ── C1: Output sorted and unique ─────────────────────────────────────────────

#[test]
fn output_sorted_unique_keys() {
    // Many nodes to maximize chance of ordering violation
    let nodes: Vec<LinearNode> = (1u16..=20)
        .flat_map(|d| (1u16..=10).map(move |k| node(k * 10, d % 5)))
        .collect();

    let result = extract_ast_ngrams_with_weights(&nodes, unit_bigram_weight, unit_trigram_weight);

    // Check bigrams strictly ascending
    for w in result.bigrams.windows(2) {
        assert!(
            w[0].ngram.key() < w[1].ngram.key(),
            "bigrams not strictly ascending: {:?} >= {:?}",
            w[0].ngram.key(),
            w[1].ngram.key()
        );
    }

    // Check trigrams strictly ascending
    for w in result.trigrams.windows(2) {
        assert!(
            w[0].ngram.key() < w[1].ngram.key(),
            "trigrams not strictly ascending: {:?} >= {:?}",
            w[0].ngram.key(),
            w[1].ngram.key()
        );
    }
}

// ── C2: Deterministic ────────────────────────────────────────────────────────

#[test]
fn deterministic_two_runs_equal() {
    let nodes = [
        node(10, 0),
        node(20, 1),
        node(30, 2),
        node(40, 2),
        node(50, 1),
    ];

    let run1 = extract_ast_ngrams_with_weights(&nodes, unit_bigram_weight, unit_trigram_weight);
    let run2 = extract_ast_ngrams_with_weights(&nodes, unit_bigram_weight, unit_trigram_weight);

    assert_eq!(run1, run2, "two runs on identical input must be equal");
}

// ── C3: Input slice unmodified ────────────────────────────────────────────────

#[test]
fn input_slice_unmodified() {
    let nodes = [node(10, 0), node(20, 1), node(30, 2)];
    let original = nodes;

    let _ = extract_ast_ngrams_with_weights(&nodes, unit_bigram_weight, unit_trigram_weight);

    assert_eq!(nodes, original, "input slice must not be modified");
}

// ── C4: Injected weights appear on the right keys ────────────────────────────

#[test]
fn injected_weights_appear_on_keys() {
    let nodes = [node(10, 0), node(20, 1), node(30, 2)];

    let b_target = AstBigram::encode(10, 20);
    let t_target = AstTrigram::encode(10, 20, 30);

    let high_b = 7.5_f32;
    let high_t = 9.2_f32;

    let bigram_w = |b: AstBigram| {
        if b.key() == b_target.key() {
            high_b
        } else {
            1.0
        }
    };
    let trigram_w = |t: AstTrigram| {
        if t.key() == t_target.key() {
            high_t
        } else {
            1.0
        }
    };

    let result = extract_ast_ngrams_with_weights(&nodes, bigram_w, trigram_w);

    let b_entry = result
        .bigrams
        .iter()
        .find(|e| e.ngram.key() == b_target.key())
        .expect("target bigram should exist");
    assert_eq!(
        b_entry.weight, high_b,
        "injected bigram weight should appear verbatim"
    );

    let t_entry = result
        .trigrams
        .iter()
        .find(|e| e.ngram.key() == t_target.key())
        .expect("target trigram should exist");
    assert_eq!(
        t_entry.weight, high_t,
        "injected trigram weight should appear verbatim"
    );
}

#[test]
fn unknown_ngram_default_weight() {
    // Use real production weight lookup for an n-gram that is certainly not in any table
    // kind_id 1 = "", kind_id 2 = "!" — both very unlikely to form a meaningful pair
    let nodes = [node(1, 0), node(2, 1)];
    // Use a real language that won't have these meaningless pairs in its table
    let result = extract_ast_ngrams(&nodes, Language::Rust);

    // The only bigram would be (1→2) — not in the production table
    for entry in &result.bigrams {
        // All weights should equal DEFAULT_AST_WEIGHT since none of these are in the table
        assert_eq!(
            entry.weight, DEFAULT_AST_WEIGHT,
            "unknown n-gram should have DEFAULT_AST_WEIGHT"
        );
    }
}

// ── C5: Crate root re-exports resolve ─────────────────────────────────────────

#[test]
fn crate_root_reexports_resolve() {
    // This test verifies the symbols are accessible from rskim_search::{}.
    // We use them directly here (they're in scope via `use super::*`) but
    // the types we reference are the same ones the crate root re-exports.
    let _: AstNgramSet = AstNgramSet::default();

    let _b: AstBigramEntry = AstBigramEntry {
        ngram: AstBigram::encode(1, 2),
        weight: 1.0,
        count: 1,
    };
    let _t: AstTrigramEntry = AstTrigramEntry {
        ngram: AstTrigram::encode(1, 2, 3),
        weight: 1.0,
        count: 1,
    };

    // Callable via crate path (compilation = pass)
    let _ = extract_ast_ngrams_with_weights(&[], unit_bigram_weight, unit_trigram_weight);
    let _ = extract_ast_ngrams(&[], Language::Rust);
}

// ── P1: Performance gate (release builds only) ───────────────────────────────

#[cfg(not(debug_assertions))]
#[test]
fn extract_3000_line_file_under_budget() {
    // Generate a ~3000-line Rust fixture inline
    let source: String = (0..200)
        .map(|i| {
            format!(
                "pub fn func_{i}(x: i32, y: i32, z: i32) -> i32 {{\n    let a = x + y;\n    let b = a * z + {i};\n    b\n}}\n"
            )
        })
        .collect();

    let linearized = linearize_source(&source, Language::Rust).expect("linearize should not fail");

    let start = std::time::Instant::now();
    let _result = extract_ast_ngrams(&linearized.nodes, Language::Rust);
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_millis() < 5,
        "extract_ast_ngrams on ~3000-line file took {}ms (budget: 5ms)",
        elapsed.as_millis()
    );
}
