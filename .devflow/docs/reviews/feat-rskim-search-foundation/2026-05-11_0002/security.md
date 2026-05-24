# Security Review Report

**Branch**: feat/rskim-search-foundation -> main
**Date**: 2026-05-11

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

- **Unbounded `SearchQuery.text` field** - `crates/rskim-search/src/types.rs:122` (Confidence: 65%) — The `SearchQuery::new()` constructor accepts arbitrary-length strings with no maximum length check. When future implementations consume this query against an index, an extremely large query string could cause excessive memory allocation or CPU usage in downstream BM25F scoring. Consider documenting or enforcing an upper bound (e.g., 10KB) at the boundary layer when parsing CLI input.

- **No `limit` default cap on `SearchQuery`** - `crates/rskim-search/src/types.rs:130` (Confidence: 60%) — The `limit` field is `Option<usize>` with no enforced maximum. A `None` limit or excessively large value could cause the search layer to return an unbounded result set, consuming memory proportional to index size. The CLI stub currently does not parse queries, but when it does, a sensible default cap (e.g., 1000) at the boundary would prevent resource exhaustion.

- **Edition 2024 if-let chaining correctness in security-sensitive path** - `crates/rskim/src/cmd/session/claude.rs:69-76` (Confidence: 70%) — The symlink traversal guard was refactored from nested `if` to if-let chaining. The new code `if let Ok(canonical_path) = path.canonicalize() && !canonical_path.starts_with(&canonical_root)` preserves the original semantics (skip files that resolve outside the projects dir), but note that when `canonicalize()` fails (Err), the file is no longer skipped — it proceeds to be processed. This matches the original behavior (the guard only triggered on `Ok`) and is acceptable since a failed canonicalize will likely fail at the subsequent `read_to_string`, but it is worth noting that uncanonicalizeable paths bypass the traversal check.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Security Score**: 9/10
**Recommendation**: APPROVED

## Analysis Notes

This PR introduces a foundation crate (`rskim-search`) with pure types and traits — no I/O, no network operations, no filesystem access, no user input parsing, no cryptography, no authentication, and no command execution. The security surface is minimal by design.

**Positive security observations:**

1. **Pure library architecture** — `rskim-search` is explicitly documented as "NO I/O" with all filesystem and CLI operations deferred to the binary crate. This separation of concerns is excellent for security (the library cannot introduce injection, SSRF, or path traversal vulnerabilities).

2. **Strict clippy lints** — `Cargo.toml` denies `unwrap_used`, `expect_used`, and `panic` in non-test code, preventing uncontrolled panics in production paths.

3. **Typed error handling** — Uses `thiserror` with a proper `SearchError` enum and `Result<T>` alias throughout. No `unwrap()` in library code.

4. **Edition 2024 if-let chaining** — The bulk of changes (52 collapsible_if fixes) are mechanical refactors from nested `if let` to if-let chaining. Manual review of all security-sensitive paths (symlink traversal guard, session ID validation, hook integrity check, input size bounds) confirms the refactored logic preserves original semantics exactly.

5. **Existing security controls preserved** — The hook mode retains its bounded stdin read (`HOOK_MAX_STDIN_BYTES`), session ID validation (`is_safe_session_id`), symlink traversal protection, integrity checks, and timeout watchdog. None of these were weakened.

6. **`thiserror` 1.0 to 2.0 upgrade** — This is a drop-in semver-compatible upgrade with no security implications (thiserror is a proc-macro with no runtime attack surface).

7. **`FileId(pub u32)` newtype** — Provides type safety for file identifiers, preventing accidental confusion between file IDs and other integer values.
