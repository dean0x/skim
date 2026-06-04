# Performance Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31T15:32

## Issues in Your Changes (BLOCKING)

### HIGH

**Per-language content-hash deduplication misses cross-language duplicates** - `crates/rskim-research/src/ast_extract.rs:225`
**Confidence**: 82%
- Problem: `process_language_files` creates a fresh `seen_hashes: HashSet<[u8; 32]>` per language call (line 225). If the same file appears in multiple language groups (e.g., a `.h` file classified as both C and Cpp due to extension overlap), it will be SHA-256-hashed and processed twice. With 44 repos (many polyglot), duplicate content across language boundaries wastes both hashing and AST extraction time. The SHA-256 hash is computed on every file before the dedup check -- this is correct per file but the scope is narrower than it could be.
- Fix: Hoist `seen_hashes` to the corpus level in `extract_ast_ngrams_from_corpus` and pass it down, or deduplicate `files` once before grouping by language. This also produces more accurate corpus-level `total_files` / `deduplicated_files` counts.

```rust
// In extract_ast_ngrams_from_corpus, before the language loop:
let mut global_seen_hashes: HashSet<[u8; 32]> = HashSet::new();

// Pass to process_language_files:
fn process_language_files(
    // ... existing params ...
    global_seen: &mut HashSet<[u8; 32]>,
) -> LangProcessResult {
    // Use global_seen instead of local seen_hashes
}
```

### MEDIUM

**String cloning in `stabilize` rebuilds `kind_to_id` with O(n) clones** - `crates/rskim-research/src/ast_types.rs:242-244`
**Confidence**: 80%
- Problem: After rebuilding `id_to_kind` via zero-copy moves (lines 230-240), the method clones every string back into `kind_to_id` on line 243: `self.kind_to_id.insert(kind.clone(), ...)`. With O(100) node kinds per language across 14 languages, the total is ~1,400 entries, so the absolute cost is small. However, the clone is avoidable and contradicts the careful zero-copy approach used for `id_to_kind`.
- Fix: Build `kind_to_id` from the already-known sorted indices without cloning, or accept the O(1400) small-string clones as negligible. Given the corpus is processed once offline, this is LOW-impact in practice but architecturally inconsistent with the zero-copy intent documented in the comments.

**`vocab.kinds()` allocates a `Vec<&str>` only to immediately convert to `Vec<String>`** - `crates/rskim-research/src/main.rs:455`
**Confidence**: 85%
- Problem: `vocab.kinds().into_iter().map(str::to_string).collect()` first allocates a `Vec<&str>` (in `kinds()`), then maps each element to `String`. The intermediate `Vec<&str>` allocation is unnecessary.
- Fix: Add a `kinds_owned()` method or iterate `id_to_kind` directly:
```rust
// Replace:
vocabulary: vocab.kinds().into_iter().map(str::to_string).collect(),
// With direct access to owned strings:
vocabulary: vocab.id_to_kind.iter().cloned().collect(),
// Or add a pub method that returns owned strings directly.
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`walk_and_load` performs linear extension search on every directory entry** - `crates/rskim-research/src/clone.rs:321`
**Confidence**: 80%
- Problem: The `allowed.contains(&ext.as_str())` call on line 321 is O(k) where k is the number of extensions (21 for `AST_TARGET_EXTENSIONS`). Called once per filesystem entry across 44 repos, this adds up. The `EXCLUDED_EXTENSIONS` check on line 312 is also linear (6 entries). Both are called in the inner loop of the file walker.
- Fix: Convert the extension list to a `HashSet<&str>` once before the walk loop. With k=21 this is a micro-optimization -- the real cost is filesystem I/O, not the linear scan -- so this is LOW priority but worth noting for correctness of intent.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Sequential language processing could be parallelized** - `crates/rskim-research/src/ast_extract.rs:307` (Confidence: 65%) -- The parallelism note in the doc comment acknowledges this. With 14 languages processed sequentially and a shared `NodeKindVocabulary`, per-language parallelism requires a map-reduce pattern. Since the vocabulary is O(100) entries per language (small merge), this is a viable future optimization for large corpora but correctly deferred for now.

- **`content_hash` uses SHA-256 where a cheaper hash would suffice for dedup** - `crates/rskim-research/src/ast_extract.rs:237` (Confidence: 62%) -- SHA-256 is cryptographic-grade; for content deduplication within a trusted local corpus, a faster hash (e.g., xxhash, FxHash on content) would reduce per-file overhead. However, SHA-256 is reused from the existing lexical pipeline and the per-file cost (~1us for 100KiB) is dwarfed by tree-sitter parsing time.

- **`compute_ast_bigram_weights` allocates `String` per weight entry** - `crates/rskim-research/src/ast_idf.rs:41-42` (Confidence: 70%) -- Each `AstBigramWeight` stores `parent_kind: String` and `child_kind: String`. For a serializable output structure this is reasonable, but for large tables (thousands of weights per language), using `&str` references with a lifetime tied to the vocabulary would avoid allocations. The current design trades runtime allocation for simpler ownership, which is appropriate for a one-shot offline tool.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The performance design is solid overall. The core AST walk uses bounded recursion (MAX_AST_DEPTH=500, MAX_AST_NODES=100K), packed integer encoding for bigrams/trigrams (u32/u64 instead of string pairs), and binary-search-ready sorted arrays in the generated code. File fetching is parallelized via rayon. The hard limits (MAX_FILE_SIZE=100KiB, MAX_TRIGRAMS_PER_FILE=50K) provide effective memory guards.

The primary actionable finding is the per-language dedup scope (HIGH) which can cause redundant SHA-256 hashing and AST processing for files shared across language boundaries. The intermediate Vec allocation in `kinds()` (MEDIUM) is a small ergonomic fix. The linear extension search (MEDIUM in touched code) is dominated by filesystem I/O cost but worth a HashSet conversion for consistency. applies ADR-001
