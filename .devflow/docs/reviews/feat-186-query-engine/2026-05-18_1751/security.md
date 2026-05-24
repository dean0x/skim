# Security Review Report

**Branch**: feat/186-query-engine -> main
**Date**: 2026-05-18
**Scope**: Incremental (4 commits since last review)
**Changed files**: `crates/rskim-search/src/lexical/mod.rs`, `crates/rskim-search/src/lexical/query.rs`, `crates/rskim-search/src/lexical/query_tests.rs`

## Issues in Your Changes (BLOCKING)

_No blocking security issues found._

## Issues in Code You Touched (Should Fix)

_No should-fix security issues found._

## Pre-existing Issues (Not Blocking)

_No pre-existing security issues found._

## Suggestions (Lower Confidence)

- **Unvalidated `limit` / `offset` fields in `QueryEngine`** - `query.rs:50` (Confidence: 65%) -- `SearchQuery` derives `Deserialize`, so untrusted JSON input could set `limit` to `usize::MAX`. The downstream reader defaults to 20 and uses safe `.skip().take()` iterators, so no memory safety risk exists today. However, the `QueryEngine` decorator positions itself as the trust boundary and validates `text` and `bm25f_config` but silently passes through `limit`, `offset`, `ast_pattern`, and `temporal_flags` without bounds checks. If a future inner layer allocates based on `limit`, this could become a resource exhaustion vector. Consider adding an upper bound on `limit` (e.g., 1000) at the decorator for defense-in-depth consistency.

- **PR description states 64KB default but code uses 4KB** - `query.rs:15` (Confidence: 70%) -- The PR description says "MAX_QUERY_BYTES (64KB default, configurable)" but the actual constant is `4096` (4 KiB) and is not configurable (it is a compile-time `const`). This is not a code vulnerability but a documentation mismatch that could mislead downstream consumers about the actual query size limit. Ensure external documentation matches the implementation.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Security Score**: 9/10
**Recommendation**: APPROVED

## Analysis Notes

### What was reviewed

The incremental changes (4 commits: `c4c3cef`, `21b07d2`, `5312a63`, `2a563b4`) refactor the `QueryEngine` decorator and its test suite. Key security-relevant changes:

1. **`query.rs`**: Added `#[must_use]` on constructor, added defense-in-depth comment on the `SearchLayer::search` impl. No logic changes to validation.
2. **`query_tests.rs`**: Replaced concrete inner layers with `SpyLayer`/`PanicLayer` test doubles. Switched from `match` error variant checks to `format!`-based string assertions. Added dedicated NaN/Infinity/NEG_INFINITY BM25F tests. Replaced silent `return` on insufficient pagination data with a hard `assert!`.
3. **`mod.rs`**: Updated module doc to reference `QueryEngine` and `MAX_QUERY_BYTES`.

### Security posture assessment

**Input validation boundary (strong):**
- Empty queries short-circuit before reaching the inner layer -- prevents unnecessary index I/O.
- Query text length is bounded by `MAX_QUERY_BYTES` (4096 bytes) -- prevents oversized payloads from reaching the parser.
- BM25F config is validated for finite, non-negative values before search -- prevents NaN/Infinity poisoning of scoring arithmetic.
- All validation errors return typed `SearchError::InvalidQuery` with descriptive messages that do not leak internal state (no file paths, no stack traces, no index structure).

**Defense in depth (good):**
- The decorator validates independently of the inner layer, as documented in the inline comment.
- `query.text.len()` measures byte length, which is correct for resource limiting (not char count which could differ for multibyte).

**Deserialization surface:**
- `SearchQuery` derives `Deserialize`, making it an untrusted input boundary. The `QueryEngine` validates `text` and `bm25f_config` but passes through `limit`, `offset`, `ast_pattern`, and `temporal_flags` without validation. Currently safe because downstream uses are bounded (iterator `.skip().take()`), but noted as a suggestion for completeness.

**Test double safety:**
- `SpyLayer` uses `Mutex<Option<SearchQuery>>` for interior mutability -- standard and safe.
- `PanicLayer` proves short-circuit paths never reach the inner layer -- good negative testing.
- `.lock().unwrap()` in test code is acceptable (panics only if a test thread panicked while holding the lock).

**No issues found in OWASP categories:**
- No injection vectors (no string interpolation into queries, no shell/SQL/command execution).
- No authentication/authorization concerns (library layer, not an endpoint).
- No cryptographic operations.
- No secrets or credentials.
- No file system access in the changed code.
- No network operations.
