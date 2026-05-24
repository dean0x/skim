# Security Review Report

**Branch**: feat/181-cochange-matrix-builder -> main
**Date**: 2026-05-24
**PR**: #250

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Unchecked arithmetic in mmap slice accessors can panic on crafted .skcc files** - `reader.rs:219,225`
**Confidence**: 85%
- Problem: `file_commit_slice()` and `pairs_slice()` compute slice boundaries using unchecked multiplication and addition (`(self.header.file_count as usize) * FILE_COMMIT_ENTRY_SIZE`). While `open()` validates the expected total file size with `checked_mul`/`checked_add`, the private slice helpers recompute the same offsets without checked arithmetic. On platforms where `u32::MAX * 12` overflows `usize` (32-bit targets), the `as usize` cast followed by unchecked multiply could wrap, producing a slice range that panics or reads incorrect memory regions from the mmap.
- Fix: Reuse the validated byte offsets computed during `open()` by storing `fc_bytes` and `pair_bytes` (or `fc_end` and `pairs_end`) as fields on `CochangeMatrixReader`, rather than recomputing them:
```rust
pub struct CochangeMatrixReader {
    header: SkccHeader,
    mmap: Mmap,
    fc_end: usize,   // HEADER_SIZE + fc_bytes (validated in open)
    pairs_end: usize, // fc_end + pair_bytes (validated in open)
}

fn file_commit_slice(&self) -> &[u8] {
    &self.mmap[HEADER_SIZE..self.fc_end]
}

fn pairs_slice(&self) -> &[u8] {
    &self.mmap[self.fc_end..self.pairs_end]
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Temp file created without explicit restrictive permissions** - `builder.rs:255`
**Confidence**: 80%
- Problem: `NamedTempFile::new_in(dir)` creates the temp file with default OS permissions. On Unix systems with a permissive umask (e.g., `0000`), the temp file and the persisted `cochange.skcc` file could be world-readable. While the data is not secret (it is derived from git history), index files that influence search ranking could be tampered with if writable by other users on a shared system. The `persist()` call preserves the temp file's permissions.
- Fix: After creating the temp file, explicitly set permissions to owner-only read/write before persisting:
```rust
fn atomic_write(dir: &Path, path: &Path, data: &[u8]) -> Result<()> {
    let mut tmp = NamedTempFile::new_in(dir)?;
    use std::io::Write as _;
    tmp.write_all(data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o644);
        tmp.as_file().set_permissions(perms)?;
    }
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}
```

## Pre-existing Issues (Not Blocking)

No pre-existing CRITICAL security issues found in unchanged code.

## Suggestions (Lower Confidence)

- **Symlink-following on output_dir** - `builder.rs:51` (Confidence: 65%) -- `output_dir.exists()` follows symlinks. A symlink pointing to an attacker-controlled directory could cause the builder to write `cochange.skcc` outside the intended location. Consider canonicalizing or checking that `output_dir` is not a symlink if the builder is ever exposed to user-supplied paths.

- **CRC32 is not a cryptographic integrity check** - `format.rs:287` (Confidence: 60%) -- CRC32 detects accidental corruption but is trivially forgeable. If an attacker can replace `cochange.skcc` on disk, they can craft a valid CRC32 for malicious content. This is acceptable for the current use case (local index files, not a trust boundary) but worth documenting explicitly.

- **Mmap TOCTOU between open() and subsequent reads** - `reader.rs:62-104` (Confidence: 70%) -- The module-level doc already acknowledges the inherent mmap TOCTOU risk. If another process truncates the file after the mmap is established but before reads complete, undefined behavior can occur. For the current use case (single-user local tool), this is acceptable. If this is ever used in a multi-process server context, consider advisory file locking.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Security Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The cochange module demonstrates strong security practices overall: all binary parsing uses bounds-checked reads with `Result` returns (no panics on malformed input), the builder enforces safety caps (`COUPLING_MAX_FILES=50`, `MAX_PAIRS=2M`) to prevent resource exhaustion, arithmetic overflow is handled with `checked_*` operations and `saturating_add`, CRC32 integrity validation catches accidental corruption, and atomic writes via tempfile prevent partial-read scenarios. The two MEDIUM findings are defense-in-depth improvements for edge cases (32-bit platform overflow in slice helpers, default file permissions) rather than exploitable vulnerabilities in the current deployment context.
