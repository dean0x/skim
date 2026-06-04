# Security Review Report

**Branch**: feature/192-wave-3c-ast-sparse-ngram -> main (PR #269)
**Date**: 2026-06-03
**Focus**: security (integer overflow/underflow, slice-index panics, unbounded allocation, untrusted-input resource exhaustion)
**Diff command**: `git diff main...HEAD`

## Scope

The only file in this PR with a runtime security surface is
`crates/rskim-search/src/ast_index/extract.rs` (new, 236 lines). It parses
linearized CST node sequences (`&[LinearNode]`) — input ultimately derived from
arbitrary source files, and additionally constructible directly by any caller
because `LinearNode { kind_id, depth }` has **public fields** and the extract
functions are **public crate exports** (re-exported from the crate root per the
ast-index KNOWLEDGE.md public API table).

All other changed files are test-only clippy fixes (`*_tests.rs`), `mod.rs`/
`lib.rs` re-exports, and a Criterion bench group. No security surface there.

## Issues in Your Changes (BLOCKING)

### HIGH

**u16 overflow in gap-fill depth arithmetic (`p + 1`)** — `crates/rskim-search/src/ast_index/extract.rs:141`
**Confidence**: 88%

- Problem: In the gap-fill block, `prev_depth` is read as `p: u16` and the code
  computes `node.depth > p + 1`. When a node has `depth == u16::MAX (65535)`,
  the next iteration evaluates `65535 + 1`, which overflows `u16`.
  - **Debug / `cargo test` builds** (overflow checks on): this **panics**
    (`attempt to add with overflow`) — a denial-of-service path on crafted input.
  - **Release builds**: `[profile.release]` in `Cargo.toml` does NOT set
    `overflow-checks = true`, so the add **silently wraps to 0**. The condition
    then becomes `node.depth > 0` and line 142 nulls `ancestors[0..d]` — the
    entire ancestor chain is spuriously cleared, corrupting all downstream
    parent/grandparent resolution for that file (silent wrong output).
- Reachability: A caller can construct `&[LinearNode { kind_id: 1, depth: 65535 }, LinearNode { kind_id: 1, depth: 0 }]`
  and pass it to the public `extract_ast_ngrams` / `extract_ast_ngrams_with_weights`.
  The function is documented as a standalone public entry point taking
  `&[LinearNode]`, and the review brief explicitly designates this input as
  untrusted. The production path through `linearize_source` saturates and bounds
  depth at `MAX_AST_DEPTH = 500`, so the overflow is **not reachable via that
  path today** — which is why this is HIGH rather than CRITICAL — but the public
  API makes no such guarantee and the existing tests already construct
  `LinearNode`s with arbitrary depths via public fields.
- Fix: Use saturating arithmetic so the comparison cannot overflow, and make the
  intent explicit:

  ```rust
  // line 141
  if node.depth > p.saturating_add(1) {
      // p.saturating_add(1) caps at u16::MAX; range start (p+1) is then
      // guaranteed <= node.depth == d, keeping the slice range valid.
      for slot in &mut ancestors[usize::from(p.saturating_add(1))..d] {
          *slot = None;
      }
  }
  ```

  With `saturating_add`, when `p == u16::MAX` the guard becomes
  `node.depth > u16::MAX` which is always false, correctly skipping gap-fill
  (there is no depth above the max to gap-fill into). This removes both the
  debug panic and the release-mode chain-corruption. Consider also adding
  `overflow-checks = true` to `[profile.release]` as defense-in-depth so future
  arithmetic mistakes fail loud rather than corrupting silently (aligns with the
  project's "fail loud" philosophy in CLAUDE.md). Add a regression test with a
  `depth: u16::MAX` node (the F-series in `extract_tests.rs` has no boundary case
  for this). applies ADR-001; avoids PF-002 (reported despite the production
  path being currently safe rather than dismissed as unreachable).

## Issues in Code You Touched (Should Fix)

_None beyond the HIGH finding above._

## Pre-existing Issues (Not Blocking)

_None. The slice index at line 142 (`ancestors[usize::from(p + 1)..d]`), the
ancestor write at line 187 (`ancestors[d] = ...`), and the allocation at line
121 (`vec![None; usize::from(max_depth) + 1]`) were all checked: the slice range
is valid in the normal flow because gap-fill is only entered when `d > p + 1`;
the write at `d` is in range because `d <= max_depth` (the vec is sized to
`max_depth + 1`); and the allocation is bounded because `max_depth` is `u16`
(worst case ~65,536 `Option<u16>` slots ≈ 200 KB). The `entry.1 += 1` count
increments (lines 167, 179) are `u32` and bounded in practice by the input slice
length. These are safe and are noted only to record that they were reviewed._

## Suggestions (Lower Confidence)

- **`count` field `u32` increment has no explicit saturation** - `crates/rskim-search/src/ast_index/extract.rs:167,179` (Confidence: 35%) — `entry.1 += 1` could in principle overflow with a slice of >4 billion identical edges. This requires ~32 GB of input and is bounded to 100K by `MAX_AST_NODES` on the production path, so it is not realistically exploitable; `saturating_add` would be a cheap hardening if desired.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Security Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The single HIGH finding is a `u16` overflow in the gap-fill depth comparison that
panics under crafted input in debug/test builds and silently corrupts output in
release builds. It is not reachable through the current `linearize_source`
production path (depth bounded at 500), but the function is public API accepting
untrusted `&[LinearNode]`, and per ADR-001 / PF-002 it should be fixed now with
`saturating_add` plus a `u16::MAX` regression test rather than deferred on
"currently unreachable" grounds.
