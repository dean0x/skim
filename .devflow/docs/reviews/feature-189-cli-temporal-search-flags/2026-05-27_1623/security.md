# Security Review Report

**Branch**: feature/189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T16:23
**Cycle**: 3 (incremental after cycle 2: 19 fixed, 4 FP)

## Cross-Cycle Awareness

Prior cycle 2 marked "path traversal via symlink" as FP (standard defense via canonicalize + strip_prefix). Confirmed: the defense is still in place and correctly applied. Not re-raised.

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Empty `--blast-radius=` value not rejected** - `crates/rskim/src/cmd/search/mod.rs:175-177`
**Confidence**: 85%
- Problem: The `--blast-radius=` (equals form) parsing uses `trim_start_matches("--blast-radius=")` which produces an empty string when the user passes `--blast-radius=` with no value. This empty string flows into `normalize_blast_radius_path("")` which calls `Path::new("")` -- on most platforms, `"".exists()` returns `false` and the function bails with "blast-radius file not found:", but on some OS configurations an empty path can resolve to the current directory. More importantly, the empty-string path produces a confusing error message: `blast-radius file not found: ` (no visible path).
- Fix: Add an empty-string guard in `parse_temporal_flag` for the equals form:
  ```rust
  s if s.starts_with("--blast-radius=") => {
      let val = s.trim_start_matches("--blast-radius=");
      if val.is_empty() {
          anyhow::bail!("--blast-radius requires a file path");
      }
      *blast_radius = Some(val.to_string());
      Ok(false)
  }
  ```

**No timeout on `git rev-parse HEAD` subprocess** - `crates/rskim/src/cmd/search/temporal.rs:152-165`
**Confidence**: 80%
- Problem: `read_git_head` spawns `git rev-parse HEAD` via `Command::new("git")` with `.output()` which blocks indefinitely. The doc comment acknowledges this: "It is NOT safe to use on network-mounted repos or corrupted `.git` directories where the subprocess may hang indefinitely." However, no timeout is applied. If the user runs `skim search --hot --root /mnt/nfs-share`, the CLI can hang forever. This is a denial-of-service against the user's own terminal session.
- Fix: Use a bounded wait or spawn + timeout. A pragmatic approach for a staleness check that is advisory-only:
  ```rust
  fn read_git_head(root: &Path) -> Option<String> {
      let child = std::process::Command::new("git")
          .arg("-C")
          .arg(root)
          .arg("rev-parse")
          .arg("HEAD")
          .stdout(std::process::Stdio::piped())
          .stderr(std::process::Stdio::null())
          .spawn()
          .ok()?;
      let output = child.wait_with_output().ok()?;
      // Or use a 5-second timeout with a thread + kill pattern.
      if output.status.success() {
          Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
      } else {
          None
      }
  }
  ```
  Note: Rust stable does not yet have `wait_timeout` on `Child`, but since this is an advisory staleness check, wrapping in a thread with a join timeout (or simply documenting the risk and accepting it for local-only use) is reasonable. The current approach is acceptable for a local CLI tool but the doc comment should be promoted to a code-level guard or an explicit design decision.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`cochanges_for_file` hard-coded LIMIT 10000 may be excessive** - `crates/rskim-search/src/temporal/storage_ops.rs:166` (Confidence: 65%) -- The LIMIT 10000 on the cochange query is generous for a per-file lookup. In pathological cases (highly coupled monorepo), this could return a large allocation. The downstream `resort_window` clamp mitigates this, but the DB query itself could be tighter.

- **Staleness warning reveals internal git HEAD SHAs to stdout** - `crates/rskim/src/cmd/search/temporal.rs:134-139` (Confidence: 60%) -- The staleness warning prints abbreviated SHA hashes of the stored vs current HEAD to stderr. In a multi-tenant CI environment, this could leak commit context. Low risk since it is stderr-only and the tool is a local CLI.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Security Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

## Security Strengths Observed

1. **Parameterized SQL everywhere**: All SQLite queries in `storage_ops.rs` use `rusqlite::params![]` with positional bindings (`?1`). No string interpolation into SQL.
2. **Path traversal defense**: `normalize_blast_radius_path` canonicalizes both the input and the project root, then uses `strip_prefix` to enforce confinement. Paths outside the repo root are rejected with a clear error.
3. **No shell invocation**: `read_git_head` uses `Command::new("git").arg(...)` (argument array), not `sh -c`, preventing command injection.
4. **Database file permissions**: `TemporalDb::open` sets `0o600` (owner-only) on Unix, preventing other users from reading/writing the temporal database.
5. **Capacity limits**: `MAX_ROWS_PER_TABLE` (500,000) prevents unbounded memory allocation from malicious or corrupted database content. The `top_*` methods clamp the `limit` parameter before binding to SQLite.
6. **Input validation**: `--limit` rejects 0 and non-numeric values. Temporal sort flags enforce mutual exclusivity with clear error messages. `--blast-radius` requires a value.
7. **Graceful degradation**: Missing temporal DB returns exit 0 with a warning rather than failing, preventing information leakage through error codes.
