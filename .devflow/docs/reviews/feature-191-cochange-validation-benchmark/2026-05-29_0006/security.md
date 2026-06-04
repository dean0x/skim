# Security Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-29

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

- **PID reuse race in timeout kill logic** - `crates/rskim-bench/src/cochange/validate.rs:680` (Confidence: 65%) — After timeout, `libc::kill(child_id)` could theoretically target a recycled PID if the child exits between the timeout and the kill call. The window is extremely small and this is a benchmark binary (not a daemon), so practical risk is negligible. The same pattern also exists in `clone.rs:100` (pre-existing, reused here).

- **Idempotent clone check uses existence only** - `crates/rskim-research/src/clone.rs:301` (Confidence: 60%) — `clone_with_history` skips re-cloning if `dest.exists()`, but a partial clone (interrupted network) would leave a broken directory. A more robust check would verify `.git/HEAD` exists inside `dest`. This is a pre-existing pattern carried over to the new function.

- **`save_to_devflow` writes to a relative path** - `crates/rskim-bench/src/bin/cochange_validate.rs:266` (Confidence: 62%) — The report is written to `.devflow/docs/` relative to cwd. If the binary is invoked from an unexpected working directory, the file lands in an unintended location. Not a security vulnerability per se, but a robustness concern for a tool that writes to disk.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Security Score**: 9/10
**Recommendation**: APPROVED

## Security Analysis Details

The changes introduce a benchmark binary (`cochange-validate`) that clones OSS repositories, parses git history, and evaluates co-change prediction accuracy. The security posture is strong:

### Positive Security Controls Observed

1. **URL scheme validation** (`clone.rs:296`): `clone_with_history` requires `https://` prefix, blocking `file://`, `git://`, and `ssh://` schemes that could be exploited for SSRF or local file access.

2. **Path traversal guard** (`clone.rs:50-64`): `extract_repo_name` rejects `.`, `..`, and names containing `/` or `\\`, preventing directory escape when constructing clone destinations.

3. **Credential helper disabled** (`clone.rs:306-307`): Git commands use `-c credential.helper=` to prevent credential leakage to external helpers during automated cloning.

4. **Object integrity verification** (`clone.rs:309`): `transfer.fsckObjects=true` validates git objects during transfer, mitigating malicious repository attacks.

5. **Subprocess timeout** (`clone.rs:90`, `validate.rs:668`): All git subprocesses have bounded execution time (300s for clone, 30s for rev-parse) with kill-on-timeout, preventing resource exhaustion.

6. **Bounded parallelism** (`cochange_validate.rs:122-124`): Thread pool capped at 3 threads, preventing unbounded resource consumption.

7. **Input validation on thresholds** (`cochange_validate.rs:200`): Rejects NaN, out-of-range values, and empty input.

8. **File count cap** (`validate.rs:48,193-199`): `MAX_FILES_FOR_EVALUATION = 20_000` prevents O(F^2) algorithmic complexity attacks from maliciously crafted repositories.

9. **Corpus config is static TOML** (`cochange-corpus.toml`): URLs are hardcoded to well-known public GitHub repos; no user-supplied URLs reach the clone path in normal operation.

### Applies ADR-001

All findings reported regardless of severity (applies ADR-001). In this case, the 3 suggestions are all below the 80% confidence threshold and represent theoretical rather than practical risks in a benchmark-only binary.
