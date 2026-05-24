# Architecture Review Report

**Branch**: feat/193-custom-field-mapping-json-yaml-toml -> main
**Date**: 2026-05-19

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Inconsistent error model between serde scanners and Markdown classifier** - `serde_fields.rs:49`, `serde_fields.rs:239`, `serde_fields.rs:398`, `markdown.rs:43`
**Confidence**: 82%
- Problem: The serde scanners (JSON, YAML, TOML) return `Vec<(Range<usize>, SearchField)>` (infallible), while the Markdown classifier returns `crate::Result<Vec<(Range<usize>, SearchField)>>` (fallible). This is an intentional design choice documented in `mod.rs` (serde scanners are infallible, Markdown uses tree-sitter which can theoretically fail), and the PR description explicitly states "JSON/YAML/TOML use lightweight std-only scanners (infallible, return Vec)". However, the Markdown classifier never actually produces an error in practice -- both parser init failure and parse failure are caught and returned as `Ok(vec![(0..len, SearchField::Other)])`. The `Result` return type propagates only because `classify_markdown` is called from `classify_source` which already returns `Result`, and the doc comment references a `size_guard` error that is actually enforced by the caller, not by the function itself. The asymmetry is acceptable because it matches the Strategy Pattern's dispatch model (tree-sitter path returns `Result`, serde path returns `Vec`), but it does create a minor layering issue: `classify_markdown`'s documented error path ("Returns SearchError only if the size guard fires") is misleading because that guard is in `classify_source`, not in `classify_markdown`.
- Fix: Update the doc comment on `classify_markdown` to clarify that the function itself is effectively infallible (parser errors are caught), and the `Result` return type exists for future extensibility or if callers bypass `classify_source`. Alternatively, make it infallible like the serde scanners and wrap in `Ok()` at the call site. Either approach is acceptable -- the current implementation is functionally correct.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Strategy Pattern dispatch could use a trait-based design** - `classifier.rs:149-163` (Confidence: 65%) -- The existing `FieldClassifier` trait in `types.rs` was designed as a "future extension point" for exactly this use case (non-tree-sitter language classification). The PR adds format-specific classifiers using a `match` dispatch in `classify_source` rather than implementing `FieldClassifier`. This is pragmatic -- the trait operates on individual `NodeInfo` structs while the new classifiers operate on whole-source byte ranges, so the trait's interface does not fit. Worth noting for future architecture decisions: if more format-specific classifiers are added, the `match` dispatch may grow unwieldy and a whole-source classifier trait may be warranted.

- **JSON scanner in_key_stack is an unbounded Vec** - `serde_fields.rs:68` (Confidence: 72%) -- The `in_key_stack` grows with nesting depth. For well-formed JSON, this is bounded by nesting depth (typically < 100). However, malformed input with many unclosed `{` braces could grow the stack proportionally to input size. The `MAX_SOURCE_BYTES` guard in `classify_source` caps input at 100 MiB, so this is not a practical memory concern, but adding a depth cap (e.g., 1000) would be defensive.

- **Markdown classifier duplicates tree-sitter walk pattern** - `markdown.rs:73-100` (Confidence: 63%) -- The pre-order walk loop in `classify_markdown` is structurally identical to the one in `classify_source` (lines 182-212 of `classifier.rs`). Extracting a shared "walk tree and collect ranges via a mapper function" utility would reduce duplication. The two differ only in the node-to-field mapping function, which makes this a straightforward higher-order function extraction. Not urgent for a 2-instance duplication.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 9/10
**Recommendation**: APPROVED

## Rationale

This PR demonstrates strong architectural discipline:

1. **Strategy Pattern compliance**: The dispatch in `classify_source` follows the existing Strategy Pattern documented in CLAUDE.md. JSON/YAML/TOML route to serde-based scanners; Markdown routes to a tree-sitter scanner with custom field mapping; all other languages continue through the generic tree-sitter path. The `match` dispatch is clean and explicit.

2. **Separation of Concerns**: The new `fields/` module cleanly separates format-specific classification from the generic tree-sitter classifier. Each scanner is self-contained with clear input/output contracts. The shared `fill_gaps_and_merge` utility is appropriately factored into the module root, while `build_field_ranges` (the overlap-resolution algorithm) is correctly reused from the classifier module for Markdown's overlapping parent/child ranges.

3. **Layering**: The dependency direction is correct -- `fields/` depends on `lexical/classifier` for shared utilities (`build_field_ranges`, `merge_adjacent`), and `classifier` dispatches to `fields/` for format-specific logic. There are no circular dependencies. The `pub(crate)` visibility on all new functions prevents external callers from bypassing the size guard in `classify_source`.

4. **Infallibility contract**: The design choice to make serde scanners infallible (return `Vec`, not `Result`) is architecturally sound. These are byte-level state machines operating on `&str` -- they cannot encounter I/O errors or grammar loading failures. Malformed input degrades gracefully to `Other` ranges.

5. **FORMAT_VERSION bump**: Bumping from v1 to v2 correctly forces a full re-index after upgrade. The version mismatch path returns an empty manifest (cold start), which is the right behavior. Test updates properly verify this contract.

6. **Deep Module principle**: The scanners encapsulate significant complexity (JSON state machine, YAML indentation tracking, TOML multi-line string scanning) behind simple `fn classify_X(source: &str) -> Vec<...>` interfaces. The API surface is minimal relative to the implementation complexity -- a good application of the Deep Modules principle.

The single MEDIUM finding (doc comment mismatch on `classify_markdown`'s error path) is a documentation accuracy issue, not a structural flaw.
