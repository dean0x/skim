# Resolution Summary

**Branch**: feature/192-wave-3c-ast-sparse-ngram -> main
**Date**: 2026-06-03_1229
**Review**: .devflow/docs/reviews/feature-192-wave-3c-ast-sparse-ngram/2026-06-03_1229
**Command**: /resolve

## Decisions Citations

- applies ADR-001 — batch-1-source (u16 overflow, collapsible_if, debug_assert, HashMap, docs), batch-2-tests (perf-gate replacement, all coverage additions)
- avoids PF-002 — batch-1-source (u16 overflow fixed rather than dismissed as "currently unreachable in production")
- avoids PF-003 — batch-2-tests (test assertions verified against current extract.rs output, not assumed)

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 13 |
| Fixed | 13 |
| False Positive | 0 |
| Deferred | 0 |
| Blocked | 0 |

## Fixed Issues
| Issue | File:Line | Commit |
|-------|-----------|--------|
| u16 overflow in gap-fill arithmetic (p+1) — widened to u32 | extract.rs:141-142 | 4800fa6 |
| Stale `#[allow(clippy::collapsible_if)]` x3 — collapsed to let-chains, allows deleted | extract.rs:139,161,173 | 4800fa6 |
| Missing assertion density — added 3 debug_assert! for table-sizing invariants | extract.rs:116-187 | 4800fa6 |
| HashMap over-allocation — capped at nodes.len().min(1024) | extract.rs:127-128 | 4800fa6 |
| Doc overstated "reproduces" the research walk chain-break — softened to "approximate" | extract.rs module doc | 4800fa6 |
| Missing rationale for no MAX_TRIGRAMS cap — added explanatory comment | extract.rs:171-181 | 4800fa6 |
| Undocumented DI allocation contract — added # Allocation doc section | extract.rs (fn doc) | 4800fa6 |
| u16::MAX depth regression test (locks the overflow fix) | extract_tests.rs:511 | cbe2a9e |
| Residual gap-fill edge case (same-depth sibling spurious edge) characterization test | extract_tests.rs:582 | cbe2a9e |
| Trigram count accumulation test (repeated triple ×3) | extract_tests.rs:636 | cbe2a9e |
| Max-depth boundary tests for gap-fill slice | extract_tests.rs:671 | cbe2a9e |
| Depth-0 / single-node underflow-guard tests | extract_tests.rs:700 | cbe2a9e |
| Flaky perf-gate test replaced with correctness smoke test (Criterion is the latency gate) | extract_tests.rs:484 | cbe2a9e |

## False Positives
(none)

## Deferred to Tech Debt
(none)

## Blocked
(none)

## Verification
- `cargo test -p rskim-search --lib ast_index`: 95 tests pass (was 86; +10 new tests, net of the replaced gated perf test)
- `cargo clippy -p rskim-search --all-targets -- -D warnings`: 0 warnings, 0 errors
- Full lib suite: 560 tests pass

## Notes
All findings were confined to `extract.rs` and `extract_tests.rs`. The dominant blocking
finding (u16 overflow at the public DI boundary) was fixed by widening the gap-fill
comparison and slice-start derivation to u32, and locked with a `u16::MAX` regression test.
No issues were deferred — every MEDIUM cleanup was applied per ADR-001.
