# Security Review Report

**Branch**: feat-rskim-search-foundation -> main
**Date**: 2026-05-10T15:00:00Z
**Snyk SAST**: 0 issues found

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

(none)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **SearchQuery `text` field unbounded** - `crates/rskim-search/src/types.rs:101` (Confidence: 65%) -- `SearchQuery::new` accepts any `impl Into<String>` with no length cap. When a future `SearchLayer` implementation processes this against an index, an adversarially large query string could cause excessive memory allocation or regex compilation cost. Consider adding a `MAX_QUERY_LEN` constant and returning `SearchError::InvalidQuery` when exceeded. This is a library-only type with no I/O path today, so impact is currently zero; flagging for when the search implementation lands.

- **`FileId` inner field is `pub`** - `crates/rskim-search/src/types.rs:25` (Confidence: 60%) -- `FileId(pub u32)` exposes the inner value, weakening the newtype invariant. Callers can construct arbitrary `FileId` values that may not correspond to indexed files. The `SearchError::FileNotFound` variant exists to handle this at query time, so the risk is mitigated. A constructor + accessor pattern would enforce validation at construction, but this is a style choice for a library crate with `publish = false`.

- **`unsafe` env var mutation in tests** - `crates/rskim/src/cmd/session/cursor.rs:620,627` (Confidence: 70%) -- The edition 2024 migration correctly wraps `set_var`/`remove_var` in `unsafe` blocks. The SAFETY comments state "single-threaded test environment," which is accurate for individual `#[test]` functions but not guaranteed when `cargo test` runs tests in parallel within the same process. This is a pre-existing pattern that the edition migration merely surfaced; the test already existed before this PR.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Security Score**: 9/10
**Recommendation**: APPROVED

## Analysis Details

### What was reviewed

3 commits covering 42 files: new `rskim-search` library crate (pure types/traits/errors), workspace `thiserror` 1.0 to 2.0 upgrade, edition 2021 to 2024 migration, and `search` CLI stub.

### Positive security observations

1. **No I/O in library crate** -- The `rskim-search` crate is deliberately designed with no file system, network, or process I/O. All types are pure data structures with `Serialize`/`Deserialize` derives. This eliminates entire vulnerability classes (path traversal, injection, SSRF) at the architectural level.

2. **Strict clippy lints** -- `Cargo.toml` sets `unwrap_used = "deny"`, `expect_used = "deny"`, `panic = "deny"` at the crate level. This prevents accidental panics in library code that could be triggered by malicious input.

3. **Typed error handling** -- `SearchError` uses `thiserror` with `#[from]` conversions for `SkimError` and `io::Error`. No `unwrap()` or `expect()` in non-test code. The `Result<T>` type alias enforces consistent error propagation.

4. **Existing security patterns preserved** -- The edition 2024 if-let chaining refactors are purely syntactic (collapsing nested `if let` into chained conditions). All existing security guards remain intact:
   - Symlink traversal guards in session providers (claude, codex, copilot, crush, gemini)
   - `MAX_AST_DEPTH` stack overflow protection in `structure.rs` and `signatures.rs`
   - `HOOK_MAX_STDIN_BYTES` bounded read in hook mode
   - `HOOK_TIMEOUT_SECS` watchdog timer
   - `is_safe_session_id` validation before command interpolation
   - `MAX_SESSION_SIZE` / `MAX_DB_SIZE` file size guards
   - `AUDIT_LOG_MAX_BYTES` rotation limit

5. **`publish = false`** -- The new crate is not published to crates.io, limiting supply chain exposure.

6. **thiserror 2.0 upgrade** -- No security implications; thiserror 2.0 is a proc-macro with no runtime behavior changes affecting security. The upgrade removes the dual-version (1.0 + 2.0) split in `Cargo.lock`, reducing dependency surface.

7. **Snyk SAST scan** -- Zero findings across the entire workspace.

### Areas to watch when search implementation lands

- Query input validation (length, pattern complexity for regex/AST patterns)
- Index file integrity (if persisted to disk -- checksums, size limits)
- Memory budget for n-gram index construction (large repositories)
- `FieldClassifier::classify` implementations receiving untrusted AST nodes
