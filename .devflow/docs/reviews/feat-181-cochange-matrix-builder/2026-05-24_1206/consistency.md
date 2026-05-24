# Consistency Review Report

**Branch**: feat/181-cochange-matrix-builder -> main
**Date**: 2026-05-24T12:06

## Issues in Your Changes (BLOCKING)

### HIGH

**Mixed f64 casting style in Jaccard computation** - `crates/rskim-search/src/cochange/reader.rs:151`
**Confidence**: 90%
- Problem: Line 151 uses `f64::from(count_ab)` for the numerator but `denominator as f64` for the denominator in the same expression. The rest of the codebase consistently uses `f64::from()` for all numeric conversions (see `index/format.rs:427-433`, `lexical/scoring.rs:40-64`). Mixing `From` trait conversions with `as` casts in a single expression is inconsistent.
- Fix:
```rust
Ok(f64::from(count_ab) / f64::from(denominator))
```
Note: `f64::from(u64)` is not implemented in std, so this would need:
```rust
Ok(f64::from(count_ab) / (denominator as f64))
```
Since `u64 -> f64` has no `From` impl, using `as f64` is acceptable, but being consistent within the expression would still read better. At minimum, add a comment explaining the asymmetry -- `u32` has `From<u32> for f64` but `u64` does not.

**Sub-module visibility `pub(crate)` deviates from existing pattern** - `crates/rskim-search/src/cochange/mod.rs:24-26`
**Confidence**: 85%
- Problem: The cochange module declares its sub-modules as `pub(crate) mod builder/format/reader`, while the existing `index` and `temporal` modules use plain `mod` (private). Since the types are re-exported via `pub use` anyway, `pub(crate)` is unnecessarily wider visibility. The `index` module -- the closest structural analog -- uses `mod builder; mod format; mod reader;`.
- Fix:
```rust
mod builder;
mod format;
mod reader;

pub use builder::CochangeMatrixBuilder;
pub use reader::CochangeMatrixReader;
```

### MEDIUM

**`atomic_write` is a free function instead of an associated method** - `crates/rskim-search/src/cochange/builder.rs:254`
**Confidence**: 82%
- Problem: In the `index` module, `atomic_write` is a private associated method on `NgramIndexBuilder` (`fn atomic_write(dir: &Path, path: &Path, data: &[u8]) -> Result<()>`). In the `cochange` module, it is a standalone free function. The implementations are byte-for-byte identical. Since the `index` module is the architectural precedent for builder/reader/format modules in this crate, the cochange module should follow the same pattern for discoverability. Alternatively, both could share a single utility function, but that would be a separate refactor.
- Fix: Move `atomic_write` into `impl CochangeMatrixBuilder`:
```rust
impl CochangeMatrixBuilder {
    // ... existing methods ...

    fn atomic_write(dir: &Path, path: &Path, data: &[u8]) -> Result<()> {
        let mut tmp = NamedTempFile::new_in(dir)?;
        use std::io::Write as _;
        tmp.write_all(data)?;
        tmp.persist(path).map_err(|e| e.error)?;
        Ok(())
    }
}
```

**Duplicated test helpers `make_history` and `make_path_map`** - `crates/rskim-search/src/cochange/builder_tests.rs:17-51` and `crates/rskim-search/src/cochange/reader_tests.rs:19-53`
**Confidence**: 85%
- Problem: `make_history` and `make_path_map` are copy-pasted identically between `builder_tests.rs` and `reader_tests.rs` (verified via diff -- files are byte-identical). The codebase generally avoids this kind of duplication. The `reader_tests.rs` adds a `build_matrix` helper that wraps both, showing the relationship is tight.
- Fix: Extract shared test helpers into a `test_helpers` module within `cochange/`:
```rust
// cochange/test_helpers.rs (or a #[cfg(test)] mod in mod.rs)
#[cfg(test)]
pub(super) fn make_history(commits: Vec<Vec<&str>>) -> HistoryResult { ... }
#[cfg(test)]
pub(super) fn make_path_map(paths: &[&str]) -> HashMap<PathBuf, FileId> { ... }
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`is_multiple_of` stabilization concern** - `crates/rskim-search/src/cochange/format.rs:256` (Confidence: 65%) -- `usize::is_multiple_of` was stabilized in Rust 1.87 (per tracking issue #128101). The existing `index` module already uses it, so this is consistent with the codebase, but worth noting if the MSRV is below 1.87.

- **Missing `#[must_use]` on reader query methods** - `crates/rskim-search/src/cochange/reader.rs:119,137,163,191` (Confidence: 70%) -- The `NgramIndexReader::stats()` method has `#[must_use]`, but the cochange reader's public query methods (`pair_count`, `jaccard`, `pairs_for_file`, `file_commits`) do not. These return `Result<T>` which Rust already warns about unused Results, so `#[must_use]` is less critical here, but it would match the project's explicit annotation style.

- **`serialize` and `accumulate_pairs` as module-level functions vs associated methods** - `crates/rskim-search/src/cochange/builder.rs:102,178` (Confidence: 62%) -- In the `index` module, the analogous `serialize_index` is an associated method (`&self`) on `NgramIndexBuilder`. In cochange, `serialize` and `accumulate_pairs` are standalone free functions. The index pattern keeps serialization logic scoped to the builder's impl block.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The new `cochange` module closely follows the established `index` module architecture (builder/reader/format split, same file naming, same test-file pattern, same binary codec approach, same atomic-write mechanism, same mmap-based reading, same CRC32 validation). The deviations are relatively minor -- sub-module visibility, function placement, and test helper duplication -- but they create inconsistencies that will compound as the codebase grows. The mixed casting style in the Jaccard method is a small but unnecessary readability issue given the project's strong preference for `f64::from()`.
