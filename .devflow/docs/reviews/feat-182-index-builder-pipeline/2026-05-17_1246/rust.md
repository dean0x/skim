# Rust Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**String-matching for error discrimination is fragile** - `walk.rs:183-185`
**Confidence**: 82%
- Problem: The `walk_and_read` function discriminates the "too large" error from `open_and_read` by checking `e.to_string().contains("too large")` against `ErrorKind::Other`. While both producer and consumer are in the same module today, this pattern is brittle — a wording change in the error message silently reclassifies the error as `ReadError` instead of `TooLarge`.
- Fix: Define a local constant or use a custom error enum to avoid string-matching:
```rust
// In open_and_read: use a typed variant instead of string matching
const TOO_LARGE_MSG: &str = "too large";
// ...
return Err(io::Error::other(TOO_LARGE_MSG));

// In walk_and_read:
} else if e.kind() == io::ErrorKind::Other
    && e.to_string().contains(TOO_LARGE_MSG)
{
```
Or better: have `open_and_read` return a custom enum that avoids string discrimination entirely.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`#[must_use]` on `Language::as_str()`** - `crates/rskim-core/src/types.rs:129` (Confidence: 65%) — Pure functions returning `&'static str` benefit from `#[must_use]` to catch accidental discard, consistent with `effective_max_files()` which already has the annotation.

- **`HashMap::with_capacity` uses hardcoded 1024** - `manifest.rs:158` (Confidence: 62%) — The manifest parser pre-allocates `HashMap::with_capacity(1024)` regardless of actual project size. For projects with significantly more files (e.g., 50K), this causes many rehashes; for tiny projects, it wastes 40 KB. A heuristic based on file size or a smaller initial capacity with growth would be more appropriate, but the performance impact is negligible relative to I/O.

- **`eprintln!` uses `{:?}` on `&str` producing quoted output** - `index.rs:267` (Confidence: 60%) — `lang.as_str()` returns `&str` but is formatted with `{:?}`, which wraps the value in quotes (e.g., `"rust"` instead of `rust`). Using `{}` would produce cleaner debug output. Minor cosmetic issue.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

## Notes

This PR demonstrates strong Rust patterns throughout:

1. **Ownership and borrowing**: Pre-computed `path_keys: Vec<String>` avoids duplicate allocations across the classify and manifest-write phases. `par_iter().zip()` cleanly borrows both slices.

2. **Error handling**: Consistent use of `anyhow::Result` with `.with_context()` for rich diagnostics. The `run_classify` fail-soft pattern (log + empty fallback) is appropriate for a non-critical classification failure that should not abort the entire index build.

3. **Type safety**: The `u32::try_from(idx)` guard with explicit error message is correct — it prevents a silent truncation that `as u32` would cause. The `to_u32_capped` helper encapsulates the saturating pattern cleanly.

4. **TOCTOU fix**: The `open_and_read` function correctly addresses the stat-then-read race by using the file handle for both metadata and read operations.

5. **Bounded iteration**: `discover_project_root` uses `for _ in 0..MAX_ANCESTORS` instead of an unbounded `loop`, satisfying the reliability principle.

6. **Atomic writes**: Manifest persistence via `NamedTempFile` + `BufWriter` + `flush()` + `persist()` is the correct pattern for crash-safe writes.

7. **Clap derive migration**: Clean replacement of manual argument parsing with `clap::Parser` derive, including proper `value_parser` for domain validation (`parse_positive_usize`).

The single MEDIUM finding (string-matching for error discrimination) is a minor robustness concern that does not affect correctness today but could silently degrade if the error message is ever changed.
