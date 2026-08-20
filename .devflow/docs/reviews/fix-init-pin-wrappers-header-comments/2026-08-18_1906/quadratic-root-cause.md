# Minimal-mode super-linear hot path — measured root cause

**Branch**: `fix/init-pin-wrappers-header-comments`
**Date**: 2026-08-20
**Method**: `sample(1)` call-graph profiling of a real `skim --mode=minimal` run, plus
an implement-and-remeasure causation test. **Measured, not inferred.**

---

## TL;DR

There was never a "third quadratic source" in the sense of a new algorithm. All three
hot sites are the **same** root cause, which none of the prior source-reading passes
identified because it lives in tree-sitter's C code, not in skim:

> **`TSNode` carries no parent pointer. Every tree-sitter API that needs a node's parent —
> `parent()`, `next_sibling()`, `next_named_sibling()`, `prev_named_sibling()`,
> `named_child(i)` — re-derives it by walking DOWN from the tree root, linearly scanning
> children. Each such call costs O(index-of-node-within-its-parent). Called once per
> root-level node, that is O(N²).**

Fix 2 swapped `named_child(i)` (O(i) via `ts_node__child`) for `next_named_sibling()`
(O(i) via `ts_node_parent`). Both are O(i) per step. It was a **lateral move, not a fix** —
which is exactly why the measured ratio did not improve.

The only genuinely O(1)-per-step traversal API is **`TreeCursor`**, which maintains an
explicit ancestor stack.

---

## A. Profile output

Fixture: 10,000 contiguous leading `# comment` lines + one small function.
Run: `./target/debug/skim fixture_10000.py --mode=minimal`, `SKIM_CACHE_DIR` = fresh temp
dir (the parser cache masks the defect completely — cached runs report ~5 ms).

**Total: 8,841 samples @ 1 ms, single-threaded, 100% on the main thread.**

### Top self-time functions

| Samples | Function | Layer |
|--------:|----------|-------|
| 2950 | `ts_node_child_iterator_next` | tree-sitter C |
| 1868 | `length_add` | tree-sitter C |
| 1023 | `ts_node_new` | tree-sitter C |
| 819 | `ts_subtree_size` | tree-sitter C |
| 708 | `point_add` | tree-sitter C |
| 578 | `ts_subtree_padding` | tree-sitter C |
| 394 | `ts_node_child_with_descendant` | tree-sitter C |
| 238 | `point__new` | tree-sitter C |
| 130 | `ts_subtree_extra` | tree-sitter C |
| 72 | `ts_node_child_iterator_done` | tree-sitter C |
| 33 | `ts_node__next_sibling` | tree-sitter C |
| 21 | `ts_node_start_byte` | tree-sitter C |

**100% of self time is tree-sitter node navigation.** Zero self time in any skim
transform/string/range code.

### Call-graph attribution by phase

```
8841  main thread
 8841  transform_tree_with_spans
  4456  transform_minimal + 108   -> compute_header_end_byte          (50.4%)
   4454    Node::next_named_sibling -> ts_node_next_named_sibling
              -> ts_node__next_sibling -> ts_node_parent
                 -> ts_node_child_with_descendant  [O(i) scan]
  4385  transform_minimal + 204   -> collect_removable_comments       (49.6%)
   2199    is_module_header_comment -> Node::parent -> ts_node_parent  (24.9%)
   2181    is_inside_function_body  -> Node::parent -> ts_node_parent  (24.7%)
```

`4456 + 4385 = 8841` — the two phases account for **100.0%** of wall time.

---

## B. Culprits

### Culprit 1 — `compute_header_end_byte` (50.4% of wall time) — the Fix-2 regression

**`crates/rskim-core/src/transform/minimal.rs:371`**

```rust
maybe_child = child.next_named_sibling();
```

**Why it is quadratic.** `tree-sitter-0.25.10/src/node.c:251`:

```c
static inline TSNode ts_node__next_sibling(TSNode self, bool include_anonymous) {
  uint32_t target_end_byte = ts_node_end_byte(self);
  TSNode node = ts_node_parent(self);      // <-- line 254: O(i) walk from ROOT
```

`ts_node_next_named_sibling` calls `ts_node__next_sibling`, whose **first action is
`ts_node_parent(self)`** — the very API Fix 2 was trying to avoid. And `ts_node_parent`
(`node.c:547`) does not read a parent pointer; it starts at `ts_tree_root_node` and walks
down via `ts_node_child_with_descendant`, which linearly iterates children with
`ts_node_child_iterator_next` until it finds the one containing the target. For the k-th
root-level comment that is ~k iterations. Summed over N comments: **O(N²)**.

The doc comment currently at `minimal.rs:345-348` asserting *"The sibling chain is O(1) per
step"* is **factually incorrect** and is what made this invisible to source review.

### Culprit 2 — `is_module_header_comment` (24.9%)

**`crates/rskim-core/src/transform/minimal.rs:404`**

```rust
let is_root_child = node.parent().map(|p| p.parent().is_none()).unwrap_or(false);
```

Two `ts_node_parent` calls per root-level comment, each O(k). **O(N²)**.
The inline comment *"O(1) — two pointer dereferences"* is **factually incorrect**:
there are no parent pointers in `TSNode` to dereference.

### Culprit 3 — `is_inside_function_body` (24.7%)

**`crates/rskim-core/src/transform/utils.rs:43` and `:62`**

```rust
let mut current = node.parent();
...
current = parent.parent();
```

An ancestor walk built entirely from `parent()` calls. For a root-level comment the walk
terminates after one step, but that single step is O(k). **O(N²)**. This one was never
examined because it lives in `utils.rs`, outside the file under review.

---

## C. Is the tree-sitter parse itself to blame? — **No. Explicitly exonerated.**

Three independent lines of evidence:

1. **Zero parse frames in the profile.** Frame counts for `ts_parser_parse`,
   `tree_sitter::Parser`, `Parser::parse`, `parse_source`, `ts_parser`: **all 0**. The
   parse does not appear anywhere in an 8.8-second sampled run.
2. **Phase arithmetic.** `compute_header_end_byte` + `collect_removable_comments`
   = 4456 + 4385 = 8841 = the entire thread sample count. Nothing is left for parsing.
3. **Structure-mode control.** Both modes dispatch through the identical
   `transform_tree_with_spans` entry point and therefore share one identical parse.
   Structure mode is flat/linear on the same fixtures:

| N | structure | minimal (baseline) |
|--:|----------:|-------------------:|
| 500 | 7.5 ms | 31.4 ms |
| 1000 | 9.1 ms | 133.8 ms |
| 2000 | 12.5 ms | 507.3 ms |
| 4000 | 19.4 ms | 2009.1 ms |
| 8000 | 34.0 ms | 8003.6 ms |

Structure mode does the same parse and skips only the comment-classification phases.
It stays linear. **The parse is not super-linear. What structure mode skips is precisely
`compute_header_end_byte` + `collect_removable_comments` — the two `parent()`-derived phases.**

## D. Accidental O(N²) in range/output assembly? — **No. Exonerated.**

Searched the profile for `adjust_range_for_line_removal`, `remove_ranges`,
`trim_and_normalize`, `build_newline_table`: **0 frames, 0 samples.** No `Vec::insert`/
`remove` in a loop, no quadratic `String` concatenation, no `contains()` over a growing
Vec, no sort inside a loop. Fix 2's `build_newline_table` + `partition_point` work is
correct and is not a bottleneck — it is simply irrelevant to the actual defect.

---

## Recommended fixes

All three sites share one rule: **never call `parent()` / `next_sibling()` /
`next_named_sibling()` / `named_child(i)` in a loop over siblings.** Use a `TreeCursor`,
or carry the needed context down through the existing top-down recursion.

### Fix 1 — `compute_header_end_byte`: use a `TreeCursor`

```rust
// TreeCursor keeps an explicit ancestor stack, so goto_next_sibling is true O(1).
let mut cursor = root.walk();
for child in root.named_children(&mut cursor) {
    ...
    // drop `maybe_child = child.next_named_sibling();` entirely
}
```

### Fix 2 — `is_module_header_comment`: use the recursion depth already in hand

`collect_removable_comments` already threads `depth`. Root is walked at depth 0, so a
direct child of root is exactly `depth == 1`:

```rust
if depth != 1 { return false; }   // replaces the two parent() calls
```

### Fix 3 — `is_inside_function_body`: thread a boolean down the walk

The walker is already top-down; the ancestor question is answerable in O(1) per node by
carrying the answer forward instead of re-deriving it upward:

```rust
let child_in_body = in_function_body
    || is_function_scope_kind(node.kind(), language);   // body_kinds ∪ fn_kinds
```

Pass `false` at the root call. This is exactly equivalent to the ancestor walk (it
inspects the same `body_kinds`/`fn_kinds` sets over the same ancestor chain), and the
`MAX_PARENT_WALK` bound becomes redundant since `MAX_AST_DEPTH` already caps recursion.

Apply to **both** walkers — `minimal.rs::collect_removable_comments` and
`pseudo.rs::collect_noise_ranges` — since both call `is_removable_comment`.

Full patch: `<scratchpad>/recommended.patch`.

### Guard the fix against a third regression

The scaling guard test should assert the **ratio**, not an absolute millisecond budget,
and should run with a fresh `SKIM_CACHE_DIR` (a warm parser cache hides this defect
entirely — cached runs report ~5 ms regardless). Suggested: `t(2N)/t(N) < 2.5`.

---

## Measured result of applying all three fixes

Debug build, fresh cache per run, warmed binary:

| N | baseline (Fix 1+2) | all three fixes | ratio/doubling (fixed) |
|--:|-------------------:|----------------:|----------------------:|
| 1000 | 133.8 ms | 9.5 ms | — |
| 2000 | 507.3 ms | 12.7 ms | 1.34x |
| 4000 | 2009.1 ms | 18.8 ms | 1.48x |
| 8000 | 8003.6 ms | 30.4 ms | 1.62x |
| 10000 | ~12500 ms | 36.3 ms | — |

- Baseline ratio/doubling: **3.79 → 3.96 → 3.98** (alpha ≈ 2.0, quadratic).
- Fixed ratio/doubling: **1.34 → 1.48 → 1.62** (alpha ≈ 0.4–0.7 and rising toward 1.0 as
  the fixed ~8 ms process startup amortizes — linear).
- **N=8000: 8003.6 ms → 30.4 ms = 263x.** Minimal mode now matches structure mode's
  34.0 ms, as it should.

### Correctness of the proposed fix

Output hashed for **75 repo fixtures × 3 modes (minimal, pseudo, structure) + 2 synthetic
header fixtures = 231 outputs**, before vs after: **all 231 byte-for-byte identical.**

Note: the unit tests in `minimal.rs` call `is_module_header_comment` /
`is_removable_comment` directly, so their call sites need the new signatures when this
lands.

---

## Confidence

| Claim | Confidence | Basis |
|-------|-----------:|-------|
| Three culprits identified at file:line | **99%** | Directly measured — call-graph profile, 100% of samples attributed |
| Root cause is `ts_node_parent`'s walk-down-from-root | **99%** | Measured in profile + read tree-sitter `node.c:251-254`, `:547-558` |
| Fix 2's `next_named_sibling` swap was a lateral move | **99%** | Measured (no ratio change) + confirmed at `node.c:254` |
| Parse exonerated | **98%** | Measured — 0 parse frames; structure-mode control linear on identical parse |
| Range/output assembly exonerated | **97%** | Measured — 0 frames in profile |
| Proposed fix restores linear scaling | **97%** | Measured end-to-end after implementing it |
| Proposed fix is behavior-preserving | **90%** | Measured on 231 outputs; full test suite not run (see note above) |

---

## Working-tree note (not part of the perf finding)

**Fix 1 and Fix 2 are uncommitted working-tree modifications to `minimal.rs` / `pseudo.rs`.**
They are *not* in HEAD (`f00e37a`) — `git stash` on those files reverts to the original
O(N³) `prev_named_sibling()` implementation. Anything that stashes, checks out, or cleans
those paths will silently destroy both prior fixes. Worth committing before further work.

All experimental instrumentation has been reverted; the working tree is back to its
pre-analysis state (verified: baseline timings reproduce at N=1000 = 133.8 ms,
N=2000 = 507.3 ms, matching the reported 125.5 / 508.0 ms, and the crate rebuilds with
zero warnings).
