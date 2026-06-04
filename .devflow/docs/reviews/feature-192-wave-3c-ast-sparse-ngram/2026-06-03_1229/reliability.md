# Reliability Review Report

**Branch**: feature/192-wave-3c-ast-sparse-ngram -> main
**PR**: #269
**Date**: 2026-06-03 12:29
**Focus**: reliability (panic-safety, bounded loops/allocation, assertion density, indirection)
**Scope reviewed**: `crates/rskim-search/src/ast_index/extract.rs` (NEW), gap-fill commit 30f6838, bench/test deltas

---

## Verdict on the Critical Question (gap-fill slice panic-safety)

The brief asked me to prove or disprove panic-safety of `ancestors[usize::from(p + 1)..d]`
(line 142) after commit 30f6838 removed the `start < end && end <= len` guard.

**Result: the slice itself is panic-safe for all inputs the PRODUCTION path can produce, but
there is a real, narrow integer-overflow defect at `p == u16::MAX` that is reachable through
the PUBLIC dependency-injection entry point. It is a logic/overflow bug, not a slice
out-of-bounds bug.**

### Slice-bounds proof (the part that IS safe)

Controlling invariants:
- `LinearNode.depth: u16` (linearize.rs:82).
- `max_depth = nodes.iter().map(|n| n.depth).max()` (extract.rs:116).
- `ancestors.len() == usize::from(max_depth) + 1` (extract.rs:121).

1. **Upper bound `..d` is in range.** For every node, `node.depth <= max_depth`, therefore
   `d = node.depth as usize <= max_depth = len - 1 < len`. The slice end `d` and the final
   write `ancestors[d]` (line 187) are both always in bounds. The KNOWLEDGE.md claim "d is
   always within ancestors" holds.
2. **Lower bound never exceeds upper bound.** The slice is reached only inside the guard
   `node.depth > p + 1` (line 141). Hence `d = node.depth > p + 1 = start`, so `start < d`
   strictly — the inverted/empty-panic case `start > d` is impossible. The commit message's
   claim "`p+1 < d` holds from the gap check" is correct.

I validated both bounds empirically by replaying the exact index logic over adversarial depth
sequences (normal chain, large jumps, non-monotone `[0,3,1,9]`, max-depth `[0,65535]`,
`[65534,65535]`) — no slice panic in any case. The guard removal in 30f6838 is therefore
**safe for the slice indices specifically**, given depth is bounded.

### The defect the proof does NOT cover (integer overflow at p == u16::MAX)

Both line 141 (`node.depth > p + 1`) and line 142 (`usize::from(p + 1)`) compute `p + 1` in
**u16 arithmetic**. When `p == u16::MAX (65535)`:
- **Debug build**: `p + 1` panics with `attempt to add with overflow` — before the slice is
  ever indexed. The slice-bounds proof is irrelevant because the panic happens one expression
  earlier, inside the guard condition itself.
- **Release build** (this workspace has NO `overflow-checks` in `[profile.release]`, Cargo.toml:82):
  `p + 1` wraps to `0`. The guard `node.depth > 0` is then true, so gap-fill executes with
  `start = 0`, spuriously nulling `ancestors[0..d]` — silent logic corruption, no panic.

**Reachability:** `p` is a previous node's `depth`. Reaching `p == 65535` requires a prior
`LinearNode { depth: 65535 }`. The production wrapper `extract_ast_ngrams` cannot produce this:
`linearize_source` bounds traversal depth to `DEFAULT_MAX_DEPTH = 500` (saturated to u16, KNOWLEDGE.md
lines 107-108, 246). **However**, `extract_ast_ngrams_with_weights` is `pub` and re-exported from
the crate root (lib.rs), and `LinearNode { pub kind_id, pub depth }` has public fields. An external
caller (or a future internal caller that does not route through `linearize_source`) can hand-build
`&[LinearNode { depth: 65535, .. }]` and trigger the overflow.

**Reproducing input** (public API):
```rust
use rskim_search::{extract_ast_ngrams_with_weights, LinearNode};
let nodes = [
    LinearNode { kind_id: 1, depth: 65535 },
    LinearNode { kind_id: 2, depth: 65535 }, // p == 65535 on 2nd node → p + 1 overflows
];
extract_ast_ngrams_with_weights(&nodes, |_| 1.0, |_| 1.0);
// debug: panics "attempt to add with overflow"
// release: silently wraps, nulls ancestors[0..65535]
```

This is the kind of latent, build-profile-dependent defect the reliability lens exists to catch:
the removed guard previously masked it incidentally (`end <= ancestors.len()` would have been the
backstop, though `p+1` still overflowed in the guard before reaching it — so even the old code was
not fully safe at `p == u16::MAX`). The honest framing (avoids PF-003, avoids PF-002): the slice is
safe, the arithmetic feeding it is not, and neither version of the code was safe at the u16 boundary.

---

## Issues in Your Changes (BLOCKING)

### HIGH

**Unchecked u16 overflow in gap-fill arithmetic at `p == u16::MAX`** — `extract.rs:141-142`
**Confidence**: 90%
- Problem: `p + 1` (u16) in the guard `node.depth > p + 1` and in `usize::from(p + 1)` overflows
  when a previous node has `depth == 65535`. Debug → panic; release (no `overflow-checks`) →
  wraps to 0 and corrupts the ancestor table via a spurious `ancestors[0..d]` null. Reachable
  via the public `extract_ast_ngrams_with_weights` + public `LinearNode.depth` field. The
  production `extract_ast_ngrams` path is safe only because `linearize_source` caps depth at 500 —
  a caller contract, not a local guarantee.
- Fix: perform the comparison in a wider type to make it total, e.g.
  ```rust
  if u32::from(node.depth) > u32::from(p) + 1 {
      for slot in &mut ancestors[usize::from(p) + 1..d] {
          *slot = None;
      }
  }
  ```
  `usize::from(p) + 1` cannot overflow (usize >= u16) and remains `< d <= len`. This removes the
  build-profile-dependent behavior split entirely.

**Documented invariants are not enforced in production code (zero assertion density)** —
`extract.rs:116-187`
**Confidence**: 85%
- Problem: KNOWLEDGE.md and commit 30f6838's message assert several load-bearing invariants
  ("d is always within ancestors", "p+1 < d holds from the gap check", "max_depth scan sizes the
  table"). None are expressed in code. There is no `debug_assert!` anywhere in the function. This
  violates the user's reliability rule (assert preconditions/invariants in production code, not
  just tests) and the project Rust rule (`debug_assert!` for invariants in hot paths). The function
  silently relies on the depth-bounding caller contract with no boundary check. applies ADR-001.
- Fix: encode the invariants the proof depends on, so a future caller that violates the depth
  bound fails loudly in debug/test rather than corrupting output:
  ```rust
  let max_depth = nodes.iter().map(|n| n.depth).max().unwrap_or(0);
  // Boundary precondition: the DI core assumes linearize-style depth bounds.
  debug_assert!(max_depth <= 500, "ancestor depth {max_depth} exceeds linearize bound");
  // ... in the loop, after computing d:
  debug_assert!(d < ancestors.len(), "depth index {d} out of ancestor table");
  // ... inside the gap branch:
  debug_assert!(usize::from(p) + 1 < d, "gap-fill slice start !< end");
  ```

---

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Unbounded up-front allocation driven by caller-controlled length (DI boundary contract not
documented)** — `extract.rs:121, 127-128`
**Confidence**: 82%
- Problem: `ancestors` is `vec![None; usize::from(max_depth) + 1]` and both maps are
  `HashMap::with_capacity(nodes.len())`. Through production `extract_ast_ngrams` these are bounded
  (`max_depth <= 500`, `nodes.len() <= MAX_AST_NODES = 100K`). Through the public DI entry both
  bounds are entirely caller-controlled: a caller passing 65535-depth nodes forces a ~128 KB
  ancestor `Vec`; a caller passing a 50M-element slice forces two 50M-capacity HashMaps allocated
  before any work. The user's allocation-discipline rule asks whether the bound is local or relies
  on a caller contract — here it relies on an undocumented caller contract at the public boundary.
- Fix: this is acceptable for a pure DI core, but the contract should be explicit. Add a doc note
  on `extract_ast_ngrams_with_weights` stating that allocation is `O(max_depth)` + `O(nodes.len())`
  and that callers are responsible for bounding `nodes` (production callers get this via
  `linearize_source`). The `debug_assert!(max_depth <= 500, ...)` above also documents the intended
  bound in code. No runtime cap needed given the production path is already bounded.

---

## Pre-existing Issues (Not Blocking)

None identified in the changed surface. The traversal/bounds primitives (`AstWalkIter`,
`AstWalkConfig` depth/node caps) live in `rskim-core` and were not touched by this PR; they
correctly bound the production path and were reviewed transitively only as the source of the
depth invariant. Per PF-002, I am not parking any in-scope finding here.

---

## Items Verified Safe (no finding)

- **`checked_sub(1)` / `checked_sub(2)` for parent/grandparent** (extract.rs:149-157): correct.
  Depth 0 → `None` parent; depth 1 → `None` grandparent. Uses `ancestors.get(..)` (bounds-safe)
  even though the index is provably in range. Robust against the same overflow class — no issue.
- **Final write `ancestors[d] = Some(...)`** (extract.rs:187): `d <= max_depth < len`. Always safe.
- **Bounded loops**: the single `for node in nodes` loop and the inner gap-fill `for slot in ..`
  are both bounded by finite slice lengths. No unbounded iteration. Compliant.
- **Indirection depth**: `Vec<Option<NodeKindId>>` and `HashMap<_, (f32, u32)>` — single level,
  no `Box<Box<>>` / pointer-to-pointer. Compliant.
- **Sort/collect**: `sort_unstable_by_key` on bounded vecs. No reliability concern.
- **Bench `unwrap()`** (linearize_bench.rs:164): test/bench code, acceptable.

---

## Suggestions (Lower Confidence)

- **Consider asserting `prev_depth` monotonic-or-jump expectations** — `extract.rs:140` (Confidence: 65%)
  — gap-fill assumes pre-order; a non-pre-order caller (depth decreasing then jumping) produces
  defined-but-meaningless output. A debug-only assert that input resembles a pre-order walk would
  catch misuse of the DI core, but is lower priority than the overflow fix.

---

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | - | - |
| Should Fix | - | - | 1 | - |
| Pre-existing | - | - | 0 | 0 |

**Reliability Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The core slice-safety claim in commit 30f6838 is correct and well-reasoned — the gap-fill slice
does not go out of bounds. But the simplification left a build-profile-dependent u16 overflow at
the `p == u16::MAX` boundary that is reachable through the public DI API, and the function asserts
none of the invariants its safety depends on in production code. Both are squarely in the changed
lines and should be fixed before merge (widen the arithmetic to u32; add `debug_assert!` guards for
the depth and slice invariants). Neither blocks the production `extract_ast_ngrams` path, which is
protected by `linearize_source`'s depth cap — hence HIGH, not CRITICAL.
