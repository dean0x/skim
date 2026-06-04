# Complexity Review Report

**Branch**: feature/187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**`AstWalkIter` struct carries 8 fields including two boolean state flags** - `ast_walk.rs:95-107`
**Confidence**: 82%
- Problem: The `AstWalkIter` struct has 8 fields (`cursor`, `level_stack`, `depth`, `node_count`, `error_count`, `config`, `done`, `first`). The `done` and `first` booleans encode a mini state machine -- `first` distinguishes the initial yield from subsequent advances, while `done` guards against post-exhaustion calls. Two boolean flags controlling iteration state increase the cognitive load of reasoning about the `next()` implementation.
- Fix: This is at the threshold (not above it) for struct field count. The boolean pair is the simplest encoding of the three-state lifecycle (first-call / iterating / done). An enum-based state machine (`enum WalkState { First, Running, Done }`) would be marginally cleaner but adds a match arm. Low-priority refactor -- the current approach is defensible given comprehensive test coverage.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Duplicated `skip_subtree` / `advance` ascent loops share identical sibling-or-ascend pattern** - `ast_walk.rs:148-164` and `ast_walk.rs:177-192` (Confidence: 65%) -- Both `skip_subtree()` and the fallback path in `advance()` execute the same loop: try sibling, else pop level_stack and go to parent. Extracting a shared `ascend_to_next_sibling()` helper would reduce the two near-identical loops to one, but the functions serve different entry contexts (skip vs. normal advance) and inlining keeps each method self-contained. Marginal improvement.

- **`LANG_MAPS` LazyLock initializer has 4 nesting levels** - `linearize.rs:109-174` (Confidence: 62%) -- The for/match/if/if nesting in the LANG_MAPS builder reaches 4 levels. This is a one-time init path executed at first access, so runtime complexity is irrelevant, but the nesting makes the init block harder to scan. A helper function `build_lang_map(lang: Language) -> Option<Vec<Option<u16>>>` could flatten the nesting by one level.

- **`walk_tree` in ast_extract.rs pre-allocates 501-element ancestors Vec** - `ast_extract.rs:136-137` (Confidence: 60%) -- `vec![None; ancestor_cap]` where `ancestor_cap = 501` allocates 501 `Option<NodeKindId>` entries upfront. Real AST depths rarely exceed 20-30 levels. A dynamically grown Vec (push on descent, truncate on ascent) would use less memory, but the fixed allocation is ~4 KiB and avoids per-node bounds checks. The current approach trades a small fixed allocation for simpler indexing -- a reasonable tradeoff.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Complexity Score**: 9/10
**Recommendation**: APPROVED

### Rationale

This PR is a net complexity **reduction**. The core architectural change -- extracting duplicated DFS traversal logic from two callers (`linearize.rs` and `ast_extract.rs`) into a shared `AstWalkIter` in `rskim-core` -- eliminates the most complex code pattern in the prior implementation: hand-rolled `TreeCursor` loops with manual depth tracking, level stacks, and bounds guards duplicated in two locations.

**Before**: Both `linearize_tree` (~80 lines, cyclomatic complexity ~10) and `walk_tree` in ast_extract (~90 lines, complexity ~12) each contained a `loop {}` with nested inner loops for cursor management, bounds checking, and sibling/ascent navigation. The ast_extract version additionally carried a `WalkContext` struct with 6 mutable reference fields threading state through the traversal.

**After**: `linearize_tree` is 32 lines with complexity ~3 (a single `for` loop with one `if`/`continue`). The ast_extract `walk_tree` is ~73 lines with complexity ~6 (a `for` loop with error-skip, vocabulary lookup, and ancestor resolution -- all linear). The shared `AstWalkIter::next()` has complexity ~6, concentrated in one well-tested location instead of duplicated across two.

The `AstWalkIter` encapsulates all DFS mechanics (cursor movement, depth tracking, bounds guards, level stack) behind the standard `Iterator` trait, which aligns with the feature knowledge anti-pattern: "reimplementing DFS outside AstWalkIter." All loops in the iterator are bounded (by `level_stack` exhaustion and `max_nodes` caps). No function exceeds 40 lines of logic. No nesting exceeds 4 levels. All bounds guards use explicit constants with documented rationale.

The single MEDIUM finding (8 struct fields) is at the warning threshold, not above it, and is well-justified by the domain requirements of an iterative tree traversal. All three suggestions are in the 60-65% confidence range and represent marginal improvements over already-clean code.

Applies ADR-001 -- surfacing all findings including marginal ones for visibility rather than silently deferring. Avoids PF-002 -- no findings classified as pre-existing to skip resolution.
