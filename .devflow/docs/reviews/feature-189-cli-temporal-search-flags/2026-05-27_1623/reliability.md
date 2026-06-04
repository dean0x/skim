# Reliability Review Report

**Branch**: feature/189-cli-temporal-search-flags -> main
**Date**: 2026-05-27

## Issues in Your Changes (BLOCKING)

### HIGH

**Unbounded subprocess in `read_git_head` — no timeout on `git rev-parse HEAD`** - `crates/rskim/src/cmd/search/temporal.rs:152-164`
**Confidence**: 85%
- Problem: `read_git_head()` spawns `git rev-parse HEAD` via `Command::new("git").output()` with no timeout. The doc comment on line 150-151 explicitly acknowledges this risk: "It is NOT safe to use on network-mounted repos or corrupted `.git` directories where the subprocess may hang indefinitely." Despite this self-documented risk, no timeout is applied. If the `.git` directory is on a network mount, is corrupted (e.g. `HEAD` points to a broken ref), or if `git` enters a credential prompt, the process blocks the entire CLI indefinitely with no upper bound.
- Fix: Use `Command::new("git")` with `.stdout(Stdio::piped())` and spawn + `wait_timeout` (via the `wait-timeout` crate), or use `std::thread::spawn` with a channel + `recv_timeout`. A 5-second timeout is generous for a local `rev-parse HEAD`:

```rust
use std::time::Duration;

fn read_git_head(root: &Path) -> Option<String> {
    let mut child = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    // Bound the subprocess to 5 seconds to prevent indefinite hangs
    // on network-mounted repos or corrupted .git directories.
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let result = child.wait_with_output();
        let _ = tx.send(result);
    });
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(output)) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
        }
        _ => {
            drop(handle); // best-effort cleanup
            None
        }
    }
}
```

Alternatively, if adding a dependency is acceptable, the `wait-timeout` crate provides `child.wait_timeout(Duration)` directly.

### MEDIUM

**No assertion that `blast_radius_paths` FileId set is non-empty before injecting into query** - `crates/rskim/src/cmd/search/query.rs:75-83`
**Confidence**: 82%
- Problem: When `config.blast_radius_paths` is `Some(allowed_paths)`, the code builds a `file_ids` HashSet by scanning the manifest. If none of the allowed paths match entries in the manifest (e.g., all paths were normalized differently, or the temporal DB is stale relative to the index), the result is `sq.file_filter = Some(empty_set)`. This silently yields zero results with no warning to the user. The user sees "no results" with no indication that the blast-radius filter eliminated everything due to a path mismatch.
- Fix: Log a warning when the resulting `file_ids` set is empty after filtering:

```rust
if let Some(ref allowed_paths) = config.blast_radius_paths {
    let mut file_ids = std::collections::HashSet::new();
    for (idx, path) in sorted.iter().enumerate() {
        if allowed_paths.contains(*path) {
            file_ids.insert(rskim_search::FileId(idx as u32));
        }
    }
    if file_ids.is_empty() {
        eprintln!(
            "skim search: blast-radius filter matched 0 indexed files \
             (allowed {} paths, index has {} files)",
            allowed_paths.len(),
            sorted.len()
        );
    }
    sq.file_filter = Some(file_ids);
}
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Resort window heuristic `limit*5` could be insufficient for large co-change sets with skewed temporal scores** - `crates/rskim/src/cmd/search/temporal.rs:244` (Confidence: 65%) -- The `resort_window = (limit.saturating_mul(5)).max(100)` heuristic pre-truncates co-change partners before re-sorting by temporal score. If the highest-temporal-score partner is ranked beyond position `limit*5` in Jaccard order, it gets silently dropped. The heuristic is reasonable for typical workloads but could miss outliers in large, skewed datasets. Document the tradeoff in the doc comment.

- **`annotate_hotspots` / `annotate_risks` perform N sequential DB lookups** - `crates/rskim/src/cmd/search/temporal.rs:608-645` (Confidence: 60%) -- Each result triggers a separate SQLite query (`hotspot_for_file` / `risk_for_file`). For default limit=20, this is 20 queries, which is fine. But if a user passes `--limit 1000`, it becomes 1000 sequential queries. The per-file lookup design is documented and intentional (avoids bulk table load), but adding a comment noting the O(N) query count and the practical limit at which a bulk approach would be better would improve maintainability.

- **`normalize_blast_radius_path` falls back to `canonicalize()` after existence check, but `unwrap_or_else` swallows canonicalize errors** - `crates/rskim/src/cmd/search/temporal.rs:83` (Confidence: 62%) -- Line 83: `let canonical = abs.canonicalize().unwrap_or_else(|_| abs.clone())`. Since `abs` was already confirmed to exist on lines 55-79, `canonicalize()` should succeed. But if it fails (e.g. race condition: file deleted between existence check and canonicalize), the fallback silently uses the un-canonicalized path, which may then fail the `strip_prefix` check and produce a confusing "outside the project root" error. A debug log would aid troubleshooting.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Reliability Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The implementation demonstrates strong reliability patterns overall:
- All top-N queries clamp limits via `MAX_ROWS_PER_TABLE` (500,000) preventing integer overflow on `usize::MAX as i64` casts.
- All `load_*` methods use `LIMIT 500001` with post-query overflow detection.
- Co-change query uses `LIMIT 10000` bounding the result set.
- Schema migrations use explicit version checks with forward-compatibility error for unknown versions.
- Graceful degradation throughout: missing temporal DB returns `None`/`Ok(None)`, missing files produce clear error messages.
- `CapacityExceeded` error variant enforces bounded storage.
- Per-file DB lookups avoid unbounded bulk loads.

The single HIGH finding (unbounded subprocess) is the only reliability concern that could cause an indefinite hang in production. The MEDIUM finding is a UX issue that could cause user confusion but not a hang or crash. The PR author has explicitly documented the subprocess risk in comments, which suggests awareness — adding the timeout would close the gap between the documented risk and the implementation.
