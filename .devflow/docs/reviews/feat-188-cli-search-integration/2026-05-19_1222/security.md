# Security Review Report

**Branch**: feat/188-cli-search-integration -> main
**Date**: 2026-05-19
**Scope**: Incremental (459d0af...HEAD, 2 commits)

## Issues in Your Changes (BLOCKING)

(none)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

(none)

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Security Score**: 9/10
**Recommendation**: APPROVED

## Analysis Notes

This incremental diff (2 commits) introduces several security-positive changes with no new vulnerabilities:

### Security Improvements Introduced

1. **Path traversal prevention in `read_git_head`** (`staleness.rs:104-108`): The new `ref_path.starts_with("refs/")` guard prevents a crafted `.git/HEAD` containing `ref: ../../etc/shadow` from reading arbitrary files. This closes a real TOCTOU-adjacent file read via symref resolution. Validated by a dedicated test (`test_read_git_head_rejects_path_traversal_ref`).

2. **TOCTOU fix in `write_hook_atomic`** (`hooks.rs:176-205`): Replaced the predictable `.tmp` suffix with `NamedTempFile::new_in()`, which creates an unpredictably-named temp file. This eliminates the symlink attack vector where an adversary could pre-create `<hook>.tmp` as a symlink to redirect the write to an arbitrary target. The temp file is persisted atomically via rename.

3. **Concurrency protection via advisory lock** (`index.rs:164-181`): Added `File::lock()` (exclusive flock) around the build pipeline. This prevents concurrent `skim init`, git-hook `--update`, and manual `--build` from racing to write `index.skidx` / `index.skfiles`, which could have produced a corrupt index. The lock is process-scoped and automatically released on drop.

4. **Input validation hardening in `parse_flags`** (`mod.rs:120`): Changed from silently ignoring invalid `--limit` values to returning `Result<Flags>`, rejecting non-numeric values and unrecognised flags. This prevents unexpected behavior from malformed CLI input.

5. **SHA-256 hash support** (`staleness.rs:155-162`): `is_hex_sha` now accepts 64-character hex strings alongside 40-character, supporting `extensions.objectFormat = sha256` repos. Uses `.bytes().all()` instead of `.chars().all()` for efficiency on ASCII-only input.

6. **Infinite rebuild loop elimination** (`staleness.rs:210-230`): The 4-state matrix for `(stored HEAD, current HEAD)` prevents the case where a non-git project would cycle `NoStoredHead -> rebuild -> NoStoredHead -> rebuild...` indefinitely. `(None, None)` now returns `Current`.

### Areas Reviewed (No Issues Found)

- **File size guard** (`snippet.rs:134-142`): 5 MB cap on snippet file reads prevents memory exhaustion when resolving 20 results simultaneously. Matches the index-build cap.
- **Lock file creation** (`index.rs:169-173`): Uses `create(true).truncate(false)` which is safe -- creates if absent, opens if present, never truncates lock file content.
- **HashMap to BTreeMap migration** (`manifest.rs`): No security implications -- deterministic iteration order is a correctness improvement.
- **Background process handling** (`install.rs:314-331`): Explicit `drop(child)` with documented zombie-window reasoning. The build lock prevents concurrent writes.
