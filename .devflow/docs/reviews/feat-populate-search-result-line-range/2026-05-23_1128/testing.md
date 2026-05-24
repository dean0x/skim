# Testing Review Report

**Branch**: feat-populate-search-result-line-range -> main
**Date**: 2026-05-23T11:28

## Issues in Your Changes (BLOCKING)

### HIGH

**Missing test for JSON serialization of `ResolvedResult.line_range`** - `crates/rskim/src/cmd/search/query_tests.rs:302`
**Confidence**: 85%
- Problem: The new `line_range: Option<Range<usize>>` field on `ResolvedResult` is documented as serialising to `{"start": N, "end": M}` in `--format json` output (types.rs:70), but `test_format_json_output_is_valid_json` uses an empty results vec and never exercises the serialization of `line_range`. The two existing tests (`test_format_text_output_includes_path_and_score`, `test_format_text_output_includes_stale_marker`) construct `ResolvedResult` with `line_range: Some(...)` but only validate text output, not JSON. There is no test that asserts the JSON shape of a non-None `line_range` value on the `ResolvedResult` type (the CLI output type). This is an important behavioral gap: consumers of `--format json` rely on this shape, and a serde misconfiguration (e.g., `Range<usize>` serializing differently than expected) would silently produce wrong output.
- Fix: Add a test that creates a `ResolvedResult` with `line_range: Some(5..10)`, serializes it via `format_json_output`, then asserts the JSON contains `"line_range": {"start": 5, "end": 10}`.

### MEDIUM

**No test for `ResolvedResult.line_range = None` in JSON output** - `crates/rskim/src/cmd/search/query_tests.rs`
**Confidence**: 82%
- Problem: When snippet extraction returns `Stale` or `Unavailable`, `line_range` is `None`. There is no test that verifies this serializes as `null` in JSON output (matching the behavior of `line_number` and `snippet`). Since `line_range` is `Option<Range<usize>>` and `Range` has a non-trivial serde representation, the null case should be explicitly verified.
- Fix: Add a test with `line_range: None` that checks the JSON output contains `"line_range": null`.

**Duplicate `byte_offset_to_line` functions not tested for behavioral equivalence** - `crates/rskim/src/cmd/search/snippet.rs:47` and `crates/rskim-search/src/types.rs:351`
**Confidence**: 80%
- Problem: There are now two `byte_offset_to_line` implementations: the library version in `rskim-search/types.rs` (returns `usize`, `#[must_use]`) and the CLI version in `rskim/snippet.rs` (returns `u32`, `pub(super)`). Both have identical logic but different return types. The library version is used by `compute_line_range` while the CLI version is used by `extract_snippet` for the `match_line` return value. There is no cross-function test asserting these produce equivalent results, meaning they could diverge silently. The snippet tests exercise the `u32` version and the types tests exercise the `usize` version, but nothing connects them.
- Fix: Either (a) replace the CLI `byte_offset_to_line` with a call to `rskim_search::byte_offset_to_line` (casting `usize` to `u32` at the call site), eliminating the duplicate, or (b) add a property-style test that asserts both functions produce the same result for a range of inputs.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`SearchResult.line_range` doc comment says "0-indexed" but `compute_line_range` returns 1-indexed ranges** - `crates/rskim-search/src/types.rs:331`
**Confidence**: 85%
- Problem: The doc comment on `SearchResult.line_range` reads `/// Source lines spanned by this match (0-indexed, exclusive end)`. However, `compute_line_range` -- the function documented as computing values for this field -- returns 1-indexed line numbers (line 364: "1-indexed, matching the convention used by SearchResult::line_range"). The reader.rs still initializes this field to `0..0` as a placeholder. The doc comment is stale or incorrect and would confuse consumers into expecting 0-indexed values when the actual populated values are 1-indexed. While this is a documentation issue, not a test issue, it directly affects anyone writing tests against this field.
- Fix: Update the doc comment on `SearchResult.line_range` to `/// Source lines spanned by this match (1-indexed, exclusive end)` to match the actual convention established by `compute_line_range`.

## Pre-existing Issues (Not Blocking)

_None identified._

## Suggestions (Lower Confidence)

- **Missing edge-case test for `compute_line_range` with match positions at exact newline bytes** - `crates/rskim-search/src/types.rs:1071` (Confidence: 65%) -- The test suite covers empty, single, multi-line, same-line, and adjacent positions, but does not test a match position whose `start` byte lands exactly on a `\n` character. Since `byte_offset_to_line` counts newlines before the offset, this boundary could behave unexpectedly for consumers who assume the newline byte belongs to the next line.

- **`test_extract_snippet_computes_line_range` does not assert the snippet context window** - `crates/rskim/src/cmd/search/snippet_tests.rs:192` (Confidence: 62%) -- The new test validates `line_no` and `lr` but uses `_ctx` (discards the context). A brief assertion that `ctx.lines` is non-empty and that the match line is marked would guard the interaction between `line_range` computation and context window extraction.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The new unit tests for `byte_offset_to_line` (7 tests) and `compute_line_range` (5 tests) are well-structured, follow AAA pattern, and cover core edge cases (empty input, clamping, single/multi-line, adjacent lines). The integration test `test_extract_snippet_computes_line_range` verifies end-to-end behavior through file I/O. However, the primary gap is the absence of JSON serialization tests for the new `line_range` field on `ResolvedResult`, which is the user-facing output type. The duplicate `byte_offset_to_line` implementations across crates also introduce a maintenance risk that should be addressed with either deduplication or equivalence testing.
