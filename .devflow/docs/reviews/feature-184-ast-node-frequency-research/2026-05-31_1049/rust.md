# Rust Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**`debug_assert` used for u16 overflow guard in production-facing vocabulary** - `crates/rskim-research/src/ast_types.rs:157`
**Confidence**: 92%
- Problem: `get_or_insert` uses `debug_assert!` to guard against exceeding `u16::MAX` entries in `NodeKindVocabulary`. In release builds `debug_assert!` is a no-op, so if the corpus ever produces more than 65,535 distinct node kinds, `self.id_to_kind.len() as NodeKindId` on line 162 silently truncates and wraps to 0, causing ID collisions and corrupted bigram/trigram data. The CLAUDE.md engineering rules state `debug_assert!` is for hot-path invariants; at module boundaries `assert!` should be used. This function sits at a data-integrity boundary, not a hot inner loop.
- Fix: Replace `debug_assert!` with a checked conversion that returns an error:
```rust
pub fn get_or_insert(&mut self, kind: &str) -> anyhow::Result<NodeKindId> {
    if let Some(&id) = self.kind_to_id.get(kind) {
        return Ok(id);
    }
    let id = u16::try_from(self.id_to_kind.len())
        .map_err(|_| anyhow::anyhow!(
            "NodeKindVocabulary overflow: {} kinds exceeds u16::MAX",
            self.id_to_kind.len()
        ))?;
    self.kind_to_id.insert(kind.to_string(), id);
    self.id_to_kind.push(kind.to_string());
    Ok(id)
}
```
  Alternatively, if changing the return type is too disruptive, at minimum change `debug_assert!` to `assert!` so release builds catch the overflow. `applies ADR-001`

**Misleading doc comment: "Iterative tree walk" is actually recursive** - `crates/rskim-research/src/ast_extract.rs:108`
**Confidence**: 95%
- Problem: The doc comment says "Iterative tree walk using `TreeCursor` to avoid recursion depth limits" but `walk_tree` calls itself recursively at line 171. It *is* recursive, bounded by `MAX_AST_DEPTH = 500`. With ~10 parameters on the stack, 500 recursive frames consume significant stack space. The doc comment is factually wrong and misleading for future maintainers.
- Fix: Change the doc comment to accurately describe the approach:
```rust
/// Recursive tree walk using `TreeCursor` with a depth bound.
///
/// Depth is capped at `MAX_AST_DEPTH` and node count at `MAX_AST_NODES`
/// to prevent stack overflow on pathological inputs.
```

### MEDIUM

**Double string allocation in `get_or_insert`** - `crates/rskim-research/src/ast_types.rs:163-164`
**Confidence**: 85%
- Problem: Lines 163-164 allocate two separate `String` copies of `kind` — one for `kind_to_id` and one for `id_to_kind`. Since this is called once per unique node kind (O(hundreds) per language, O(low thousands) across all 14 languages), the performance impact is negligible, but it is unnecessary allocation where one clone would suffice.
- Fix:
```rust
let owned = kind.to_string();
self.kind_to_id.insert(owned.clone(), id);
self.id_to_kind.push(owned);
```

**`lang_to_ident` underscore collapsing only handles exactly 2 consecutive underscores** - `crates/rskim-research/src/ast_codegen.rs:161`
**Confidence**: 80%
- Problem: `.split("__").collect::<Vec<_>>().join("_")` only collapses pairs of underscores. An input like `"C++"` maps to `"C__"` which becomes `"C_"` (correct), but a hypothetical `"C+++"` would produce `"C___"` which would split into `["C", "_"]` and join as `"C__"` — still containing doubles. Current language names don't trigger this, but the function documents general behavior.
- Fix: Use a loop or regex, or simply document that it handles only the known language set:
```rust
// Collapse all runs of consecutive underscores.
let mut result = /* ... */;
while result.contains("__") {
    result = result.replace("__", "_");
}
```

**`kinds()` redundantly sorts after `stabilize()` already sorts** - `crates/rskim-research/src/ast_types.rs:237-241`
**Confidence**: 82%
- Problem: After `stabilize()` is called, `id_to_kind` is already in sorted order. `kinds()` sorts again, performing unnecessary O(n log n) work. If `kinds()` is called before `stabilize()`, the sort is needed, but the doc comment says "Sorted order matches the ID assignment after stabilize" — implying the expected usage is post-stabilize.
- Fix: Either remove the redundant sort (if `kinds()` is only called post-stabilize) or document that it handles both pre- and post-stabilize states, making the sort intentional for the pre-stabilize case.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`files.len() as u32` truncation** - `crates/rskim-research/src/ast_extract.rs:220`
**Confidence**: 83%
- Problem: `files.len() as u32` silently truncates if the corpus has more than ~4.3 billion files. While practically impossible for this use case, the pattern violates the project rule of encoding invariants in types. The same `as u32` cast pattern appears at line 128 (`MAX_AST_NODES as u32`), though that one is safe since 100,000 is a const that fits in u32.
- Fix: Use `u32::try_from(files.len()).unwrap_or(u32::MAX)` or simply leave as-is given the practical impossibility. The `MAX_AST_NODES as u32` cast is fine since the const is 100,000.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`walk_tree` has 10 parameters** - `crates/rskim-research/src/ast_extract.rs:115` (Confidence: 70%) — Consider bundling mutable state into a struct (e.g., `WalkState { bigrams, trigrams, error_count, node_count }`) to reduce the parameter count. Already suppressed with `#[allow(clippy::too_many_arguments)]`.

- **`stabilize()` clones the entire old_kinds vector** - `crates/rskim-research/src/ast_types.rs:212` (Confidence: 65%) — `sorted_kinds = old_kinds.clone()` creates a full copy. Could sort in-place and build remap from a secondary index, but vocabulary size is small enough that this is negligible.

- **Stack depth at MAX_AST_DEPTH=500** - `crates/rskim-research/src/ast_extract.rs:21` (Confidence: 65%) — With 10 parameters (pointers/references on the stack), 500 recursive frames uses roughly 400KB of stack. Default thread stack is 8MB, so it fits, but a truly iterative approach using an explicit stack would be safer for edge cases.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 3 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The code demonstrates strong Rust patterns overall: proper use of `#[must_use]`, `Result` propagation with `anyhow`, newtype-style type aliases for bit-packed IDs, thorough test coverage with encode/decode roundtrips, correct ownership patterns in vocabulary management, and well-bounded resource limits (depth, node count, file size, trigram cap). The stabilize/rekey design correctly addresses the vocabulary ID remapping bug from the prior self-review. Two HIGH issues should be addressed: the `debug_assert` should be strengthened to an `assert!` or `Result` return for production safety, and the misleading "iterative" doc comment should be corrected to "recursive." `applies ADR-001`
