# Reliability Review Report

**Branch**: feature/187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01T16:58

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**`level_stack` in `AstWalkIter` has no pre-allocation or capacity bound** - `crates/rskim-core/src/ast_walk.rs:118`
**Confidence**: 82%
- Problem: `level_stack: Vec::new()` starts empty and grows dynamically on each `goto_first_child()` call (line 172). While `max_depth` bounds the conceptual depth, the `level_stack` capacity is never pre-sized and has no explicit cap. The stack grows proportionally to tree depth (up to `max_depth = 500`), which is fine for 500 entries of `u32` (2 KiB). However, the `level_stack` is only bounded *implicitly* by `max_depth` -- the bounds guard in `next()` (line 214) prevents yielding nodes at `depth >= max_depth`, but the `advance()` method (line 170-173) pushes onto `level_stack` *before* the bounds guard runs on the next iteration, meaning the stack can briefly reach `max_depth + 1` entries. This is not a memory safety issue (2 KiB), but violates the reliability principle that every collection should have an explicit capacity bound when the maximum size is known.
- Fix: Pre-allocate `level_stack` to the known maximum depth:
```rust
level_stack: Vec::with_capacity(config.max_depth as usize),
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Ancestor table allocation of 501 `Option<NodeKindId>` entries regardless of input** - `crates/rskim-research/src/ast_extract.rs:136-137`
**Confidence**: 80%
- Problem: `vec![None; ancestor_cap]` allocates 501 entries (each `Option<NodeKindId>` is 4 bytes = ~2 KiB) on every call to `walk_tree`, even for trivially small files. The allocation itself is small, but the pattern violates allocation discipline: the actual depth reached for most files is 10-30 levels, so 95%+ of the allocation is never written. For the linearize.rs caller this is avoided (no ancestor table needed), but this caller always allocates the full table.
- Fix: Consider a fixed-size stack array `[Option<NodeKindId>; 64]` for common depths with a fallback `Vec` for deep trees, or accept the 2 KiB allocation as acceptable for this use case. Given the typical call frequency (once per file), this is low-impact and the current approach is defensible.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Inner loop in `next()` relies on cursor movement for termination** - `crates/rskim-core/src/ast_walk.rs:212` (Confidence: 70%) -- The inner `loop` at line 212 terminates when either `skip_subtree()` returns false (exhaustion) or the bounds guard passes and a node is yielded. Termination depends on tree-sitter's `TreeCursor` making forward progress on each `goto_next_sibling()` / `goto_parent()` call. If a tree-sitter bug caused cursor movement to silently fail (returning true without advancing), this loop would spin. An iteration counter inside the loop (bounded by `max_nodes`) would provide defense-in-depth, though this is extremely unlikely given tree-sitter's maturity.

- **`node.kind_id() as usize` unchecked widening cast** - `crates/rskim-search/src/ast_index/linearize.rs:256` (Confidence: 65%) -- `kind_id()` returns `u16`, cast to `usize` for indexing into `lang_map`. The cast is always safe (u16 fits in usize on all platforms), and the subsequent `.get()` handles out-of-bounds gracefully. No action needed, but the explicit `usize::from()` pattern would be more idiomatic for documenting intent.

- **`MAX_AST_DEPTH` and `MAX_AST_NODES` duplicated across two crates** - `crates/rskim-search/src/ast_index/linearize.rs:40-45` and `crates/rskim-research/src/ast_extract.rs:21-24` (Confidence: 72%) -- Both crates define identical constants (`MAX_AST_DEPTH: 500`, `MAX_AST_NODES: 100_000`). The `AstWalkConfig::default()` in `rskim-core` also uses these same values. If one crate's constant drifts, the bounds behavior would silently diverge. Consider exporting the constants from `rskim-core` alongside `AstWalkConfig` to establish a single source of truth.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 0 | 0 |

**Reliability Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Rationale

The code demonstrates strong reliability engineering. The `AstWalkIter` extraction is a significant improvement: it centralizes all traversal bounds guarding (`max_depth`, `max_nodes`) into a single, well-tested iterator with 15 dedicated unit tests covering zero-limit edge cases, exhaustion fuse behavior, and the `node_count` invariant. Both callers (`linearize.rs` and `ast_extract.rs`) delegate correctly to `AstWalkIter` and maintain their caller-specific invariants. The `saturating_add` pattern is used consistently for counter increments (lines 170, 173, 222-224), preventing overflow on pathological inputs. The `u32 -> u16` depth conversion uses `min(u16::MAX)` saturation rather than truncation (linearize.rs:261).

The one blocking finding (pre-allocating `level_stack`) is a minor allocation discipline improvement -- the maximum size is known at construction time and is small (2 KiB), so the fix is a one-line `with_capacity()` call. The feature knowledge bounds (MAX_FILE_SIZE=100KiB, MAX_AST_DEPTH=500, MAX_AST_NODES=100000) are correctly applied and tested. *applies ADR-001* -- surfacing the level_stack pre-allocation rather than deferring it.
