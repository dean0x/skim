# Reliability Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31T13:38

## Issues in Your Changes (BLOCKING)

### HIGH

**DF counter overflow via non-saturating `+= 1` on `u32` (2 occurrences)** - Confidence: 85%
- `crates/rskim-research/src/ast_extract.rs:283`, `crates/rskim-research/src/ast_extract.rs:286`
- Problem: The bigram and trigram document-frequency counters use `+= 1` (wrapping in release, panicking in debug) while the node counters on lines 279-280 correctly use `saturating_add`. If a single bigram appears in more than `u32::MAX` files (unlikely in practice but possible with a large corpus of deduplicated files), the counter wraps to 0, corrupting the IDF weight for that bigram. The inconsistency with the saturating pattern used four lines above suggests this was an oversight rather than a deliberate choice.
- Fix: Use `saturating_add` for consistency with the surrounding counters:
```rust
for bigram in result.bigrams {
    let count = bigram_df.entry(bigram).or_default();
    *count = count.saturating_add(1);
}
for trigram in result.trigrams {
    let count = trigram_df.entry(trigram).or_default();
    *count = count.saturating_add(1);
}
```

**`lang_file_count` and `total_unique_files` u32 counters use non-saturating `+= 1`** - Confidence: 82%
- `crates/rskim-research/src/ast_extract.rs:260`, `crates/rskim-research/src/ast_extract.rs:261`
- Problem: These counters increment with `+= 1`, which panics in debug builds on overflow. The `total_deduplicated` counter on line 256 has the same pattern. While `u32::MAX` files is improbable for a research tool, the inconsistency with `saturating_add` used on lines 279-280 for node counts introduces an asymmetric failure mode: node counts safely saturate but file counts would panic. The Iron Law of reliability requires consistent bounding on all counters.
- Fix: Use `saturating_add` for all file counters to match the node counter pattern:
```rust
lang_file_count = lang_file_count.saturating_add(1);
total_unique_files = total_unique_files.saturating_add(1);
// ...
total_deduplicated = total_deduplicated.saturating_add(1);
```

### MEDIUM

**`percentile()` panics on negative or NaN `pct` values via `as usize` cast** - Confidence: 80%
- `crates/rskim-research/src/ast_validate.rs:140`
- Problem: The expression `((pct / 100.0) * (sorted.len() - 1) as f32).round() as usize` converts an `f32` to `usize`. If `pct` is negative (e.g., -10.0), the result after `.round()` is a negative `f32`, and casting a negative `f32` to `usize` is saturating to 0 in current Rust editions but was previously UB and remains implementation-defined. If `pct` is NaN (from 0.0/0.0), `.round()` returns NaN, and `NaN as usize` yields 0 on current Rust but is not guaranteed to be portable or stable across editions. The function is currently only called with hardcoded values (50.0, 90.0, 99.0) so this is unlikely to manifest, but the function is `fn percentile` with a general signature that invites reuse.
- Fix: Add a `debug_assert!` precondition consistent with the project's assertion density standards:
```rust
fn percentile(sorted: &[f32], pct: f32) -> f32 {
    debug_assert!(
        pct >= 0.0 && pct <= 100.0 && !pct.is_nan(),
        "percentile pct must be in [0.0, 100.0], got {pct}"
    );
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((pct / 100.0) * (sorted.len() - 1) as f32).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Recursive `walk_tree` with MAX_AST_DEPTH=500 may exceed default stack** - `crates/rskim-research/src/ast_extract.rs:132` (Confidence: 65%) -- Each recursion frame includes a `TreeCursor`, `WalkContext` reference, `depth`, and two `Option<NodeKindId>` values (~100+ bytes per frame). At depth 500, this uses ~50KB of stack, which fits within the default 8MB Rust stack but leaves less margin if called from a deeply nested call chain. The depth bound (500) is well within safe limits for normal use; this is a minor robustness observation.

- **`compute_idf` uses `debug_assert!` for `total_docs > 0` but callers guard externally** - `crates/rskim-research/src/idf.rs:18` (Confidence: 62%) -- The `total_docs == 0` guard exists at every call site (`compute_ast_bigram_weights` line 28, `compute_ast_trigram_weights` line 74), so the debug_assert is redundant in practice. However, the function's contract relies on callers remembering to guard -- a production `assert!` would be more defensive. This is pre-existing code not modified in this PR.

- **`NodeKindVocabulary::stabilize` performs a `clone()` per kind string during `kind_to_id` reconstruction** - `crates/rskim-research/src/ast_types.rs:243` (Confidence: 60%) -- Line 243 calls `kind.clone()` to rebuild the `kind_to_id` HashMap after stabilize. With O(100-1000) node kinds, this is negligible, but avoiding the clone via an `Rc<str>` or storing `&str` references would be more allocation-disciplined per the reliability pattern for allocation minimization.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Reliability Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The code demonstrates strong reliability practices overall -- bounded AST traversal (MAX_AST_DEPTH, MAX_AST_NODES, MAX_FILE_SIZE, MAX_TRIGRAMS_PER_FILE), git subprocess timeouts, overflow-checked vocabulary insertion, and SHA-256 deduplication. The three findings are all about consistency: two counter groups use different overflow strategies (`saturating_add` vs bare `+= 1`), and one private function lacks a precondition assertion. Addressing these aligns the new code with the reliability standards already demonstrated by the surrounding implementation. Applies ADR-001 (fix all noticed issues immediately). Avoids PF-002 (all findings surfaced for resolution, none deferred).
