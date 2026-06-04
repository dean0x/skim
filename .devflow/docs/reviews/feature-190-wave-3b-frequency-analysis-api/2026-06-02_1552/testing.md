# Testing Review Report

**Branch**: feature/190-wave-3b-frequency-analysis-api -> main
**Date**: 2026-06-02
**Cycle**: 2 (incremental — prior cycle fixed 6 issues, resolved 3 false positives)

## Cross-Cycle Awareness

Prior cycle (Cycle 1) fixed 6 issues including ordering semantics tests (T14), misleading weight comments, and a TypeScript IDF test. 3 items were resolved as false positives (proptest cost-benefit unfavorable, test prefix style cosmetic). This review focuses on the incremental diff since those fixes (e2fa552...HEAD) and does not re-raise resolved items.

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Missing `vocab_resolve_and_lookup_are_inverses` fails-silently for missing kinds** - `ngram_tests.rs:261-275`
**Confidence**: 82%
- Problem: The test uses `if let Some(id) = vocab_lookup(kind)` to silently skip kinds that are not in the vocabulary. If a future vocabulary regeneration drops one of the four hardcoded kind strings (`"abstract_type"`, `"bounded_type"`, `"function_item"`, `"source_file"`), this test passes vacuously without testing any roundtrip. The test's intent is to prove that `vocab_resolve` and `vocab_lookup` are inverses, but the conditional silently weakens the coverage.
- Fix: Replace the conditional with an `expect` or `unwrap`, since these kinds are fundamental to the vocabulary and their absence would indicate a regression. The test already has `#![allow(clippy::unwrap_used)]` at the module level:
```rust
for kind in ["abstract_type", "bounded_type", "function_item", "source_file"] {
    let id = vocab_lookup(kind).expect(&format!("{kind} must be in vocabulary"));
    assert_eq!(
        vocab_resolve(id),
        Some(kind),
        "roundtrip failed for {kind:?}"
    );
}
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **No negative/error-path test for `ast_bigram_idf` with a valid-but-low-weight bigram** - `ngram_tests.rs` (Confidence: 65%) -- Tests check `> DEFAULT_AST_WEIGHT` for known entries and `== DEFAULT_AST_WEIGHT` for unknown entries, but there is no test verifying the actual f32 weight value from the table is finite and positive. If the codegen ever emitted NaN or negative weights, the current tests would not catch it.

- **PR description claims "45 tests" but actual `#[test]` count in ngram_tests.rs is 41** - (Confidence: 72%) -- The discrepancy is minor and may reflect a different counting methodology (e.g., counting parameterized loop iterations). The test groups T1-T15 (plus T9b) are present and well-labeled, so traceability is good. Not a functional issue.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

## Detailed Assessment

### Test Coverage Quality

The ngram module (ngram.rs, 256 lines of production code) has excellent test coverage across 41 `#[test]` functions organized into 16 labeled groups (T1-T15, T9b). Coverage spans:

- **Encode/decode roundtrips** (T1, T2): Boundary values (0, u16::MAX), asymmetric pairs, typical vocabulary-range IDs. Both bigram and trigram. This is thorough.
- **Key formula verification** (T3): Explicit formula assertions for both types, including boundary values. Good.
- **Internal constructor safety** (T4): `from_raw` roundtrip tests confirm raw key reconstruction.
- **Display formatting** (T5): Known IDs, sentinel ID 0, out-of-bounds IDs -- all three branches of `fmt_kind_id` are covered.
- **Vocabulary helpers** (T6, T7, T8): Lookup, resolve, length. Sentinel behavior, roundtrips, nonexistent keys, out-of-bounds. Thorough.
- **IDF weight lookup** (T9, T9b, T10, T11, T12): Known Rust and TypeScript bigrams return above-default weight. Unknown bigrams and non-tree-sitter languages return default. Trigram parallels. Solid.
- **Encoding consistency** (T13): Verifies `encode()` produces keys matching the stored weight table entries. Uses safe `u16::try_from` casts (applies ADR-001 -- the earlier cycle fixed the raw `as u16` casts here).
- **Ordering semantics** (T14): Parent-major bigram ordering, grandparent-major trigram ordering, child tiebreak. Added per Cycle 1 findings.
- **Constant value** (T15): `DEFAULT_AST_WEIGHT == 1.0` pinned.

### Test Design Quality

- **Behavior-focused**: Tests assert observable outcomes (decode values, Display strings, weight magnitudes) rather than internal state. No spying on private methods.
- **Arrange-Act-Assert**: Clean AAA structure throughout. Setup is minimal (1-3 lines per test).
- **Assertion messages**: Every assert includes a descriptive message with context values. This is excellent for debugging failures.
- **No flaky patterns**: All tests are deterministic -- no timing, no shared mutable state, no ordering dependencies.
- **Node count invariant**: The linearize_tests.rs changes consistently assert `result.node_count == result.nodes.len() + result.error_count` on every result, per the documented invariant (avoids PF-002 -- all invariant checks are explicit, not silently skipped).

### Linearize Test Changes

The linearize_tests.rs changes (68 lines) are exclusively formatting reformats (rustfmt compliance) with no behavioral changes. The 30 existing tests continue to pass and cover the 8 documented test cycles.

### What's Well Done

1. Test group labels (T1-T15) map cleanly to the KNOWLEDGE.md documentation, making traceability straightforward.
2. Boundary value testing is comprehensive -- 0, 1, u16::MAX, asymmetric pairs.
3. The encoding consistency test (T13) directly validates that the production API produces keys compatible with the stored weight tables -- this is the most valuable test for catching encoding regressions.
4. The TypeScript IDF test (T9b) was added per Cycle 1 findings, expanding coverage beyond Rust-only weight tables.
