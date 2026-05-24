# Resolution Summary

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17
**Review**: .docs/reviews/feat-182-index-builder-pipeline/2026-05-17_1513
**Command**: /resolve

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 18 |
| Fixed | 10 |
| False Positive | 1 |
| Deferred | 7 |
| Blocked | 0 |

## Fixed Issues
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Mutex .lock().unwrap() poisoning (5 reviewers) | walk.rs:261,272,275,287 | 330efa3 |
| Arc::try_unwrap().expect() panic on thread leak | walk.rs:300-307 | 330efa3 |
| Walker closure 4-level nesting (extract handle_entry) | walk.rs:252-298 | 330efa3 |
| index::run missing &AnalyticsConfig parameter | index.rs:60 | 29b62e7 |
| add_file_classified error aborts entire build | index.rs:230 | 29b62e7 |
| write! unwrap inconsistency (no explanatory message) | index.rs:316 | 29b62e7 |
| sync_data() on regenerable cache file (5-200ms waste) | manifest.rs:277 | 09316f8 |
| Missing SkipReason::Minified assertion in test | walk_tests.rs:358 | 17b4744 |
| Missing error-path test for build_index | index_tests.rs:new | 17b4744 |
| Determinism test missing lang field assertion | walk_tests.rs:186 | 17b4744 |

## False Positives
| Issue | File:Line | Reasoning |
|-------|-----------|-----------|
| String::from_utf8 redundant validation on hot path | walk.rs:388 | Reviewer acknowledged "no action needed" — 64-byte scan is negligible, safety > micro-optimization |

## Deferred to Tech Debt
| Issue | File:Line | Risk Factor |
|-------|-----------|-------------|
| FileManifest lacks trait abstraction (DIP) | manifest.rs:87-285 | Multi-file refactor, new trait + mock infrastructure |
| build_index God Function (6 responsibilities) | index.rs:150-249 | Orchestrator decomposition, many shared locals |
| Atomic write ordering not type-enforced | index.rs:238-240 | Type-state pattern requires new types, affects builder API |
| walk_and_read mixed-concern return type | walk.rs:231-234 | Changes all call sites, new WalkResult struct |
| Mutex contention serializes parallel hot path | walk.rs:270-272 | Concurrency model change (channel/thread-local) |
| No mtime early-exit for incremental builds | walk.rs:202, index.rs:162 | New feature (mtime+size fields in ManifestEntry) |
| Relaxed atomic ordering allows over-collection | walk.rs:259,271 | Documented trade-off; Acquire/Release may not help |

## Blocked
(none)
