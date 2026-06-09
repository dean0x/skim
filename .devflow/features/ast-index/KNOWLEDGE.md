---
feature: ast-index
name: AST Index (CST Linearization + N-gram Encoding + On-Disk Store)
description: "Use when implementing AST-based n-gram extraction, building or reading the on-disk structural index, adding a new language to the structural index, debugging depth or node-count truncation, extending the shared vocabulary, working with AstBigram/AstTrigram IDF weights, extracting structural n-grams or structural metrics from linearized nodes, using the Pattern Library (structural code patterns), using the shared AstWalkIter traversal primitive, or working with the Wave 3f BM25-ranked AST structural query engine (AstQueryEngine, AstQuery, parse_ast_query, AstPostingSource). Keywords: linearize, CST, AST, n-gram, bigram, trigram, NodeKindId, AstBigram, AstTrigram, AstNgramSet, AstBigramEntry, AstTrigramEntry, NODE_KIND_VOCABULARY, LANG_MAPS, LinearNode, AstWalkIter, AstWalkConfig, tree-sitter, depth-encoded, pre-order, IDF, ast_bigram_idf, ast_trigram_idf, extract_ast_ngrams, extract_ast_ngrams_with_metrics, extract_ast_ngrams_with_weights, StructuralMetrics, structural, Pattern, patterns, EMPTY_BODY, DEEP_NODE, LARGE_BODY, MANY_PARAMS, bucket_label, synthetic n-gram, store, AstIndexBuilder, AstIndexReader, AstPosting, AstFileMetaEntry, skidx, skpost, SKAX, FORMAT_VERSION, AST_INDEX_FORMAT_VERSION, on-disk index, mmap, posting list, build_from_files, lookup_bigram, lookup_trigram, index_version, AstQuery, AstQueryEngine, AstPostingSource, parse_ast_query, search_ast, AST_BM25_K1, AST_BM25_B, query.rs, Wave 3f, Wave 3g, cmd-search, self-heal, auto-rebuild."
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
  - crates/rskim-search/src/ast_index/query.rs
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
updated: 2026-06-09
version: 6
---

# AST Index (CST Linearization + N-gram Encoding + On-Disk Store)

## Overview

The `ast_index` module converts tree-sitter Concrete Syntax Trees (CSTs) into a
compact, flat representation suitable for downstream n-gram extraction and IDF-weighted
structural search. It is the AST layer of a 3-layer search system (Lexical, Temporal,
AST n-gram) built across Waves 3a–3g.

Seven sub-modules make up the full Wave 3f/3g implementation:

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
- **`query`** — (Wave 3f, #197) BM25-ranked structural pattern query engine. Exposes
  `AstQueryEngine<R: AstPostingSource>`, `AstQuery` enum, and `parse_ast_query` parser.
  Implements the `SearchLayer` adapter for Wave-3g CLI integration. `pub mod query` — the
  only sub-module with public module visibility (others are `mod`-private, re-exported
  via `mod.rs`).

The design is intentionally minimal: `linearize_source` is the only stateful-setup
entry point. All n-gram encoding, weight lookup, extraction, and BM25 scoring are pure.

The DFS traversal logic lives in `rskim-core::AstWalkIter` to be shared with
`rskim-research` without duplicating cursor management or bounds guarding.

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

- `extract_ast_ngrams`, `extract_ast_ngrams_with_metrics`, `extract_ast_ngrams_with_weights`
- `AstBigramEntry`, `AstNgramSet`, `AstTrigramEntry`
- `LinearNode`, `LinearizeResult`, `linearize_source`
- `AstBigram`, `AstTrigram`, `DEFAULT_AST_WEIGHT`, `ast_bigram_idf`, `ast_trigram_idf`,
  `vocab_len`, `vocab_lookup`, `vocab_resolve`
- `Pattern`, `PatternCategory`, `all_patterns`, `lookup_pattern`, `pattern_to_query_set`
- `AstFileMetaEntry`, `AstIndexBuilder`, `AstIndexReader`, `AstPosting`
- `StructuralMetrics`
- `NodeKindId` (type alias for `u16`)
- **Wave 3f**: `AST_BM25_B`, `AST_BM25_K1`, `AstPostingSource`, `AstQuery`,
  `AstQueryEngine`, `parse_ast_query`

### From `rskim_search::*` (crate-root re-exports)

As of Wave 3g (#199, lib.rs), the following items are re-exported at the crate root.
This is the full set — use `rskim_search::{name}` for all of them:

```
AST_BM25_B, AST_BM25_K1,
AstBigram, AstBigramEntry, AstFileMetaEntry, AstIndexBuilder, AstIndexReader,
AstNgramSet, AstPosting, AstPostingSource, AstQuery, AstQueryEngine,
AstTrigram, AstTrigramEntry,
DEFAULT_AST_WEIGHT, LinearNode, LinearizeResult, NodeKindId,
Pattern, PatternCategory, StructuralMetrics,
all_patterns, ast_bigram_idf, ast_trigram_idf,
extract_ast_ngrams, extract_ast_ngrams_with_metrics, extract_ast_ngrams_with_weights,
linearize_source, lookup_pattern, parse_ast_query,
vocab_len, vocab_lookup, vocab_resolve
```

Additionally, `AST_INDEX_FORMAT_VERSION: u16` is a standalone crate-root constant
(not re-exported from `ast_index` — defined directly in `lib.rs`). **As of Wave 3g
single-source refactor**, it is defined as:

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
  `AstQueryEngine<AstIndexReader>` in `query.rs` (Wave 3g adapter)
- `crate::index::lang_map::{lang_to_id, lang_from_id}` — single source of truth for
  language ↔ u8 ID mapping (widened to `pub(crate)` in `index/mod.rs` so `store/` reuses it)
- `crate::io_util::atomic_write` — shared atomic-write helper (NamedTempFile + sync_all +
  persist); also used by `cochange::builder`

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

### query.rs — AST Structural Query Engine (Wave 3f, #197)

The query-side of the AST index. Implements BM25-ranked structural pattern search over
the on-disk index. Three key types:

**`AstQuery` enum** — the only `String → AstQuery` boundary is `parse_ast_query`:

| Variant | Created by | Meaning |
|---|---|---|
| `Pattern(&'static Pattern)` | hyphenated input e.g. `"try-catch"` | Named catalog pattern |
| `Containment(AstNgramSet)` | `A > B` or `A > B > C` | Direct containment bigram/trigram |
| `SingleNode(NodeKindId)` | underscore-separated vocab name | Deferred to #283 (unigram index) |

`AstQuery` implements `PartialEq` using pointer equality for `Pattern` variants.

**`AstPostingSource` trait** — DI seam between the query engine and its index:

```rust
pub trait AstPostingSource: Send + Sync {
    fn lookup_bigram(&self, b: AstBigram) -> Result<Vec<AstPosting>>;
    fn lookup_trigram(&self, t: AstTrigram) -> Result<Vec<AstPosting>>;
    fn file_meta(&self, doc_id: u32) -> Result<AstFileMetaEntry>;
    fn avg_node_count(&self) -> f32;
    fn file_count(&self) -> u32;
}
```

`AstIndexReader` implements this trait. Tests use `FakePostingSource` (in `query_tests.rs`).

**`AstQueryEngine<R: AstPostingSource>`** — immutable, `&self`-only, `Send + Sync`:

```rust
impl<R: AstPostingSource> AstQueryEngine<R> {
    pub fn new(reader: R) -> Self                          // DI constructor (tests/Wave 4)
    pub fn search_ast(&self, q: &AstQuery) -> Result<Vec<(FileId, f64)>>  // Wave-4 hook
}
impl AstQueryEngine<AstIndexReader> {
    pub fn open(dir: &Path) -> Result<Self>                // CLI convenience constructor
}
```

`search_ast` returns results sorted **FileId-ASC** (Wave-4 merge-join contract).
`SingleNode` variant returns `SearchError::InvalidQuery` referencing #283.

**The CLI layer (`cmd/search/ast.rs`) calls `search_ast` directly** (not through
`SearchLayer::search`) for both `resolve_ast_file_filter` and `run_ast_standalone`.
This avoids `SearchResult` construction, `usize::MAX` sort, and `SearchLayer` overhead.
`SearchLayer` is still implemented for Wave 4 integration but is not the primary
CLI dispatch path as of Wave 3g.

**OR-union BM25 scoring:**

```
score(file) = Σ idf(lang, ngram) · (tf_norm / (tf_norm + k1))
  where tf_norm = tf / length_norm
        length_norm = 1 - b + b · (node_count / avg_node_count)
        k1 = 1.2, b = 0.75
```

Length normalization uses `node_count` (from `AstFileMetaEntry`) not byte count. IDF is
per-language (from `ast_bigram_idf`/`ast_trigram_idf`); falls back to `1.0` for unknown
language. When `avg_node_count == 0`, `length_norm = 1.0`.

**Gap-fix #6**: query n-gram keys are deduped before lookup (`dedup_by_key` on sorted
bigrams and trigrams). Without this, a pattern with duplicate n-gram entries would
double-score files. `debug_assert!` verifies post-dedup uniqueness.

**C4 guarantee**: `AstPosting.count >= 1` is validated by `decode_posting` in the reader;
the `bm25_with_idf` helper relies on this — no separate guard for `tf > 0`.

**`SearchLayer` adapter (Wave 3g)**:

`AstQueryEngine<AstIndexReader>` implements `SearchLayer` via a concrete `impl` block
(not a blanket). The `search` method:

1. Returns `Ok(vec![])` if `query.ast_pattern == None` (Wave-4 no-op)
2. Returns `Err(InvalidQuery("empty AST query"))` if pattern is `Some("")`
3. Otherwise: `parse_ast_query` → `search_ast` → apply `file_filter` → apply `lang` filter
   → sort score-DESC/FileId-ASC tie-break → apply `offset`/`limit` → return `Vec<SearchResult>`

`line_range: 0..0` and `match_positions: vec![]` are stub values — full match attribution
is deferred to Wave 4.

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

**Test coverage**: comprehensive unit suite (groups A1–A6 engine correctness, B2–B6
scoring/dedup/sort, plus parse-error tests) in `query_tests.rs` using `FakePostingSource` harness.
Criterion bench in `benches/ast_query.rs`: 3 scenarios × 10k synthetic files
(`bench_hot_bigram`, `bench_rare_trigram`, `bench_multi_ngram_pattern`).

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
    ├── SingleNode     → Err(InvalidQuery) [deferred to #283]
    ├── Pattern(p)     → pattern_to_query_set(p) → run_ngram_set
    └── Containment(s) → run_ngram_set(s)

    run_ngram_set(set: &AstNgramSet)
        ├── dedup_by_key bigrams and trigrams (gap-fix #6)
        ├── For each bigram: lookup_bigram → bm25 → scores[doc_id] += score
        ├── For each trigram: lookup_trigram → bm25 → scores[doc_id] += score
        └── filter (score > 0) → sort FileId-ASC → Vec<(FileId, f64)>
```

## Constraints and Bounds

| Constant | Value | Source |
|---|---|---|
| `MAX_FILE_SIZE` | 100 KiB | `linearize.rs` |
| `MAX_FILE_SIZE_LARGE` (SQL) | 1 MiB | `linearize.rs` |
| `DEFAULT_MAX_DEPTH` | 500 | `AstWalkConfig` |
| `DEFAULT_MAX_NODES` | 100,000 | `AstWalkConfig` |
| `MAX_AST_QUERY_BYTES` | 4096 | `query.rs` |
| `HEADER_SIZE` | 48 B | `store/format.rs` |
| `BIGRAM_ENTRY_SIZE` | 16 B | `store/format.rs` |
| `TRIGRAM_ENTRY_SIZE` | 20 B | `store/format.rs` |
| `POSTING_ENTRY_SIZE` | 8 B | `store/format.rs` |
| `FILE_META_SIZE` (v2) | **15 B** | `store/format.rs` |
| `AST_BM25_K1` | 1.2 | `query.rs` |
| `AST_BM25_B` | 0.75 | `query.rs` |
| Vocabulary size | 1740 | `ast_weights.rs` |
| Free synthetic ID start | 1740 | `structural.rs` comment |
| `EMPTY_BODY` | 65000 | `structural.rs` |
| `DEEP_NODE` | 65001 | `structural.rs` |
| `LARGE_BODY` | 65002 | `structural.rs` |
| `MANY_PARAMS` | 65003 | `structural.rs` |
| `BUCKET_LABEL_BASE` | 64900 | `structural.rs` |
| `MAX_BUCKET_EDGES` | 99 | `structural.rs` |
| `AST_INDEX_FORMAT_VERSION` | 2 (alias of `FORMAT_VERSION`) | `lib.rs` (crate root) |

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

## Gotchas

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

- **`query.rs` is `pub mod`, not `mod`**: it is the only `ast_index` sub-module with
  public module visibility. This is intentional to expose the `AstPostingSource` trait
  for external implementors (Wave 4 integrators, test fakes).

- **BM25 uses node_count for length normalization, not byte count**: this means two files
  with the same byte size but different language grammars will have different `length_norm`
  values if their node densities differ.

- **`pattern_to_query_set` is NOT at the crate root**: unlike `all_patterns`, `lookup_pattern`,
  and `Pattern`/`PatternCategory`, `pattern_to_query_set` is only available via
  `rskim_search::ast_index::pattern_to_query_set`. The CLI layer accesses it through
  `ast_index::*` imports, not the crate-root re-export.

- **AST index is rebuilt on every `skim search index` call (no incremental cache yet)**:
  the CLI currently re-extracts all files' AST n-grams on every refresh. Incremental
  caching is tracked in issue #290.

- **`AST_INDEX_FORMAT_VERSION` is a type alias of `FORMAT_VERSION` with a compile-time
  assert**: `pub const AST_INDEX_FORMAT_VERSION: u16 = ast_index::store::format::FORMAT_VERSION;`.
  Changing it to a separate literal requires updating both constants and the assert.

## Key Files

- `crates/rskim-core/src/ast_walk.rs` — `AstWalkIter`, `AstWalkConfig` (canonical limit source), `AstWalkNode`
- `crates/rskim-search/src/ast_index/linearize.rs` — `LANG_MAPS`, `linearize_source`, `linearize_tree`; SQL size override; delegates DFS to `AstWalkIter`
- `crates/rskim-search/src/ast_index/ngram.rs` — `AstBigram`, `AstTrigram`, vocabulary helpers, IDF weight lookups
- `crates/rskim-search/src/ast_index/extract.rs` — `extract_ast_ngrams_with_metrics` (single-pass, Wave 3e), `extract_ast_ngrams_with_weights` (DI core), `AstNgramSet`, `AstBigramEntry`, `AstTrigramEntry`
- `crates/rskim-search/src/ast_index/structural.rs` — synthetic IDs, bucket edge tables, `StructuralMetrics`, `is_counted_child`, `COMMENT_KIND_IDS`, `PUNCTUATION_KIND_IDS` (Wave 3e); `pub(crate)` visibility
- `crates/rskim-search/src/ast_index/patterns.rs` — 29-pattern GOLD-verified catalog, `Pattern`, `PatternCategory`, `lookup_pattern`, `pattern_to_query_set` (Wave 3e); `f6_exact_catalog_count` and `f6_per_category_counts` tests lock catalog counts
- **`crates/rskim-search/src/ast_index/query.rs`** — `AstQuery`, `AstQueryEngine`, `AstPostingSource`, `parse_ast_query`, BM25 scoring (Wave 3f, #197); `pub mod`
- `crates/rskim-search/src/ast_index/store/format.rs` — pure binary codec: all on-disk struct definitions (v2), encode/decode, binary search helpers, CRC32; no I/O; `pub(crate)` visibility (now accessible from `lib.rs`)
- `crates/rskim-search/src/ast_index/store/builder.rs` — `AstIndexBuilder`: merge primitive, parallel `build_from_files`, atomic write via `crate::io_util::atomic_write`, FileId enforcement
- `crates/rskim-search/src/ast_index/store/reader.rs` — `AstIndexReader`, `AstPosting`: mmap open/validate, `lookup_bigram`, `lookup_trigram`, `file_meta`, `file_metrics`, `index_version`, `avg_max_depth`
- `crates/rskim-search/src/ast_index/mod.rs` — public re-exports for all seven sub-modules
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
- Issue #198 / #200 (deferred, Wave 4): ranking integration of structural-complexity scoring.
- Issue #273 (follow-up): on-disk compression (delta + VarInt / Roaring Bitmaps).
- Issue #283 (deferred): unigram index for `AstQuery::SingleNode` execution.
- Issue #289 (follow-up): temporal populate path for the AST index.
- Issue #290 (follow-up): AST incremental build cache — the CLI currently re-extracts all
  files' AST n-grams on every `skim search index` refresh; no per-file cache yet.
