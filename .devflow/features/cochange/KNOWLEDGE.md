---
feature: cochange
name: Co-Change Matrix
description: "Use when implementing co-change coupling queries, modifying the .skcc binary format, adding new query methods to CochangeMatrixReader, or debugging Jaccard similarity calculations. Keywords: cochange, co-change, coupling, jaccard, skcc, binary format, cochange.skcc, CochangeMatrixBuilder, CochangeMatrixReader, HistoryResult, COUPLING_MAX_FILES."
category: domain-knowledge
directories: [crates/rskim-search/src/cochange/]
referencedFiles:
  - crates/rskim-search/src/cochange/mod.rs
  - crates/rskim-search/src/cochange/builder.rs
  - crates/rskim-search/src/cochange/format.rs
  - crates/rskim-search/src/cochange/reader.rs
  - crates/rskim-search/src/types.rs
  - crates/rskim-search/src/lib.rs
created: 2026-05-24
updated: 2026-05-24
---

# Co-Change Matrix

## Overview

The co-change matrix captures file coupling signals from git history: when two files change together frequently, they have a high coupling score. The subsystem produces a single binary file (`cochange.skcc`) from a `HistoryResult` and provides a memory-mapped reader for Jaccard similarity queries and top-K partner retrieval.

The module is intentionally separate from the `LayerBuilder`/`SearchLayer` trait pair — it consumes a pre-parsed `HistoryResult` rather than raw file content, because the signal comes from commit graphs, not file bytes. This separation is explicit in the builder's doc comment and is a deliberate design constraint to preserve.

## Business Context

Co-change coupling is a static approximation of runtime coupling: files that have been modified together in the past are likely to need to be modified together in the future. The Jaccard metric normalises for file popularity, so a file that appears in every commit does not dominate the results.

Two safety constants govern data quality:
- `COUPLING_MAX_FILES = 50` — commits touching more than 50 files are bulk refactors or merges. Including them would pollute coupling signal with unrelated co-changes. These commits are counted in `CochangeStats::commits_skipped_too_large`.
- `MAX_PAIRS = 2_000_000` — bounds memory growth during accumulation. Exceeding this returns `SearchError::IndexCorrupted`.

## Core Business Rules

### Canonical pair ordering

Every co-change pair is stored as `(min(a, b), max(a, b))`. This invariant is enforced during accumulation by `accumulate_pairs` and verified with `debug_assert!` in the hot path. The reader's `pair_count` and `jaccard` methods call `canonicalize()` before any lookup, so callers can pass IDs in either order without getting misses.

### Per-commit deduplication

Within a single commit, a file path can appear more than once (rename + modify). `accumulate_pairs` sorts and deduplicates `ids` per commit before generating pairs. Without this, a commit with a rename would produce self-pairs `(a, a)`, violating the `a < b` invariant and corrupting pair counts.

### Jaccard formula

```
Jaccard(a, b) = count_ab / (count_a + count_b - count_ab)
```

`count_a` and `count_b` are per-file commit counts (how many commits touched each file individually). The denominator is computed in `u64` to prevent overflow when both files have high commit counts. Returns `0.0` for self-pairs, absent pairs, and zero denominators — the caller always gets a valid `f64`.

### `pairs_for_file` is O(pair_count)

The reader performs a linear scan over all `PairEntry` records to collect partners for a given file. This is intentional — there is no secondary per-file index in the format. For large repositories (millions of pairs) this may become a bottleneck; binary search over sorted pairs is only used for the point-lookup case (`pair_count`, `jaccard`).

## State Transitions

```
HistoryResult (from TemporalSource)
      |
      | CochangeMatrixBuilder::build()
      |   1. accumulate_pairs — HashMap<(u32,u32), u32> + HashMap<u32, u32>
      |   2. serialize — sorted byte arrays + CRC32 header
      |   3. atomic_write — NamedTempFile + persist (rename)
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
      +-- pairs_for_file(id)  linear scan, sorted by count desc
      +-- file_commits(id)    binary search over FileCommitEntry array
```

## Technical Implementation Patterns

### Three-module separation

The `cochange` module splits responsibilities across three files that never import from each other's private internals:

- `format.rs` — pure codec, operates only on `&[u8]` or owned byte arrays. Zero `std::fs` or `std::io::Write`. Every encode/decode function is independently testable with raw bytes.
- `builder.rs` — accumulation and I/O. Imports from `format.rs`, never from `reader.rs`.
- `reader.rs` — memory-mapped queries. Imports from `format.rs`, never from `builder.rs`.

`mod.rs` re-exports only `CochangeMatrixBuilder` and `CochangeMatrixReader` — all `format.rs` types are `pub(crate)`.

### Binary format layout

The `.skcc` file is a flat concatenation of three sections:

```
[SkccHeader:        18 bytes  — magic(4) + version(2) + pair_count(4) + file_count(4) + checksum(4)]
[FileCommitEntry × file_count — 8 bytes each, sorted by file_id ascending]
[PairEntry       × pair_count — 12 bytes each, sorted by (file_a, file_b) ascending]
```

All integers are little-endian. The CRC32 checksum covers the `FileCommitEntry` array bytes concatenated with the `PairEntry` array bytes — the header itself is not checksummed.

When a format-breaking change is needed, increment `FORMAT_VERSION` in `format.rs`. The reader will return `SearchError::IndexCorrupted` with a human-readable message for any file with a non-matching version.

### Atomic write contract

`builder.rs` uses `tempfile::NamedTempFile::new_in(dir)` and `.persist(path)` (a rename) so readers never observe a partially written file. The temp file is created in the same directory as the target, ensuring the rename is always on the same filesystem.

### Memory mapping and Send + Sync

`CochangeMatrixReader` memory-maps the file read-only. The `Mmap` type from `memmap2` is `Send + Sync`, and the header is copied out into a `SkccHeader` struct (a `Copy` type) at open time for cheap repeated access. The reader inherits `Send + Sync` automatically with no unsafe impl needed.

The one safety caveat documented in the source: if another process truncates or overwrites `cochange.skcc` after mapping, behaviour is undefined. This is an inherent mmap constraint, not a bug.

## Error Handling and Recovery

| Error variant | Cause | Recovery |
|---|---|---|
| `SearchError::Io` | Directory does not exist (`builder::new`), or file cannot be opened (`reader::open`) | Caller ensures directory exists before constructing builder |
| `SearchError::IndexCorrupted(msg)` | Magic mismatch, version mismatch, size mismatch, checksum mismatch, malformed entry slice, overflow in checked arithmetic | Delete `cochange.skcc` and re-run the builder |
| `SearchError::IndexCorrupted` (MAX_PAIRS) | More than 2M unique co-change pairs accumulated | Review `COUPLING_MAX_FILES` threshold or examine for degenerate history |

The `decode_header`, `decode_file_commit`, and `decode_pair` functions never panic — all slice accesses go through `read_array` which returns `SearchError::IndexCorrupted` on truncation or overflow.

## Anti-Patterns

- **Skipping the `NamedTempFile` + persist pattern** when writing the `.skcc` file will expose readers to partial writes if the process is interrupted. Always use atomic write.
- **Using the raw `u32` file IDs directly** instead of `FileId` wrappers breaks type safety and makes it easy to accidentally mix pair IDs with file-commit-count IDs. Always accept and return `FileId`.
- **Calling `pairs_for_file` in a hot loop** over all files will scan the entire pair array for each file. Batch queries by reading all pairs once if all-pairs traversal is needed.
- **Bypassing `CochangeMatrixBuilder` to write `.skcc` directly** requires manually maintaining CRC32, sort order, and format version — all invariants that the builder enforces. Don't do it.

## Gotchas

- `pair_count` and `jaccard` return `0` / `0.0` (not an error) for pairs not present in the matrix. Callers must treat `0` as "no coupling signal observed", not as "files are unrelated" — absence from the matrix may mean the paths were not in the `path_map` at build time.
- `CochangeMatrixBuilder` does NOT implement `LayerBuilder`. Do not attempt to register it via the `LayerBuilder` trait. It is a standalone builder that takes `&HistoryResult` and `&HashMap<PathBuf, FileId>`.
- `unknown_paths_skipped` in `CochangeStats` counts individual file-path appearances across all commits, not distinct paths. A single unrecognised path in 100 commits increments this counter 100 times.
- The builder's `path_map` key type is `PathBuf` with repo-root-relative paths. If the caller normalises paths differently (e.g., with a leading `./`), lookups will silently miss and inflate `unknown_paths_skipped`.
- Format version is checked on `open`, not lazily. Opening a stale `.skcc` file from a previous format version returns an error immediately with a message directing the caller to rebuild.

## Key Files

- `crates/rskim-search/src/cochange/format.rs` — the pure binary codec; extend here when adding fields to the on-disk format
- `crates/rskim-search/src/cochange/builder.rs` — accumulation logic and atomic write; `COUPLING_MAX_FILES` and `MAX_PAIRS` constants live here
- `crates/rskim-search/src/cochange/reader.rs` — memory-mapped query API; add new query methods here
- `crates/rskim-search/src/cochange/mod.rs` — public re-exports; the only public surface is `CochangeMatrixBuilder` and `CochangeMatrixReader`
- `crates/rskim-search/src/types.rs` — `CochangeStats`, `HistoryResult`, `CommitInfo`, `FileId` — all shared types the cochange module depends on
- `crates/rskim-search/src/lib.rs` — confirms both types are part of the public `rskim-search` crate API

## Related

- `crates/rskim-search/src/temporal/` — provides `GixSource` and `HistoryResult`, the upstream input to `CochangeMatrixBuilder::build`
- `crates/rskim-search/src/types.rs` — `FileId`, `CochangeStats`, `HistoryResult`, `SearchError`
- `crates/rskim-search/src/index/` — sibling persistence layer using the same atomic-write and mmap-read patterns; useful cross-reference for format evolution precedent
