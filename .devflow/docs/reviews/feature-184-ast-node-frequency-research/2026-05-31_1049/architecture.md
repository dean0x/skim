# Architecture Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31T10:49

## Issues in Your Changes (BLOCKING)

### HIGH

**Misleading doc comment: "Iterative tree walk" is actually recursive** - `crates/rskim-research/src/ast_extract.rs:108`
**Confidence**: 95%
- Problem: The doc comment on `walk_tree` says "Iterative tree walk using `TreeCursor` to avoid recursion depth limits" but the implementation is recursive -- it calls itself at line 171. While the `MAX_AST_DEPTH` guard (500) prevents unbounded recursion, the comment creates a false safety claim. A reviewer or future maintainer reading "iterative" would expect a loop-with-stack pattern, not recursive calls limited to 500 frames. The Rust default stack is 8 MiB; 500 recursive frames with 10 mutable references each is within bounds but not "iterative."
- Fix: Correct the doc comment to accurately describe the implementation:
```rust
/// Recursive tree walk using `TreeCursor` with a bounded depth limit.
///
/// Uses `MAX_AST_DEPTH` (500) to prevent stack overflow on pathological
/// inputs. Collects parent->child bigrams and ...
```

**`get_or_insert` uses `debug_assert` for u16 overflow -- silent truncation in release builds** - `crates/rskim-research/src/ast_types.rs:157-162`
**Confidence**: 85%
- Problem: The `debug_assert!` guard on line 157 only fires in debug builds. In a release build, if the vocabulary exceeds `u16::MAX` (65,535) entries, line 162 silently truncates `self.id_to_kind.len() as NodeKindId`, wrapping to 0. This would corrupt the entire vocabulary by aliasing new kinds onto existing IDs. The comment says "O(100) node kinds per language" but the vocabulary is shared across all 14 languages and across an entire corpus -- it is not bounded by a single grammar's size. With 14 languages, combined unique node kinds could theoretically approach a few thousand, though 65K is unlikely in practice.
- Fix: Replace `debug_assert!` with a runtime check that returns a `Result` or at minimum uses `assert!` (which fires in release):
```rust
pub fn get_or_insert(&mut self, kind: &str) -> NodeKindId {
    if let Some(&id) = self.kind_to_id.get(kind) {
        return id;
    }
    assert!(
        self.id_to_kind.len() < usize::from(NodeKindId::MAX),
        "NodeKindVocabulary overflow: {} kinds exceeds u16::MAX",
        self.id_to_kind.len()
    );
    let id = self.id_to_kind.len() as NodeKindId;
    self.kind_to_id.insert(kind.to_string(), id);
    self.id_to_kind.push(kind.to_string());
    id
}
```
Alternatively, return `Result<NodeKindId, Error>` to avoid panicking in a research tool, consistent with the project's "return Result types for all fallible operations" principle. The `assert!` is the minimum acceptable fix since this is a data-integrity invariant at a module boundary (`applies ADR-001`).

### MEDIUM

**`walk_tree` accepts 10 parameters via `#[allow(clippy::too_many_arguments)]`** - `crates/rskim-research/src/ast_extract.rs:114-126`
**Confidence**: 82%
- Problem: The `walk_tree` function takes 10 parameters, requiring a clippy suppression. This is a classic "parameter object" opportunity. The function mixes traversal state (`cursor`, `depth`, `parent_id`, `grandparent_id`), accumulation targets (`bigrams`, `trigrams`, `error_count`, `node_count`), configuration (`collect_trigrams`), and shared mutable state (`vocab`). Grouping these into a struct would improve readability and make the recursive call site cleaner.
- Fix: Extract a traversal context struct:
```rust
struct AstWalkContext<'a> {
    cursor: &'a mut tree_sitter::TreeCursor<'a>,
    vocab: &'a mut NodeKindVocabulary,
    bigrams: &'a mut HashSet<AstBigram>,
    trigrams: &'a mut HashSet<AstTrigram>,
    collect_trigrams: bool,
    error_count: &'a mut u32,
    node_count: &'a mut u32,
}
```
Then `walk_tree` becomes `fn walk_tree(ctx: &mut AstWalkContext, depth: usize, parent_id: Option<NodeKindId>, grandparent_id: Option<NodeKindId>)` -- reducing from 10 to 4 parameters and removing the clippy suppression.

**`total_files_seen` cast truncation risk** - `crates/rskim-research/src/ast_extract.rs:220`
**Confidence**: 80%
- Problem: `files.len() as u32` on line 220 silently truncates if there are more than ~4 billion files. While this is practically impossible for a corpus, the pattern is inconsistent with the defensive guards applied elsewhere in this PR (e.g., the `MAX_AST_NODES` guard, the `MAX_FILE_SIZE` guard). A `u32::try_from` or a simple truncation guard would be more consistent.
- Fix:
```rust
let total_files_seen = u32::try_from(files.len()).unwrap_or(u32::MAX);
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`walk_and_load` signature change breaks encapsulation for callers** - `crates/rskim-research/src/clone.rs:287`
**Confidence**: 82%
- Problem: The existing `walk_and_load` was a private implementation detail. Adding `Option<&[&str]>` to its signature and making it `pub` exposes internal walker configuration to external callers. The two calling patterns (lexical vs AST) are cleanly separated by `walk_and_load_ast` and the default `walk_and_load(&dest, None)`, but the `pub` visibility on `walk_and_load` itself means any future caller can pass arbitrary extensions, bypassing the curated lists. This is a minor ISP concern -- the public API exposes a knob that most callers should not touch.
- Fix: Consider keeping `walk_and_load` as `pub(crate)` and only exposing `walk_and_load_ast` and the default-extensions variant publicly. This preserves the Strategy Pattern while hiding the extension-list parameter.

## Pre-existing Issues (Not Blocking)

### MEDIUM

**Duplicated `sample_table()` test helpers across modules** - `crates/rskim-research/src/ast_codegen.rs:342`, `crates/rskim-research/src/ast_validate.rs:212`
**Confidence**: 85%
- Problem: Both `ast_codegen::tests::sample_table()` and `ast_validate::tests::sample_table()` construct nearly identical `AstWeightTable` instances for testing but with slightly different data. This duplication will drift over time as the `AstWeightTable` struct evolves. In the existing lexical pipeline, `codegen::tests` has its own `sample_table()` too, so this mirrors the existing pattern -- but now there are three such helpers.
- Fix: Consider a shared `test_utils` module (or `#[cfg(test)] pub mod test_fixtures`) that provides a canonical `AstWeightTable` builder for tests. Not blocking because it matches the existing codebase pattern.

## Suggestions (Lower Confidence)

- **Code generation string building uses raw `writeln!` calls** - `crates/rskim-research/src/ast_codegen.rs:105-316` (Confidence: 65%) -- The codegen module builds Rust source via 6 functions writing to a `Vec<u8>` with `writeln!`. The existing lexical codegen uses the same pattern, so this is consistent, but a template-based approach (e.g., `quote!` or `askama`) would be more maintainable as the generated output grows. Low priority since it matches existing patterns.

- **No snapshot test for generated AST weight source** - `crates/rskim-research/src/ast_codegen.rs:399-481` (Confidence: 70%) -- The existing lexical `codegen.rs` has a `snapshot_generated_format` test (line 383) that pins the exact output format using `insta`. The AST codegen tests verify presence of key identifiers but do not snapshot the full output, making format regressions harder to catch.

- **`AstGitCloneSource` and `GitCloneSource` have near-identical `fetch_files` implementations** - `crates/rskim-research/src/clone.rs:65-97` (Confidence: 72%) -- Both struct implementations clone the repo identically and differ only in the `walk_and_load` call. A single parameterized implementation with an extension-list field would reduce duplication while preserving the `FileSource` trait contract.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Architecture Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The AST n-gram pipeline is well-architected overall. It cleanly mirrors the existing lexical pipeline's module decomposition (types, extract, idf, codegen, validate) while properly separating AST-specific concerns. The two-pass design (extract with temporary IDs, stabilize, re-key, then compute IDF) is sound and the stabilize/remap mechanism correctly solves the ID-ordering reproducibility problem. The `FileSource` trait extension with `AstGitCloneSource` follows the established Strategy Pattern. Module boundaries are clear and dependencies point in the right direction (extract -> types, idf -> types, codegen -> types).

The two HIGH findings are the misleading "iterative" doc comment (accuracy issue, not a runtime bug) and the `debug_assert` for a data-integrity invariant that should be a release-mode check. Both are straightforward fixes. The MEDIUM items (parameter count, cast truncation, API visibility) are improvement opportunities that strengthen the design's consistency with the project's defensive-coding patterns. `avoids PF-002` -- all findings are surfaced for decision, none deferred.
