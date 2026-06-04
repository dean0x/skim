# Resolution Summary

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31_1338
**Review**: .devflow/docs/reviews/feature-184-ast-node-frequency-research/2026-05-31_1338
**Command**: /resolve

## Decisions Citations

- applies ADR-001 — batch-1 (all 5 issues), batch-2 (all 5 issues), batch-3 (all 2 issues), batch-4 (all 5 issues), batch-5 (1 issue)
- avoids PF-002 — batch-1 (no deferral), batch-2 (parallelism constraint documented in code, not silently deferred), batch-3 (no deferral), batch-4 (no deferral), batch-5 (no deferral)

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 18 |
| Fixed | 18 |
| False Positive | 0 |
| Deferred | 0 |
| Blocked | 0 |

## Fixed Issues

### Batch 1: clone.rs + config.rs (5 issues)
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Extract ensure_cloned() helper — DRY clone logic | clone.rs:65-97 | b94abe4 |
| Use imported PathBuf/Path types (consistency) | clone.rs:82, config.rs:85 | b94abe4 |
| Change AST_VALID_LANGUAGES "Sql" to "SQL" | config.rs:49 | b94abe4 |
| Add walk_and_load_explicit_extensions_includes_md test | clone.rs (tests) | b94abe4 |
| Add ast_git_clone_source_is_trait_object_compatible test | clone.rs (tests) | b94abe4 |

### Batch 2: ast_extract.rs (5 issues)
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Use saturating_add for bigram/trigram DF counters | ast_extract.rs:283,286 | d5edac8 |
| Use saturating_add for file counters | ast_extract.rs:256,260,261 | d5edac8 |
| Remove Markdown from non-tree-sitter comment | ast_extract.rs:77 | d5edac8 |
| Extract process_language_files() helper (105→orchestration) | ast_extract.rs:205-309 | d5edac8 |
| Document walk_tree param design rationale | ast_extract.rs:132-137 | d5edac8 |

### Batch 3: main.rs (2 issues)
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Extract log_ast_summary() — reuse ast_validate | main.rs:450-478 | af0263f |
| Create generic write_json_table<T: Serialize> | main.rs:495-515 | af0263f |

### Batch 4: ast_types.rs + ast_validate.rs + ast_codegen.rs (5 issues)
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Remove redundant sort in kinds() post-stabilize | ast_types.rs:253-257 | fed92ef |
| Add debug_assert precondition to percentile() | ast_validate.rs:140 | fed92ef |
| Extend distribution_stats_correct with p90/p99 assertions | ast_validate.rs (tests) | fed92ef |
| Strengthen codegen test assertions with structural markers | ast_codegen.rs (tests) | fed92ef |
| Add nan_idf_returns_error and infinity_idf_returns_error tests | ast_codegen.rs (tests) | fed92ef |

### Batch 5: ast-corpus.toml (1 issue)
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Pin all 16 HEAD commits to SHA hashes | ast-corpus.toml (16 repos) | d5edac8 |

## False Positives
(none)

## Deferred to Tech Debt
(none — all issues fixed per ADR-001)

## Blocked
(none)
