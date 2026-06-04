# Complexity Review Report

**Branch**: feature/192-wave-3c-ast-sparse-ngram -> main
**PR**: #269
**Date**: 2026-06-03 12:29

## Scope

Focus: cognitive load of the main extraction loop in
`crates/rskim-search/src/ast_index/extract.rs` (gap-fill + resolve + emit + record in one
pass), the `#[allow(clippy::collapsible_if)]` suppressions, function length, and whether the
simplification commit `30f6838` improved or obscured clarity. Diff is `git diff main...HEAD`;
the new core file is the only non-test logic added. Other touched files are pre-existing
clippy fixes in `*_tests.rs` and bench wiring — no complexity concerns there.

Verdict up front: this is well-structured, readable code. The ancestor-table algorithm is
explainable to a junior dev in ~5 minutes given the module doc comment. The simplification
commit was a net improvement on density but a small regression on rationale visibility. No
blocking complexity issues. Findings are MEDIUM/LOW.

## Issues in Your Changes (BLOCKING)

None at CRITICAL or HIGH.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`#[allow(clippy::collapsible_if)]` is now stale — let-chains are stable on this toolchain (3 occurrences)** — Confidence: 88%
- `extract.rs:139`, `extract.rs:161`, `extract.rs:173`
- Problem: The crate is `edition = "2024"` on `rustc 1.96.0`, where `let_chains` is stable.
  All three suppressed `if let … { if … }` pairs can be written as a single `if let … && …`
  condition with identical semantics, removing both a nesting level and the `#[allow]`:
  - Bigram (161-169): `if let Some(p) = parent && p != 0 && node.kind_id != 0 {`
  - Trigram (173-181): `if let (Some(gp), Some(p)) = (grandparent, parent) && gp != 0 && p != 0 && node.kind_id != 0 {`
  - Gap-fill (139-146): `if let Some(p) = prev_depth && node.depth > p + 1 {`
  The original code (pre-`30f6838`) carried comments explaining *why* the split was kept
  ("Collapsing would force construction of the Option tuple unconditionally" etc.). The
  trigram justification was never actually true — `if let (Some, Some) = (a, b)` already
  evaluates the tuple, and a let-chain short-circuits the `!= 0` checks identically. Commit
  `30f6838` deleted those comments but kept the `#[allow]`s, so the suppressions are now
  unexplained. An unexplained `#[allow]` reads as "lint is wrong here" when the real reason
  is "older nesting style." This is exactly the "hiding complexity vs. justified readability"
  question from the brief: on this toolchain the answer is the allows are no longer earned.
- Fix: Collapse the three guards to let-chains and delete the three `#[allow(clippy::collapsible_if)]`
  attributes. This drops the loop's deepest nesting from 5 levels to 4 and removes 3 lint
  suppressions. applies ADR-001 (fix noticed issues rather than deferring). If the team
  prefers to keep the split for symmetry with `rskim-research/ast_extract.rs`, keep ONE
  one-line comment stating that reason — silent `#[allow]`s are the problem, not the split
  itself.

**Public DI entry point can panic / miscompute on `u16::MAX` depth (gap-fill arithmetic overflow)** — Confidence: 80%
- `extract.rs:141` (`node.depth > p + 1`), `extract.rs:142` (`usize::from(p + 1)`)
- Problem: `p` is the previous node's `u16` depth. When `p == u16::MAX` (65535), the
  expression `p + 1` overflows `u16`: debug builds panic on overflow; release builds wrap to
  `0`, turning the gap-fill check into `node.depth > 0` and then slicing `ancestors[0..d]`
  (silently nulling the entire ancestor prefix — a correctness corruption, not a panic).
  `extract_ast_ngrams_with_weights` is `pub` and accepts an arbitrary `&[LinearNode]`, and
  `LinearNode::depth` is saturated to `u16::MAX` in `linearize_tree` (`linearize.rs:271`),
  not to the 500 traversal bound. So the value is representable. In practice the production
  linearizer caps traversal depth at `AstWalkConfig::DEFAULT_MAX_DEPTH = 500`, so this is
  unreachable on the production path — but the public DI signature makes it reachable for any
  caller building synthetic input, and the brief explicitly asked to verify "max depth"
  safety. No test exercises depth near `u16::MAX` (`extract_tests.rs` gap-fill cases top out
  at small depths). Reporting honestly rather than dismissing as out-of-scope — avoids PF-002,
  avoids PF-003.
- Fix: Use a non-wrapping comparison and slice start, e.g. guard with
  `node.depth as u32 > u32::from(p) + 1` for the check and `usize::from(p) + 1` for the slice
  start (the `+1` then happens in `usize`, which cannot overflow for `u16` inputs). Or assert
  the precondition at the boundary: `debug_assert!(max_depth < u16::MAX, "depth must leave room for gap-fill arithmetic");`
  Either removes the only non-panic-safe arithmetic the simplification commit's message
  claimed to have proven safe. Add a test feeding `node(_, u16::MAX)` followed by a deeper-
  looking node to lock the behavior.

## Pre-existing Issues (Not Blocking)

None relevant to complexity. The pre-existing clippy fixes in the touched `*_tests.rs` files
reduce complexity (removing `manual_range_contains`, `field_reassign_with_default`, etc.) and
are positive.

## Suggestions (Lower Confidence)

- **Loop body bundles four concerns; extraction would lower per-iteration load** - `extract.rs:132-189` (Confidence: 62%) — The single pass does gap-fill, parent/gp resolve, bigram emit, trigram emit, and record. It is well-sectioned with banner comments and each section is short, so it reads fine today (~58 lines, 9 decision points, cyclomatic ~10). Extracting `emit_bigram`/`emit_trigram` helpers taking `(&mut map, parent, gp, kind_id, &weight_fn)` would trim the loop, but would add parameter-passing noise and is a judgment call — current form is acceptable.
- **`max_depth` single-pass scan duplicates iteration the main loop already performs** - `extract.rs:116` (Confidence: 60%) — The pre-scan for `max_depth` is a clean way to get one allocation, but it walks `nodes` twice. A single pass with a growable `ancestors` (resize-on-demand to `d+1`) trades the second scan for occasional growth. Current approach is the deliberate "one allocation" design per KNOWLEDGE.md and is fine; noting only as an alternative.

## Assessment of commit 30f6838 (the simplification)

- **Improved**: Removing the `start < end && end <= ancestors.len()` guard is correct and
  genuinely simpler. `d <= max_depth < ancestors.len()` (table is `max_depth + 1`) and
  `p + 1 < d` from the gap check together make the slice in-bounds — the dropped guard was
  dead. Resolving from `node.depth` instead of round-tripping through `(d as u16)` is clearer
  and removes a needless cast.
- **Obscured**: It deleted the comments that justified the three `#[allow(clippy::collapsible_if)]`
  while keeping the attributes (see MEDIUM finding 1), and its commit message asserts the
  remaining `p + 1` arithmetic is "panic-safe" without noting the `u16::MAX` boundary
  (MEDIUM finding 2). Net: a real readability win on the slice, a small rationale-visibility
  loss elsewhere.

## Can a junior dev understand the ancestor-table algorithm in 5 minutes?

Yes. The module-level doc (lines 1-19) and the function `# Algorithm` doc (82-97) state the
gap-fill / resolve / emit / record contract plainly, and the four banner-commented sections
map 1:1 to the doc. The depth-indexed table ("`ancestors[d]` = kind at depth d") is the kind
of invariant a junior can hold in their head. The only non-obvious part — why sentinel `0`
nodes are *recorded* but not *emitted* — is explicitly commented at lines 183-186. The
algorithm clears the 5-minute bar.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 2 | - |
| Pre-existing | - | - | 0 | 0 |

**Complexity Score**: 8
**Recommendation**: APPROVED_WITH_CONDITIONS

Conditions (both MEDIUM, both fixable in a few lines, both align with ADR-001): collapse the
three stale `collapsible_if` guards to let-chains and drop the silent `#[allow]`s; make the
gap-fill `p + 1` arithmetic non-wrapping (or add a boundary assertion + test) so the public
DI entry point cannot panic/miscompute on `u16::MAX` depth.
