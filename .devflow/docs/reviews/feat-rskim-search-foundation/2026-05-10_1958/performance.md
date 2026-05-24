# Performance Review Report

**Branch**: feat/rskim-search-foundation -> main
**Date**: 2026-05-10

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

- **SearchResult heap allocations per result** - `crates/rskim-search/src/types.rs:145-158` (Confidence: 65%) -- Each `SearchResult` contains `Vec<Range<usize>>` for `match_positions` and `Option<String>` for `snippet`. When returning thousands of search results, this creates one heap allocation per result per field. Consider a `SmallVec<[Range<usize>; 4]>` for `match_positions` and/or `Cow<'a, str>` for `snippet` to reduce allocator pressure in hot search paths. However, since this is Wave 0 (types-only, no implementation yet), the actual access patterns are unknown; premature optimization risk is real.

- **SearchQuery owns String where &str may suffice** - `crates/rskim-search/src/types.rs:106-119` (Confidence: 60%) -- `SearchQuery.text` is `String`, which forces a heap allocation on every query construction. A `Cow<'a, str>` or lifetime-parameterized `&'a str` would allow zero-copy queries when the caller already owns the string. Again, this is a foundational type and the actual usage patterns will determine whether this matters.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Performance Score**: 9/10
**Recommendation**: APPROVED

### Rationale

This PR introduces Wave 0 of the `rskim-search` crate: pure types, traits, and error handling with no runtime I/O or algorithmic logic. The performance review surface is minimal because:

1. **No hot paths exist yet.** All code is type definitions, trait declarations, error enums, and a CLI stub that prints help text. There are no loops, no I/O, no queries, no indexing -- nothing that executes in a performance-sensitive context.

2. **Type design is sound for performance.** Key decisions are well-aligned with the project's performance targets:
   - `FileId(pub u32)` is a Copy-able newtype -- zero overhead vs raw u32, perfect for posting lists and map keys.
   - `SearchField` is a Copy enum -- no heap allocation, trivial to pass by value.
   - `TemporalFlags` uses `Option<u32>` -- compact, no indirection.
   - `IndexStats` is all primitive/Copy fields -- cache-friendly layout.
   - Traits use `&SearchQuery` (borrowed) and return `Vec<SearchResult>` -- standard Rust patterns that avoid unnecessary cloning.

3. **The `SearchLayer` trait is `Send + Sync`** -- this correctly enables future parallel search pipelines without requiring `Arc` wrappers at the trait level.

4. **The `LayerBuilder` pattern** separates the mutable build phase from the immutable query phase. This is the correct architecture for index construction: build once, query many times without synchronization overhead.

5. **No performance regressions.** The remaining changes in this PR are exclusively `cargo fmt` (edition 2024 import reordering) and whitespace reformatting across ~100 files. These are zero-runtime-impact changes.

6. **Workspace upgrade to thiserror 2.0** has no performance implications -- thiserror is a proc-macro that generates code at compile time.

7. **Removal of `rskim-search` from rskim's Cargo.toml** (the binary crate no longer depends on the library) means no additional code is compiled into the production binary, keeping binary size unchanged.

The two suggestions in the lower-confidence section note potential future optimizations for when search is actually implemented. They are not actionable now -- the correct time to optimize allocations is after profiling real workloads, consistent with the Iron Law: measure before optimizing.
