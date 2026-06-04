# Complexity Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31

## Issues in Your Changes (BLOCKING)

### HIGH

**`walk_tree` has 10 parameters (clippy lint suppressed)** - `ast_extract.rs:114-126`
**Confidence**: 92%
- Problem: `walk_tree` accepts 10 parameters (`cursor`, `vocab`, `bigrams`, `trigrams`, `collect_trigrams`, `error_count`, `node_count`, `depth`, `parent_id`, `grandparent_id`), far exceeding the recommended maximum of 5. The `#[allow(clippy::too_many_arguments)]` annotation silences the warning rather than addressing the design issue. While tree-walking functions are inherently stateful, 10 mutable references passed through recursive calls is a high cognitive burden for anyone reading or modifying this function.
- Fix: Introduce a `WalkContext` struct to bundle the traversal state:
```rust
struct WalkContext<'a> {
    cursor: &'a mut tree_sitter::TreeCursor<'a>,
    vocab: &'a mut NodeKindVocabulary,
    bigrams: &'a mut HashSet<AstBigram>,
    trigrams: &'a mut HashSet<AstTrigram>,
    collect_trigrams: bool,
    error_count: &'a mut u32,
    node_count: &'a mut u32,
}
// walk_tree(&mut ctx, depth, parent_id, grandparent_id)
```
This reduces the call-site parameter list to 4 and groups logically related state. The three remaining scalar params (`depth`, `parent_id`, `grandparent_id`) change per-call and stay as arguments.

**`walk_tree` doc comment claims "iterative" but implementation is recursive** - `ast_extract.rs:108`
**Confidence**: 95%
- Problem: The doc comment says "Iterative tree walk using `TreeCursor` to avoid recursion depth limits" but the function calls itself recursively at line 171. While the `MAX_AST_DEPTH` of 500 bounds the recursion, the doc comment is misleading. A 500-deep call stack consumes roughly 500 * (frame size of ~200+ bytes for 10 params) = 100+ KB of stack, which is safe but not "iterative."
- Fix: Correct the doc comment to reflect the actual implementation:
```rust
/// Recursive tree walk bounded by `MAX_AST_DEPTH` using `TreeCursor`.
///
/// Uses cursor-based sibling iteration to avoid allocating child node
/// vectors, with recursion bounded to MAX_AST_DEPTH levels.
```

### MEDIUM

**Structural duplication between bigram and trigram codegen functions (4 near-identical function pairs)** - Confidence: 85%
- `ast_codegen.rs:166-198` and `ast_codegen.rs:200-232` (`write_language_bigram_arrays` / `write_language_trigram_arrays`)
- `ast_codegen.rs:234-266` and `ast_codegen.rs:268-299` (`write_bigram_lookup_fn` / `write_trigram_lookup_fn`)
- `ast_idf.rs:22-60` and `ast_idf.rs:68-107` (`compute_ast_bigram_weights` / `compute_ast_trigram_weights`)
- `ast_types.rs:96-107` and `ast_types.rs:113-124` (`rekey_bigram_df_map` / `rekey_trigram_df_map`)
- Problem: These 4 function pairs follow near-identical structure, differing only in type parameters (`u32`/`u64`, `AstBigram`/`AstTrigram`, 2-field vs 3-field decode). This is a reasonable trade-off for a research tool (generics would add complexity for marginal benefit), but worth noting as it increases the maintenance surface area -- any bug fix must be applied to both halves.
- Fix: Acceptable as-is for a research crate (publish = false). If this grows further, consider a trait-based abstraction (e.g., `trait NgramKey { fn decode(&self, vocab: &NodeKindVocabulary) -> Vec<String>; }`) to unify the pairs.

**`GitCloneSource` and `AstGitCloneSource` are near-identical structs** - `clone.rs:60-98`
**Confidence**: 82%
- Problem: `AstGitCloneSource` duplicates `GitCloneSource` almost line-for-line (same `corpus_dir` field, same clone-if-missing logic, same `extract_repo_name` call). The only difference is the final call: `walk_and_load(&dest, None)` vs `walk_and_load_ast(&dest)`.
- Fix: Parameterize the existing `GitCloneSource` with the extension list instead of duplicating the struct:
```rust
pub struct GitCloneSource {
    pub corpus_dir: PathBuf,
    pub extensions: Option<&'static [&'static str]>,
}
impl FileSource for GitCloneSource {
    fn fetch_files(&self, repo: &RepoEntry) -> anyhow::Result<Vec<SourceFile>> {
        // ... same clone logic ...
        walk_and_load(&dest, self.extensions)
    }
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`main.rs` file length at 540 lines, approaching warning threshold** - `main.rs:1-540`
**Confidence**: 80%
- Problem: The file grew from ~300 lines to 540 lines with the addition of three AST subcommand handlers. While each individual function is well-sized (cmd_ast_run = 78 lines, cmd_ast_codegen = 25 lines, cmd_ast_validate = 17 lines), the main.rs file is accumulating orchestration for two distinct subsystems (lexical bigrams and AST n-grams) in a single file. At 540 lines this is at the boundary of the 300-500 warning range.
- Fix: Consider extracting the AST command handlers into a separate `ast_commands.rs` module, keeping main.rs as just the CLI dispatch.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`lang_to_ident` underscore-collapsing is fragile** - `ast_codegen.rs:153-164` (Confidence: 65%) -- The `.split("__").collect::<Vec<_>>().join("_")` approach only collapses pairs of underscores; a language name producing `___` (triple) would leave a double underscore. Unlikely with current names but worth noting.

- **`extract_ast_ngrams_from_corpus` returns a 3-tuple** - `ast_extract.rs:204-307` (Confidence: 70%) -- The return type `(BigramDfMap, TrigramDfMap, AstCorpusStats)` is a 3-element tuple that could be a named struct for clarity at call sites, but the function is only called once and the destructuring at the call site is clear.

- **`total_files_seen` narrowing cast** - `ast_extract.rs:220` (Confidence: 62%) -- `files.len() as u32` could truncate on a corpus with more than 4 billion files. Practically impossible, but inconsistent with the `debug_assert` overflow guard in `get_or_insert`.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Complexity Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The code is well-structured with good separation of concerns across 5 new modules. Functions are individually within size limits, nesting depth is controlled (max 3 levels), and all loops/recursion have explicit bounds (MAX_AST_DEPTH, MAX_AST_NODES, MAX_TRIGRAMS_PER_FILE, MAX_FILE_SIZE) -- avoids PF-002 by surfacing all findings including structural duplication rather than deferring. The main complexity concerns are: (1) `walk_tree`'s 10-parameter signature which impairs readability, and (2) structural duplication between bigram/trigram code paths which is acceptable for a research crate but should not propagate to the production `rskim-search` crate. The misleading "iterative" doc comment should be corrected before merge (applies ADR-001).
