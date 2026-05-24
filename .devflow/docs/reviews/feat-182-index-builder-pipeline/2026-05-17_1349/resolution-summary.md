# Resolution Summary

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17_1349
**Review**: .docs/reviews/feat-182-index-builder-pipeline/2026-05-17_1349
**Command**: /resolve

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 28 |
| Fixed | 22 |
| False Positive | 0 |
| Deferred | 6 |
| Blocked | 0 |

## Fixed Issues
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Remove unsafe block in sha256_hex | walk.rs:332 | 29586b3 |
| Hex encoding consistency (resolved by unsafe fix) | walk.rs:323 | 29586b3 |
| Extract classify_entry helper from walk_and_read | walk.rs:123 | 29586b3 |
| Replace is_tree_sitter_language with is_serde_based | walk.rs:299 | 29586b3 |
| Cap skipped vec at 10,000 entries | walk.rs:131 | 29586b3 |
| Add manifest entry count cap (60,000) | manifest.rs:158 | d71c08e |
| Add manifest file size gate (256 MiB) | manifest.rs:126 | d71c08e |
| Add fsync before manifest atomic rename | manifest.rs:237 | d71c08e |
| Make manifest path clone explicit | manifest.rs:185 | d71c08e |
| Use is_debug_enabled() instead of env var check | index.rs:196 | 67b6e79 |
| Replace mem::take with zip-consume for path_keys | index.rs:186 | 67b6e79 |
| Add CHANGELOG entry for skim search index (#182) | CHANGELOG.md:10 | 1647662 |
| Fix misleading help text — remove unimplemented options | search/mod.rs:66 | 1647662 |
| Merge redundant incremental test | index_tests.rs:128 | eae616b |
| Add SHA verification for modified file reindex | index_tests.rs:227 | eae616b |
| Add --force cache_hits == 0 assertion via build_index | index_tests.rs:253 | eae616b |
| Bound find_file_with_ext recursion depth to 5 | index_tests.rs:383 | eae616b |
| Fix non-UTF8 test to use .rs extension with invalid bytes | walk_tests.rs:120 | 40c5c15 |
| Add explicit sort before determinism comparison | walk_tests.rs:160 | 40c5c15 |
| Inline is_tree_sitter_language (simplifier) | walk.rs | simplify |
| Collapse collapsible_if in manifest (simplifier) | manifest.rs | simplify |
| Remove redundant .into_iter() calls (simplifier) | index.rs | simplify |

## Deferred to Tech Debt
| Issue | File:Line | Risk Factor |
|-------|-----------|-------------|
| All file contents held in memory simultaneously | walk.rs:123 / index.rs:162 | Architectural — requires streaming/mmap redesign of two-phase pipeline |
| Sequential walker forces single-threaded I/O | walk.rs:143 | Performance optimization — requires parallel walker with post-collection sort |
| build_index monolith (93 lines, 6 responsibilities) | index.rs:150-243 | Tolerable at current size — extract Pipeline struct when pipeline grows |
| SHA-256 computed for every file on every build | walk.rs:237 | Feature addition — mtime pre-screening requires ManifestEntry schema change |
| dns.rs exceeds 1000 lines (two independent parsers) | dns.rs | Separate module, separate PR — not part of search pipeline |
| No test for incremental cache hit count via public API | index.rs:76-80 | Requires exposing build_index as pub(crate) — partially addressed by --force test |

## Pre-existing (Not Addressed)
| Issue | File:Line | Reasoning |
|-------|-----------|-----------|
| Manifest path validation for directory traversal | manifest.rs:165 | Pre-existing pattern, informational only |
| rskim-core version mismatch in workspace | Cargo.toml | Pre-existing drift, harmless for unpublished crates |
