# Security Review Report

**Branch**: feature/187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Unbounded `level_stack` growth in `AstWalkIter` could cause OOM on adversarial input** - `crates/rskim-core/src/ast_walk.rs:98,172`
**Confidence**: 82%
- Problem: The `level_stack: Vec<u32>` in `AstWalkIter` grows by one entry per descent into a child node (line 172: `self.level_stack.push(self.depth)`). While `max_depth` (default 500) caps the *yielded* depth, the `level_stack` is pushed in `advance()` *before* the bounds check in `next()`. In the current implementation, this means the stack could grow up to `max_depth` entries (500 x 4 bytes = 2 KiB), which is safe. However, `AstWalkConfig` has public fields, so a caller could set `max_depth` to `u32::MAX` and feed a deeply nested AST, causing unbounded stack growth. The `MAX_FILE_SIZE` guard in callers mitigates this for the current call sites (`linearize_source` and `extract_ast_ngrams_from_file`), but `AstWalkIter` is now a public API in `rskim-core` and could be used without file-size guards by future callers.
- Fix: Pre-allocate `level_stack` with a capacity capped to a reasonable maximum (e.g., `min(config.max_depth, 1024) as usize`), or add a `max_depth` ceiling assertion in the constructor. This makes the public API defensively safe regardless of caller-provided config.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`vocab_idx as u16` cast in LANG_MAPS init** - `crates/rskim-search/src/ast_index/linearize.rs:164` (Confidence: 65%) -- The cast `vocab_idx as u16` is safe today because `NODE_KIND_VOCABULARY.len() == 1740 < u16::MAX`, but a future vocabulary expansion past 65,535 entries would silently truncate. A `u16::try_from(vocab_idx).ok()` pattern would be more defensive, consistent with the `kind_id_u16` handling two lines above.

- **`item.depth as usize` unchecked cast in ast_extract** - `crates/rskim-research/src/ast_extract.rs:148` (Confidence: 62%) -- `item.depth` is `u32` and is cast to `usize` for indexing into `ancestors`. On 32-bit targets, `usize` is 32 bits so this is fine. On 64-bit targets, `u32 -> usize` is always widening. The real protection is the `depth < ancestor_cap` bounds check at lines 156 and 174. This is adequate but relies on the caller remembering to check -- a `ancestors.get_mut(depth)` pattern would make safety structural rather than procedural.

- **`item.node.kind_id() as usize` unchecked widening cast** - `crates/rskim-search/src/ast_index/linearize.rs:256` (Confidence: 60%) -- `kind_id()` returns `u16`, cast to `usize` for indexing into `lang_map`. The `.get()` call on line 257 prevents out-of-bounds access, so this is safe. The cast itself is always widening and cannot overflow. No action needed but noted for completeness.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Security Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The single MEDIUM finding concerns a defensive hardening opportunity in the newly public `AstWalkIter` API. All current call sites are protected by `MAX_FILE_SIZE` guards that prevent adversarial input from reaching the iterator with extreme `max_depth` values. The code is well-structured with proper bounds guards (`max_depth`, `max_nodes`, `MAX_FILE_SIZE`), saturating arithmetic throughout, safe numeric conversions (using `try_from` and `.get()` for bounds-checked indexing), and no unsafe code. The refactoring from duplicated DFS loops into a shared `AstWalkIter` actually *improves* the security posture by centralizing bounds-guarding logic.

Decision context: All findings are surfaced per `applies ADR-001` (fix noticed issues immediately). No findings classified as deferred -- `avoids PF-002`.
