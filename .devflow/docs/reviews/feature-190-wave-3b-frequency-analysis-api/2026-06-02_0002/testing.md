# Testing Review Report

**Branch**: feature-190-wave-3b-frequency-analysis-api -> main
**Date**: 2026-06-02
**PR**: #266

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**T9/T12 IDF tests assert weight > DEFAULT but do not verify the exact value** - `ngram_tests.rs:293-347`
**Confidence**: 82%
- Problem: Tests `bigram_idf_known_rust_entry_above_default` (T9) and `trigram_idf_known_rust_entry_above_default` (T12) hardcode comments referencing a specific weight value (11.251047) from `RUST_AST_BIGRAM_WEIGHTS[0]` / `RUST_AST_TRIGRAM_WEIGHTS[0]`, but the assertion only checks `w > DEFAULT_AST_WEIGHT`. If a weight table regeneration changes the first entry's kind pair but keeps its weight at 1.0001, the test would vacuously pass. The comment creates false confidence about what is being verified.
- Fix: Either (a) assert the exact expected weight `assert!((w - 11.251047).abs() < f32::EPSILON)` to truly pin the known entry, or (b) remove the comments referencing the exact weight since the test only verifies the "above default" contract. Option (b) is preferred since it keeps the test behavioral and resilient to table regeneration (applies ADR-001 -- fix the misleading comment now rather than defer).

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Consider property-based test for encode/decode roundtrip** - `ngram_tests.rs:12-52` (Confidence: 65%) -- The T1/T2 roundtrip tests cover boundary values and typical ranges well, but a proptest/quickcheck-style test with `fc.assert(fc.property(fc.u16(), fc.u16(), (a, b) => AstBigram::encode(a, b).decode() == (a, b)))` would exhaustively verify the full u16 domain. Current coverage is adequate but not exhaustive.

- **Only Rust is exercised for "known entry" IDF paths** - `ngram_tests.rs:293-370` (Confidence: 70%) -- T9/T12 verify known-entry lookup for Rust only. 14 languages have weight tables. A single additional language (e.g., TypeScript or Python) would increase confidence that `ast_bigram_idf` dispatch works across languages, not just for the "Rust" match arm.

- **No test for `AstBigram`/`AstTrigram` ordering correctness** - `ngram.rs:60,123` (Confidence: 62%) -- Both types derive `PartialOrd, Ord` but no test verifies that ordering of the packed key matches the intended semantic ordering (parent-major for bigrams, grandparent-major for trigrams). If a consumer sorts a vec of bigrams, the ordering contract matters.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Rationale

The test suite is well-structured and thorough. 37 tests covering T1-T13 acceptance criteria is strong coverage. Specific strengths:

1. **Behavior-focused**: Tests verify observable behavior (roundtrip, display output, weight lookup contracts) rather than implementation details. The Arrange-Act-Assert structure is clean and consistent throughout.

2. **Boundary value coverage**: Encode/decode roundtrips test zero, max, asymmetric, and typical values -- the important boundary cases for bit-packing logic.

3. **Error path coverage**: Unknown/out-of-bounds IDs, non-tree-sitter language fallbacks, and sentinel values are all tested explicitly.

4. **Consistent patterns**: Uses the same `#[path]` test module pattern as `linearize_tests.rs` (confirmed by feature knowledge). Test naming is descriptive and follows the `{component}_{behavior}` convention.

5. **Clean setup**: No test requires more than 3-4 lines of setup. No mocks needed -- the module's pure-function design makes testing trivial, which is a sign of good architecture.

The single MEDIUM finding is about misleading comments, not missing behavior. The three suggestions are genuine improvements but none represent gaps in the acceptance criteria.
