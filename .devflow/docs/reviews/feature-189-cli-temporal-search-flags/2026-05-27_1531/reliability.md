# Reliability Review Report

**Branch**: feature/189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T15:31

## Issues in Your Changes (BLOCKING)

### MEDIUM

**`read_git_head` spawns subprocess with no timeout** - `crates/rskim/src/cmd/search/temporal.rs:146-152`
**Confidence**: 82%
- Problem: `std::process::Command::new("git").output()` blocks indefinitely if the git process hangs (e.g., a network-backed filesystem holding `.git/HEAD`, or a locked index file). This is called from `check_temporal_staleness`, which runs on every standalone temporal query and every combined query path where temporal flags are active. Although `.output().ok()?` converts failures to `None`, a hanging process never returns an error -- it simply blocks the caller forever.
- Fix: Since this is a local `rev-parse HEAD` (no network), the practical risk is low but non-zero. The most pragmatic fix is to document the assumption that this is a fast local operation, or use a timeout-guarded spawn:
  ```rust
  use std::time::Duration;
  fn read_git_head(root: &Path) -> Option<String> {
      let child = std::process::Command::new("git")
          .arg("-C").arg(root)
          .arg("rev-parse").arg("HEAD")
          .stdout(std::process::Stdio::piped())
          .stderr(std::process::Stdio::null())
          .spawn().ok()?;
      let output = child.wait_with_output().ok()?;
      // Alternatively, use a 5-second timeout via a thread + channel
      if output.status.success() {
          Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
      } else {
          None
      }
  }
  ```
  Realistically, `rev-parse HEAD` is near-instant for local repos. A simple comment noting the assumption would suffice as a minimal fix.

**No upper bound on CLI `--limit` for temporal top-N queries** - `crates/rskim/src/cmd/search/mod.rs:136-143` and `crates/rskim-search/src/temporal/storage_ops.rs:187,217,248`
**Confidence**: 80%
- Problem: `parse_limit_value` validates `>= 1` but has no upper bound. A user can pass `--limit 999999999` which flows to `top_hotspots(limit)`, `top_risks(limit)`, and `top_coldspots(limit)` as `LIMIT ?1` in SQL. While SQLite handles large LIMIT values gracefully (it just returns all rows), the real concern is `limit as i64` cast on line 196/226/257 of `storage_ops.rs` -- on a 64-bit platform `usize::MAX` exceeds `i64::MAX`, which would wrap to a negative value and SQLite would return 0 rows (confusing but not catastrophic). On 32-bit, `usize` fits in `i64`. Additionally, for the `cochanges_for_file` query, the hard-coded `LIMIT 10000` provides an explicit bound, but the `top_*` methods rely entirely on the caller-provided `limit`.
- Fix: Add a reasonable upper bound (e.g., 10,000) to `parse_limit_value`, or clamp the `limit` before the `as i64` cast:
  ```rust
  fn parse_limit_value(raw: &str) -> anyhow::Result<usize> {
      let parsed = raw.parse::<usize>()
          .map_err(|_| anyhow::anyhow!("--limit value must be a positive integer, got {:?}", raw))?;
      if parsed == 0 {
          anyhow::bail!("--limit must be >= 1 (got 0)");
      }
      if parsed > 10_000 {
          anyhow::bail!("--limit must be <= 10000 (got {parsed})");
      }
      Ok(parsed)
  }
  ```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`canonicalize` fallback masks missing-file errors** - `crates/rskim/src/cmd/search/temporal.rs:82` (Confidence: 65%) -- `abs.canonicalize().unwrap_or_else(|_| abs.clone())` silently proceeds with the non-canonical path when canonicalization fails on an existing file (e.g., permission denied on a parent directory). The existence check at line 54-77 mitigates the most common case (missing file), but a permission-denied error would produce an un-canonical path that might fail the `strip_prefix` on line 90-98 with a confusing "outside the project root" error.

- **`resort_partners_by_temporal` allocates a full clone Vec for reordering** - `crates/rskim/src/cmd/search/temporal.rs:292-296` (Confidence: 62%) -- The index-based reordering pattern collects all partners into a new `Vec` via `.clone()`. For the LIMIT 10000 on cochanges this is bounded and acceptable, but a permutation sort could avoid the allocation. Low priority given the bound.

- **`apply_temporal_enrichment` signature returns `Result` but never errors** - `crates/rskim/src/cmd/search/temporal.rs:476-523` (Confidence: 70%) -- The function body only calls `annotate_hotspots`/`annotate_risks` which swallow errors via `eprintln!`. The `Ok(())` return is always reached. The `anyhow::Result` return type is misleading -- callers might expect meaningful error propagation but individual failures are silently degraded.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Reliability Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The code demonstrates strong reliability practices overall: all SQL queries have explicit bounds (LIMIT 10000 on cochanges, caller-provided limits on top-N), store operations enforce `MAX_ROWS_PER_TABLE = 500_000`, all DB errors propagate via `Result` types, per-file lookups handle `QueryReturnedNoRows` gracefully, and the `CapacityExceeded` guard prevents unbounded insert loops. The `unchecked_transaction` usage is correctly justified by the `Send`-but-not-`Sync` design. The prior cycle-1 fixes (LIMIT on cochanges, error propagation, CWD mutation removal) are all in place and verified.

The two MEDIUM findings are defense-in-depth improvements: (1) the subprocess spawn has no timeout, though `rev-parse HEAD` is practically instant for local repos, and (2) the CLI `--limit` has no upper bound, though the cast overflow only produces confusing-but-safe behavior. Neither represents a realistic production outage, but both violate the Iron Law that "every operation must terminate and every resource must be bounded."

Conditions: Consider adding an upper bound to `--limit` to prevent the `usize -> i64` cast issue and to provide a better user experience for clearly unreasonable values.
