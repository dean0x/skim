# Code Review Summary

**Branch**: feat/193-custom-field-mapping-json-yaml-toml -> main
**Date**: 2026-05-19_1726
**Reviewers**: 9 (security, architecture, performance, complexity, consistency, regression, testing, reliability, rust)

## Merge Recommendation: CHANGES_REQUESTED

**Reasoning**: The feature is architecturally sound and well-tested, but two HIGH-priority issues must be resolved before merge:

1. **JSON scanner `in_key_stack` unbounded growth** (flagged by 4 reviewers: security, performance, reliability, rust) - defense-in-depth depth limit needed
2. **TOML `find_toml_eq_sign` missing escape handling** (flagged by 3 reviewers: security, testing, rust) - correctness issue on edge-case inputs

Both are production-visible on real-world files and should be fixed. Additionally, 3 MEDIUM-priority issues in your changes need attention (stale documentation, missing test coverage, size guard bypass).

---

## Issue Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW | Total |
|----------|----------|------|--------|-----|-------|
| **Blocking** (Your changes) | 0 | 2 | 5 | 1 | **8** |
| **Should Fix** (Code you touched) | 0 | 0 | 2 | 0 | **2** |
| **Pre-existing** (Not blocking) | 0 | 0 | 1 | 0 | **1** |
| **TOTAL** | 0 | 2 | 8 | 1 | **11** |

---

## Blocking Issues (Must Fix Before Merge)

### HIGH Priority

#### 1. JSON scanner `in_key_stack` grows unboundedly with nesting depth
**Reviewers**: Security (82%), Performance (82%), Reliability (90%), Rust (62%)
**Confidence**: 89% (flagged by 4 reviewers, average confidence 79%)
**File**: `crates/rskim-search/src/fields/serde_fields.rs:68-77`

The `in_key_stack: Vec<bool>` pushes one entry per `{` character with no depth limit. For pathological input with deeply-nested braces (e.g., 50M nested objects within the 100 MiB `MAX_SOURCE_BYTES` limit), this allocates O(depth) heap memory and triggers excessive reallocations.

**Why it matters**: While upstream `MAX_SOURCE_BYTES` guard (100 MiB) limits total input, a deeply-nested file could allocate a 100 MB Vec. This violates the reliability rule: "every loop, retry, and resource has an explicit bound."

**Suggested fix**:
```rust
const MAX_JSON_DEPTH: usize = 1024;
// In b'{' match arm:
b'{' => {
    brace_depth += 1;
    if brace_depth <= MAX_JSON_DEPTH {
        in_key_stack.push(true);
    }
    i += 1;
}
```

#### 2. TOML `find_toml_eq_sign` does not handle escaped quotes
**Reviewers**: Security (80%), Testing (N/A), Reliability (82%), Rust (85%)
**Confidence**: 82% (flagged by 3 reviewers)
**File**: `crates/rskim-search/src/fields/serde_fields.rs:587-608`

The function tracks `in_str` state but does not skip `\"` (escaped quote) sequences inside basic strings. A TOML key like `"key\"=val"` would incorrectly find the `=` inside the string text, misclassifying the key/value boundary.

**Why it matters**: Silent misclassification of ranges affects BM25F field scoring for TOML files with quoted keys containing escapes (uncommon but valid).

**Suggested fix**:
```rust
fn find_toml_eq_sign(content: &[u8]) -> Option<usize> {
    let mut in_str = false;
    let mut str_char = b'"';
    let mut i = 0;
    while i < content.len() {
        let b = content[i];
        if in_str {
            if b == b'\\' && str_char == b'"' {
                i += 2; // skip escaped character
                continue;
            }
            if b == str_char {
                in_str = false;
            }
        } else {
            match b {
                b'"' | b'\'' => {
                    in_str = true;
                    str_char = b;
                }
                b'=' => return Some(i),
                b'#' => return None,
                _ => {}
            }
        }
        i += 1;
    }
    None
}
```

---

## Should-Address Issues (In Code You Touched)

### MEDIUM Priority

#### 3. `classify_markdown` can be called directly without size guard
**Reviewers**: Reliability (92%)
**Confidence**: 92%
**File**: `crates/rskim-search/src/fields/markdown.rs:43-101` and dispatcher at `classifier.rs:149-163`

`classify_markdown` is declared `pub(crate)` and called directly from tests. Any future caller could bypass the `MAX_SOURCE_BYTES` guard that lives in `classify_source`. The same applies to serde scanners (`classify_json`, `classify_yaml`, `classify_toml`). While the dispatcher guards all four formats, defense-in-depth would add bounds-checking inside each classifier.

**Suggested fix** (lightweight):
```rust
pub(crate) fn classify_markdown(source: &str) -> crate::Result<Vec<(Range<usize>, SearchField)>> {
    if source.is_empty() {
        return Ok(Vec::new());
    }
    if source.len() > crate::lexical::classifier::MAX_SOURCE_BYTES {
        return Err(crate::SearchError::FileTooLarge {
            size: source.len(),
            limit: crate::lexical::classifier::MAX_SOURCE_BYTES,
        });
    }
    // ... existing code
}
```

#### 4. Stale module doc comment in classifier.rs
**Reviewers**: Consistency (95%)
**Confidence**: 95%
**File**: `crates/rskim-search/src/lexical/classifier.rs:13-17`

Doc comment still claims: "JSON, YAML, TOML are classified as a single `SearchField::Other` range." This is now factually wrong -- they are dispatched to format-specific classifiers producing rich field mappings.

**Fix**: Update to describe the new dispatch behavior:
```rust
//! # Format-specific languages
//!
//! JSON, YAML, and TOML are dispatched to dedicated scanners in
//! [`crate::fields::serde_fields`] before the tree-sitter path. Markdown
//! uses a custom tree-sitter classifier in [`crate::fields::markdown`].
//! These produce format-appropriate field classifications (TypeDefinition,
//! SymbolName, StringLiteral, etc.) instead of a single `Other` range.
```

#### 5. Stale comments in boundary test
**Reviewers**: Consistency (90%)
**Confidence**: 90%
**File**: `crates/rskim-search/src/lexical/classifier_tests.rs:87,90-92`

Test `test_source_at_limit_boundary_does_not_error` has misleading comments: "it returns a single Other range without touching the parser" and "Json parser returns an error (unsupported for tree-sitter)" are both wrong now.

**Fix**: Update to reflect new behavior:
```rust
// We use JSON so this stays fast even at 100 MiB;
// the format-specific scanner classifies without tree-sitter overhead.
let at_limit = " ".repeat(MAX_SOURCE_BYTES);
let result = classify_source(&at_limit, rskim_core::Language::Json);
// The size guard must not fire at exactly MAX_SOURCE_BYTES.
```

---

## Informational Issues (Not Blocking)

### MEDIUM Priority

#### 6. Inconsistent error model (intentional design choice)
**Reviewers**: Architecture (82%), Consistency (85%)
**Confidence**: 83%
**File**: `serde_fields.rs` vs `markdown.rs`

Serde scanners return `Vec` (infallible), Markdown returns `Result<Vec>` (fallible). This is documented as intentional (tree-sitter can theoretically fail), but `classify_markdown` never actually returns `Err` in practice.

**Recommendation**: Document that `classify_markdown` is fault-tolerant (catches errors internally). The asymmetry is acceptable since it matches the Strategy Pattern's dispatch model.

#### 7. YAML newline in StringLiteral range
**Reviewers**: Rust (82%)
**Confidence**: 82%
**File**: `crates/rskim-search/src/fields/serde_fields.rs:347`

For quoted YAML values like `name: "skim"\n`, the range includes the trailing newline. The newline gets boosted with `StringLiteral` field weight, slightly skewing BM25F scores.

**Fix**: Trim trailing newline before pushing:
```rust
let mut str_end = line_end;
if str_end > actual_val_start && bytes[str_end - 1] == b'\n' {
    str_end -= 1;
}
ranges.push((actual_val_start..str_end, SearchField::StringLiteral));
```

#### 8. TOML `eol + 1` pattern when `eol == len`
**Reviewers**: Reliability (85%), Rust (80%)
**Confidence**: 83%
**File**: `crates/rskim-search/src/fields/serde_fields.rs:435,450,489`

When TOML source has no trailing newline, `eol` becomes `len` via `.unwrap_or(len)`. Then `i = eol + 1` sets `i` to `len + 1`. The loop guard `while i < len` exits safely, but this is a fragile pattern. Suggested: `i = (eol + 1).min(len)` for clarity.

#### 9. Missing test: TOML triple-quoted strings
**Reviewers**: Testing (88%)
**Confidence**: 88%
**File**: `crates/rskim-search/src/fields/serde_fields.rs:520-558` (production) vs `fields_tests.rs` (no test)

Production code handles triple-quoted strings (`"""..."""`, `'''...'''`) with escape handling, but no test exercises this path. This is non-trivial parser logic that could silently regress.

**Fix**: Add coverage for both triple-quote variants (examples in testing.md review).

### LOW Priority

#### 10. `classify_json` function length and nesting depth
**Reviewers**: Complexity (85%)
**Confidence**: 85%
**File**: `crates/rskim-search/src/fields/serde_fields.rs:49-174`

Function is 125 lines with 5 levels of nesting in key classification. The look-ahead logic (lines 123-155) is a 32-line block embedded inside a conditional.

**Recommendation**: Extract look-ahead into `classify_json_key_at_depth0` helper to reduce nesting and improve readability (suggested in complexity.md review).

#### 11. `classify_yaml` cognitive load
**Reviewers**: Complexity (82%)
**Confidence**: 82%
**File**: `crates/rskim-search/src/fields/serde_fields.rs:239-358`

Function is 119 lines handling blank lines, comments, markers, list-item stripping, and value scanning. The list-item destructuring (lines 289-302) produces an unnamed 3-tuple.

**Recommendation**: Extract list-prefix stripping into `strip_list_prefix` helper (suggested in complexity.md review).

---

## Strengths Noted Across Reviewers

1. **Architectural discipline**: Strategy Pattern dispatch correctly routes to format-specific classifiers. Separation of concerns is clean (`fields/` module is self-contained). No circular dependencies.

2. **Infallibility contract**: Serde scanners are infallible (`return Vec`), handling malformed input gracefully. This prevents new error paths.

3. **Test quality**: Comprehensive coverage with behavior-focused assertions (47 field tests + 17 classifier tests). Contiguity and invariant testing for all formats. Test updates (manifest tests) correctly use `FileManifest::FORMAT_VERSION` instead of hardcoded values.

4. **FORMAT_VERSION bump**: Correctly forces re-indexing. Tests verify v1 manifests are rejected (cold-start pattern).

5. **Defense-in-depth**: `MAX_SOURCE_BYTES` guard at dispatcher entry. `fill_gaps_and_merge` clamps ranges to source bounds. No hardcoded secrets or injection vectors.

6. **Well-documented**: Module docs, doc comments on scanner functions, field mapping table, and design rationale in PR description are clear and thorough.

---

## Recommendation Details

**Current State**: 2 HIGH-priority blocking issues + 3 MEDIUM documentation/test issues.

**Path to Approval**:
1. Add `MAX_JSON_DEPTH` constant and depth guard to JSON scanner (HIGH)
2. Fix escape handling in `find_toml_eq_sign` (HIGH)
3. Update doc comment in `classifier.rs` (MEDIUM)
4. Update stale test comments (MEDIUM)
5. Add size guard to `classify_markdown` (MEDIUM)
6. Add test coverage for TOML triple-quoted strings (MEDIUM)

After these fixes, the PR will be **APPROVED**. The feature is architecturally sound and production-ready; these are polish and defense-in-depth improvements.

---

## Reviewer Scores Summary

| Reviewer | Category | Score | Recommendation |
|----------|----------|-------|-----------------|
| Security | Infallible scanners, size guard | 8/10 | APPROVED_WITH_CONDITIONS |
| Architecture | Strategy Pattern, separation of concerns | 9/10 | APPROVED |
| Performance | O(n) algorithms, allocation patterns | 8/10 | APPROVED_WITH_CONDITIONS |
| Complexity | Extract helpers from JSON/YAML | 7/10 | APPROVED_WITH_CONDITIONS |
| Consistency | Stale docs, test naming | 8/10 | APPROVED_WITH_CONDITIONS |
| Regression | H4+ heading downgrade (intentional), no exports removed | 9/10 | APPROVED_WITH_CONDITIONS |
| Testing | Triple-quoted strings, empty Markdown | 7/10 | APPROVED_WITH_CONDITIONS |
| Reliability | Depth guard, size guard bypass | 7/10 | CHANGES_REQUESTED |
| Rust | Escape handling, Vec patterns | 8/10 | CHANGES_REQUESTED |

**Consensus**: Feature is solid (architecture 9/10, regression 9/10) but needs 2 HIGH fixes (reliability + rust) before merge.
