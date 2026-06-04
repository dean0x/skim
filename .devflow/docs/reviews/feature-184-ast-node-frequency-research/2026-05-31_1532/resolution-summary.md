# Resolution Summary

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31_1532
**Review**: .devflow/docs/reviews/feature-184-ast-node-frequency-research/2026-05-31_1532
**Command**: /resolve

## Decisions Citations

- applies ADR-001 — batch-1 (all 3 issues), batch-2 (all 3 issues), batch-3 (all 5 issues), batch-4 (3 issues fixed), batch-5 (all 5 issues)
- avoids PF-002 — batch-1 (no deferral), batch-2 (no deferral), batch-3 (no deferral), batch-4 (SF1 documented as false positive with explicit reasoning), batch-5 (no deferral)

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 21 |
| Fixed | 20 |
| False Positive | 1 |
| Deferred | 0 |
| Blocked | 0 |

## Fixed Issues

### Batch 1: ast_codegen.rs (3 issues)
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Harden lang_to_ident — replace debug_assert! with anyhow::bail! | ast_codegen.rs:177-190 | d03d357 |
| Add empty-bigrams and empty-trigrams validation to validate_ast_table | ast_codegen.rs:56 | d03d357 |
| DRY codegen — extract sorted_lang_idents<V> and write_lookup_fn helpers | ast_codegen.rs:195-329 | d03d357 |

### Batch 2: ast_extract.rs (3 issues)
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Hoist seen_hashes to corpus level for cross-language dedup | ast_extract.rs:225 | de5d4ef |
| Convert walk_tree from recursive to iterative with manual stack | ast_extract.rs:142 | de5d4ef |
| Add node_count assertion to empty_source_returns_empty_result test | ast_extract.rs:384 | de5d4ef |

### Batch 3: ast_types.rs (5 issues)
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Change SAFETY: to INVARIANT: comment on safe code | ast_types.rs:234 | b2f9943 |
| Add #[must_use] to get_or_insert | ast_types.rs:151 | b2f9943 |
| Add sort-contract docs on AstBigramWeight and AstTrigramWeight | ast_types.rs:265,276 | 2171351 |
| Add rekey_bigram_df_map_merges_collisions test | ast_types.rs (tests) | b2f9943 |
| Change kinds() to return &[String] instead of Vec<&str> | ast_types.rs:255 | b2f9943 |

### Batch 4: main.rs pipeline + tests (3 issues)
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Extract build_ast_weight_table into ast_pipeline.rs | main.rs:387-465 | 51ec638 |
| Add 5 integration tests for AST pipeline | ast_pipeline.rs (tests) | 51ec638 |
| Extract AST CLI handlers into ast_cmd.rs (main.rs 562→399 lines) | main.rs | 51ec638 |

### Batch 5: config/toml/validate/clone (5 issues)
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Adopt corpus.toml comment style in ast-corpus.toml | ast-corpus.toml:15-17 | b2f9943 |
| Extract validate_repo_common helper — DRY validation | config.rs:99,133 | b2f9943 |
| Add top_bigrams_capped_at_20_with_more_than_20_inputs test | ast_validate.rs (tests) | b2f9943 |
| Add percentile boundary tests (0th, 100th, single element) | ast_validate.rs (tests) | b2f9943 |
| Convert extension lookup to HashSet for O(1) contains | clone.rs:321 | b2f9943 |

### Simplification
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Change generated_at parameter from String to &str | ast_pipeline.rs | simplify commit |
| Prune redundant doc comments on ast_cmd module | ast_cmd.rs, main.rs | simplify commit |

## False Positives
| Issue | File:Line | Reasoning |
|-------|-----------|-----------|
| String cloning in stabilize() | ast_types.rs:242-244 | stabilize() clones ~100-300 short strings (one per distinct node kind) exactly once during an offline corpus pass. Absolute cost is microseconds. The reviewer correctly identified the O(n) clone but misassessed whether it matters for an offline tool. |

## Deferred to Tech Debt
(none — all issues fixed per ADR-001)

## Blocked
(none)
