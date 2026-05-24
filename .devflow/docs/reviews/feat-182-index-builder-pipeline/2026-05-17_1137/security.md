# Security Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17
**Snyk SAST**: 0 issues found

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**TOCTOU race between file size check and file read** - `walk.rs:147-166`
**Confidence**: 82%
- Problem: `fs::metadata(abs_path)` checks the file size at line 147-157, then `fs::read_to_string(abs_path)` reads the file at line 166. Between the check and the read, a file could be replaced with a much larger file (symlink swap or rapid write). Since `read_to_string` reads the entire file into memory without a size limit, this could cause OOM on a crafted input where a file grows between the metadata check and the read.
- Impact: An attacker with local write access to the indexed directory could cause the skim process to allocate unbounded memory by swapping a small file for a multi-gigabyte file between the metadata call and the read call. Practically exploitable only in adversarial local-user scenarios (e.g., shared CI build agent indexing an untrusted repo).
- Fix: Read the file first (bounded), then check size on the bytes read, or use `fs::File::open` + `metadata()` on the open handle + `read_to_string` with a pre-allocated bounded buffer:
  ```rust
  let file = fs::File::open(abs_path)?;
  let meta = file.metadata()?;
  if meta.len() > MAX_FILE_BYTES {
      // skip
      continue;
  }
  let mut content = String::with_capacity(meta.len() as usize);
  use std::io::Read;
  file.take(MAX_FILE_BYTES + 1).read_to_string(&mut content)?;
  if content.len() as u64 > MAX_FILE_BYTES {
      // file grew between metadata and read
      continue;
  }
  ```

### MEDIUM

**`--max-files=0` allows zero-file cap with no validation** - `index.rs:97-101`, `types.rs:33-34`
**Confidence**: 85%
- Problem: `--max-files=0` parses successfully as `usize` and passes through to `walk_and_read`. With `max_files=0`, the walker immediately pushes `SkipReason::CapReached` and breaks, producing an empty index. While not exploitable, this is a missing boundary validation — `0` is not a "positive integer" as the error message claims, yet it is accepted.
- Impact: A user passing `--max-files=0` gets a silently empty index with no error, which could mask misconfiguration. The error message says "requires a positive integer" but accepts zero.
- Fix: Validate that the parsed value is `>= 1`:
  ```rust
  let n = val.parse::<usize>()
      .map_err(|_| anyhow::anyhow!("--max-files requires a positive integer"))?;
  if n == 0 {
      anyhow::bail!("--max-files requires a positive integer");
  }
  max_files = Some(n);
  ```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Cache directory hash uses truncated SHA-256 (64-bit / 16 hex chars)** - `index.rs:277-287`
**Confidence**: 80%
- Problem: `project_root_hash` truncates SHA-256 to 8 bytes (16 hex chars). With 64 bits of entropy, the birthday bound for a collision is ~2^32 (~4 billion) distinct project roots, which is practically unreachable for any single user. However, the truncation reduces collision resistance from 128-bit (SHA-256 second preimage) to 64-bit. If two different project roots collide, their indexes silently overwrite each other's cache, leading to incorrect search results.
- Impact: Negligible in practice for the local-user single-machine use case. If this were ever extended to a shared/multi-tenant cache, collisions could corrupt indexes across projects. The wrong-root detection in the manifest header mitigates this — a collision would cause the manifest to be discarded (cold start), not silent corruption of search results.
- Fix: Consider using 16 bytes (32 hex chars) for a stronger collision margin, or document the intentional trade-off. Given the manifest wrong-root detection already guards against silent corruption, this is acceptable as-is with a comment documenting the rationale:
  ```rust
  // 64-bit hash: birthday bound ~4B roots. Wrong-root detection in
  // manifest header prevents silent corruption on collision (forces cold start).
  ```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Manifest deserialization from untrusted JSONL** - `manifest.rs:160` (Confidence: 65%) — The manifest file is deserialized with `serde_json::from_str` without size limits on individual entries. A crafted manifest with extremely large `field_map` arrays could cause high memory allocation. Mitigated by the fact that the manifest is written by skim itself and lives in the user's own cache directory, so the attacker would need local write access.

- **`discover_project_root` walks up to filesystem root** - `walk.rs:52-68` (Confidence: 60%) — The `.git` discovery loop walks from `start` all the way to `/`. On deeply nested paths this is O(depth) filesystem stat calls, and on network-mounted filesystems each `.exists()` check could be slow. Not a security vulnerability per se, but could be a denial-of-service vector if invoked on a network mount with high latency. A depth bound (e.g., 100 ancestors) would make this more robust.

- **`--index-dir` flag exposed without documentation in help text** - `index.rs:102-104` (Confidence: 70%) — The `--index-dir` flag controls where index files and the manifest are written but is deliberately undocumented (internal/test). If a user discovers it, they could point it at arbitrary writable paths. Since this is a local CLI tool and the user already has shell access, this is not a privilege escalation, but the flag should be noted as internal in a code comment (which it already is at line 103).

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Security Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The code demonstrates strong security practices overall:
- Atomic manifest writes via `NamedTempFile` + `persist()` prevent partial-write corruption
- Wrong-root detection prevents cross-project cache confusion
- `.gitignore` respecting walker prevents indexing of secrets in ignored directories
- Symlink following is explicitly disabled (`follow_links(false)`)
- File size caps prevent unbounded memory allocation (with the TOCTOU caveat noted above)
- SHA-256 content hashing for cache integrity is a solid choice
- No `unsafe` code, no hardcoded secrets, no shell command injection surfaces
- Snyk SAST scan returned zero findings

**Conditions for merge**: Fix the TOCTOU race in walk.rs (HIGH) by checking size on the open file handle rather than a separate metadata call. The `--max-files=0` validation (MEDIUM) should also be addressed. The truncated cache hash (MEDIUM, Should Fix) is acceptable with a documenting comment given the manifest wrong-root guard.
