# Architecture Review Report

**Branch**: feature/192-wave-3c-ast-sparse-ngram -> main
**PR**: #269
**Date**: 2026-06-03

## Issues in Your Changes (BLOCKING)

None. The new `extract.rs` module is architecturally sound on every dimension reviewed:
separation of concerns (pure extraction, no I/O, no global state), dependency-injection
consistency, public API surface design, module-boundary alignment, and the
`(ngram, weight, count)` contract extension. No SOLID violation, no layering violation,
no tight coupling, no circular dependency introduced.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Convergent ancestor-stack / chain-break logic duplicated across crates** — Confidence: 82%
- `crates/rskim-search/src/ast_index/extract.rs:121-188`
- Sibling: `crates/rskim-research/src/ast_extract.rs:124-193` (`walk_tree`)
- Problem: Both functions implement the same conceptual algorithm — a depth-indexed
  `Vec<Option<NodeKindId>>` ancestor table, `parent = depth.checked_sub(1)`,
  `grandparent = depth.checked_sub(2)` resolution, sentinel/error chain-break by nulling
  ancestor slots, and bigram/trigram emission keyed on `encode(parent, child)` /
  `encode(gp, parent, child)`. The parent/grandparent resolution blocks
  (`extract.rs:149-157` vs `ast_extract.rs:163-168`) and the record step are near-identical.
  The feature's own KNOWLEDGE.md anti-pattern warns: *"Reimplementing DFS cursor logic in a
  new consumer... Adding a parallel implementation creates a divergence risk."* The chain-break
  semantics are the load-bearing correctness property here, and they now live in two places.
- Mitigating context (why this is MEDIUM, not HIGH): the two functions consume **genuinely
  different inputs** and that difference is not incidental. `walk_tree` walks a live
  `tree_sitter::Tree` via `AstWalkIter` and nulls `ancestors[depth]` inline because the ERROR
  node IS visited (`is_error` branch). `extract_ast_ngrams_with_weights` replays an
  already-linearized `&[LinearNode]` in which ERROR nodes were *already dropped* by
  `linearize_tree`, so it must *reconstruct* the chain-break from a depth-jump (`node.depth >
  prev_depth + 1`). This is the documented "residual gap-fill edge case." So the search-side
  version legitimately cannot reuse `walk_tree` as-is — it operates one layer downstream. The
  duplication is in the *emission + ancestor-resolution* mechanics, not the traversal.
- Fix (lowest-risk, no behavior change required now): extract the shared emission core into a
  small helper in `rskim-core` alongside `AstWalkIter`, e.g.
  `AncestorContext { table: Vec<Option<NodeKindId>> }` exposing `resolve_parent(depth)`,
  `resolve_grandparent(depth)`, `record(depth, id)`, and `break_chain(depth)` /
  `null_range(from, to)`. Both `walk_tree` and `extract_ast_ngrams_with_weights` would then
  own only their input-specific framing (live-tree null-on-error vs flat-sequence gap-fill) and
  share the resolution/record primitive. This removes the divergence risk on the correctness-
  critical resolution math while preserving the legitimate input-shape difference. If deferring,
  add a cross-referencing comment in each function pointing at the other so a future edit to one
  prompts review of the other (applies ADR-001 — surface, do not silently defer).

## Pre-existing Issues (Not Blocking)

**`rskim-research::walk_tree` grows its ancestor table on demand; `extract.rs` pre-sizes once** — Confidence: 80%
- `crates/rskim-research/src/ast_extract.rs:137,148-150`
- Problem: The research version starts at capacity 64 and `resize`s on demand
  (`ancestors.resize(depth + 1, None)`), explicitly trading the single-allocation guarantee for
  lower per-file memory in corpus extraction. The new `extract.rs` does a single up-front
  `max_depth` scan and one allocation. Neither is wrong — they optimize for different workloads
  (one-shot per-file vs. corpus batch) — but the two now embody **opposite allocation
  strategies for the same data structure**, which is exactly the kind of silent divergence that
  the consolidation in the MEDIUM finding above would resolve. Flagged for visibility per ADR-001
  / PF-002 (not hand-waved as out-of-scope); informational because it is in unchanged
  research-crate code and is a deliberate, documented trade-off, not a defect.
- Fix: fold into the shared-primitive consolidation above, or leave as-is with a comment noting
  the intentional strategy difference.

## Suggestions (Lower Confidence)

- **`HashMap` capacity pre-sized to `nodes.len()`** - `crates/rskim-search/src/ast_index/extract.rs:127-128` (Confidence: 65%) — bigram/trigram maps are pre-sized to the full node count, but unique-ngram cardinality is typically far smaller than node count (structural edges repeat heavily). Mild over-allocation; not an architecture defect, and the comment already labels it "a reasonable upper bound." Acceptable as-is.

## Notes on Verified Non-Issues

- **Gap-fill slice panic-safety** (`extract.rs:142`, `ancestors[usize::from(p + 1)..d]`): verified
  safe directly from invariants (PF-003 — verified, not assumed). `d = node.depth <= max_depth`
  and `ancestors.len() == max_depth + 1`, so `d < len` (upper bound safe). The branch is entered
  only when `node.depth > p + 1`, i.e. `p + 1 < d` (start < end). `p <= max_depth <= 500`, so
  `p + 1` cannot overflow `u16`. The commit-30f6838 simplification that dropped the explicit
  `start < end && end <= len` guard is correct.
- **DI separation**: `extract_ast_ngrams_with_weights` (pure, closure-injected weights) +
  `extract_ast_ngrams` (production wrapper binding `ast_bigram_idf`/`ast_trigram_idf`) is a clean
  DIP-compliant split (Fowler-style injection of the weight strategy). It mirrors the documented
  convention and is matched by tests: synthetic `unit_*_weight` for the core, end-to-end Rust
  fixtures for the wrapper.
- **Public API surface**: `AstNgramSet` / `AstBigramEntry` / `AstTrigramEntry` re-exports
  (mod.rs:50-53, lib.rs:36-38) follow existing naming (`Ast*` prefix, `*Entry` suffix), derive a
  consistent trait set (`Debug, Clone, Copy, PartialEq`; `Default` on the set), and structurally
  mirror each other (bigram/trigram symmetry). Consistent with `AstBigram`/`AstTrigram`.
- **Module boundary**: `extract` correctly sits one layer above `linearize` (consumes its
  `&[LinearNode]`) and below the future on-disk index (#194) / query covering-set (#197) layers,
  delegating all weight lookup to `ngram`/`ast_weights`. Dependencies point inward only.
- **`(ngram, weight, count)` contract extension**: extending issue #192's literal `(ngram, f32)`
  to carry `count` (term frequency) is justified (avoids a second pass for downstream TF-IDF/BM25),
  documented in both the module header and KNOWLEDGE.md, applied consistently to both entry types,
  and the `count` semantics (TF, not DF) are explicitly disambiguated. Sound forward-design.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 1 | 0 |

**Architecture Score**: 9/10
**Recommendation**: APPROVED

The single MEDIUM finding is a maintainability/divergence-risk concern, not a correctness or
layering defect, and the duplicated logic spans a legitimate input-shape boundary. Per ADR-001
and PF-002 it is surfaced (not deferred silently) for an explicit fix-or-acknowledge decision,
but it does not block merge of this pure-additive change.
