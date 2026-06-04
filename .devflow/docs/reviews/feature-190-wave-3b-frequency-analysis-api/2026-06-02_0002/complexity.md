# Complexity Review Report

**Branch**: feature-190 -> main
**Date**: 2026-06-02
**PR**: #266

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

(none)

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Complexity Score**: 10/10
**Recommendation**: APPROVED

## Detailed Analysis

### New File: `ngram.rs` (256 lines)

| Metric | Value | Threshold | Status |
|--------|-------|-----------|--------|
| File length | 256 lines | < 300 (good) | Pass |
| Longest function | 5 lines (`encode`, `decode`) | < 30 (good) | Pass |
| Max cyclomatic complexity | 3 (`fmt_kind_id` — 3-branch match) | < 5 (good) | Pass |
| Max nesting depth | 1 | < 3 (good) | Pass |
| Max parameters | 3 (`AstTrigram::encode`) | < 3 (warning edge) | Pass |
| Magic values | 0 | 0 | Pass |
| Boolean complexity | 0 compound conditions | 0 | Pass |

Every function in this module is explainable in under 30 seconds. The newtypes (`AstBigram`, `AstTrigram`) each have a symmetric set of methods (`encode`, `decode`, `key`, `from_raw`, `Display`) with no branching. The vocabulary helpers (`vocab_lookup`, `vocab_resolve`, `vocab_len`) are single-expression wrappers. The weight lookup functions (`ast_bigram_idf`, `ast_trigram_idf`) are one-liner delegations with `unwrap_or` fallback.

The `fmt_kind_id` private helper is the most complex function at 3 match arms, and it handles all Display edge cases (known kind, sentinel, out-of-bounds) cleanly.

### New File: `ngram_tests.rs` (400 lines)

| Metric | Value | Threshold | Status |
|--------|-------|-----------|--------|
| File length | 400 lines | < 500 (warning edge) | Pass |
| Longest test function | ~15 lines (`trigram_roundtrip_typical_ids`) | < 30 (good) | Pass |
| Max nesting depth | 3 (triple-nested for loop in `trigram_roundtrip_typical_ids`) | 3 (warning edge) | Pass |
| Loop bounds | All explicit, small fixed arrays | explicit | Pass |

The 3-deep nested loop in `trigram_roundtrip_typical_ids` (lines 82-93) iterates 3x3x3 = 27 combinations — bounded and reasonable for exhaustive boundary testing. The test file is well-organized with clear T1-T14 section labels.

### Modified Files (formatting only)

All changes to `ast_walk.rs`, `ast_extract.rs`, `linearize_bench.rs`, `linearize.rs`, and `linearize_tests.rs` are rustfmt reformatting (line wrapping adjustments). No logic changes, no complexity impact.

### Module Structure (`mod.rs`, `lib.rs`)

Clean re-export additions. The `ast_index` module now has two sub-modules (`linearize`, `ngram`) — single responsibility maintained.

### Decisions Context

- ADR-001 (fix all noticed issues immediately): No issues found to surface.
- PF-002 (classifying findings as pre-existing to skip): Not applicable — zero findings in any category.
