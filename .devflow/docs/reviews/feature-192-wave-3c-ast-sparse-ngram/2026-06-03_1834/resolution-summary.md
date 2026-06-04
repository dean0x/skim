# Resolution Summary

**Branch**: feature/192-wave-3c-ast-sparse-ngram -> main
**Date**: 2026-06-03_1834
**Review**: .devflow/docs/reviews/feature-192-wave-3c-ast-sparse-ngram/2026-06-03_1834
**Command**: /resolve

## Decisions Citations

- applies ADR-001 — batch-1 (per-emit weight lookup, saturating_add, cast style), batch-2 (struct-update migration), batch-3 (B6/B7 coverage additions)
- avoids PF-002 — batch-1 (each finding given explicit Fixed/False-Positive reasoning; nothing silently deferred)
- avoids PF-003 — batch-3 (flaky-test concern verified against the diff, not assumed)
- avoids PF-004 — batch-1 (existing u32::from gap-fill widening at extract.rs:159-160 left intact; the line-152 cast fix is a different conversion)

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 9 |
| Fixed | 6 |
| False Positive | 3 |
| Deferred | 0 |
| Blocked | 0 |

## Fixed Issues
| Issue | File:Line | Commit |
|-------|-----------|--------|
| HIGH (perf): per-emit weight lookup — deferred to first insertion via `or_insert_with` (O(emits) → O(unique keys)) | extract.rs:196,212 | a9b96a5 |
| SUGGESTION: unchecked count `+= 1` → `saturating_add(1)` (self-documents the 100K bound at the pub DI boundary) | extract.rs:198,214 | a9b96a5 |
| SUGGESTION: cast style `node.depth as usize` → `usize::from(node.depth)` (matches rest of file) | extract.rs:152 | a9b96a5 |
| MEDIUM (consistency): scoring_tests.rs struct-update migration; dropped blanket `field_reassign_with_default` allow | lexical/scoring_tests.rs:3-6 | 4b9bbc8 |
| SUGGESTION (testing): B6 — independent counts across distinct edges (A×3, B×2 in one result) | extract_tests.rs:779 | 6e54960 |
| SUGGESTION (testing): B7 — weight-constant-per-key while count accumulates (guards the batch-1 `or_insert_with` refactor) | extract_tests.rs:832 | 6e54960 |

## False Positives
| Issue | File:Line | Reasoning |
|-------|-----------|-----------|
| MEDIUM (perf): sort comparator recomputes `.key()` → use `sort_by_cached_key` | extract.rs:235,245 | `key()` is an `#[inline]` u32/u64 field read (Copy, trivially cheap). `sort_by_cached_key` allocates an auxiliary `Vec<(K, usize)>` — net WORSE for keys this cheap. Reviewer themselves noted "within budget, for completeness". Correct to leave unchanged. |
| SUGGESTION (rust): `d` binding only feeds debug_assert | extract.rs:152+ | `d` is used at runtime in `ancestors[fill_start..d]` (line 167) and `ancestors[d] = ...` (line 221) — live code, not debug-only. Not removable without harming gap-fill clarity. |
| SUGGESTION (testing): dense B4 comment walkthrough | extract_tests.rs (B4) | Reviewer flagged this as a documentation nit, not a test gap. B4 already has thorough inline comments. No change warranted. |

## Deferred to Tech Debt
(none)

## Blocked
(none)

## Verification
- `cargo test -p rskim-search --lib ast_index`: 97 tests pass (was 95; +2 from B6/B7)
- `cargo test -p rskim-search --lib`: 559 pass, 1 ignored (pre-existing 100 MiB boundary test — not introduced by this resolution; no `#[ignore]` added by any resolver)
- `cargo clippy -p rskim-search --all-targets -- -D warnings`: clean

## Notes
The dominant finding (HIGH per-emit weight lookup) was a clean, behavior-preserving O(emits) → O(unique keys) optimization on the extraction hot path — all existing extract tests pass unchanged, and new test B7 pins the "weight set once, count accumulates" contract that the `or_insert_with` restructuring relies on. Two MEDIUM/suggestion items were validated as false positives with concrete reasoning (the `sort_by_cached_key` swap would regress performance for trivially-cheap keys; the `d` binding is live runtime code). No issues deferred — every actionable finding resolved per ADR-001.

## Process Note (devflow state, not code)
PF-004 ("u16 depth arithmetic overflow — widen to u32 before adding offset") is cited in the ast-index KNOWLEDGE base and decisions-log but is **missing from `.devflow/decisions/pitfalls.md`** (only PF-001..PF-003 present). A background sidecar reported appending it, but the canonical file does not contain the entry — likely a silent append failure or race. The code complies with PF-004's intent regardless. Recommend a manual decisions-append to restore the canonical PF-004 entry.
