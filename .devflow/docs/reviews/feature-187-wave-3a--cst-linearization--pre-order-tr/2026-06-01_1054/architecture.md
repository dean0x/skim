# Architecture Review Report

**Branch**: feature/187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01

## Issues in Your Changes (BLOCKING)

### HIGH

**Redundant parser instantiation in `linearize_source` -- Parser created per-call despite LANG_MAPS already parsing per-language at init** - `crates/rskim-search/src/ast_index/linearize.rs:206-207`
**Confidence**: 85%
- Problem: `LANG_MAPS` initializes by creating a `Parser` and parsing empty source for every tree-sitter language at startup (lines 133-143). Then `linearize_source` creates a **second** `Parser::new(language)` on every call (line 206). Each `Parser::new` allocates a new `tree_sitter::Parser`, sets the grammar, and returns it. The LANG_MAPS init already proved the grammar loads -- the second instantiation is redundant work per file. For batch linearization of thousands of files, this is N extra parser allocations per language group.
- Impact: Performance overhead that scales linearly with file count. The `Parser` struct wraps `tree_sitter::Parser` which allocates internal state on construction. This is not just a HashMap lookup -- it involves `parser.set_language()` which validates the grammar ABI.
- Fix: Store the `tree_sitter::Language` (grammar object) in `LANG_MAPS` alongside the lookup table, then construct the parser from the grammar directly. Or better: store a per-thread Parser in thread-local storage and reuse it. The existing `rskim-core::Parser` already supports reuse via `parse()` on the same instance.
```rust
// Option A: Extend LANG_MAPS to store grammar + lookup table
static LANG_MAPS: LazyLock<HashMap<Language, (tree_sitter::Language, Vec<Option<u16>>)>> = ...;

// Option B: Thread-local parser cache (matches pattern in rskim-core)
thread_local! {
    static PARSERS: RefCell<HashMap<Language, Parser>> = RefCell::new(HashMap::new());
}
```

**Dual parse-empty-source pattern in LANG_MAPS init to extract grammar metadata** - `crates/rskim-search/src/ast_index/linearize.rs:138-145`
**Confidence**: 82%
- Problem: The LANG_MAPS initializer creates a `Parser`, calls `parser.parse("")` to get a `Tree`, then calls `tree.language()` to get the `tree_sitter::Language` object for iterating node kinds. This is an indirect way to access grammar metadata. The `rskim-core::Parser` already wraps `tree_sitter::Parser`, and the grammar `tree_sitter::Language` object is what `Language::to_tree_sitter()` returns. The pattern of parsing empty source just to get grammar info is a design smell.
- Impact: Makes initialization logic harder to follow. Creates a coupling to `Parser::parse()` for metadata extraction rather than grammar introspection.
- Fix: Use `Language::to_tree_sitter()` directly (it is `pub(crate)` in rskim-core, so rskim-search cannot call it today). The proper fix is either: (a) expose a `Language::tree_sitter_language()` public method on rskim-core, or (b) use the `tree-sitter` crate directly (which this PR already adds as a dependency) to create the grammar. Since the PR adds `tree-sitter` as a direct dependency to rskim-search, you could use the grammar crates directly:
```rust
// Direct grammar access -- no parse-empty-source indirection
let ts_lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
let kind_count = ts_lang.node_kind_count();
```
This matches the pattern in `rskim-core/src/types.rs:163` (`to_tree_sitter()`).

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`LANG_MAPS` uses `HashMap<Language, ...>` where a fixed-size array would suffice** - `crates/rskim-search/src/ast_index/linearize.rs:109`
**Confidence**: 80%
- Problem: The `LANG_MAPS` static is `HashMap<Language, Vec<Option<u16>>>`. The set of tree-sitter languages is known at compile time (14 languages, enumerated explicitly at lines 111-124). A `HashMap` introduces heap allocation, hashing overhead, and indirection for what is conceptually a fixed lookup table. The feature knowledge notes the pattern uses "LANG_MAPS (LazyLock HashMap) for per-language O(1) vocabulary lookup" -- but array indexing is truly O(1) while HashMap lookup involves hashing and potential collision resolution.
- Impact: Minor runtime overhead; the HashMap is initialized once and read-only after. The bigger concern is architectural clarity -- a `HashMap` signals "dynamic key set" when the key set is actually static.
- Fix: Consider a `[Option<Vec<Option<u16>>>; N]` array indexed by a `Language::index()` method, or accept the HashMap since it was a deliberate choice for readability and `Language` already derives `Hash + Eq`.

## Pre-existing Issues (Not Blocking)

### MEDIUM

**Partial code duplication between `rskim-research/ast_extract.rs::walk_tree` and `rskim-search/ast_index/linearize.rs::linearize_tree`** - across both files
**Confidence**: 85%
- Problem: Both modules implement an iterative pre-order DFS over tree-sitter CSTs using `TreeCursor` with the same bounded-depth/bounded-nodes guards, ERROR node skipping, and level-stack approach. The `rskim-research` version extracts bigrams/trigrams; the new `rskim-search` version emits `LinearNode` sequences. While the output formats differ, the traversal skeleton (descend, visit, skip errors, ascend) is nearly identical. This duplication means any traversal bug fix or guard adjustment must be applied in two places. Applies ADR-001 (fix noticed issues immediately).
- Impact: Long-term maintenance burden. If a traversal bug is found in one, the other must be updated separately. The `MAX_AST_DEPTH` (500) and `MAX_AST_NODES` (100,000) constants are duplicated with the same values across both modules.
- Fix: Extract a shared traversal primitive in a common crate (or in `rskim-core`) that accepts a visitor/callback. Both modules would implement their specific logic via the callback while sharing the traversal, bounds guards, and error handling:
```rust
// Shared traversal in rskim-core or a new shared module
pub fn walk_cst<V: CstVisitor>(tree: &Tree, visitor: &mut V) -> Result<()> { ... }

trait CstVisitor {
    fn visit_node(&mut self, kind_id: u16, depth: u16, is_error: bool);
}
```

## Suggestions (Lower Confidence)

- **`linearize_source` silently returns empty for oversized/serde languages vs returning typed error** - `crates/rskim-search/src/ast_index/linearize.rs:194-201` (Confidence: 70%) -- Callers cannot distinguish "empty because JSON" from "empty because oversized" from "empty because parse failed." A `LinearizeSkipReason` enum or Result variant would make the contract clearer for downstream consumers that need to distinguish these cases.

- **Sentinel value 0 for unknown kinds overlaps with the first vocabulary entry** - `crates/rskim-search/src/ast_index/linearize.rs:61-62` (Confidence: 65%) -- `NODE_KIND_VOCABULARY[0]` is the empty string `""`, which serves double duty as both a real vocabulary entry and the sentinel for "unknown kind." This works because `""` is semantically meaningless as a node kind, but it means downstream code must special-case `kind_id == 0` to exclude sentinels from n-gram analysis. If the vocabulary ever changes such that index 0 is meaningful, the sentinel assumption breaks silently.

- **`#[must_use]` annotation on `linearize_source` may conflict with future batch APIs** - `crates/rskim-search/src/ast_index/linearize.rs:188` (Confidence: 62%) -- The `#[must_use]` attribute is good practice for single-file calls, but if a batch `linearize_corpus` is added that internally calls `linearize_source` and aggregates results, the inner calls would need to suppress the warning.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | 0 | 1 | 0 |

**Architecture Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The new `ast_index` module is well-structured and follows the established crate patterns (module layout matches `cochange/`, error variant follows existing `SearchError` conventions, public re-exports through `lib.rs` are consistent). The iterative `TreeCursor` DFS with bounded depth/nodes is a sound approach. The `LazyLock` initialization of per-language lookup tables is architecturally clean for the O(1) traversal-time goal.

The two HIGH findings center on the same theme: the module creates redundant `Parser` instances per-call when the LANG_MAPS init already proves grammar availability, and the init itself uses an indirect parse-empty-source pattern to access grammar metadata. These are not correctness issues but represent architectural inefficiency that will matter at scale (batch linearization of thousands of files). The traversal duplication with `rskim-research` is a longer-term concern worth tracking for future refactoring.
