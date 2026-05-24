# Reliability Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Error discrimination relies on string matching for "too large" I/O error** - `walk.rs:183-184`
**Confidence**: 85%
- Problem: The code uses `e.to_string().contains("too large")` to detect when a file grew between the pre-screen and open. This couples error classification to the exact message string returned by `io::Error::other("too large")` in `open_and_read`. If that string changes, or if another I/O layer wraps the error, this branch silently becomes dead code and the error falls through to the generic `ReadError` path.
- Fix: Use a custom error enum or a typed wrapper instead of string matching. For example:

```rust
enum ReadError {
    TooLarge,
    NonUtf8,
    Io(io::Error),
}

fn open_and_read(path: &Path) -> Result<String, ReadError> { ... }
```

Alternatively, since both "too large" outcomes result in `SkipReason::TooLarge` and the behavior is non-fatal (file is skipped either way), the practical impact is limited to slightly misleading skip-reason reporting.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`open_and_read` casts `u64` to `usize` without overflow guard on 32-bit targets** - `walk.rs:252`
**Confidence**: 82%
- Problem: `(size as usize).saturating_add(1)` — on a 32-bit platform where `usize` is 32 bits, a file just under 5 MiB (valid per `MAX_FILE_BYTES`) could silently truncate when cast from `u64` to `usize`. The `MAX_FILE_BYTES` constant is 5 MiB which fits in 32-bit `usize`, so the practical risk here is near-zero for the current constant value, but the pattern is fragile if `MAX_FILE_BYTES` is ever raised above 4 GiB.
- Fix: Add a compile-time or runtime assertion that `MAX_FILE_BYTES` fits in `usize`:

```rust
// Compile-time guard (placed at module level):
const _: () = assert!(MAX_FILE_BYTES <= usize::MAX as u64);
```

This makes the cast provably safe.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Manifest entry count has no upper bound on parse** - `manifest.rs:158-169` (Confidence: 65%) — The `HashMap::with_capacity(1024)` pre-allocates for load, but there is no cap on how many entries can be parsed from a corrupt/crafted manifest file. On a malicious 50 GB manifest file, this could exhaust memory. In practice, the manifest is written by this process and lives in a user-owned cache directory, so exploitation is unlikely.

- **`walk_and_read` `skipped` Vec grows without bound** - `walk.rs:101` (Confidence: 62%) — On a project with millions of unsupported files (e.g. node_modules without .gitignore), the `skipped` vector could grow very large since skip reasons are collected per-file. The walker does respect .gitignore which mitigates this in practice.

- **`discover_project_root` fallback path** - `walk.rs:74-75` (Confidence: 60%) — If MAX_ANCESTORS (256) is exhausted without finding `.git` and without `parent()` returning `None`, the function falls through to the fallback. This cannot happen in practice (filesystem roots always return `None` from `parent()`), but the intent is slightly ambiguous between "hit ancestor limit" and "reached filesystem root".

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Reliability Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The PR demonstrates strong reliability practices overall:
- Bounded iteration in `discover_project_root` (MAX_ANCESTORS = 256) replaces the previous unbounded loop -- a clear improvement.
- The `open_and_read` function eliminates a TOCTOU race by using the file handle for both size check and read.
- Pre-allocation patterns are used correctly (`String::with_capacity`, `HashMap::with_capacity`).
- The `to_u32_capped` helper provides explicit overflow handling for display counters.
- The FileId overflow guard (`u32::try_from(idx)`) is a good defensive check.
- Atomic write ordering (skpost, skidx, then manifest) ensures readers never see partial state.
- Fail-soft error handling in `run_classify` allows indexing to continue past individual file failures.

The one condition is addressing the string-matching error discrimination pattern which, while non-fatal, introduces a fragile coupling that could mask errors silently.
