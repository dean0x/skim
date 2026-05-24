# Reliability Review Report

**Branch**: feat-176-empirical-sparse-ngram-weights -> main
**Date**: 2026-05-13T09:30
**PR**: #220

## Issues in Your Changes (BLOCKING)

### CRITICAL

**Unbounded external process execution in `clone_repo`** - `crates/rskim-research/src/clone.rs:65-113`
**Confidence**: 95%
- Problem: Four calls to `std::process::Command::new("git")` spawn child processes with no timeout. If a git clone hangs (network stall, DNS resolution hang, unresponsive server), the process blocks indefinitely. This is a textbook violation of the bounded iteration principle: every operation on external I/O must terminate after a known maximum time.
- Impact: Running `rskim-research run` against the 25-repo corpus.toml could hang forever on a single repo, with no recovery path. Since repos are processed via `par_iter`, a single hung clone blocks a rayon worker thread permanently, eventually starving the thread pool.
- Fix: Use `std::process::Command` with a timeout wrapper. Rust's stdlib does not have a built-in timeout for `Command::status()`, but you can use `Command::spawn()` + `Child::wait_timeout()` (unstable) or a simple pattern with `spawn()` + a thread/channel:
```rust
const CLONE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

fn run_git_with_timeout(args: &[&str], dest: Option<&Path>) -> anyhow::Result<bool> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(args);
    if let Some(d) = dest {
        cmd.arg(d);
    }
    let mut child = cmd.spawn().context("spawning git")?;
    match child.wait_timeout(CLONE_TIMEOUT) {
        Ok(Some(status)) => Ok(status.success()),
        Ok(None) => {
            let _ = child.kill();
            anyhow::bail!("git command timed out after {}s", CLONE_TIMEOUT.as_secs());
        }
        Err(e) => Err(e.into()),
    }
}
```
Note: `wait_timeout` is unstable. A stable alternative is to spawn a monitoring thread that kills the child after the deadline.

### HIGH

**No precondition assertion on `compute_idf` for `total_docs == 0`** - `crates/rskim-research/src/idf.rs:12-14`
**Confidence**: 90%
- Problem: `compute_idf(df, 0)` computes `(0.0 / (df+1) as f64).ln()` which is `(-inf).ln()` -- actually `ln(0)` when df > 0, producing `-inf`, and `ln(0/1) = -inf` for df = 0. The result is `-inf + 1.0 = -inf` cast to `f32::NEG_INFINITY`. This silent production of `f32::NEG_INFINITY` could propagate through the weight table and corrupt downstream binary search lookups or selectivity computations.
- Impact: If `extract_bigrams_from_corpus` is called with zero files (e.g., all repos fail to clone), `total_docs = 0` is passed to `compute_weight_table`. The `threshold` filter might catch negative-infinity values, but only if `threshold >= f32::NEG_INFINITY`, which is always true -- so the `-inf` entries would be filtered. However, this relies on implicit float comparison behavior rather than an explicit guard. A precondition assertion makes the invariant explicit and catches the root cause immediately.
- Fix:
```rust
pub fn compute_idf(df: u32, total_docs: u32) -> f32 {
    assert!(total_docs > 0, "compute_idf requires total_docs > 0");
    ((total_docs as f64) / ((df + 1) as f64)).ln() as f32 + 1.0
}
```
Additionally, add a guard in `compute_weight_table`:
```rust
if total_docs == 0 {
    return vec![];
}
```

**No NaN/infinity validation on generated IDF values in `codegen.rs`** - `crates/rskim-research/src/codegen.rs:57-65`
**Confidence**: 85%
- Problem: The codegen validation loop at line 57-65 checks `w.idf <= 0.0` but `f32::NAN <= 0.0` evaluates to `false` in IEEE 754 (NaN comparisons always return false). A NaN IDF value would silently pass validation and be written into the generated `weights.rs` as a NaN literal, producing a broken weight table.
- Impact: Any NaN in the const table would cause all binary search lookups that return that entry to propagate NaN through downstream arithmetic, silently corrupting search ranking.
- Fix:
```rust
for w in &table.weights {
    if !w.idf.is_finite() || w.idf <= 0.0 {
        anyhow::bail!(
            "invalid IDF {} for bigram 0x{:04X} — must be finite and positive",
            w.idf,
            w.bigram
        );
    }
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`is_border_bigram` has an overly broad match condition** - `crates/rskim-research/src/validate.rs:76-98`
**Confidence**: 82%
- Problem: Line 87-89 checks `if window[0] == first2[0] || window[0] == last2[0]` which matches when the first byte of the bigram equals the first or last byte of ANY token. For code queries like `"fn parse"`, nearly every bigram will match this condition because common bytes like `f`, `n`, `p`, `a`, etc. appear as first/last bytes of the tokens. This effectively makes `BORDER_MULTIPLIER` apply to almost every bigram, reducing the border-weighted strategy to a nearly-uniform multiplied score rather than a targeted positional bonus.
- Impact: The validation report's "improvement" metric is inflated because border weighting is applied too broadly, making the selectivity comparison between uniform and border-weighted strategies unreliable as a quality signal.
- Fix: The border check should use byte-position awareness relative to the original query string, not just byte-value matching:
```rust
fn is_border_bigram(bigram_start_pos: usize, query: &str, tokens: &[&[u8]]) -> bool {
    let bytes = query.as_bytes();
    // Find which token this position falls in
    let mut offset = 0;
    for token in tokens {
        // Skip whitespace
        while offset < bytes.len() && bytes[offset] == b' ' {
            offset += 1;
        }
        let token_start = offset;
        let token_end = offset + token.len();
        if bigram_start_pos >= token_start && bigram_start_pos + 1 < token_end {
            // Bigram is within this token — check if it overlaps first/last 2 bytes
            let pos_in_token = bigram_start_pos - token_start;
            return pos_in_token <= 1 || pos_in_token >= token.len().saturating_sub(2);
        }
        offset = token_end;
    }
    false
}
```

**`_temp_dir_guard` uninitialized on `Some` branch risks use-before-init confusion** - `crates/rskim-research/src/main.rs:104-112`
**Confidence**: 80%
- Problem: The `_temp_dir_guard` variable is declared at line 104 but only assigned in the `None` branch. While Rust's ownership rules make this safe (the variable is simply never used on the `Some` path), the pattern is fragile: adding code after the match block that references `_temp_dir_guard` would cause a compile error that is non-obvious. The intent -- keeping the TempDir alive for the duration of the function -- is not self-documenting.
- Impact: Maintenance risk. A future contributor adding cleanup logic could be confused by the guard's conditional initialization.
- Fix: Use `Option<TempDir>` to make the conditional lifetime explicit:
```rust
let (corpus_dir, _temp_guard) = match corpus_dir {
    Some(p) => (p, None),
    None => {
        let td = tempfile::tempdir().context("creating temporary corpus directory")?;
        let path = td.path().to_path_buf();
        (path, Some(td))
    }
};
```

## Pre-existing Issues (Not Blocking)

No pre-existing reliability issues identified in unchanged code at CRITICAL level.

## Suggestions (Lower Confidence)

- **`walk_and_load` silently ignores file read errors** - `crates/rskim-research/src/clone.rs:165-168` (Confidence: 70%) -- `std::fs::read(path)` errors are silently swallowed with `Err(_) => continue`. While this is acceptable for a corpus-loading best-effort scenario, a debug-level log or counter of skipped files would aid diagnosis when the corpus yields fewer files than expected.

- **Large checked-in JSON file (38,409 lines)** - `crates/rskim-search/data/bigram_weights.json` (Confidence: 65%) -- The 38K-line JSON file and the 9,659-line generated `weights.rs` are both checked into version control. The JSON is the source-of-truth for codegen, which is reasonable, but git diffs on regeneration will be extremely noisy. Consider whether `.gitattributes` with `bigram_weights.json linguist-generated=true` and `weights.rs linguist-generated=true` would improve PR readability.

- **`covering_set_heuristic` uses `unwrap_or` on float comparison** - `crates/rskim-research/src/validate.rs:161` (Confidence: 62%) -- `partial_cmp(...).unwrap_or(std::cmp::Ordering::Equal)` silently treats NaN as equal. If NaN IDF values sneak in (see the NaN guard finding above), NaN entries sort unpredictably, potentially corrupting the greedy covering set selection.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 1 | 2 | - | - |
| Should Fix | - | - | 2 | - |
| Pre-existing | - | - | - | - |

**Reliability Score**: 6/10
**Recommendation**: CHANGES_REQUESTED

The unbounded git process execution is a CRITICAL reliability violation -- every external I/O operation must have a finite timeout. The missing NaN/infinity guards on IDF computation and codegen validation are HIGH severity because they create a silent corruption path through the weight table pipeline. The core data structures and algorithms (bigram encoding, IDF math, binary search, deduplication) are sound and well-tested. The bounded directory traversal in `find_workspace_root` and the u16 key-space natural bound on `df_map` are positive reliability patterns.
