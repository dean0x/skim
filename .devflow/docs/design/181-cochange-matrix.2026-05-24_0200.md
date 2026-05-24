---
type: design-artifact
version: 1
status: APPROVED
issue: 181
title: "Co-change matrix builder"
slug: cochange-matrix
created: 2026-05-24T02:00:00Z
execution-strategy: SINGLE_CODER
context-risk: LOW
---

## Problem Statement

The rskim-search temporal layer parses git history via `GixSource` but cannot persist or query file coupling data. The existing `compute_coupling` in the heatmap CLI recomputes co-change pairs on every invocation with asymmetric weighted confidence. This blocks coupling-aware search ranking and forces unnecessary re-computation.

Target users: the search engine's temporal scoring layer (internal API), with future consumers in IDE plugins and refactoring tools.

## Acceptance Criteria

1. auth.rs + auth_test.rs always co-changing in test history → Jaccard score > 0.8
2. Matrix is symmetric: `jaccard(a, b) == jaccard(b, a)` for all file pairs
3. Performance: < 5s for 10k-commit, 1k-file repo (release mode)
4. Top-K co-change partners retrievable per file, sorted by count descending

## Scope

### v1 Included
- `CochangeMatrixBuilder` consuming `HistoryResult` + `&HashMap<PathBuf, FileId>` path mapping
- `CochangeMatrixReader` with mmap-based Jaccard queries and top-K retrieval
- Binary format `.skcc` with magic bytes, versioning, CRC32 integrity
- Safety caps: COUPLING_MAX_FILES=50 per commit, MAX_PAIRS=2,000,000 total
- Per-file commit count tracking for accurate Jaccard denominators

### Deferred
- Heatmap CLI integration (consuming library-level matrix)
- Incremental updates (append new commits to existing matrix)
- Rayon parallelism for pair accumulation
- Rename following (tracking file identity across renames)
- Consolidation with heatmap's `compute_coupling`

### Excluded
- CLI subcommand for co-change queries
- Search query integration via `TemporalFlags`

## Gap Analysis Results

### Blocking (resolved)
1. **Path-to-FileId mapping** — Builder accepts `&HashMap<PathBuf, FileId>` from caller; unknown paths silently skipped with count returned in `CochangeStats`
2. **Canonical unordered pairs** — `(min(a,b), max(a,b))` at insertion time, halving memory
3. **File rename handling** — Distinct files for v1; documented limitation
4. **Binary format versioning** — Magic `SKCC`, version u16, CRC32 checksum over payload
5. **COUPLING_MAX_FILES/Jaccard interaction** — `paired_commits[file]` tracked separately; Jaccard uses paired count
6. **compute_coupling relationship** — Both coexist; library uses Jaccard, CLI uses weighted confidence

### Should-address (integrated)
- Self-pair exclusion (canonical form prevents)
- Top-K API: `pairs_for_file(FileId) → Vec<(FileId, u32)>` sorted by count desc
- HashMap pre-allocation with `with_capacity`
- Thread safety: reader is Send+Sync via read-only Mmap
- MAX_PAIRS breach returns `SearchError::IndexCorrupted`
- Empty history writes valid empty matrix
- u32 `saturating_add` for counters

## Execution Strategy

**SINGLE_CODER** — Binary format is the critical contract both builder and reader depend on. Sequential implementation ensures format invariant correctness. 600-800 lines across 7 new files + 2 modifications.

## Implementation Plan

### Step 1: types.rs — Add CochangeStats

**File:** `crates/rskim-search/src/types.rs`

Add `CochangeStats` struct:
- `pair_count: u32` — unique file pairs stored
- `file_count: u32` — distinct files with at least one pair
- `commits_processed: u32` — total commits iterated
- `commits_skipped_too_large: u32` — commits exceeding COUPLING_MAX_FILES
- `unknown_paths_skipped: u32` — paths not found in path_map

### Step 2: cochange/format.rs — Binary codec

**File:** `crates/rskim-search/src/cochange/format.rs` (~150 lines)

Header (18 bytes): `[magic 4][version u16][pair_count u32][file_count u32][checksum u32]`
FileCommitEntry (8 bytes): `[file_id u32][commit_count u32]` — sorted by file_id
PairEntry (12 bytes): `[file_a u32][file_b u32][count u32]` — sorted by (file_a, file_b)

Layout: `[Header][FileCommitEntries][PairEntries]`

All integers little-endian. CRC32 covers file_commit + pair bytes.

Functions: `read_array<N>()`, encode/decode for each struct, `compute_checksum`, `lookup_pair` (binary search).

**Tests:** 13 cases — roundtrips, rejection of bad magic/version/truncated data, binary search, checksum.

### Step 3: cochange/builder.rs — Accumulate + serialize

**File:** `crates/rskim-search/src/cochange/builder.rs` (~250 lines)

```
CochangeMatrixBuilder::new(output_dir) → build(&self, history, path_map) → Result<CochangeStats>
```

Internal split: `accumulate_pairs()` + `serialize_and_write()`

Accumulation: iterate commits, resolve paths via path_map, canonical `(min,max)` pairs, self-pairs excluded, COUPLING_MAX_FILES=50 cap, MAX_PAIRS=2M cap. HashMap with_capacity pre-allocated. saturating_add for counters. Unknown paths counted.

Serialization: sort by key, encode via format functions, CRC32, atomic_write via NamedTempFile.

Empty history: write valid empty matrix (header with zeros).

**Tests:** 14 cases — constructor, empty history, pair counting, self-pairs, canonical ordering, COUPLING_MAX_FILES, unknown paths, MAX_PAIRS, atomic write, build-then-read roundtrip.

### Step 4: cochange/reader.rs — mmap reader

**File:** `crates/rskim-search/src/cochange/reader.rs` (~150 lines)

```
CochangeMatrixReader::open(dir) → Result<Self>
  .pair_count(a, b) → Result<u32>       // binary search, O(log n)
  .jaccard(a, b) → Result<f64>          // count_ab / (count_a + count_b - count_ab)
  .pairs_for_file(id) → Result<Vec<(FileId, u32)>>  // linear scan, O(pair_count)
  .file_commits(id) → Result<u32>       // binary search, O(log n)
```

Validation: magic, version, size consistency, CRC32. On failure: `SearchError::IndexCorrupted` — caller should rebuild.

Send + Sync via read-only Mmap. SAFETY comment documenting concurrent-write UB.

**Tests:** 16 cases — open errors, pair_count, Jaccard (known values, self-pair, absent, zero-denom), file_commits, pairs_for_file sorted desc, Send+Sync, CRC32 mismatch.

### Step 5: cochange/mod.rs — Module wiring

Re-export `CochangeMatrixBuilder` and `CochangeMatrixReader`.

### Step 6: lib.rs — Export

Add `pub mod cochange;` and re-exports.

## Patterns to Follow

| Pattern | Source | Reference |
|---------|--------|-----------|
| Builder accumulate → serialize → atomic_write | `index/builder.rs` | :254-291, :297-377 |
| Pure codec encode/decode | `index/format.rs` | :182-369 |
| read_array<N>() bounds-safe extraction | `index/format.rs` | :163-179 |
| mmap open → validate → query | `index/reader.rs` | :75-142 |
| CRC32 checksum validation | `index/reader.rs` | :125-134 |
| atomic_write via NamedTempFile | `index/builder.rs` | :87-93 |
| Test file sibling pattern | `temporal/git_parser.rs` | :327 |
| COUPLING_MAX_FILES cap | `heatmap/metrics.rs` | :57 |

## Integration Points

| Entry Point | File | Action |
|-------------|------|--------|
| Module registration | `lib.rs` | Add `pub mod cochange;` |
| Type exports | `lib.rs` | Add `CochangeMatrixBuilder`, `CochangeMatrixReader`, `CochangeStats` |
| CochangeStats | `types.rs` | New struct in shared types |
| Error handling | `types.rs` | Reuse existing `SearchError::IndexCorrupted`, `Io`, `InvalidQuery` |
| Dependencies | `Cargo.toml` | No changes needed (memmap2, tempfile, crc32fast all present) |

## Design Review Results

| Finding | Severity | Status |
|---------|----------|--------|
| Code duplication (read_array, atomic_write) | HIGH | Accepted for v1 — shared util is future cleanup |
| MAX_PAIRS breach behavior | HIGH | Mitigated — returns SearchError::IndexCorrupted |
| CRC recovery path | HIGH | Mitigated — documented as cache-miss → rebuild |
| build() god function risk | MEDIUM | Mitigated — split into accumulate + serialize |
| pairs_for_file() O(n) scan | MEDIUM | Accepted — documented, profile before optimizing |

## Risk Assessment

- **Context risk:** LOW
- **Unresolved risks:**
  - `pairs_for_file()` is O(pair_count) linear scan — acceptable for v1, profile before adding secondary index
  - `read_array<N>()` and `atomic_write()` duplicated from index/ — future extraction to shared util

## PR Description Guidance

### Problem Being Solved
The rskim-search temporal layer can parse git history but has no way to persist or query which files change together, forcing re-computation on every invocation and blocking coupling-aware search ranking.

### Key Changes to Highlight
- New `cochange/` module with builder, format codec, and mmap reader
- Jaccard similarity normalization (symmetric, unlike heatmap's asymmetric confidence)
- Binary format `.skcc` with CRC32 integrity checking
- Safety caps: COUPLING_MAX_FILES=50 per commit, MAX_PAIRS=2M total

### Breaking Changes
None expected

### Reviewer Focus Areas
- Binary format layout correctness (format.rs encode/decode symmetry)
- Jaccard denominator handling when COUPLING_MAX_FILES skips commits
- HashMap pre-allocation sizing and MAX_PAIRS breach behavior
