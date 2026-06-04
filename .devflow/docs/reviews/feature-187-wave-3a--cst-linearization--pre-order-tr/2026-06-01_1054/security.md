# Security Review Report

**Branch**: feature-187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01
**PR**: #265

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Silent u16 truncation in LANG_MAPS initialization when kind_count > 65535** - `linearize.rs:153`
**Confidence**: 82%
- Problem: `node_kind_count()` returns `usize` but the loop variable `kind_id` is cast to `u16` via `kind_id as u16` on line 153. If a tree-sitter grammar ever reports more than 65,535 node kinds, the cast silently wraps around, causing `node_kind_for_id()` to receive the wrong ID. This would produce incorrect vocabulary mappings for any grammar kind with ID >= 65536.
- Impact: Incorrect kind-to-vocabulary mappings could cause wrong structural fingerprints. Current grammars have well under 65K kinds (typical: 200-500), so the risk is theoretical but the pattern is unsafe. The tree-sitter API already constrains `node_kind_for_id()` to accept `u16`, meaning it cannot return kinds above 65535 either, which makes the cast correct in practice. However, the code allocates a `Vec<Option<u16>>` of size `kind_count` (usize), which could be wastefully large if `kind_count` exceeds u16 range.
- Fix: Add a guard or use `u16::try_from(kind_id)` to fail explicitly:
```rust
let kind_id_u16 = match u16::try_from(kind_id) {
    Ok(id) => id,
    Err(_) => continue, // kind_id exceeds u16 range; skip
};
if let Some(kind_str) = ts_lang.node_kind_for_id(kind_id_u16) {
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Uncounted nodes in the MAX_AST_NODES skip path** - `linearize.rs:253-266` (Confidence: 65%) -- When the `MAX_AST_NODES` guard triggers at line 253, the inner loop at lines 255-265 moves the cursor to the next sibling or ascends without incrementing `node_count` for the nodes being skipped. This means the `node_count` field understates the total nodes in the tree, and the documented invariant `node_count == nodes.len() + error_count` still holds but `node_count` no longer reflects actual tree size. Not a security issue but could affect downstream accuracy assumptions.

- **Error message includes language Debug repr** - `linearize.rs:207` (Confidence: 62%) -- The error message `format!("grammar load failure for {language:?}: {e}")` uses `Debug` formatting for the language enum, which is fine for internal error messages but could expose internal type names if error messages are ever surfaced to end users. Low risk given this is a library-internal error for a configuration failure scenario.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Security Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Conditions

1. Consider guarding the `kind_id as u16` cast with `u16::try_from()` to prevent silent truncation if future grammar changes push kind counts beyond u16 range.

### Positive Security Observations

- **MAX_FILE_SIZE guard** (line 51, 194): 100 KiB input size limit prevents resource exhaustion from oversized files. Well-documented and consistent with existing `ast_extract.rs`.
- **MAX_AST_DEPTH guard** (line 40, 253): Depth cap of 500 prevents stack-like memory exhaustion from pathological nesting. Uses `saturating_add` (line 299) to prevent depth counter overflow.
- **MAX_AST_NODES guard** (line 45, 253): 100K node cap bounds output allocation and prevents runaway traversal on generated/minified files.
- **No file I/O**: The module processes `&str` input only -- no filesystem access, no network calls, no credential handling. The attack surface is limited to the source string parameter.
- **No unsafe code**: All operations use safe Rust. Tree-sitter FFI is encapsulated in the upstream crate.
- **Bounded allocations**: `Vec::with_capacity` uses `min(descendant_count, MAX_AST_NODES)` (line 237), preventing allocation amplification.
- **LazyLock thread safety**: Static initialization via `LazyLock` is inherently thread-safe, preventing TOCTOU races during concurrent first-use initialization.
- **Result types throughout**: All fallible operations return `Result`, with clear distinction between configuration errors (Err) and parse errors (empty Ok). Applies ADR-001 by surfacing rather than hiding failures.
- **Error/MISSING node exclusion**: Malformed input does not produce vocabulary entries -- error nodes are counted but excluded from the linearized output, preventing garbage data from contaminating downstream analysis.
