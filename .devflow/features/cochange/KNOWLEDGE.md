---
feature: cochange
name: Co-Change Matrix
description: "Use when implementing co-change coupling queries, modifying the .skcc binary format, adding new query methods to CochangeMatrixReader, debugging Jaccard similarity calculations, or working with the SQLite temporal persistence layer for co-change pairs. Keywords: cochange, co-change, coupling, jaccard, skcc, binary format, cochange.skcc, CochangeMatrixBuilder, CochangeMatrixReader, CochangeRow, TemporalDb, HistoryResult, COUPLING_MAX_FILES, builder_tests, format_tests, reader_tests, test_helpers, atomic_write, io_util, MIN_JACCARD_THRESHOLD, cochanges_for_file, UNION ALL."
category: architecture
directories: [crates/rskim-search/src/cochange/]
referencedFiles:
  - crates/rskim-search/src/cochange/mod.rs
  - crates/rskim-search/src/cochange/builder.rs
  - crates/rskim-search/src/cochange/format.rs
  - crates/rskim-search/src/cochange/reader.rs
  - crates/rskim-search/src/cochange/builder_tests.rs
  - crates/rskim-search/src/cochange/format_tests.rs
  - crates/rskim-search/src/cochange/reader_tests.rs
  - crates/rskim-search/src/cochange/test_helpers.rs
  - crates/rskim-search/src/types.rs
  - crates/rskim-search/src/lib.rs
  - crates/rskim-search/src/temporal/storage.rs
  - crates/rskim-search/src/temporal/storage_types.rs
  - crates/rskim-search/src/temporal/storage_ops.rs
  - crates/rskim-search/src/io_util.rs
created: 2026-06-21
updated: 2026-07-01
version: 4
---

# Co-Change Matrix

## Overview

The `cochange` module computes and persists a **file co-change matrix** — a
compact binary index of which files tend to change together across commits. It
provides:

1. **`CochangeMatrixBuilder` / `CochangeMatrixReader` / `.skcc` binary file** —
   a build-once, mmap-based format for computing Jaccard similarity across all
   file pairs. Available as a library primitive but **no longer called by the
   CLI temporal build pipeline** (see Architecture note below).
2. **`CochangeRow` / `MIN_COCHANGE_JACCARD`** — the data types and thresholds
   used by `temporal_build::build_cochange_rows` to populate the SQLite `cochange`
   table, which serves `--blast-radius` queries at query time.

## Architecture Note: `.skcc` vs. inline `build_cochange_rows`

The CLI temporal build pipeline (`temporal_build.rs`) does **not** use
`CochangeMatrixBuilder` or `CochangeMatrixReader`. It uses its own inline
`build_cochange_rows(history: &HistoryResult) -> Vec<CochangeRow>` function that
applies the same Jaccard formula as `CochangeMatrixReader::jaccard` but writes
results directly to `Vec<CochangeRow>` without the intermediate `.skcc` binary.

The `.skcc` format (`CochangeMatrixBuilder` / `CochangeMatrixReader`) survives
as a reusable library primitive (e.g. for future large-corpus batch analysis)
but is currently a build artefact not read at query time by any CLI path.

At query time, `--blast-radius` reads from `temporal.db` (SQLite) via
`TemporalDb::cochanges_for_file` — NOT from `.skcc`.

## Module Structure

```
cochange/
  mod.rs          — public re-exports: CochangeMatrixBuilder, CochangeMatrixReader,
                    COUPLING_MAX_FILES; test_helpers pub(super) for tests
  builder.rs      — CochangeMatrixBuilder: accumulates pairs from HistoryResult,
                    writes cochange.skcc atomically
  format.rs       — pure binary codec (no I/O): SkccHeader, FileCommitEntry,
                    PairEntry, encode/decode/lookup helpers, CRC32 checksum
  reader.rs       — CochangeMatrixReader: mmap-based read-only query interface
  builder_tests.rs — co-located tests for builder (included via #[path])
  format_tests.rs  — co-located tests for format codec
  reader_tests.rs  — co-located tests for reader
  test_helpers.rs  — pub(super) shared helpers for all three test modules
```

## On-Disk Format: `.skcc`

File: `cochange.skcc` in the cache directory.

Magic: `b"SKCC"`, `FORMAT_VERSION = 1`.

```
[SkccHeader: 18 bytes]
  [0..4]   magic:       b"SKCC"
  [4..6]   version:     u16 = 1  (FORMAT_VERSION)
  [6..10]  pair_count:  u32
  [10..14] file_count:  u32
  [14..18] checksum:    u32 (CRC32 of file_commit ++ pair bytes)

[FileCommitEntry × file_count: 8 bytes each, sorted by file_id ASC]
  [0..4] file_id:      u32
  [4..8] commit_count: u32

[PairEntry × pair_count: 12 bytes each, sorted by (file_a, file_b) ASC]
  [0..4]  file_a: u32  (always < file_b — canonical ordering)
  [4..8]  file_b: u32
  [8..12] count:  u32
```

All integers are little-endian. The CRC32 covers `file_commit_bytes ++ pair_bytes`
(the payload after the header). The checksum is validated at `open()` time.

## Public API

### `CochangeMatrixBuilder`

```rust
pub struct CochangeMatrixBuilder { output_dir: PathBuf }

pub fn new(output_dir: PathBuf) -> Result<Self>  // Err if dir does not exist
pub fn build(
    &self,
    history: &HistoryResult,
    path_map: &HashMap<PathBuf, FileId>,
) -> Result<CochangeStats>
```

`build` delegates to `build_with_limit` (pub(crate)) with `MAX_PAIRS = 2_000_000`.
Tests can use `build_with_limit` with a smaller cap to exercise the capacity error
path cheaply.

`build` writes `cochange.skcc` atomically via `crate::io_util::atomic_write`
(`NamedTempFile::new_in + write_all + sync_all + persist`) — readers never see
a partial write.

### `CochangeMatrixReader`

```rust
pub struct CochangeMatrixReader { mmap: Mmap, fc_start, fc_end, pairs_end }

pub fn open(dir: &Path) -> Result<Self>
    // validates magic, version, size, CRC32

pub fn pair_count(&self, a: FileId, b: FileId) -> Result<u32>
    // binary search; returns 0 for absent pairs; canonicalises (min,max)

pub fn jaccard(&self, a: FileId, b: FileId) -> Result<f64>
    // Jaccard(a,b) = count_ab / (count_a + count_b - count_ab)
    // returns 0.0 for self-pairs, absent pairs, zero denominator

pub fn pairs_for_file(&self, file_id: FileId) -> Result<Vec<(FileId, u32)>>
    // O(log(pairs) + k); sorted by count DESC, FileId ASC tie-break
    // Scans (file_b == id) in prefix then (file_a == id) in contiguous block

pub fn file_commits(&self, file_id: FileId) -> Result<u32>
    // binary search over FileCommitEntry; returns 0 for unknown FileId
```

`CochangeMatrixReader` is `Send + Sync` — `Mmap` auto-derives both traits
(read-only, no interior mutation). On POSIX, atomic rename in builder means
existing mmaps continue referencing the old inode even after a rebuild.

## Algorithm: Pair Accumulation

1. Iterate commits in `HistoryResult.commits`.
2. Skip commits touching more than `COUPLING_MAX_FILES = 50` files (bulk
   refactors pollute the coupling signal).
3. Resolve each changed file's path to a `FileId` via `path_map`. Paths absent
   from `path_map` are counted in `CochangeStats::unknown_paths_skipped`.
4. Sort and dedup the resolved IDs (a path can appear twice in one commit, e.g.
   rename-with-modify; dedup prevents self-pairs).
5. Update per-file commit counts.
6. Generate canonical `(min(a,b), max(a,b))` pairs using a double-loop over the
   sorted-dedup'd slice. Since the slice is already sorted, `ids[i] < ids[j]`
   always, so no `.min()`/`.max()` is needed — the inner assignment is always
   canonical by construction.
7. Cap total distinct pairs at `MAX_PAIRS = 2_000_000` via
   `SearchError::CapacityExceeded`. Existing keys can still be incremented when
   at capacity; only new-key insertion is blocked.

## Key Constants

| Constant | Value | Location |
|---|---|---|
| `COUPLING_MAX_FILES` | 50 | `builder.rs` (pub — also imported by temporal_build) |
| `MAX_PAIRS` | 2,000,000 | `builder.rs` (pub(crate)) |
| `FORMAT_VERSION` | 1 | `format.rs` (pub(crate)) |
| `HEADER_SIZE` | 18 | `format.rs` |
| `FILE_COMMIT_ENTRY_SIZE` | 8 | `format.rs` |
| `PAIR_ENTRY_SIZE` | 12 | `format.rs` |

## Crate-Root Re-Exports

From `rskim_search`:
```rust
pub use cochange::{COUPLING_MAX_FILES, CochangeMatrixBuilder, CochangeMatrixReader};
```

`CochangeRow`, `MIN_COCHANGE_JACCARD` are exported from `temporal::storage`:
```rust
pub use temporal::storage::{CochangeRow, MIN_COCHANGE_JACCARD, ...};
```

`CochangeStats` is exported from `types`:
```rust
pub use types::{CochangeStats, ...};
```

## Relationship with SQLite Temporal Layer

**Current CLI path (since inline `build_cochange_rows` was introduced):**

`temporal_build::build_cochange_rows(history)` computes `Vec<CochangeRow>`
directly from `HistoryResult` using the same Jaccard formula as
`CochangeMatrixReader::jaccard`, applying `MIN_COCHANGE_JACCARD` as a filter,
and returns rows for `TemporalDb::sync`. No `.skcc` file is written or read
on this path.

**Historical / library path (CochangeMatrixBuilder — not used by CLI since
`build_cochange_rows` was introduced):**
1. `CochangeMatrixBuilder::build` writes `cochange.skcc`.
2. `CochangeMatrixReader::open` + `pairs_for_file` + `jaccard` enumerate pairs.
3. Results converted to `CochangeRow` and stored via `TemporalDb::store_cochanges`.

At query time, `--blast-radius` always uses `TemporalDb::cochanges_for_file`
(SQLite) — the `.skcc` file is never opened at query time.

## Anti-Patterns

- **Not sorting + deduping commit IDs before `generate_pairs`**: the canonical-
  pair algorithm (`a < b` by construction) silently breaks if the IDs are not
  sorted. Self-pairs (`a == a`) would also appear if not deduped.

- **Comparing or passing `file_id` values across different indexing runs**: FileId
  values are positional indices tied to the file ordering of a specific build. A
  reader opened against `cochange.skcc` from a different build has mismatched
  FileIds. The CLI always rebuilds the `.skcc` together with the other indexes
  if `CochangeMatrixBuilder` is used.

- **Relying on `pairs_for_file` for exact membership when `file_id` is absent**:
  returns `Ok(vec![])` (same as a file with no pairs), not an error.

- **Opening the `.skcc` reader on a partially-written file**: never possible in
  production because builder uses `atomic_write`. In tests, always use the helper
  that writes to a temp dir.

- **Reusing the same `path_map` across an incremental rebuild**: FileIds in the
  `.skcc` correspond to the `path_map` used during that specific build run. If
  the file set changes between builds, the path_map and the `.skcc` must be
  regenerated together.

- **Calling `CochangeMatrixBuilder::build` from CLI temporal code**: the CLI
  temporal build path uses `build_cochange_rows` (inline, no `.skcc`). Only use
  `CochangeMatrixBuilder` for standalone batch analysis outside the CLI pipeline.

## Gotchas

- **`COUPLING_MAX_FILES` is `pub`** (not `pub(crate)`) specifically so that
  `crates/rskim/src/cmd/search/temporal_build.rs` can import it instead of
  re-declaring its own constant. Do not lower its visibility without updating the
  CLI layer.

- **`generate_pairs` double-loop is O(k²) per commit** where k is the number of
  files. Because commits over `COUPLING_MAX_FILES = 50` are skipped, k is bounded
  at 50, making the worst-case per-commit cost 50×49/2 = 1225 pair lookups. This
  is intentional and documented.

- **Canonical pair ordering is `(min, max)` but checked via `debug_assert!`**:
  the `a < b` invariant is validated in debug builds. A violation would produce
  incorrect binary-search results because the pair entries are sorted by
  `(file_a, file_b)` with the assumption `file_a < file_b`.

- **`pairs_for_file` has O(file_a_start) linear scan for `file_b == id` matches**:
  files with IDs near the upper end of the range will scan more entries in the
  prefix. For very large indexes this could be slow — but the SQLite read path
  (`TemporalDb::cochanges_for_file`) is what CLI queries use, not this method.

- **CRC32 covers only the payload (after the header), not the header itself**:
  the checksum bytes in the header are excluded from their own checksum (avoids
  chicken-and-egg). The header's magic + version guard against wrong-format reads.

- **The `.skcc` file is NOT used at query time from the CLI**: `--blast-radius`
  reads from `temporal.db` (SQLite). The `.skcc` builder result is NOT called by
  the CLI temporal pipeline either (since `build_cochange_rows` was introduced);
  it is a standalone library primitive.

- **`CochangeMatrixReader::open` still runs a full-payload CRC32 on every call
  (#376 / #384)**: the lexical and AST readers had their per-open CRC32 moved off
  the hot path via the `crate::validity` marker mechanism in #376. The co-change
  reader was intentionally deferred to #384 (filed up-front per ADR-004). Until
  #384 lands, every `CochangeMatrixReader::open` hashes the entire payload.
  Do not apply the #376 validity-marker pattern to `.skcc` in a hotfix — file
  the work under #384 to keep the scope tracked.

## Key Files

- `crates/rskim-search/src/cochange/mod.rs` — module declaration, public re-exports
- `crates/rskim-search/src/cochange/format.rs` — pure binary codec; no I/O; all
  on-disk struct layouts, CRC32, encode/decode/lookup
- `crates/rskim-search/src/cochange/builder.rs` — `CochangeMatrixBuilder`;
  `COUPLING_MAX_FILES` (pub); `MAX_PAIRS` (pub(crate)); atomic write
- `crates/rskim-search/src/cochange/reader.rs` — `CochangeMatrixReader`; mmap;
  `pair_count`, `jaccard`, `pairs_for_file`, `file_commits`
- `crates/rskim-search/src/io_util.rs` — `atomic_write` shared helper
- `crates/rskim-search/src/temporal/storage_ops.rs` — `MIN_JACCARD_THRESHOLD`,
  `cochanges_for_file`, `store_cochanges`, `load_cochanges`
- `crates/rskim-search/src/types.rs` — `CochangeStats`, `HistoryResult`, `FileId`
- `crates/rskim/src/cmd/search/temporal_build.rs` — CLI-side builder that uses
  inline `build_cochange_rows` (not `CochangeMatrixBuilder`) to populate the
  SQLite `cochange` table

## Related

- Feature: `temporal-scoring` — sibling in `rskim-search`; shares `HistoryResult`
  and `FileId` types; temporal_build.rs orchestrates both.
- Feature: `cmd-search` — CLI orchestration layer; `temporal.rs` provides
  `resolve_blast_radius_paths` / `resolve_blast_radius_file_ids` for `--blast-radius`.
- `crates/rskim-search/src/io_util.rs` — `atomic_write` shared with `ast_index/store/builder.rs`.
- Issue #191: co-change validation benchmark that established `MIN_JACCARD_THRESHOLD = 0.10`.
