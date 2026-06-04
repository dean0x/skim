# Performance Review Report

**Branch**: feature/187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Misleading performance test: 100 functions labeled as "1000-function" / "~1000-line"** - `crates/rskim-search/src/ast_index/linearize_tests.rs:427-441`
**Confidence**: 95%
- Problem: The performance test at line 427 generates only 100 functions via `(0..100)` but the comment says "Generate a 1000-function Rust file" and the assertion message says "~1000-line Rust file". The performance target of <5ms for 1000 lines is stated in FEATURE_KNOWLEDGE, but this test only exercises ~100 lines. This means the performance target is not actually validated by this test — a 10x regression could slip through undetected.
- Fix: Change the range to `(0..1000)` to match the stated target, or fix the comment and assertion message to say "~100-function":
```rust
// Option A: Fix to match the stated target
let source: String = (0..1000)
    .map(|i| format!("fn func_{i}(x: i32) -> i32 {{ x + {i} }}\n"))
    .collect();
// ...
"linearize_source took {}ms for ~1000-function Rust file, expected < 5ms",

// Option B: Fix the labels to match reality
// Generate a 100-function Rust file (well under MAX_FILE_SIZE).
let source: String = (0..100)
// ...
"linearize_source took {}ms for ~100-function Rust file, expected < 5ms",
```

**`level_stack` not pre-allocated** - `crates/rskim-search/src/ast_index/linearize.rs:248`
**Confidence**: 82%
- Problem: The `level_stack: Vec<u16>` is created with `Vec::new()` (zero capacity) even though the tree depth is bounded by `MAX_AST_DEPTH` (500) and the `nodes` Vec is pre-allocated via `Vec::with_capacity(capacity)`. For trees with moderate depth (20-50 levels, common for real code), the level_stack will reallocate 4-5 times during traversal (capacities 0->1->2->4->8->16->32->64). Each reallocation involves a `memcpy`.
- Fix: Pre-allocate with a reasonable initial capacity. A conservative estimate based on typical AST depth:
```rust
// Most real-world ASTs have depth < 64. Pre-allocate to avoid resizing.
let mut level_stack: Vec<u16> = Vec::with_capacity(64);
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **HashMap lookup for LANG_MAPS on every linearize_source call** - `crates/rskim-search/src/ast_index/linearize.rs:199` (Confidence: 65%) — `LANG_MAPS` uses `HashMap<Language, Vec<Option<u16>>>` for 14 entries. Since `Language` is a fixed enum with 17 variants, an array `[Option<Vec<Option<u16>>>; 17]` indexed by discriminant would replace hash+compare with a single array dereference. The HashMap lookup is fast for 14 entries but is called on every `linearize_source()` invocation. Benchmark before changing.

- **Parser created and destroyed on every linearize_source call** - `crates/rskim-search/src/ast_index/linearize.rs:206` (Confidence: 68%) — Each call to `linearize_source()` creates a new `Parser` (which allocates a `tree_sitter::Parser` on the heap) and destroys it after parsing. For batch scenarios (indexing hundreds of files), a `thread_local!` parser cache per language could avoid repeated allocation/deallocation. The overhead per call is likely ~microseconds vs. parse time of ~milliseconds, so this matters more at scale.

- **LANG_MAPS init parses empty string to extract Language object** - `crates/rskim-search/src/ast_index/linearize.rs:140` (Confidence: 60%) — The init code creates a `Parser`, parses an empty string, then calls `tree.language()` to get the `tree_sitter::Language` for node kind enumeration. This works but is indirect — the `tree_sitter::Language` is available directly from `Language::to_tree_sitter()`, but that method is `pub(crate)` in `rskim-core`. This is a one-time init cost so impact is negligible. If `to_tree_sitter()` is ever made `pub`, this could be simplified.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The core hot path (`linearize_tree`) is well-designed: iterative DFS with TreeCursor, O(1) vocabulary lookup via pre-built array tables, pre-allocated output Vec via `descendant_count()`, bounded by MAX_AST_NODES and MAX_AST_DEPTH guards. The LazyLock one-time init is correct and amortized. The two blocking MEDIUM issues are: (1) a performance test that does not actually validate the stated <5ms target for 1000-line files (it only tests ~100 lines), and (2) a minor allocation inefficiency in the level_stack. Both are straightforward fixes. Applies ADR-001 — surfacing all findings for immediate resolution rather than deferral.
