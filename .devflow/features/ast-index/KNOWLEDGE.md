---
feature: ast-index
name: AST Index (CST Linearization + N-gram Encoding + On-Disk Store)
description: "Use when implementing AST-based n-gram extraction, building or reading the on-disk structural index, adding a new language to the structural index, debugging depth or node-count truncation, extending the shared vocabulary, working with AstBigram/AstTrigram IDF weights, extracting structural n-grams from linearized nodes, or using the shared AstWalkIter traversal primitive. Keywords: linearize, CST, AST, n-gram, bigram, trigram, NodeKindId, AstBigram, AstTrigram, AstNgramSet, AstBigramEntry, AstTrigramEntry, NODE_KIND_VOCABULARY, LANG_MAPS, LinearNode, AstWalkIter, AstWalkConfig, tree-sitter, depth-encoded, pre-order, IDF, ast_bigram_idf, ast_trigram_idf, extract_ast_ngrams, extract_ast_ngrams_with_weights, store, AstIndexBuilder, AstIndexReader, AstPosting, AstFileMetaEntry, skidx, skpost, SKAX, on-disk index, mmap, posting list, build_from_files, lookup_bigram, lookup_trigram."
category: architecture
directories: [crates/rskim-search/src/ast_index/, crates/rskim-core/src/]
referencedFiles:
  - crates/rskim-core/src/ast_walk.rs
  - crates/rskim-core/src/lib.rs
  - crates/rskim-search/src/ast_index/linearize.rs
  - crates/rskim-search/src/ast_index/ngram.rs
  - crates/rskim-search/src/ast_index/extract.rs
  - crates/rskim-search/src/ast_index/mod.rs
  - crates/rskim-search/src/ast_index/store/format.rs
  - crates/rskim-search/src/ast_index/store/builder.rs
  - crates/rskim-search/src/ast_index/store/reader.rs
  - crates/rskim-search/src/ast_index/store/mod.rs
  - crates/rskim-search/src/ast_weights.rs
  - crates/rskim-search/src/lib.rs
  - crates/rskim-search/benches/ast_index_bench.rs
created: 2026-06-01
updated: 2026-06-05
---

# AST Index (CST Linearization + N-gram Encoding + On-Disk Store)

## Overview

The `ast_index` module converts tree-sitter Concrete Syntax Trees (CSTs) into a
compact, flat representation suitable for downstream n-gram extraction and IDF-weighted
structural search. It has four sub-modules:

- **`linearize`** — converts source text into `Vec<LinearNode>`, a pre-order
  depth-first sequence where each node carries a vocabulary ID and traversal depth.
- **`ngram`** — provides `AstBigram` and `AstTrigram` newtypes for packing
  node-kind ID pairs/triples into compact integer keys, plus vocabulary helpers
  and IDF weight lookup backed by the per-language weight tables in `ast_weights`.
- **`extract`** — (Wave 3c) consumes a `Vec<LinearNode>` and produces a deduplicated,
  weighted `AstNgramSet` of structural bigrams and trigrams. This is the document-side
  extraction step; query-side covering-set (#197) is deferred to Wave 3f.
- **`store`** — (Wave 3d) persists an `AstNgramSet` corpus as a two-file mmap'd
  on-disk inverted index, mirroring the lexical `index/` module. The structural
  query engine (Wave 3f #197) is still out of scope.

The design is intentionally minimal: `linearize_source` is the only stateful-setup
entry point (it triggers `LANG_MAPS` initialization on first call). All n-gram
encoding, weight lookup, and extraction are pure (no I/O, no mutable state).

The DFS traversal logic was extracted into `rskim-core::AstWalkIter` so it can be
shared with `rskim-research` without duplicating cursor management, bounds guarding,
or depth tracking.

## System Context

`ast_index` sits between the tree-sitter grammar layer (`rskim-core::Parser`) and
whatever consumes structural n-grams (the `lexical` / `index` layers in `rskim-search`
and future AST search layers). It depends on:

- `rskim-core::Language` and `rskim-core::Parser` for grammar dispatch
- `rskim-core::AstWalkIter` and `rskim-core::AstWalkConfig` for shared DFS traversal
- `crate::ast_weights::NODE_KIND_VOCABULARY` — the shared, auto-generated vocabulary
  of 1740 node kind strings sorted alphabetically for binary search
- `crate::ast_weights::{ast_bigram_weight, ast_trigram_weight}` — per-language
  IDF weight tables keyed by the same packed integer encoding as `AstBigram`/`AstTrigram`
- `crate::types::SearchError::Ast` for the one error path that is not gracefully silenced
- `crate::index::lang_map::{lang_to_id, lang_from_id}` — single source of truth for
  language ↔ u8 ID mapping. `index/mod.rs` was widened to `pub(crate) mod lang_map` so
  the `store/` sub-module can reuse the same mapping without duplication.

Non-tree-sitter languages (JSON, YAML, TOML) have no entry in `LANG_MAPS`. Calling
`linearize_source` for these languages returns the empty default result without an
error. `ast_bigram_idf` and `ast_trigram_idf` return `DEFAULT_AST_WEIGHT` for them.
`extract_ast_ngrams` also returns an empty `AstNgramSet` because it delegates weight
lookup through the same fallback path.

## Component Architecture

### AstWalkIter (rskim-core)

The shared traversal primitive. Lives in `crates/rskim-core/src/ast_walk.rs`.

`AstWalkIter` encapsulates:
- `TreeCursor`-based iterative pre-order DFS
- `level_stack: Vec<u32>` — depth restoration on ascent (internal, not visible to callers)
- Bounds guards: `max_depth` and `max_nodes` in `AstWalkConfig`
- Error/missing node detection: `AstWalkNode::is_error`
- Post-exhaustion stats: `node_count()` and `error_count()`
- `FusedIterator` impl — once exhausted it always returns `None`

`AstWalkConfig` exposes its defaults as associated constants so all consumers share
one canonical source:

```rust
// AstWalkConfig::DEFAULT_MAX_DEPTH and DEFAULT_MAX_NODES are the canonical values.
// linearize.rs aliases them as test-only pub(crate) constants:
#[cfg(test)]
pub(crate) const MAX_AST_DEPTH: u32 = AstWalkConfig::DEFAULT_MAX_DEPTH;  // 500
#[cfg(test)]
pub(crate) const MAX_AST_NODES: u32 = AstWalkConfig::DEFAULT_MAX_NODES;  // 100_000
```

Takeaways: update limits in one place (`ast_walk.rs`) and they propagate to `linearize.rs`,
`rskim-research/ast_extract.rs`, and any future consumer automatically.

### LinearNode

The unit of linearization output. Two fields: `kind_id: u16` (vocabulary index) and
`depth: u16` (0-indexed from tree root). Being `Copy` makes it cheap in `Vec` and
by-value pass. The sentinel `kind_id == 0` maps to `""` at index 0 of
`NODE_KIND_VOCABULARY`, used for grammar kinds not found in the shared vocabulary.

`AstWalkNode::depth` is `u32`; `linearize_tree` saturates it to `u16` before storing
in `LinearNode` (the max_depth bound of 500 makes overflow impossible in practice).

Parent–child relationships are recoverable from depth alone: the parent of node at
index `i` with depth `d` is the nearest preceding node with depth `d - 1`.

### AstBigram and AstTrigram (ngram.rs)

Compact newtypes for packing AST node-kind ID pairs/triples into integer keys.

```
// Bigram:  (u32::from(parent) << 16) | u32::from(child)
// Trigram: (u64::from(gp) << 32) | (u64::from(parent) << 16) | u64::from(child)
```

These encodings match the keys stored in `ast_weights::RUST_AST_BIGRAM_WEIGHTS` (and
the other per-language tables). The `ast_bigram_idf` and `ast_trigram_idf` functions
look up weights using these packed keys, falling back to `DEFAULT_AST_WEIGHT` (1.0)
when the bigram/trigram is not found or the language has no table.

The `Display` impl resolves IDs through the vocabulary for human-readable output:
`"function_item > block"`. Sentinel ID 0 displays as `"<unknown>"`; out-of-bounds
IDs display as `"?{id}"`.

### LANG_MAPS (LazyLock)

A `HashMap<Language, Vec<Option<u16>>>` initialized once at first use. For each
of the 14 tree-sitter languages, a `Vec` is built indexed by tree-sitter's own
`kind_id` (grammar-local, non-portable). Each slot holds the index into
`NODE_KIND_VOCABULARY` if the kind string is in the vocabulary, or `None`.

The vocabulary lookup is O(1) during traversal (array index) at the cost of one
binary search per kind string at initialization.

### linearize_tree (private)

Delegates the DFS loop entirely to `AstWalkIter`. Caller-specific logic — vocabulary
lookup via `LANG_MAPS` and `LinearNode` construction — stays here.

### extract sub-module (extract.rs)

Converts a `&[LinearNode]` into an `AstNgramSet` by replaying the pre-order sequence
through a depth-indexed ancestor stack. This is the document-side extraction layer;
it has no I/O and no global state.

**Key types:**
- `AstNgramSet { bigrams: Vec<AstBigramEntry>, trigrams: Vec<AstTrigramEntry> }` — the
  output; both vecs are sorted by packed key ascending, contain unique keys.
- `AstBigramEntry { ngram: AstBigram, weight: f32, count: u32 }` — one structural
  parent→child edge with its IDF weight and term frequency (emitted occurrences).
- `AstTrigramEntry { ngram: AstTrigram, weight: f32, count: u32 }` — one structural
  grandparent→parent→child triple with its IDF weight and term frequency.

The `count` field records term frequency (how many times the structural edge appeared
in the file). It extends beyond the `(ngram, f32)` contract from issue #192 to
future-proof BM25-style scoring without a separate pass.

**Ancestor stack algorithm:**

The function maintains `Vec<Option<NodeKindId>>` sized to `max_depth + 1` (one
allocation, no per-iteration growth). For each node in pre-order traversal order:

1. **Gap-fill**: if `node.depth > prev_depth + 1`, the skipped ancestor slots are
   nulled. A depth jump greater than +1 in pre-order means an ERROR/MISSING node
   was dropped during linearization; nulling breaks the spurious parent–child chain.
2. **Resolve**: `parent = ancestors[depth - 1]`, `grandparent = ancestors[depth - 2]`.
3. **Emit bigram**: when `parent` is `Some(p)` AND `p != 0` AND `node.kind_id != 0`.
4. **Emit trigram**: when both `grandparent` and `parent` are `Some` AND all three
   kind IDs are non-zero.
5. **Record**: `ancestors[depth] = Some(node.kind_id)` — sentinel nodes ARE recorded
   to preserve correct depth positions for deeper descendants.

**Two entry points:**

```rust
// Dependency-injected core — testable with synthetic weights
pub fn extract_ast_ngrams_with_weights(
    nodes: &[LinearNode],
    bigram_weight: impl Fn(AstBigram) -> f32,
    trigram_weight: impl Fn(AstTrigram) -> f32,
) -> AstNgramSet { ... }

// Production wrapper — uses ast_bigram_idf / ast_trigram_idf
pub fn extract_ast_ngrams(nodes: &[LinearNode], lang: Language) -> AstNgramSet {
    extract_ast_ngrams_with_weights(nodes, |b| ast_bigram_idf(lang, b), |t| ast_trigram_idf(lang, t))
}
```

The split follows the project's dependency-injection convention: pure core is covered
by unit tests with synthetic weights; `extract_ast_ngrams` is covered by end-to-end
tests against real grammars and production weight tables.

**Residual documented divergence (gap-fill edge case):**

A dropped ERROR node that had a same-depth preceding sibling leaves no gap in depth
values, so the orphaned child binds to that sibling as its parent — a spurious edge.
This is confined to malformed code regions. The spurious edge's packed key almost
always misses the selective weight table (receiving the 1.0 default weight) and does
not structurally corrupt the output for syntactically valid regions. This behavior is
intentionally characterized by test B2 in `extract_tests.rs` so any future silent
change will cause that test to fail.

**u16 depth arithmetic — widen before adding offset (PF-004):**

Gap-fill uses `u32::from(node.depth) > u32::from(prev_depth) + 1` rather than
`node.depth > prev_depth + 1`. The widening is load-bearing: u16 addition wraps at
65535, so `p + 1` when `p == u16::MAX` silently evaluates to 0, bypassing gap-fill
and producing a panic or spurious edge. Test B1 locks this regression.

**HashMap capacity cap:**

`bigram_map` and `trigram_map` are pre-allocated with `nodes.len().min(1024)` slots.
Capping at 1024 avoids a large `nodes.len()` (up to 100K) driving unnecessary
allocation — unique structural edges are typically an order of magnitude smaller than
total node count because most edges repeat within a file.

### store sub-module (Wave 3d, PR #272)

Persists an `AstNgramSet` corpus as a two-file mmap'd on-disk inverted index,
mirroring the lexical `index/` module. Library-only; the structural query engine
(Wave 3f #197) is still out of scope.

#### On-Disk Format

Two files are written to `output_dir`:

- **`ast_index.skidx`** — fixed-size header + sorted bigram/trigram lookup tables + per-file metadata.
- **`ast_index.skpost`** — concatenated posting lists (all bigram lists, then all trigram lists).

Magic `b"SKAX"`, version `1`. Distinct from lexical `b"SKIX"` / `index.*` so both
coexist in the same `.skim/` directory without collision.

**Layout of `ast_index.skidx`:**

| Section | Size | Details |
|---|---|---|
| `AstSkidxHeader` | 48 B | Magic, version, counts, averages, CRC32 |
| `AstBigramEntry` × bigram_count | 16 B each | u32 key, u64 offset, u32 length |
| `AstTrigramEntry` × trigram_count | 20 B each | u64 key, u64 offset, u32 length |
| `AstFileMetaEntry` × file_count | 5 B each | u8 lang_id + u32 node_count |

**Posting entry (`AstPostingEntry`):** 8 B — `doc_id: u32` + `count: u32`. Postings are
uncompressed. `count` is the per-file structural term-frequency from `AstBigramEntry.count`
/ `AstTrigramEntry.count`; the `weight` field is discarded at build time (IDF is
recomputed at query time in Wave 3f via `ast_bigram_idf`, with language recovered via
`AstFileMetaEntry.lang_id` → `lang_from_id`).

**CRC32:** covers `idx_mmap[48..expected_idx_size]` as one contiguous slice —
bigram entries + trigram entries + file-meta entries in serialization order. This
matches the order they appear on disk, so no copies are needed during verification.

#### AstIndexBuilder API

Core merge primitive is `add_file_ngrams(id, lang, set, node_count)`. The convenience
`add_file(id, content, lang)` calls `linearize_source` + `extract_ast_ngrams`
internally. `build_from_files(output_dir, files)` rayon-parallelizes the pure extract
step and merges sequentially — measured at ~12.8 ms for 1000 files, meeting the
`<10 s` acceptance target with margin.

**Atomic write contract:** `ast_index.skpost` is written first, then `ast_index.skidx`
(commit point). A reader that finds `.skidx` present can assume `.skpost` is coherent.
Uses `NamedTempFile::new_in` + `write_all` + `sync_all` + `persist` — matching the
cochange sibling pattern; stronger than the lexical index (which uses simple rename).

**FileId invariant (PRECONDITION):** FileIds must be dense, sequential, starting from
zero. Every file — even files yielding zero n-grams (non-tree-sitter languages,
`>100 KiB`, empty content) — must receive exactly one `add_file_ngrams` call and
advances `file_count` by 1. The builder enforces this: duplicate FileId →
`SearchError::InvalidQuery` (message: "duplicate"); non-sequential FileId →
`SearchError::InvalidQuery` (message: "sequential"). The shared file manifest is
owned by the CLI / Wave 4 layer, not this library.

**`node_count` narrowing:** `add_file` uses `u32::try_from(lin.nodes.len())` — NOT
`as u32` — producing `SearchError::IndexCorrupted` on overflow (applies PF-004 analog:
no silent narrowing).

**Re-index concurrency:** NOT concurrency-safe; same posture as the lexical index.
Callers must serialize re-index operations against concurrent reads.

#### AstIndexReader API

`open(dir)` validates magic, version, file sizes, and CRC32 before returning. The
`idx_mmap` is always present; `post_mmap` is `None` when `postings_file_size == 0`
(prevents mmap-ing a zero-length file, which fails on some platforms).

All size validation and posting bounds use `checked_*` arithmetic.

Public accessors: `file_count()`, `avg_node_count()`, `avg_bigram_count()`,
`avg_trigram_count()`, `file_meta(file_index)`.

Lookup methods return `Vec<AstPosting>` (each posting: `doc_id: u32, count: u32`).

#### Reader API Contract (C1–C7)

These contracts are what Wave 3f's query engine relies on:

| Contract | Guarantee |
|---|---|
| C1 | Postings sorted ascending by `doc_id`, at most one per `doc_id` |
| C2 | Absent key → `Ok(vec![])` (no error) |
| C3 | Malformed entry (bad offset/len, OOB, `len % 8 != 0`) → `Err(IndexCorrupted)` |
| C4 | Every `count >= 1` (`decode_posting` rejects `count == 0`) |
| C5 | `count` is structural TF, enables BM25 scoring |
| C6 | `file_meta(i)` recovers `lang_id` → call `lang_from_id` to get `Language` |
| C7 | `AstIndexReader: Send + Sync` (compile-time verified by test A6) |

#### Cross-Index FileId Contract

The AST index and the lexical index must be built over the identical, identically-ordered
file set. The shared file manifest is owned by the CLI / Wave 4 layer; neither index
builder owns it. `AstIndexBuilder` and `NgramIndexBuilder` both enforce the sequential-
FileId invariant independently. Building them out of sync produces subtly mismatched
results at query time with no runtime error.

## Component Interactions

```
linearize_source(&str, Language)
    │
    ├── Guard: source.len() > size_limit (100 KiB general; 1 MiB for SQL)
    │       → Ok(default)
    ├── Guard: language not in LANG_MAPS  → Ok(default)
    │
    ├── Parser::new(language)             → Err → SearchError::Ast
    ├── parser.parse(source)              → Err → Ok(default)
    │
    └── linearize_tree(&Tree, &[Option<u16>])
            │
            └── AstWalkIter::new(tree.walk(), AstWalkConfig::default())
                    │  [max_depth=500, max_nodes=100_000]
                    ├── Yields AstWalkNode { node, depth: u32, is_error }
                    └── Per item: is_error → skip emit
                                 Normal   → LANG_MAPS lookup → LinearNode

extract_ast_ngrams(&[LinearNode], Language)
    │
    └── extract_ast_ngrams_with_weights(nodes, ast_bigram_idf(lang,·), ast_trigram_idf(lang,·))
            │
            ├── max_depth scan → allocate ancestors Vec (single allocation)
            ├── For each node: gap-fill → resolve parent/gp → emit → record
            └── Collect bigram_map + trigram_map → sort by key → AstNgramSet

AstIndexBuilder::build_from_files(output_dir, files)
    │
    ├── rayon par_iter → linearize_source + extract_ast_ngrams per file (parallel)
    ├── Sequential merge: add_file_ngrams(id, lang, set, node_count) per file
    └── build()
            ├── Sort posting lists by doc_id
            ├── Sort keys ascending (bigram u32, trigram u64)
            ├── Serialize postings + entries + file_meta + header
            ├── CRC32 over post-header payload
            ├── Atomic write: skpost first, then skidx (commit point)
            └── AstIndexReader::open(output_dir)

AstIndexReader::lookup_bigram(AstBigram)
    │
    ├── Binary search bigram entries in idx_mmap[48..bigram_end]
    ├── Found entry → fetch offset/length into post_mmap
    ├── Validate: OOB, alignment (len % 8 == 0)
    └── Decode AstPostingEntry × n → Vec<AstPosting>

ast_bigram_idf(Language, AstBigram) → f32
    └── ast_bigram_weight(lang.name(), bigram.key())
            └── binary search in per-language weight table
                fallback: DEFAULT_AST_WEIGHT (1.0)
```

## Constraints and Bounds

| Constant | Value | Source | Purpose |
|---|---|---|---|
| `MAX_FILE_SIZE` | 100 KiB | `linearize.rs` | Most languages: oversized files return empty |
| `MAX_FILE_SIZE_LARGE` | 1 MiB | `linearize.rs` | SQL only: matches `rskim-research` |
| `DEFAULT_MAX_DEPTH` | 500 | `AstWalkConfig` | Pathological nesting stops descent |
| `DEFAULT_MAX_NODES` | 100,000 | `AstWalkConfig` | Caps output Vec allocation |
| `AST_HEADER_SIZE` | 48 B | `store/format.rs` | Fixed header size |
| `AST_BIGRAM_ENTRY_SIZE` | 16 B | `store/format.rs` | u32 key + u64 offset + u32 length |
| `AST_TRIGRAM_ENTRY_SIZE` | 20 B | `store/format.rs` | u64 key + u64 offset + u32 length |
| `AST_POSTING_ENTRY_SIZE` | 8 B | `store/format.rs` | u32 doc_id + u32 count |
| `AST_FILE_META_SIZE` | 5 B | `store/format.rs` | u8 lang_id + u32 node_count |

SQL uses `MAX_FILE_SIZE_LARGE` because migrations and schema dumps routinely exceed
100 KiB. All other languages use `MAX_FILE_SIZE`. The branch is a `match` on
`Language::Sql` at the top of `linearize_source`.

When a depth or node-count guard triggers, the traversal moves to the next sibling
or ascends — it does not abort entirely. The subtree is skipped; traversal continues.

## Node Count Invariant

`result.node_count == result.nodes.len() + result.error_count`

This invariant holds at every exit point of `linearize_tree`. `node_count` is the
total nodes visited (including ERROR/MISSING) from `iter.node_count()`.
`error_count` comes from `iter.error_count()`. Tests assert this invariant on every
result.

## Vocabulary

`NODE_KIND_VOCABULARY` in `ast_weights.rs` is auto-generated by `rskim-research
ast-codegen`. It is a sorted `&[&str]` of 1740 node kind strings shared across all
14 languages. It must not be edited manually — regenerate with the codegen tool.

The vocabulary being sorted is a load-bearing property: `LANG_MAPS` initialization
uses `binary_search` on it, and `vocab_lookup` (exposed from `ngram.rs`) also uses
`binary_search`. Breaking the sort order silently corrupts all vocabulary lookups.
The test `vocabulary_is_sorted` guards this invariant.

## Public API Surface

The module's public exports (via `mod.rs` and re-exported from `rskim_search`) are:

| Symbol | Type | Purpose |
|---|---|---|
| `linearize_source` | `fn` | Linearize source to `LinearizeResult` |
| `LinearNode` | `struct` | Single node in linearized sequence |
| `LinearizeResult` | `struct` | Output of linearization |
| `NodeKindId` | `type alias` | `u16` index into vocabulary |
| `AstBigram` | `struct` | Packed parent→child pair (u32) |
| `AstTrigram` | `struct` | Packed gp→parent→child triple (u64) |
| `DEFAULT_AST_WEIGHT` | `const f32` | Fallback IDF weight (1.0) |
| `vocab_lookup` | `fn` | Kind string → `NodeKindId` (binary search) |
| `vocab_resolve` | `fn` | `NodeKindId` → kind string |
| `vocab_len` | `fn` | Total vocabulary entries |
| `ast_bigram_idf` | `fn` | IDF weight for a bigram by language |
| `ast_trigram_idf` | `fn` | IDF weight for a trigram by language |
| `AstNgramSet` | `struct` | Output of extraction: bigrams + trigrams vecs |
| `AstBigramEntry` | `struct` | One bigram with weight and count |
| `AstTrigramEntry` | `struct` | One trigram with weight and count |
| `extract_ast_ngrams` | `fn` | Production extraction (uses real IDF tables) |
| `extract_ast_ngrams_with_weights` | `fn` | DI core: caller supplies weight closures |
| `AstIndexBuilder` | `struct` | Builds the two-file on-disk index |
| `AstIndexReader` | `struct` | mmap'd read-only query layer |
| `AstPosting` | `struct` | One decoded posting: `doc_id` + `count` |
| `AstFileMetaEntry` | `struct` | Per-file `lang_id` + `node_count` (public) |

## Anti-Patterns

- **Calling `linearize_source` in a tight per-file loop without accounting for
  `LazyLock` initialization**: the first call for each language triggers grammar
  loading and `kind_id` table construction. Warm subsequent calls are fast; cold
  first calls per process are heavier. Benchmarks exclude warm-up from timing.

- **Treating `kind_id == 0` as a signal that a node is unimportant**: sentinel 0
  means the grammar kind is not in the shared vocabulary — it is still a real node.
  Consumers that weight by vocabulary index must handle `kind_id == 0` explicitly.
  In `extract.rs` the sentinel is recorded in the ancestor table to preserve depth
  positions, but suppressed at emit time; this is correct and intentional.

- **Adding non-tree-sitter languages to the `LANG_MAPS` init list**: JSON, YAML,
  and TOML have no tree-sitter grammar in this crate. `Parser::new` returns `Err`
  for them; the `continue` in the init loop silently drops them. This is correct
  behavior. These languages return the empty default from `linearize_source` and
  `DEFAULT_AST_WEIGHT` from `ast_bigram_idf`/`ast_trigram_idf`.

- **Holding a `LinearizeResult` across a rebuild of `LANG_MAPS`**: the `kind_id`
  values in a result are only meaningful relative to the `NODE_KIND_VOCABULARY`
  version used when the result was produced. If the vocabulary is regenerated
  (changing indices), cached results become stale.

- **Reimplementing DFS cursor logic in a new consumer**: use `AstWalkIter` from
  `rskim-core` instead. The `level_stack`, bounds guards, and `is_error` detection
  are all handled there. Adding a parallel implementation creates a divergence risk.

- **Using `AstBigram::from_raw` / `AstTrigram::from_raw` in external callers**:
  these are `pub(crate)` and intended for internal iteration over stored weight
  tables. External callers must use `encode()` to guarantee correct encoding.

- **Treating `count` in `AstBigramEntry`/`AstTrigramEntry` as document frequency**:
  `count` is term frequency (occurrences in one file), not the number of documents
  containing the n-gram. Document frequency is a corpus-level metric computed at
  index build time, not here.

- **Skipping `add_file_ngrams` for files that yield zero n-grams**: every file in
  the manifest must produce exactly one call even if its `AstNgramSet` is empty.
  Omitting it causes `file_count` to diverge from the lexical index's file count,
  producing mismatched results at query time.

- **Building the AST and lexical indexes from different file orderings**: both
  indexes enforce sequential FileId starting from 0, but they check independently.
  Building them over different file sets is a logic error with no runtime trap.

- **Using `as u32` for `node_count` narrowing**: always use `u32::try_from(lin.nodes.len())`
  so overflow surfaces as `IndexCorrupted` rather than a silent truncation (applies PF-004).

## Gotchas

- **`level_stack` is internal to `AstWalkIter`**: any depth-related bug fix must
  be made in `crates/rskim-core/src/ast_walk.rs`, not in `linearize.rs`.

- **`MAX_AST_DEPTH` / `MAX_AST_NODES` in `linearize.rs` are test-only aliases**:
  they are `#[cfg(test)] pub(crate)` and alias `AstWalkConfig::DEFAULT_MAX_DEPTH/NODES`.
  They exist only so test assertions can reference them by name. Production code
  gets these limits via `AstWalkConfig::default()` inside `linearize_tree`.

- **`AstWalkNode::depth` is `u32`; `LinearNode::depth` is `u16`**: `linearize_tree`
  saturates via `.min(u32::from(u16::MAX)) as u16`. Marked with
  `#[allow(clippy::cast_possible_truncation)]` as documentation of intent.

- **tree-sitter `kind_id` is grammar-local, not vocabulary-local**: `node.kind_id()`
  returns a u16 valid only within one grammar. It is NOT safe to compare `kind_id`
  values across languages. The `LANG_MAPS` indirection exists precisely to map from
  grammar-local IDs to the shared vocabulary.

- **Vocabulary index 0 is the empty sentinel, not a skip signal**: `NODE_KIND_VOCABULARY[0]`
  is `""`. A node that maps to sentinel 0 is still emitted in `result.nodes` by
  `linearize_source`. In `extract.rs` it is additionally recorded in the ancestor table
  but silently skipped at emit time to prevent corrupt edges.

- **SQL file size limit is 1 MiB, not 100 KiB**: the `match language { Sql => ..., _ => ... }`
  branch at the top of `linearize_source` is easy to miss when debugging why a large
  SQL file produces results while a large Rust file returns empty.

- **`ast_bigram_idf` and `ast_trigram_idf` return `DEFAULT_AST_WEIGHT` for unknown
  bigrams and for all non-tree-sitter languages**: callers that want to detect
  "not found" vs. "found with weight 1.0" must check the weight table directly via
  `ast_weights::ast_bigram_weight`.

- **`MAX_FILE_SIZE` is checked against `source.len()` (bytes), not chars**: for
  ASCII-heavy source this is fine. UTF-8 multibyte identifiers shrink effective
  character count slightly below the byte limit, which is acceptable and consistent
  with `rskim-research/src/ast_extract.rs`.

- **Gap-fill in `extract.rs` checks `node.depth > prev_depth + 1`**: the comparison
  uses `u16` arithmetic — it will not wrap at depth 0 thanks to `checked_sub` in the
  parent-resolution step, but the gap-fill check itself is only reached when
  `prev_depth` is `Some`, so there is no underflow risk.

- **Ancestor stack is NOT re-used between files**: each `extract_ast_ngrams*` call
  allocates a fresh `Vec<Option<NodeKindId>>`. For batch extraction over many files,
  the allocation overhead is proportional to `max_depth`, which is bounded at 500.

- **Index size ratio is ~1.23× source, NOT < 5%**: the original #194 acceptance criterion
  "index < 5% of source" is structurally unachievable for structural AST n-grams. The
  tiny vocabulary (~161 distinct bi/trigrams for Rust) means each key appears in nearly
  every file, producing dense posting lists that scale O(vocab × files). The regression
  guard is `< 3.0×` (measured ~1.23×). Industry uncompressed trigram indexes run 3–5×.
  Actual compression (delta + VarInt / Roaring posting) is tracked in issue #273.

- **`post_mmap` is `None` for an empty corpus**: `AstIndexReader::open` does not mmap
  a zero-length `.skpost` file. `lookup_bigram`/`lookup_trigram` return `Ok(vec![])`
  in this case — callers must not assume `None` means "not found" at the API level.

- **`lang_map` visibility was widened to `pub(crate)`**: `index/mod.rs` now declares
  `pub(crate) mod lang_map` so `store/format.rs` can reuse `lang_to_id`/`lang_from_id`
  without a separate mapping. Do not add a second mapping table elsewhere.

## Key Files

- `crates/rskim-core/src/ast_walk.rs` — `AstWalkIter`, `AstWalkConfig` (with `DEFAULT_MAX_DEPTH`/`DEFAULT_MAX_NODES`), `AstWalkNode`; shared DFS primitive and canonical limit source
- `crates/rskim-search/src/ast_index/linearize.rs` — `LANG_MAPS`, `linearize_source`, `linearize_tree`; SQL size override; delegates DFS to `AstWalkIter`
- `crates/rskim-search/src/ast_index/ngram.rs` — `AstBigram`, `AstTrigram`, `NodeKindId`, vocabulary helpers, IDF weight lookups
- `crates/rskim-search/src/ast_index/extract.rs` — `extract_ast_ngrams`, `extract_ast_ngrams_with_weights`, `AstNgramSet`, `AstBigramEntry`, `AstTrigramEntry`; depth-indexed ancestor stack; document-side extraction only
- `crates/rskim-search/src/ast_index/store/format.rs` — pure binary codec: all on-disk struct definitions, encode/decode functions, binary search helpers, CRC32; no I/O
- `crates/rskim-search/src/ast_index/store/builder.rs` — `AstIndexBuilder`: merge primitive, parallel `build_from_files`, atomic write, FileId enforcement
- `crates/rskim-search/src/ast_index/store/reader.rs` — `AstIndexReader`, `AstPosting`: mmap open/validate, `lookup_bigram`, `lookup_trigram`, `file_meta`, `Send + Sync`
- `crates/rskim-search/src/ast_index/store/mod.rs` — re-exports `AstIndexBuilder`, `AstIndexReader`, `AstPosting`, `AstFileMetaEntry`
- `crates/rskim-search/src/ast_index/mod.rs` — public re-exports for all four sub-modules
- `crates/rskim-search/src/ast_index/linearize_tests.rs` — 8 test cycles: types, vocabulary, traversal, error nodes, bounds, multi-language, edge cases, performance
- `crates/rskim-search/src/ast_index/ngram_tests.rs` — 14 test groups: encode/decode roundtrips, key formula, Display, vocab helpers, IDF weight lookup
- `crates/rskim-search/src/ast_index/extract_tests.rs` — 26 test cases: empty input, chain, siblings, dedup/count, gap-fill, sentinel suppression (parent + grandparent), end-to-end, sort/uniqueness, determinism, input immutability, injected weights, unknown-weight default, crate-root re-exports, large-input smoke test; batch-2 regression tests: B1 u16::MAX depth overflow guard (×2), B2 documented spurious same-depth-sibling edge (×1), B3 trigram count accumulation (×1), B4 gap-fill at max-depth boundary (×2), B5 depth-0 underflow via checked_sub (×2). Helper `bigram_keys(set)` deduplicates bigram key extraction across tests.
- `crates/rskim-search/src/ast_index/store/builder_tests.rs` — tests A7–A14 + build_from_files + parallel tests; FileId guards, zero-ngram file meta, avg stats, duplicate/non-sequential FileId errors
- `crates/rskim-search/src/ast_index/store/reader_tests.rs` — tests A1–A6, C1–C7 contracts; `ast_index_size_ratio` normal test (not `#[ignore]`) asserting `< 3.0×`
- `crates/rskim-search/src/ast_index/store/format_tests.rs` — encode/decode roundtrips and boundary checks for all on-disk structs
- `crates/rskim-search/src/ast_weights.rs` — auto-generated `NODE_KIND_VOCABULARY` (1740 entries, sorted) and per-language bigram/trigram weight tables; do not edit manually
- `crates/rskim-search/benches/ast_index_bench.rs` — Criterion benchmark `build_1000_rust_files` (A15: `< 10 s`); measured ~12.8 ms. A16 size ratio is a normal unit test in `reader_tests.rs`.

## Related

- ADR-001: Fix all noticed issues immediately regardless of scope — applies when finding invariant violations or guard logic gaps during traversal or extraction changes. PR #269 applied this (Wave 3c); PR #272 applied it again (Wave 3d).
- PF-004: u16 depth arithmetic overflow — always widen to u32 before adding an offset in depth comparisons. See gap-fill note above. The store builder applies the analogous rule: `u32::try_from` for node_count narrowing, no silent `as u32`.
- Feature: `cochange` — consumes `FileId`-keyed data built from git history; the store builder's atomic-write pattern (`NamedTempFile::new_in + sync_all + persist`) matches the cochange sibling.
- Feature: `temporal-scoring` — parallel sibling in `rskim-search`; same `SearchError` type, same `Result<T>` alias pattern
- `crates/rskim-search/src/ast_weights.rs` — vocabulary source; per-language bigram/trigram IDF tables; regenerated by `rskim-research ast-codegen`
- `crates/rskim-core/src/parser.rs` — `Parser::new(Language)` and `parser.parse(&str)` used by `linearize_source`
- `crates/rskim-search/src/index/mod.rs` — lexical sibling; `lang_map` was widened to `pub(crate)` here so `store/format.rs` can reuse `lang_to_id`/`lang_from_id`
- Issue #197 (deferred): query-side covering-set from `AstNgramSet` (Wave 3f)
- Issue #273 (follow-up): on-disk compression (delta encoding + VarInt / Roaring Bitmaps) to push index size below 1× source
