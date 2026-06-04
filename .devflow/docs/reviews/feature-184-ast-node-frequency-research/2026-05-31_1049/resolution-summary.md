# Resolution Summary

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31_1049
**Review**: .devflow/docs/reviews/feature-184-ast-node-frequency-research/2026-05-31_1049
**Command**: /resolve

## Decisions Citations

- applies ADR-001 — batch-1 (all 5 issues), batch-2 (all 5 issues + 1 bonus), batch-3 (all 5 issues), batch-4 (all 5 issues)
- avoids PF-002 — batch-2 (no silent deferral), batch-3 (config doc, version pin), batch-4 (no deferral)

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 20 |
| Fixed | 20 |
| False Positive | 0 |
| Deferred | 0 |
| Blocked | 0 |

## Fixed Issues

### Batch 1: ast_types.rs (5 issues)
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Replace debug_assert with assert! for u16 overflow guard | ast_types.rs:157 | 432a788 |
| Add remap_trigram correctness + rekey_trigram_df_map tests | ast_types.rs (tests) | 432a788 |
| Add remap out-of-bounds returns None tests (bigram + trigram) | ast_types.rs (tests) | 432a788 |
| Eliminate double string allocation in get_or_insert | ast_types.rs:162 | 432a788 |
| Reduce stabilize() clones via index-sort approach | ast_types.rs:207 | 432a788 |

### Batch 2: ast_extract.rs (5 issues + 1 bonus)
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Fix misleading "iterative" doc comment → "recursive with depth guard" | ast_extract.rs:108 | 15e9207 |
| Extract WalkContext struct, reduce walk_tree from 10 to 4 params | ast_extract.rs:114 | 15e9207 |
| Replace files.len() as u32 with try_from saturating | ast_extract.rs:220 | 15e9207 |
| Use saturating_add for lang_total_nodes/lang_error_nodes | ast_extract.rs:246 | 15e9207 |
| Add real assertions to error_nodes_counted_but_not_in_bigrams test | ast_extract.rs:397 | 15e9207 |
| (Bonus) Fix expect_used lint violation in stabilize() | ast_types.rs:233 | 15e9207 |

### Batch 3: ast_codegen.rs + config.rs + Cargo.toml (5 issues)
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Sanitize comment strings with {:?} debug formatting | ast_codegen.rs:189 | 3bddc0d |
| Add debug_assert postcondition for valid Rust identifier | ast_codegen.rs:153 | 3bddc0d |
| Fix underscore collapsing to handle runs of any length | ast_codegen.rs:153 | 3bddc0d |
| Fix validate_ast_repo doc: "lowercase" → "upper or lowercase" | config.rs:110 | 3bddc0d |
| Update stale rskim-core version pins 2.9.0 → 2.10.0 | Cargo.toml (2 files) | 3bddc0d |

### Batch 4: ast_validate.rs + main.rs + clone.rs (5 issues)
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Change eprintln! → println! in ast_validate for stdout consistency | ast_validate.rs:146 | 15e9207 |
| Add inline validation summary to cmd_ast_run | main.rs:448 | 15e9207 |
| Hoist progress.set_message from per-file to per-language loop | ast_extract.rs:249 | 15e9207 |
| Add full stabilize-rekey-IDF pipeline integration test | ast_extract.rs:523 | 15e9207 |
| Change walk_and_load from pub to pub(crate) | clone.rs:287 | 15e9207 |

## False Positives
(none)

## Deferred to Tech Debt
(none — all issues fixed per ADR-001)

## Blocked
(none)
