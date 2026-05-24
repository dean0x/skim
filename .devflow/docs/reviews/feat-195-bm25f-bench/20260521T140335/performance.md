---
focus: performance
reviewer: Reviewer
timestamp: 2026-05-21T14:03:35Z
pr: 247
branch: feat/195-bm25f-bench
head: 101385eb
---

# Performance Review

## Summary

The rskim-bench crate is well-structured as a batch benchmarking tool with bounded iteration counts and sensible data structures. The main performance concerns are: (1) a new tree-sitter Parser is allocated per file in every extractor call, which dominates extraction cost at scale; (2) the tuning loop re-opens the on-disk index for every candidate configuration evaluation, creating significant I/O overhead during coordinate descent; (3) several avoidable string clones in the hot qrel-generation pipeline; and (4) repos are processed sequentially when they could be parallelized.

## Findings

### should-fix -- Tree-sitter Parser allocated per file in extractors
- **File:** `crates/rskim-bench/src/extract/rust_lang.rs:18-21`, `crates/rskim-bench/src/extract/python.rs:15-20`, `crates/rskim-bench/src/extract/go.rs:17-21`
- **Confidence:** 92%
- **Description:** Each call to `extract()` creates a new `tree_sitter::Parser`, sets its language, parses the content, then drops the parser. When processing a corpus with hundreds or thousands of files, this means hundreds of parser allocations and language-setting operations. Parser creation is not free -- it allocates internal state. Since the extractors are always called in a loop over files (in `generate_qrels`), the parser could be created once and reused.
- **Suggestion:** Accept a `&mut Parser` parameter in each `extract()` function and have `extract_symbols()` in `mod.rs` maintain one parser per language. Alternatively, use a thread-local or pass a pre-configured parser from the caller. Example:

```rust
// In extract/mod.rs
pub fn extract_symbols(
    path: &Path,
    content: &str,
    language: rskim_core::Language,
) -> Vec<ExtractedSymbol> {
    // Create parser once per language at the dispatch level
    let mut parser = tree_sitter::Parser::new();
    match language {
        rskim_core::Language::Rust => {
            let _ = parser.set_language(&tree_sitter_rust::LANGUAGE.into());
            rust_lang::extract_with_parser(&mut parser, path, content)
        }
        // ... etc
    }
}
```

Or better: since `generate_qrels` loops over all files, hoist the parser creation to that level and pass it down.

### should-fix -- Index re-opened from disk on every tuning evaluation
- **File:** `crates/rskim-bench/src/main.rs:291-306`
- **Confidence:** 88%
- **Description:** The `coordinate_descent` evaluator closure calls `NgramIndexReader::open_with_config(&idx_path, cfg)` for every candidate configuration. Coordinate descent evaluates: 6 (k1) + 8*9 (boosts) + 2*5 (b) = 88 evaluations per pass, up to 3 passes = 264 index opens. Each `open_with_config` re-reads index files from disk and reconstructs in-memory structures. Since the index data itself never changes -- only the BM25F scoring parameters change -- this is redundant I/O.
- **Suggestion:** If the `NgramIndexReader` API supports it, consider adding a method like `with_config(cfg)` that swaps scoring parameters on an already-loaded reader without re-reading the index from disk. If the API does not currently support this, file an issue on rskim-search. This is the single biggest performance bottleneck in the tuning workflow.

### should-fix -- Sequential repo processing in bench and tune commands
- **File:** `crates/rskim-bench/src/main.rs:161-200`, `crates/rskim-bench/src/main.rs:228-248`
- **Confidence:** 85%
- **Description:** The `run_bench` function iterates over repos sequentially (`for repo_entry in &corpus.repos`). Each repo involves git clone (or cache hit), file reading, indexing, and evaluation. These per-repo operations are independent and could be parallelized with rayon, which is already a dependency of the parent workspace. Similarly, `run_tune` loads all repos sequentially.
- **Suggestion:** Use `rayon::prelude::*` and `.par_iter()` for the repo loop in `run_bench`. Each repo gets its own tempdir and index, so there are no shared-state conflicts. This would give near-linear speedup on multi-core machines when benchmarking across multiple repos.

### should-fix -- Redundant content clone in qrel input construction
- **File:** `crates/rskim-bench/src/harness.rs:45`
- **Confidence:** 86%
- **Description:** `contents.get(&f.file_id).cloned().unwrap_or_default()` clones the full file content string for every file when building `QrelInput`. These strings can be large (entire source files). The `QrelInput` struct owns a `String`, but the content is only read (not mutated) during symbol extraction. This forces an O(n) allocation per file.
- **Suggestion:** Change `QrelInput.content` to a `&str` lifetime-borrowed from the `contents` HashMap, or pass the contents map directly to `generate_qrels` to avoid the intermediate allocation. If lifetime constraints make this difficult, an `Arc<String>` or `Cow<'_, str>` could avoid the clone.

### informational -- Clones in qrel generation pipeline
- **File:** `crates/rskim-bench/src/qrel.rs:75`, `crates/rskim-bench/src/qrel.rs:95`
- **Confidence:** 82%
- **Description:** In `generate_qrels`, `sym.name.clone()` is called twice per symbol: once in the DF map insertion (`df_map.entry(sym.name.clone())`) and once in the deduplication filter (`seen_names.insert(sym.name.clone())`). For corpora with thousands of symbols, this adds up. The filtering pipeline also collects into three intermediate `Vec`s (filtered, deduped, df_filtered) before stratification.
- **Suggestion:** Consider using a single-pass approach: iterate symbols once, maintain both DF tracking and deduplication in the same loop, and avoid the intermediate collections. The name could use `Rc<str>` to share the allocation between the symbol and the map key. This is a moderate optimization -- the current approach is clear and correct, and the symbol names are typically short strings.

### informational -- SHA-256 for train/test split is heavier than needed
- **File:** `crates/rskim-bench/src/split.rs:26-28`
- **Confidence:** 80%
- **Description:** `assign_split` computes a full SHA-256 hash just to get a single byte for bucketing. SHA-256 is cryptographically strong but overkill for a deterministic split function. A faster non-cryptographic hash (e.g., FNV, xxhash, or even the built-in `DefaultHasher`) would produce equally deterministic results with lower per-call cost.
- **Suggestion:** This matters only if `assign_split` is called in a very tight loop with millions of items. For the current use case (~50-100 qrels), SHA-256 is fine. Consider switching to a faster hasher if the corpus grows significantly. Not blocking.

## Suggestions (Lower Confidence)

- **Duplicate filter application in qrel generation** - `crates/rskim-bench/src/qrel.rs:71-88` (Confidence: 70%) -- The same `sym.name.len() >= MIN_NAME_LEN && sym.name.chars().any(|c| c.is_alphabetic())` filter is applied identically in both Phase 1 (lines 71-73, for DF counting) and Phase 2 (lines 84-88, for filtering). The Phase 1 check gates DF map insertion, while Phase 2 re-filters from `raw_symbols` which includes unfiltered entries. This means symbols that fail the filter still get pushed to `raw_symbols` (line 79), only to be filtered out in Phase 2. Filtering before push would avoid storing and later scanning these entries.

- **`path.to_path_buf()` called per symbol in extractors** - `crates/rskim-bench/src/extract/rust_lang.rs:52` and equivalents (Confidence: 65%) -- Every extracted symbol gets a `path.to_path_buf()` clone of the file path. Since all symbols from the same file share the same path, this could use `Arc<PathBuf>` or store a reference. Impact is low since paths are short, but it is a repeated allocation pattern.

- **`to_string()` on string format arguments in report.rs** - `crates/rskim-bench/src/report.rs:49-96` (Confidence: 62%) -- Report generation uses `format!()` extensively to build markdown strings, including string interpolation of already-owned strings. Using `write!()` to a single `String` buffer (already done via `md.push_str(&format!(...))`) could avoid some intermediate allocations by writing directly.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 2 | 2 | 0 |
| Pre-existing | 0 | 0 | 2 | 0 |

**Performance Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The crate is a benchmarking harness with naturally bounded workloads (finite repos, finite configs, finite qrels). The coordinate descent loop has a hard cap of 3 passes with ~88 evaluations each, so the absolute cost ceiling is well-defined. The two most impactful findings are the per-evaluation index re-open during tuning (should-fix, HIGH) and per-repo sequential processing (should-fix, MEDIUM). The parser allocation and content clone issues are proportional improvements that matter more as the corpus grows. None of these are blocking for a benchmarking tool, but the index re-open issue in particular could make the `tune` subcommand noticeably slow on larger corpora and should be addressed before heavy use.
