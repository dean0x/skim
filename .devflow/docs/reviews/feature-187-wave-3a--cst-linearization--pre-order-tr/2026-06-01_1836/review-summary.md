# Code Review Summary

**Branch**: feature/187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01 18:36
**Review Cycle**: 3 (convergence phase after Cycle 2 fixes)

## Merge Recommendation: CHANGES_REQUESTED

The foundation is excellent — security is pristine (10/10), reliability is strong (9/10), regression testing passes all checks (9/10). However, two HIGH test findings require addressing before merge. Both are fixable in 30 minutes.

---

## Issue Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW | Total |
|----------|----------|------|--------|-----|-------|
| Blocking (Your Changes) | 0 | 2 | 2 | 0 | 4 |
| Should Fix (Code You Touched) | 0 | 0 | 5 | 0 | 5 |
| Pre-existing (Not Blocking) | 0 | 0 | 2 | 0 | 2 |
| **TOTALS** | **0** | **2** | **9** | **0** | **11** |

---

## Blocking Issues (Must Fix Before Merge)

### HIGH (Confidence: 85%) — Hardcoded Vocabulary Size Assertion Couples Test to Data File

**Location**: `crates/rskim-search/src/ast_index/linearize_tests.rs:82`

**Issue**: 
```rust
#[test]
fn vocabulary_has_1740_entries() {
    assert_eq!(NODE_KIND_VOCABULARY.len(), 1740);
}
```

The assertion hardcodes `1740`, a snapshot of the generated `ast_weights.rs` file (105K lines). Any future vocabulary regeneration (e.g., adding a language grammar, re-running `ast_codegen`) breaks this test even though linearization logic is unchanged. This tests an implementation detail rather than behavior.

**Fix** (Suggested):
```rust
#[test]
fn vocabulary_is_non_empty_and_within_u16_range() {
    assert!(!NODE_KIND_VOCABULARY.is_empty(), "vocabulary must not be empty");
    assert!(
        NODE_KIND_VOCABULARY.len() <= u16::MAX as usize,
        "vocabulary must fit in u16 index space"
    );
}
```

**Why it Matters**: Brittle tests that snapshot implementation details create false negatives during routine maintenance, eroding confidence in the test suite. The behavioral property (vocabulary exists and is indexable) is what matters, not the exact size.

---

### HIGH (Confidence: 82%) — Performance Test Uses Single Wall-Clock Sample

**Location**: `crates/rskim-search/src/ast_index/linearize_tests.rs:428-449`

**Issue**:
```rust
#[test]
#[cfg(not(debug_assertions))]
fn linearize_1000_line_file_under_10ms() {
    let source = gen_rust_fns(1000); // ~55KB
    let start = Instant::now();
    let _ = parse_and_linearize(&source, Language::Rust);
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_millis() < 10,
        "linearize_source took {}ms, expected < 10ms",
        elapsed.as_millis()
    );
}
```

Single-sample wall-clock measurements are inherently flaky on shared CI runners. The `#[cfg(not(debug_assertions))]` guard helps, but one sample is non-deterministic. The project already has rigorous Criterion benchmarks (`linearize_bench.rs`) that cover this case with statistical validity.

**Fix** (Either option):
1. **Remove the test** (Criterion benchmarks already validate this)
2. **Add multi-sample percentile check**:
```rust
let mut times = Vec::with_capacity(5);
for _ in 0..5 {
    let start = Instant::now();
    let _ = parse_and_linearize(&source, Language::Rust);
    times.push(start.elapsed());
}
times.sort();
let median = times[2]; // p50
assert!(
    median.as_millis() < 10,
    "median linearize_source took {}ms, expected < 10ms",
    median.as_millis()
);
```

**Why it Matters**: Flaky tests on CI erode developer confidence. If this test occasionally fails under load (creating false positives), it becomes ignored or disabled, defeating its purpose.

---

### MEDIUM (Confidence: 85%) — Unknown Kind Sentinel Path Not Exercised

**Location**: `crates/rskim-search/src/ast_index/linearize_tests.rs:384-397`

**Issue**:
The test `unknown_kind_emits_sentinel_zero` attempts to validate the sentinel emission logic but doesn't actually exercise it:

```rust
#[test]
fn unknown_kind_emits_sentinel_zero() {
    // Comment: "We can't easily inject an unknown kind"
    // Then checks if kind_id == 0 exists, assert vocabulary[0] == ""
    // This is tautological on the static vocabulary, not a test of sentinel logic
}
```

The test passes even if no node has `kind_id == 0`, making the assertion vacuously true.

**Fix** (Suggested):
```rust
#[test]
fn lang_map_unknown_kind_resolves_to_sentinel() {
    let maps = &*LANG_MAPS;
    let rust_map = maps.get(&Language::Rust).expect("Rust must have a lang map");
    // Test an out-of-bounds index (beyond any valid kind_id)
    let out_of_bounds = rust_map.len();
    let result = rust_map.get(out_of_bounds).copied().flatten().unwrap_or(0);
    assert_eq!(result, 0, "out-of-bounds kind_id must resolve to sentinel 0");
    assert_eq!(NODE_KIND_VOCABULARY[0], "", "sentinel ID 0 must map to empty string");
}
```

---

### MEDIUM (Confidence: 90%) — Test `#![allow]` Pattern Inconsistency

**Location**: `crates/rskim-core/src/ast_walk.rs:265`, `crates/rskim-search/src/ast_index/linearize_tests.rs:13`, `crates/rskim-research/src/ast_extract.rs:370`

**Issue**:
Three files use:
```rust
#![allow(clippy::unwrap_used, clippy::expect_used)]
```

The remaining 38 test files consistently use only `#![allow(clippy::unwrap_used)]`.

The `clippy::expect_used` addition is necessary when the code uses `.expect()` (given `expect_used = "deny"` in lint config), but the inconsistency creates ambiguity for future contributors — should they add `expect_used` or avoid `.expect()`?

**Fix**:
Adopt the new pattern project-wide. The new pattern (using `.expect()` with descriptive messages) is better practice than `.unwrap()`. Either:
1. Add `clippy::expect_used` to all 38 existing test files' allow lists (consistency)
2. Update the new 3 files to use `.unwrap()` instead of `.expect()` (conformity to legacy)

**Recommendation**: Option 1 (upgrade all to use `.expect()`). The new pattern is clearer and should be the standard.

---

## Should Fix Issues (Category 2 — Recommend Addressing)

### MEDIUM (Confidence: 82%) — Missing SQL `MAX_FILE_SIZE_LARGE` Override

**Location**: `crates/rskim-search/src/ast_index/linearize.rs:50`

**Issue**: 
The sibling `ast_extract.rs` (in rskim-research) uses `MAX_FILE_SIZE_LARGE = 1024 * 1024` (1 MiB) for SQL files because SQL migrations/schema dumps are routinely larger than 100 KiB. The new `linearize.rs` uses a flat `MAX_FILE_SIZE = 100 * 1024` for all languages.

Result: SQL files between 100 KiB and 1 MiB will produce bigrams in `ast_extract` but empty linearization results, creating a downstream inconsistency in the search index.

**Fix**:
```rust
const MAX_FILE_SIZE_LARGE: usize = 1024 * 1024;

// In linearize_source:
let size_limit = match language {
    Language::Sql => MAX_FILE_SIZE_LARGE,
    _ => MAX_FILE_SIZE,
};
if source.len() > size_limit {
    return Ok(LinearizeResult::default());
}
```

**Priority**: Low-to-medium (affects SQL indexing consistency, but SQL files 100-1000 KiB are not common).

---

### MEDIUM (Confidence: 82%) — tree-sitter Direct Dependency Architectural Pattern

**Location**: `crates/rskim-search/Cargo.toml:20`

**Issue**:
`rskim-search` adds `tree-sitter = { workspace = true }` as a direct dependency. The established pattern (per `rskim-research/Cargo.toml`) is to access tree-sitter types through `rskim-core` re-exports, not as a direct dependency.

Current approach creates hidden coupling: a tree-sitter major bump must be coordinated across both `rskim-core` and `rskim-search` simultaneously, rather than being handled once in `rskim-core`.

**Fix** (Suggested):
Remove the direct `tree-sitter` dependency from `rskim-search/Cargo.toml`. Re-export the types needed by `linearize.rs` from `rskim-core`:
- `tree_sitter::Tree`
- `tree_sitter::Language`

**Note**: Confidence is 82% (not higher) because there may be intentional reasons to keep the direct dep (e.g., accessing `tree_sitter::Language::node_kind_for_id` which `rskim-core` doesn't wrap). If this is deliberate, document it in a comment.

**Priority**: Medium (architectural consistency and coupling concern, but not a blocking issue if design is deliberate).

---

### MEDIUM (Confidence: 85%) — Benchmark Comment Misleading About Parsing

**Location**: `crates/rskim-search/benches/linearize_bench.rs:94-101`

**Issue**:
The benchmark comment says "Parsing happens in setup (outside b.iter()) — benchmark linearization only", but this is false. `linearize_source` calls `parser.parse(source)` internally, so `b.iter()` includes both parse time AND linearization time.

Result: Benchmark results conflate parsing latency with traversal, making it impossible to attribute regressions.

**Fix**:
Update the comment to be accurate:
```rust
// Benchmarks end-to-end linearize_source, including parsing overhead.
// To isolate linearization traversal from parsing, use linearize_tree() directly.
```

**Priority**: Low (documentation/clarity, not a functional issue).

---

### MEDIUM (Confidence: 80%) — Error Node Chain-Break Test Uses Indirect Proof

**Location**: `crates/rskim-research/src/ast_extract.rs:492-541`

**Issue**:
The test `error_node_breaks_ancestor_chain_for_descendants` compares bigram counts between clean and broken source to prove the chain-break. This proof is sensitive to tree-sitter grammar versions — a grammar update could change how error recovery works, breaking the assumption.

**Fix**:
Keep the indirect comparison (still valid as a behavioral test), but add a direct assertion:
```rust
// At minimum: the broken parse must NOT have zero bigrams
assert!(!result_broken.bigrams.is_empty(), "broken source should still produce some bigrams");
```

**Priority**: Low (the test is valid as written; this is a hardening suggestion).

---

### MEDIUM (Confidence: 83%) — Ancestor Vec Resizes One Element at a Time

**Location**: `crates/rskim-research/src/ast_extract.rs:148-149`

**Issue**:
The ancestor vector resizes to exactly `depth + 1` on each new depth level, triggering reallocations. With typical depths of 20-30, this is not a concern. With pathological inputs reaching 500, it causes ~3 reallocations (allocator's doubling strategy), which is still negligible.

**Status**: Acknowledged as an acceptable trade-off. No fix needed (the current approach is optimal). Raised for completeness per ADR-001.

**Priority**: Informational (no action needed).

---

## Pre-existing Issues (Category 3 — Informational Only)

### MEDIUM (Confidence: 82%) — `collect_import_names` Nesting Depth Reaches 4 Levels

**Location**: `crates/rskim-bench/src/extract/typescript.rs:91-136`

**Issue**: Nesting depth of 4 levels (warning threshold per complexity metrics) driven by tree-sitter AST shape.

**Note**: This is in `rskim-bench` (benchmark support code), not production. Pre-existing, not introduced by this PR.

---

### MEDIUM (Confidence: 70%) — MISSING Node Behavioral Change

**Location**: `crates/rskim-research/src/ast_extract.rs:152`

**Issue**: The refactored `AstWalkIter` excludes MISSING nodes (via `node.is_missing()` check), whereas the old code treated them as normal nodes. This changes bigram counts slightly but is an improvement (MISSING nodes represent parse artifacts, not real grammar structure).

**Note**: All tests pass. This is a behavioral improvement, not a regression. Raised for visibility in case downstream consumers depend on exact historical bigram counts.

---

## Convergence Status (Cross-Cycle)

**Cycle 2 Resolutions**: 11 issues, 9 fixed, 0 FP, 0 deferred
- ✅ Centralized bounds constants
- ✅ Pre-allocated level_stack
- ✅ Fused iterator implementation
- ✅ Strengthened error tests
- ✅ `#[must_use]` on accessor methods
- ✅ Lazy-grow ancestor vec
- ✅ Chain-break test
- ✅ Re-exports restored

**Cycle 3 Verification**:
- ✅ All 9 Cycle 2 fixes remain intact
- ✅ No regressions detected
- ⚠️ 4 new issues in test quality / 5 in code patterns identified
- ⚠️ 2 pre-existing issues surfaced for reference

---

## Quality Scorecard

| Focus | Score | Status |
|-------|-------|--------|
| **Security** | 10/10 | ✅ APPROVED — Snyk SAST clean, all bounds guards in place |
| **Reliability** | 9/10 | ✅ APPROVED — All loops bounded, invariants enforced |
| **Regression** | 9/10 | ✅ APPROVED — All prior fixes verified, no regressions |
| **Rust** | 9/10 | ✅ APPROVED — Excellent ownership, error handling, type system usage |
| **Performance** | 8/10 | ⚠️ APPROVED_WITH_CONDITIONS — Parser re-creation per call (HIGH, deferred to batch API) |
| **Architecture** | 9/10 | ⚠️ APPROVED_WITH_CONDITIONS — tree-sitter direct dependency pattern (MEDIUM) |
| **Consistency** | 8/10 | ⚠️ APPROVED_WITH_CONDITIONS — SQL file size, test allow patterns (MEDIUM) |
| **Complexity** | 9/10 | ✅ APPROVED — Well-factored, appropriate nesting, no duplicated logic |
| **Dependencies** | 10/10 | ✅ APPROVED — No new crates, workspace pinned, no CVEs |
| **Testing** | 8/10 | ⚠️ **CHANGES_REQUESTED** — 2 HIGH test quality issues (vocabulary hardcoding, flaky wall-clock test) |

---

## Action Plan (Before Merge)

**BLOCKING (Fix These):**
1. Replace hardcoded vocabulary size assertion with behavioral test (vocabulary non-empty, within u16 range)
2. Either remove single-sample wall-clock test or add multi-sample percentile check

**HIGH PRIORITY (Fix Together):**
3. Fix misleading benchmark comment (clarify parsing is included)
4. Add direct assertion to sentinel test to exercise the actual codepath

**MEDIUM PRIORITY (Before Merge or in Follow-Up):**
5. Add SQL `MAX_FILE_SIZE_LARGE` override for consistency with ast_extract.rs
6. Consider tree-sitter re-export pattern (or document deliberate divergence)
7. Upgrade test allow pattern project-wide to include `clippy::expect_used`

**DEFERRED (Design Phase):**
- Parser re-creation per call (acceptable to defer; design consideration for batch indexing API)

---

## Summary

**Strength**: Excellent foundation — zero security issues, strong reliability, clean refactor with no regressions. The `AstWalkIter` extraction cleanly eliminates duplication and the test suite is thorough.

**Gap**: Test quality issues (hardcoded snapshots, flaky timing) are fixable in 30 minutes. These are the only blockers.

**Recommendation**: **CHANGES_REQUESTED** — Fix the two HIGH test findings and address the MEDIUM issues listed above, then resubmit. The code itself is solid; polish the test suite and merge.
