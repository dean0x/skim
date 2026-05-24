# Architecture Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**FileManifest couples persistence and in-memory state without trait abstraction** - `crates/rskim/src/cmd/search/manifest.rs:87-285`
**Confidence**: 82%
- Problem: `FileManifest` combines data storage (HashMap of entries), I/O (file read/write), and validation (root-mismatch detection, version checks) into a single concrete struct with no trait boundary. The `index.rs` pipeline directly instantiates `FileManifest::new(...)` and `FileManifest::load(...)`, making the pipeline untestable without real filesystem I/O. This violates DIP — the pipeline orchestrator depends on a concrete implementation rather than an abstraction.
- Fix: Extract a trait (e.g., `ManifestStore`) with `lookup`, `insert`, `save`, and `load` methods. The pipeline would accept a `&dyn ManifestStore` or generic `M: ManifestStore`, enabling in-memory mock stores for unit testing. The current `FileManifest` becomes the production implementor.

```rust
pub(super) trait ManifestStore {
    fn lookup(&self, path: &str) -> Option<&ManifestEntry>;
    fn insert(&mut self, entry: ManifestEntry);
    fn save(&self) -> anyhow::Result<()>;
}
```

**build_index function has 6 responsibilities (God Function tendency)** - `crates/rskim/src/cmd/search/index.rs:150-249`
**Confidence**: 84%
- Problem: `build_index` performs: (1) cache directory resolution, (2) directory creation, (3) file walking, (4) manifest loading, (5) parallel classification, (6) sequential index building + manifest writing. At 100 lines, it is approaching but has not yet crossed the "god function" threshold. However, mixing orchestration (steps 1-3) with domain logic (steps 4-6) creates a function with multiple reasons to change — violating SRP.
- Fix: Extract phases into composable units. Separate "resolve infrastructure" (steps 1-3) from "classify files" (step 4-5) from "build index + persist" (step 6). Each could be a named function with clear inputs/outputs:

```rust
fn resolve_pipeline_dirs(config: &IndexConfig) -> anyhow::Result<(PathBuf, Vec<ReadFile>, Vec<SkipReason>)> { ... }
fn classify_files(files: &[ReadFile], manifest: &FileManifest, ...) -> Vec<ClassifiedFile> { ... }
fn build_and_persist(files: &[ReadFile], classified: Vec<ClassifiedFile>, ...) -> anyhow::Result<IndexResult> { ... }
```

### MEDIUM

**Atomic write ordering documented but not enforced by type system** - `crates/rskim/src/cmd/search/index.rs:238-240`
**Confidence**: 83%
- Problem: The PR description and code comment state the write order must be `.skpost` -> `.skidx` -> `.skfiles` for coherence. This invariant is critical (a partial write that only produces `.skidx` without `.skpost` would create a corrupt state). However, the ordering is enforced only by sequential code placement — there is no type-state pattern or builder phase that makes it impossible to call `manifest.save()` before `builder.build()`. A future refactor could accidentally reorder these calls.
- Fix: Consider documenting this invariant more prominently (it already exists in the builder.rs doc comment). For additional safety, the `build()` method could return a token type that is consumed by `manifest.save()`, making compilation fail if ordering is violated. However, given the module is `pub(super)` scoped, this is a medium priority.

**walk_and_read returns (Vec<ReadFile>, Vec<SkipReason>) — mixed concerns in return type** - `crates/rskim/src/cmd/search/walk.rs:231-234`
**Confidence**: 80%
- Problem: Returning a tuple of two vectors conflates the "successful results" with "diagnostic data." The caller must destructure and handle both. While this is a common Rust pattern, the skip reasons are purely diagnostic (used only for count display) and could grow large (up to 10,000 entries). Coupling diagnostics with business results means the caller cannot opt out of collecting skip reasons.
- Fix: Wrap in a struct for clarity and future extensibility (e.g., adding walk duration, file-type distribution stats):

```rust
pub(super) struct WalkResult {
    pub files: Vec<ReadFile>,
    pub skipped: Vec<SkipReason>,
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**No error propagation path for `NgramIndexBuilder::add_file_classified` failures** - `crates/rskim/src/cmd/search/index.rs:230`
**Confidence**: 85%
- Problem: If `add_file_classified` returns `Err` (e.g., duplicate FileId, content exceeds u32::MAX), the `?` operator propagates it as a fatal error that aborts the entire build. However, the function's doc comment says "user-facing errors... do not cause a non-zero exit code." This means a single corrupt file can abort the entire 50K-file index build. The fail-soft philosophy stated in the module doc is inconsistent with the actual error handling in the sequential loop.
- Fix: Match on the error and either skip the file (logging to stderr under debug) or propagate only truly unrecoverable errors:

```rust
if let Err(e) = builder.add_file_classified(FileId(file_id), &rf.content, rf.lang, &field_map) {
    if debug_enabled {
        eprintln!("skim search index [debug]: add_file_classified failed for {:?}: {e}", path_key);
    }
    continue; // Skip this file, don't abort entire build
}
```

## Pre-existing Issues (Not Blocking)

### MEDIUM

**rskim-search crate exposes LayerBuilder trait but NgramIndexBuilder::new takes PathBuf** - `crates/rskim-search/src/index/builder.rs:60`
**Confidence**: 80%
- Problem: The `LayerBuilder` trait is defined in the types module as an abstraction, but `NgramIndexBuilder::new()` requires a `PathBuf` (I/O-specific infrastructure detail). This means the trait-based abstraction is not fully honored — you cannot construct a `LayerBuilder` without committing to filesystem I/O. The library advertises itself as "pure" in lib.rs but the builder requires a directory.
- Fix: This is a pre-existing design choice from the `rskim-search` crate foundation and not introduced by this PR. A potential improvement would be a two-phase builder (accumulate in memory, then flush via a separate `IndexWriter`), but this is out of scope.

## Suggestions (Lower Confidence)

- **Consider a pipeline struct** - `crates/rskim/src/cmd/search/index.rs` (Confidence: 72%) — The `build_index` function passes `config` fields to multiple sub-operations. A `Pipeline` struct holding config + derived state (cache_dir, max_files) would make dependencies explicit and enable method chaining for future pipeline stages (e.g., temporal scoring pass).

- **classify_entry does too much for a "classify" function** - `crates/rskim/src/cmd/search/walk.rs:139` (Confidence: 68%) — The function does language detection, size screening, file reading, minification detection, and SHA computation. It is well-structured internally, but "classify" is misleading — "evaluate_entry" or "process_entry" would better describe the multi-step operation.

- **Arc<Mutex<Vec>> pattern in walk_and_read could use channel** - `crates/rskim/src/cmd/search/walk.rs:235-237` (Confidence: 65%) — The parallel walker uses `Arc<Mutex<Vec>>` for collecting results. A crossbeam channel or rayon's parallel collect could reduce lock contention on large repos. However, the `ignore` crate's parallel walker callback API constrains the options here.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Architecture Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The pipeline architecture is well-structured with clear module boundaries (types/walk/manifest/index), proper separation of the library crate (rskim-search) from the CLI orchestration (rskim/cmd/search), and good layering discipline. The two-phase processing model (parallel classification, sequential build) is appropriate given the NgramIndexBuilder's !Sync constraint.

Conditions for full approval:
1. Address the fail-soft inconsistency in the sequential build loop (Should Fix item) — a single file error should not abort a 50K-file build.
2. Consider (not required) extracting a trait for ManifestStore to improve testability of the pipeline without filesystem I/O.

Strengths:
- Clean data flow: types.rs holds pure data, walk.rs handles discovery, manifest.rs handles caching, index.rs orchestrates.
- Dependency direction is correct: CLI (rskim) depends on library (rskim-search), never the reverse.
- Fail-soft design in classification (run_classify falls back to empty field_map) is well-implemented.
- Safety limits are comprehensive (MAX_ANCESTORS, MAX_SKIP_REASONS, MAX_MANIFEST_ENTRIES, MAX_FILE_BYTES, compile-time assertions).
- Atomic write strategy (.skpost -> .skidx -> .skfiles) provides crash-safe coherence.
