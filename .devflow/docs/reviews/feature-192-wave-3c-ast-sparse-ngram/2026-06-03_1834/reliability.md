# Reliability Review Report

**Branch**: feature/192-wave-3c-ast-sparse-ngram -> main
**PR**: #269 (Wave 3c — AST sparse n-gram extraction)
**Scope**: crates/rskim-search/src/ast_index/extract.rs (new)
**Cycle**: 2 (verifying cycle-1 fixes hold; raising only NEW issues)
**Date**: 2026-06-03

## Cycle-1 Fix Verification (all hold)

Verified against current `extract.rs`. All four reliability fixes from cycle 1 are
present and correct:

| Fix | Location | Status |
|-----|----------|--------|
| u16 depth arithmetic widened to u32 before `+1` (PF-004) | lines 159-160: `u32::from(node.depth) > u32::from(p) + 1` | HOLDS |
| 3 debug_assert! table-sizing invariants | line 129 (max_depth < 65536), line 163 (gap-fill range non-empty), line 174 (depth in table) | HOLDS |
| HashMap capacity capped | line 145: `nodes.len().min(1024)` for both maps | HOLDS |
| Depth-0/depth-1 underflow guarded via checked_sub | lines 181, 186: `node.depth.checked_sub(1/2)` | HOLDS |

## Reliability Property Audit (against ast-index KNOWLEDGE.md)

| Property | Finding |
|----------|---------|
| Bounded iteration | PASS. Three loops, all bounded: max-depth scan over `nodes` (128), gap-fill over `ancestors[fill_start..d]` ≤ 501 slots (167), main pass over `nodes` (151). No `while`, no `loop`, no recursion (function is iterative). |
| Bounded allocation | PASS. Single ancestor-table allocation sized `max_depth+1` (137); two HashMaps capped at 1024 (145-147); no per-iteration allocation in the hot loop. |
| Integer overflow safety (u16 depth) | PASS. Gap-fill widens to u32 (159-160); parent/grandparent use `checked_sub` (181/186); `max_depth+1` index documented < 65536 and debug-asserted (129). |
| Indirection depth | PASS. `Vec<Option<NodeKindId>>` is single-level; no `Box<Box>`, no pointer-to-pointer. |
| Graceful malformed-input handling | PASS. Empty input early-returns (122); gap-fill nulls orphaned ancestor slots; sentinel `kind_id == 0` suppressed at emit on both sides (191-194, 206-210); documented residual same-depth-sibling edge case is characterized by test B2. |
| Index safety | PASS by construction. `ancestors[d]` (221) and `ancestors[fill_start..d]` (167) are always in-bounds because `max_depth = max(node.depth)` and `d <= max_depth < table_len`. Parent/grandparent reads use non-panicking `.get()` (182, 187). |

## Issues in Your Changes (BLOCKING)

None.

## Issues in Code You Touched (Should Fix)

None at >=80% confidence.

## Pre-existing Issues (Not Blocking)

None. File is new.

## Suggestions (Lower Confidence)

- **`count` field increment can wrap silently in release builds** — `extract.rs:198, 214`
  (Confidence: 65%) — `entry.1 += 1` on a `u32` count. Release profile (root `Cargo.toml`)
  does not set `overflow-checks = true`, so this wraps rather than panics if it ever
  overflowed. In practice unreachable: production input is bounded at 100K nodes
  (DEFAULT_MAX_NODES), so a single edge's term frequency cannot approach `u32::MAX` (4.2B).
  The `pub` DI core is documented as caller-bounded. Structurally safe; flagged only because
  the bound is external (caller/upstream) rather than enforced at the increment site.
  A `saturating_add(1)` would make the safety self-evident with zero cost, but is optional.

- **Raw `as usize` cast breaks local consistency** — `extract.rs:152`
  (Confidence: 60%) — `let d = node.depth as usize` uses a raw cast while the rest of the
  function consistently uses `usize::from(...)` (137, 162, 182, 187) for the same lossless
  u16->usize widening. Not a reliability defect (widening is always lossless), purely a
  consistency nit; `usize::from(node.depth)` would match surrounding style.

## Note on DECISIONS_CONTEXT

PF-004 is referenced in DECISIONS_CONTEXT, FEATURE_KNOWLEDGE, and the ADR-001 cross-link,
but the live `.devflow/decisions/pitfalls.md` contains only PF-001..PF-003 (no PF-004 heading).
The PF-004 *intent* (widen u16->u32 before adding an offset in depth comparisons) is correctly
applied at lines 159-160, so the code complies regardless. Flagging the index/file drift for
maintainers — the pitfalls file may need a PF-004 entry to match the index, but this does not
affect the code under review.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Reliability Score**: 10
**Recommendation**: APPROVED

The PR description's claim — "pure, bounded, single-allocation, overflow-hardened" — is
accurate and verified. All cycle-1 reliability fixes hold. No new reliability issues at or
above the 80% reporting threshold; the two suggestions are optional polish, not defects.
