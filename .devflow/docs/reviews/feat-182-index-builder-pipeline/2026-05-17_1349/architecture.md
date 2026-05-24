# Architecture Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**`build_index` monolith combines orchestration, cache resolution, and I/O** - `crates/rskim/src/cmd/search/index.rs:150-243`
**Confidence**: 82%
- Problem: The `build_index` function is 93 lines and handles six responsibilities in sequence: cache directory resolution, file walking, manifest loading, parallel classification, sequential index building with manifest accumulation, and final persistence. While each step is delegated to a helper or sub-module, the orchestration function itself mixes concerns that could be tested independently (e.g. cache dir resolution is tested only through the full pipeline). As the pipeline grows (temporal scoring, query index, etc.), this function will accumulate more steps and become harder to reason about.
- Fix: Consider extracting a `Pipeline` struct that holds the config and cache_dir, with methods for each phase. This would make individual phases unit-testable without running the full pipeline. Not blocking because the current size is within the tolerable range for an orchestrator function and the sub-modules are well-separated, but this is the right time to plan the seam.

**`path_keys` mutation via `std::mem::take` couples iteration order to correctness** - `crates/rskim/src/cmd/search/index.rs:186-229`
**Confidence**: 85%
- Problem: The `path_keys` vector is built as `Vec<String>`, then individual elements are consumed via `std::mem::take(&mut path_keys[idx])` inside the sequential builder loop (line 225). This leaves empty strings in `path_keys` at consumed indices. While this works because each index is visited exactly once, it creates an implicit ordering invariant: the loop must process indices in order, and no subsequent code may access `path_keys`. This is a fragile coupling between the iteration pattern and the data structure's state. Any future refactor that re-orders or parallelises the builder loop would silently produce empty path keys.
- Fix: Either consume `path_keys` into the loop via `into_iter().enumerate()` (making the ownership transfer explicit), or clone the key at insertion time. The `take` pattern saves one allocation per file but introduces a subtle correctness hazard:
  ```rust
  // Option A: consume the vec
  for (idx, (rf, path_key)) in read_files.iter().zip(path_keys.into_iter()).enumerate() {
      // path_key is owned, no take needed
      new_manifest.insert(ManifestEntry { path: path_key, ... });
  }
  ```

### MEDIUM

**`find_file_with_ext` test helper uses unbounded recursion** - `crates/rskim/src/cmd/search/index_tests.rs:383-398`
**Confidence**: 83%
- Problem: The `find_file_with_ext` helper recurses into subdirectories without a depth bound. While this is test-only code running against tempdir fixtures (so depth is controlled), it violates the project's reliability principle that "every loop and retry must have a fixed upper bound." A symlink loop in a test fixture (however unlikely) would stack-overflow.
- Fix: Add a depth parameter or use a non-recursive iterator (`walkdir` or a manual stack with a bound). Low severity since it is test-only, but worth aligning with project conventions.

**`unsafe` block in `sha256_hex` lacks safety justification beyond a comment** - `crates/rskim/src/cmd/search/walk.rs:331-332`
**Confidence**: 80%
- Problem: `String::from_utf8_unchecked` is used for the hex encoding result. The safety argument (NIBBLES only contains ASCII hex) is sound but the function is `pub(super)` and could be called from other modules. If `NIBBLES` were ever modified (unlikely but possible), the unsafety would silently propagate. The safe alternative (`String::from_utf8(hex).unwrap()`) adds negligible overhead since the string is always exactly 64 bytes.
- Fix: Replace with the safe variant. The performance gain from `unsafe` on a 64-byte string called once per file is immeasurable:
  ```rust
  String::from_utf8(hex).expect("hex encoding is always valid ASCII")
  ```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`ReadOutcome` enum is private to `walk.rs` but could benefit the broader pipeline** - `crates/rskim/src/cmd/search/walk.rs:62-71`
**Confidence**: 80%
- Problem: `ReadOutcome` is a well-designed typed enum replacing fragile string-matching on I/O errors. However, it is module-private while the pattern it represents (distinguishing "expected skip conditions" from "real errors") is likely needed by other pipeline stages as the search feature grows. Currently the information is thrown away at the `walk_and_read` boundary, which maps everything into `SkipReason` (a debug-only enum with `#[allow(dead_code)]`).
- Fix: No immediate action needed. When the next pipeline stage needs richer error classification, promote `ReadOutcome` or its equivalent to `types.rs`. This is a design note for future work.

## Pre-existing Issues (Not Blocking)

_No critical pre-existing issues identified in reviewed files._

## Suggestions (Lower Confidence)

- **`FileManifest` uses `HashMap<String, ManifestEntry>` with cloned path keys** - `crates/rskim/src/cmd/search/manifest.rs:74,183-185` (Confidence: 65%) -- The `insert` method clones `entry.path` for the key, and `ManifestEntry` already contains the path as a field. An `IndexMap` or a set-like structure keyed by the entry itself could avoid the duplication, but this is a minor allocation concern for a structure sized at most 50K entries.

- **No trait abstraction for the manifest store** - `crates/rskim/src/cmd/search/manifest.rs:68-75` (Confidence: 70%) -- `FileManifest` is a concrete struct with direct filesystem I/O in `load`/`save`. For testing, the index tests create real temp directories and exercise the full I/O path. A trait-based abstraction (e.g. `ManifestStore`) would allow unit-testing the pipeline's cache-hit logic without filesystem round-trips. However, the current approach is consistent with the project's preference for integration tests (CLAUDE.md: "Test behaviors, not implementation"), so this is a style preference rather than a defect.

- **`ClassifiedFile` type alias is a bare tuple** - `crates/rskim/src/cmd/search/index.rs:41` (Confidence: 62%) -- `type ClassifiedFile = (FieldMap, bool)` uses a bare tuple where the `bool` field (cache hit?) has no self-documenting name. A small struct `ClassifiedFile { field_map: FieldMap, cache_hit: bool }` would improve readability at no runtime cost. The alias comment helps but doesn't survive into code that destructures it.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | - | 0 | 1 | 0 |
| Pre-existing | - | - | 0 | 0 |

**Architecture Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Rationale

The architecture of the new `search/` module is well-structured. Key strengths:

1. **Clean module decomposition**: `types.rs` (pure data), `walk.rs` (discovery + I/O), `manifest.rs` (persistence), `index.rs` (orchestration) follow SRP with clear boundaries.
2. **Correct dependency direction**: The `cmd/search/` module depends downward on `rskim-search` (library crate) for `NgramIndexBuilder`, `classify_source`, and `SearchField`. The library crate has no knowledge of CLI concerns. This follows the Clean Architecture dependency rule.
3. **Two-phase processing**: Parallel classification (rayon) followed by sequential builder accumulation correctly respects `NgramIndexBuilder`'s non-`Sync` constraint.
4. **Atomic write ordering**: `.skpost` and `.skidx` are written by `builder.build()`, then the manifest is written last, providing a coherence marker as documented.
5. **Fail-soft error handling**: Classification errors fall back to empty field maps, I/O errors become `SkipReason` entries -- the pipeline never panics on bad input.
6. **Typed error classification**: `ReadOutcome` enum replaces string-matching on I/O errors, which is a sound architectural choice.
7. **Good test coverage**: 25+ tests covering roundtrips, edge cases (empty dir, corrupted manifest, wrong root), incremental builds, and argument validation.

The conditions for approval are minor: the `std::mem::take` mutation pattern should be replaced with explicit ownership transfer to avoid a subtle ordering invariant, and the `unsafe` block should be replaced with a safe alternative given the negligible performance difference on 64-byte strings.
