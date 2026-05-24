# Security Review Report

**Branch**: feat/rskim-search-foundation -> main
**Date**: 2026-05-10

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Missing `deny_unknown_fields` on deserializable structs** - `types.rs:105,119,158,179` (Confidence: 65%) — `TemporalFlags`, `SearchResult`, `IndexStats` derive `Deserialize` without `#[serde(deny_unknown_fields)]`. If these types are ever deserialized from untrusted external input (e.g. a JSON API request), unknown fields would be silently dropped rather than rejected. Currently these are internal types with no external deserialization boundary, so this is speculative.

- **No input length bound on `SearchQuery::new`** - `types.rs:138` (Confidence: 60%) — `SearchQuery::new(text: impl Into<String>)` accepts unbounded input. If a future caller passes user-controlled text without a length limit, it could lead to excessive memory allocation or expensive search operations. This is a library type with no direct user input path today, so the risk is future-facing.

- **`NodeInfo` public but not re-exported** - `types.rs:242` (Confidence: 70%) — `NodeInfo` is `pub struct` with public fields including `byte_range: Range<usize>`, but it is not re-exported from `lib.rs`. If it becomes part of the public API later, `byte_range` values from untrusted sources could be used to index into source buffers without bounds checking. The `from_ts_node` constructor is safe (tree-sitter provides valid ranges), but direct construction via public fields has no validation. Currently unexposed, so not blocking.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Security Score**: 9/10
**Recommendation**: APPROVED

### Rationale

This is a well-structured Wave 0 foundation PR introducing pure types, traits, and error enums for a search library crate. The security posture is strong:

1. **No I/O or network surface**: The crate is explicitly designed as a pure library with no I/O. All I/O is deferred to the CLI layer (`crates/rskim/src/cmd/search.rs`), which currently only prints help text and an "unimplemented" message.

2. **No unsafe code**: Zero `unsafe` blocks, no raw pointer manipulation, no `transmute`.

3. **No hardcoded secrets**: No credentials, API keys, or sensitive data anywhere in the diff.

4. **No deserialization from untrusted input**: All `serde_json::from_str` calls are in `#[cfg(test)]` blocks only. The `Deserialize` derives exist for future JSON output roundtripping, not for ingesting external data.

5. **Sound error handling**: Uses `thiserror` with typed `SearchError` enum and a `Result<T>` alias. No panics in non-test code (clippy `unwrap_used` and `expect_used` are denied at the crate level).

6. **Strict clippy lints**: The crate's `Cargo.toml` denies `unwrap_used`, `expect_used`, and `panic`, which is a strong defense-in-depth measure for a library crate.

7. **Type safety**: `FileId(u32)` newtype prevents accidental misuse of raw integers. `SearchField` is a closed enum with exhaustive matching.

The three suggestions are all below the 80% confidence threshold and relate to hypothetical future attack surfaces that do not exist today. No changes are required for merge.
