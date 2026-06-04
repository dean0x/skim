# Performance Review Report

**Branch**: feature/192-wave-3c-ast-sparse-ngram -> main
**PR**: #269
**Date**: 2026-06-03

Scope: `crates/rskim-search/src/ast_index/extract.rs` (new, 236 lines) and
`crates/rskim-search/benches/linearize_bench.rs` (extract_ngrams bench group).
Test files and clippy-cleanup edits in other modules carry no runtime-path cost
and are out of performance scope.

## Issues in Your Changes (BLOCKING)

### CRITICAL
None.

### HIGH
None.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**HashMap pre-sized to `nodes.len()` — large over-allocation** — `extract.rs:127-128`
**Confidence**: 85%
- Problem: Both accumulation maps are sized `HashMap::with_capacity(nodes.len())`.
  The number of *unique* n-grams is bounded by the count of distinct structural
  edges in the file, which is far smaller than the node count — in real code
  hundreds of unique bigrams/trigrams against tens of thousands of nodes (the
  1000-function fixture). The KNOWLEDGE.md "reasonable upper bound" framing is
  technically a valid ceiling but a poor *estimate*: it over-reserves two hash
  tables by roughly an order of magnitude, spreading inserts across more buckets
  (worse cache locality) and reserving memory that is never used. This runs
  counter to the project's "minimize allocation after initialization / pre-sized
  collections" reliability guidance — pre-sizing is only a win when the size is
  close to the actual count.
- Impact: Wasted heap and extra cache pressure per extraction call; the ancestor
  table is fresh per file (KNOWLEDGE.md "Ancestor stack is NOT re-used between
  files"), so batch extraction pays this twice per file across a whole corpus.
- Fix: Use a smaller heuristic capacity rather than `nodes.len()`, or let the
  maps grow from default. A bigram per node is the theoretical max but the unique
  set is tiny; for example seed with a fraction and let growth handle outliers:
  ```rust
  // Unique n-grams << nodes; seed modestly and let the map grow if needed.
  let cap = nodes.len().min(1024);
  let mut bigram_map: HashMap<AstBigram, (f32, u32)> = HashMap::with_capacity(cap);
  let mut trigram_map: HashMap<AstTrigram, (f32, u32)> = HashMap::with_capacity(cap);
  ```
  Worth a quick Criterion comparison against the existing `extract_ngrams`
  group before settling on the constant (applies ADR-001: measure, then fix).

## Pre-existing Issues (Not Blocking)
None — this is net-new code.

## Suggestions (Lower Confidence)

- **Two-pass `max_depth` scan is separable** — `extract.rs:116` (Confidence: 70%) —
  The `max()` scan is a dedicated O(n) pass before the main loop. Depth is already
  bounded at 500 (`AstWalkConfig::DEFAULT_MAX_DEPTH`), so the ancestor table could
  be sized `min(nodes.len(), 501)` directly, dropping the scan entirely while
  keeping the single allocation. The scan is cheap (sequential, branch-friendly)
  so this is a micro-optimization, not a correctness or scaling issue.
- **`weight` closure called on every emit, not once per unique key** — `extract.rs:165,177`
  (Confidence: 65%) — `bigram_weight(key)` / `trigram_weight(key)` run on every
  occurrence, but the result is only used when the entry is first inserted
  (`or_insert((w, 0))`); for repeated edges the weight is computed and discarded.
  Production weight lookup is a binary search per call. Computing it lazily (only
  on insert) would skip redundant lookups for high-frequency edges. Low impact
  because the lookup is fast and cache-resident, but it is avoidable work in the
  hot loop.

## Performance Verification Notes (positive findings)

- **O(n) / scaling claims confirmed.** Main loop is O(n); the gap-fill inner loop
  (`extract.rs:142-144`) nulls skipped slots but total nulling work across the run
  is bounded by total depth traversed, so it does not make the loop super-linear.
  Overall cost is O(n + u·log u) where u = unique n-grams ≤ n, dominated by the two
  sorts — unavoidable given the sorted-output contract. Comfortably within the
  project's <50ms/1000-line target for extraction-only timing.
- **Single-allocation ancestor table confirmed.** `vec![None; max_depth + 1]`
  (`extract.rs:121`) is one allocation with no per-iteration growth, exactly as
  documented.
- **`sort_unstable_by_key` cost is correct and cheap.** `key()` (ngram.rs:81,145)
  is `#[inline]` and returns the already-packed integer field — no allocation, no
  derived computation. `sort_unstable` avoids the scratch allocation a stable sort
  would need; ordering is total on the packed key so unstable is safe.
- **Bench design is correct.** `linearize_source` runs once outside the timed
  closure (`linearize_bench.rs:160`), so the group measures extraction in isolation
  from parsing. `black_box` wraps both the node slice and the language argument.
- **Zero-copy adherence confirmed.** `nodes: &[LinearNode]` is borrowed and not
  mutated; `LinearNode` is `Copy`; no `String` allocation in the hot path — only
  the bounded ancestor `Vec` and the two result `Vec`s built from the maps.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 0 | 0 |

**Performance Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The extraction core is O(n)-correct, single-allocation for the ancestor table,
zero-copy on input, and the benchmark is honestly constructed. The one
should-fix is the HashMap over-allocation to `nodes.len()`; addressing it (with
a Criterion check per ADR-001) removes the only non-trivial waste in the hot path.
