# Security Review Report

**Branch**: feature/189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T15:31

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Path traversal via symlink in `--blast-radius` normalization** - `crates/rskim/src/cmd/search/temporal.rs:82`
**Confidence**: 82%
- Problem: The `normalize_blast_radius_path` function resolves symlinks via `canonicalize()` after the existence check but before the `strip_prefix` containment check. A symlink inside the repo root that points to a file outside the repo would pass the `root_relative.exists()` check (line 62) and then be canonicalized to the real path (line 82). The subsequent `strip_prefix(&canonical_root)` on line 90 would reject this case correctly. However, if the symlink target happens to share a prefix with the canonical root (e.g., on systems where `/tmp` resolves to `/private/tmp`), the containment check could theoretically pass for paths that conceptually escape the repo. This is defense-in-depth: the current code does reject out-of-root paths, but the check relies on string-prefix matching of canonicalized paths rather than true directory containment.
- Fix: The current implementation is functionally correct for its threat model (local CLI tool, not networked). No code change required -- this is an observation for future hardening if the path normalization is ever exposed through an API. The `strip_prefix` check is the correct final gate.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Unbounded `git rev-parse HEAD` subprocess** - `crates/rskim/src/cmd/search/temporal.rs:146` (Confidence: 65%) -- The `read_git_head` function spawns `git rev-parse HEAD` without a timeout. In degenerate cases (corrupted `.git`, stale NFS mount), this could hang indefinitely. Consider adding a timeout via `std::process::Command` with a wrapper or using `wait_with_output` with a bounded duration.

- **File path used as SQL parameter from user input** - `crates/rskim/src/cmd/search/temporal.rs:216` and `crates/rskim-search/src/temporal/storage_ops.rs:97` (Confidence: 62%) -- The normalized blast-radius path is passed directly as a parameterized SQL query argument (`?1`). This is correctly parameterized (not string-interpolated), so there is no SQL injection risk. However, the path originates from user CLI input and flows through normalization before reaching the DB. The parameterization is the correct pattern -- this note confirms the pattern was verified, not that there is an issue.

- **`cochanges_for_file` LIMIT 10000 may return large result sets** - `crates/rskim-search/src/temporal/storage_ops.rs:158` (Confidence: 60%) -- The `cochanges_for_file` query has a LIMIT of 10,000 rows. While bounded (good), this is a large upper bound for a per-file lookup. If a single file co-changes with thousands of others, the Vec allocation and subsequent processing could be expensive. The `truncate(limit)` call in `query_standalone` (line 222) caps the final output, but the full 10,000-row result is materialized first.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Security Score**: 9/10
**Recommendation**: APPROVED

## Rationale

This PR demonstrates strong security practices throughout:

1. **SQL injection prevention**: All database queries use parameterized statements (`?1`, `rusqlite::params![]`). No string interpolation in SQL. The new per-file lookups (`hotspot_for_file`, `risk_for_file`, `cochanges_for_file`) and top-N queries (`top_hotspots`, `top_risks`, `top_coldspots`) all follow this pattern consistently.

2. **Path traversal defense**: The `normalize_blast_radius_path` function implements a layered defense: existence check before canonicalization, canonicalization to resolve symlinks and `..` components, and `strip_prefix` containment verification against the canonical project root. Paths outside the repo are rejected with a clear error message. The prior cycle's fix removing `set_current_dir` (thread-safety issue) is confirmed in place.

3. **Input validation at boundaries**: The `--blast-radius` flag validates its argument exists on disk and is within the project root before any processing. The `--hot`/`--cold`/`--risky` flags enforce mutual exclusivity at parse time. The `--limit` validation (>= 1, numeric) is preserved. Unknown flags produce clear errors.

4. **Bounded queries**: All SQL queries have explicit `LIMIT` clauses. The `cochanges_for_file` uses LIMIT 10000, and `top_*` methods parameterize their limit from the CLI `--limit` flag. The existing `MAX_ROWS_PER_TABLE` (500,000) capacity checks on store/load operations are unchanged.

5. **Graceful degradation without information leakage**: When the temporal database is missing, the tool warns on stderr and exits 0 -- it does not expose internal paths or stack traces. Error messages from path normalization are user-friendly ("blast-radius file not found") rather than exposing canonicalized absolute paths of unrelated directories.

6. **No secrets, no credentials**: No hardcoded tokens, API keys, or sensitive data introduced. The `file_filter` field is correctly `#[serde(skip)]` so it never leaks through JSON serialization.

7. **No command injection**: The `git rev-parse HEAD` subprocess uses explicit argument passing via `.arg()` -- the project root path is never interpolated into a shell string.
