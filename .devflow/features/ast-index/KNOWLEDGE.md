---
feature: ast-index
name: AST Index (CST Linearization + N-gram Encoding)
description: "Use when implementing AST-based n-gram extraction, adding a new language to the structural index, debugging depth or node-count truncation, extending the shared vocabulary, working with AstBigram/AstTrigram IDF weights, extracting structural n-grams from linearized nodes, or using the shared AstWalkIter traversal primitive. Keywords: linearize, CST, AST, n-gram, bigram, trigram, NodeKindId, AstBigram, AstTrigram, AstNgramSet, AstBigramEntry, AstTrigramEntry, NODE_KIND_VOCABULARY, LANG_MAPS, LinearNode, AstWalkIter, AstWalkConfig, tree-sitter, depth-encoded, pre-order, IDF, ast_bigram_idf, ast_trigram_idf, extract_ast_ngrams, extract_ast_ngrams_with_weights."
category: architecture
directories: [crates/rskim-search/src/ast_index/, crates/rskim-core/src/]
referencedFiles:
  - crates/rskim-core/src/ast_walk.rs
  - crates/rskim-core/src/lib.rs
  - crates/rskim-search/src/ast_index/linearize.rs
  - crates/rskim-search/src/ast_index/ngram.rs
  - crates/rskim-search/src/ast_index/extract.rs
  - crates/rskim-search/src/ast_index/mod.rs
  - crates/rskim-search/src/ast_index/linearize_tests.rs
  - crates/rskim-search/src/ast_index/ngram_tests.rs
  - crates/rskim-search/src/ast_index/extract_tests.rs
  - crates/rskim-search/src/ast_weights.rs
  - crates/rskim-search/src/lib.rs
  - crates/rskim-search/benches/linearize_bench.rs
created: 2026-06-01
updated: 2026-06-03T15:32Z
---

# AST Index (CST Linearization + N-gram Encoding)

## Overview

The `ast_index` module converts tree-sitter Concrete Syntax Trees (CSTs) into a
compact, flat representation suitable for downstream n-gram extraction and IDF-weighted
structural search. It has three sub-modules:

- **`linearize`** — converts source text into `Vec<LinearNode>`, a pre-order
  depth-first sequence where each node carries a vocabulary ID and traversal depth.
- **`ngram`** — provides `AstBigram` and `AstTrigram` newtypes for packing
  node-kind ID pairs/triples into compact integer keys, plus vocabulary helpers
  and IDF weight lookup backed by the per-language weight tables in `ast_weights`.
- **`extract`** — (Wave 3c) consumes a `Vec<LinearNode>` and produces a deduplicated,
  weighted `AstNgramSet` of structural bigrams and trigrams. This is the document-side
  extraction step; query-side covering-set and on-disk index format are deferred to
  future issues (#197, #194).

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

## Key Files

- `crates/rskim-core/src/ast_walk.rs` — `AstWalkIter`, `AstWalkConfig` (with `DEFAULT_MAX_DEPTH`/`DEFAULT_MAX_NODES`), `AstWalkNode`; shared DFS primitive and canonical limit source
- `crates/rskim-search/src/ast_index/linearize.rs` — `LANG_MAPS`, `linearize_source`, `linearize_tree`; SQL size override; delegates DFS to `AstWalkIter`
- `crates/rskim-search/src/ast_index/ngram.rs` — `AstBigram`, `AstTrigram`, `NodeKindId`, vocabulary helpers, IDF weight lookups
- `crates/rskim-search/src/ast_index/extract.rs` — `extract_ast_ngrams`, `extract_ast_ngrams_with_weights`, `AstNgramSet`, `AstBigramEntry`, `AstTrigramEntry`; depth-indexed ancestor stack; document-side extraction only
- `crates/rskim-search/src/ast_index/mod.rs` — public re-exports for all three sub-modules
- `crates/rskim-search/src/ast_index/linearize_tests.rs` — 8 test cycles: types, vocabulary, traversal, error nodes, bounds, multi-language, edge cases, performance
- `crates/rskim-search/src/ast_index/ngram_tests.rs` — 14 test groups: encode/decode roundtrips, key formula, Display, vocab helpers, IDF weight lookup
- `crates/rskim-search/src/ast_index/extract_tests.rs` — 26 test cases: empty input, chain, siblings, dedup/count, gap-fill, sentinel suppression (parent + grandparent), end-to-end, sort/uniqueness, determinism, input immutability, injected weights, unknown-weight default, crate-root re-exports, large-input smoke test; batch-2 regression tests: B1 u16::MAX depth overflow guard (×2), B2 documented spurious same-depth-sibling edge (×1), B3 trigram count accumulation (×1), B4 gap-fill at max-depth boundary (×2), B5 depth-0 underflow via checked_sub (×2). Helper `bigram_keys(set)` deduplicates bigram key extraction across tests.
- `crates/rskim-search/src/ast_weights.rs` — auto-generated `NODE_KIND_VOCABULARY` (1740 entries, sorted) and per-language bigram/trigram weight tables; do not edit manually
- `crates/rskim-search/benches/linearize_bench.rs` — Criterion benchmarks: per-language, scaling by function count, nesting depth, LazyLock init latency, and extraction (`extract_ngrams` group, 100/500/1000-function Rust fixtures)

## Related

- ADR-001: Fix all noticed issues immediately regardless of scope — applies when finding invariant violations or guard logic gaps during traversal or extraction changes. PR #269 applied this: all 13 review findings (1 blocking + 12 others) were fixed before merge.
- PF-004: u16 depth arithmetic overflow — always widen to u32 before adding an offset in depth comparisons. See gap-fill note above.
- Feature: `cochange` — consumes `FileId`-keyed data built from git history; future n-gram consumers will similarly receive `FileId`-keyed `LinearizeResult` and `AstNgramSet` output
- Feature: `temporal-scoring` — parallel sibling in `rskim-search`; same `SearchError` type, same `Result<T>` alias pattern
- `crates/rskim-search/src/ast_weights.rs` — vocabulary source; per-language bigram/trigram IDF tables; regenerated by `rskim-research ast-codegen`
- `crates/rskim-core/src/parser.rs` — `Parser::new(Language)` and `parser.parse(&str)` used by `linearize_source`
- Issue #197 (deferred): query-side covering-set from `AstNgramSet`
- Issue #194 (deferred): on-disk index format for persisting `AstNgramSet` entries
