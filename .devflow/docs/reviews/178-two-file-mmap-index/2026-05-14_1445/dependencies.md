# Dependencies Review Report

**Branch**: feat/178-two-file-mmap-index -> main
**Date**: 2026-05-14

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**tempfile is a regular dependency but could be dev-dependency if builder is restructured** - `crates/rskim-search/Cargo.toml:23`
**Confidence**: 65% -- moved to Suggestions (see below)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **tempfile as regular dependency adds transitive weight** - `crates/rskim-search/Cargo.toml:23` (Confidence: 65%) -- tempfile (with transitive deps: fastrand, getrandom, once_cell, rustix, windows-sys) is listed under `[dependencies]` rather than `[dev-dependencies]`. It is legitimately used in production code for atomic writes in `builder.rs:82-88`. This is architecturally sound (atomic writes prevent corrupt index reads on crash), so this is not a blocking issue. However, if the builder were ever split into a separate feature gate, tempfile could be feature-gated too. Low priority.

- **Workspace crc32fast version floor (1.4) could be tightened to 1.5** - `Cargo.toml:55` (Confidence: 62%) -- The workspace declares `crc32fast = "1.4"` but Cargo.lock resolves to `1.5.0`. The `Hasher::new()` and `hash()` APIs used in this PR exist since 1.0, so the floor is safe. However, `1.5` specifically added SIMD acceleration improvements. Since the crate is `publish = false` and the lockfile pins the actual version, this is cosmetic. The existing `^1.4` range is consistent with the workspace pattern of specifying minimum-compatible rather than latest.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Dependencies Score**: 9/10
**Recommendation**: APPROVED

## Analysis Details

### New Direct Dependencies (3)

| Dependency | Version (workspace) | Resolved | Transitive Deps | New to Lockfile | Purpose |
|------------|-------------------|----------|-----------------|-----------------|---------|
| `memmap2` | `0.9` | `0.9.10` | libc (existing) | Yes | Memory-mapped file I/O for index reader |
| `crc32fast` | `1.4` | `1.5.0` | cfg-if (existing) | Yes | CRC32 checksum for index integrity |
| `tempfile` | `3.0` | `3.23.0` | fastrand, getrandom, once_cell, rustix (all existing) | No (already in lockfile) | Atomic file writes in builder |

### Dependency Health Assessment

**memmap2 v0.9.10**
- Actively maintained by the Rust community (RazrFalcon)
- Standard choice for memory-mapped I/O in Rust (successor to memmap)
- Minimal dependency footprint (only libc)
- License: MIT/Apache-2.0 (compatible with project MIT license)
- Used by major projects (ripgrep, tantivy, polars)
- Appropriate for the stated use case (mmap'd index reading)

**crc32fast v1.5.0**
- Maintained by the Rust community
- SIMD-accelerated CRC32 computation
- Minimal dependency footprint (only cfg-if)
- License: MIT/Apache-2.0 (compatible)
- Used by widely-adopted crates (flate2, zip, etc.)
- Appropriate for index integrity checking

**tempfile v3.23.0**
- Actively maintained (Stebalien)
- Standard Rust crate for secure temporary files
- License: MIT/Apache-2.0 (compatible)
- Already in workspace lockfile (used by rskim crate as dev-dependency)
- Used in production code for atomic writes (builder.rs)

### Positive Observations

1. **Minimal new dependencies**: Only 2 truly new packages added to lockfile (crc32fast, memmap2). tempfile was already present.
2. **Zero new transitive dependencies**: All transitive deps (cfg-if, libc, fastrand, getrandom, once_cell, rustix) were already in the lockfile.
3. **Workspace-managed versions**: All three dependencies use `{ workspace = true }` in the crate Cargo.toml, following the established workspace pattern.
4. **Well-chosen crates**: memmap2 and crc32fast are the de-facto standard Rust crates for their respective purposes. No lighter alternatives exist that would be appropriate.
5. **Appropriate version ranges**: Using caret ranges (e.g., `"0.9"`, `"1.4"`, `"3.0"`) is consistent with the rest of the workspace and appropriate for a `publish = false` crate with a committed lockfile.
6. **License compatibility**: All new dependencies are MIT/Apache-2.0 dual-licensed, compatible with the project's MIT license.
7. **Lockfile committed**: Cargo.lock changes are included in the diff, ensuring reproducible builds.
8. **Each dependency is actually used**: grep confirms memmap2 in reader.rs, crc32fast in format.rs and builder.rs, tempfile in builder.rs -- no phantom dependencies.
