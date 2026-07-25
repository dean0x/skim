---
feature: cochange
name: Co-Change Matrix
description: "Use when implementing co-change coupling queries, modifying the .skcc binary format, adding new query methods to CochangeMatrixReader, debugging Jaccard similarity calculations, or working with the SQLite temporal persistence layer for co-change pairs. Keywords: cochange, co-change, coupling, jaccard, skcc, binary format, cochange.skcc, CochangeMatrixBuilder, CochangeMatrixReader, CochangeRow, TemporalDb, HistoryResult, COUPLING_MAX_FILES, builder_tests, format_tests, reader_tests, test_helpers, atomic_write, io_util, MIN_JACCARD_THRESHOLD, cochanges_for_file, UNION ALL."
category: domain-knowledge
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
created: 2026-05-24
updated: 2026-06-09
version: 2
---

# Co-Change Matrix

## Overview

The co-change matrix captures file coupling signals from git history: when two files change
together frequently, they have a high coupling score. The subsystem produces a single binary
file (`cochange.skcc`) from a `HistoryResult` and provides a memory-mapped reader for Jaccard
similarity queries and top-K partner retrieval.

The module is intentionally separate from the `LayerBuilder`/`SearchLayer` trait pair — it
consumes a pre-parsed `HistoryResult` rather than raw file content, because the signal comes
from commit graphs, not file bytes. This separation is explicit in the builder's doc comment
and is a deliberate design constraint to preserve.

## Test Module Organization

Tests are split into four files alongside their implementation files (not inline in `mod.rs`):

- `builder_tests.rs` — covers `CochangeMatrixBuilder`: constructor, accumulation, atomic write,
  duplicate-path deduplication, `MAX_PAIRS` safety cap
- `format_tests.rs` — covers `format.rs` codec: encode/decode round-trips, header validation,
  CRC32 integrity, edge cases with zero entries
- `reader_tests.rs` — covers `CochangeMatrixReader`: open/corrupt-file/size-mismatch detection,
  `pair_count`, `jaccard`, `pairs_for_file`, `file_commits`, Send+Sync assertion
- `test_helpers.rs` — shared `build_matrix(tmp, commits, paths)` helper consumed by both
  `builder_tests.rs` and `reader_tests.rs` to reduce setup boilerplate

`test_helpers` is `pub(super)` — accessible within the `cochange` module but not exported.

## Business Context

Co-change coupling is a static approximation of runtime coupling: files that have been modified
together in the past are likely to need to be modified together in the future. The Jaccard metric
normalises for file popularity, so a file that appears in every commit does not dominate the
results.

Two safety constants govern data quality:
- `COUPLING_MAX_FILES = 50` — commits touching more than 50 files are bulk refactors or merges.
  Including them would pollute coupling signal with unrelated co-changes. These commits are counted
  in `CochangeStats::commits_skipped_too_large`.
- `MAX_PAIRS = 2_000_000` — bounds memory growth during accumulation. Exceeding this returns
  `SearchError::CapacityExceeded`.

## Dual Persistence Model

Co-change data has two complementary persistence formats, each optimised for a different access
pattern:

| Format | Location | API | Access pattern |
|--------|----------|-----|----------------|
| `.skcc` binary | `{index_dir}/cochange.skcc` | `CochangeMatrixReader` | Point queries: `jaccard(a, b)`, `pairs_for_file(id)` |
| SQLite `cochange` table | `{cache_dir}/temporal.db` | `TemporalDb::load_cochanges` / `store_cochanges` | Bulk-load: all pairs with human-readable paths for ranking pipelines |

The `.skcc` format uses `FileId` (u32 integers) and is memory-mapped. The SQLite `cochange` table
stores the same pairs using repo-root-relative path strings (`file_a TEXT`, `file_b TEXT`), making
them accessible without a path-map lookup. Pre-computed Jaccard scores are stored in the SQLite row
alongside the raw co-change count.

Both formats are always written together during an index refresh. The `.skcc` file is the
authoritative coupling store; the SQLite table is a projection for ranking signal aggregation
alongside `hotspot` and `risk` data.

## Core Business Rules

### Canonical pair ordering

Every co-change pair is stored as `(min(a, b), max(a, b))`. This invariant is enforced during
accumulation: `ids` are sorted and deduplicated before pair generation, so `a = ids[i]` and
`b = ids[j]` with `i < j` guarantees `a < b` without calling `.min()` / `.max()`. The reader's
`pair_count` and `jaccard` methods call `canonicalize()` before any lookup, so callers can pass
IDs in either order without getting misses.

### Per-commit deduplication

Within a single commit, a file path can appear more than once (rename + modify). `accumulate_pairs`
sorts and deduplicates `ids` per commit before generating pairs. Without this, a commit with a
rename would produce self-pairs `(a, a)`, violating the `a < b` invariant and corrupting pair
counts.

### Jaccard formula

```
Jaccard(a, b) = count_ab / (count_a + count_b - count_ab)
```

`count_a` and `count_b` are per-file commit counts. The denominator is computed in `u64` to
prevent overflow when both files have high commit counts. Returns `0.0` for self-pairs, absent
pairs, and zero denominators.

### `pairs_for_file` is O(log n + k), not O(n)

The reader uses binary search to locate the start of the contiguous `file_a == id` block within
the sorted `PairEntry` array, then performs a short linear scan over only the prefix where
`file_b == id` might appear. The previously O(n) linear scan was replaced in PR #251.

## State Transitions

```
HistoryResult (from TemporalSource)
      |
      | CochangeMatrixBuilder::build()
      |   1. accumulate_pairs — HashMap<(u32,u32), u32> + HashMap<u32, u32>
      |   2. serialize — sorted byte arrays + CRC32 header
      |   3. atomic_write — NamedTempFile + sync_all + persist (rename)
      v
cochange.skcc (on disk)
      |
      | CochangeMatrixReader::open()
      |   1. mmap read-only
      |   2. decode_header — magic, version, size validation
      |   3. CRC32 verification
      v
CochangeMatrixReader (queryable)
      |
      +-- pair_count(a, b)    binary search over PairEntry array
      +-- jaccard(a, b)       pair_count + file_commits binary searches
      +-- pairs_for_file(id)  binary search to start block + prefix scan
      +-- file_commits(id)    binary search over FileCommitEntry array
```

## Technical Implementation Patterns

### Three-module separation

The `cochange` module splits responsibilities across three files that never import from each
other's private internals:

- `format.rs` — pure codec, operates only on `&[u8]` or owned byte arrays. Zero `std::fs` or
  `std::io::Write`. Every encode/decode function is independently testable with raw bytes.
- `builder.rs` — accumulation and I/O. Imports from `format.rs`, never from `reader.rs`.
- `reader.rs` — memory-mapped queries. Imports from `format.rs`, never from `builder.rs`.

`mod.rs` re-exports only `CochangeMatrixBuilder` and `CochangeMatrixReader` — all `format.rs`
types are `pub(crate)`.

### Binary format layout

The `.skcc` file is a flat concatenation of three sections:

```
[SkccHeader:        18 bytes  — magic(4) + version(2) + pair_count(4) + file_count(4) + checksum(4)]
[FileCommitEntry × file_count — 8 bytes each, sorted by file_id ascending]
[PairEntry       × pair_count — 12 bytes each, sorted by (file_a, file_b) ascending]
```

All integers are little-endian. The CRC32 checksum covers the `FileCommitEntry` array bytes
concatenated with the `PairEntry` array bytes. When a format-breaking change is needed, increment
`FORMAT_VERSION` in `format.rs` (currently `1`).

### Atomic write contract

`builder.rs` delegates atomic writes to `crate::io_util::atomic_write` (PR #272 extracted the
previously inline function into a shared helper). The helper uses `tempfile::NamedTempFile::new_in(dir)`,
writes all bytes, calls `sync_all()` to flush to storage (crash safety), sets `0o644` permissions
on Unix, and then calls `.persist(path)` (a rename) so readers never observe a partially written file.
The `ast_index` store builder uses the same `atomic_write` helper.

### `generate_pairs` capacity guard

`generate_pairs` uses the `Entry` API for a single hash probe in the common (under-capacity) case.
When the map is already full (`pair_counts.len() >= max_pairs`), the function allows incrementing
existing entries but returns `SearchError::CapacityExceeded` if a new distinct pair would be added.
This prevents memory growth from exceeding `max_pairs` while still accurately counting pairs already
accumulated.

The public `build` method calls `build_with_limit(history, path_map, MAX_PAIRS)`. The
`pub(crate)` `build_with_limit` accepts a custom limit so tests can trigger the cap cheaply.

### SQLite co-change table schema

```sql
CREATE TABLE cochange (
    file_a  TEXT NOT NULL,
    file_b  TEXT NOT NULL,
    count   INTEGER NOT NULL,
    jaccard REAL    NOT NULL,
    PRIMARY KEY (file_a, file_b)
);
```

Performance index (added in temporal schema **v2**):
```sql
CREATE INDEX IF NOT EXISTS idx_cochange_file_b ON cochange(file_b);
```

There is **no** `idx_cochange_file_b` on `file_a` — the composite PRIMARY KEY `(file_a, file_b)`
already serves `file_a = ?` prefix queries. The `idx_cochange_file_b` secondary index enables
efficient lookup of rows where `file_b` matches a given path.

`file_a` is always lexically less than or equal to `file_b`. Both `store_cochanges` and `sync` use
DELETE + batch INSERT in a single transaction. An `insert_cochanges_in_tx` helper enforces the
`file_a < file_b` invariant via `debug_assert!`.

### `cochanges_for_file` (SQLite query)

The `TemporalDb::cochanges_for_file` method uses `UNION ALL` of two indexed sub-queries rather
than `OR`:

```sql
SELECT file_a, file_b, count, jaccard FROM cochange
  WHERE file_a = ?1 AND jaccard >= ?2
UNION ALL
SELECT file_a, file_b, count, jaccard FROM cochange
  WHERE file_b = ?1 AND jaccard >= ?2
ORDER BY jaccard DESC LIMIT 10000
```

- `?1` = the query path; `?2` = `MIN_JACCARD_THRESHOLD = 0.10`
- `UNION ALL` (not `UNION`): the canonical ordering `file_a < file_b` means no row can satisfy
  both arms, so deduplication is unnecessary and `ALL` avoids the extra sort.
- Pairs with `jaccard < 0.10` are filtered out — they are noise with no predictive coupling signal
  (empirically validated in benchmark #191).

## Error Handling

| Error variant | Cause | Recovery |
|---|---|---|
| `SearchError::Io` | Directory does not exist or file cannot be opened | Caller ensures directory exists |
| `SearchError::IndexCorrupted(msg)` | Magic mismatch, version mismatch, size mismatch, checksum mismatch | Delete `cochange.skcc` and re-run the builder |
| `SearchError::CapacityExceeded(msg)` | More than 2M unique co-change pairs accumulated | Review `COUPLING_MAX_FILES` threshold |
| `SearchError::Database(msg)` | SQLite failure in `TemporalDb` | rusqlite errors are converted to strings at storage boundary |

## Anti-Patterns

- **Skipping the `NamedTempFile` + `sync_all` + persist pattern** exposes readers to partial
  writes on process interrupt. Always use `atomic_write`.
- **Using the raw `u32` file IDs directly** instead of `FileId` wrappers breaks type safety.
- **Treating `pairs_for_file` as an O(n) operation** — it is O(log n + k). Do not avoid it on
  the assumption it scans the full pair array.
- **Bypassing `CochangeMatrixBuilder` to write `.skcc` directly** requires manually maintaining
  CRC32, sort order, and format version.
- **Populating only the SQLite `cochange` table without writing `.skcc`** — point queries must go
  through `CochangeMatrixReader`.
- **Adding tests inline in the implementation files** — tests live in `*_tests.rs` companion files
  and `test_helpers.rs`. Follow the existing split.
- **Adding `idx_cochange_file_a` to the temporal schema** — the PRIMARY KEY `(file_a, file_b)`
  already covers file_a prefix queries; a secondary index is redundant and wastes space.

## Gotchas

- `pair_count` and `jaccard` return `0` / `0.0` (not an error) for pairs not present. Callers
  must treat `0` as "no coupling signal observed", not as "files are unrelated".
- `CochangeMatrixBuilder` does NOT implement `LayerBuilder`. It is a standalone builder.
- `unknown_paths_skipped` in `CochangeStats` counts individual file-path appearances across all
  commits, not distinct paths.
- Format version is checked on `open`, not lazily. Opening a stale `.skcc` file returns an error
  immediately directing the caller to rebuild.
- `SearchError::CapacityExceeded` (not `IndexCorrupted`) is returned when `MAX_PAIRS` is hit.
- `TemporalDb` is not `Sync`. Each thread must open its own connection.
- `CochangeRow::count` is **`u32`** in `storage_types.rs` — rusqlite maps the SQLite INTEGER
  column to `u32` via `row.get(2)?`. No `i64` cast is needed when reading from the database.
- `cochanges_for_file` via `TemporalDb` only returns pairs with Jaccard ≥ `MIN_JACCARD_THRESHOLD`
  (0.10). To see all pairs regardless of Jaccard, use `CochangeMatrixReader::pairs_for_file`
  (the binary `.skcc` reader has no Jaccard filter).
- `TemporalDb::cochanges_for_file` caps results at 10,000 rows. For files with very many co-change
  partners (e.g., a shared constants file), callers will see only the top 10,000 by Jaccard.

## Key Files

- `crates/rskim-search/src/cochange/format.rs` — pure binary codec; extend here when adding
  fields to the on-disk format; `FORMAT_VERSION = 1`
- `crates/rskim-search/src/cochange/builder.rs` — accumulation logic, `generate_pairs`, and
  atomic write; `COUPLING_MAX_FILES = 50` and `MAX_PAIRS = 2_000_000` constants live here
- `crates/rskim-search/src/cochange/reader.rs` — memory-mapped query API; `pairs_for_file` uses
  binary search; offsets cached at `open()` time
- `crates/rskim-search/src/cochange/mod.rs` — public re-exports; usage doc example
- `crates/rskim-search/src/cochange/test_helpers.rs` — `build_matrix()` helper for tests
- `crates/rskim-search/src/temporal/storage_types.rs` — `CochangeRow` (count: u32), `HotspotRow`, `RiskRow`
- `crates/rskim-search/src/temporal/storage.rs` — `TemporalDb` struct, schema migrations, WAL
- `crates/rskim-search/src/temporal/storage_ops.rs` — `store_cochanges`, `load_cochanges`,
  `cochanges_for_file` with `MIN_JACCARD_THRESHOLD = 0.10` and `LIMIT 10000`
- `crates/rskim-search/src/types.rs` — `CochangeStats`, `HistoryResult`, `FileId`, `SearchError`
- `crates/rskim-search/src/lib.rs` — re-exports public crate API
- `crates/rskim-search/src/io_util.rs` — `atomic_write` shared helper (NamedTempFile + sync_all + persist)

## Related

- `crates/rskim-search/src/temporal/` — provides `GixSource` and `HistoryResult`, the upstream
  input to `CochangeMatrixBuilder::build`; also owns `TemporalDb` and the SQLite persistence layer
- `crates/rskim-search/src/types.rs` — `FileId`, `CochangeStats`, `HistoryResult`, `SearchError`
- `crates/rskim-search/src/index/` — sibling persistence layer using the same atomic-write and
  mmap-read patterns; useful cross-reference for format evolution precedent
- `crates/rskim-search/src/ast_index/store/` (Wave 3d–3f) — the closest format sibling: a
  two-file mmap'd on-disk index (magic `b"SKAX"`, v2) for AST structural n-grams, built with the
  identical `NamedTempFile` + `sync_all` + `persist` atomic-write contract and CRC32-validated
  binary-search reader. Mirror its `format.rs`/`builder.rs`/`reader.rs` split when evolving `.skcc`.
