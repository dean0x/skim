# Resolution Summary

**Branch**: feature-187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01
**Review**: .devflow/docs/reviews/feature-187-wave-3a--cst-linearization--pre-order-tr/2026-06-01_1658
**Command**: /resolve

## Decisions Citations

- applies ADR-001 — batch-1 (centralize-constants, level-stack-prealloc, fused-iterator, weak-error-test), batch-2 (test-name-mismatch, must-use-removed, compilation-fix), batch-3 (ancestor-alloc, chain-break-test)
- avoids PF-002 — batch-1 (no issues deferred), batch-2 (pre-existing #[must_use] fixed not deferred), batch-3 (chain-break test gap surfaced and fixed)

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 11 |
| Fixed | 9 |
| False Positive | 0 |
| Deferred | 0 |
| Blocked | 0 |
| Pre-existing (also fixed) | 2 |

## Fixed Issues
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Centralize MAX_AST_DEPTH/MAX_AST_NODES as AstWalkConfig associated constants | ast_walk.rs:69-71 | 3194132 |
| Pre-allocate level_stack with Vec::with_capacity(min(max_depth, 64)) | ast_walk.rs:118 | 3194132 |
| Add FusedIterator marker trait impl for AstWalkIter | ast_walk.rs (Iterator impl) | 3194132 |
| Strengthen error_children_still_yielded test assertion (depth-based proof) | ast_walk.rs:335-348 | 3194132 |
| Rename test linearize_1000_line_file_under_5ms → under_10ms | linearize_tests.rs:428 | ad63f34 |
| Re-add #[must_use] on linearize_source (convention consistency) | linearize.rs:195 | ad63f34 |
| Restore MAX_AST_DEPTH/NODES re-exports for test module (compilation fix) | linearize.rs:36-40 | ad63f34 |
| Lazy-grow ancestor Vec (64 initial, resize on demand) | ast_extract.rs:137 | 3b611ae |
| Add error_node_breaks_ancestor_chain_for_descendants test | ast_extract.rs:153-159 | 3b611ae |

## False Positives
(none)

## Deferred to Tech Debt
(none — applies ADR-001)

## Blocked
(none)
