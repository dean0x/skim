# Security Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-29T04:47:00Z

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

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

### Command Injection and URL Validation

The `clone_with_history` function in `clone.rs` enforces an `https://` prefix check on all repository URLs before passing them to `git clone` (line 349). This prevents `file://`, `git://`, and SSH-based scheme injection. The `extract_repo_name` function (line 51) performs path traversal validation, rejecting `.`, `..`, `/`, and `\` in extracted names. These defenses are consistent with the existing `clone_repo` patterns in the codebase and are correctly applied by `validate_repo` at line 358 before any filesystem operations. The credential helper is explicitly disabled via `-c credential.helper=` on all git subprocess invocations (line 370, line 768), preventing credential leakage to external helpers. `transfer.fsckObjects=true` is set (line 372) to reject corrupted objects during clone.

### Subprocess Timeout and Resource Control

Both `git_run_with_timeout` and `git_output_with_timeout` enforce bounded execution via `recv_timeout`, with SIGKILL on timeout and thread joining after the kill (lines 113-116 and 164-165). This prevents zombie processes and indefinitely detached threads. The `capture_head_sha` function uses the shared `git_output_with_timeout` helper with a 30-second cap (line 763). The `unsafe { libc::kill(...) }` call (lines 101-103, 153-155) is correctly scoped with a valid pid obtained from `child.id()`, and the SAFETY comment is present. The concurrent repo processing is capped at 3 threads (line 123), preventing unbounded parallelism.

### Bounded Computation

The evaluation pipeline has explicit upper bounds on all computational dimensions:
- `MAX_FILES_FOR_EVALUATION = 20_000` (line 48) prevents O(F^2) explosion
- `MAX_TEST_COMMITS = 50_000` (line 55) prevents wall-time explosion
- `MAX_FILES_PER_COMMIT = 500` (line 63) prevents single-commit memory spikes
- `build_path_map` validates `unique_paths.len() <= u32::MAX` (line 87) to prevent FileId overflow

All bounds return `Result::Err` rather than panicking, consistent with ADR-001 (applies ADR-001).

### Partial Clone Recovery

The idempotency check in `clone_with_history` (lines 359-365) now validates `.git/HEAD` exists before treating a clone as complete. A partial or interrupted clone directory is removed and re-cloned, preventing silent data corruption from broken repository state.

### Input Validation at CLI Boundary

`parse_thresholds` validates inputs are finite, in range `(0.0, 1.0]`, and rejects NaN (lines 200-201). `temporal_split` clamps `train_fraction` and handles NaN via a default fallback (lines 80-84). These are defense-in-depth against malformed CLI inputs.

### Corpus Configuration

The `cochange-corpus.toml` file contains only public GitHub HTTPS URLs to well-known OSS projects. No authentication tokens, private repository URLs, or secrets are present. The `commit` field pins specific SHAs for reproducibility but the benchmark clones full history and stays at HEAD, so the pin is informational for the corpus config format.

### Tempdir Usage

The co-change matrix is built in a `tempfile::tempdir()` (line 585), which is automatically cleaned up on drop. This prevents persistent disk accumulation from benchmark runs.

### No Secrets or Credentials

No hardcoded secrets, API keys, tokens, or credentials were found in any of the 14 changed files. All git operations disable credential helpers explicitly.

### Cross-Cycle Awareness

Prior cycle 2 fixed `capture_head_sha` deduplication, detached thread joins, and partial clone validation. All three fixes are confirmed present and correctly integrated in the current diff. The `libc` dependency was removed from `rskim-bench/Cargo.toml` in cycle 2 (capture_head_sha now delegates to `git_output_with_timeout` in `rskim_research::clone`), but `libc` reappears in `Cargo.lock` because `clone.rs` still uses `libc::kill` directly in `git_run_with_timeout` and `git_output_with_timeout` -- this is correct and expected at the `rskim-research` crate level.
