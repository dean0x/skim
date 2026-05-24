# Resolution Summary

**Branch**: feat/195-bm25f-bench -> main
**Date**: 2026-05-21T15:10:00Z
**Review**: .devflow/docs/reviews/feat-195-bm25f-bench/20260521T140335/
**Command**: /resolve

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 33 |
| Fixed | 17 |
| False Positive | 2 |
| Deferred | 14 |
| Blocked | 0 |

## Fixed Issues
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Hardcoded field count 8 → FIELD_COUNT | tuning.rs:82, configs.rs:8 | 09c3f2e |
| result_to_config silent zeroing (resolved by Vec→array) | tuning.rs:162-182 | 09c3f2e |
| TuningResult Vec\<f32\> → [f32; FIELD_COUNT] | types.rs:97-98 | 09c3f2e |
| Dead EvalResult type removed | types.rs:24-34 | 09c3f2e |
| Search errors propagated in evaluate_split | harness.rs:144 | 5856267 |
| BenchConfig missing Debug derive | harness.rs:17 | 5856267 |
| &Vec\<ConfigMetrics\> → &[ConfigMetrics] | harness.rs:203 | 5856267 |
| NaN debug assertion in mrr() | metrics.rs:47 | 42a0be8 |
| Redundant tempfile dev-dependency removed | Cargo.toml:35 | 42a0be8 |
| Allow comment justifications added (7 files) | configs/metrics/split/harness/qrel/tuning/report | 42a0be8 |
| Format flag standardized (--output → --format) + ValueEnum | main.rs:58-105 | 5c2f4b6 |
| file_id_counter overflow → checked arithmetic | main.rs:226-240 | 5c2f4b6 |
| Tuning closure error logging (first 5 + summary) | main.rs:291-306 | 5c2f4b6 |
| &PathBuf → &Path in open_corpus | main.rs:128 | 5c2f4b6 |
| report::to_json → anyhow::Result | report.rs:19 | 2e27481 |
| Tautological FileId test replaced | tests/integration.rs:204 | 608b4a8 |
| Weak MRR assertion split into two assertions | tests/integration.rs:284 | 608b4a8 |

## False Positives
| Issue | File:Line | Reasoning |
|-------|-----------|-----------|
| Internal path deps missing version field | Cargo.toml:20-22 | Main `rskim` crate also omits version for `publish = false` path deps — omitting is the established workspace convention |
| Dead EvalResult (duplicate) | types.rs:24 | Already removed in prior commit before batch-1 ran |

## Deferred to Tech Debt
| Issue | File:Line | Risk Factor |
|-------|-----------|-------------|
| run_tune 134-line god function | main.rs:220-353 | Architectural: 7+ responsibilities, requires extracting 5-6 helpers |
| coordinate_descent 109-line function | tuning.rs:46-154 | Architectural: 3 identical sweep blocks need generic extraction |
| Duplicate file-loading across 3 subcommands | main.rs (3 locations) | Architectural: shared helper needed, variants differ in ID strategy |
| Extractor boilerplate triplication | extract/go.rs, python.rs, rust_lang.rs | Architectural: shared parse_and_walk helper needed |
| to_markdown 69 lines with hardcoded field names | report.rs:33-101 | Moderate: extract section renderers |
| open_corpus → Box\<dyn FileSource\> | main.rs:135-138 | Moderate: testability improvement |
| RepoBenchResult.repo_url initialized empty | harness.rs:102 | Low: incomplete construction pattern |
| Parser allocated per file in extractors | extract/*.rs | Performance: hoist parser to caller |
| Index re-opened from disk per tuning evaluation | main.rs:291-306 | Performance: requires rskim-search API change |
| Sequential repo processing | main.rs:161-200 | Performance: parallelize with rayon |
| Content clone in qrel input construction | harness.rs:45 | Performance: lifetime or Arc optimization |
| No integration test for extract_symbols dispatch | extract/mod.rs:39 | Testing gap: cross-language dispatch |
| Missing error-path test for run_on_files | harness.rs:36 | Testing gap: harness error propagation |
| No test for aggregate_results with mismatched configs | harness.rs:171 | Testing gap: edge case |

## Blocked
(none)
