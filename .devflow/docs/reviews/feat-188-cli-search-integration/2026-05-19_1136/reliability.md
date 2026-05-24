# Reliability Review Report

**Branch**: feat/188-cli-search-integration -> main
**Date**: 2026-05-19

## Issues in Your Changes (BLOCKING)

### HIGH

**No file-size guard on snippet extraction at query time** - `crates/rskim/src/cmd/search/snippet.rs:127`
**Confidence**: 90%
- Problem: `extract_snippet` reads the entire file into memory via `std::fs::read(&abs_path)` without any size check. The index build pipeline enforces a 5 MB cap (`MAX_FILE_BYTES` in `walk.rs`), but snippet extraction at query time bypasses this entirely. A file that grew substantially since indexing (or a symlink that was retargeted to a large file) would be read fully into memory per search result.
- Impact: On a query returning 20 results, up to 20 large files could be read into memory simultaneously. A pathological case (e.g., log files or generated artifacts) could exhaust available memory.
- Fix: Add a metadata size check before reading, consistent with the indexing cap:
```rust
// In extract_snippet, before reading the file:
const MAX_SNIPPET_FILE_BYTES: u64 = 5 * 1024 * 1024;
let meta = match std::fs::metadata(&abs_path) {
    Ok(m) => m,
    Err(_) => return SnippetOutcome::Unavailable,
};
if meta.len() > MAX_SNIPPET_FILE_BYTES {
    return SnippetOutcome::Unavailable;
}
```

**No concurrent index build protection (TOCTOU race)** - `crates/rskim/src/cmd/init/install.rs:316` and `crates/rskim/src/cmd/search/staleness.rs:199`
**Confidence**: 85%
- Problem: `execute_install` spawns a background `skim search --build` process. Separately, `auto_refresh_if_stale` (called on every query) can also trigger `build_index`. The git hooks (`post-commit`, `post-merge`, `post-checkout`) run `skim search --update` which also calls `auto_refresh_if_stale`. There is no file lock or coordination mechanism. Two concurrent builds can corrupt the index files or manifest by racing on writes to the same `index.skidx` and `index.skfiles` files.
- Impact: Index corruption requiring a `--rebuild`. The manifest uses atomic rename (via `NamedTempFile::persist`), which mitigates partial writes, but two concurrent builders writing to the same temp directory can still produce incoherent index+manifest pairs (builder A writes index, builder B writes manifest on top of A's).
- Fix: Add an advisory file lock in the cache directory before building. Example using `fs2` or `fd-lock`:
```rust
use std::fs::File;
use fs2::FileExt;

let lock_path = cache_dir.join(".skim-build.lock");
let lock_file = File::create(&lock_path)?;
if lock_file.try_lock_exclusive().is_err() {
    eprintln!("skim search: another build is in progress, skipping");
    return Ok(false);
}
// ... proceed with build ...
// lock released on drop
```

**Predictable temp file name in `write_hook_atomic`** - `crates/rskim/src/cmd/search/hooks.rs:176`
**Confidence**: 82%
- Problem: `write_hook_atomic` uses a deterministic temp path: `hook_path.with_extension("tmp")`. This means every `post-commit` hook write goes to `.git/hooks/post-commit.tmp`. If two processes call `install_search_hooks` concurrently, they race on the same temp file. The manifest module correctly uses `NamedTempFile` (random names) -- this should too.
- Impact: On concurrent `skim init` invocations, one process's temp file content could be clobbered by the other before the rename, resulting in a corrupted hook script.
- Fix: Use `tempfile::NamedTempFile` in the hooks directory, matching the pattern used in `manifest.rs:320`:
```rust
fn write_hook_atomic(hook_path: &Path, content: &str) -> anyhow::Result<()> {
    let dir = hook_path.parent().unwrap_or(Path::new("."));
    let tmp = tempfile::NamedTempFile::new_in(dir)?;
    std::fs::write(tmp.path(), content)?;
    #[cfg(unix)]
    {
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(tmp.path(), perms)?;
    }
    tmp.persist(hook_path)
        .map_err(|e| anyhow::anyhow!("failed to persist hook: {}", e.error))?;
    Ok(())
}
```

### MEDIUM

**No size guard on `read_to_string` calls for git internal files** - `crates/rskim/src/cmd/search/staleness.rs:114`
**Confidence**: 80%
- Problem: `resolve_symbolic_ref` reads `packed-refs` via `std::fs::read_to_string` without any file-size check. While `packed-refs` is normally small, in pathological repositories (millions of refs, or a corrupted/symlinked `.git` directory), it could be arbitrarily large. The manifest module explicitly guards against this with `MAX_MANIFEST_FILE_BYTES`.
- Impact: Unbounded memory allocation on a malformed or unusually large `packed-refs` file. Not exploitable in normal workflows, but violates the "every resource must be bounded" principle.
- Fix: Add a metadata size check before `read_to_string`, e.g. cap at 16 MB:
```rust
const MAX_GIT_INTERNAL_FILE: u64 = 16 * 1024 * 1024;
let meta = std::fs::metadata(&packed_refs_path).ok()?;
if meta.len() > MAX_GIT_INTERNAL_FILE {
    return None;
}
```

**Truncating `usize` to `u32` for line count without check** - `crates/rskim/src/cmd/search/snippet.rs:67`
**Confidence**: 80%
- Problem: `lines.len() as u32` silently truncates if the line count exceeds `u32::MAX`. While the 5 MB file size cap (at index time) makes this practically impossible (~4 billion lines in 5 MB would require sub-byte lines), snippet extraction has no file size cap (see first finding above). If the size guard is not added, a very large file could produce a truncated `total_lines` value, causing incorrect line number calculations and potential out-of-bounds access at line 81 (`lines[idx]`).
- Impact: Panic via index out of bounds if truncation occurs. Practically low risk given normal file sizes, but the cast should be defensive.
- Fix: Use `u32::try_from` with a fallback:
```rust
let total_lines = u32::try_from(lines.len()).unwrap_or(u32::MAX);
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`is_hex_sha` rejects SHA-256 object IDs** - `crates/rskim/src/cmd/search/staleness.rs:135-137`
**Confidence**: 82%
- Problem: `is_hex_sha` accepts only exactly 40-character hex strings (SHA-1). Git repositories using the SHA-256 object format (introduced in Git 2.29, enabled via `extensions.objectFormat = sha256`) produce 64-character hex commit hashes. Such repositories would fail all staleness checks because `read_git_head` would return `None` for valid detached HEAD SHAs and for ref resolution.
- Impact: Staleness detection silently breaks for SHA-256 git repos. Every query would trigger a full rebuild (treating the index as having `NoStoredHead`), defeating incremental builds.
- Fix: Accept both 40-char (SHA-1) and 64-char (SHA-256) hex strings:
```rust
fn is_hex_sha(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.chars().all(|c| c.is_ascii_hexdigit())
}
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Background build orphan has no error reporting** - `crates/rskim/src/cmd/init/install.rs:316` (Confidence: 70%) -- The spawned `skim search --build` process runs with `stdout` and `stderr` nulled. If it fails, the user gets no feedback. Consider logging to a file in the cache directory (e.g., `build.log`) so users can diagnose post-install failures.

- **`extract_context_window` collects all lines then indexes a small window** - `crates/rskim/src/cmd/search/snippet.rs:66` (Confidence: 65%) -- `content.lines().collect()` allocates a `Vec<&str>` for every line in the file, but only 2*context+1 lines are ever used. For large files, this is wasteful. An iterator-based approach (`.nth()` and `.take()`) would avoid allocating the full line vector.

- **`as_millis() as u64` can truncate on extremely long queries** - `crates/rskim/src/cmd/search/query.rs:77` (Confidence: 60%) -- `Duration::as_millis()` returns `u128`. The cast to `u64` truncates after ~584 million years, so practically harmless, but `u64::try_from(...).unwrap_or(u64::MAX)` would be more idiomatic for a project that uses `saturating_add` elsewhere.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 3 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Reliability Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The code demonstrates strong reliability fundamentals: bounded iteration everywhere (manifest entry cap, file count cap, `CHANNEL_CAPACITY` on the producer channel, `for _ in 0..256` in `find_git_root_from_cwd`), atomic writes with temp-file-then-rename for the manifest, graceful degradation via `SnippetOutcome` and `SkipReason` enums, and comprehensive test coverage for edge cases (corrupted manifests, missing files, oversized manifests). The bounded-channel streaming pipeline with explicit `u32` overflow check (`checked_add`) is particularly well designed.

The two areas that need attention are: (1) the missing file-size guard on snippet extraction, which creates an unbounded memory allocation path at query time, and (2) the lack of any coordination between concurrent index builds, which creates a realistic race condition given that `skim init` spawns a background build while git hooks can trigger concurrent builds.
