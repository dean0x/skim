# Complexity Review Report

**Branch**: feature/190-wave-3b-frequency-analysis-api -> main
**Date**: 2026-06-02
**Cycle**: 2 (prior cycle resolved 6 of 9 issues; 3 false positives)

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Test file `ngram_tests.rs` exceeds 400-line soft limit (451 lines)** - `crates/rskim-search/src/ast_index/ngram_tests.rs`
**Confidence**: 82%
- Problem: The test file is 451 lines, exceeding the 400-line target that commit `9cf5e0e` ("consolidate ngram tests to meet 400-line file limit (AC-6)") was specifically created to address. The subsequent commit `45af2be` added ordering tests (T14) and pushed the file back over the limit. The file has 14 test groups with 45 individual test functions -- the high count comes from thorough coverage, not poor structure.
- Fix: This is borderline. The file is well-organized with clear section headers (T1 through T15) and every test is short (2-10 lines of assertions). At 451 lines the overshoot is minor (13%). If the project enforces the 400-line limit strictly, the T5 Display formatting tests (lines 155-208, 54 lines, 5 tests) could be extracted into a `ngram_display_tests.rs` file since they test a distinct concern (Display trait impl) from the core encode/decode/weight logic.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

**Test file `linearize_tests.rs` exceeds 500 lines (518 lines)** - `crates/rskim-search/src/ast_index/linearize_tests.rs`
**Confidence**: 80%
- Problem: The linearize test file is 518 lines. Only formatting changes were made in this PR (no new logic). The file covers 8 test cycles and is structurally well-organized but exceeds the WARNING threshold (300-500 lines).
- Note: Pre-existing. Not introduced by this PR. Informational only per applies ADR-001 (surface for user decision).

## Suggestions (Lower Confidence)

(none)

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 1 |

**Complexity Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

## Rationale

This PR introduces exceptionally low-complexity code. The core production module (`ngram.rs`, 256 lines) contains:

- **13 functions**, all under 7 lines of body. The longest function is `AstTrigram::decode` at 5 lines.
- **Zero nesting beyond 1 level** (single match in `fmt_kind_id`).
- **Cyclomatic complexity of 1 for every function** except `fmt_kind_id` (complexity 3, three match arms -- well within bounds).
- **No boolean complexity**: no compound conditions anywhere.
- **No magic values**: all constants are named (`DEFAULT_AST_WEIGHT`, `NODE_KIND_VOCABULARY`).
- **Clean separation**: newtypes, vocabulary helpers, weight lookup, and formatting are each in their own labeled section with section-header comments.

The design follows the "intentionally minimal" pattern documented in the feature knowledge. Every public function is a one-liner delegation or a pure bitwise operation. The `#[repr(transparent)]` newtype pattern keeps the API type-safe while the internal representation matches the weight table layout for zero-cost lookup.

Test complexity is proportionally higher (451 lines for 256 lines of production code), which is expected for boundary-value and roundtrip coverage of bit-packing operations. The tests are individually simple -- the volume comes from thorough coverage across T1-T15 test groups, not from complex test logic.

The only condition for approval is deciding whether the 451-line `ngram_tests.rs` needs to be split given the project's prior commitment to a 400-line limit (commit `9cf5e0e`). The overshoot is modest and the file is well-structured, so this is a judgment call rather than a complexity problem.
