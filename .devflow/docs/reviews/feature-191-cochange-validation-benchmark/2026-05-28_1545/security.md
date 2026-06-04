# Security Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-28T15:45

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**Path traversal: `validate_repo` extracts repo name without `extract_repo_name` validation** - `crates/rskim-bench/src/cochange/validate.rs:327-335`
**Confidence**: 85%
- Problem: `validate_repo` extracts `repo_name` using a simple `rsplit('/').next().unwrap_or("unknown").trim_end_matches(".git")` and immediately joins it into a filesystem path via `corpus_dir.join(&repo_name)`. This skips the `extract_repo_name()` validation already present in `clone.rs:51-65` which rejects `.`, `..`, and names containing `/` or `\`. A malicious `url` field in `cochange-corpus.toml` such as `https://evil.com/../../../etc` would produce `repo_name = "etc"` (benign), but `https://evil.com/..` would yield `repo_name = ".."`, causing `dest = corpus_dir.join("..")` to escape the corpus directory. While `clone_with_history` also validates the HTTPS prefix, the `dest` path is passed directly to `GixSource.parse_history` and `capture_head_sha` before the clone would fail, and more importantly the pattern diverges from the existing safe `extract_repo_name` function without justification.
- Fix: Reuse the existing `extract_repo_name` function from `rskim_research::clone` (or make it `pub` and call it):
```rust
let repo_name = rskim_research::clone::extract_repo_name(&entry.url)
    .unwrap_or_else(|_| "unknown".to_string());
```
Alternatively, inline the same validation:
```rust
let repo_name = entry.url.rsplit('/').next().unwrap_or("unknown")
    .trim_end_matches(".git").to_string();
if repo_name == "." || repo_name == ".." || repo_name.contains('/') || repo_name.contains('\\') {
    return Ok(error_result(entry, &repo_name, "unsafe repo name".to_string()));
}
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`save_to_devflow` timestamp sanitization incomplete** - `crates/rskim-bench/src/bin/cochange_validate.rs:267` (Confidence: 65%) -- The timestamp is sanitized by replacing `:` and `/` with `-`, but the `chrono_now` function is internally controlled so it cannot currently produce traversal characters. If the timestamp generation ever changes or is user-supplied, the `..` pattern is not stripped. Low risk because the timestamp is internally generated.

- **`clone_with_history` idempotency trusts existing directory** - `crates/rskim-research/src/clone.rs:301-303` (Confidence: 70%) -- If `dest.exists()` returns true, the function skips cloning entirely. A pre-existing directory at that path (placed by an attacker or a previous partial clone) would be trusted without verifying it contains a valid git repository. The existing `clone_repo` function has the same pattern so this is consistent, but in the benchmark context where the corpus directory is user-controllable via `--corpus-dir`, a non-git directory would cause confusing failures in `GixSource.parse_history` rather than a security breach.

- **Parallel repo processing shares error messages across threads** - `crates/rskim-bench/src/cochange/validate.rs:321-494` (Confidence: 60%) -- The `validate_repo` function embeds error messages from cloned repository operations (clone failure messages, git stderr) into `RepoCochangeResult.error` which is later serialized to JSON/Markdown output. If a malicious repository name or error output contains injection content (e.g., Markdown injection), it would appear in the report. This is a benchmark tool so the risk is minimal.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Security Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

### Notes

**What was done well (applies ADR-001):**

1. **HTTPS-only clone restriction** -- `clone_with_history` at `clone.rs:296-297` correctly enforces `url.starts_with("https://")`, blocking `git://`, `file://`, and `ssh://` schemes that could enable SSRF or local file access. This matches the existing `clone_repo` pattern.

2. **Git security hardening** -- Both `clone_with_history` and the existing `clone_repo` apply `credential.helper=` (suppress credential prompts) and `transfer.fsckObjects=true` (reject corrupted/malicious git objects). The new function correctly reuses the same security args pattern.

3. **Subprocess timeout** -- The clone operation goes through `git_run_with_timeout` with a 300-second hard kill, preventing denial-of-service from a hanging clone.

4. **Deny-list filtering** -- The deny-list module correctly filters lock files, vendored directories, and machine-generated content before analysis. Path normalization handles Windows backslashes.

5. **Input validation on thresholds** -- `parse_thresholds` validates range `(0.0, 1.0]`, rejects NaN, and requires at least one value. The `train_fraction` is clamped to `[0.01, 0.99]` with NaN guard.

6. **Tempdir usage** -- Co-change matrices are built in `tempfile::tempdir()` which is automatically cleaned up, preventing persistent state leakage.

**Blocking finding rationale:**

The single HIGH finding (path traversal via unsanitized repo name) is a defense-in-depth gap -- the HTTPS check in `clone_with_history` provides partial protection, but the repo name extraction in `validate_repo` diverges from the established safe pattern (`extract_repo_name`) already present in the same crate. Since the corpus URLs come from a TOML config file that could be user-supplied via `--corpus-config`, this should use the same validation (avoids PF-002 -- not deferring a noticed issue).
