# Architecture Review Report

**Branch**: refactor-230-232-233-tech-debt-pipeline -> main
**Date**: 2026-05-17T23:15

## Issues in Your Changes (BLOCKING)

### HIGH

**Producer thread captures `manifest` by move without `Send` verification comment** - `crates/rskim/src/cmd/search/index.rs:226-245`
**Confidence**: 82%
- Problem: The producer thread closure captures `manifest` (a `FileManifest`) and `walk_entries` (a `Vec<WalkEntry>`) by move. While these types are `Send` (they contain `HashMap<String, ManifestEntry>` and `Vec<WalkEntry>` respectively, both `Send`), the ownership transfer across thread boundaries is a significant architectural concern. If `FileManifest` ever gains a non-`Send` field (e.g., an `Rc`, file handle, or DB connection), this will become a compile error with no comment explaining why `Send` is required. The current code relies on implicit compiler guarantees without documenting the contract.
- Fix: Add a compile-time assertion or a brief comment at the `thread::spawn` call site documenting that `manifest` and `walk_entries` must remain `Send`:
```rust
// Both `manifest` and `walk_entries` are moved into the producer thread.
// If FileManifest ever gains a non-Send field, this will fail to compile —
// that's intentional: the producer must own its data without shared state.
let producer_handle = std::thread::spawn(move || {
```

**Mtime field persisted but never used for pre-screening in `read_and_classify`** - `crates/rskim/src/cmd/search/index.rs:361-378`
**Confidence**: 85%
- Problem: The PR description and commit message describe "4-tier mtime/SHA cache logic" and "mtime pre-screening", and the `WalkEntry` and `ManifestEntry` types now carry an `mtime: Option<u64>` field. However, the actual `read_and_classify` function at line 361-378 does not use `entry.mtime` for any pre-screening. It always reads the file content and computes the SHA-256, then checks the manifest by SHA alone. The mtime field is collected, forwarded through `ProcessedFile`, and persisted to the manifest, but never consulted as a cache optimization. This is dead infrastructure — the "mtime pre-screening hint" documented everywhere is not implemented.
- Fix: Either (a) implement the mtime pre-screening as documented (skip SHA computation when mtime matches), or (b) update all documentation and commit messages to accurately describe the current state as "mtime collection for future use" rather than "4-tier mtime/SHA cache logic." The current state is misleading — the code and docs disagree.

### MEDIUM

**`Pipeline::run` method is 134 lines — still a god method despite struct extraction** - `crates/rskim/src/cmd/search/index.rs:184-318`
**Confidence**: 85%
- Problem: Commit 822ca98 extracted `Pipeline` from the monolithic `build_index` function and described four private stage methods (`walk`, `load_manifest`, `classify`, `build_and_write`). However, the subsequent streaming rewrite in commit 07b091c collapsed all stages back into a single `run()` method spanning lines 184-318 (134 lines). This undoes the decomposition claimed by commit 2. The `run()` method now handles: (1) metadata walk, (2) empty-project early return, (3) manifest loading, (4) channel creation, (5) producer thread spawn, (6) consumer loop with indexing, (7) producer join, (8) index build/flush, (9) manifest save, and (10) result aggregation. This is 10 distinct responsibilities in one method.
- Fix: Extract discrete stages. The producer thread body (lines 226-245) could be a named function (it already delegates to `read_and_classify`, so this is lightweight). The consumer loop (lines 251-292) could be extracted into a `consume_and_index` method. The join + finalize logic (lines 294-317) could be `finalize`. This preserves the streaming architecture while restoring the decomposition that commit 2 intended:
```rust
impl<'cfg> Pipeline<'cfg> {
    pub(super) fn run(self) -> anyhow::Result<IndexResult> {
        let (walk_entries, walk_skips) = self.walk()?;
        if walk_entries.is_empty() { return self.empty_result(walk_skips.len()); }
        let manifest = self.load_manifest()?;
        let (rx, producer_handle, producer_skips) =
            self.spawn_producer(walk_entries, manifest);
        let (builder, new_manifest, next_file_id, cache_hits) =
            self.consume(rx)?;
        self.finalize(producer_handle, builder, new_manifest,
                      next_file_id, cache_hits, walk_skips.len(), producer_skips)
    }
}
```

**Code duplication between `classify_entry` and `classify_entry_metadata`** - `crates/rskim/src/cmd/search/walk.rs:144-218` and `walk.rs:311-353`
**Confidence**: 84%
- Problem: `classify_entry` (test-only, lines 144-218) and `classify_entry_metadata` (production, lines 311-353) share identical logic for file-type check, language detection, and size pre-screening. The test-only version additionally reads content and checks minification. This is a maintenance risk: any change to the classification rules (e.g., a new skip condition, a changed size threshold) must be applied in two places, and divergence would silently cause test behavior to differ from production behavior.
- Fix: Extract the shared prefix (file-type check, language detection, size pre-screen, mtime extraction, rel_path computation) into a shared helper that returns a "metadata-classified entry" struct. The test-only `classify_entry` can call this helper and then layer on content reading and minification checks. This ensures both paths stay synchronized:
```rust
struct ClassifiedMeta {
    abs_path: PathBuf,
    rel_path: PathBuf,
    lang: Language,
    mtime: Option<u64>,
}

fn classify_entry_core(entry: &ignore::DirEntry, root: &Path) -> Result<ClassifiedMeta, MetaOutcome> { ... }
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`walk_metadata` duplicates the parallel-collection-and-extract pattern from `walk_and_read`** - `crates/rskim/src/cmd/search/walk.rs:370-453`
**Confidence**: 80%
- Problem: `walk_metadata` (lines 370-453) and `walk_and_read` (lines 236-290) share the same structural pattern: create `Arc<Mutex<Vec<T>>>` for results and skips, create atomics for count and cap, clone root, configure builder, run parallel walker, unwrap arcs, truncate, sort. The only differences are the entry classification function called and the element type collected. This is 80+ lines of duplicated scaffolding.
- Fix: This is a known trade-off in this PR since `walk_and_read` is now `#[cfg(test)]` only. The duplication is acceptable given that the test code path is frozen and will not evolve. No action needed unless `walk_and_read` is ever promoted back to production.

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`ReadOutcome` is not a `Result` type** - `crates/rskim/src/cmd/search/walk.rs:76-85`
**Confidence**: 80%
- Problem: `ReadOutcome` is a 4-variant enum where `Content` is the success case and the other three are failure modes. The project's engineering principles mandate Result types for all fallible operations. Using a custom enum instead of `Result<String, ReadError>` where `ReadError` is an enum of `{NonUtf8, TooLarge(u64), Io(io::Error)}` means callers must pattern-match all four arms instead of using `?` propagation or combinators.
- Fix: Refactor in a separate PR to `Result<String, ReadError>` with a `ReadError` enum. The match arms in consumers would become more idiomatic.

## Suggestions (Lower Confidence)

- **Channel capacity as a configurable parameter** - `crates/rskim/src/cmd/search/index.rs:162` (Confidence: 65%) -- The `CHANNEL_CAPACITY = 64` constant controls peak memory (up to 320 MiB). For memory-constrained environments or very large files, users may want to tune this. Consider making it part of `IndexConfig` with a sensible default.

- **Producer error details lost** - `crates/rskim/src/cmd/search/index.rs:238` (Confidence: 70%) -- When `read_and_classify` returns `Err(_reason)`, the `SkipReason` is discarded (only counted). Under `--debug`, it would be useful to log the specific skip reason, similar to how `add_file_classified` errors are logged at line 266-269.

- **`ProcessedFile` carries `content: String` across thread boundary** - `crates/rskim/src/cmd/search/types.rs:108-123` (Confidence: 62%) -- The `ProcessedFile` struct sends potentially large `String` content through the channel. An alternative design would use `Arc<str>` or `Bytes` to enable zero-copy sharing if the content were ever needed by multiple consumers. This is speculative for the current single-consumer design.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Architecture Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The streaming producer-consumer architecture is a well-motivated improvement that bounds peak memory proportional to channel capacity rather than total project size. The `Pipeline` struct extraction, `WalkEntry` / `ProcessedFile` type separation, and the new `walk_metadata` function all represent sound architectural decisions that improve separation of concerns.

The two HIGH findings are worth addressing before merge: (1) the "mtime pre-screening" feature is documented and plumbed through the type system but never actually used for pre-screening — the code always reads content and computes SHA regardless of mtime, making the documentation misleading; and (2) the `Pipeline::run` method re-accumulated all stages into a single 134-line method, undoing the decomposition that commit 2 explicitly introduced. Both are tractable fixes that would bring the implementation into alignment with its documented design.
