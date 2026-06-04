# Performance Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31

## Issues in Your Changes (BLOCKING)

### HIGH

**Sequential corpus extraction prevents parallelism for file-level AST parsing** - `crates/rskim-research/src/ast_extract.rs:205-309`
**Confidence**: 85%
- Problem: `extract_ast_ngrams_from_corpus` processes all files sequentially in a single thread because it shares a mutable `&mut NodeKindVocabulary` across all files. The corpus (44 repos, 14 languages, potentially tens of thousands of files) cannot leverage rayon for the most CPU-intensive phase (tree-sitter parsing + AST walking) because the vocabulary requires exclusive mutable access. The repo-cloning phase uses rayon (`fetch_files_parallel`), but the actual AST extraction -- which is the compute-heavy part -- is entirely single-threaded.
- Impact: For a 44-repo corpus with potentially 10,000+ files, the AST extraction phase becomes the bottleneck. Tree-sitter parsing is CPU-bound and embarrassingly parallel per-file; only the vocabulary insertion needs synchronization.
- Fix: Use per-thread local vocabularies (one `NodeKindVocabulary` per rayon task), then merge them after the parallel pass. Each thread collects its own bigram/trigram `HashSet` per file and its own local vocabulary. After the parallel phase, merge all local vocabularies into a single global one and remap the per-file results. This is the standard map-reduce pattern for shared-nothing parallel extraction:

```rust
// Sketch: parallel extraction with per-thread vocabularies
use rayon::prelude::*;

let file_results: Vec<(AstFileResult, Vec<String>)> = lang_files
    .par_iter()
    .filter_map(|file| {
        let mut local_vocab = NodeKindVocabulary::new();
        let result = extract_ast_ngrams_from_file(
            &file.content, file.language, &mut local_vocab, collect_trigrams
        ).ok()?;
        let kinds = local_vocab.kinds().into_iter().map(str::to_string).collect();
        Some((result, kinds))
    })
    .collect();

// Merge local vocabularies into global vocab, remap bigrams/trigrams
```

**Redundant sort in `NodeKindVocabulary::kinds()` after `stabilize()`** - `crates/rskim-research/src/ast_types.rs:253-257`
**Confidence**: 82%
- Problem: The `kinds()` method unconditionally sorts the `id_to_kind` vector every call via `v.sort_unstable()`. After `stabilize()` has been called, `id_to_kind` is already in sorted alphabetical order (that is the entire purpose of `stabilize()`). The method is called from `cmd_ast_run` (line 442) to build the vocabulary for the weight table, which always happens after `stabilize()`.
- Impact: Unnecessary O(n log n) sort on an already-sorted vector. With O(100-300) node kinds this is microseconds, so the impact is LOW in absolute terms, but it violates the zero-waste principle and the sort could be avoided with a simple design change.
- Fix: After `stabilize()`, `id_to_kind` is already sorted. Either document that `kinds()` returns items in insertion order (which is sorted after stabilize), or add an `is_stabilized` flag to skip the sort:

```rust
pub fn kinds(&self) -> Vec<&str> {
    // After stabilize(), id_to_kind is already in sorted order.
    // Return directly without re-sorting.
    self.id_to_kind.iter().map(String::as_str).collect()
}
```

### MEDIUM

**SHA-256 hash computed per file for deduplication -- consider cheaper hash** - `crates/rskim-research/src/ast_extract.rs:254`
**Confidence**: 80%
- Problem: `content_hash()` uses SHA-256 to deduplicate files. SHA-256 is cryptographically secure but ~3-5x slower than non-cryptographic alternatives like xxHash or FxHash for the same data. Deduplication only needs collision resistance, not cryptographic security.
- Impact: For a large corpus with thousands of files (many 10-100 KiB each), SHA-256 adds measurable overhead. For a research tool that runs infrequently, this is acceptable but suboptimal.
- Fix: This is a pre-existing pattern (reused from `extract.rs`). Consider switching to `xxhash-rust` or `ahash` for deduplication in a future optimization pass. Not blocking since the existing lexical pipeline already uses SHA-256 and consistency is reasonable. (Applies ADR-001: noting for immediate awareness, but impact is low enough that the existing pattern is acceptable.)

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`lang_to_ident` iterates the input string twice** - `crates/rskim-research/src/ast_codegen.rs:153-193`
**Confidence**: 80%
- Problem: `lang_to_ident` first maps all characters into `mapped` (one allocation + iteration), then iterates `mapped` again to collapse consecutive underscores into `result` (second allocation + iteration). This is two passes over short strings (~10 chars) so the absolute cost is trivial, but it could be a single pass.
- Impact: Negligible -- language names are short strings and this function is called O(number_of_languages) times (14). Not a real bottleneck.
- Fix: Combine both passes into a single iterator. Not worth changing given the tiny input size -- noting for completeness.

## Pre-existing Issues (Not Blocking)

No pre-existing CRITICAL performance issues found in the reviewed files.

## Suggestions (Lower Confidence)

- **`extract_ast_ngrams_from_corpus` creates a new `Parser` per file** - `crates/rskim-research/src/ast_extract.rs:79` (Confidence: 70%) -- `Parser::new(language)` is called for every file. If parser creation has non-trivial setup cost (loading grammar), reusing a parser per-language-group could reduce overhead. However, tree-sitter parsers are lightweight to create, so this may not matter.

- **`walk_tree` recursive depth of 500 on default thread stack** - `crates/rskim-research/src/ast_extract.rs:21,132-195` (Confidence: 65%) -- The recursive `walk_tree` function allows up to 500 levels of recursion. Each frame holds a `TreeCursor`, several `Option<u16>` values, and references (~100-200 bytes). At depth 500, this is ~50-100 KiB of stack, well within the default 8 MiB thread stack. Not a real issue, but worth noting that an iterative approach would eliminate the theoretical stack concern entirely.

- **`rekey_bigram_df_map` / `rekey_trigram_df_map` allocate new HashMaps** - `crates/rskim-research/src/ast_types.rs:96-124` (Confidence: 62%) -- These functions create new HashMaps with `with_capacity(df_map.len())` and re-insert all entries. For large DF maps, this duplicates memory briefly. An in-place remap would halve peak memory, but the current approach is correct and clear.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The code has solid performance guards (MAX_AST_DEPTH, MAX_AST_NODES, MAX_FILE_SIZE, MAX_TRIGRAMS_PER_FILE) and uses appropriate data structures (packed u32/u64 bigram/trigram encoding, HashSet for per-file deduplication, binary search in generated lookup functions). The main performance concern is the sequential extraction bottleneck in `extract_ast_ngrams_from_corpus`, which prevents the compute-heavy AST parsing phase from leveraging available CPU cores. For a research tool that runs infrequently, this is acceptable but would benefit from parallelization if corpus sizes grow. The redundant sort in `kinds()` is a minor inefficiency. The `serde_json::to_string_pretty` for output and `Vec<u8>` buffer with 256 KiB pre-allocation in codegen are both well-considered choices. (Avoids PF-002: all findings surfaced regardless of blocking status.)
