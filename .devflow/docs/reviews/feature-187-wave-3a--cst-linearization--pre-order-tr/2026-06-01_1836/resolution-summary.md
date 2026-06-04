# Resolution Summary

**Branch**: feature-187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01
**Review**: .devflow/docs/reviews/feature-187-wave-3a--cst-linearization--pre-order-tr/2026-06-01_1836
**Command**: /resolve

## Decisions Citations

- applies ADR-001 — batch-1 (vocab-size, wall-clock, sentinel), batch-2 (sql-file-size, bench-comment), batch-3 (chain-break, missing-node-test), batch-4 (allow-pattern, nesting-depth)
- avoids PF-002 — batch-1 (sentinel reasoning explicit), batch-2 (tree-sitter dep justified with evidence), batch-3 (MISSING node change tested not deferred), batch-4 (pre-existing nesting fixed not skipped)

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 11 |
| Fixed | 9 |
| False Positive | 2 |
| Deferred | 0 |
| Blocked | 0 |

## Fixed Issues
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Replace hardcoded vocabulary size 1740 with behavioral test (non-empty + u16 range) | linearize_tests.rs:81 | a057aec |
| Replace single-sample wall-clock test with 5-iteration median check | linearize_tests.rs:437 | a057aec |
| Replace vacuous sentinel test with direct LANG_MAPS out-of-bounds probe | linearize_tests.rs:384 | a057aec |
| Add SQL MAX_FILE_SIZE_LARGE (1 MiB) override for consistency with ast_extract | linearize.rs:50 | 91557d1 |
| Correct misleading benchmark comment (parsing is included in b.iter()) | linearize_bench.rs:94 | 91557d1 |
| Strengthen chain-break test with non-empty assertion on clean source bigrams | ast_extract.rs:528 | 59d051e |
| Add missing_nodes_excluded_from_bigrams test for MISSING node behavioral change | ast_extract.rs:551 | 59d051e |
| Unify #![allow(clippy::expect_used)] across all 45 test modules in workspace | 45 test files | a174547 |
| Extract collect_named_imports to reduce nesting from 4 to 3 levels | typescript.rs:91 | a174547 |

## False Positives
| Issue | File:Line | Reasoning |
|-------|-----------|-----------|
| tree-sitter direct dependency in rskim-search | Cargo.toml:20 | rskim-core's public API exposes tree_sitter::Tree and TreeCursor; consumers must depend on tree-sitter directly. rskim-research follows same pattern. Justified. |
| Ancestor vec resize one element at a time | ast_extract.rs:148 | Intentional and documented. vec![None; 64] initial + lazy growth is optimal trade-off. Code has inline comment explaining why. |

## Deferred to Tech Debt
(none — applies ADR-001)

## Blocked
(none)
