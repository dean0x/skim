---
focus: reliability
reviewer: Reviewer
timestamp: 2026-05-21T14:03:35Z
pr: 247
branch: feat/195-bm25f-bench
head: 101385eb
---

# Reliability Review

## Summary

The rskim-bench crate is well-structured with bounded loops, proper Result propagation, and clippy lints that deny unwrap/expect in production code. The primary reliability concerns are: (1) `result_to_config` silently zeroes out fields when deserialized Vec lengths are short, (2) `file_id_counter` in the tune subcommand can overflow u32 without detection on very large corpora, (3) the `evaluate` closure in `run_tune` silently swallows errors converting them to 0.0 MRR which masks index corruption, and (4) search errors in `evaluate_split` are silently swallowed via `unwrap_or_default`.

## Findings

### blocking -- `result_to_config` silently produces wrong config when Vec lengths are < 8
- **File:** `crates/rskim-bench/src/tuning.rs:162-182`
- **Confidence:** 92%
- **Description:** `result_to_config` copies `best_field_boosts` and `best_field_b` into fixed `[f32; 8]` arrays using `.iter().take(8)`. If the Vecs have fewer than 8 elements (e.g., from a truncated or corrupted JSON file loaded via the `report` subcommand, or a future code change to tuning), the remaining array slots stay at `0.0`. The function then calls `validate()`, but a boost of 0.0 is valid per BM25F rules, so validation passes silently. The result is a config that zeroes out fields the caller never intended to zero, producing misleading benchmark numbers with no error or warning. This is especially dangerous because `TuningResult` uses `Vec<f32>` (not `[f32; 8]`), so there is no compile-time guarantee the Vec has exactly 8 elements.
- **Suggestion:** Assert the Vec lengths are exactly 8 before copying, or return an error:
```rust
pub fn result_to_config(result: &TuningResult) -> anyhow::Result<BM25FConfig> {
    use anyhow::{Context, bail};

    if result.best_field_boosts.len() != 8 || result.best_field_b.len() != 8 {
        bail!(
            "TuningResult has wrong field count: boosts={}, b={} (expected 8 each)",
            result.best_field_boosts.len(),
            result.best_field_b.len()
        );
    }

    let cfg = BM25FConfig {
        k1: result.best_k1,
        field_boosts: result.best_field_boosts[..8].try_into()
            .context("converting boosts to [f32; 8]")?,
        field_b: result.best_field_b[..8].try_into()
            .context("converting b to [f32; 8]")?,
    };
    cfg.validate()
        .context("tuning result produced invalid config")?;
    Ok(cfg)
}
```

### blocking -- Search errors silently swallowed in `evaluate_split`
- **File:** `crates/rskim-bench/src/harness.rs:144`
- **Confidence:** 85%
- **Description:** `layer.search(&query).unwrap_or_default()` converts any search error (index corruption, I/O failure, deserialization error) into an empty result set. This means a broken index produces 0.0 MRR for every query, which looks identical to "no results found" rather than "the index is broken." For a benchmarking harness whose entire purpose is measuring search quality, silently masking search failures produces misleading metrics. The caller has no way to distinguish "config X is bad" from "the index is corrupt."
- **Suggestion:** Propagate the error or at minimum count and report error rates:
```rust
let results = layer.search(&query)
    .with_context(|| format!("searching for query '{}'", qrel.query))?;
```

### should-fix -- `file_id_counter` overflow unchecked in `run_tune`
- **File:** `crates/rskim-bench/src/main.rs:226-240`
- **Confidence:** 82%
- **Description:** `file_id_counter` is a `u32` that is incremented once per file across all repos. If the corpus contains more than `u32::MAX` (~4.3 billion) files, this wraps silently. While practically unlikely today, the counter also lacks an overflow assertion which would catch misconfiguration or bugs in file enumeration. More importantly, the same `i as u32` cast in `run_bench` (line 183) and `run_qrels` (line 399) would panic on debug builds but silently truncate on release builds if a single repo has > 4B files.
- **Suggestion:** Use checked arithmetic or assert a bound:
```rust
let fid = FileId(
    file_id_counter.checked_add(1)
        .context("file_id_counter overflow: too many files")?
        // Or alternatively:
);
file_id_counter += 1;
```
For the `i as u32` casts, use `u32::try_from(i).context("too many files")?`.

### should-fix -- Tuning evaluate closure swallows all errors as 0.0
- **File:** `crates/rskim-bench/src/main.rs:291-306`
- **Confidence:** 83%
- **Description:** The closure passed to `coordinate_descent` returns `0.0` for any `NgramIndexReader::open_with_config` failure and uses `unwrap_or_else` to mask `evaluate_split` errors. During coordinate descent, every candidate config opens a new reader (up to `6 * 3 + 8 * 9 * 3 + 2 * 5 * 3 = 276` evaluations per pass). If the index directory is deleted, corrupted, or the filesystem runs out of inodes, every evaluation silently returns 0.0, the tuner sees no improvement, and exits claiming the default config is optimal. The user gets a plausible-looking but completely wrong result with no indication of failure.
- **Suggestion:** At minimum, add an error counter or log the first N errors to stderr so the user sees that evaluations are failing:
```rust
let error_count = std::sync::atomic::AtomicUsize::new(0);
let tuning_result = coordinate_descent(None, move |cfg| {
    let reader = match rskim_search::NgramIndexReader::open_with_config(&idx_path, cfg) {
        Ok(r) => r,
        Err(e) => {
            if error_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 3 {
                eprintln!("warning: index open failed during tuning: {e}");
            }
            return 0.0;
        }
    };
    // ...
});
```

### should-fix -- NaN propagation risk in MRR/precision calculations
- **File:** `crates/rskim-bench/src/metrics.rs:47-51`, `crates/rskim-bench/src/harness.rs:155-158`
- **Confidence:** 80%
- **Description:** `mrr()` divides by `rrs.len()` and `evaluate_split` divides sums by `n = qrels.len() as f64`. If by any code path `n` is 0.0 (which is guarded against for `mrr` but not for the precision divisions in `evaluate_split` -- there is a guard on `qrels.is_empty()` at line 124, so the current code is safe). However, if a future `evaluate` call introduces NaN values (e.g., from `0.0 / 0.0` in a metric), NaN would propagate silently through all aggregation in `macro_average` since NaN comparisons and sums behave unexpectedly. There are no `is_finite()` assertions anywhere in the pipeline.
- **Suggestion:** Add a debug assertion after computing metrics:
```rust
debug_assert!(mrr_val.is_finite(), "MRR must be finite, got {mrr_val}");
debug_assert!(p_at_5.is_finite(), "P@5 must be finite");
debug_assert!(p_at_10.is_finite(), "P@10 must be finite");
```

### informational -- Recursive AST walk functions have implicit depth bound
- **File:** `crates/rskim-bench/src/extract/rust_lang.rs:39-110`, `crates/rskim-bench/src/extract/go.rs:37-86`, `crates/rskim-bench/src/extract/python.rs:36-86`
- **Confidence:** 65%
- **Description:** All three extractors use recursive `walk_node` functions that recurse to the depth of the AST tree. Tree-sitter ASTs are bounded by the source file size and grammar structure, so in practice AST depth rarely exceeds ~50-100 levels even for pathological inputs. Additionally, `find_last_identifier` in `rust_lang.rs:118-147` recurses into children of `use_declaration` nodes, which are typically very shallow (< 10 levels). While the recursion is implicitly bounded by AST structure, there is no explicit depth limit, which means a crafted input with deeply nested syntax could in theory cause a stack overflow. However, tree-sitter grammars naturally limit nesting depth, making this a theoretical rather than practical concern.

### informational -- `TuningResult` uses `Vec<f32>` instead of `[f32; 8]` for field arrays
- **File:** `crates/rskim-bench/src/types.rs:97-98`
- **Confidence:** 88%
- **Description:** `best_field_boosts: Vec<f32>` and `best_field_b: Vec<f32>` are dynamically sized, but the BM25F config always has exactly 8 fields. Using `Vec<f32>` means the type system does not enforce the invariant that these arrays have exactly 8 elements. This is the root cause of the `result_to_config` issue above, and also means deserialized JSON could have any number of elements without a schema error.
- **Suggestion:** Change to `[f32; 8]` to make the invariant compile-time enforced:
```rust
pub struct TuningResult {
    pub best_k1: f32,
    pub best_field_boosts: [f32; 8],
    pub best_field_b: [f32; 8],
    // ...
}
```
This would make `result_to_config` trivially safe and eliminate the need for length checks.

## Suggestions (Lower Confidence)

- **Recursive AST walk depth** - `crates/rskim-bench/src/extract/rust_lang.rs:39` (Confidence: 65%) -- The recursive `walk_node` functions across all three extractors lack an explicit depth bound; tree-sitter AST structure provides an implicit bound but an explicit `max_depth` parameter would be more defensive.

- **`i as u32` truncation in `run_bench` and `run_qrels`** - `crates/rskim-bench/src/main.rs:183,399` (Confidence: 70%) -- The `i as u32` casts from `enumerate()` index to `FileId` silently truncate on release builds if a repo has more than `u32::MAX` files; use `u32::try_from(i)?` for safety.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | - | 2 | - | - |
| Should Fix | - | 3 | - | - |
| Pre-existing | - | - | - | - |

**Reliability Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The crate demonstrates strong reliability fundamentals: all loops are bounded (coordinate descent capped at MAX_PASSES=3 with finite candidate arrays), clippy lints deny unwrap/expect in non-test code, and error propagation uses Result/anyhow throughout the main paths. The two blocking issues are: (1) `result_to_config` silently producing wrong configs from short Vecs, and (2) search errors being swallowed in `evaluate_split` which can produce misleading benchmark numbers. The should-fix items around error visibility in the tuning closure and overflow checking are important for a tool whose purpose is producing trustworthy metrics.
