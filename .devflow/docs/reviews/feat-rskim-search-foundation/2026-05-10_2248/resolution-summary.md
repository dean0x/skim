# Resolution Summary

**Branch**: feat/rskim-search-foundation -> main
**Date**: 2026-05-10_2248
**Review**: .docs/reviews/feat-rskim-search-foundation/2026-05-10_2248
**Command**: /resolve

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 6 |
| Fixed | 4 |
| False Positive | 1 |
| Deferred | 0 |
| Blocked | 0 |
| N/A (removed by prior fix) | 1 |

## Fixed Issues
| Issue | File:Line | Commit |
|-------|-----------|--------|
| NodeInfo not re-exported from lib.rs — FieldClassifier trait unusable by downstream consumers | `crates/rskim-search/src/lib.rs:15` | `75391d5` |
| tree-sitter leaks into public API via NodeInfo::from_ts_node — removed from_ts_node and tree-sitter dep (Option A) | `crates/rskim-search/src/types.rs:258` | `75391d5` |
| No concrete FieldClassifier implementation test — added KindClassifier test covering 3 node kinds | `crates/rskim-search/src/types.rs` | `75391d5` |
| IndexStats roundtrip deserialization not tested — added 2 roundtrip tests matching SearchResult pattern | `crates/rskim-search/src/types.rs` | `75391d5` |

## False Positives
| Issue | File:Line | Reasoning |
|-------|-----------|-----------|
| Test comment style inconsistency (// vs ///) | `crates/rskim/src/cmd/search.rs:87-107` | The claimed "crate convention" does not hold within the `rskim` crate. `stats.rs` and all five `heatmap/` test modules use bare `#[test]` with no `///` doc comments. The `types.rs` example cited is in the separate `rskim-search` crate. No change warranted. |

## N/A (Superseded)
| Issue | File:Line | Reasoning |
|-------|-----------|-----------|
| Missing test for NodeInfo::from_ts_node | `crates/rskim-search/src/types.rs:258-264` | `from_ts_node` was removed under Option A for the tree-sitter leak fix. Constructor no longer exists. |

## Simplification
| Change | File | Commit |
|--------|------|--------|
| Removed redundant `test_search_result_serialization_null_snippet` (strict subset of roundtrip test) | `types.rs` | `ae2d2ea` |
| Inlined single-use `args` variables, removed redundant inline comments | `search.rs` | `ae2d2ea` |
| Fixed double blank line before FieldClassifier | `types.rs` | `ae2d2ea` |

## Deferred to Tech Debt
_(none)_

## Blocked
_(none)_
