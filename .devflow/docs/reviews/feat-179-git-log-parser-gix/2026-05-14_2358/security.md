# Security Review Report

**Branch**: feat/179-git-log-parser-gix -> main
**Date**: 2026-05-14

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Unbounded commit vector allocation** - `crates/rskim-search/src/temporal/git_parser.rs:123`
**Confidence**: 82%
- Problem: `parse_history_impl` collects all commits into an unbounded `Vec<CommitInfo>` (line 123). When `lookback_days` is 0 (no time filter), this walks the entire repository history. For very large repositories (e.g., linux kernel with 1M+ commits, each containing multiple `FileChangeInfo` entries), this could exhaust memory. The `TemporalSource::parse_history` trait provides no mechanism for the caller to limit the number of commits returned.
- Fix: Consider adding a `max_commits` parameter or an internal safety cap:
```rust
const MAX_COMMITS: usize = 100_000;
// ...
if commits.len() >= MAX_COMMITS {
    break;
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Lossy path conversion may silently alter file paths** - `crates/rskim/src/cmd/heatmap/mod.rs:223`
**Confidence**: 80%
- Problem: The new code converts `PathBuf` to `&str` via `f.path.to_str().unwrap_or("")` for the `should_exclude` call. On systems with non-UTF-8 paths, `to_str()` returns `None` and the path becomes empty string `""`, which will never match any exclusion pattern. This means files with non-UTF-8 paths silently bypass all exclusion rules. The previous implementation used `String` paths directly (no lossy conversion needed).
- Fix: Use `to_string_lossy()` instead to preserve best-effort matching:
```rust
.retain(|f| !should_exclude(&f.path.to_string_lossy(), &exclude_set));
```
  This requires `should_exclude` to accept `&str` (which it already does via deref from `Cow<str>`), or adjust the `should_exclude` signature. Alternatively, skip files where `to_str()` returns `None` (exclude them by default as a safe fallback).

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Error message string matching for control flow** - `crates/rskim-search/src/temporal/git_parser.rs:252-258` (Confidence: 65%) -- The `is_unborn_error` function matches on error message substrings to detect unborn repos. If gix changes error wording in a future version, this could misclassify errors, potentially swallowing real git errors as "empty repo". Consider checking for a typed error variant if gix exposes one.

- **`to_str_lossy` on git author/message data** - `crates/rskim-search/src/temporal/git_parser.rs:151,154` (Confidence: 62%) -- Author names and commit messages are converted via `to_str_lossy()`, which replaces invalid UTF-8 with the replacement character. While this is safe for display/analysis purposes, it means two distinct authors with similar names differing only in invalid byte sequences would appear identical. Not a vulnerability, but could affect heatmap author diversity metrics downstream.

- **`max-performance-safe` gix feature set scope** - `Cargo.toml:61` (Confidence: 60%) -- The `max-performance-safe` feature enables optimized SHA computation and zlib backends. While "safe" (no `unsafe` Rust), it pulls in a significant dependency tree. This is a reasonable tradeoff for performance, but worth documenting that the "safe" qualifier refers to Rust `unsafe` avoidance, not a security audit of all transitive dependencies.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Security Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Rationale

This PR introduces a well-structured, pure-Rust git history parser with good security properties:

1. **No unsafe code**: Zero `unsafe` blocks in the entire temporal module.
2. **No shell command injection**: Uses the `gix` library for git operations (pure Rust, no subprocess spawning) -- a significant improvement over shelling out to `git`.
3. **Proper error handling**: All gix errors are converted to `SearchError::Git(String)` at the boundary via `gix_err()`. No panics in production code (clippy `unwrap_used`/`expect_used` are denied in the crate's lint config).
4. **No deserialization of untrusted data**: Git objects are parsed by gix's vetted parser, not custom deserialization.
5. **No secrets/credentials exposed**: No hardcoded secrets. The module only reads git history data.
6. **Type safety at boundaries**: All gix types are converted to owned public types at the parser boundary -- no library types leak into the API.

The two MEDIUM findings (unbounded allocation and lossy path handling) are defense-in-depth concerns, not exploitable vulnerabilities. The unbounded allocation is mitigated in practice by typical repository sizes and the `lookback_days` filter. The lossy path issue is a correctness concern on edge-case platforms rather than a direct security risk.
