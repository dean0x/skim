# Reliability Review Report

**Branch**: feat/180-bm25f-scoring -> main
**Date**: 2026-05-16

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**NaN/Infinity pass through BM25FConfig validation** - `crates/rskim-search/src/lexical/config.rs:64-86`
**Confidence**: 95%
- Problem: The `validate()` method checks `k1 < 0.0`, `boost < 0.0`, and `b` outside `[0.0, 1.0]`, but `f32::NAN` compares `false` for all these conditions. A caller can submit `k1: f32::NAN`, `field_boosts: [f32::NAN; 8]`, or `field_b: [f32::NAN; 8]` and pass validation. NaN propagates through `bm25f_score()` producing NaN scores, which corrupt the sort comparator (`partial_cmp` returns `None` for NaN, falling back to `Equal`), yielding non-deterministic result ordering. Similarly, `f32::INFINITY` passes the boost check (`INFINITY >= 0.0` is true) and can produce infinite scores.
- Fix: Add explicit `is_finite()` checks to `validate()`:
```rust
pub fn validate(&self) -> Result<()> {
    if !self.k1.is_finite() || self.k1 < 0.0 {
        return Err(SearchError::InvalidQuery(format!(
            "BM25FConfig: k1 must be finite and >= 0.0, got {}",
            self.k1
        )));
    }
    for (i, &boost) in self.field_boosts.iter().enumerate() {
        if !boost.is_finite() || boost < 0.0 {
            return Err(SearchError::InvalidQuery(format!(
                "BM25FConfig: field_boosts[{i}] must be finite and >= 0.0, got {boost}"
            )));
        }
    }
    for (i, &b) in self.field_b.iter().enumerate() {
        if !b.is_finite() || !(0.0..=1.0).contains(&b) {
            return Err(SearchError::InvalidQuery(format!(
                "BM25FConfig: field_b[{i}] must be finite and in [0.0, 1.0], got {b}"
            )));
        }
    }
    Ok(())
}
```

### MEDIUM

(none)

## Issues in Code You Touched (Should Fix)

### HIGH

(none)

### MEDIUM

**`compute_field_lengths` silently saturates to `u32::MAX` without error** - `crates/rskim-search/src/index/builder.rs:200-217`
**Confidence**: 80%
- Problem: When a single `field_map` range exceeds `u32::MAX` bytes, or when `saturating_add` clamps the accumulated length, the function silently produces a clamped value. Since `doc_length` was already validated to fit `u32` above (line 133), and `classify_source` bounds input to 100 MiB (well under `u32::MAX` = 4 GiB), saturation is unreachable in practice. However, the `unwrap_or(u32::MAX)` on line 208 (empty field_map path) and line 212 would silently hide a logic error if a future change to `MAX_SOURCE_BYTES` increased the limit. A debug assertion would catch this invariant violation during development without runtime cost.
- Fix: Add a `debug_assert!` after computing lengths to catch silent clamping in tests:
```rust
debug_assert!(
    lengths.iter().all(|&l| l < u32::MAX),
    "field_length saturation: a field exceeded u32::MAX, which should be impossible given MAX_SOURCE_BYTES"
);
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Tree-sitter AST walk loop in classifier has implicit termination** - `crates/rskim-search/src/lexical/classifier.rs:136-174` (Confidence: 70%) -- The `loop` at line 136 relies on the tree-sitter cursor reaching the root to terminate. While tree-sitter guarantees finite traversal for valid trees, a malformed AST or tree-sitter bug could theoretically cause infinite traversal. A bounded iteration guard (e.g., `MAX_NODES = source_len * 2`) would make termination explicit per the "bounded iteration" reliability rule, though tree-sitter has no known bugs that would trigger this.

- **HashMaps in search() grow unbounded by query** - `crates/rskim-search/src/index/reader.rs:281-287` (Confidence: 65%) -- `doc_scores`, `doc_field_tfs`, `doc_positions`, and `doc_meta_cache` are all unbounded HashMaps that grow proportional to the number of matching documents. For a large corpus with a very common bigram, these could consume significant memory. A pre-sized allocation with `HashMap::with_capacity(file_count.min(1024))` or an early-exit after accumulating enough candidates would improve predictability.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 0 | 0 |

**Reliability Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The implementation demonstrates strong reliability practices overall: bounded input via `MAX_SOURCE_BYTES` (100 MiB), checked arithmetic throughout (`checked_add`, `checked_mul`, `try_from`), division-by-zero guards in `bm25f_score()`, explicit `norm <= 0.0` protection, CRC32 integrity checking on the on-disk format, and format version rejection with clear error messages. The single blocking issue is the NaN/Infinity gap in `BM25FConfig::validate()` -- since `validate()` is called at trust boundaries (`open_with_config`, `search` with per-query config), fixing it closes the gap between the documented invariants and what the validator actually enforces. The `compute_field_lengths` saturation is a minor defense-in-depth improvement for future-proofing.
