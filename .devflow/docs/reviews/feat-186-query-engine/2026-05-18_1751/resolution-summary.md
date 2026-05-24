# Resolution Summary

**Branch**: feat/186-query-engine -> main
**Date**: 2026-05-18
**Review**: .docs/reviews/feat-186-query-engine/2026-05-18_1751
**Command**: /resolve

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 11 |
| Fixed | 5 |
| False Positive | 6 |
| Deferred | 0 |
| Blocked | 0 |

## Fixed Issues
| Issue | File:Line | Commit |
|-------|-----------|--------|
| Delegation test missing 3/7 field assertions | query_tests.rs:207-234 | 74f9eaa |
| Delegation test only exercises None-valued optionals | query_tests.rs:213 | 74f9eaa |
| Error variant assertions weakened to Display string | query_tests.rs:112-179 | 74f9eaa |
| Oversized-query short-circuit not proven via PanicLayer | query_tests.rs:106-117 | 74f9eaa |
| Invalid BM25F short-circuit not proven via PanicLayer | query_tests.rs:129+ | 74f9eaa |

## False Positives
| Issue | File:Line | Reasoning |
|-------|-----------|-----------|
| PR description says "64KB" vs code 4096 | query.rs:15 | Code is correct; issue is in PR description, not code |
| Unvalidated limit/offset passthrough | query.rs:50 | Intentional — decorator's responsibility is text boundary validation, not downstream iterator semantics |
| BM25F tests could be parameterized | query_tests.rs:128-180 | Style preference; explicit tests for distinct edge cases are more readable |
| Arc<SpyLayer> orphan-rule workaround | query_tests.rs:46-54 | Well-understood pattern, used once, does not recur |
| format! allocation on error path | query.rs:56-59 | Error path only triggers for >4KiB queries; diagnostic value outweighs negligible cost |
| Mutex poisoning on panic | query_tests.rs:31,37 | Standard Rust test practice with #![allow(clippy::unwrap_used)] |

## Deferred to Tech Debt

_(none)_

## Blocked

_(none)_
