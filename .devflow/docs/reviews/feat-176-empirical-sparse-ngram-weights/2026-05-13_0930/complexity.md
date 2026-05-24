# Complexity Review Report

**Branch**: feat-176-empirical-sparse-ngram-weights -> main
**Date**: 2026-05-13 09:30

## Issues in Your Changes (BLOCKING)

### HIGH

**`build_weights_rs` exceeds function length threshold (136 lines of string-building)** - `crates/rskim-research/src/codegen.rs:82`
**Confidence**: 90%
- Problem: `build_weights_rs` spans lines 82-218 (136 lines). The function is a single long sequence of `writeln!` calls building Rust source code -- the entire generated file structure (header, const array, lookup function, tests module) is a single monolithic function. While each individual `writeln!` is simple, the function's length makes it hard to verify completeness or modify one section without risk to others.
- Fix: Extract logical sections into helper functions:
```rust
fn write_header(buf: &mut Vec<u8>, table: &WeightTable) -> anyhow::Result<()> { ... }
fn write_weight_entries(buf: &mut Vec<u8>, weights: &[BigramWeight]) -> anyhow::Result<()> { ... }
fn write_lookup_fn(buf: &mut Vec<u8>) -> anyhow::Result<()> { ... }
fn write_tests(buf: &mut Vec<u8>) -> anyhow::Result<()> { ... }

fn build_weights_rs(table: &WeightTable) -> anyhow::Result<String> {
    let mut buf = Vec::with_capacity(64 * 1024);
    write_header(&mut buf, table)?;
    write_weight_entries(&mut buf, &table.weights)?;
    write_lookup_fn(&mut buf)?;
    write_tests(&mut buf)?;
    String::from_utf8(buf).context("building weights.rs source")
}
```

**`cmd_run` exceeds function length threshold (121 lines, 7 sequential responsibilities)** - `crates/rskim-research/src/main.rs:92`
**Confidence**: 85%
- Problem: `cmd_run` spans lines 92-213 (121 lines) and handles 7 distinct responsibilities in sequence: config loading, temp directory setup, repo cloning, bigram extraction, IDF computation, validation, and JSON output. While the linear flow is readable, modifications to any step require understanding the full function context, and the mixed concerns (I/O, computation, formatting) make targeted testing impossible.
- Fix: Extract at least the temp-dir setup and the output-writing steps into named helpers. The core pipeline (clone -> extract -> compute -> validate -> write) could remain, but temp-dir resolution and file writing deserve extraction:
```rust
fn resolve_corpus_dir(corpus_dir: Option<PathBuf>) -> anyhow::Result<(PathBuf, Option<tempfile::TempDir>)> { ... }
fn write_weight_table(table: &WeightTable, output: Option<PathBuf>) -> anyhow::Result<()> { ... }
```

**`is_border_bigram` has overly broad matching logic** - `crates/rskim-research/src/validate.rs:76`
**Confidence**: 82%
- Problem: The function combines 3 nesting levels (for-loop -> if/else-if chain -> inner if) with boolean conditions that are broader than the doc comment suggests. Specifically, line 87 `if window[0] == first2[0] || window[0] == last2[0]` matches any bigram whose first byte equals the first byte of any token's first or last 2 bytes. For common bytes like `(` or `f`, this effectively makes almost every bigram a "border bigram", undermining the selectivity benefit the border-weighting strategy is supposed to provide. This is a complexity problem because the nested conditions are hard to reason about and the comment says "overlap with first 2 bytes" but the code matches far more broadly.
- Fix: Remove the over-broad byte-level match at line 87. The slice-level comparison at line 83 (`window == first2 || window == last2`) already captures the semantically meaningful border positions:
```rust
fn is_border_bigram(window: &[u8], tokens: &[&[u8]]) -> bool {
    for token in tokens {
        if token.len() >= 2 {
            let first2 = &token[..2];
            let last2 = &token[token.len() - 2..];
            if window == first2 || window == last2 {
                return true;
            }
        } else if token.len() == 1 {
            if window.contains(&token[0]) {
                return true;
            }
        }
    }
    false
}
```

### MEDIUM

**`clone_repo` has 3 levels of nesting with imperative fallback logic** - `crates/rskim-research/src/clone.rs:59`
**Confidence**: 85%
- Problem: `clone_repo` (lines 59-117, 58 lines) has a nested structure: `if shallow_ok { if checkout_ok { if status.success() { ... } } }` reaches nesting depth 3. The fallback-from-shallow-to-full-clone logic interleaves I/O commands with error checking, making the control flow non-obvious. The function runs 4 external commands with different failure semantics.
- Fix: Extract `try_shallow_clone` as a separate function returning `Result<bool>` (true = success, false = fallback needed):
```rust
fn try_shallow_clone(url: &str, commit: &str, dest: &Path) -> anyhow::Result<bool> {
    // shallow clone + checkout attempt; returns Ok(true) on success, Ok(false) to fallback
}

fn clone_repo(url: &str, commit: &str, dest: &Path) -> anyhow::Result<()> {
    if !try_shallow_clone(url, commit, dest)? {
        full_clone(url, commit, dest)?;
    }
    Ok(())
}
```

**`walk_and_load` has 4 sequential filter conditions interleaved with I/O** - `crates/rskim-research/src/clone.rs:119`
**Confidence**: 80%
- Problem: Lines 119-190 (71 lines) contain a for-loop with 6 `continue` guards (file type check, excluded extension, non-target extension, file size, language detection, read failure) plus a binary detection step and a UTF-8 validation step. While each guard is individually simple, the sequential nature makes it easy to overlook ordering dependencies (e.g., binary detection after read but before UTF-8 conversion). The function mixes filtering concerns with I/O (reading file bytes).
- Fix: Consider extracting a `should_include` predicate for the metadata-only filters (type, extension, size) and a `load_source_file` helper for the content-based filters (binary detection, UTF-8, language):
```rust
fn should_include(entry: &ignore::DirEntry) -> bool { ... }
fn load_source_file(path: &Path) -> Option<SourceFile> { ... }
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`gen_synthetic.rs` main function is 220 lines with hardcoded data** - `crates/rskim-research/src/bin/gen_synthetic.rs:9`
**Confidence**: 82%
- Problem: The `main()` function spans the entire 230-line file. It contains ~100 lines of hardcoded string literals (code samples at lines 38-140), inline loop logic for generating ASCII pairs (lines 161-178), and output writing. The hardcoded code samples are a maintenance burden -- when languages are added, this list must be manually updated.
- Fix: Extract `code_samples` into a named constant or a separate function, and extract the "add all printable ASCII pairs" and "add special byte bigrams" logic into helper functions. Even as a developer-only tool, the single-function structure makes the generation pipeline opaque:
```rust
const CODE_SAMPLES: &[&str] = &[ ... ];

fn add_ascii_pair_coverage(df: &mut HashMap<u16, u32>) { ... }
fn add_special_byte_coverage(df: &mut HashMap<u16, u32>) { ... }
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`border_weighted_selectivity` recomputes token list per call** - `crates/rskim-research/src/validate.rs:45` (Confidence: 65%) -- In `run_validation`, `tokenize(query)` is called inside `border_weighted_selectivity` for each query, while `uniform_selectivity` does its own byte iteration. If this code path were hot, pre-tokenizing would reduce redundant work. Developer-only tool, so acceptable.

- **`covering_set_heuristic` uses `candidates.sort_by` with `partial_cmp` fallback** - `crates/rskim-research/src/validate.rs:161` (Confidence: 70%) -- The `partial_cmp(...).unwrap_or(Ordering::Equal)` pattern silently treats NaN as equal. Since IDF values are validated to be positive in codegen, NaN should be impossible here, but a `debug_assert!(!idf.is_nan())` would make the invariant explicit.

- **9,659-line generated `weights.rs` committed to source** - `crates/rskim-search/src/weights.rs` (Confidence: 60%) -- The generated file is large but machine-generated and clearly marked "DO NOT EDIT MANUALLY". Committing it is a valid choice (deterministic builds without running the generator). The 38K-line JSON source file similarly. Not a complexity issue per se, but worth noting the repo size impact.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 3 | 0 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Complexity Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The codebase demonstrates good separation of concerns at the module level (types, config, clone, extract, idf, codegen, validate are well-factored). Individual functions are mostly straightforward linear pipelines. The main complexity issues are: (1) two functions exceeding the 50-line threshold (`build_weights_rs` at 136 lines, `cmd_run` at 121 lines), (2) `is_border_bigram` has logic that is broader than its doc comment describes, which conflates a correctness concern with a readability concern, and (3) `clone_repo` has nested fallback logic that would benefit from extraction. For a developer-only research tool, these are reasonable to address before merge but none represent critical risk.
