# Security Review Report

**Branch**: feature-189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T10:27

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Byte-index string slicing on database-sourced content can panic on non-ASCII** - `crates/rskim/src/cmd/search/temporal.rs:136-137`
**Confidence**: 82%
- Problem: `&stored_head[..stored_head.len().min(7)]` uses byte-index slicing on a `String` retrieved from the SQLite `meta` table. If the stored value contains multi-byte UTF-8 characters and the 7-byte boundary falls mid-character, this will panic at runtime. While git HEAD SHAs are always hex (ASCII), the `meta` table is a generic key-value store. A corrupted or tampered database could store non-ASCII data under the `META_GIT_HEAD` key, triggering a panic in production.
- Fix: Use `stored_head.chars().take(7).collect::<String>()` or `stored_head.get(..7).unwrap_or(&stored_head)` (the latter still byte-indexes but won't panic). Cleanest fix:
  ```rust
  fn safe_prefix(s: &str, max_bytes: usize) -> &str {
      if s.len() <= max_bytes {
          s
      } else {
          let mut end = max_bytes;
          while end > 0 && !s.is_char_boundary(end) {
              end -= 1;
          }
          &s[..end]
      }
  }
  ```
  Then: `safe_prefix(&stored_head, 7)` and `safe_prefix(&current_head, 7)`.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **No upper-bound on `limit` passed to `top_hotspots`/`top_risks`/`top_coldspots`** - `crates/rskim-search/src/temporal/storage_ops.rs:187,217,248` (Confidence: 65%) -- The `limit: usize` parameter is passed directly to SQL `LIMIT ?1` without a ceiling. The CLI's `parse_limit_value` provides a practical guard, but the library API could be hardened with a `min(limit, MAX_ROWS_PER_TABLE)` clamp for defense-in-depth.

- **`std::env::set_current_dir` in tests mutates global process state** - `crates/rskim/src/cmd/search/temporal_tests.rs:55,131` (Confidence: 70%) -- `set_current_dir` affects all threads. Under `cargo test` (parallel by default), concurrent tests relying on CWD could observe stale or wrong directories. This is a test-reliability issue, not a production security concern, but it could mask CWD-dependent path traversal bugs. Consider using `assert_cmd` or passing explicit CWD to avoid global mutation.

- **`open_temporal_db` silently swallows all open errors** - `crates/rskim/src/cmd/search/temporal.rs:120` (Confidence: 62%) -- `TemporalDb::open(db_path).ok()` discards the error variant. A permission-denied or locked-database error is silently treated as "no temporal data." In a multi-process scenario (concurrent `skim heatmap` writes), this could hide WAL lock contention. Consider logging the error to stderr when `SKIM_DEBUG` is set.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Security Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Conditions

1. Fix the byte-index string slicing in `check_temporal_staleness` to use char-boundary-safe slicing (MEDIUM, blocking).

### Security Strengths

- **Parameterized SQL throughout**: All SQLite queries in `storage_ops.rs` use `rusqlite::params![]` with positional parameters (`?1`, `?2`). Zero string interpolation in SQL. No injection surface.
- **Path traversal defense**: `normalize_blast_radius_path` properly canonicalizes paths, strips the project root prefix, and rejects paths outside the repository root. Existence checks happen before canonicalization to prevent confusing error messages.
- **Capacity bounds**: `MAX_ROWS_PER_TABLE` (500,000) is enforced on all store operations, preventing unbounded memory allocation from adversarial database content.
- **Graceful degradation**: Missing `temporal.db` produces exit 0 with a warning, not a crash. Corrupt databases are handled by `open().ok()`.
- **No secrets or credentials**: No hardcoded tokens, no sensitive environment variable handling. The temporal data (file paths, scores) is non-sensitive metadata.
- **Schema migration safety**: V1-to-V2 migration uses `CREATE INDEX IF NOT EXISTS` inside a transaction, making it idempotent and crash-safe.
- **`file_filter` not serializable**: The `#[serde(skip)]` annotation on `SearchQuery.file_filter` prevents the internal `HashSet<FileId>` from leaking through JSON serialization boundaries.
