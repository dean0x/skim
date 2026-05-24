# Resolution Summary

**Branch**: refactor/230-232-233-tech-debt-pipeline -> main
**Date**: 2026-05-17_2315
**Review**: .docs/reviews/refactor-230-232-233-tech-debt-pipeline/2026-05-17_2315
**Command**: /resolve

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 12 |
| Fixed | 9 |
| False Positive | 1 |
| Deferred | 1 |
| Blocked | 0 |
| Pre-existing (not addressed) | 4 |

## Fixed Issues
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Mtime docs: update "4-tier" to "2-tier SHA cache" | index.rs:1,10,213,325,362 | ae14669 |
| Pipeline::run decomposition into stage methods | index.rs:188-416 | ae14669 |
| FileId overflow: break instead of fatal error | index.rs:293-301 | ae14669 |
| ProcessedFile DRY: single construction site | index.rs:382-401 | ae14669 |
| Producer thread Send requirement documented | index.rs:296 | dab1bfc |
| run_classify doc: "rayon worker pool" → "producer thread" | index.rs:475 | dab1bfc |
| walk_metadata: extract handle_metadata_entry helper | walk.rs:391-475 | dab1bfc |
| Module doc: mention walk_metadata as production entry point | walk.rs:1-6 | dab1bfc |
| classify_entry_metadata: capture metadata() once | walk.rs:331-355 | dab1bfc |

## False Positives
| Issue | File:Line | Reasoning |
|-------|-----------|-----------|
| walk_skips vector kept alive across pipeline | index.rs:186-188 | After Batch 1 decomposed Pipeline::run into stage methods, walk() returns (entries, skip_count) and the skips vector is dropped when walk() returns — before the channel is opened. Already resolved. |

## Deferred to Tech Debt
| Issue | File:Line | Risk Factor |
|-------|-----------|-------------|
| Single-threaded producer serializes I/O and classification | index.rs:226-245 | Intentional design tradeoff per plan: "Sequential bottleneck acknowledged... current design prioritizes correctness and simplicity." Parallel producer (rayon inside producer thread) is a viable follow-up optimization but adds concurrency complexity. |

## Pre-Existing (Informational)
| Issue | File:Line | Note |
|-------|-----------|------|
| ReadOutcome is not a Result type | walk.rs | Pre-existing pattern, not introduced by this PR |
| classify_entry duplicates classify_entry_metadata logic | walk.rs | Test-only code, acceptable duplication |
| Test-only walk_and_read diverges from production walk_metadata | walk.rs | Known limitation of #[cfg(test)] retention |
| walk_metadata/walk_and_read orchestration duplication | walk.rs | Test-only code, acceptable |
