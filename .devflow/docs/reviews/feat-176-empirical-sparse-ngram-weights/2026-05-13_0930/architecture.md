# Architecture Review Report

**Branch**: feat-176-empirical-sparse-ngram-weights -> main
**Date**: 2026-05-13
**PR**: #220

## Issues in Your Changes (BLOCKING)

### HIGH

**`is_border_bigram` compares window bytes against token bytes without positional correlation** - `crates/rskim-research/src/validate.rs:76-98`
**Confidence**: 85%
- Problem: The function compares the raw byte values of `window` against token prefix/suffix bytes, but `window` is a sliding window over the full query string (including spaces between tokens). A bigram like `(space, 'p')` from `"fn parse"` will match `window[0] == last2[0]` for any token whose last byte happens to be a space. The check `window[0] == first2[0] || window[0] == last2[0]` on line 87-88 is overly broad -- it classifies a bigram as a border bigram if its first byte matches the first byte of ANY token's prefix or suffix, regardless of whether the bigram actually occurs at that token's boundary. This means most bigrams in a multi-token query will be classified as border bigrams, reducing the discriminating power of the border-weighted strategy.
- Impact: The border-weighted selectivity metric becomes inflated and less meaningful as a quality signal. Since this is a research/validation tool (not a runtime component in rskim-search), the impact is limited to potentially misleading validation reports rather than production correctness.
- Fix: Compare positionally -- track the byte offset of each bigram window within the query, then check whether that offset falls within 2 bytes of a token boundary:
```rust
fn is_border_bigram(window_offset: usize, query: &str, tokens: &[&[u8]]) -> bool {
    let bytes = query.as_bytes();
    let mut pos = 0;
    for token in tokens {
        // Skip whitespace
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        let start = pos;
        let end = start + token.len();
        // Border = within first 2 or last 2 bytes of token
        let in_prefix = window_offset >= start && window_offset < start + 2;
        let in_suffix = end >= 2 && window_offset + 1 >= end - 2 && window_offset < end;
        if in_prefix || in_suffix {
            return true;
        }
        pos = end;
    }
    false
}
```

**`chrono_now()` uses `unwrap_or(0)` -- silent fallback hides system clock failures** - `crates/rskim-research/src/main.rs:278-286`
**Confidence**: 82%
- Problem: The function returns `"unix:0"` if `SystemTime::now().duration_since(UNIX_EPOCH)` fails. This silently produces a timestamp of epoch zero, which would be indistinguishable from the `"unix:0"` used in test fixtures (see `codegen.rs` sample_table). A developer reviewing `bigram_weights.json` would not know the timestamp is bogus.
- Impact: Misleading provenance metadata in generated artifacts. Low practical risk since the system clock failure is rare, but the pattern violates the project's "fail loud" philosophy.
- Fix: Return `Result` and propagate the error, or use `anyhow::Context`:
```rust
fn chrono_now() -> anyhow::Result<String> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before UNIX epoch")?
        .as_secs();
    Ok(format!("unix:{secs}"))
}
```

### MEDIUM

**Large generated JSON artifact (565 KB) checked into version control** - `crates/rskim-search/data/bigram_weights.json`
**Confidence**: 85%
- Problem: `bigram_weights.json` (38,410 lines, 565 KB) is a generated intermediate artifact checked into the repo. The canonical output is `weights.rs` (9,659 lines), which is the actual compile-time artifact consumed by rskim-search. The JSON serves as an intermediate step between research analysis and codegen, but it could be regenerated from the research pipeline at any time. Checking in both the JSON source and the generated `.rs` file creates two representations of the same data, either of which could drift from the other.
- Impact: Repository bloat and potential for inconsistency between `bigram_weights.json` and `weights.rs`. Each update to the weights requires updating both files in lockstep.
- Fix: Consider whether the JSON file should be checked in or treated as a build artifact. If kept, add a CI check or Makefile target that verifies `weights.rs` matches what `rskim-research codegen` would produce from the checked-in JSON. Alternatively, add `bigram_weights.json` to `.gitignore` and only check in the generated `weights.rs`.

**`extract_bigrams_from_corpus` is single-threaded despite the crate using rayon elsewhere** - `crates/rskim-research/src/extract.rs:65-115`
**Confidence**: 80%
- Problem: The `extract_bigrams_from_corpus` function iterates files sequentially with a mutable `HashMap`, while the calling code in `main.rs:136-151` uses `par_iter()` for cloning repos. The extraction loop accumulates into shared mutable state (`df_map`, `seen_hashes`) which prevents easy parallelization. For a research tool processing thousands of files, this is a throughput bottleneck.
- Impact: Performance limitation in the research pipeline. Not a correctness issue, but architecturally, the single-threaded accumulation pattern is inconsistent with the crate's otherwise parallel approach.
- Fix: This is a should-consider-for-later optimization. A two-pass approach (parallel per-file bigram extraction into thread-local sets, then sequential merge) would align with the parallel architecture used elsewhere. No blocking issue.

**`gen_synthetic.rs` hardcodes language breakdown rather than computing it** - `crates/rskim-research/src/bin/gen_synthetic.rs:188-201`
**Confidence**: 82%
- Problem: The `language_breakdown` vector is manually constructed with hardcoded counts (`Rust: 2, TypeScript: 1, Python: 1`) instead of being computed from the actual fixture files loaded. If fixture files are added or removed, this metadata will silently become stale. The real `cmd_run` path in `main.rs` derives this from the data, creating an inconsistency in how synthetic vs. real tables report their provenance.
- Impact: Misleading metadata in synthetic weight tables. Low production risk since synthetic tables are a bootstrap mechanism, but the inconsistency between the two code paths is a maintenance hazard.
- Fix: Compute the breakdown from `fixture_files`:
```rust
let mut lang_map: HashMap<String, u32> = HashMap::new();
for f in &fixture_files {
    *lang_map.entry(format!("{:?}", f.language)).or_default() += 1;
}
let language_breakdown: Vec<LanguageCount> = lang_map.into_iter()
    .map(|(language, file_count)| LanguageCount { language, file_count })
    .collect();
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`idf::selectivity` function is misnamed -- it computes total IDF score, not selectivity** - `crates/rskim-research/src/idf.rs:47-57`
**Confidence**: 80%
- Problem: The function `selectivity` sums IDF weights for all bigrams in a query string. "Selectivity" in information retrieval typically means the fraction of documents matching a predicate (lower is more selective). This function returns a raw score sum, which is the opposite semantic -- higher values indicate more distinctive queries. The name is used in both `idf.rs` and `validate.rs`, creating a naming inconsistency with the concept it claims to represent.
- Impact: Confusing API for future contributors. The `ValidationResult` fields are also named `uniform_selectivity` and `border_weighted_selectivity`, propagating the misnomer throughout the research crate.
- Fix: Rename to `total_idf_score` or `discriminativeness_score` to match the actual semantics. This is a research-only crate (`publish = false`), so the rename is safe.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`validate.rs` BORDER_MULTIPLIER is a hardcoded magic constant** - `crates/rskim-research/src/validate.rs:20` (Confidence: 65%) -- The 3.5 multiplier is not documented with any empirical justification or reference to literature. Consider making it configurable via CLI arg or corpus.toml, or at minimum add a doc comment explaining how 3.5 was chosen.

- **`clone_repo` shells out to `git` without timeout bounds** - `crates/rskim-research/src/clone.rs:59-117` (Confidence: 70%) -- The function spawns `git clone` subprocesses without a timeout, which could hang indefinitely on network issues. The CLAUDE.md reliability principle states "every loop, retry, and resource has an explicit bound." Consider adding a timeout via `std::process::Command` with a spawned child and `wait_timeout`.

- **`config.rs` language validation uses strings instead of the `Language` enum** - `crates/rskim-research/src/config.rs:25` (Confidence: 72%) -- The VALID_LANGUAGES constant is `&[&str]` while the crate already depends on `rskim_core::Language`. Parsing the language string into the enum at config-load time would catch mismatches at the boundary rather than relying on string comparison.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

### Overall Assessment

The architecture of this PR is fundamentally sound. The new `rskim-research` crate is correctly isolated as an unpublished developer tool with a clean separation from the production `rskim-search` crate. Key architectural strengths:

- **Proper crate boundary**: `rskim-research` depends on `rskim-core` but not on `rskim-search`, keeping the dependency graph acyclic and unidirectional. The data flows research -> JSON -> codegen -> weights.rs -> rskim-search, which is a clean pipeline.
- **FileSource trait**: The `FileSource` trait with `GitCloneSource` and `FixtureSource` implementations follows DIP correctly, enabling testing without network access.
- **Module decomposition**: Each module (config, clone, extract, idf, codegen, validate) has a single clear responsibility. The six modules map neatly to the pipeline stages.
- **Generated code pattern**: Using codegen to produce a `const` lookup table with binary search is an efficient zero-allocation pattern for the weights, fitting the project's performance requirements.

The main architectural concerns are the `is_border_bigram` logic correctness (which affects validation quality) and the silent error handling in `chrono_now` (which contradicts the project's fail-loud philosophy). The checked-in JSON artifact is worth discussing but not blocking.
