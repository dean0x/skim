# Resolution Summary

**Branch**: feat/188-cli-search-integration -> main
**Date**: 2026-05-19_1136
**Review**: .docs/reviews/feat-188-cli-search-integration/2026-05-19_1136
**Command**: /resolve

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 25 |
| Fixed | 23 |
| False Positive | 0 |
| Deferred | 1 |
| Blocked | 0 |
| Skipped (already fixed) | 1 |

## Fixed Issues

### Batch 1 — staleness.rs (commit 5175910)
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Infinite rebuild loop for non-git projects | staleness.rs:153 | 5175910 |
| Misleading staleness comment (stored HEAD + unreadable git) | staleness.rs:174 | 5175910 |
| is_hex_sha rejects SHA-256 (64-char hashes) | staleness.rs:135 | 5175910 |
| Unsanitized ref path from .git/HEAD | staleness.rs:88 | 5175910 |
| auto_refresh_if_stale returns manifest (eliminates duplicate load) | staleness.rs:199 | 5175910 |
| 17 new tests for check_staleness, auto_refresh_if_stale, read_git_head | staleness_tests.rs | 5175910 |

### Batch 2 — snippet.rs (commit 8f6ad5e)
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Unguarded file read in snippet extraction (5MB cap) | snippet.rs:127 | 8f6ad5e |
| Overflow in context window (saturating_add) | snippet.rs:77 | 8f6ad5e |
| Truncating usize→u32 cast (try_from) | snippet.rs:67 | 8f6ad5e |
| extract_context_window collects all lines (skip/take) | snippet.rs:66 | 8f6ad5e |
| byte_offset_to_line out-of-bounds test | snippet_tests.rs | 8f6ad5e |

### Batch 3 — mod.rs + query.rs (commit 6b2aa89)
| Issue | File:Line | Commit |
|-------|-----------|--------|
| parse_flags returns bare Flags → anyhow::Result | mod.rs:116 | 6b2aa89 |
| -j alias inconsistent with other subcommands (removed) | mod.rs:137 | 6b2aa89 |
| Debug formatting for user-facing StalenessCheck (Display impl) | mod.rs:271 | 6b2aa89 |
| Flags struct with 6 booleans → SearchAction enum | mod.rs:102 | 6b2aa89 |
| parse_flags missing tests (12 new tests) | mod.rs | 6b2aa89 |
| Missing edge-case test for non-git query | query_tests.rs | 6b2aa89 |
| Missing [stale] marker test | query_tests.rs | 6b2aa89 |
| Weak disjunctive assertion fixed | query_tests.rs | 6b2aa89 |
| Corrupt index error-path test | query_tests.rs | 6b2aa89 |
| run_stats double manifest load eliminated | mod.rs:259 | 6b2aa89 |

### Batch 4 — hooks/install/manifest (commit 5175910)
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Concurrent build lock (advisory file lock in build_index) | index.rs:165 | 5175910 |
| Predictable temp file → NamedTempFile | hooks.rs:176 | 5175910 |
| sorted_paths re-sorts → BTreeMap | manifest.rs:268 | 5175910 |
| Zombie process documented (explicit drop + comment) | install.rs:314 | 5175910 |

## False Positives
None.

## Deferred to Tech Debt
| Issue | File:Line | Risk Factor |
|-------|-----------|-------------|
| Duplicate git root discovery logic | install.rs:332 | Different semantic contracts (Option vs Result with cwd fallback). Refactoring requires changing caller contracts across modules — architectural overhaul risk. |

## Skipped (Already Fixed by Prior Batch)
| Issue | File:Line | Reasoning |
|-------|-----------|-----------|
| Duplicate manifest load on query hot path | query.rs:55,70 | Fixed by batch 1 as part of auto_refresh_if_stale signature change |

## Test Impact
- Search module tests: 128 → 141 (+13 tests)
- All 141 search tests passing
- Zero clippy warnings
- No regressions in broader test suite
