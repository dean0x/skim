# Performance Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31

## Issues in Your Changes (BLOCKING)

### HIGH

**Recursive `walk_tree` with misleading doc comment -- stack depth risk on pathological ASTs** - `crates/rskim-research/src/ast_extract.rs:108-193`
**Confidence**: 85%
- Problem: The doc comment says "Iterative tree walk using `TreeCursor` to avoid recursion depth limits" but the implementation is actually recursive -- `walk_tree` calls itself at line 171 with `depth + 1`. The `MAX_AST_DEPTH` guard at 500 protects against truly runaway recursion, but 500 recursive frames each carrying 10 parameters (including two `&mut HashSet` references, `&mut NodeKindVocabulary`, etc.) will consume roughly 40-80 KiB of stack per call chain. For a default Rust thread stack of 8 MiB this is survivable but wasteful, and for rayon worker threads (which may use smaller stacks) this could be a problem if parallelism is added later. The misleading doc comment also creates a false sense of safety for future maintainers.
- Fix: At minimum, correct the doc comment from "Iterative tree walk" to "Recursive tree walk with depth guard". For a true iterative approach, use the `TreeCursor`'s built-in `goto_first_child()`/`goto_next_sibling()`/`goto_parent()` loop pattern with an explicit stack for `(parent_id, grandparent_id)` pairs:
```rust
fn walk_tree_iterative(
    cursor: &mut tree_sitter::TreeCursor,
    vocab: &mut NodeKindVocabulary,
    bigrams: &mut HashSet<AstBigram>,
    trigrams: &mut HashSet<AstTrigram>,
    collect_trigrams: bool,
    error_count: &mut u32,
    node_count: &mut u32,
) {
    // Explicit stack: (parent_id, grandparent_id)
    let mut ancestry: Vec<Option<NodeKindId>> = Vec::new();
    let mut depth = 0;
    // ... iterative cursor-based loop
}
```

**`progress.set_message(lang_name.clone())` allocates a String on every file iteration** - `crates/rskim-research/src/ast_extract.rs:249`
**Confidence**: 82%
- Problem: Inside the inner file loop, `progress.set_message(lang_name.clone())` clones the language name string on every file processed. For a 40-repo corpus with thousands of files per repo, this produces thousands of unnecessary heap allocations. The language name does not change within the inner loop -- it only changes in the outer language loop.
- Fix: Move `progress.set_message` to the outer loop (before the inner `for file in lang_files.iter()` loop):
```rust
for lang_name in sorted_languages {
    let lang_files = &by_language[&lang_name];
    progress.set_message(lang_name.clone()); // <-- move here, once per language
    // ...
    for file in lang_files.iter() {
        progress.inc(1);
        // ...
    }
}
```

### MEDIUM

**`NodeKindVocabulary::stabilize()` clones the entire kind list twice** - `crates/rskim-research/src/ast_types.rs:207-231`
**Confidence**: 83%
- Problem: `stabilize()` calls `self.id_to_kind.drain(..).collect()` into `old_kinds`, then immediately clones it with `old_kinds.clone()` into `sorted_kinds`. Then in the re-insertion loop at line 219, each kind is cloned again with `kind.clone()`. For a vocabulary of O(100-1000) node kinds this is three full copies of every string. While not impactful at the current vocabulary scale, it is needlessly wasteful.
- Fix: Avoid the extra clone by sorting in place and building the remap from index positions:
```rust
pub fn stabilize(&mut self) -> Vec<NodeKindId> {
    let mut old_kinds: Vec<String> = self.id_to_kind.drain(..).collect();
    // Record where each kind was before sorting.
    let mut indexed: Vec<(usize, String)> = old_kinds.into_iter().enumerate().collect();
    indexed.sort_unstable_by(|a, b| a.1.cmp(&b.1));

    let mut remap = vec![0u16; indexed.len()];
    self.kind_to_id.clear();
    for (new_id, (old_id, kind)) in indexed.into_iter().enumerate() {
        remap[old_id] = new_id as NodeKindId;
        self.kind_to_id.insert(kind.clone(), new_id as NodeKindId);
        self.id_to_kind.push(kind);
    }
    remap
}
```

**`NodeKindVocabulary::get_or_insert` allocates the kind string twice** - `crates/rskim-research/src/ast_types.rs:162-164`
**Confidence**: 80%
- Problem: When inserting a new kind, `kind.to_string()` is called twice -- once for the HashMap key and once for the Vec entry. Since tree-sitter node kind strings are interned (`&'static str` in practice), the two allocations are identical. While this only happens once per unique kind (O(100-1000) total), it could be reduced.
- Fix: Allocate once, clone for the second use:
```rust
let owned = kind.to_string();
self.id_to_kind.push(owned.clone());
self.kind_to_id.insert(owned, id);
```

**`extract_ast_ngrams_from_corpus` is single-threaded despite processing thousands of files** - `crates/rskim-research/src/ast_extract.rs:204-307`
**Confidence**: 80%
- Problem: The corpus extraction iterates files sequentially within each language. The `vocab: &mut NodeKindVocabulary` parameter prevents parallelization because it requires mutable access. For a 40-repo corpus with potentially tens of thousands of files, this is the bottleneck. The repo cloning phase already uses rayon (`fetch_files_parallel`), but the CPU-intensive AST extraction phase does not.
- Fix: This is a design trade-off noted in the FEATURE_KNOWLEDGE ("offline research binary, not runtime-critical"). A future improvement would be to use thread-local vocabularies per file, collect `AstFileResult` values in parallel, then merge into the shared vocabulary and DF maps in a single-threaded pass. Since this is an offline research tool, this is not blocking, but worth noting for when corpus size grows.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`files.len() as u32` truncation risk in corpus extraction** - `crates/rskim-research/src/ast_extract.rs:220`
**Confidence**: 82%
- Problem: `files.len() as u32` silently truncates if the file list exceeds 4 billion entries. While extremely unlikely for a file corpus, the same pattern is used for `total_files_seen` which feeds the progress bar (`ProgressBar::new(total_files_seen as u64)`). The real concern is `lang_total_nodes` at line 246 and line 278 -- summing `result.node_count` (u32, max 100K per file) across thousands of files. With 10,000 files * 100K nodes = 1 billion, this stays in u32 range, but with larger corpora it could wrap.
- Fix: Either use `u64` for `lang_total_nodes` and `total_node_count` or add a saturating add:
```rust
lang_total_nodes = lang_total_nodes.saturating_add(result.node_count);
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`write_ast_weight_table` builds full JSON string in memory** - `crates/rskim-research/src/main.rs:479` (Confidence: 65%) -- `serde_json::to_string_pretty` materializes the entire JSON output as a single String before writing. For very large weight tables, using `serde_json::to_writer_pretty` with a `BufWriter<File>` would stream directly to disk. At current scale this is fine.

- **SHA-256 hashing for dedup could use a faster non-cryptographic hash** - `crates/rskim-research/src/ast_extract.rs:252` (Confidence: 62%) -- `content_hash` uses SHA-256 for file deduplication. Since this is not a security context (just dedup within a local corpus), a faster hash like xxhash or FxHash on (len, prefix, suffix) would be sufficient and faster. However, SHA-256 is reused from the existing lexical bigram module, so this is a consistency trade-off.

- **`kinds()` re-sorts an already-sorted vector after `stabilize`** - `crates/rskim-research/src/ast_types.rs:237-241` (Confidence: 70%) -- `kinds()` calls `sort_unstable()` on `id_to_kind` which is already in sorted order after `stabilize()`. The sort is O(n) on pre-sorted data with `sort_unstable`, but the comment implies this is the intended behavior for cases where `stabilize` has not been called.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The code is well-designed for an offline research binary with appropriate safety limits (MAX_AST_DEPTH, MAX_AST_NODES, MAX_FILE_SIZE, MAX_TRIGRAMS_PER_FILE). The packed integer types (u32 bigram, u64 trigram) and binary-search-based codegen are excellent choices for compact storage and O(log n) runtime lookup. The main concerns are: (1) the recursive walk_tree with a misleading "iterative" doc comment -- the recursion is depth-guarded but the comment should be corrected (applies ADR-001), and (2) the per-file String allocation in the progress bar inner loop. The single-threaded extraction is acceptable for an offline tool but is the clear bottleneck for larger corpora.
