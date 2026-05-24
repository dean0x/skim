---
title: "Wave 1d: Index Builder — File Walker, Incremental Updates"
issue: 182
depends_on: [178]
status: planned
date: 2026-05-17
---

# Wave 1d: Index Builder

## Goal

Build an indexing pipeline that walks a codebase, classifies source files using tree-sitter into 7 BM25F fields, extracts bigram n-grams, and writes a two-file mmap'd index (.skidx + .skpost). Support incremental updates by caching field maps and detecting changes via mtime+SHA256.

## Architecture

```
CLI (crates/rskim)                    Library (crates/rskim-search)
┌─────────────────────────┐           ┌──────────────────────────┐
│ cmd/search/             │           │ Existing APIs:           │
│   walker.rs ─── walk ───┤           │   NgramIndexBuilder      │
│   manifest.rs ── cache  │           │   classify_source()      │
│   pipeline.rs ──────────┼──────────►│   Language::from_path()  │
│                         │  feeds    │   extract_ngrams()       │
└─────────────────────────┘           └──────────────────────────┘
```

## Blocking Decisions

| Decision | Resolution | Rationale |
|----------|-----------|-----------|
| FileId→path mapping | `.skfiles` JSONL sidecar alongside index | Avoids FORMAT_VERSION bump; human-readable for debugging |
| Incremental strategy | Rebuild-from-manifest: full rebuild but skip `classify_source()` for unchanged files | Respects consuming `build(self)` API; tree-sitter parse is ~80% of cost |
| Pipeline ownership | CLI crate (`crates/rskim/src/cmd/search/`) | Library stays pure; `ignore`/`rayon` already deps of CLI |
| FileId stability | Fresh sequential IDs per build; manifest provides path mapping | Avoids complex ID reuse/compaction logic |

## Incremental Update Flow

```
Walk filesystem → compare against previous .skfiles manifest
  ├── Unchanged (mtime+size match) → reuse cached field_map, skip tree-sitter
  ├── Modified (mtime/size differ) → SHA256 check → re-classify if hash differs
  └── New files → full classify_source()

All files: read content → feed to NgramIndexBuilder (builder needs &str for bigrams)
Savings: skip classify_source() for unchanged files (~80% of per-file cost)
```

## File Changes

### New Files (7)

| File | Purpose | ~Lines |
|------|---------|--------|
| `crates/rskim/src/cmd/search/mod.rs` | Module entry, re-exports | 30 |
| `crates/rskim/src/cmd/search/pipeline.rs` | Orchestrator: `build_index()` decomposed into 5 helpers | 300 |
| `crates/rskim/src/cmd/search/pipeline_tests.rs` | Integration tests (full build, incremental, corruption) | 300 |
| `crates/rskim/src/cmd/search/manifest.rs` | `.skfiles` sidecar read/write | 200 |
| `crates/rskim/src/cmd/search/manifest_tests.rs` | Manifest roundtrip, corruption, large manifest tests | 250 |
| `crates/rskim/src/cmd/search/walker.rs` | File walking + skip logic (>5MB, non-UTF8, minified) | 200 |
| `crates/rskim/src/cmd/search/walker_tests.rs` | Walk/skip condition tests with temp directories | 250 |

### Modified Files (1)

| File | Change |
|------|--------|
| `crates/rskim/Cargo.toml` | Add `sha2`, `tempfile` to `[dependencies]` |

## Key Types

```rust
// manifest.rs
struct FileManifestEntry {
    rel_path: PathBuf,
    mtime_secs: u64,
    sha256: String,
    size: u64,
    lang: String,
    field_map: Vec<(usize, usize, u8)>,  // (start, end, field_discriminant)
}

struct FileManifest {
    version: u32,
    root: PathBuf,
    files: Vec<FileManifestEntry>,
}

// walker.rs
struct FileMeta { rel_path, abs_path, mtime_secs, size, lang }
struct FileCandidate { meta: FileMeta, content: String }
enum SkipReason { TooLarge, NonUtf8, Minified, UnsupportedLanguage, ReadError }
struct WalkResult { candidates: Vec<FileCandidate>, skipped: Vec<SkipReason> }

// pipeline.rs
struct IndexConfig { root, cache_dir, max_file_size, respect_gitignore }
struct IndexResult {
    layer: Box<dyn SearchLayer>,
    file_count: u32,
    skipped: Vec<SkipReason>,
    errors: Vec<(PathBuf, String)>,
    was_incremental: bool,
    duration: Duration,
}
```

## Pipeline Decomposition

`build_index()` orchestrates 5 focused helpers:
1. `resolve_or_create_cache_dir(root)` → `PathBuf`
2. `detect_changes(walk_result, previous_manifest)` → `(changed, unchanged)`
3. `classify_parallel(changed_files)` → `Vec<ClassifiedFile>` (rayon)
4. `build_sequential(classified + unchanged, cache_dir)` → `Box<dyn SearchLayer>`
5. `write_manifest_atomic(cache_dir, entries)` → `()`

## Cache Layout

```
~/.cache/skim/search/{sha256(canonical_root)[..16]}/
  index.skidx
  index.skpost
  index.skfiles   (JSONL manifest)
```

## Skip Conditions

| Condition | Threshold | Applies to |
|-----------|-----------|------------|
| File too large | >5MB | All files |
| Non-UTF8 | read_to_string fails | All files |
| Minified | Avg line >500 bytes in first 8KB | Tree-sitter languages only |
| Unsupported language | Language::from_path returns None | All files |

## Implementation Order (TDD)

1. Manifest — test roundtrip/corruption → implement
2. Walker — test skip conditions → implement
3. Pipeline — test full build → implement → test incremental → implement
4. CLI wiring — search.rs → search/mod.rs
5. Performance validation — run against real repo

## Design Review Notes

- God function: `build_index()` decomposed into 5 helpers
- Manifest corruption: version check + JSON parse error → log warning + full rebuild
- FileMeta/FileCandidate split: lightweight metadata for diff, full content only for changed files
- Memory: all file contents held during parallel phase — acceptable for <100K files

## Acceptance Criteria

- [ ] Index 1000 files in < 5 seconds
- [ ] Incremental re-index (10 changed files) in < 500ms
- [ ] Respects .gitignore, skips binaries
- [ ] Index size < 10% of total indexed source bytes
- [ ] All 17 languages handled (14 tree-sitter + 3 serde as full-text)
- [ ] No file exceeds 400 lines
- [ ] All fallible operations return Result
- [ ] Module has tests: happy path, edge case, error path

## PR Description Guidance

**Problem:** Search index requires a pipeline to walk codebases, classify files, and build the n-gram index with incremental update support.

**Key Changes:** New `cmd/search/` module with pipeline, manifest, and walker. JSONL sidecar for FileId→path mapping and cached field maps. Two-phase processing: parallel tree-sitter classification, sequential builder accumulation.

**Breaking Changes:** None.

**Reviewer Focus:** Incremental diff logic in pipeline.rs, atomic write ordering (.skpost → .skidx → .skfiles), skip condition thresholds, fail-soft error handling.
