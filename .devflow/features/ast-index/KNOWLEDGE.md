---
feature: ast-index
name: AST Index (CST Linearization + N-gram Encoding + On-Disk Store)
description: "Use when implementing AST-based n-gram extraction, building or reading the on-disk structural index, adding a new language to the structural index, debugging depth or node-count truncation, extending the shared vocabulary, working with AstBigram/AstTrigram IDF weights, extracting structural n-grams or structural metrics from linearized nodes, using the Pattern Library (structural code patterns), using the shared AstWalkIter traversal primitive, or working with the Wave 3f BM25-ranked AST structural query engine (AstQueryEngine, AstQuery, parse_ast_query, AstPostingSource). Keywords: linearize, CST, AST, n-gram, bigram, trigram, NodeKindId, AstBigram, AstTrigram, AstNgramSet, AstBigramEntry, AstTrigramEntry, NODE_KIND_VOCABULARY, LANG_MAPS, LinearNode, AstWalkIter, AstWalkConfig, tree-sitter, depth-encoded, pre-order, IDF, ast_bigram_idf, ast_trigram_idf, extract_ast_ngrams, extract_ast_ngrams_with_metrics, extract_ast_ngrams_with_weights, StructuralMetrics, structural, Pattern, patterns, EMPTY_BODY, DEEP_NODE, LARGE_BODY, MANY_PARAMS, bucket_label, synthetic n-gram, store, AstIndexBuilder, AstIndexReader, AstPosting, AstFileMetaEntry, skidx, skpost, SKAX, FORMAT_VERSION, AST_INDEX_FORMAT_VERSION, on-disk index, mmap, posting list, build_from_files, lookup_bigram, lookup_trigram, index_version, AstQuery, AstQueryEngine, AstPostingSource, parse_ast_query, search_ast, AST_BM25_K1, AST_BM25_B, ScoringCtx, LiteMeta, file_lang_and_node_count, ast_query_to_ngram_set, score_ngram_set, CAPACITY_FLOOR, query submodule split, Wave 3f, Wave 3g, Wave 4, cmd-search, self-heal, auto-rebuild, validate_ast_pattern, ValidityMarker, validity, ast_index.skverify, CRC32 fast path, AD-376-5, AstNgramCache, CachedAstEntry, skcache, ast_cache, AST_CACHE_FILENAME, AST_CACHE_FORMAT_VERSION."
category: architecture
directories: [crates/rskim-search/src/ast_index/, crates/rskim-core/src/]
referencedFiles:
  - crates/rskim-core/src/ast_walk.rs
  - crates/rskim-core/src/lib.rs
  - crates/rskim-search/src/ast_index/linearize.rs
  - crates/rskim-search/src/ast_index/ngram.rs
  - crates/rskim-search/src/ast_index/extract.rs
  - crates/rskim-search/src/ast_index/structural.rs
  - crates/rskim-search/src/ast_index/patterns.rs
  - crates/rskim-search/src/ast_index/query/mod.rs
  - crates/rskim-search/src/ast_index/query/adapter.rs
  - crates/rskim-search/src/ast_index/query/engine.rs
  - crates/rskim-search/src/ast_index/query/parse.rs
  - crates/rskim-search/src/ast_index/query/scoring.rs
  - crates/rskim-search/src/ast_index/mod.rs
  - crates/rskim-search/src/ast_index/store/format.rs
  - crates/rskim-search/src/ast_index/store/builder.rs
  - crates/rskim-search/src/ast_index/store/reader.rs
  - crates/rskim-search/src/ast_index/store/mod.rs
  - crates/rskim-search/src/ast_weights.rs
  - crates/rskim-search/src/lib.rs
  - crates/rskim-search/benches/ast_index_bench.rs
  - crates/rskim-search/benches/ast_query.rs
created: 2026-06-01
updated: 2026-07-01
version: 11
---

# AST Index (CST Linearization + N-gram Encoding + On-Disk Store)

## Overview

The `ast_index` module converts tree-sitter Concrete Syntax Trees (CSTs) into a
compact, flat representation suitable for downstream n-gram extraction and IDF-weighted
structural search. It is the AST layer of a 3-layer search system (Lexical, Temporal,
AST n-gram) built across Waves 3a–4.

Nine sub-modules make up the full Wave 3f/3g/4 implementation:

- **`ast_cache`** — (Wave 4 incremental build) `AstNgramCache` / `CachedAstEntry`:
  a content-SHA-keyed binary cache of per-file n-gram payloads persisted to
  `ast_index.skcache`. Keyed by hex-encoded SHA-256 of file content. The CLI
  consume loop checks the cache before calling `derive_ast_entry`; hits skip the
  tree-sitter linearization + extraction entirely. Public visibility (`pub mod
  ast_cache`) so `crates/rskim/src/cmd/search/index.rs` can import it directly.

- **`linearize`** — converts source text into `Vec<LinearNode>` (pre-order depth-first
  sequence), each node carrying a shared vocabulary ID and traversal depth.
- **`ngram`** — provides `AstBigram` / `AstTrigram` newtypes, vocabulary helpers,
  and IDF weight lookup backed by per-language weight tables in `ast_weights`.
- **`extract`** — single-pass extraction of deduplicated, weighted `AstNgramSet`
  (real containment bigrams/trigrams) AND per-file `StructuralMetrics` via
  `extract_ast_ngrams_with_metrics`. A separate entry point `extract_ast_ngrams_with_weights`
  is the dependency-injected core used in unit tests.
- **`structural`** — (Wave 3e) defines reserved synthetic parent IDs
  (`EMPTY_BODY`, `DEEP_NODE`, `LARGE_BODY`, `MANY_PARAMS`), bucket-label child IDs
  (`BUCKET_LABEL_BASE`), cumulative bucket edge tables, `StructuralMetrics`, and
  `is_counted_child` (the central counting rule). Visibility: `pub(crate) mod structural`.
- **`patterns`** — (Wave 3e) data-driven catalog of 29 named structural code patterns
  in 5 categories, GOLD-verified against real code examples.
- **`store`** — (Wave 3d/3e) two-file mmap'd on-disk inverted index; format v2 adds
  per-file structural metrics and `avg_max_depth`.
- **`query/`** — (Wave 3f, #197; split #287) BM25-ranked structural pattern query engine.
  **As of #287, `query.rs` is a 4-way submodule directory**, not a single file. The
  directory structure is:
  - `query/mod.rs` — public re-export surface; `#[path]`-includes `query_tests.rs`
  - `query/parse.rs` — `AstQuery` enum, `parse_ast_query()`, parsing helpers
  - `query/engine.rs` — `AstQueryEngine`, `SearchLayer` adapter, `ast_query_to_ngram_set`
  - `query/scoring.rs` — `ScoringCtx`, BM25 helpers, IDF memoization, `LiteMeta`
  - `query/adapter.rs` — `AstPostingSource` trait and its `AstIndexReader` impl

  The `query` module remains `pub mod query` in `ast_index/mod.rs` — externally visible.
  Internal sub-modules (`parse`, `engine`, `scoring`, `adapter`) are `mod`-private within
  `query/`; only items re-exported from `query/mod.rs` are part of the API.

The design is intentionally minimal: `linearize_source` is the only stateful-setup
entry point. All n-gram encoding, weight lookup, extraction, and BM25 scoring are pure.

The DFS traversal logic lives in `rskim-core::AstWalkIter` to be shared with
`rskim-research` without duplicating cursor management or bounds guarding.

## Module Visibility

`ast_cache` is `pub mod` — externally visible so the CLI crate can import
`AstNgramCache` and `CachedAstEntry` directly.

`query` is `pub mod` — externally visible (unchanged from pre-split).

`store` and `structural` are `pub(crate)` — accessible within `rskim-search`
but not from external crates.

All other sub-modules (`extract`, `linearize`, `ngram`, `patterns`) are
`mod`-private within `ast_index`.

## Module Visibility: store sub-modules are pub(crate)

As of Wave 3g (#199, single-source-of-truth refactor), both `ast_index::store` and
`ast_index::store::format` have `pub(crate)` module visibility (previously `mod`-private).
This allows `crates/rskim-search/src/lib.rs` to reference `ast_index::store::format::FORMAT_VERSION`
directly for the `AST_INDEX_FORMAT_VERSION` constant definition. Do not revert this to
`mod`-private visibility — the CLI staleness check depends on `FORMAT_VERSION` being
reachable at the crate-root level.

## Public API Exports

### From `rskim_search::ast_index::*`

All items below are accessible via `rskim_search::ast_index::{name}`:

- **Wave 4 incremental cache**: `AstNgramCache`, `CachedAstEntry`,
  `AST_CACHE_FILENAME` (alias of `ast_cache::CACHE_FILENAME`),
  `AST_CACHE_FORMAT_VERSION` (alias of `ast_cache::CACHE_FORMAT_VERSION`) —
  re-exported from `ast_cache`
- `extract_ast_ngrams`, `extract_ast_ngrams_with_metrics`, `extract_ast_ngrams_with_weights`
- `AstBigramEntry`, `AstNgramSet`, `AstTrigramEntry`
- `LinearNode`, `LinearizeResult`, `linearize_source`
- `AstBigram`, `AstTrigram`, `DEFAULT_AST_WEIGHT`, `ast_bigram_idf`, `ast_trigram_idf`,
  `vocab_len`, `vocab_lookup`, `vocab_resolve`
- `Pattern`, `PatternCategory`, `all_patterns`, `lookup_pattern`, `pattern_to_query_set`
- `AstFileMetaEntry`, `AstIndexBuilder`, `AstIndexReader`, `AstPosting`
- `StructuralMetrics`
- `NodeKindId` (type alias for `u16`)
- **Wave 3f / #287**: `AST_BM25_B`, `AST_BM25_K1`, `AstPostingSource`, `AstQuery`,
  `AstQueryEngine`, `parse_ast_query` — re-exported from `query/mod.rs`

### From `rskim_search::*` (crate-root re-exports)

As of Wave 3g (#199, lib.rs), the following items are re-exported at the crate root.
This is the full set — use `rskim_search::{name}` for all of them:

```
AST_BM25_B, AST_BM25_K1,
AST_CACHE_FILENAME, AST_CACHE_FORMAT_VERSION,
AstBigram, AstBigramEntry, AstFileMetaEntry, AstIndexBuilder, AstIndexReader,
AstNgramCache, AstNgramSet, AstPosting, AstPostingSource, AstQuery, AstQueryEngine,
AstTrigram, AstTrigramEntry,
CachedAstEntry,
DEFAULT_AST_WEIGHT, LinearNode, LinearizeResult, NodeKindId,
Pattern, PatternCategory, StructuralMetrics,
all_patterns, ast_bigram_idf, ast_trigram_idf,
extract_ast_ngrams, extract_ast_ngrams_with_metrics, extract_ast_ngrams_with_weights,
linearize_source, lookup_pattern, parse_ast_query,
vocab_len, vocab_lookup, vocab_resolve
```

Additionally, `AST_INDEX_FORMAT_VERSION: u16` is a standalone crate-root constant
(not re-exported from `ast_index` — defined directly in `lib.rs`). As of Wave 3g
single-source refactor, it is defined as:

```rust
pub const AST_INDEX_FORMAT_VERSION: u16 = ast_index::store::format::FORMAT_VERSION;
```

A compile-time `assert!` keeps the two values in sync — bumping only one will fail
the build. `AST_INDEX_FORMAT_VERSION` is the intended public interface for CLI staleness
checks; the internal `FORMAT_VERSION` constant is the single source of truth.

Note: `pattern_to_query_set` is in `ast_index::*` but is NOT re-exported at the crate root.
Access it via `rskim_search::ast_index::pattern_to_query_set`.

## System Context

`ast_index` depends on:

- `rskim-core::Language` and `rskim-core::Parser` for grammar dispatch
- `rskim-core::AstWalkIter` and `rskim-core::AstWalkConfig` for shared DFS traversal
- `crate::ast_weights::NODE_KIND_VOCABULARY` — auto-generated sorted `&[&str]` of
  **1740** node kind strings (IDs 0–1739); IDs ≥ 1740 are free for synthetic use
- `crate::ast_weights::{ast_bigram_weight, ast_trigram_weight}` — per-language IDF tables
- `crate::types::SearchError::Ast` for the one error path not silenced gracefully
- `crate::types::{SearchLayer, SearchQuery, SearchResult, SearchField}` — implemented by
  `AstQueryEngine<AstIndexReader>` in `query/engine.rs` (Wave 3g adapter)
- `crate::index::lang_map::{lang_to_id, lang_from_id}` — single source of truth for
  language ↔ u8 ID mapping (widened to `pub(crate)` in `index/mod.rs` so `store/` reuses it)
- `crate::io_util::atomic_write` — shared atomic-write helper (NamedTempFile + sync_all +
  persist); also used by `cochange::builder`
- `crate::lexical::MAX_QUERY_BYTES` — `MAX_AST_QUERY_BYTES` in `query/parse.rs` is now
  aliased from this so both layers share one source of truth (4096 bytes)

Non-tree-sitter languages (JSON, YAML, TOML) have no entry in `LANG_MAPS`.
`linearize_source` returns an empty default; `ast_bigram_idf` returns `DEFAULT_AST_WEIGHT`.

## Component Architecture

### AstWalkIter (rskim-core)

The shared traversal primitive in `crates/rskim-core/src/ast_walk.rs`. Encapsulates
cursor management, depth tracking (`level_stack`), bounds guards, and error node
detection. `AstWalkConfig` exposes `DEFAULT_MAX_DEPTH = 500` and
`DEFAULT_MAX_NODES = 100_000` as associated constants — the canonical bound source.

### LinearNode and linearize_source

`LinearNode { kind_id: u16, depth: u16 }` — the unit of linearization output.
`kind_id` indexes into `NODE_KIND_VOCABULARY`; sentinel `0` maps to `""` for
grammar kinds absent from the vocabulary. `depth` is 0-indexed from the root.

`linearize_source` guards: files > 100 KiB (1 MiB for SQL) → empty result; language
not in `LANG_MAPS` → empty result; grammar load failure → `Err(SearchError::Ast)`.
Parse errors → empty result (tree-sitter is error-tolerant).

`LANG_MAPS` is a `LazyLock<HashMap<Language, Vec<Option<u16>>>>`. Each `Vec` is
indexed by tree-sitter's grammar-local `kind_id` and holds the vocabulary index (or
`None`) for that kind. O(1) lookup during traversal.

### AstBigram and AstTrigram (ngram.rs)

Compact newtypes packing AST node-kind IDs into integer keys:

- Bigram: `(u32::from(parent) << 16) | u32::from(child)`
- Trigram: `(u64::from(gp) << 32) | (u64::from(parent) << 16) | u64::from(child)`

These encodings match the keys in `ast_weights` weight tables. `ast_bigram_idf` and
`ast_trigram_idf` do a single binary-search call with no transformation.

`DEFAULT_AST_WEIGHT = 1.0` is the fallback for absent bigrams/trigrams and for all
non-tree-sitter languages.

### extract.rs — N-gram Extraction and Structural Metrics

The document-side extraction layer. Three main entry points:

```rust
// Dependency-injected core — testable with synthetic weights
pub fn extract_ast_ngrams_with_weights(
    nodes: &[LinearNode],
    bigram_weight: impl Fn(AstBigram) -> f32,
    trigram_weight: impl Fn(AstTrigram) -> f32,
) -> AstNgramSet { ... }

// Production extraction with structural metrics (Wave 3e) — single pass
pub fn extract_ast_ngrams_with_metrics(
    nodes: &[LinearNode],
    lang: Language,
) -> (AstNgramSet, StructuralMetrics) { ... }

// Production wrapper without metrics
pub fn extract_ast_ngrams(nodes: &[LinearNode], lang: Language) -> AstNgramSet { ... }
```

`extract_ast_ngrams_with_metrics` extends the ancestor-stack algorithm to fold in
structural computation (body-statement counting, parameter counting, depth tracking,
branch counting) and synthetic n-gram emission — all in ONE traversal pass with no
additional allocations beyond the ancestor table.

**Ancestor stack algorithm (shared core):**

1. One-pass scan for `max_depth` → allocate `Vec<Option<NodeKindId>>` of size `max_depth + 1`.
2. For each node in pre-order:
   - **Gap-fill**: if `node.depth > prev_depth + 1`, null skipped slots (u32 widening
     required to prevent u16 overflow — applies PF-004).
   - Resolve `parent = ancestors[depth-1]`, `grandparent = ancestors[depth-2]`.
   - **Emit bigram**: when `parent` is `Some(p)` AND `p != 0` AND `node.kind_id != 0`.
   - **Emit trigram**: when both ancestors are `Some` AND all three IDs are non-zero.
   - Record `ancestors[depth] = Some(node.kind_id)` (sentinel nodes ARE recorded to
     preserve correct depth positions for descendants).

**Synthetic marker emission in `extract_ast_ngrams_with_metrics`:**

Synthetic markers are bigrams whose parent ID is ≥ 65000 — outside the real vocabulary
range (0–1739) — so `vocab_resolve` returns `None` for them and no real containment
bigram can ever collide:

| Synthetic parent | ID | Trigger |
|---|---|---|
| `EMPTY_BODY` | 65000 | body/block kind with zero counted children; child = enclosing construct kind |
| `DEEP_NODE` | 65001 | any node at depth ≥ bucket edge; child = `bucket_label(i)` |
| `LARGE_BODY` | 65002 | function/method body with ≥ bucket-edge statements; child = `bucket_label(i)` |
| `MANY_PARAMS` | 65003 | parameter list with ≥ bucket-edge params; child = `bucket_label(i)` |

Bucket labels: `BUCKET_LABEL_BASE = 64900`, `bucket_label(i) = 64900 + i`. Cumulative
emission: a function body with 25 statements crosses `BODY_STMT_EDGES = [10, 20, 40]`
at indices 0 and 1, emitting both `LARGE_BODY → 64900` and `LARGE_BODY → 64901`.

Depth bucket edges: `[4, 6, 8]`. Param bucket edges: `[5, 8, 12]`.

### structural.rs (Wave 3e)

Defines all shared constants, sets, and helpers for structural n-gram emission.
Visibility is `pub(crate) mod structural` — consumers outside `rskim-search` must go
through `rskim_search::ast_index::StructuralMetrics` (re-exported from `mod.rs`).

- Synthetic parent IDs: `EMPTY_BODY` (65000), `DEEP_NODE` (65001), `LARGE_BODY` (65002),
  `MANY_PARAMS` (65003)
- Bucket constants: `BUCKET_LABEL_BASE` (64900), `MAX_BUCKET_EDGES` (99), `bucket_label(i)`
- Bucket edge tables: `BODY_STMT_EDGES = [10, 20, 40]`, `PARAM_EDGES = [5, 8, 12]`,
  `DEPTH_EDGES = [4, 6, 8]`
- `StructuralMetrics { max_depth: u16, max_block_stmts: u16, max_params: u16, branch_count: u32 }`
- `COMMENT_KIND_IDS`, `PUNCTUATION_KIND_IDS`, `FUNCTION_KIND_IDS`, `BODY_KIND_IDS`,
  `PARAM_LIST_KIND_IDS`, `BRANCH_KIND_IDS` — all `LazyLock<HashSet<NodeKindId>>`
- `is_counted_child(kind_id)` — the central counting rule

All synthetic IDs satisfy `vocab_resolve(id) == None`, which is the isolation invariant
guaranteeing no collision with real containment bigrams (where `parent <= 1739`).

### patterns.rs (Wave 3e)

Data-driven catalog of 29 named structural code patterns. A `Pattern` carries:

- `name`: kebab-case query key (e.g. `"try-catch"`, `"god-function"`)
- `description`: honest about accuracy (`exact: true` vs. approximate)
- `bigrams`/`trigrams`: string pairs/triples resolved via `vocab_lookup` or
  synthetic-name mapping (`"__empty_body__"` → `EMPTY_BODY`, `"__large_body_b10__"` →
  `bucket_label(0)`, etc.)
- `example` + `example_lang`: GOLD-verified against real code via test F7

The GOLD test (`patterns_tests.rs::f7_gold_all_patterns`) is the honesty gate:
every pattern's example must actually emit all declared n-grams when linearized
and extracted with `extract_ast_ngrams_with_metrics`.

**Catalog count guard (Wave 3g addition):** Two new tests lock the catalog count:
- `f6_exact_catalog_count` asserts `all_patterns().len() == 29`. Adding or removing
  a pattern without updating CLAUDE.md, README, and the doc table in `patterns.rs`
  will fail this test.
- `f6_per_category_counts` locks the per-category breakdown: ErrorHandling=6,
  Performance=5, Concurrency=6, Quality=7, Structure=5.

**29 patterns in 5 categories:**

| Category | Count | Examples |
|---|---|---|
| ErrorHandling | 6 | try-catch, empty-catch, python-try-except, ruby-begin-rescue |
| Performance | 5 | nested-loop, deep-nesting, call-in-loop, rust-nested-loop |
| Concurrency | 6 | go-goroutine, go-defer, go-channel-send, rust-unsafe-block, java-synchronized |
| Quality | 7 | god-function, excessive-params, empty-function, match-with-arms, unhandled-result |
| Structure | 5 | impl-method, class-method, switch-with-cases, ternary-expression |

Pattern API:

```rust
all_patterns() -> &'static [Pattern]
lookup_pattern(name: &str) -> Result<&'static Pattern>   // Err for unknown names
pattern_to_query_set(pattern: &Pattern) -> AstNgramSet   // count=1 per resolved n-gram
pattern.resolved_bigrams() -> Vec<AstBigram>             // silently drops unresolved
pattern.resolved_trigrams() -> Vec<AstTrigram>
```

### query/ — AST Structural Query Engine (Wave 3f #197; split #287; perf #286)

**As of #287, `query.rs` is a 4-way submodule directory** (`crates/rskim-search/src/ast_index/query/`).
The split is structural only — the public API surface is identical to the pre-split file;
`query/mod.rs` re-exports exactly the same symbols. The internal decomposition is:

```
query/
  mod.rs     — public re-exports; includes query_tests.rs via #[path]
  parse.rs   — AstQuery enum, parse_ast_query, parsing helpers
  engine.rs  — AstQueryEngine, SearchLayer adapter, ast_query_to_ngram_set, score_ngram_set
  scoring.rs — ScoringCtx, bm25_with_lite, idf_for_language, LiteMeta, CAPACITY_FLOOR
  adapter.rs — AstPostingSource trait, AstIndexReader impl
```

#### query/parse.rs

**`AstQuery` enum** — the only `String → AstQuery` boundary is `parse_ast_query`:

| Variant | Created by | Meaning |
|---|---|---|
| `Pattern(&'static Pattern)` | hyphenated input e.g. `"try-catch"` | Named catalog pattern |
| `Containment(AstNgramSet)` | `A > B` or `A > B > C` | Direct containment bigram/trigram |
| `SingleNode(NodeKindId)` | underscore-separated vocab name | Deferred to #283 (unigram index) |

`AstQuery` implements `PartialEq` using pointer equality for `Pattern` variants.

**`parse_ast_query`** — total function, never panics:

| Input form | Dispatch rule |
|---|---|
| Contains `-` and one segment | `lookup_pattern` → `AstQuery::Pattern` |
| `A > B` (2 segments) | `parse_bigram` → `AstQuery::Containment` |
| `A > B > C` (3 segments) | `parse_trigram` → `AstQuery::Containment` |
| One segment, no `-` | `vocab_lookup` → `AstQuery::SingleNode` |
| `>>` (transitive ancestor) | `Err(InvalidQuery)` |
| Empty segment or > 3 segments | `Err(InvalidQuery)` |
| > 4096 bytes | `Err(InvalidQuery)` |

`MAX_AST_QUERY_BYTES` is `pub(super)` in `parse.rs` and is now aliased from
`crate::lexical::MAX_QUERY_BYTES` (single source of truth — both layers share 4096 bytes).

#### query/adapter.rs

**`AstPostingSource` trait** — DI seam between the query engine and its index.
As of Wave 4 (#286, P1), a new method `file_lang_and_node_count` was added with a
default implementation that delegates to `file_meta`:

```rust
pub trait AstPostingSource: Send + Sync {
    fn lookup_bigram(&self, b: AstBigram) -> Result<Vec<AstPosting>>;
    fn lookup_trigram(&self, t: AstTrigram) -> Result<Vec<AstPosting>>;
    fn file_meta(&self, doc_id: u32) -> Result<AstFileMetaEntry>;
    fn avg_node_count(&self) -> f32;
    fn file_count(&self) -> u32;
    // P1 (#286): partial decode — default impl delegates to file_meta
    fn file_lang_and_node_count(&self, doc_id: u32) -> Result<(u8, u32)> { ... }
}
```

`AstIndexReader` implements this trait and overrides `file_lang_and_node_count` with a
fast path that decodes only bytes `[0..5]` of the 15-byte on-disk record (lang_id + node_count).
Test fakes compiled against the trait before P1 continue to work via the default implementation.

#### query/scoring.rs

Houses BM25 scoring helpers extracted into a dedicated module (#287):

- **`AST_BM25_K1: f64 = 1.2`** and **`AST_BM25_B: f64 = 0.75`** — note these are `f64`,
  not `f32`. They are re-exported through `query/mod.rs` as `pub` items.
- **`CAPACITY_FLOOR: usize = 16`** — minimum initial capacity for the `scores` FxHashMap.
  Prevents pathological grow-from-1 churn on tiny queries that suddenly fan out.
- **`LiteMeta { lang_id: u8, node_count: u32 }`** — 5-byte minimal metadata used as
  the per-posting cache value type (P1 #286). Replaces `AstFileMetaEntry` (15 bytes) in
  the meta cache, reducing cache footprint by 10 bytes per entry.
- **`ScoringCtx { scores: FxHashMap<u32, f64>, meta_cache: Option<FxHashMap<u32, LiteMeta>>, file_count: usize }`**
  — accumulates scoring state for one `run_ngram_set` call. Bundles capacity reservation
  and score accumulation into one struct to avoid 7-parameter function signatures.
  `meta_cache` is `None` for single-n-gram queries (no cross-list cache benefit from C1).

**Wave 4 performance optimizations (#286)**:

| Code | Optimization | Detail |
|---|---|---|
| P1 | Partial decode | `score_postings` calls `file_lang_and_node_count` (5 bytes) instead of `file_meta` (15 bytes) |
| P2 | Scalar IDF cache | `last_lang`/`last_idf` scalar pair collapses O(postings) IDF lookups to O(distinct langs per n-gram) |
| P3 | Posting-driven capacity | `scores.reserve(postings.len().min(file_count).saturating_sub(scores.len()))` per posting list; starts at `CAPACITY_FLOOR`; avoids both over-allocation (broad queries) and under-sizing (empty-first-list) |
| P4 | Lang filter fold-in | `run_ngram_set` accepts `lang_filter: Option<Language>`; postings whose lang_id doesn't match are skipped before insertion, eliminating the second `file_meta` decode loop that previously ran in `SearchLayer::search` |

#### query/engine.rs

**`AstQueryEngine<R: AstPostingSource>`** — immutable, `&self`-only, `Send + Sync`:

```rust
impl<R: AstPostingSource> AstQueryEngine<R> {
    pub fn new(reader: R) -> Self                           // DI constructor (tests/Wave 4)
    pub fn search_ast(&self, q: &AstQuery) -> Result<Vec<(FileId, f64)>>  // Wave-4 hook
    pub(super) fn run_ngram_set(&self, set: &AstNgramSet, lang_filter: Option<Language>) -> Result<Vec<(FileId, f64)>>
    pub(super) fn score_ngram_set(&self, set: &AstNgramSet, lang_filter: Option<Language>) -> Result<ScoringCtx>
}
impl AstQueryEngine<AstIndexReader> {
    pub fn open(dir: &Path) -> Result<Self>                 // CLI convenience constructor
}
```

`search_ast` returns results sorted **FileId-ASC** (Wave-4 merge-join contract), always
passes `lang_filter = None` (unfiltered, AC12 contract).
`SingleNode` variant returns `SearchError::InvalidQuery` referencing #283.

**`score_ngram_set`** is a private helper shared by `run_ngram_set` (production) and
`run_ngram_set_with_capacity` (test-only capacity hook). It handles dedup, ScoringCtx
construction, and the scoring loop, returning the populated `ScoringCtx`. Both callers
delegate to it and differ only in how they consume the result.

**`ast_query_to_ngram_set`** is the single `AstQuery → AstNgramSet` dispatch point,
shared by `search_ast` and `SearchLayer::search` to prevent the match arms and
`InvalidQuery` message for `SingleNode` from drifting between call sites (#286):

```rust
pub(super) fn ast_query_to_ngram_set(q: &AstQuery) -> Result<Cow<'_, AstNgramSet>> {
    match q {
        AstQuery::SingleNode(_) => Err(InvalidQuery("...#283")),
        AstQuery::Pattern(p) => Ok(Cow::Owned(pattern_to_query_set(p))),
        AstQuery::Containment(set) => Ok(Cow::Borrowed(set)),  // zero-clone hot path
    }
}
```

The `Containment` arm borrows directly (no clone) on the hot path.

**The CLI layer (`cmd/search/ast.rs`) calls `search_ast` directly** (not through
`SearchLayer::search`) for both `resolve_ast_file_filter` and `run_ast_standalone`.
This avoids `SearchResult` construction, `usize::MAX` sort, and `SearchLayer` overhead.
`SearchLayer` is still implemented for Wave 4 integration but is not the primary
CLI dispatch path as of Wave 3g.

`validate_ast_pattern` in `cmd/search/ast.rs` returns `anyhow::Result<AstQuery>` (not
`anyhow::Result<()>`). The return value is the parsed query, enabling callers that need
both validation and the query object to avoid a second `parse_ast_query` call. The
pre-dispatch call in `mod.rs` uses `?` and discards the value; `run_ast_standalone` calls
`validate_ast_pattern` and uses the returned `AstQuery` directly.

**`SearchLayer` adapter (Wave 3g, #286 P4 enhancement)**:

`AstQueryEngine<AstIndexReader>` implements `SearchLayer` via a concrete `impl` block
(not a blanket). The `search` method:

1. Returns `Err(InvalidQuery)` if `temporal_flags` is set (temporal sorting on AST
   layer is deferred to Wave 4)
2. Returns `Ok(vec![])` if `query.ast_pattern == None` (Wave-4 no-op)
3. Returns `Err(InvalidQuery("empty AST query"))` if pattern is `Some("")`
4. Trims the raw pattern before parsing (so the >4096-byte length guard applies to
   actual content, not surrounding whitespace)
5. Calls `ast_query_to_ngram_set` → `run_ngram_set(set, query.lang)` (P4: lang filter
   folded into scoring)
6. Applies `file_filter` allowlist (no I/O)
7. Sorts score-DESC/FileId-ASC tie-break
8. Applies `offset`/`limit` (default limit: **20**)
9. Returns `Vec<SearchResult>` with `line_range: 0..0` and `match_positions: vec![]` (stubs)

**OR-union BM25 scoring:**

```
score(file) = Σ idf(lang, ngram) · (tf_norm / (tf_norm + k1))
  where tf_norm = tf / length_norm
        length_norm = 1 - b + b · (node_count / avg_node_count)
        k1 = 1.2 (f64), b = 0.75 (f64)
```

Length normalization uses `node_count` (from `LiteMeta`, sourced from `AstFileMetaEntry`)
not byte count. IDF is per-language (from `ast_bigram_idf`/`ast_trigram_idf`); falls
back to `UNKNOWN_LANG_IDF = 1.0` for unknown language. When `avg_node_count == 0`,
`length_norm = 1.0`.

**Gap-fix #6**: query n-gram keys are deduped before lookup (`dedup_by_key` on sorted
bigrams and trigrams). Without this, a pattern with duplicate n-gram entries would
double-score files. `debug_assert!` verifies post-dedup uniqueness.

**C4 guarantee**: `AstPosting.count >= 1` is validated by `decode_posting` in the reader;
`bm25_with_lite` relies on this — no separate guard for `tf > 0`.

**Test coverage**: comprehensive unit suite (groups A1–A6 engine correctness, B2–B6
scoring/dedup/sort, AC1–AC12 Wave 4 perf acceptance tests) in `query_tests.rs` using
`FakePostingSource` harness. Criterion bench in `benches/ast_query.rs`: 3 scenarios ×
10k synthetic files (`bench_hot_bigram`, `bench_rare_trigram`, `bench_multi_ngram_pattern`).

### store sub-module — On-Disk Format v2

Two files in `output_dir`:

- **`ast_index.skidx`** — header + sorted lookup tables + per-file metadata
- **`ast_index.skpost`** — concatenated posting lists

Magic `b"SKAX"`, version **2** (FORMAT_VERSION=2). Distinct from lexical `b"SKIX"`.

**v2 changes from v1 (Wave 3e):**

- `AstFileMetaEntry` extended from 5 to **15 bytes** (adds `max_depth:u16`,
  `max_block_stmts:u16`, `max_params:u16`, `branch_count:u32` — exactly +10 bytes per file)
- Header reserved bytes `[38..42]` now store `avg_max_depth` as f32 LE (was zero in v1)
- Synthetic n-grams from the Pattern Library stored alongside real n-grams
- All v1 indexes are invalid: reader rejects them with "please rebuild the AST index"

**Layout of `ast_index.skidx`:**

| Section | Size | Details |
|---|---|---|
| `AstSkidxHeader` | 48 B | Magic, version, counts, averages, CRC32 |
| `AstBigramEntry` × bigram_count | 16 B each | u32 key + u64 offset + u32 length |
| `AstTrigramEntry` × trigram_count | 20 B each | u64 key + u64 offset + u32 length |
| `AstFileMetaEntry` × file_count | **15 B** each (v2) | lang_id + node_count + metrics |

**Posting entry:** 8 B — `doc_id: u32` + `count: u32`. Postings are uncompressed.
`count` is per-file structural term-frequency; IDF weight is discarded at build time
and recomputed at query time via `ast_bigram_idf`/`ast_trigram_idf`.

**CRC32** covers `idx_mmap[48..expected_idx_size]` (bigram entries + trigram entries
+ file-meta entries) as one contiguous slice. Matches serialization order on disk.

**Atomic write:** `ast_index.skpost` first, then `ast_index.skidx` (commit point).
A reader finding `.skidx` can assume `.skpost` is coherent. Uses `atomic_write` from
`crate::io_util` (the same shared helper now used by `cochange::builder`).

**FileId invariant (PRECONDITION):** FileIds must be dense, sequential, starting from
zero. Every file — including those yielding zero n-grams — must receive exactly one
`add_file_ngrams` call. Violations produce `SearchError::InvalidQuery` (duplicate or
non-sequential).

**Version probing:** `AstIndexReader::index_version(dir)` reads only the first 6 bytes
(magic + version) cheaply. The CLI self-heal path in `crates/rskim/src/cmd/search/`
(Wave 3g, #199) uses this probe: if the stored version is absent or below
`AST_INDEX_FORMAT_VERSION`, the CLI triggers an auto-rebuild before executing the query.
See `cmd-search` feature knowledge for the consumer-side wiring.

**Partial decode path (P1 #286):** `AstIndexReader::file_lang_and_node_count(file_index)` reads
the same byte range as `file_meta` but calls `decode_lang_and_node_count` to decode only
bytes `[0..5]` (lang_id + node_count). The decode offset is the single source of truth shared
with `decode_file_meta` so the two paths cannot drift.

**Validity marker CRC32 fast path (#376, AD-376-5):** `AstIndexReader::open` carries the
same validity-marker mechanism as the lexical reader. After successfully verifying the CRC32,
`open` writes `ast_index.skverify` (a 52-byte `ValidityMarker` sidecar from `crate::validity`).
On subsequent opens, if the marker's `(idx_len, idx_mtime_ns, post_len, post_mtime_ns,
header_checksum)` matches the freshly-stat'd files, the full CRC32 is SKIPPED. This moves the
per-open CRC32 cost off the `--ast` query hot path (was median 57 ms / p90 77 ms on large corpora).
Trust boundary (AD-376-2): a content byte-flip that preserves len+mtime+header.checksum is
served unverified (silent mis-rank risk, accepted per AC1 / PF-007). On any marker miss the
full CRC32 still runs. `AstIndexBuilder::flush` unlinks `ast_index.skverify` BEFORE writing
fresh files (AD-376-4) so aborted rebuilds cannot leave a stale marker. The verify-back open
in `flush` re-validates and stamps a fresh marker (AC8).

#### Reader API Contracts (C1–C7)

| Contract | Guarantee |
|---|---|
| C1 | Postings sorted ascending by `doc_id`, at most one per `doc_id` |
| C2 | Absent key → `Ok(vec![])` (no error) |
| C3 | Malformed entry (bad offset/len, OOB, `len % 8 != 0`) → `Err(IndexCorrupted)` |
| C4 | Every `count >= 1` (validated by `decode_posting`) |
| C5 | `count` is structural TF, enables BM25-style scoring |
| C6 | `file_meta(i).language()` recovers `Language`; `None` for unrecognised IDs |
| C7 | `AstIndexReader: Send + Sync` (compile-time verified by test A6) |

Reader also exposes:

- `file_metrics(file_index) -> Result<StructuralMetrics>` — extracts structural fields
  from the same on-disk entry as `file_meta`
- `avg_max_depth() -> f32` — corpus-average CST depth (from v2 header bytes [38..42])
- `file_lang_and_node_count(file_index) -> Result<(u8, u32)>` — P1 fast path (5 bytes)

#### Cross-Index FileId Contract

The AST index and the lexical index must be built over the identical, identically-ordered
file set. Neither builder owns the file manifest — that is the CLI / Wave 4 layer's
responsibility (enforced in `crates/rskim/src/cmd/search/` as of Wave 3g). Building them
over different file sets is a logic error with no runtime trap.

## Component Interactions

```
linearize_source(&str, Language)
    │
    ├── Guard: source.len() > size_limit (100 KiB; 1 MiB for SQL)  → Ok(default)
    ├── Guard: language not in LANG_MAPS                            → Ok(default)
    ├── Parser::new(language)   → Err                              → SearchError::Ast
    ├── parser.parse(source)    → Err                              → Ok(default)
    └── linearize_tree(&Tree, &[Option<u16>])
            └── AstWalkIter [max_depth=500, max_nodes=100_000]
                    ├── ERROR/MISSING nodes → skip emit (counted in error_count)
                    └── Normal → LANG_MAPS lookup → LinearNode { kind_id, depth }

extract_ast_ngrams_with_metrics(&[LinearNode], Language)
    │
    ├── max_depth scan → allocate ancestors + child_counts + depth_kind tables
    ├── For each node:
    │     ├── Update metrics.max_depth
    │     ├── Emit DEEP_NODE synthetic markers for crossed depth bucket edges
    │     ├── Gap-fill (widen to u32) → null slots + reset child_counts
    │     ├── Increment parent's child_count (if is_counted_child)
    │     ├── Close subtrees at depth ≥ current → emit EMPTY_BODY / LARGE_BODY / MANY_PARAMS
    │     ├── Increment branch_count for BRANCH_KIND_IDS
    │     ├── Emit real bigram (parent → current, sentinels suppressed)
    │     ├── Emit real trigram (gp → parent → current, sentinels suppressed)
    │     └── Record ancestors[d], depth_kind[d]; reset child_counts[d]
    ├── Close remaining open depths (end-of-stream)
    └── Collect → sort → (AstNgramSet, StructuralMetrics)

AstQueryEngine::search_ast(q: &AstQuery)
    │
    ├── ast_query_to_ngram_set(q)
    │       ├── SingleNode     → Err(InvalidQuery) [deferred to #283]
    │       ├── Pattern(p)     → pattern_to_query_set(p) → Cow::Owned
    │       └── Containment(s) → Cow::Borrowed (zero-clone)
    └── run_ngram_set(set, lang_filter=None)   [Wave-4 unfiltered contract]
            └── score_ngram_set(set, None)     [private shared helper]
                    ├── dedup_by_key bigrams and trigrams (gap-fix #6)
                    ├── P3: CAPACITY_FLOOR init; reserve(new_slots) per posting list
                    ├── For each bigram: lookup_bigram → score_postings → scores[doc_id] += BM25
                    │       ├── P1: file_lang_and_node_count (5 bytes, not 15)
                    │       ├── P2: last_lang/last_idf scalar IDF cache
                    │       └── P4: skip if lang_filter mismatch (None here → no-op)
                    └── For each trigram: lookup_trigram → score_postings → scores[doc_id] += BM25
            └── ScoringCtx::into_sorted_vec → filter (score > 0) → sort FileId-ASC
```

## Constraints and Bounds

| Constant | Value | Source |
|---|---|---|
| `MAX_FILE_SIZE` | 100 KiB | `linearize.rs` |
| `MAX_FILE_SIZE_LARGE` (SQL) | 1 MiB | `linearize.rs` |
| `DEFAULT_MAX_DEPTH` | 500 | `AstWalkConfig` |
| `DEFAULT_MAX_NODES` | 100,000 | `AstWalkConfig` |
| `MAX_AST_QUERY_BYTES` | 4096 (alias of `lexical::MAX_QUERY_BYTES`) | `query/parse.rs` |
| `HEADER_SIZE` | 48 B | `store/format.rs` |
| `BIGRAM_ENTRY_SIZE` | 16 B | `store/format.rs` |
| `TRIGRAM_ENTRY_SIZE` | 20 B | `store/format.rs` |
| `POSTING_ENTRY_SIZE` | 8 B | `store/format.rs` |
| `FILE_META_SIZE` (v2) | **15 B** | `store/format.rs` |
| `AST_BM25_K1` | 1.2 (**f64**) | `query/scoring.rs` |
| `AST_BM25_B` | 0.75 (**f64**) | `query/scoring.rs` |
| `CAPACITY_FLOOR` | 16 | `query/scoring.rs` |
| Vocabulary size | 1740 | `ast_weights.rs` |
| Free synthetic ID start | 1740 | `structural.rs` comment |
| `EMPTY_BODY` | 65000 | `structural.rs` |
| `DEEP_NODE` | 65001 | `structural.rs` |
| `LARGE_BODY` | 65002 | `structural.rs` |
| `MANY_PARAMS` | 65003 | `structural.rs` |
| `BUCKET_LABEL_BASE` | 64900 | `structural.rs` |
| `MAX_BUCKET_EDGES` | 99 | `structural.rs` |
| `AST_INDEX_FORMAT_VERSION` | 2 (alias of `FORMAT_VERSION`) | `lib.rs` (crate root) |
| `SearchLayer::search` default limit | 20 | `query/engine.rs` |

## Anti-Patterns

- **Omitting `add_file_ngrams` for files yielding zero n-grams**: every file in the
  manifest must produce exactly one call even if `AstNgramSet` is empty. Omitting it
  causes `file_count` to diverge from the lexical index.

- **Building the AST and lexical indexes from different file orderings**: both indexes
  enforce sequential FileId starting from 0 but check independently — a logic error
  with no runtime trap.

- **Using `as u32` for `node_count` narrowing**: always `u32::try_from(lin.nodes.len())`
  (applies PF-004 — no silent narrowing).

- **Treating `kind_id == 0` as "skip this node entirely"**: the sentinel is recorded
  in the ancestor table to preserve depth positions. It is suppressed only at emit time.
  Code that removes sentinel nodes from the input slice before extraction will produce
  incorrect depth relationships.

- **Treating pattern structural markers as plain-query ranking signals**: `EMPTY_BODY`,
  `DEEP_NODE`, `LARGE_BODY`, `MANY_PARAMS` are a code-audit capability. Ranking
  integration is deferred to Wave 4 (#198/#200).

- **Assuming `lookup_pattern` returns a match for any user-supplied string**: it returns
  `SearchError::InvalidQuery` for unknown names. All 29 pattern names are kebab-case;
  the error message lists all valid names.

- **Passing the `AstQuery::SingleNode` variant to `search_ast`**: always returns
  `SearchError::InvalidQuery` until #283 lands. Parse the query and check the variant
  before calling `search_ast` if `SingleNode` is a case you need to handle.

- **Skipping the gap-fix #6 dedup when building a custom `AstNgramSet` for queries**:
  duplicate keys in the query set cause double-scoring. Use `dedup_by_key` on sorted
  entries, or prefer `parse_ast_query` / `pattern_to_query_set` which produce unique sets.

- **Constructing `AstQueryEngine` with `open` in tests**: tests should use `new(FakePostingSource)`
  to avoid touching disk and to control corpus statistics.

- **Adding non-tree-sitter languages to the `LANG_MAPS` init list**: JSON, YAML, TOML
  have no tree-sitter grammar. They return empty results from `linearize_source` and
  `DEFAULT_AST_WEIGHT` from IDF lookups. This is correct behavior.

- **Holding a `LinearizeResult` across a vocabulary regeneration**: `kind_id` values are
  only meaningful relative to the `NODE_KIND_VOCABULARY` version at extraction time.
  Cached results become stale if the vocabulary is regenerated.

- **Reimplementing DFS cursor logic**: use `AstWalkIter` from `rskim-core`. All cursor
  management, bounds guarding, and `is_error` detection live there.

- **Treating `count` in `AstBigramEntry`/`AstTrigramEntry` as document frequency**:
  `count` is term frequency (occurrences in one file), not the number of documents
  containing the n-gram.

- **Accessing `structural` internals directly from outside `rskim-search`**: the module is
  `pub(crate)`. External callers use only `StructuralMetrics` re-exported from `ast_index`.

- **Using `FORMAT_VERSION` from `store/format.rs` for CLI staleness checks**: use
  `AST_INDEX_FORMAT_VERSION` from the crate root (`lib.rs`) instead. The crate-root
  constant is the intended public interface; the internal one may not be re-exported.

- **Routing through `SearchLayer::search` for AST-only or AST+text queries from the CLI**:
  the CLI layer (`cmd/search/ast.rs`) calls `search_ast` directly on `AstQueryEngine` for
  both `resolve_ast_file_filter` and `run_ast_standalone`. This avoids overhead from
  `SearchResult` construction, `usize::MAX` sort, and the `SearchLayer` wrapper. Use
  `SearchLayer` only for Wave 4 integrations that need the unified interface.

- **Reverting `ast_index::store` or `ast_index::store::format` to `mod`-private**: these
  are `pub(crate)` to allow `lib.rs` to reference `FORMAT_VERSION` as the single source
  of truth for `AST_INDEX_FORMAT_VERSION`. Reverting breaks the compile-time assertion.

- **Implementing `AstPostingSource` in a test fake without overriding `file_lang_and_node_count`**:
  the default implementation delegates to `file_meta`, so test fakes that implement
  `file_meta` correctly get the P1 path for free. Only the production `AstIndexReader`
  needs to override with the 5-byte fast path.

- **Pre-allocating `scores` at `file_count()`**: P3 (#286) uses posting-driven capacity
  (`CAPACITY_FLOOR` + `reserve(new_slots)` per list). Pre-allocating at `file_count` wastes
  memory for selective queries. Always let `score_ngram_set` handle capacity.

- **Adding a temporal filter to an AST-only query via `SearchLayer::search`**: the
  `SearchLayer` impl now returns `Err(InvalidQuery)` immediately when `temporal_flags`
  is set. Temporal sorting on the AST layer is deferred to Wave 4.

## Gotchas

- **`query/` is a directory, not a file** (as of #287): do not attempt to open or create
  `crates/rskim-search/src/ast_index/query.rs`. The module is at
  `crates/rskim-search/src/ast_index/query/mod.rs`. `query_tests.rs` remains at the
  `ast_index/` level (not inside `query/`) and is included via `#[path = "../query_tests.rs"]`
  in `query/mod.rs`.

- **`level_stack` is internal to `AstWalkIter`**: any depth-related bug fix must be made
  in `crates/rskim-core/src/ast_walk.rs`, not in `linearize.rs`.

- **`MAX_AST_DEPTH` / `MAX_AST_NODES` in `linearize.rs` are test-only aliases**: they
  are `#[cfg(test)] pub(crate)` and alias `AstWalkConfig::DEFAULT_MAX_DEPTH/NODES`.

- **Gap-fill uses `u32::from(node.depth) > u32::from(prev_depth) + 1`** (not `node.depth > prev_depth + 1`):
  the u32 widening is load-bearing. u16 addition wraps at 65535, so `p + 1` when `p == u16::MAX`
  silently evaluates to 0, bypassing gap-fill. Test B1 locks this regression.

- **tree-sitter `kind_id` is grammar-local, not vocabulary-local**: `node.kind_id()` is valid
  only within one grammar. Do not compare `kind_id` values across languages. The `LANG_MAPS`
  indirection exists to map from grammar-local IDs to the shared vocabulary.

- **SQL file size limit is 1 MiB, not 100 KiB**: a `match` on `Language::Sql` at the top of
  `linearize_source` is easy to miss when debugging why a large SQL file produces results
  while a large Rust file returns empty.

- **`post_mmap` is `None` for an empty corpus**: `AstIndexReader::open` does not mmap a
  zero-length `.skpost`. `lookup_bigram`/`lookup_trigram` return `Ok(vec![])` — callers
  must not confuse `None` post_mmap with "not found" at the API level.

- **v1 indexes are hard-rejected**: `decode_header` returns "unsupported format version: 1
  (expected 2); please rebuild the AST index". The `index_version` probe lets callers detect
  this before a full `open` call fails. The CLI self-heal path (Wave 3g, #199) uses this probe
  in `crates/rskim/src/cmd/search/` — see the `cmd-search` feature knowledge for wiring details.

- **`COMMENT_KIND_IDS` and `PUNCTUATION_KIND_IDS` lazy init at first `is_counted_child` call**:
  the initialization is O(#kinds × log(vocab_len)), tiny but not zero. Benchmarks should
  warm these sets before timing extraction.

- **`lang_map` visibility was widened to `pub(crate)` in `index/mod.rs`**: do not add a
  second language → u8 ID mapping table elsewhere; everything reuses `lang_to_id`/`lang_from_id`.

- **`ast_weights.rs` is auto-generated**: do not edit manually. Regenerate via
  `rskim-research ast-run + ast-codegen`. The vocabulary being sorted is load-bearing:
  binary search depends on it. Test `vocabulary_is_sorted` guards this invariant.

- **Index size ratio is ~1.23× source** for typical Rust corpora. The < 5% criterion
  from issue #194 is unachievable for structural AST n-grams (tiny vocabulary → dense
  posting lists). The regression guard is `< 2.2×` (measured ~1.23×; industry
  uncompressed trigram indexes run 3–5×). Compression is tracked in issue #273.

- **Structural metrics deferred from ranking**: per-file `StructuralMetrics` are stored
  and exposed via `AstIndexReader::file_metrics`, but ranking integration is deferred
  to Wave 4 (#198/#200). Do not factor them into scoring before the integration is wired.

- **`query/mod.rs` is `pub mod`, not `mod`**: all sub-modules inside `query/` are
  `mod`-private to `query/`. This keeps `ScoringCtx`, `LiteMeta`, `CAPACITY_FLOOR`,
  `ast_query_to_ngram_set`, and the `score_ngram_set` helper crate-internal. Only items
  re-exported by `query/mod.rs` are part of the external API — same surface as pre-split.

- **BM25 constants `AST_BM25_K1` and `AST_BM25_B` are `f64`**, not `f32`. The BM25
  scoring in `bm25_with_lite` uses `f64` arithmetic throughout. Any code comparing or
  combining these constants should use `f64`, not `f32`.

- **BM25 uses node_count for length normalization, not byte count**: this means two files
  with the same byte size but different language grammars will have different `length_norm`
  values if their node densities differ.

- **`pattern_to_query_set` is NOT at the crate root**: unlike `all_patterns`, `lookup_pattern`,
  and `Pattern`/`PatternCategory`, `pattern_to_query_set` is only available via
  `rskim_search::ast_index::pattern_to_query_set`. The CLI layer accesses it through
  `ast_index::*` imports, not the crate-root re-export.

- **`AstNgramCache::save()` takes no path argument** (as of the `fbc67b0` refactor):
  the cache directory is stored internally at construction time (`load(dir)` or
  `with_dir(dir)`). `empty()` creates a detached instance (empty `PathBuf`) for
  tests that inspect the cache without saving — do not call `save()` on an
  `empty()` instance in production. All three constructors: `load(dir)` (incremental
  build, reads existing skcache), `with_dir(dir)` (cold-start/`--force`, empty
  cache bound to dir), `empty()` (detached test instance, no backing store).

- **`AstNgramCache` is keyed by content SHA-256, not file path**: cache hits are
  portable across renames. Lookup is `O(1)` (HashMap). Cache misses call
  `derive_ast_entry` (linearize + extract). The cache is loaded once at build start
  and saved once at build end via `AstNgramCache::save()` (atomic write, no path
  argument — the path is stored internally when constructed via `load` or
  `with_dir`). A partial failure in `save` leaves the old cache intact.

- **AST n-gram cache (`ast_index.skcache`) is now incremental** (as of Wave 4):
  the CLI consume loop checks `AstNgramCache::lookup` by content SHA before calling
  `derive_ast_entry`. Files whose content did not change between builds skip AST
  extraction entirely. Issue #290 is addressed by this cache implementation.

- **`AST_INDEX_FORMAT_VERSION` is a type alias of `FORMAT_VERSION` with a compile-time
  assert**: `pub const AST_INDEX_FORMAT_VERSION: u16 = ast_index::store::format::FORMAT_VERSION;`.
  Changing it to a separate literal requires updating both constants and the assert.

- **The scalar IDF cache resets per n-gram, not per file**: `last_lang`/`last_idf` in
  `score_postings` are local to one `score_postings` call (one n-gram's posting list).
  This is correct (AC8): cross-n-gram reuse would be incorrect since different n-grams
  have different IDF weights for the same language.

- **P3 `new_slots` is a lower bound for `meta_cache`**: on lang-filtered runs, decoded-but-
  skipped postings populate the cache without entering `scores`, so `scores.len()` can be
  smaller than `meta_cache.len()`. The `reserve(new_slots)` call is additive and never
  under-sizes the cache.

## Key Files

- `crates/rskim-search/src/ast_index/ast_cache.rs` — `AstNgramCache`, `CachedAstEntry`,
  `CACHE_FILENAME = "ast_index.skcache"`, `CACHE_FORMAT_VERSION`; content-SHA-keyed
  incremental build cache; `pub mod` visibility. Constructors: `load(dir)`,
  `with_dir(dir)` (cold-start), `empty()` (detached test instance). `save()` writes to
  the stored `cache_dir` (no path arg).
- `crates/rskim-search/src/ast_index/ast_cache_tests.rs` — co-located tests (included
  via `#[path]` in `ast_cache.rs`)
- `crates/rskim-core/src/ast_walk.rs` — `AstWalkIter`, `AstWalkConfig` (canonical limit source), `AstWalkNode`
- `crates/rskim-search/src/ast_index/linearize.rs` — `LANG_MAPS`, `linearize_source`, `linearize_tree`; SQL size override; delegates DFS to `AstWalkIter`
- `crates/rskim-search/src/ast_index/ngram.rs` — `AstBigram`, `AstTrigram`, vocabulary helpers, IDF weight lookups
- `crates/rskim-search/src/ast_index/extract.rs` — `extract_ast_ngrams_with_metrics` (single-pass, Wave 3e), `extract_ast_ngrams_with_weights` (DI core), `AstNgramSet`, `AstBigramEntry`, `AstTrigramEntry`
- `crates/rskim-search/src/ast_index/structural.rs` — synthetic IDs, bucket edge tables, `StructuralMetrics`, `is_counted_child`, `COMMENT_KIND_IDS`, `PUNCTUATION_KIND_IDS` (Wave 3e); `pub(crate)` visibility
- `crates/rskim-search/src/ast_index/patterns.rs` — 29-pattern GOLD-verified catalog, `Pattern`, `PatternCategory`, `lookup_pattern`, `pattern_to_query_set` (Wave 3e); `f6_exact_catalog_count` and `f6_per_category_counts` tests lock catalog counts
- **`crates/rskim-search/src/ast_index/query/mod.rs`** — public re-export surface; `#[path]`-includes `query_tests.rs`; `pub mod query`
- **`crates/rskim-search/src/ast_index/query/parse.rs`** — `AstQuery`, `parse_ast_query`, parsing helpers; `MAX_AST_QUERY_BYTES` aliased from `lexical::MAX_QUERY_BYTES`
- **`crates/rskim-search/src/ast_index/query/engine.rs`** — `AstQueryEngine`, `SearchLayer` adapter, `ast_query_to_ngram_set`, `score_ngram_set`; Wave 3f/3g/4
- **`crates/rskim-search/src/ast_index/query/scoring.rs`** — `ScoringCtx`, `LiteMeta`, `AST_BM25_K1` (f64), `AST_BM25_B` (f64), `CAPACITY_FLOOR`, `bm25_with_lite`, `idf_for_language`; Wave 4 P1-P4 (#286)
- **`crates/rskim-search/src/ast_index/query/adapter.rs`** — `AstPostingSource` trait (with `file_lang_and_node_count` default), `AstIndexReader` impl with P1 override
- `crates/rskim-search/src/ast_index/store/format.rs` — pure binary codec: all on-disk struct definitions (v2), encode/decode, binary search helpers, CRC32, `decode_lang_and_node_count`; no I/O; `pub(crate)` visibility (now accessible from `lib.rs`)
- `crates/rskim-search/src/ast_index/store/builder.rs` — `AstIndexBuilder`: merge primitive, parallel `build_from_files`, atomic write via `crate::io_util::atomic_write`, FileId enforcement
- `crates/rskim-search/src/ast_index/store/reader.rs` — `AstIndexReader`, `AstPosting`: mmap open/validate, `lookup_bigram`, `lookup_trigram`, `file_meta`, `file_metrics`, `file_lang_and_node_count`, `index_version`, `avg_max_depth`
- `crates/rskim-search/src/ast_index/mod.rs` — public re-exports for all sub-modules
- `crates/rskim-search/src/ast_weights.rs` — auto-generated `NODE_KIND_VOCABULARY` (1740 entries, sorted) and per-language IDF tables; do not edit manually
- `crates/rskim-search/src/lib.rs` — crate-root re-exports including `AST_INDEX_FORMAT_VERSION` (alias of `FORMAT_VERSION` with compile-time assert) and full Wave 3g export set
- `crates/rskim-search/benches/ast_query.rs` — Criterion benchmark: 3 scenarios × 10k synthetic files

## Related

- PF-004: widen u16 depth values to u32 before arithmetic in depth comparisons
  (`u32::from(p) + 1`, not `p + 1`) to prevent wrap at `u16::MAX`. Unrelated to
  saturating casts: `max_block_stmts`/`max_params` saturate at `u16::MAX` (never wrap)
  and `branch_count` saturates at `u32::MAX` — these are direct `min()`/`saturating_add`
  patterns, not the PF-004 widening concern.
- PF-005 / ADR-003: replace empirically-baseless acceptance criteria with grounded ones —
  the index size guard is a measured `< 2.2×` regression guard (measured ~1.23×), not a
  phantom number. Background: `< 5%` is structurally unachievable for structural AST n-grams.
- Feature: `cochange` — consumes `FileId`-keyed data built from git history; the store
  builder's atomic-write pattern mirrors this module (both now use `crate::io_util::atomic_write`).
- Feature: `temporal-scoring` — parallel sibling in `rskim-search`; same `SearchError` type
  and `Result<T>` alias pattern.
- Feature: `cmd-search` — CLI command layer (`crates/rskim/src/cmd/search/`) that builds
  and queries this index. Owns the file manifest, FileId alignment between AST and lexical
  indexes, the `--ast` flag, and the self-heal/auto-rebuild path using `AstIndexReader::index_version`
  vs `AST_INDEX_FORMAT_VERSION`. Cross-link: the `cmd-search` feature knowledge documents
  the consumer-side wiring for Wave 3g.
- Feature: `research-ast` — `rskim-research` crate that produces `ast_weights.rs` via
  `ast-codegen`; also uses `AstWalkIter` from `rskim-core`.
- `crates/rskim-search/src/index/mod.rs` — lexical sibling; `lang_map` widened to `pub(crate)` here.
- `crates/rskim-search/src/io_util.rs` — `atomic_write` shared helper (NamedTempFile + sync_all + persist).
- Issue #197 (complete, Wave 3f): `AstQueryEngine`, `AstQuery`, `parse_ast_query`, BM25 scoring, `SearchLayer` adapter.
- Issue #199 (shipped, Wave 3g, PR #291): CLI `--ast` flag, building the AST index alongside
  the lexical index with FileId alignment, and self-heal/auto-rebuild on absent-or-below-FORMAT_VERSION
  via the `AstIndexReader::index_version` 6-byte probe. Consumer in `crates/rskim/src/cmd/search/`.
  Note: `run_ast_standalone` in `ast.rs` accepts `blast_file_ids: Option<HashSet<FileId>>` (pre-resolved
  by `mod.rs` via `temporal::resolve_blast_radius_file_ids`) — not raw path strings. The function is
  DB-free by design.
- Issue #286 (shipped, Wave 4 perf): P1-P4 BM25 scoring optimizations — partial decode, scalar IDF
  cache, posting-driven capacity sizing, lang filter fold-in. Adds `ScoringCtx`, `LiteMeta`,
  `ast_query_to_ngram_set`, `file_lang_and_node_count`.
- Issue #287 (shipped, Wave 4 refactor): structural split of `query.rs` into 4-way submodule directory
  (`query/mod.rs`, `parse.rs`, `engine.rs`, `scoring.rs`, `adapter.rs`). Pure structural — public API
  unchanged (AC-3: re-exports byte-identical to pre-split).
- Issue #198 / #200 (deferred, Wave 4): ranking integration of structural-complexity scoring.
- Issue #273 (follow-up): on-disk compression (delta + VarInt / Roaring Bitmaps).
- Issue #283 (deferred): unigram index for `AstQuery::SingleNode` execution.
- Issue #289 (follow-up): temporal populate path for the AST index.
- Issue #290 (follow-up): AST incremental build cache — the CLI currently re-extracts all
  files' AST n-grams on every `skim search index` refresh; no per-file cache yet.
