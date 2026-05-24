# Resolution Summary

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17_1137
**Review**: .docs/reviews/feat-182-index-builder-pipeline/2026-05-17_1137
**Command**: /resolve

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 18 |
| Fixed | 17 |
| False Positive | 0 |
| Deferred | 0 |
| Blocked | 0 |
| Not Applicable | 1 |

## Fixed Issues

### Batch 1: index.rs (commit 7a7a39e)
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Unsafe `as u32` cast on FileId | index.rs:217 | 7a7a39e |
| Hand-rolled argument parser bypasses clap | index.rs:83-127 | 7a7a39e |
| `--max-files=0` accepted without validation | index.rs:97-101 | 7a7a39e |
| Duplicate path-key string allocation | index.rs:198+226 | 7a7a39e |
| Debug-format-based language serialization | index.rs:230 | 7a7a39e |
| Silent classify error swallowing | index.rs:250-256 | 7a7a39e |

### Batch 2: walk.rs (commit 8a1bef5 + 7a7a39e)
| Issue | File:Line | Commit |
|-------|-----------|--------|
| TOCTOU race between file size check and read | walk.rs:147-166 | 8a1bef5 |
| Redundant fs::metadata syscall per file | walk.rs:147 | 8a1bef5 |
| I/O errors misclassified as NonUtf8 | walk.rs:166-171 | 8a1bef5 |
| Unbounded discover_project_root loop | walk.rs:52-68 | 7a7a39e |
| Walker error creates nonsensical path | walk.rs:117-123 | 8a1bef5 |

### Batch 3: manifest.rs (commit 8a1bef5)
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Mixed error return types (io::Result vs anyhow) | manifest.rs:109+205 | 8a1bef5 |
| Manifest save writes unbuffered | manifest.rs:216-228 | 8a1bef5 |
| No pre-allocation for HashMap in manifest load | manifest.rs:153 | 8a1bef5 |

### Batch 4: mod.rs + lib.rs (commit 7a7a39e)
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Help text regression (index --help shows parent help) | mod.rs:34 | 7a7a39e |
| Stale doc comment references search.rs | lib.rs:11 | 7a7a39e |

### Batch 5: Tests (commit 3d2a37b)
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Incremental build test doesn't verify cache hits | index_tests.rs:112-124 | 3d2a37b |
| Minified file detection has no test | walk.rs:217-228 | 3d2a37b |
| No --max-files integration test | index_tests.rs | 3d2a37b |
| Duplicated encode/decode helpers in manifest tests | manifest_tests.rs:32-46 | 3d2a37b |

## Not Applicable
| Issue | File:Line | Reasoning |
|-------|-----------|-----------|
| Sequential file walk (build_parallel) | walk.rs:108 | Optimization opportunity, not a defect. Profiling needed first. |

## Commits Created
- `8a1bef5` refactor(#182): unify error types, buffer writes, and preallocate manifest HashMap
- `7a7a39e` fix(#182): correct search index help routing and related cleanup
- `3d2a37b` test(#182): strengthen index pipeline test coverage

## Verification
- All 4,012 tests pass
- `cargo clippy -- -D warnings`: clean (zero warnings)
