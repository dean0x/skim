# Rust Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**`walk_tree` is recursive with MAX_AST_DEPTH=500 — stack overflow risk on default thread stack** - `ast_extract.rs:142`
**Confidence**: 82%
- Problem: `walk_tree` is a recursive function with `MAX_AST_DEPTH=500`. Each stack frame carries a `&mut TreeCursor`, `&mut WalkContext<'_>`, `depth: usize`, and two `Option<u16>` (roughly 48-64 bytes per frame). At 500 depth that is ~24-32 KB of stack, well within the default 8 MB main thread stack. However, `extract_ast_ngrams_from_corpus` is called from a non-parallel context today, but the `FEATURE_KNOWLEDGE` mentions "Iterative TreeCursor traversal" as the expected pattern. If this code is ever called from a `rayon` thread pool (rayon default stack size is 8 MB, same as main), 500 recursive frames is still safe, but an iterative `TreeCursor` walk using `goto_first_child/goto_next_sibling/goto_parent` would eliminate the risk entirely and match the documented pattern.
- Fix: Convert to an iterative loop using a manual stack `Vec<(Option<NodeKindId>, Option<NodeKindId>)>` to track parent/grandparent IDs as the cursor descends/ascends. The `TreeCursor` already supports stateful traversal via `goto_first_child`, `goto_next_sibling`, `goto_parent`.

### MEDIUM

**`SAFETY` comment on non-unsafe code is misleading** - `ast_types.rs:234`
**Confidence**: 88%
- Problem: The comment `// SAFETY: sorted_indices is a permutation of [0, N), so each slot is taken exactly once.` uses the `SAFETY:` convention that is reserved for `unsafe` blocks by Rust convention and clippy (`undocumented_unsafe_blocks`). This code is safe Rust — the `unwrap_or_else(|| unreachable!(...))` handles the invariant. Using `SAFETY:` here could confuse readers scanning for actual `unsafe` code.
- Fix: Change the comment to use a different prefix:
```rust
// INVARIANT: sorted_indices is a permutation of [0, N), so each slot
// is taken exactly once.  The unwrap_or_else branch is unreachable.
```

**Missing `#[must_use]` on `extract_ast_ngrams_from_corpus` return tuple** - `ast_extract.rs:307`
**Confidence**: 85%
- Problem: `extract_ast_ngrams_from_corpus` returns a 3-tuple `(BigramDfMap, TrigramDfMap, AstCorpusStats)` that is the entire point of calling it. Discarding the return value would be a silent logic error. The codebase style (per CLAUDE.md Rust rules and the `devflow:rust` skill) calls for `#[must_use]` on functions with important return values.
- Fix: Add `#[must_use]` attribute:
```rust
#[must_use]
pub fn extract_ast_ngrams_from_corpus(
```

**Missing `#[must_use]` on `run_ast_validation`** - `ast_validate.rs:42`
**Confidence**: 85%
- Problem: `run_ast_validation` returns an `AstValidationReport` that is the sole output of the function. Already correctly marked with `#[must_use]` in the code. Actually, reviewing again — it IS marked. This finding is withdrawn.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`kinds()` allocates a `Vec<&str>` unnecessarily** - `ast_types.rs:255`
**Confidence**: 80%
- Problem: `kinds()` returns `Vec<&str>` by collecting from an iterator. The only call site (`main.rs:455`) immediately maps and collects into `Vec<String>`. Returning an iterator or a slice would avoid the intermediate `Vec` allocation. Since this is called once per pipeline run (not a hot path), the impact is minimal, but it's an easy win.
- Fix: Return a slice reference instead:
```rust
pub fn kinds(&self) -> &[String] {
    &self.id_to_kind
}
```
Then at the call site:
```rust
vocabulary: vocab.kinds().iter().map(|s| s.to_string()).collect(),
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Consider `BTreeMap` for `NodeKindVocabulary::kind_to_id`** - `ast_types.rs:138` (Confidence: 60%) — The FEATURE_KNOWLEDGE mentions `BTreeMap in NodeKindVocabulary`, but `HashMap` is used. `HashMap` is faster for the hot `get_or_insert` path during extraction. `BTreeMap` would eliminate the need for the `stabilize()` method since iteration order would be deterministic. The current `HashMap + stabilize()` design is correct but has a more complex protocol.

- **Recursive `walk_tree` vs iterative alternative** - `ast_extract.rs:142` (Confidence: 70%) — The FEATURE_KNOWLEDGE says "Iterative TreeCursor traversal" is the expected pattern. The current recursive implementation works correctly and is bounded by `MAX_AST_DEPTH`, but an iterative version would be more robust against future changes to the depth limit and would align with the documented pattern.

- **`percentile` function accepts any f32 range but only guards via `debug_assert`** - `ast_validate.rs:137` (Confidence: 65%) — In release builds, `percentile(sorted, 200.0)` would silently return a value from the array (clamped by `.min(sorted.len() - 1)`). Since all call sites pass literal 50.0/90.0/99.0, this is not exploitable today, but the `debug_assert` provides no protection in release.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The code is well-structured with thorough error handling, proper `#[must_use]` annotations on most functions, comprehensive test coverage (60 AST-specific tests), defensive bounds checks (overflow guards, depth limits, node count limits), and correct use of `saturating_add` for counters. The `stabilize + rekey` protocol for vocabulary determinism is well-designed and regression-tested. The main concern is the recursive `walk_tree` which, while currently safe within bounds, deviates from the iterative `TreeCursor` pattern noted in the feature knowledge and adds unnecessary stack-depth coupling. The `SAFETY:` comment misuse is a minor readability issue worth fixing per ADR-001 (applies ADR-001).
