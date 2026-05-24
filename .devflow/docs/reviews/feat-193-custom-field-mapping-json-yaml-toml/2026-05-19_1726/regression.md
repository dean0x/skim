# Regression Review Report

**Branch**: feat/193-custom-field-mapping-json-yaml-toml -> main
**Date**: 2026-05-19T17:26

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Markdown H4+ headings now classified as Other instead of TypeDefinition** - `crates/rskim-search/src/fields/markdown.rs:114`
**Confidence**: 82%
- Problem: On `main`, the generic tree-sitter path mapped all `atx_heading` and `setext_heading` nodes to priority 5 (TypeDefinition) regardless of heading level. The new `classify_markdown` function downgrades H4+ headings to `SearchField::Other`. This is an intentional design choice (documented in field mapping table at line 11), but it is a behavior change that affects BM25F scoring for existing Markdown documents containing H4-H6 headings. Those headings previously received a TypeDefinition boost and will now receive an Other (1.0x) boost.
- Fix: This appears intentional per the PR description. If confirmed intentional, no fix needed. If H4+ should retain boosted scoring, change the threshold from `1..=3` to `1..=6` or introduce a new intermediate field.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Manifest FORMAT_VERSION bump forces full re-index** - `crates/rskim/src/cmd/search/manifest.rs:130` (Confidence: 70%) -- The version bump from 1 to 2 silently discards existing v1 manifests, triggering a cold-start full re-index on first run after upgrade. The version mismatch path returns an empty manifest (line 203-204), which is the correct degradation pattern. However, users of large repositories may experience a one-time latency spike. The design is sound (documented in commit and code comments), but there is no user-facing message explaining why re-indexing is happening.

- **`in_key_stack` unbounded growth on deeply nested malformed JSON** - `crates/rskim-search/src/fields/serde_fields.rs:68` (Confidence: 65%) -- The `in_key_stack: Vec<bool>` grows by one entry per `{` character. For well-formed JSON this is bounded by the nesting depth, but adversarial input with millions of `{` characters and no matching `}` could grow the stack unboundedly up to the MAX_SOURCE_BYTES (100 MiB) limit. The MAX_SOURCE_BYTES guard in the caller (`classify_source`) bounds the maximum allocation, but a 100 MiB file of `{{{...` would still allocate a large Vec. This is a theoretical concern given the size guard.

- **Visibility widening of `build_field_ranges` and `merge_adjacent`** - `crates/rskim-search/src/lexical/classifier.rs:238,287` (Confidence: 62%) -- Both functions changed from `fn` (private) to `pub(crate)`. This widens their contract surface -- callers in other modules (`fields/mod.rs`, `fields/markdown.rs`) now depend on their pre-order and interval-subtraction semantics. The functions have thorough doc comments specifying preconditions and output invariants, which mitigates risk. No regression from this change alone.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Regression Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Conditions

1. Confirm that the H4+ heading downgrade to Other is intentional. The test `f_md_03_h4_is_not_type_def` explicitly asserts this behavior, and the PR description does not mention it as a breaking change. If intentional, this is a well-considered design choice (major headings H1-H3 get boosted, minor headings do not).

### Regression Checklist

- [x] No exports removed without deprecation
- [x] Return types backward compatible (`classify_source` still returns `Result<Vec<(Range<usize>, SearchField)>>`)
- [x] Default values unchanged (FORMAT_VERSION bump is intentional)
- [x] Side effects preserved (error logging, fallback patterns)
- [x] All consumers of changed code updated (index builder calls `classify_source` unchanged)
- [x] Migration complete across codebase (no stale v1 references remain in production code)
- [x] CLI options preserved (no CLI changes)
- [x] Commit message matches implementation (3 commits: feature, self-review fixes, rustfmt)
- [x] Breaking changes documented (FORMAT_VERSION bump documented in code comment)
- [x] Tests updated (old "single-Other" tests replaced with format-specific assertions, new tests added: 47 field tests + 17 classifier tests + 32 manifest tests all passing)

### Positive Observations

- The FORMAT_VERSION bump is correctly handled: existing v1 manifests are silently discarded and re-indexed, matching the existing cold-start pattern.
- Tests that previously verified JSON/YAML/TOML were "single Other" have been updated to verify the new field mappings, preventing silent test regression.
- Two pre-existing test bugs were fixed in the self-review commit (tests that hardcoded `"version": 1` were testing version mismatch rather than their stated purpose).
- The `MAX_SOURCE_BYTES` guard fires before format-specific dispatch, preserving the security boundary.
- The serde scanners are infallible (return `Vec`, not `Result`), preventing new error paths.
- Integration tests (I-01 through I-06) verify end-to-end dispatch through `classify_source`.
- Test I-05 explicitly verifies that Rust (tree-sitter) classification is unchanged by the new dispatch.
