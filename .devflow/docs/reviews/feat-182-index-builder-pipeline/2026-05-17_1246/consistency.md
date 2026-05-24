# Consistency Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Argument parsing style diverges from sibling subcommands** - `crates/rskim/src/cmd/search/index.rs:91`
**Confidence**: 82%
- Problem: The `index` subcommand now uses clap derive (`#[derive(Parser)]`) for argument parsing, while the sibling `discover` and `learn` subcommands in the same `cmd/` directory use hand-rolled `parse_args()` functions with manual while-loop iteration. This creates two divergent patterns within the same architectural layer.
- Fix: This is an intentional modernization — clap derive is the direction the main CLI (`main.rs:171`) already uses. The inconsistency is temporary and acceptable as a migration step. However, document in a comment or issue tracker that `discover` and `learn` should be migrated to clap derive for parity.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Error signalling via string matching in `open_and_read` caller** - `crates/rskim/src/cmd/search/walk.rs:183-184`
**Confidence**: 85%
- Problem: The caller of `open_and_read` distinguishes "too large" errors via `e.to_string().contains("too large")` — a fragile string comparison. This is inconsistent with how other error types in this codebase use typed variants (e.g., `ErrorKind::InvalidData` is already used for non-UTF-8). The pattern couples the caller to the exact wording of an internal error message.
- Fix: Use a custom error enum or a dedicated `ErrorKind` constant. One approach:

```rust
// In open_and_read, define a constant or use a different ErrorKind:
const TOO_LARGE_MSG: &str = "too large";

// Return:
return Err(io::Error::new(io::ErrorKind::FileTooLarge, TOO_LARGE_MSG));
// Note: FileTooLarge is unstable. Alternative: continue with ErrorKind::Other
// but extract the constant so both producer and consumer reference it.
```

Alternatively, keep the `io::Error::other("too large")` pattern but extract the message to a module-level constant shared between producer and consumer to prevent silent breakage.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`anyhow::Context` usage only in search module** - `crates/rskim/src/cmd/search/walk.rs:22`, `manifest.rs:33` (Confidence: 65%) — The `anyhow::Context as _` import and `.with_context(|| ...)` pattern is used exclusively in the search module. No other `cmd/` subcommands use it (they rely on bare `?` or `anyhow::bail!`). This is not wrong, but creates a subtle style split. Consider adopting or documenting this as the project-wide preference.

- **`discover_project_root` loop bound vs. other unbounded patterns** - `crates/rskim/src/cmd/search/walk.rs:64` (Confidence: 70%) — The `MAX_ANCESTORS = 256` bound on `discover_project_root` is good defensive coding (matches the reliability principle). However, other directory-walking code in the codebase (e.g., `ignore::WalkBuilder`) does not have an explicit depth bound. This is fine for now since `WalkBuilder` is bounded by the filesystem, but worth noting the asymmetry.

- **`HashMap::with_capacity(1024)` in manifest load is a magic number** - `crates/rskim/src/cmd/search/manifest.rs:158` (Confidence: 62%) — The capacity hint `1024` is undocumented. Other `HashMap::new()` calls in the codebase do not pre-allocate. A brief comment explaining the heuristic (e.g., "typical projects have hundreds to low thousands of files") would align with the project's thorough documentation style.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The PR is well-executed with strong internal consistency within its own module. The clap derive migration is a positive modernization that aligns with the main CLI pattern. The `Language::as_str()` method correctly mirrors the `Mode::name()` pattern already established in the codebase. Error type promotion from `std::io::Result` to `anyhow::Result` is consistently applied across all touched functions. The main actionable item is the string-matching error discrimination pattern which introduces a fragile coupling that could silently break.
