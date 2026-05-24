# Regression Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17T15:13

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

- **Parallel walker TOCTOU allows over-collection before truncation** - `walk.rs:259-272` (Confidence: 65%) — Multiple threads may pass the `file_count.load() >= max_files` check before any increments the counter, causing brief over-collection. The `files.truncate(max_files)` at line 312 corrects this, but the intermediate state allocates more memory than strictly necessary. The current design is acceptable since the overshoot is bounded by thread count.

- **CapReached not guaranteed to appear in skipped list if max_files reached exactly** - `walk.rs:259-266` (Confidence: 62%) — If exactly `max_files` files are accepted and no further entries are visited, `CapReached` will not be pushed (the cap check fires only on the *next* entry). The old sequential code had similar behavior (`break` after pushing), so this is not a new regression, but the parallel version is slightly less deterministic about whether the sentinel appears.

- **Removed help text options could break scripts parsing `--help` output** - `mod.rs:55-74` (Confidence: 60%) — The removal of `--lang`, `--ast`, `--json`, `--limit` from help text is intentional (unimplemented features), but any automated tools parsing help output for supported flags would see a different interface. Since the options were never implemented (query mode returns FAILURE), this is a documentation fix with no functional regression.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Regression Score**: 9/10
**Recommendation**: APPROVED

## Analysis Summary

This PR introduces a well-structured index builder pipeline with careful attention to regression prevention:

**No Regressions Detected:**

1. **`is_tree_sitter_language` replaced by `!lang.is_serde_based()`** — Semantically identical (`!matches!(lang, Json | Yaml | Toml)` in both cases). No behavioral change.

2. **Sequential walker replaced by parallel `build_parallel()`** — Output ordering preserved via post-collection sort (`files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path))`). `files.truncate(max_files)` handles TOCTOU over-collection. Cap enforcement via `AtomicBool` ensures single `CapReached` push.

3. **`unsafe { String::from_utf8_unchecked }` replaced by safe `String::from_utf8().expect()`** — Safety improvement, not a regression. The invariant (hex nibbles are always valid UTF-8) is still documented.

4. **`std::mem::take` replaced by zip-consume pattern** — Same ordering, same data transfer semantics, just explicit ownership via iterators instead of indexed mutation.

5. **`std::env::var_os("SKIM_DEBUG").is_some()` replaced by `crate::debug::is_debug_enabled()`** — The old check treated ANY non-empty value (including "false") as debug-enabled. The new function respects truthiness (`1`/`true`/`yes`). This is a bugfix, not a regression.

6. **DNS module split** — Pure file reorganization. `dns/mod.rs` re-exports `run_dig` and `run_nslookup` at the same path (`dns::run_dig`, `dns::run_nslookup`). All 29 DNS tests pass unchanged.

7. **Help text updated** — Removed unimplemented options from display. Runtime behavior (FAILURE for query mode) unchanged. Existing test `test_search_unimplemented_returns_failure` still passes.

8. **Test consolidation** — `test_index_incremental_manifest_correctness` merged into `test_index_incremental_cache_hits_verified_via_manifest`. All assertions preserved. New tests add coverage for `cache_hits` counter, modified-file SHA change, and `--force` zero-hits guarantee.

**All 2,359+ tests pass with 0 failures.**
