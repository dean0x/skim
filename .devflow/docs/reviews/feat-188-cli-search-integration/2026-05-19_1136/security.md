# Security Review Report

**Branch**: feat/188-cli-search-integration -> main
**Date**: 2026-05-19

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Predictable temp file path in `write_hook_atomic` enables symlink race** - `crates/rskim/src/cmd/search/hooks.rs:176`
**Confidence**: 80%
- Problem: `write_hook_atomic` creates a temp file at a predictable path (`hook_path.with_extension("tmp")`, e.g. `.git/hooks/post-commit.tmp`) using `std::fs::write`. If a local attacker with write access to `.git/hooks/` places a symlink at that path before the write, the content would be written to the symlink target, and then `set_permissions(0o755)` would set the target executable. The manifest module correctly uses `NamedTempFile` (random name); hooks should follow the same pattern.
- Fix: Use `tempfile::NamedTempFile::new_in()` (already a dependency) instead of a predictable `.tmp` extension:
```rust
fn write_hook_atomic(hook_path: &Path, content: &str) -> anyhow::Result<()> {
    let parent = hook_path.parent()
        .ok_or_else(|| anyhow::anyhow!("hook path has no parent"))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(content.as_bytes())?;
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

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Unsanitized ref path from `.git/HEAD` used in file read** - `crates/rskim/src/cmd/search/staleness.rs:88-90,104`
**Confidence**: 80%
- Problem: `read_git_head` reads `.git/HEAD`, strips `ref: `, and passes the remainder directly to `git_dir.join(ref_path)` for a `read_to_string`. A crafted `.git/HEAD` containing `ref: ../../etc/shadow` would cause the code to read an arbitrary file outside the git directory. While the result must pass `is_hex_sha` to be returned (limiting data exfiltration), the `read_to_string` call itself loads arbitrary file content into memory. In a local-only CLI tool the threat model is limited (the attacker already has local file access), but defense-in-depth recommends validating that `ref_path` starts with `refs/` before using it as a file path component.
- Fix: Add a prefix check before path resolution:
```rust
if let Some(ref_path) = head_str.strip_prefix("ref: ") {
    // Defense-in-depth: only follow refs that start with "refs/"
    if ref_path.starts_with("refs/") {
        resolve_symbolic_ref(&git_dir, ref_path)
    } else {
        None
    }
}
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`is_hex_sha` rejects SHA-256 object IDs** - `crates/rskim/src/cmd/search/staleness.rs:135-137` (Confidence: 70%) -- The function only accepts 40-char hex strings (SHA-1). Git repositories using the SHA-256 object format produce 64-char hashes. Detached HEAD on such repos would silently fail staleness detection, causing unnecessary rebuilds. Consider accepting both 40 and 64 character hex strings.

- **`resolve_git_dir` follows arbitrary gitdir pointers without path validation** - `crates/rskim/src/cmd/search/staleness.rs:57-64` (Confidence: 65%) -- The worktree `.git` file parser follows the `gitdir:` pointer to any path on the filesystem. A crafted `.git` file could point to a directory outside the repository. The returned path is only used for reading `.git/HEAD` and ref files (read-only), so the risk is limited to information disclosure of file existence and content snippets. Consider validating the resolved path is within expected locations (e.g., parent `.git/worktrees/` directory).

- **No upper bound on `--limit` flag** - `crates/rskim/src/cmd/search/mod.rs:140` (Confidence: 60%) -- The `--limit` flag accepts any `usize` value without a cap. An extremely large limit combined with a large index could cause excessive memory allocation when resolving results and extracting snippets. Consider capping to a reasonable maximum (e.g., 1000).

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Security Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The code demonstrates strong security practices overall: atomic writes for the manifest, bounded manifest parsing (size and entry count caps), hardcoded hook content with no user-input interpolation, proper `Result` propagation, and validated SHA outputs. The two MEDIUM findings are defense-in-depth improvements rather than exploitable vulnerabilities in the CLI's local-only threat model. The predictable temp file in `write_hook_atomic` is the most actionable item since `tempfile::NamedTempFile` is already a project dependency and used correctly in the manifest module.
