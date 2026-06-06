//! Tests for structural.rs constants, helpers, and counting rules.
//!
//! Tests F1–F5 from the acceptance criteria.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::ast_index::extract::extract_ast_ngrams_with_metrics;
use crate::ast_index::{AstBigram, vocab_resolve};

// ============================================================================
// F5: Synthetic ID isolation — vocab_resolve returns None for all synthetic IDs
// ============================================================================

#[test]
fn f5_synthetic_ids_not_in_vocab() {
    // Parent synthetic IDs
    assert!(
        vocab_resolve(EMPTY_BODY).is_none(),
        "EMPTY_BODY={EMPTY_BODY} must not be in the vocabulary"
    );
    assert!(
        vocab_resolve(DEEP_NODE).is_none(),
        "DEEP_NODE={DEEP_NODE} must not be in the vocabulary"
    );
    assert!(
        vocab_resolve(LARGE_BODY).is_none(),
        "LARGE_BODY={LARGE_BODY} must not be in the vocabulary"
    );
    assert!(
        vocab_resolve(MANY_PARAMS).is_none(),
        "MANY_PARAMS={MANY_PARAMS} must not be in the vocabulary"
    );

    // Bucket labels for each dimension's edge indices
    for (i, _) in BODY_STMT_EDGES.iter().enumerate() {
        let id = bucket_label(i);
        assert!(
            vocab_resolve(id).is_none(),
            "bucket_label({i})={id} must not be in the vocabulary"
        );
    }
    for (i, _) in PARAM_EDGES.iter().enumerate() {
        let id = bucket_label(i);
        assert!(
            vocab_resolve(id).is_none(),
            "param bucket_label({i})={id} not in vocab"
        );
    }
    for (i, _) in DEPTH_EDGES.iter().enumerate() {
        let id = bucket_label(i);
        assert!(
            vocab_resolve(id).is_none(),
            "depth bucket_label({i})={id} not in vocab"
        );
    }
}

#[test]
fn f5_bucket_label_range_is_safe() {
    // All bucket labels (0..MAX_BUCKET_EDGES) must be < BUCKET_LABEL_BASE..EMPTY_BODY
    let max_label = BUCKET_LABEL_BASE + MAX_BUCKET_EDGES as NodeKindId - 1;
    assert!(
        max_label < EMPTY_BODY,
        "bucket label range [{}..{}] must not overlap synthetic parent IDs [{}..)",
        BUCKET_LABEL_BASE,
        max_label,
        EMPTY_BODY
    );
}

// ============================================================================
// F2: Central counting rule — is_counted_child
// ============================================================================

#[test]
fn f2_sentinel_is_not_counted() {
    // kind_id == 0 is the punctuation/unknown sentinel
    assert!(
        !is_counted_child(0),
        "sentinel kind_id=0 must NOT be a counted child"
    );
}

#[test]
fn f2_comment_kinds_are_not_counted() {
    use crate::ast_index::vocab_lookup;
    // If "comment" is in vocab, it must not be counted
    if let Some(id) = vocab_lookup("comment") {
        assert!(
            !is_counted_child(id),
            "'comment' kind_id={id} must NOT be a counted child"
        );
    }
    if let Some(id) = vocab_lookup("line_comment") {
        assert!(
            !is_counted_child(id),
            "'line_comment' kind_id={id} must NOT be a counted child"
        );
    }
    if let Some(id) = vocab_lookup("block_comment") {
        assert!(
            !is_counted_child(id),
            "'block_comment' kind_id={id} must NOT be a counted child"
        );
    }
}

#[test]
fn f2_real_statement_kinds_are_counted() {
    use crate::ast_index::vocab_lookup;
    // A real, non-comment kind that IS in the vocab should be counted
    if let Some(id) = vocab_lookup("function_item") {
        assert!(
            is_counted_child(id),
            "'function_item' kind_id={id} must be a counted child"
        );
    }
    if let Some(id) = vocab_lookup("if_statement") {
        assert!(
            is_counted_child(id),
            "'if_statement' kind_id={id} must be a counted child"
        );
    }
}

#[test]
fn f2_punctuation_kinds_are_not_counted() {
    // Regression guard for the PUNCTUATION_KIND_IDS set-membership branch in
    // is_counted_child.  Uses vocab_lookup to resolve real kind_ids so the test
    // remains correct even if the vocabulary is regenerated (applies ADR-003,
    // avoids PF-005).  Tokens that resolve to None are absent from the active
    // grammar vocabulary and are skipped rather than forcing a false failure.
    use crate::ast_index::vocab_lookup;
    for token in &["{", "}", "(", ")", ";", ","] {
        if let Some(id) = vocab_lookup(token) {
            assert!(
                !is_counted_child(id),
                "punctuation token {token:?} kind_id={id} must NOT be a counted child"
            );
        }
    }
}

// ============================================================================
// F1: StructuralMetrics computed from hand-built LinearNode sequences
// ============================================================================

use crate::ast_index::LinearNode;
use rskim_core::Language;

#[test]
fn f1_empty_input_gives_zero_metrics() {
    let (_, m) = extract_ast_ngrams_with_metrics(&[], Language::Rust);
    assert_eq!(m.max_depth, 0);
    assert_eq!(m.max_block_stmts, 0);
    assert_eq!(m.max_params, 0);
    assert_eq!(m.branch_count, 0);
}

#[test]
fn f1_max_depth_tracks_maximum() {
    // Build a simple flat list: depths 0, 1, 2, 3
    let nodes = vec![
        LinearNode {
            kind_id: 1,
            depth: 0,
        },
        LinearNode {
            kind_id: 1,
            depth: 1,
        },
        LinearNode {
            kind_id: 1,
            depth: 2,
        },
        LinearNode {
            kind_id: 1,
            depth: 3,
        },
    ];
    let (_, m) = extract_ast_ngrams_with_metrics(&nodes, Language::Rust);
    assert_eq!(m.max_depth, 3);
}

#[test]
fn f1_max_depth_handles_depth_jump() {
    // A depth jump (0 → 5 via gap): max should still be 5
    let nodes = vec![
        LinearNode {
            kind_id: 1,
            depth: 0,
        },
        LinearNode {
            kind_id: 1,
            depth: 5,
        }, // jump by 5
    ];
    let (_, m) = extract_ast_ngrams_with_metrics(&nodes, Language::Rust);
    assert_eq!(m.max_depth, 5);
}

#[test]
fn f1_branch_count_increments_for_branch_kinds() {
    use crate::ast_index::vocab_lookup;
    // Place an if_statement and a while_statement in the stream
    let if_id = vocab_lookup("if_statement").unwrap_or(0);
    let while_id = vocab_lookup("while_statement").unwrap_or(0);

    // Only count them if they are actually in the vocabulary
    let nodes: Vec<LinearNode> = [if_id, while_id]
        .iter()
        .enumerate()
        .map(|(i, &kid)| LinearNode {
            kind_id: kid,
            depth: i as u16,
        })
        .collect();

    let (_, m) = extract_ast_ngrams_with_metrics(&nodes, Language::Rust);
    // Both should be counted if both IDs are in BRANCH_KIND_IDS
    let expected: u32 = [if_id, while_id]
        .iter()
        .filter(|&&id| id != 0 && BRANCH_KIND_IDS.contains(&id))
        .count() as u32;
    assert_eq!(m.branch_count, expected);
}

// ============================================================================
// F1 (continued): max_block_stmts and max_params scalar values
// ============================================================================

#[test]
fn f1_max_block_stmts_counts_body_children() {
    use crate::ast_index::vocab_lookup;

    let fn_id = match vocab_lookup("function_item") {
        Some(id) => id,
        None => return,
    };
    let block_id = match vocab_lookup("block") {
        Some(id) => id,
        None => return,
    };
    // Use expression_statement or fall back to the first counted kind in vocab.
    let stmt_id = match vocab_lookup("expression_statement") {
        Some(id) if id != 0 && is_counted_child(id) => id,
        _ => (1u16..1740)
            .find(|&id| is_counted_child(id))
            .expect("at least one counted kind exists"),
    };

    // Build: function_item(depth=0) → block(depth=1) → 7×stmt_id(depth=2)
    let mut nodes = vec![
        LinearNode {
            kind_id: fn_id,
            depth: 0,
        },
        LinearNode {
            kind_id: block_id,
            depth: 1,
        },
    ];
    for _ in 0..7 {
        nodes.push(LinearNode {
            kind_id: stmt_id,
            depth: 2,
        });
    }

    let (_, m) = extract_ast_ngrams_with_metrics(&nodes, Language::Rust);
    assert_eq!(
        m.max_block_stmts, 7,
        "block with 7 counted children must yield max_block_stmts == 7"
    );
}

#[test]
fn f1_max_params_counts_parameter_list_children() {
    use crate::ast_index::vocab_lookup;

    let fn_id = match vocab_lookup("function_item") {
        Some(id) => id,
        None => return,
    };
    let params_id = match vocab_lookup("parameters") {
        Some(id) => id,
        None => return,
    };
    // Use a counted-child kind as the stand-in for a parameter node.
    let param_node_id = match vocab_lookup("identifier") {
        Some(id) if id != 0 && is_counted_child(id) => id,
        _ => (1u16..1740)
            .find(|&id| is_counted_child(id))
            .expect("at least one counted kind exists"),
    };

    // Build: function_item(depth=0) → parameters(depth=1) → 3×param_node(depth=2)
    let mut nodes = vec![
        LinearNode {
            kind_id: fn_id,
            depth: 0,
        },
        LinearNode {
            kind_id: params_id,
            depth: 1,
        },
    ];
    for _ in 0..3 {
        nodes.push(LinearNode {
            kind_id: param_node_id,
            depth: 2,
        });
    }

    let (_, m) = extract_ast_ngrams_with_metrics(&nodes, Language::Rust);
    assert_eq!(
        m.max_params, 3,
        "parameter list with 3 counted children must yield max_params == 3"
    );
}

// ============================================================================
// F3: Cumulative bucket emissions at exact boundary values
// ============================================================================

#[test]
fn f3_body_stmt_buckets_emit_cumulatively() {
    use crate::ast_index::extract::extract_ast_ngrams_with_metrics;
    use crate::ast_index::vocab_lookup;

    let fn_id = match vocab_lookup("function_item") {
        Some(id) => id,
        None => return, // skip if not in vocab (should be present for Rust)
    };
    let block_id = match vocab_lookup("block") {
        Some(id) => id,
        None => return,
    };
    // Some real statement kind that is counted
    let expr_id = match vocab_lookup("expression_statement") {
        Some(id) if id != 0 => id,
        _ => (1u16..1740)
            .find(|&id| is_counted_child(id))
            .expect("at least one counted kind exists"),
    };

    // Build: function_item at depth 0, block at depth 1,
    // then `n_stmts` statement nodes at depth 2.
    let build_nodes = |n_stmts: u32| -> Vec<LinearNode> {
        let mut v = vec![
            LinearNode {
                kind_id: fn_id,
                depth: 0,
            },
            LinearNode {
                kind_id: block_id,
                depth: 1,
            },
        ];
        for _ in 0..n_stmts {
            v.push(LinearNode {
                kind_id: expr_id,
                depth: 2,
            });
        }
        v
    };

    // 9 stmts → no body bucket
    let (set9, _) = extract_ast_ngrams_with_metrics(&build_nodes(9), Language::Rust);
    for i in 0..BODY_STMT_EDGES.len() {
        let key = AstBigram::encode(LARGE_BODY, bucket_label(i));
        assert!(
            !set9.bigrams.iter().any(|e| e.ngram == key),
            "9 stmts should not emit LARGE_BODY bucket {i}"
        );
    }

    // 10 stmts → b0 only
    let (set10, _) = extract_ast_ngrams_with_metrics(&build_nodes(10), Language::Rust);
    assert!(
        set10
            .bigrams
            .iter()
            .any(|e| e.ngram == AstBigram::encode(LARGE_BODY, bucket_label(0))),
        "10 stmts must emit LARGE_BODY→bucket_label(0)"
    );
    assert!(
        !set10
            .bigrams
            .iter()
            .any(|e| e.ngram == AstBigram::encode(LARGE_BODY, bucket_label(1))),
        "10 stmts must NOT emit LARGE_BODY→bucket_label(1)"
    );

    // 25 stmts → b0 AND b1
    let (set25, _) = extract_ast_ngrams_with_metrics(&build_nodes(25), Language::Rust);
    assert!(
        set25
            .bigrams
            .iter()
            .any(|e| e.ngram == AstBigram::encode(LARGE_BODY, bucket_label(0))),
        "25 stmts must emit LARGE_BODY→bucket_label(0)"
    );
    assert!(
        set25
            .bigrams
            .iter()
            .any(|e| e.ngram == AstBigram::encode(LARGE_BODY, bucket_label(1))),
        "25 stmts must emit LARGE_BODY→bucket_label(1)"
    );
    assert!(
        !set25
            .bigrams
            .iter()
            .any(|e| e.ngram == AstBigram::encode(LARGE_BODY, bucket_label(2))),
        "25 stmts must NOT emit LARGE_BODY→bucket_label(2)"
    );

    // 40 stmts → b0, b1, AND b2
    let (set40, _) = extract_ast_ngrams_with_metrics(&build_nodes(40), Language::Rust);
    for i in 0..BODY_STMT_EDGES.len() {
        assert!(
            set40
                .bigrams
                .iter()
                .any(|e| e.ngram == AstBigram::encode(LARGE_BODY, bucket_label(i))),
            "40 stmts must emit LARGE_BODY→bucket_label({i})"
        );
    }
}

#[test]
fn f3_depth_buckets_emit_cumulatively() {
    // A node at depth 4 must emit DEEP_NODE→bucket_label(0)
    // A node at depth 6 must also emit bucket_label(1)
    // A node at depth 8 must emit bucket_label(0), bucket_label(1), bucket_label(2)
    let make_nodes_at_depth = |d: u16| -> Vec<LinearNode> {
        (0..=d)
            .map(|depth| LinearNode { kind_id: 1, depth })
            .collect()
    };

    let (set4, _) = extract_ast_ngrams_with_metrics(&make_nodes_at_depth(4), Language::Rust);
    assert!(
        set4.bigrams
            .iter()
            .any(|e| e.ngram == AstBigram::encode(DEEP_NODE, bucket_label(0))),
        "depth 4 must emit DEEP_NODE→bucket_label(0)"
    );
    assert!(
        !set4
            .bigrams
            .iter()
            .any(|e| e.ngram == AstBigram::encode(DEEP_NODE, bucket_label(1))),
        "depth 4 must NOT emit DEEP_NODE→bucket_label(1)"
    );

    let (set6, _) = extract_ast_ngrams_with_metrics(&make_nodes_at_depth(6), Language::Rust);
    assert!(
        set6.bigrams
            .iter()
            .any(|e| e.ngram == AstBigram::encode(DEEP_NODE, bucket_label(1))),
        "depth 6 must emit DEEP_NODE→bucket_label(1)"
    );

    let (set8, _) = extract_ast_ngrams_with_metrics(&make_nodes_at_depth(8), Language::Rust);
    for i in 0..DEPTH_EDGES.len() {
        assert!(
            set8.bigrams
                .iter()
                .any(|e| e.ngram == AstBigram::encode(DEEP_NODE, bucket_label(i))),
            "depth 8 must emit DEEP_NODE→bucket_label({i})"
        );
    }
}

#[test]
fn f3_param_buckets_emit_cumulatively() {
    use crate::ast_index::vocab_lookup;

    let fn_id = match vocab_lookup("function_item") {
        Some(id) => id,
        None => return,
    };
    let params_id = match vocab_lookup("parameters") {
        Some(id) => id,
        None => return,
    };
    // A simple identifier kind for parameters
    let identifier_id = vocab_lookup("identifier").unwrap_or(1);

    let build_nodes = |n_params: u32| -> Vec<LinearNode> {
        let mut v = vec![
            LinearNode {
                kind_id: fn_id,
                depth: 0,
            },
            LinearNode {
                kind_id: params_id,
                depth: 1,
            },
        ];
        for _ in 0..n_params {
            v.push(LinearNode {
                kind_id: if is_counted_child(identifier_id) {
                    identifier_id
                } else {
                    1
                },
                depth: 2,
            });
        }
        v
    };

    // 4 params → no bucket
    let (set4, _) = extract_ast_ngrams_with_metrics(&build_nodes(4), Language::Rust);
    assert!(
        !set4
            .bigrams
            .iter()
            .any(|e| e.ngram == AstBigram::encode(MANY_PARAMS, bucket_label(0))),
        "4 params must NOT emit MANY_PARAMS bucket"
    );

    // 5 params → b0 only
    let (set5, _) = extract_ast_ngrams_with_metrics(&build_nodes(5), Language::Rust);
    assert!(
        set5.bigrams
            .iter()
            .any(|e| e.ngram == AstBigram::encode(MANY_PARAMS, bucket_label(0))),
        "5 params must emit MANY_PARAMS→bucket_label(0)"
    );
    assert!(
        !set5
            .bigrams
            .iter()
            .any(|e| e.ngram == AstBigram::encode(MANY_PARAMS, bucket_label(1))),
        "5 params must NOT emit bucket_label(1)"
    );

    // 8 params → b0, b1
    let (set8, _) = extract_ast_ngrams_with_metrics(&build_nodes(8), Language::Rust);
    assert!(
        set8.bigrams
            .iter()
            .any(|e| e.ngram == AstBigram::encode(MANY_PARAMS, bucket_label(1))),
        "8 params must emit MANY_PARAMS→bucket_label(1)"
    );

    // 12 params → b0, b1, b2
    let (set12, _) = extract_ast_ngrams_with_metrics(&build_nodes(12), Language::Rust);
    for i in 0..PARAM_EDGES.len() {
        assert!(
            set12
                .bigrams
                .iter()
                .any(|e| e.ngram == AstBigram::encode(MANY_PARAMS, bucket_label(i))),
            "12 params must emit MANY_PARAMS→bucket_label({i})"
        );
    }
}

// ============================================================================
// F4: EMPTY_BODY keyed on enclosing kind, not body kind
// ============================================================================

#[test]
fn f4_empty_body_keyed_on_enclosing_kind() {
    use crate::ast_index::vocab_lookup;

    let catch_id = match vocab_lookup("catch_clause") {
        Some(id) => id,
        None => return, // not in this vocab — skip
    };
    let fn_id = match vocab_lookup("function_declaration") {
        Some(id) => id,
        None => return,
    };
    let block_id = match vocab_lookup("statement_block") {
        Some(id) => id,
        None => return,
    };

    // Empty catch: catch_clause at depth 0 → statement_block at depth 1, nothing at depth 2
    let catch_nodes = vec![
        LinearNode {
            kind_id: catch_id,
            depth: 0,
        },
        LinearNode {
            kind_id: block_id,
            depth: 1,
        },
        // punctuation at depth 2 (should NOT count as a statement)
        LinearNode {
            kind_id: 0,
            depth: 2,
        },
    ];

    // Empty function: function_declaration at depth 0 → statement_block at depth 1
    let fn_nodes = vec![
        LinearNode {
            kind_id: fn_id,
            depth: 0,
        },
        LinearNode {
            kind_id: block_id,
            depth: 1,
        },
        // punctuation at depth 2
        LinearNode {
            kind_id: 0,
            depth: 2,
        },
    ];

    let (catch_set, _) = extract_ast_ngrams_with_metrics(&catch_nodes, Language::TypeScript);
    let (fn_set, _) = extract_ast_ngrams_with_metrics(&fn_nodes, Language::TypeScript);

    let empty_catch_key = AstBigram::encode(EMPTY_BODY, catch_id);
    let empty_fn_key = AstBigram::encode(EMPTY_BODY, fn_id);

    assert!(
        catch_set.bigrams.iter().any(|e| e.ngram == empty_catch_key),
        "empty catch must emit EMPTY_BODY→catch_clause"
    );
    assert!(
        fn_set.bigrams.iter().any(|e| e.ngram == empty_fn_key),
        "empty function must emit EMPTY_BODY→function_declaration"
    );

    // The empty-catch key must NOT appear in the fn set and vice versa
    assert!(
        !fn_set.bigrams.iter().any(|e| e.ngram == empty_catch_key),
        "empty function must NOT emit EMPTY_BODY→catch_clause"
    );
    assert!(
        !catch_set.bigrams.iter().any(|e| e.ngram == empty_fn_key),
        "empty catch must NOT emit EMPTY_BODY→function_declaration"
    );
}

// ============================================================================
// F2: Punctuation-only / comment-only body → empty; one real stmt → not empty
// ============================================================================

#[test]
fn f2_punctuation_only_body_is_empty() {
    use crate::ast_index::vocab_lookup;

    let fn_id = match vocab_lookup("function_item") {
        Some(id) => id,
        None => return,
    };
    let block_id = match vocab_lookup("block") {
        Some(id) => id,
        None => return,
    };

    // Body contains only sentinel (punctuation) nodes at depth 2
    let nodes = vec![
        LinearNode {
            kind_id: fn_id,
            depth: 0,
        },
        LinearNode {
            kind_id: block_id,
            depth: 1,
        },
        LinearNode {
            kind_id: 0,
            depth: 2,
        }, // punctuation
        LinearNode {
            kind_id: 0,
            depth: 2,
        }, // punctuation
    ];
    let (set, _) = extract_ast_ngrams_with_metrics(&nodes, Language::Rust);
    let empty_key = AstBigram::encode(EMPTY_BODY, fn_id);
    assert!(
        set.bigrams.iter().any(|e| e.ngram == empty_key),
        "punctuation-only body must emit EMPTY_BODY→function_item"
    );
}

#[test]
fn f2_comment_only_body_is_empty() {
    use crate::ast_index::vocab_lookup;

    let fn_id = match vocab_lookup("function_item") {
        Some(id) => id,
        None => return,
    };
    let block_id = match vocab_lookup("block") {
        Some(id) => id,
        None => return,
    };
    let comment_id = match vocab_lookup("line_comment") {
        Some(id) => id,
        None => return,
    };

    // Body contains only comment nodes at depth 2
    let nodes = vec![
        LinearNode {
            kind_id: fn_id,
            depth: 0,
        },
        LinearNode {
            kind_id: block_id,
            depth: 1,
        },
        LinearNode {
            kind_id: comment_id,
            depth: 2,
        },
    ];
    let (set, _) = extract_ast_ngrams_with_metrics(&nodes, Language::Rust);
    let empty_key = AstBigram::encode(EMPTY_BODY, fn_id);
    assert!(
        set.bigrams.iter().any(|e| e.ngram == empty_key),
        "comment-only body must emit EMPTY_BODY→function_item"
    );
}

#[test]
fn f2_one_real_statement_body_is_not_empty() {
    use crate::ast_index::vocab_lookup;

    let fn_id = match vocab_lookup("function_item") {
        Some(id) => id,
        None => return,
    };
    let block_id = match vocab_lookup("block") {
        Some(id) => id,
        None => return,
    };
    let stmt_id = match vocab_lookup("expression_statement") {
        Some(id) if id != 0 => id,
        _ => return,
    };

    let nodes = vec![
        LinearNode {
            kind_id: fn_id,
            depth: 0,
        },
        LinearNode {
            kind_id: block_id,
            depth: 1,
        },
        LinearNode {
            kind_id: stmt_id,
            depth: 2,
        }, // one real statement
    ];
    let (set, _) = extract_ast_ngrams_with_metrics(&nodes, Language::Rust);
    let empty_key = AstBigram::encode(EMPTY_BODY, fn_id);
    assert!(
        !set.bigrams.iter().any(|e| e.ngram == empty_key),
        "body with one real statement must NOT emit EMPTY_BODY→function_item"
    );
}

// ============================================================================
// F5 (continued): Bucket-edge / bucket_label invariant
//
// Guards the structural coupling between the three edge tables
// (BODY_STMT_EDGES, PARAM_EDGES, DEPTH_EDGES) and the bucket_label() function.
// If a new dimension is added or an edge table is resized, these assertions
// will catch the inconsistency at test time.
// ============================================================================

#[test]
fn f5_bucket_label_encodes_edge_index_correctly() {
    // bucket_label(i) must equal BUCKET_LABEL_BASE + i for every valid edge index.
    // This is the compile-time identity that keeps edge tables and labels in sync:
    // changing BUCKET_LABEL_BASE or the formula in bucket_label() will break it.
    for i in 0..BODY_STMT_EDGES.len() {
        assert_eq!(
            bucket_label(i),
            BUCKET_LABEL_BASE + i as NodeKindId,
            "BODY_STMT_EDGES: bucket_label({i}) must equal BUCKET_LABEL_BASE + {i}"
        );
    }
    for i in 0..PARAM_EDGES.len() {
        assert_eq!(
            bucket_label(i),
            BUCKET_LABEL_BASE + i as NodeKindId,
            "PARAM_EDGES: bucket_label({i}) must equal BUCKET_LABEL_BASE + {i}"
        );
    }
    for i in 0..DEPTH_EDGES.len() {
        assert_eq!(
            bucket_label(i),
            BUCKET_LABEL_BASE + i as NodeKindId,
            "DEPTH_EDGES: bucket_label({i}) must equal BUCKET_LABEL_BASE + {i}"
        );
    }
}

#[test]
fn f5_all_edge_table_indices_are_within_max_bucket_edges() {
    // Every index used by any edge table must be < MAX_BUCKET_EDGES.
    // MAX_BUCKET_EDGES is the sentinel that keeps bucket labels below EMPTY_BODY.
    assert!(
        BODY_STMT_EDGES.len() <= MAX_BUCKET_EDGES as usize,
        "BODY_STMT_EDGES has {} entries but MAX_BUCKET_EDGES is {}",
        BODY_STMT_EDGES.len(),
        MAX_BUCKET_EDGES
    );
    assert!(
        PARAM_EDGES.len() <= MAX_BUCKET_EDGES as usize,
        "PARAM_EDGES has {} entries but MAX_BUCKET_EDGES is {}",
        PARAM_EDGES.len(),
        MAX_BUCKET_EDGES
    );
    assert!(
        DEPTH_EDGES.len() <= MAX_BUCKET_EDGES as usize,
        "DEPTH_EDGES has {} entries but MAX_BUCKET_EDGES is {}",
        DEPTH_EDGES.len(),
        MAX_BUCKET_EDGES
    );
}

// ============================================================================
// F10: Zero-node files produce degenerate (zero) metrics without panicking
// ============================================================================

#[test]
fn f10_zero_node_files_give_zero_metrics() {
    let (_, m) = extract_ast_ngrams_with_metrics(&[], Language::Rust);
    assert_eq!(m, StructuralMetrics::default());

    // JSON/YAML/TOML → linearize_source returns empty — just test the function
    let (_, m2) = extract_ast_ngrams_with_metrics(&[], Language::Json);
    assert_eq!(m2, StructuralMetrics::default());
}
