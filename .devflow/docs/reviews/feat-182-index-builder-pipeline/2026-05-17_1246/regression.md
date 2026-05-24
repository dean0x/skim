# Regression Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17
**Commits reviewed**: 4 (8a1bef5, 7a7a39e, 3d2a37b, cdc0a22)

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Manifest format incompatibility on incremental build across versions** - `crates/rskim/src/cmd/search/index.rs:223`
**Confidence**: 82%
- Problem: The `lang` field changed from `format!("{:?}", rf.lang).to_lowercase()` to `rf.lang.as_str().to_string()`. For most variants these produce identical strings (`typescript`, `rust`, `python`, etc.). However, if a user has an existing manifest produced by the OLD code on disk and then upgrades to this version, the `lang` field values written will differ in case or format for future consumers that may key on `lang`. Currently the `lang` field is not used for cache-hit decisions (only `sha256` is compared), so no functional regression occurs today. The risk is forward-facing: if any future code adds logic that compares `lang` values across manifest versions, old manifests would fail silently.
- Fix: This is acceptable as-is given that (a) the field is informational-only in the current implementation, and (b) the format version (`FORMAT_VERSION = 1`) is unchanged, which means older manifests will still load and function correctly (cache hits still use `sha256`). Consider bumping `FORMAT_VERSION` to 2 if `lang` ever becomes semantically significant. No action required now.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Clap error message format may differ from previous hand-written errors** - `crates/rskim/src/cmd/search/index.rs:69` (Confidence: 65%) — The migration from hand-rolled argument parsing to clap changes the error message format for invalid arguments (e.g., `"unknown argument: --foo"` becomes clap's standard `"error: unexpected argument '--foo' found"`). Any scripts that parse stderr for specific error message strings would break. This is unlikely in practice for a local developer tool.

- **`open_and_read` uses `io::Error::other("too large")` with string matching** - `crates/rskim/src/cmd/search/walk.rs:183-184` (Confidence: 70%) — The error differentiation logic matches on `e.to_string().contains("too large")` which is fragile. If the error message from `io::Error::other()` changes in a future refactor, the TOCTOU "grew past limit" case would silently fall into the generic `ReadError` bucket instead of `TooLarge`. This is not a regression today but a maintenance risk.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 0 | 0 |

**Regression Score**: 9/10
**Recommendation**: APPROVED

## Rationale

This PR introduces no functional regressions. Key findings:

1. **Write ordering preserved**: The atomic write sequence (.skpost -> .skidx -> .skfiles) is maintained. The refactoring merges the two sequential enumerate loops into one but `build()` still precedes `save()`.

2. **Help routing fix is correct**: Moving the `"index"` subcommand check before the `--help` check prevents `skim search index --help` from being intercepted by the parent help handler. A regression test covers this explicitly.

3. **Return type changes are internal**: `discover_project_root` and `FileManifest::load` changed from `std::io::Result` to `anyhow::Result` but both are `pub(super)` — no external consumers exist.

4. **Incremental build compatibility**: The SHA-256 comparison (the sole mechanism for cache hits) is unchanged. The `lang` field format change is cosmetic for the current implementation.

5. **Error handling improved**: The fail-soft pattern in `run_classify` now provides debug logging. The TOCTOU fix in `open_and_read` strengthens correctness.

6. **Bounded loops**: The `discover_project_root` loop now has an explicit `MAX_ANCESTORS = 256` bound, eliminating a theoretical infinite loop risk.

7. **All 49 relevant tests pass** (22 index + 27 walk) confirming no behavioral regressions.
