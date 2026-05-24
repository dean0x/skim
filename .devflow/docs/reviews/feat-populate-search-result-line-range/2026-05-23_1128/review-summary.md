# Code Review Summary

**Branch**: feat-populate-search-result-line-range -> main
**Date**: 2026-05-23_1128
**Reviewers**: 9 (architecture, complexity, consistency, performance, regression, reliability, rust, security, testing)

## Merge Recommendation: CHANGES_REQUESTED

The PR achieves its core goal of populating `ResolvedResult.line_range` with real 1-indexed line numbers. All existing tests pass and the new utility functions are well-tested. However, **two blocking HIGH-severity issues must be resolved before merge**: duplicate `byte_offset_to_line` implementations creating a maintenance hazard, and contradictory documentation about indexing conventions.

---

## Issue Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW | Total |
|----------|----------|------|--------|-----|-------|
| Blocking | 0 | 2 | 5 | 0 | **7** |
| Should Fix | 0 | 0 | 2 | 0 | **2** |
| Pre-existing | 0 | 0 | 2 | 0 | **2** |

---

## Convergence Status

Multiple reviewers (architecture, complexity, consistency, regression, rust) independently flagged the same core issues, indicating high confidence in findings:

| Issue | Reviewers Flagged | Base Confidence | Boosted Confidence |
|-------|------------------|-----------------|-------------------|
| Duplicate `byte_offset_to_line` | 6 reviewers | 82% | **92%** (+10% per reviewer) |
| Doc comment contradicts indexing | 5 reviewers | 92% | **100%** (capped) |
| `SnippetOutcome::Ok` tuple readability | 4 reviewers | 80% | **90%** |
| Missing JSON serialization test | 1 reviewer | 85% | **85%** |

---

## Blocking Issues

### HIGH — Duplicate `byte_offset_to_line` Creates Two Sources of Truth
**Location**: `crates/rskim-search/src/types.rs:351`, `crates/rskim/src/cmd/search/snippet.rs:47`
**Confidence**: 92% (6 reviewers)

**Problem**: 
Two `byte_offset_to_line` implementations now coexist with identical logic but different return types:
- Library version in `rskim-search/types.rs`: returns `usize`, uses `+ 1`
- CLI version in `snippet.rs`: returns `u32`, uses `saturating_add(1)`

Both are used in `snippet.rs`: the local `u32` version for `match_line` (line 160), while the library `usize` version is called indirectly via `compute_line_range` (line 162) for the same offset. This violates DRY and creates a latent regression vector: a bug fix to one will not propagate to the other.

**Fix**:
Delete the `snippet.rs` version and have `extract_snippet` call `rskim_search::byte_offset_to_line` instead, casting the result to `u32` at the call site:
```rust
// snippet.rs, line 160 — replace local call with library call
let match_line = rskim_search::byte_offset_to_line(&content, match_positions[0].start) as u32;
```
Then remove `pub(super) fn byte_offset_to_line` from `snippet.rs` entirely (tests in `snippet_tests.rs` already covered by library tests in `types.rs`).

---

### HIGH — Documentation Contradicts Indexing Convention
**Location**: `crates/rskim-search/src/types.rs:331`, `crates/rskim-search/src/types.rs:364`
**Confidence**: 100% (5 reviewers, multiple independent findings)

**Problem**:
- `SearchResult::line_range` doc comment (line 331) says: "0-indexed, exclusive end"
- `compute_line_range` doc comment (line 364) claims to return values "matching the convention used by `SearchResult::line_range`"
- Actual implementation: `byte_offset_to_line` returns **1-indexed** line numbers (`newlines + 1`)

This creates a false contract: callers reading the `SearchResult` doc would expect 0-indexed values, but `compute_line_range` returns 1-indexed. The CLI crate's `ResolvedResult` correctly uses 1-indexed convention, but the cross-reference creates active confusion at the library level.

**Fix**:
Update the `SearchResult::line_range` doc comment (line 331) to state the actual convention:
```rust
/// Source lines spanned by this match (1-indexed, exclusive end; 0..0 when not yet computed)
pub line_range: Range<usize>,
```
This aligns with the actual behavior and the `ResolvedResult` convention established by this PR.

---

## Should-Fix Issues

### MEDIUM — `SnippetOutcome::Ok` Tuple Grows Unwieldy
**Location**: `crates/rskim/src/cmd/search/snippet.rs:32`
**Confidence**: 90% (4 reviewers, consistency across domains)

**Problem**:
`SnippetOutcome::Ok(u32, Range<usize>, SnippetContext)` is a 3-field positional tuple. Every match site must destructure all three fields in order (e.g., `SnippetOutcome::Ok(ln, lr, ctx)`). There is no compiler help if the types are accidentally swapped in future changes (both start values are small integers). Adding a fourth field would make this worse.

**Fix**:
Convert to a named-field struct variant (two equivalent options):

Option A — Named fields in enum variant:
```rust
pub(super) enum SnippetOutcome {
    Ok {
        match_line: u32,
        line_range: Range<usize>,
        context: SnippetContext,
    },
    Stale,
    Unavailable,
}
// Usage: SnippetOutcome::Ok { match_line, line_range, context } => { ... }
```

Option B — Dedicated struct:
```rust
pub(super) struct SnippetMatch {
    pub match_line: u32,
    pub line_range: Range<usize>,
    pub context: SnippetContext,
}
pub(super) enum SnippetOutcome {
    Ok(SnippetMatch),
    Stale,
    Unavailable,
}
```

Either approach makes destructuring self-documenting and future-proof.

---

### MEDIUM — Missing JSON Serialization Test for `ResolvedResult.line_range`
**Location**: `crates/rskim/src/cmd/search/query_tests.rs`
**Confidence**: 85% (testing reviewer)

**Problem**:
The new `line_range: Option<Range<usize>>` field is documented as serializing to `{"start": N, "end": M}` in `--format json` output, but no test verifies this. The existing `test_format_json_output_is_valid_json` uses an empty results vec and never exercises serialization of the field. This is a critical gap: JSON consumers rely on this shape, and a serde misconfiguration would silently produce wrong output.

**Fix**:
Add two tests to `query_tests.rs`:
```rust
#[test]
fn test_format_json_with_line_range() {
    let result = ResolvedResult {
        line_range: Some(5..10),
        // ... other fields
    };
    let json = format_json_output(&[result]);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed[0]["line_range"]["start"], 5);
    assert_eq!(parsed[0]["line_range"]["end"], 10);
}

#[test]
fn test_format_json_with_missing_line_range() {
    let result = ResolvedResult {
        line_range: None,
        // ... other fields
    };
    let json = format_json_output(&[result]);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(parsed[0]["line_range"].is_null());
}
```

---

### MEDIUM — No Test for Behavioral Equivalence of `byte_offset_to_line` Duplicates
**Location**: `crates/rskim/src/cmd/search/snippet.rs:47`, `crates/rskim-search/src/types.rs:351`
**Confidence**: 80% (testing reviewer)

**Problem**:
Two implementations of `byte_offset_to_line` are tested independently, but nothing verifies they produce equivalent results. The snippet tests exercise the `u32` version, and the types tests exercise the `usize` version. They could diverge silently if one gets a bug fix the other doesn't.

**Fix**:
This becomes moot once the duplicate is removed (HIGH blocking issue above). If the CLI version is retained for some reason, add a property-style test:
```rust
#[test]
fn test_byte_offset_to_line_equivalence() {
    let content = b"hello\nworld\ntest";
    for offset in [0, 5, 6, 11, 15] {
        let usize_result = rskim_search::byte_offset_to_line(content, offset);
        let u32_result = snippet::byte_offset_to_line(content, offset) as usize;
        assert_eq!(usize_result, u32_result);
    }
}
```

---

## Pre-existing Issues (Informational)

### MEDIUM — `SearchResult::line_range` Doc Was Always Inaccurate
**Confidence**: 90%

The doc comment describing `line_range` as "0-indexed" pre-dates this PR. The field was initialized to the sentinel `0..0` and never actually populated with real values until now. This PR establishes the 1-indexed convention for the first time. Fixing the doc comment (HIGH blocking issue above) corrects this historical inaccuracy.

### MEDIUM — Performance: `byte_offset_to_line` Called Twice for Same Offset
**Location**: `crates/rskim/src/cmd/search/snippet.rs:160-162`
**Confidence**: 85%

For the first match position, `byte_offset_to_line` is called at line 160 for `match_line`, then again inside `compute_line_range` at line 162. This is two O(offset) scans of the same byte slice. For a 100KB offset, this means two 100KB scans instead of one. Mitigation: This is dwarfed by the file I/O that precedes it, and the 5MB size guard bounds the worst case. Performance reviewer rated this APPROVED_WITH_CONDITIONS (not blocking).

---

## Suggestions (Lower Confidence)

- **`compute_line_range` clones iterator to compute min/max separately** (65%) — Each call scans `content[..offset]` for newlines, and the function clones the iterator to compute `min()` and `max()` separately, resulting in `2 * N` calls instead of `N`. Use a single-pass fold to optimize. Low impact since match position counts are typically small (<20 per result).

- **`types.rs` exceeds recommended file length** (65%) — At 1,101 lines, the file exceeds the 500-line warning threshold. Consider extracting line-range utilities into a separate `line_utils.rs` module to keep `types.rs` focused on type definitions.

- **Consider a precomputed line-offset index for repeated conversions** (65%) — If `compute_line_range` is ever called on files with many match positions, a line-offset table with binary search would reduce from O(N * avg_offset) to O(content_len + N * log(lines)). Currently unnecessary given match position limits.

---

## Action Plan

**Before Merge** (blocking):
1. **Delete duplicate `byte_offset_to_line` from `snippet.rs`** — Call the library version with `as u32` cast at line 160. Update any tests.
2. **Fix `SearchResult::line_range` doc comment** — Update to "1-indexed, exclusive end; 0..0 when not yet computed".
3. **Convert `SnippetOutcome::Ok` to named fields** — Either use named-field enum variant or dedicated struct. Update all destructuring sites.

**Before Merge** (strongly recommended):
4. **Add JSON serialization tests** — Verify `line_range: Some(...)` and `line_range: None` round-trip correctly through `format_json_output`.

**Optional** (low-effort improvements):
5. **Single-pass fold for min/max in `compute_line_range`** — Reduce iterator cloning.

---

## Review Statistics

| Domain | Score | Confidence | Recommendation |
|--------|-------|-----------|-----------------|
| Architecture | 7/10 | 90% | CHANGES_REQUESTED |
| Complexity | 8/10 | 92% | CHANGES_REQUESTED |
| Consistency | 6/10 | 95% | CHANGES_REQUESTED |
| Performance | 8/10 | 85% | APPROVED_WITH_CONDITIONS |
| Regression | 8/10 | 82% | APPROVED_WITH_CONDITIONS |
| Reliability | 8/10 | 85% | APPROVED_WITH_CONDITIONS |
| Rust | 7/10 | 90% | CHANGES_REQUESTED |
| Security | 10/10 | 100% | APPROVED |
| Testing | 7/10 | 85% | CHANGES_REQUESTED |

**Average Score**: 7.7/10

---

## Key Strengths

✅ **Core logic is sound** — `byte_offset_to_line` and `compute_line_range` correctly compute 1-indexed line ranges with proper input validation.

✅ **Security clean** — No new I/O, no injection vectors, proper bounds checking with safe Rust.

✅ **Unit test coverage is thorough** — 12 new tests (7 for `byte_offset_to_line`, 5 for `compute_line_range`) cover edge cases and integration paths.

✅ **All existing tests pass** — No regressions in the broader codebase.

---

## Critical Fixes Required

The two HIGH blocking issues are quick to fix and necessary for merge:

1. **Duplicate `byte_offset_to_line`** — Delete the CLI version, call the library version with `as u32` cast. Impact: ~5 lines changed, eliminates maintenance hazard.

2. **Doc comment contradiction** — Update `SearchResult::line_range` doc to say "1-indexed" instead of "0-indexed". Impact: 1 doc string fixed, eliminates caller confusion.

Once these are addressed, the PR is ready for merge.
