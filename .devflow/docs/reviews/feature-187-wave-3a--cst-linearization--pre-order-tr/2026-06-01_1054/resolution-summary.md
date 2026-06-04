# Resolution Summary

**Branch**: feature-187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01
**Review**: .devflow/docs/reviews/feature-187-wave-3a--cst-linearization--pre-order-tr/2026-06-01_1054
**Command**: /resolve

## Decisions Citations

- applies ADR-001 — batch-1 (ast-error-naming, infallible-result, u16-truncation), batch-2 (must-use-message, saturating-add), batch-3 (allow-style, error-test-tautology, max-nodes-test, perf-test-mislabel), batch-4 (lib.rs-doc, is-missing)
- avoids PF-002 — batch-1 (redundant-parser, parse-empty-source), batch-3 (max-nodes-test), batch-4 (traversal-duplication)

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 15 |
| Fixed | 12 |
| False Positive | 2 |
| Deferred | 1 |
| Blocked | 0 |

## Fixed Issues
| Issue | File:Line | Commit |
|-------|-----------|--------|
| SearchError::AstError → Ast (naming convention) | types.rs:624 | 6fd8392 |
| linearize_tree returns Result but infallible | linearize.rs:232 | 6fd8392 |
| Silent u16 truncation in LANG_MAPS init | linearize.rs:153 | 6fd8392 |
| #[must_use] custom message inconsistent | linearize.rs:188 | 4acd8e2 |
| Doc comment says "named" but includes anonymous | linearize.rs:83 | 4acd8e2 |
| Counter increments use raw += not saturating_add | linearize.rs:271,274 | 4acd8e2 |
| Split #![allow] lines inconsistent | linearize_tests.rs:13 | d6b75df |
| error_nodes test tautological disjunction | linearize_tests.rs:184 | d6b75df |
| max_nodes test never exercises cap (renamed) | linearize_tests.rs:242 | d6b75df |
| Perf test generates 100 fns not 1000 | linearize_tests.rs:427 | d6b75df |
| lib.rs module doc missing ast_index | lib.rs:1 | 9cf7c04 |
| ast_extract.rs missing is_missing() check | ast_extract.rs:190 | 9cf7c04 |

## False Positives
| Issue | File:Line | Reasoning |
|-------|-----------|-----------|
| Redundant Parser::new per call | linearize.rs:206 | LANG_MAPS init extracts grammar metadata (one-time); linearize_source creates Parser for actual source parsing (per-call). Different purposes. Parser reuse deferred to #194. |
| Parse-empty-source anti-pattern | linearize.rs:138 | Language::to_tree_sitter() is pub(crate) in rskim-core. Grammar crates not direct deps of rskim-search. Parse-empty-source is the only viable pattern. |

## Deferred to Tech Debt
| Issue | File:Line | Risk Factor |
|-------|-----------|-------------|
| Traversal duplication (ast_extract.rs + linearize.rs) | ast_extract.rs:152, linearize.rs:232 | Pre-existing. Cross-crate refactor touching dependency graphs. Awaiting user decision. |
