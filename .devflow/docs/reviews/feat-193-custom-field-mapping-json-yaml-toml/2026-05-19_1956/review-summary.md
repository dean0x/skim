# Code Review Summary

**Branch**: feat/193-custom-field-mapping-json-yaml-toml -> main
**Date**: 2026-05-19_1956
**Scope**: Incremental review (commits 13e13e9..0468ade)

## Merge Recommendation: CHANGES_REQUESTED

Two HIGH-severity blocking issues (missing test coverage) and two MEDIUM-severity blocking issues (correctness bugs) prevent merge. All issues are fixable. After fixes, PR is mergeable.

## Issue Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW | Total |
|----------|----------|------|--------|-----|-------|
| Blocking | 0 | 2 | 2 | 0 | **4** |
| Should Fix | 0 | 0 | 2 | 0 | **2** |
| Pre-existing | 0 | 0 | 3 | 0 | **3** |

---

## Blocking Issues

### HIGH Severity

#### 1. Missing test for MAX_JSON_DEPTH cap behavior
**File**: `crates/rskim-search/src/fields/serde_fields.rs:62`
**Confidence**: 85%
**Reviewers**: Testing (1)

The new `MAX_JSON_DEPTH` constant (1024) introduces a behavioral branch (line 83-85) where `in_key_stack` stops tracking state. No test validates this code path. Existing F-JSON-06 deep nesting test only goes 3 levels deep, far below the 1024 threshold.

**Fix**: Add test that constructs JSON nested to 1024+ levels and verifies:
- No panic
- Contiguous output ranges
- Ranges sum to source length

Example test pattern:
```rust
#[test]
fn f_json_09_depth_beyond_max_json_depth_cap() {
    let depth = 1024 + 10;
    let mut source = String::new();
    for _ in 0..depth {
        source.push_str("{\"k\":");
    }
    source.push_str("\"v\"");
    for _ in 0..depth {
        source.push('}');
    }
    let ranges = classify_json(&source);
    assert_contiguous(&ranges, source.len());
    assert_field_lengths_sum(&ranges, source.len());
}
```

#### 2. Missing test for YAML newline-trimming fix
**File**: `crates/rskim-search/src/fields/serde_fields.rs:345-351`
**Confidence**: 82%
**Reviewers**: Testing (1)

The YAML scanner now trims trailing `\n` from quoted string values (behavioral change, bugfix). Existing test F-YAML-03 passes but does not explicitly verify that the StringLiteral range *excludes* the newline byte. The boundary condition is untested.

**Fix**: Add test that explicitly verifies StringLiteral does not include trailing newline:
```rust
#[test]
fn f_yaml_07_quoted_string_excludes_trailing_newline() {
    let source = "name: \"skim\"\n";
    let ranges = classify_yaml(source);
    assert_contiguous(&ranges, source.len());
    let str_ranges: Vec<_> = ranges.iter()
        .filter(|(_, f)| *f == SearchField::StringLiteral)
        .collect();
    assert!(!str_ranges.is_empty(), "should have StringLiteral");
    for (r, _) in &str_ranges {
        let text = &source[r.clone()];
        assert!(!text.ends_with('\n'),
            "StringLiteral must not include trailing newline; got: {text:?}");
    }
}
```

---

### MEDIUM Severity

#### 3. JSON depth cap causes incorrect key/value classification (BLOCKING)
**File**: `crates/rskim-search/src/fields/serde_fields.rs:79-91`
**Confidence**: 100% (5 reviewers: Security 82%, Architecture 82%, Regression 82%, Reliability 82%, Rust 82% → boosted to 100%)
**Reviewers**: Security, Architecture, Regression, Reliability, Rust

When `brace_depth > 1024`, the `{` handler skips `in_key_stack.push(true)` (line 83-85), but the `}` handler unconditionally calls `in_key_stack.pop()` (line 90). For pathologically deep JSON (>1024 nesting levels), closing braces will pop entries belonging to shallower (still-open) scopes, causing key/value classification misalignment.

Example: JSON nested to depth 1025 unwinds to depth 1024, stack is empty, all keys at depths 1-1024 are misclassified as values (`in_key_stack.last()` returns `None` → `unwrap_or(false)`).

**Impact**: BM25F scoring accuracy degradation on adversarial input (unlikely in practice, but violates correctness contract).

**Fix**: Guard the pop symmetrically with the push:
```rust
b'}' => {
    if brace_depth <= MAX_JSON_DEPTH {
        in_key_stack.pop();
    }
    brace_depth = brace_depth.saturating_sub(1);
    i += 1;
}
```

Note: The `brace_depth` comparison must happen *before* the `saturating_sub` so the condition mirrors the push (line 83).

#### 4. YAML newline trim does not handle `\r\n` line endings (BLOCKING)
**File**: `crates/rskim-search/src/fields/serde_fields.rs:348-351`
**Confidence**: 90% (3 reviewers: Consistency 82%, Regression 80%, Reliability 80% → boosted to 90%)
**Reviewers**: Consistency, Regression, Reliability

The newline trim (lines 348-350) only strips trailing `\n`. On Windows-style `\r\n` line endings, the `\r` byte remains inside the StringLiteral range after the `\n` is trimmed. This `\r` receives StringLiteral BM25F boost weight — the same score-skewing issue the trim was introduced to fix, just for a different whitespace byte. Inconsistent with other parts of the codebase that handle both `\r` and `\n` (lines 274, 277, 383, 406, 461).

**Impact**: BM25F scoring inaccuracy on YAML files with CRLF line endings.

**Fix**: After stripping `\n`, also strip `\r`:
```rust
let mut str_end = line_end;
if str_end > actual_val_start && bytes[str_end - 1] == b'\n' {
    str_end -= 1;
}
if str_end > actual_val_start && bytes[str_end - 1] == b'\r' {
    str_end -= 1;
}
if str_end > actual_val_start {
    ranges.push((actual_val_start..str_end, SearchField::StringLiteral));
}
```

---

## Should-Fix Issues

### MEDIUM Severity

#### 5. Missing test for TOML escape fix in find_toml_eq_sign
**File**: `crates/rskim-search/src/fields/serde_fields.rs:625-634`
**Confidence**: 83%
**Reviewers**: Testing (1)
**Category**: Should Fix

`find_toml_eq_sign` was refactored from `for` loop to `while` loop to handle backslash escapes in double-quoted TOML keys (e.g., `"key=\"with=equals"` should not treat the internal `=` as the separator). New triple-quote tests (F-TOML-07-10) test multi-line strings but don't exercise this specific fix with escaped characters in single-line keys.

**Fix**: Add targeted test:
```rust
#[test]
fn f_toml_11_escaped_eq_in_quoted_key() {
    let source = "\"path=here\" = \"value\"\n";
    let ranges = classify_toml(source);
    assert_contiguous(&ranges, source.len());
    let sym_texts = field_text(source, &ranges, SearchField::SymbolName);
    assert!(
        sym_texts.iter().any(|t| t.contains("path=here")),
        "quoted key with = should be a single SymbolName; sym texts: {sym_texts:?}"
    );
}
```

#### 6. Missing test for classify_markdown size guard
**File**: `crates/rskim-search/src/fields/markdown.rs:50-55`
**Confidence**: 80%
**Reviewers**: Testing (1)
**Category**: Should Fix (lower priority)

`classify_markdown` added a `MAX_SOURCE_BYTES` size guard (lines 50-55) that returns `SearchError::FileTooLarge`. Existing size-guard test uses JSON, not Markdown. No unit test validates the Markdown classifier's own size guard in isolation.

**Mitigation**: `classify_source` dispatcher calls `classify_markdown` and has its own guard (same constant), so the Markdown guard is defense-in-depth. Blocking is not required, but a unit test would confirm correctness if dispatch path changes.

---

## Pre-existing Issues (Not Blocking)

### MEDIUM Severity

1. **`classify_yaml` function length: 115 lines** — Exceeds 50-line threshold. Pre-existing; incremental changes (list prefix extraction, newline trimming) actually improved this. Suggested for future refactor: extract key-detection block into `classify_yaml_key_value_line()`.

2. **`classify_toml` function length: 93 lines** — Exceeds 50-line threshold. Pre-existing. Suggested for future refactor: extract key-value arm into `classify_toml_kv_line()`.

3. **`classify_json` function length: 99 lines** — Exceeds 50-line threshold. Pre-existing; incremental diff extracted `classify_json_key_at_depth0`, which was the right move.

---

## Positive Observations

1. **Extract Method refactorings (SRP)**: `classify_json_key_at_depth0`, `strip_list_prefix`, `skip_json_whitespace` are clean extractions that reduce function length and clarify intent.

2. **Bounded resource usage**: `MAX_JSON_DEPTH` cap (1024) prevents unbounded heap growth on adversarial input. Constant is reasonable (RFC 7159 doesn't mandate a limit; most parsers cap at 512-1024). `(eol + 1).min(len)` bounds fix potential off-by-one at EOF.

3. **Consistent Strategy Pattern**: Size guard added to `classify_markdown` mirrors guard in `classify_source`, maintaining symmetry. Reuses `MAX_SOURCE_BYTES` constant (DRY).

4. **TOML escape handling**: Conversion to `while` loop with `i += 2` skip for `\\` escapes correctly implements TOML spec (basic strings support escapes, literal strings don't).

5. **Test quality**: New TOML triple-quote tests (F-TOML-07-10) follow established naming convention, use shared helpers (`assert_contiguous`, `assert_field_lengths_sum`), test meaningful edge cases (embedded quotes, backslash), and include diagnostic assertion messages.

6. **Performance**: All changes maintain <50ms target for 1000-line files. Byte-by-byte and line-by-line scanners are O(n) with small constant factors. No N+1 patterns, no unbounded allocations.

---

## Action Plan

**Before Merge:**
1. Fix JSON depth cap asymmetry (Issue #3) — guard the pop at line 90
2. Fix YAML `\r\n` trimming (Issue #4) — add `\r` strip after `\n` strip
3. Add test for MAX_JSON_DEPTH (Issue #1) — test >1024 nesting
4. Add test for YAML newline trim (Issue #2) — assert StringLiteral excludes newline

**Recommended (not blocking):**
5. Add test for TOML escape fix (Issue #5) — targeted test with `=` in quoted key
6. Verify `classify_markdown` size guard test (Issue #6) — optional unit test

**Post-Fix Merge:** All blocking conditions cleared. Architecture, performance, and consistency are solid. Test coverage will be complete.
