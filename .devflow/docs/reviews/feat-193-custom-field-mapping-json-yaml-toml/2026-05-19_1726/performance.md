# Performance Review Report

**Branch**: feat/193-custom-field-mapping-json-yaml-toml -> main
**Date**: 2026-05-19

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**JSON scanner `in_key_stack` grows unboundedly with nesting depth** - `crates/rskim-search/src/fields/serde_fields.rs:68`
**Confidence**: 82%
- Problem: The `in_key_stack: Vec<bool>` in `classify_json` pushes one entry per `{` encountered. For deeply-nested or adversarial JSON (e.g., 10,000 nested objects), this allocates O(depth) heap memory and triggers O(depth) individual `Vec` reallocations. While the `MAX_SOURCE_BYTES` guard (100 MiB) limits total input size, a pathological file with 50 million nested braces (each is 1 byte) would push 50M entries onto this stack.
- Fix: Cap the depth to a reasonable bound (e.g., 1024 or 4096) and stop pushing once exceeded, treating deeper keys as `SymbolName`. This is consistent with how most JSON parsers impose depth limits. Alternatively, pre-allocate with `Vec::with_capacity(32)` for the common case to avoid early reallocation churn:
  ```rust
  let mut in_key_stack: Vec<bool> = Vec::with_capacity(32);
  ```
  For the depth cap:
  ```rust
  const MAX_JSON_DEPTH: usize = 1024;
  // ...
  b'{' => {
      brace_depth += 1;
      if in_key_stack.len() < MAX_JSON_DEPTH {
          in_key_stack.push(true);
      }
      i += 1;
  }
  ```

### MEDIUM

**No `Vec::with_capacity` hints on scanner output vectors** - `crates/rskim-search/src/fields/serde_fields.rs:57,246,405`
**Confidence**: 85%
- Problem: All three scanners (`classify_json`, `classify_yaml`, `classify_toml`) allocate `ranges` with `Vec::new()` (zero capacity). For a typical config file with, say, 200 key-value pairs, this causes ~8 reallocations as the Vec doubles. While each reallocation is cheap (amortized O(1)), the repeated copying during reallocation is wasteful when a reasonable estimate is available.
- Fix: Use a heuristic capacity hint. For line-based scanners (YAML/TOML), `source.lines().count()` is too expensive since it iterates the whole string. A byte-count heuristic works well:
  ```rust
  // Rough estimate: ~1 range per 30 bytes of source (key + value + punctuation).
  let estimated_ranges = len / 30;
  let mut ranges: Vec<(Range<usize>, SearchField)> = Vec::with_capacity(estimated_ranges);
  ```
  This eliminates nearly all reallocation for typical config files (under ~1KB).

**`fill_gaps_and_merge` over-allocates result buffer** - `crates/rskim-search/src/fields/mod.rs:65`
**Confidence**: 83%
- Problem: `Vec::with_capacity(ranges.len() * 2 + 1)` pessimistically assumes every classified range has a gap before it. In practice, scanners often emit near-contiguous ranges (especially JSON, where keys and values are adjacent). The `* 2` multiplier wastes ~50% of allocated memory for dense inputs. For a JSON file with 1000 key-value pairs producing ~2000 ranges, this allocates space for 4001 entries when ~2200 are typical.
- Fix: A tighter estimate is `ranges.len() + ranges.len() / 4 + 1` (25% gap overhead). This still avoids reallocation in the common case while reducing peak memory:
  ```rust
  let mut result: Vec<(Range<usize>, SearchField)> = Vec::with_capacity(ranges.len() + ranges.len() / 4 + 1);
  ```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**YAML newline search uses linear `iter().position()` instead of `memchr`** - `crates/rskim-search/src/fields/serde_fields.rs:252-256`
**Confidence**: 80%
- Problem: The YAML scanner finds end-of-line via `bytes[line_start..].iter().position(|&b| b == b'\n')`. While correct, this is a byte-at-a-time scan. The `memchr` crate provides SIMD-accelerated single-byte search that is ~4-8x faster for this exact pattern. The same pattern appears in the TOML scanner at line 420-424. For large YAML/TOML config files (multi-KB kubernetes manifests, large Cargo.toml), this adds up.
- Fix: Since `memchr` is already a transitive dependency via tree-sitter, adding a direct dependency is low-cost:
  ```rust
  let line_end = memchr::memchr(b'\n', &bytes[line_start..])
      .map(|p| line_start + p + 1)
      .unwrap_or(len);
  ```
  This is a "should-do" optimization, not blocking -- the current implementation is correct and performant for typical config files under a few KB.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **JSON look-ahead whitespace skip could use `memchr` or byte table** - `crates/rskim-search/src/fields/serde_fields.rs:129-148` (Confidence: 65%) -- The two whitespace-skipping loops in the JSON key TypeDefinition look-ahead scan each byte individually. For very large JSON files with extensive whitespace formatting, a lookup table or SIMD skip could help, but the look-ahead only fires for depth-0 keys, limiting the impact.

- **Markdown classifier creates a new tree-sitter parser per call** - `crates/rskim-search/src/fields/markdown.rs:52` (Confidence: 70%) -- `Parser::new(Language::Markdown)` is called every time `classify_markdown` runs. If the parser construction involves grammar loading overhead (depends on the `rskim_core::Parser` internals), caching the parser across calls could reduce per-file indexing cost. However, tree-sitter parser creation is typically fast (~microseconds), so this may not matter in practice.

- **`find_toml_eq_sign` does not handle backslash escapes in quoted keys** - `crates/rskim-search/src/fields/serde_fields.rs:587-608` (Confidence: 72%) -- The string-tracking in `find_toml_eq_sign` flips `in_str` on seeing the delimiter byte but does not account for escaped delimiters (`\"` inside a basic string). A key like `"key=\"val"` would cause the scanner to exit the string prematurely at the escaped quote. This is a correctness concern with performance implications (misclassified ranges mean wrong field boosting in BM25F scoring), but such keys are extremely rare in real TOML files.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The scanners are well-designed: lightweight, std-only, byte-oriented, and single-pass. The algorithmic complexity is O(n) for all three formats, which is optimal. The `fill_gaps_and_merge` post-processing is linear. The key performance concern is the unbounded `in_key_stack` in the JSON scanner which could cause excessive allocation on adversarial inputs -- adding a depth cap would close this gap. The Vec capacity hints and memchr suggestions are incremental optimizations that reduce allocation pressure but are not blocking. Overall, this is solid performance-aware code with minor room for hardening against pathological inputs.
