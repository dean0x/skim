# Security Review Report

**Branch**: feat/rskim-search-foundation -> main
**Date**: 2026-05-10
**PR**: #213

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

- **Unbounded `SearchQuery.text` field** - `crates/rskim-search/src/types.rs:108` (Confidence: 70%) -- The `text` field on `SearchQuery` accepts any `impl Into<String>` with no length limit. When a future implementation accepts user-supplied queries (e.g., from CLI args or HTTP), an extremely large string could cause excessive memory allocation. Consider adding a `MAX_QUERY_LEN` constant and validating in `SearchQuery::new()` before the search layer is implemented.

- **`SearchResult` deserialization of `score: f64` without validation** - `crates/rskim-search/src/types.rs:149` (Confidence: 65%) -- `SearchResult` now derives `Deserialize`. A malicious JSON payload could set `score` to `NaN` or `Infinity`, which could cause panics or incorrect ordering in downstream sorting. Since the type intentionally omits `PartialEq` due to NaN concerns, consider adding `#[serde(deserialize_with = "...")]` to reject non-finite values when the deserialization path is used in production.

- **`FileId(pub u32)` inner field is public** - `crates/rskim-search/src/types.rs:30` (Confidence: 60%) -- The doc comment explains the rationale ("index builders need to construct FileId values directly"), but a public inner field means any consumer can fabricate arbitrary FileId values. This is an intentional design choice documented in the code, but worth noting for future layers that look up files by FileId -- they must validate existence rather than trusting the ID.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Security Score**: 9/10
**Recommendation**: APPROVED

## Analysis Notes

### What was reviewed

This PR introduces Wave 0 of the `rskim-search` crate -- pure types, traits, and error handling for a 3-layer code search system. The changes comprise:

1. **New types and traits** (`crates/rskim-search/src/types.rs`): `FileId`, `SearchField`, `TemporalFlags`, `SearchQuery`, `SearchResult`, `IndexStats`, `SearchLayer` trait, `LayerBuilder` trait, `FieldClassifier` trait, `SearchError` enum, and a `Result` type alias.
2. **New CLI stub** (`crates/rskim/src/cmd/search.rs`): A stub `run()` function that prints help or "not yet implemented". No user input is processed beyond `--help`/`-h` flag matching.
3. **Explicit re-exports** (`crates/rskim-search/src/lib.rs`): Changed from `pub use types::*` to explicit named re-exports.
4. **Workspace-wide edition 2024 formatting**: ~100 files with purely cosmetic let-chain reformatting (no logic changes).
5. **Removed `rskim-search` dependency from `crates/rskim/Cargo.toml`**: The CLI binary no longer depends on the search library crate (stub is self-contained).

### Security posture assessment

**Positive security patterns observed:**

- **Strict clippy lints** in `crates/rskim-search/Cargo.toml`: `unwrap_used = "deny"`, `expect_used = "deny"`, `panic = "deny"`. This prevents panic-inducing code in the library.
- **No I/O in the library crate**: The architecture explicitly separates I/O (CLI layer) from pure types/traits (library). This boundary prevents the library from introducing file system, network, or process-spawning attack surface.
- **Typed error handling**: `SearchError` uses `thiserror` with explicit variants rather than string-based errors, preventing error message injection.
- **Result types throughout**: All trait methods return `Result<T>`, following the project's "never throw" principle.
- **No `unsafe` code** anywhere in the new code.
- **No secrets, credentials, or hardcoded keys** in any changed file.
- **No new dependencies** with security implications -- `thiserror`, `serde`, and `tree-sitter` are well-established crates already in the workspace.
- **Serde `rename_all = "snake_case"`** on `SearchField` ensures consistent serialization without exposing Rust naming internals.
- **`#[must_use]`** on constructor and accessor methods prevents silently discarding results.

**Snyk SAST scan**: 0 issues found across the entire project.

### Why no blocking issues

The PR is Wave 0 -- pure types and traits with no implementations that process external input. The CLI stub (`search.rs`) only checks for `--help`/`-h` flags using safe string comparison and prints static text. No user-controlled data flows into any computation, serialization, file system operation, or subprocess invocation. The formatting changes across ~100 files are purely cosmetic (Rust edition 2024 let-chain style) with no behavioral impact.
