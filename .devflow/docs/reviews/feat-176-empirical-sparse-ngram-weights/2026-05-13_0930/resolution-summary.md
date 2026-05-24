# Resolution Summary

**Branch**: feat/176-empirical-sparse-ngram-weights -> main
**Date**: 2026-05-13
**Review**: .docs/reviews/feat-176-empirical-sparse-ngram-weights/2026-05-13_0930
**Command**: /resolve

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 16 |
| Fixed | 14 |
| False Positive | 0 |
| Deferred | 2 |
| Blocked | 0 |

## Fixed Issues
| Issue | File:Line | Commit |
|-------|-----------|--------|
| CRITICAL: Unbounded git subprocess (300s timeout) | clone.rs:65-117 | a6afca7 |
| HIGH: Path traversal in extract_repo_name | clone.rs:51-57 | a6afca7 |
| MEDIUM: Git credential hardening | clone.rs:65-117 | a6afca7 |
| HIGH: is_border_bigram overly broad logic (positional rewrite) | validate.rs:76-98 | 69d6391 |
| HIGH: is_border_bigram missing unit tests (4 added) | validate.rs | 69d6391 |
| HIGH: higher_idf_bigrams_preferred test weak assertion | validate.rs:246-259 | 69d6391 |
| HIGH: run_validation weak >= 0.0 assertions | validate.rs:278-285 | 69d6391 |
| MEDIUM: covering_set test silent empty pass | validate.rs:219-243 | 69d6391 |
| HIGH: compute_idf NEG_INFINITY on total_docs=0 | idf.rs:12-14 | 13d8c35 |
| HIGH: NaN passes codegen validation | codegen.rs:57-65 | 13d8c35 |
| MEDIUM: selectivity doc copy-paste error | idf.rs:43-46 | 13d8c35 |
| MEDIUM: codegen missing negative IDF test | codegen.rs | 13d8c35 |
| MEDIUM: codegen missing version==0 test | codegen.rs | 13d8c35 |
| HIGH: Inconsistent workspace clap dependency | Cargo.toml + rskim/Cargo.toml | e581829 |

## Deferred to Tech Debt
| Issue | File:Line | Risk Factor |
|-------|-----------|-------------|
| MEDIUM: Const table 350KB source compile time | weights.rs | Wave 1 — real corpus data will replace synthetic, binary format evaluation planned |
| MEDIUM: 565KB JSON artifact in version control | data/bigram_weights.json | Wave 1 — consider .gitignore or binary format when real data arrives |

## Blocked
(none)
