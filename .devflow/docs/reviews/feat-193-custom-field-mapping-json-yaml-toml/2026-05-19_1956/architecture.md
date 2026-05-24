# Architecture Review Report

**Branch**: feat/193-custom-field-mapping-json-yaml-toml -> main
**Date**: 2026-05-19
**Scope**: Incremental review of commits 13e13e9 and 0468ade (2 commits since last review)

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Asymmetric depth-tracking for `{` push vs `}` pop in JSON scanner** - `serde_fields.rs:79-91`
**Confidence**: 82%
- Problem: On `{` open, the stack push is guarded by `if brace_depth <= MAX_JSON_DEPTH` (line 83), so beyond depth 1024 no entry is pushed. However, on `}` close (line 90), `in_key_stack.pop()` is called unconditionally. For well-formed JSON this is fine because pops will be `None` once the stack is empty (Vec::pop returns Option). But for malformed JSON with extra `}` characters at shallow depth, a pop could remove a stack entry that belongs to a different nesting level, causing subsequent key/value classification to be wrong.

  The current code is safe from panics (Vec::pop on empty returns None, and brace_depth uses saturating_sub), and well-formed JSON is handled correctly because the stack and brace_depth remain in sync for depth <= 1024. The scenario requires both depth > 1024 AND malformation, which is unlikely in practice. Flagged as MEDIUM because the asymmetry between guarded push and unguarded pop is an architectural smell that could surprise future maintainers.

- Fix: Guard the pop symmetrically with the push:
  ```rust
  b'}' => {
      if brace_depth <= MAX_JSON_DEPTH {
          in_key_stack.pop();
      }
      brace_depth = brace_depth.saturating_sub(1);
      i += 1;
  }
  ```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **YAML `\r\n` newline trimming incomplete** - `serde_fields.rs:348-349` (Confidence: 65%) -- The new newline-trimming logic for YAML quoted string values only strips a trailing `\n` byte. On Windows-style line endings (`\r\n`), the `\r` would remain inside the StringLiteral range. This is cosmetic (slightly inflates the range by 1 byte) and unlikely on YAML files in practice, but a second `\r` check after the `\n` strip would be more robust.

- **`classify_json_key_at_depth0` duplicates whitespace-skip pattern** - `serde_fields.rs:158-181` (Confidence: 70%) -- This newly extracted function still contains inline whitespace-skipping loops despite the `skip_json_whitespace` helper existing on line 177. The current file shows that the function body was updated to call `skip_json_whitespace` (lines 160, 164), so this may be a diff artifact from an intermediate commit that was already resolved. If the current on-disk code uses the helper, no action needed.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

## Rationale

This incremental diff is architecturally clean. The key changes demonstrate solid design thinking:

1. **Extract Method refactorings** (SRP): `classify_json_key_at_depth0` and `strip_list_prefix` are good extractions that reduce function length and clarify intent. Each new function has a single responsibility and clear documentation.

2. **Bounded resource usage** (reliability): The `MAX_JSON_DEPTH` cap on the `in_key_stack` vector is the right pattern for preventing unbounded heap growth on adversarial input. The `(eol + 1).min(len)` bounds on TOML line advancement fix potential out-of-bounds arithmetic at end-of-input.

3. **Consistent Strategy Pattern**: The size guard added to `classify_markdown` mirrors the guard already in `classify_source`, maintaining symmetry across all entry points. The Markdown classifier reuses `MAX_SOURCE_BYTES` from the classifier module rather than defining its own constant -- good DRY practice.

4. **TOML escape handling**: The switch from `for (i, &b)` to `while i < content.len()` with manual `i += 2` for escape skipping in `find_toml_eq_sign` is the correct structural change -- `for` iterators cannot skip ahead, so the `while` loop is the right pattern for escape-aware scanning.

5. **Test quality**: New TOML triple-quote tests (F-TOML-07 through F-TOML-10) cover the important edge cases (basic multi-line, literal multi-line, embedded quotes, backslash-as-literal) and all use the shared contract helpers (`assert_contiguous`, `assert_field_lengths_sum`).

The only blocking-tier finding is the minor push/pop asymmetry in the JSON depth cap, which is safe in practice but should be tightened for defensive correctness.
