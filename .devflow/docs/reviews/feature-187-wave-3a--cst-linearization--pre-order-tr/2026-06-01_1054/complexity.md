# Complexity Review Report

**Branch**: feature-187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01

## Issues in Your Changes (BLOCKING)

### HIGH

**Duplicated ascend-loop pattern in `linearize_tree`** - `crates/rskim-search/src/ast_index/linearize.rs:255-265` and `crates/rskim-search/src/ast_index/linearize.rs:300-311`
**Confidence**: 85%
- Problem: The "try next sibling, else ascend parent and pop level_stack" loop appears twice with near-identical structure. Lines 255-265 handle the bounds-guard skip path; lines 300-311 handle the normal advance path. Both contain the same `goto_next_sibling` / `level_stack.is_empty()` / `goto_parent` / `pop` sequence. This is a DRY violation that increases maintenance risk — a fix to the ascend logic must be applied in two places.
- Fix: Extract a shared helper closure or inline function:
  ```rust
  /// Advance to next sibling, or ascend until one is found.
  /// Returns `false` when the traversal is complete (level_stack exhausted).
  fn advance_or_ascend(
      cursor: &mut tree_sitter::TreeCursor,
      level_stack: &mut Vec<u16>,
      depth: &mut u16,
  ) -> bool {
      loop {
          if cursor.goto_next_sibling() {
              return true;
          }
          if level_stack.is_empty() {
              return false;
          }
          cursor.goto_parent();
          *depth = level_stack.pop().unwrap_or(0);
      }
  }
  ```
  Then both call sites become:
  ```rust
  if !advance_or_ascend(&mut cursor, &mut level_stack, &mut depth) {
      return Ok(result);
  }
  ```
  This reduces `linearize_tree` from ~82 to ~60 lines, drops cyclomatic complexity from ~11 to ~8, and eliminates the duplication.

### MEDIUM

**`linearize_tree` cyclomatic complexity at warning threshold** - `crates/rskim-search/src/ast_index/linearize.rs:232-313`
**Confidence**: 82%
- Problem: The function has an estimated cyclomatic complexity of ~11 (outer loop, 2 inner loops, bounds-guard conditional, error/non-error branch, descend/no-children branch, plus the inner-loop conditionals). The complexity skill threshold flags 10-20 as HIGH warning territory. The function is 82 lines with 4 levels of nesting depth — at the boundary of the "explain in 5 minutes" principle, though the clear section comments help mitigate this.
- Fix: Extracting the ascend helper (see previous finding) would bring complexity to ~8 and nesting to 3, both within the "good" range. No further extraction needed — the remaining logic (process node, descend or advance) is inherently sequential and would not benefit from being split.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`LANG_MAPS` initializer closure is 58 lines with nesting depth 4** - `crates/rskim-search/src/ast_index/linearize.rs:109-167`
**Confidence**: 80%
- Problem: The `LazyLock` closure iterates 14 languages, each going through Parser::new, parse, kind iteration, and binary search — totaling 58 lines with 4 nesting levels (closure > for > match > if). While this is initialization-only code (runs once), the nesting makes it harder to scan.
- Fix: Extract the inner per-language body into a named function:
  ```rust
  fn build_lang_map(lang: Language) -> Option<Vec<Option<u16>>> {
      let mut parser = Parser::new(lang).ok()?;
      let tree = parser.parse("").ok()?;
      let ts_lang = tree.language();
      let kind_count = ts_lang.node_kind_count();
      let mut lang_map: Vec<Option<u16>> = vec![None; kind_count];
      for (kind_id, entry) in lang_map.iter_mut().enumerate() {
          if let Some(kind_str) = ts_lang.node_kind_for_id(kind_id as u16) {
              if let Ok(vocab_idx) = NODE_KIND_VOCABULARY.binary_search(&kind_str) {
                  *entry = Some(vocab_idx as u16);
              }
          }
      }
      Some(lang_map)
  }
  ```
  This drops the closure to ~15 lines: iterate languages, call `build_lang_map`, insert if `Some`. Reduces nesting from 4 to 2 in the closure and makes the per-language logic independently testable.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`known_kind_roundtrips_through_lang_map` test does not exercise the lang_map** - `crates/rskim-search/src/ast_index/linearize_tests.rs:120-127` (Confidence: 65%) — Despite its name, this test only verifies `NODE_KIND_VOCABULARY` binary search roundtrip, not an actual lang_map lookup. Consider renaming to `vocabulary_binary_search_roundtrip` for clarity, or enhancing to test through the actual lang_map.

- **Performance test generates only 100 functions despite comment saying "1000-line file"** - `crates/rskim-search/src/ast_index/linearize_tests.rs:427` (Confidence: 70%) — The comment says "1000-function Rust file" but the iterator range is `0..100` (100 functions). Each function is one line, so this is a ~100-line file, not 1000. This may be intentional (each function expands to ~10 AST nodes) but the comment is misleading.

- **Test file at 445 lines approaching file length warning threshold** - `crates/rskim-search/src/ast_index/linearize_tests.rs` (Confidence: 60%) — At 445 lines, the test file is near the 500-line complexity threshold. Currently well-organized by cycle with clear section headers. No action needed now, but worth noting if more test cycles are added.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Complexity Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The core algorithmic logic in `linearize_tree` is correct and well-bounded (all loops have explicit termination via `level_stack` exhaustion and `MAX_AST_NODES`/`MAX_AST_DEPTH` guards — avoids unbounded iteration). The module follows the "intentionally minimal: single public function, no builder pattern" design documented in the feature knowledge. The duplicated ascend-loop pattern is the primary complexity concern — extracting it into a helper would bring all metrics into the "good" range. The `LANG_MAPS` initializer extraction is a should-fix that improves readability without affecting correctness. All findings surfaced per applies ADR-001.
