# Resolution Summary

**Branch**: feat/179-git-log-parser-gix -> main
**Date**: 2026-05-14_2358
**Review**: .docs/reviews/feat-179-git-log-parser-gix/2026-05-14_2358
**Command**: /resolve

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 19 |
| Fixed | 16 |
| False Positive | 1 |
| Deferred | 2 |
| Blocked | 0 |

## Fixed Issues
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Duplicate fix-commit regex — removed `build_fix_regex()`, use `is_fix_commit()` | `metrics.rs:20-22` | c6d44d0 |
| Inconsistent PathBuf conversion in `compute_coupling` — `unwrap_or_default()` | `metrics.rs:99` | c6d44d0 |
| Unchecked i64→u64 timestamp cast — `.max(0) as u64` | `metrics.rs:215` | c6d44d0 |
| Repeated `to_string_lossy().into_owned()` — added `FileChangeInfo::path_str()` | `metrics.rs` (7 sites) | c6d44d0 |
| Stale comment referencing old borrowing mechanism | `metrics.rs:89-90` | c6d44d0 |
| Redundant `info.object()` call — pass decoded commit to `changed_files_for_commit` | `git_parser.rs:133,192` | eaee6e1 |
| Unbounded commit vector — added `MAX_COMMITS = 100_000` safety cap | `git_parser.rs:123` | eaee6e1 |
| SystemTime silent fallback — return `SearchError::Git` instead of defaulting to 0 | `git_parser.rs:98-101` | eaee6e1 |
| Double allocation in tree-diff closure — use `Cow` `.as_ref()` | `git_parser.rs:229,235` | eaee6e1 |
| Missing `debug_assert` on `commit_count` invariant | `git_parser.rs:173-180` | eaee6e1 |
| `once_cell` → `std::sync::LazyLock` for consistency | `temporal/mod.rs:15` | 91c500b |
| Lossy path conversion bypasses exclusion rules — use `to_string_lossy()` | `heatmap/mod.rs:223` | d0b5fcd |
| Lossy u64→i64 timestamp cast — use `i64::try_from().unwrap_or(i64::MAX)` | `git_source.rs:216` | d0b5fcd |
| Silent test skip — added `eprintln!` before early returns | `git_parser_tests.rs` (12 sites) | ff1a8e0 |
| Missing lookback exclusion test — `test_lookback_excludes_old_commits` | `git_parser_tests.rs` (new) | ff1a8e0 |
| Missing rename tracking test — `test_file_rename_appears_in_changed_files` | `git_parser_tests.rs` (new) | ff1a8e0 |

## False Positives
| Issue | File:Line | Reasoning |
|-------|-----------|-----------|
| `parse_history_impl` function length (108 lines) | `git_parser.rs:73` | Function reads as sequential pipeline; gix lifetime constraints make extraction awkward without clones. Batch-2 changes already improved clarity. |

## Deferred to Tech Debt
| Issue | File:Line | Risk Factor |
|-------|-----------|-------------|
| Type alias divergence (`CommitInfo as CommitRecord`) | `heatmap/types.rs:9` | Migration compatibility alias — safe one-line bridge. Cleanup requires updating all CommitRecord/FileChange references in heatmap module. Follow-up item. |
| Dual git history traits (`GitDataSource` vs `TemporalSource`) | `git_source.rs:34-50`, `types.rs:213-228` | Intentional incremental migration — convergence requires replacing CLI git driver entirely. Architectural overhaul deferred. |
