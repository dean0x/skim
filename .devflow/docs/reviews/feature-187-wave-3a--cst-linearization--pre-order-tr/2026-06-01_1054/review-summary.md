# Code Review Summary

**Branch**: feature-187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01
**Review Cycle**: 1 (no prior resolutions)

## Merge Recommendation: CHANGES_REQUESTED

The new `ast_index` module is well-designed and architecturally sound, with strong test coverage and no regressions. However, blocking HIGH-severity issues in architecture (2), consistency (1), and rust typing (1) must be resolved before merge. These are not correctness issues but represent design inefficiencies and API clarity problems that will impact maintenance and scalability.

---

## Issue Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| **Blocking** | 0 | 4 | 6 | 0 |
| **Should Fix** | 0 | 0 | 3 | 0 |
| **Pre-existing** | 0 | 0 | 1 | 0 |
| **TOTAL** | 0 | 4 | 10 | 0 |

---

## Blocking Issues (Must Fix Before Merge)

### HIGH: Redundant Parser Instantiation Pattern (3 findings converged)

**Location**: `crates/rskim-search/src/ast_index/linearize.rs:206-207`
**Confidence**: 85% (from architecture + rust + performance reviewers)

**Problem**: `LANG_MAPS` initializes by creating a `Parser` and parsing empty source for every tree-sitter language at startup (lines 133-143) to build lookup tables. Then `linearize_source` creates a **second** `Parser::new` on every call (line 206). This redundant allocation happens on the hot path and will scale linearly with file count in batch operations.

**Impact**: Performance overhead scales with linearize call frequency. Each `Parser::new` allocates internal state and validates grammar ABI. For batch linearization of thousands of files, this represents N extra allocations per language group.

**Fix**: Cache the `tree_sitter::Language` grammar object in `LANG_MAPS` alongside the lookup table, then reuse it for all parser instances. Or use thread-local parser cache:
```rust
// Option A: Extend LANG_MAPS to store grammar + lookup table
static LANG_MAPS: LazyLock<HashMap<Language, (tree_sitter::Language, Vec<Option<u16>>)>> = ...;

// Option B: Thread-local parser cache (matches pattern in rskim-core)
thread_local! {
    static PARSERS: RefCell<HashMap<Language, Parser>> = RefCell::new(HashMap::new());
}
```

---

### HIGH: Indirect Grammar Access via Parse-Empty-Source (2 findings converged)

**Location**: `crates/rskim-search/src/ast_index/linearize.rs:138-145`
**Confidence**: 82% (from architecture + rust reviewers)

**Problem**: The `LANG_MAPS` initializer creates a `Parser`, calls `parser.parse("")` to get a `Tree`, then calls `tree.language()` to extract grammar metadata for node kind enumeration. This indirect pattern couples init logic to parsing semantics when the `tree_sitter::Language` object is available directly from the grammar crates.

**Impact**: Makes initialization logic harder to follow. Creates unnecessary coupling to `Parser::parse()` for metadata extraction.

**Fix**: Use `tree-sitter` crate directly (already added as direct dependency) to access grammar without parsing:
```rust
// Direct grammar access -- no parse-empty-source indirection
let ts_lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
let kind_count = ts_lang.node_kind_count();
```

This matches the pattern already used in `rskim-core/src/types.rs:163`.

---

### HIGH: SearchError Variant Naming Breaks Convention

**Location**: `crates/rskim-search/src/types.rs:624`
**Confidence**: 95% (consistency reviewer)

**Problem**: New variant is named `AstError(String)`, but all 9 existing `SearchError` variants avoid the "Error" suffix (using `Core`, `IndexCorrupted`, `InvalidQuery`, `FileNotFound`, `Io`, `Git`, `FileTooLarge`, `CapacityExceeded`, `Database`). The "Error" suffix is redundant since these are already variants of `SearchError` enum.

**Impact**: API inconsistency confuses developers about naming patterns. Violates the established convention throughout the crate.

**Fix**: Rename to `Ast` to match the convention:
```rust
/// AST processing error (e.g. grammar load failure for a tree-sitter language).
#[error("AST error: {0}")]
Ast(String),
```
Update construction site in `linearize.rs:207`:
```rust
.map_err(|e| SearchError::Ast(format!("grammar load failure for {language:?}: {e}")))?;
```

---

### MEDIUM: Silent u16 Truncation on Kind Count

**Location**: `crates/rskim-search/src/ast_index/linearize.rs:153`
**Confidence**: 82% (security reviewer)

**Problem**: `node_kind_count()` returns `usize` but cast to `u16` via `kind_id as u16` on line 153. If a tree-sitter grammar reports >65,535 node kinds, the cast silently wraps. Current grammars have 200-500 kinds (safe), but the unsafe pattern could break on future grammar expansions.

**Impact**: Theoretical but represents an unsafe pattern. Would produce incorrect vocabulary mappings for grammar kinds with ID >= 65536 if such grammars ever exist.

**Fix**: Use explicit fallible conversion:
```rust
let kind_id_u16 = match u16::try_from(kind_id) {
    Ok(id) => id,
    Err(_) => continue,
};
if let Some(kind_str) = ts_lang.node_kind_for_id(kind_id_u16) {
```

---

### MEDIUM: Misleading Performance Test (2 findings converged)

**Location**: `crates/rskim-search/src/ast_index/linearize_tests.rs:427-441`
**Confidence**: 95% (performance reviewer), 85% (testing reviewer)

**Problem**: Test generates only 100 functions via `(0..100)` but comment says "Generate a 1000-function Rust file" and assertion says "~1000-line Rust file". The stated target is <5ms for 1000 lines, but test only exercises ~100 lines. A 10x regression could slip through undetected.

**Impact**: Performance target validation is incomplete. Developers may assume performance is validated when it is not.

**Fix**: Change to match the stated target:
```rust
// Option A: Fix to match the stated target
let source: String = (0..1000)
    .map(|i| format!("fn func_{i}(x: i32) -> i32 {{ x + {i} }}\n"))
    .collect();
// ...
"linearize_source took {}ms for ~1000-function Rust file, expected < 5ms",

// Option B: Fix the labels to match reality
let source: String = (0..100)
// ...
"linearize_source took {}ms for ~100-function Rust file, expected < 5ms",
```

---

### MEDIUM: infallible Function Returns Result Type

**Location**: `crates/rskim-search/src/ast_index/linearize.rs:232-235`
**Confidence**: 82% (rust reviewer)

**Problem**: `linearize_tree` function return type is `Result<LinearizeResult>` but both exit points (lines 261 and 308) return `Ok(...)`. No error path exists. This violates Rust principle that types represent actual possibilities.

**Impact**: Misleads callers about error handling. Creates dead code paths. Type system does not match reality.

**Fix**: Change to return `LinearizeResult` directly:
```rust
fn linearize_tree(
    tree: &tree_sitter::Tree,
    lang_map: &[Option<u16>],
) -> LinearizeResult {
```

---

## Should-Fix Issues (Category 2: Code You Touched)

### MEDIUM: `#[must_use]` Custom Message Inconsistent

**Location**: `crates/rskim-search/src/ast_index/linearize.rs:188`
**Confidence**: 90% (consistency reviewer)

**Problem**: Uses `#[must_use = "linearize_source returns a Result that must be checked"]`, but all 13+ other `#[must_use]` annotations in rskim-search use bare `#[must_use]` without custom messages.

**Fix**: Replace with bare `#[must_use]`:
```rust
#[must_use]
pub fn linearize_source(
```

---

### MEDIUM: Test File `#![allow]` Style Inconsistent Within PR

**Location**: `crates/rskim-search/src/ast_index/linearize_tests.rs:13-14`
**Confidence**: 85% (consistency reviewer)

**Problem**: Uses two separate `#![allow]` lines but same PR modifies `rskim-research/src/ast_extract.rs` to combine them on one line. Existing test files in rskim-search use single-line style.

**Fix**: Combine into one line matching `ast_extract.rs` precedent:
```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]
```

---

### MEDIUM: lib.rs Module Documentation Missing ast_index

**Location**: `crates/rskim-search/src/lib.rs:1-15`
**Confidence**: 92% (consistency reviewer)

**Problem**: Architecture doc comment enumerates every module (`types`, `index`, `ngram`, `temporal`, `cochange`) but omits the newly added `ast_index` module.

**Fix**: Add bullet for `ast_index`:
```rust
//! - The `ast_index` module linearizes tree-sitter CSTs into compact
//!   depth-encoded node sequences for AST n-gram extraction (pure, no I/O).
```

---

### MEDIUM: Doc Comment Says "Named" But Code Emits All Nodes

**Location**: `crates/rskim-search/src/ast_index/linearize.rs:83`
**Confidence**: 84% (rust reviewer)

**Problem**: `LinearizeResult` doc says "named, non-error" but traversal does NOT filter by `node.is_named()`. Anonymous nodes (punctuation tokens) are emitted. Consistent with `rskim-research/src/ast_extract.rs` but doc is misleading.

**Fix**: Update doc comment:
```rust
/// `nodes` (non-error) or is counted in `error_count` (ERROR/MISSING).
```

---

### MEDIUM: Counter Increments Use Raw `+=` Not `saturating_add`

**Location**: `crates/rskim-search/src/ast_index/linearize.rs:271`, `274`
**Confidence**: 82% (reliability reviewer)

**Problem**: `result.node_count += 1` and `result.error_count += 1` use raw addition on `u32` counters, but PR description states "saturating_add for counters". Only `depth` (line 297) uses `saturating_add`. While overflow is impossible in practice (MAX_AST_NODES caps at 100K), the inconsistency between documentation and implementation creates a reliability gap.

**Fix**: Use saturating_add for consistency:
```rust
result.node_count = result.node_count.saturating_add(1);
result.error_count = result.error_count.saturating_add(1);
```

---

### MEDIUM: `is_missing()` Not Checked in ast_extract.rs

**Location**: `crates/rskim-research/src/ast_extract.rs:190`
**Confidence**: 80% (rust reviewer)

**Problem**: The modified file checks only `node.is_error() || kind == "ERROR"`, but the new `linearize.rs` correctly checks `node.is_error() || node.is_missing()` (line 273). MISSING nodes (synthetic, error recovery) should receive same treatment as ERROR nodes.

**Fix**: Update ast_extract.rs line 190:
```rust
let is_error = node.is_error() || node.is_missing();
```

---

## Additional MEDIUM Issues

### Bounds Guards Don't Actually Validate (2 findings converged)

**Location**: `crates/rskim-search/src/ast_index/linearize_tests.rs:242-267`, `184-194`
**Confidence**: 85% (testing reviewer), 82% (testing reviewer)

**Problem 1 - MAX_AST_NODES Cap Test**: Test generates only `"let x = 1;\n".repeat(100)` which produces far fewer than 100,000 AST nodes. Assertion always passes trivially. No evidence the inner bounds guard loop (lines 253-267 of `linearize.rs`) works when `node_count >= MAX_AST_NODES`.

**Problem 2 - Error Counting Test**: Assertion is `result.error_count > 0 || result.node_count > 0` which is tautologically true (tree-sitter always produces root node). Test never verifies `error_count > 0` despite name promising it validates error handling.

**Fix 1**: Generate compact source that stays under MAX_FILE_SIZE but produces high node count, or reduce MAX_AST_NODES locally in test.

**Fix 2**: Assert `result.error_count > 0` directly with malformed input that reliably produces ERROR nodes.

---

### Pre-existing: Code Duplication with ast_extract.rs

**Location**: Across `rskim-research/src/ast_extract.rs` and `rskim-search/src/ast_index/linearize.rs`
**Confidence**: 85% (architecture reviewer)

**Problem**: Both modules implement nearly identical iterative pre-order DFS over tree-sitter CSTs using `TreeCursor`, with same bounded-depth/bounded-nodes guards, ERROR skipping, and level-stack approach. Duplication means bug fixes must be applied in two places.

**Categorization**: PRE-EXISTING (not in your changes, but noted per ADR-001)

**Recommendation**: Extract shared traversal primitive in common crate. This is a longer-term refactoring task, not blocking this PR.

---

## Convergence Status

**Cycle**: 1 (baseline review, no prior resolutions)

**Convergence Patterns**:
- **Redundant Parser** issue flagged by 3 reviewers (architecture, rust, performance) with high confidence (85%+) — strength of finding increased from 85% to 95% via cross-reviewer confirmation
- **Parse-Empty-Source** pattern flagged by 2 reviewers (architecture, rust) with 82% confidence — converged to 85% via cross-reviewer agreement
- **Performance Test Mislabeling** flagged by 2 reviewers (performance, testing) with 95% and 85% confidence — converged to 95% via strong first-reviewer assessment and second-reviewer confirmation
- **SearchError Naming** single reviewer but very high confidence (95%) — no convergence needed, finding is definitive
- **Testing Gaps** (bounds guards) flagged by 1 reviewer (testing) but represents two distinct test assertions that are independently tautological

**Deduplication Summary**:
- 4 HIGH findings after deduplication (from 5 raw findings with convergence boost)
- 10 MEDIUM findings after deduplication (from 13 raw with some convergence)
- 1 pre-existing issue flagged for tracking

---

## Key Strengths

1. **No regressions** - All exports additive, no API changes, `#[non_exhaustive]` on SearchError ensures backward compatibility
2. **Strong test coverage** - 30+ tests across 8 test cycles covering types, vocabulary, linearization, error handling, bounds, multi-language, edge cases, performance
3. **Excellent reliability** - MAX_AST_DEPTH (500), MAX_AST_NODES (100K), MAX_FILE_SIZE (100 KiB) guards prevent pathological inputs
4. **LazyLock initialization** - Thread-safe one-time init of per-language lookup tables is architecturally clean
5. **Allocation discipline** - Pre-allocated Vec, bounded traversal, no unbounded loops
6. **Zero unsafe code** - All operations in safe Rust
7. **Dependencies** - Both additions (tree-sitter, criterion) are justified and workspace-consistent; zero binary size impact for tree-sitter (already transitive)

---

## Action Plan

**Phase 1 (Blocking HIGH)**: Must resolve all 4 HIGH issues
1. Consolidate Parser/Grammar handling into LANG_MAPS and eliminate redundant instantiation
2. Replace parse-empty-source with direct grammar access via tree-sitter crate
3. Rename `AstError` to `Ast` in SearchError
4. Fix performance test to generate 1000 lines or rename to match reality
5. Change `linearize_tree` to return `LinearizeResult` directly (remove Result wrapper)

**Phase 2 (Should-Fix MEDIUM)**: Resolve consistency and reliability issues
1. Standardize `#[must_use]` to bare form (no custom message)
2. Combine `#![allow]` directives into single line
3. Add `ast_index` to lib.rs architecture doc
4. Fix doc comment "named" → "non-error"
5. Use `saturating_add` for node_count and error_count
6. Update ast_extract.rs to check `is_missing()` alongside `is_error()`
7. Fix bounds guard test assertions to actually validate the guards
8. Silence u16 truncation with explicit `try_from`

**Phase 3 (Informational)**: Note for future work
- Extract shared CST traversal primitive shared between ast_extract.rs and linearize.rs (separate PR)

---

## Confidence Summary by Category

| Category | Issues | Avg Confidence | Status |
|----------|--------|-----------------|--------|
| Blocking (HIGH) | 4 | 89% | Requires changes before merge |
| Should-Fix (MEDIUM) | 6 | 85% | Recommend fixing while here |
| Pre-existing | 1 | 85% | Note for future refactoring |

All findings apply ADR-001: surface issues immediately rather than defer. None are architectural blockers, but all represent clarity and consistency gaps worth resolving now.
