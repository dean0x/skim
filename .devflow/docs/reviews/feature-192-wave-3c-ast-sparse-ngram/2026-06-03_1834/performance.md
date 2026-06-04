# Performance Review Report

**Branch**: feature/192-wave-3c-ast-sparse-ngram -> main
**PR**: #269
**Date**: 2026-06-03 18:34
**Focus**: crates/rskim-search/src/ast_index/extract.rs (hot path), benches/linearize_bench.rs

Cycle 2. Cycle-1 fixes (HashMap cap at `nodes.len().min(1024)`, perf-gate test swap) verified present and NOT re-raised. PRIOR_RESOLUTIONS parsed cleanly.

---

## Issues in Your Changes (BLOCKING)

### HIGH

**Weight lookup runs per-emit instead of per-unique-key — redundant binary searches in the hot path** — `crates/rskim-search/src/ast_index/extract.rs:195-198` and `211-214`
**Confidence**: 90%

- Problem: The weight closure is invoked unconditionally on *every* emitted edge, before the HashMap dedup decides whether the key is new:
  ```rust
  let key = AstBigram::encode(p, node.kind_id);
  let w = bigram_weight(key);                       // ← runs every emit
  let entry = bigram_map.entry(key).or_insert((w, 0));
  entry.1 += 1;
  ```
  `bigram_weight` / `trigram_weight` resolve to `ast_bigram_idf` / `ast_trigram_idf`, which do a `match lang.name()` (string compare) plus a `binary_search_by_key` over the per-language weight table. The Rust trigram table alone is ~24K entries (`ast_weights.rs:81423`+), so each lookup is ~15 comparisons. Because the IDF weight is a *pure function of the key* (the module doc itself states "Weight is a pure function of the key so it's constant per unique key"), recomputing it for every repeated occurrence is pure waste. The KNOWLEDGE.md notes most edges repeat within a file, so the emit count is an order of magnitude larger than the unique-key count — meaning the binary search runs ~N times when it only needs to run ~unique-keys times.
- Impact: For a 1000-function fixture (already in the bench at `extract_ngrams/rust_fns/1000`) the node count is in the tens of thousands and the vast majority of emits are repeated edges (`function_item > block`, `block > expression_statement`, etc.). The wasted work is `(total_emits − unique_keys) × log2(table_len)` comparisons per file — directly on the `<50ms / 1000-line` budget path that CLAUDE.md mandates. This is the single largest avoidable cost in the function.
- Fix: Only compute the weight when the entry is vacant, using the Entry API so the lookup happens once per unique key:
  ```rust
  use std::collections::hash_map::Entry;

  let key = AstBigram::encode(p, node.kind_id);
  match bigram_map.entry(key) {
      Entry::Occupied(mut e) => { e.get_mut().1 += 1; }
      Entry::Vacant(e) => { let w = bigram_weight(key); e.insert((w, 1)); }
  }
  ```
  Same shape for the trigram path. This drops the weight lookups from O(emits) to O(unique keys) with no behavior change (the value is identical). Validate with the existing `extract_ngrams` Criterion group before/after.

---

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Sort comparator recomputes `.key()` on every comparison** — `crates/rskim-search/src/ast_index/extract.rs:235` and `245`
**Confidence**: 82%

- Problem: `bigrams.sort_unstable_by_key(|e| e.ngram.key())` calls `.key()` once per comparison element access. `sort_unstable_by_key` does NOT cache the key — it re-invokes the closure on each comparison, so `.key()` runs O(n log n) times rather than O(n). `AstBigram::key()` is a cheap field read here, so the absolute cost is small, but the idiomatic and measurably-faster form for sort-by-derived-key is `sort_by_cached_key` (caches one key per element) — or, since the entries are already unique HashMap keys, the keys could be sorted as a `Vec` of `(key, entry)` pairs.
- Impact: Minor for cheap keys; the trigram key is a `u64` so the recomputation is negligible per call but multiplied by `n log n`. On the bounded input (unique keys ≪ 100K) this is well within budget — flagged for completeness, not as a budget threat.
- Fix: If profiling shows the sort is hot, switch to `bigrams.sort_unstable_by_key(...)` → `bigrams.sort_by_cached_key(|e| e.ngram.key())`. For trivially-copyable u32/u64 keys the current form is acceptable; prefer leaving it unless the `extract_ngrams` bench shows sort dominating.

---

## Pre-existing Issues (Not Blocking)

None relevant to performance in unchanged code.

---

## Suggestions (Lower Confidence)

- **Per-file fresh ancestor `Vec` allocation** — `extract.rs:137` (Confidence: 65%) — Each call allocates `vec![None; max_depth+1]`. KNOWLEDGE.md documents this as an intentional tradeoff (bounded at 500, fresh per file). For batch extraction over thousands of files a caller-supplied reusable buffer would eliminate the per-file alloc, but that is a future API-shape change (#194), not a defect in this PR. Noting only.
- **`extract_ngrams` bench omits a high-repetition / deep-nesting fixture** — `linearize_bench.rs:158-172` (Confidence: 62%) — The bench uses `gen_rust_fns` (flat, many distinct functions) which maximizes node count but the n-gram *shapes* are highly uniform, so it under-exercises the unique-key-vs-emit ratio that the HIGH finding above hinges on. Adding a deeply-nested fixture (reuse `gen_rust_nested`) would make the per-emit weight-lookup regression visible in Criterion. Suggestion only.

---

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 0 | 0 |

**Performance Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The extraction core is well-structured — single ancestor allocation, capacity-capped maps, `sort_unstable`, zero clones, `Copy` entries, no I/O. The one real issue is the per-emit weight lookup (HIGH): the IDF weight is constant per key but is recomputed (binary search over a ~24K-entry table) for every repeated edge. Moving the lookup behind the Vacant arm of the Entry API removes the dominant avoidable cost with zero behavior change. The sort comparator note is minor and within budget.
