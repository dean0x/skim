---
focus: architecture
reviewer: Reviewer
timestamp: 2026-05-21T14:03:35Z
pr: 247
branch: feat/195-bm25f-bench
head: 101385eb
---

# Architecture Review

## Summary

The new `rskim-bench` crate is well-structured for an additive, unpublished benchmarking tool. Module boundaries are clean with clear separation of concerns (extract, qrel, harness, tuning, metrics, report). The main architectural risks are hardcoded magic numbers that duplicate upstream constants and significant code repetition in the CLI entry points that could become a maintenance burden.

## Issues in Your Changes (BLOCKING)

### HIGH

**Hardcoded field count 8 duplicates upstream constant** - `crates/rskim-bench/src/tuning.rs:82`, `crates/rskim-bench/src/tuning.rs:165-166`, `crates/rskim-bench/src/report.rs:82-91`, `crates/rskim-bench/src/configs.rs:8`
**Confidence**: 85%
- Problem: The literal `8` appears in loop bounds (`0..8`), array sizes (`[0.0f32; 8]`), and field name arrays throughout the crate. The upstream `rskim-search` defines `SearchField::count()` and `SearchField::ALL` as the single authoritative source for field count. If fields are ever added or removed from `SearchField`, the bench crate will silently produce incorrect results (truncated parameter arrays, skipped fields) rather than failing at compile time.
- Fix: Use `SearchField::count()` or `SearchField::ALL.len()` for loop bounds. For the report field name array in `report.rs:82-91`, derive names from `SearchField::ALL` rather than maintaining a parallel static list. For array sizes in `tuning.rs`, define a local constant: `const N: usize = SearchField::count();` and use `[0.0f32; N]`.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Duplicate file-loading and indexing logic across run_bench, run_tune, and run_qrels** - `crates/rskim-bench/src/main.rs:141-218`, `crates/rskim-bench/src/main.rs:220-353`, `crates/rskim-bench/src/main.rs:373-416`
**Confidence**: 82%
- Problem: Three CLI subcommand handlers each repeat the pattern: open corpus, iterate repos, sort files by path, assign FileIds, build HashMap of contents, construct QrelInput/IndexedFile vectors. The sort-then-enumerate-then-map-to-IndexedFile pattern alone appears in three places with minor variations (`run_bench` creates per-repo IDs starting at 0, `run_tune` uses a global counter, `run_qrels` starts at 0 per repo). This is a classic SRP violation -- the "load and prepare files from a corpus" concern is duplicated rather than extracted.
- Fix: Extract a shared helper, e.g. `fn load_repo_files(source: &dyn FileSource, repo: &RepoEntry) -> Result<(Vec<IndexedFile>, HashMap<FileId, String>)>` that handles fetch, sort, ID assignment, and content mapping. The global-counter variant for `run_tune` could take a `starting_id: u32` parameter.

**TuningResult uses Vec for fixed-size field arrays** - `crates/rskim-bench/src/types.rs:97-98`
**Confidence**: 82%
- Problem: `TuningResult.best_field_boosts` and `best_field_b` are `Vec<f32>` but always contain exactly 8 elements (one per field). This forces `result_to_config()` in `tuning.rs:165-172` to perform fallible conversion with manual index-by-index copying and `.take(8)` guards, and the report renderer in `report.rs:93-94` uses `.get(i).copied().unwrap_or(0.0)`. The Vec type communicates "variable length" when the domain invariant is "exactly FIELD_COUNT elements."
- Fix: Change to `[f32; 8]` (or `[f32; SearchField::count()]` if const generics allow). This removes the fallible conversion in `result_to_config()` and the defensive `.get()` calls in the report renderer.

**main.rs directly constructs GitCloneSource rather than using the FileSource trait** - `crates/rskim-bench/src/main.rs:135-138`
**Confidence**: 80%
- Problem: `open_corpus()` directly constructs a `GitCloneSource` with a concrete struct literal rather than returning `Box<dyn FileSource>`. While this works for the CLI, it means the corpus-loading path cannot be tested without network access. The `rskim-research` crate already provides `FixtureSource` for this purpose. Since the harness functions (`run_on_files`, `evaluate_split`) correctly accept trait objects / abstract types, the tight coupling is localized to `open_corpus()` but still limits testability of the CLI orchestration layer.
- Fix: Return `Box<dyn FileSource>` from `open_corpus()` to allow injection of `FixtureSource` in tests. This aligns with the DIP principle and the existing trait design in rskim-research.

### LOW

**RepoBenchResult.repo_url initialized empty and filled by caller** - `crates/rskim-bench/src/harness.rs:102`
**Confidence**: 80%
- Problem: `run_on_files()` returns a `RepoBenchResult` with `repo_url: String::new()` and a comment "filled in by caller." This is an incomplete-construction pattern that leaves the type temporarily invalid. Every call site must remember to set the URL afterwards.
- Fix: Accept `repo_url: &str` as an additional parameter to `run_on_files()` so the struct is fully initialized at construction time.

## Pre-existing Issues (Not Blocking)

None identified. This is an additive-only change with no modifications to existing crates.

## Suggestions (Lower Confidence)

- **Extract module duplicates rskim-core's parser initialization** - `crates/rskim-bench/src/extract/rust_lang.rs:18-28`, `python.rs:15-25`, `go.rs:16-26` (Confidence: 70%) -- Each extractor repeats the same parser setup boilerplate (create Parser, set language, parse, handle errors). A shared helper `fn parse_with_language(content: &str, lang: tree_sitter::Language) -> Option<tree_sitter::Tree>` would reduce this to one line per extractor. The extractors also duplicate the tree-sitter grammar dependency because `rskim-core::Language::to_tree_sitter()` is `pub(crate)` -- worth considering whether to expose that method or maintain the current duplication.

- **Coordinate descent has implicit coupling to BM25FConfig's field layout** - `crates/rskim-bench/src/tuning.rs:59-107` (Confidence: 65%) -- The tuning loop sweeps k1, then field_boosts by index, then field_b by index. The sweep order and the notion that BM25FConfig has exactly these three parameter groups is hardcoded. If BM25FConfig gains a new parameter, the tuner would silently skip it. A more extensible approach would define a `TunableParam` enum, but given the crate is `publish = false` and tightly coupled to BM25F by design, this is likely acceptable.

- **No abstraction boundary between bench-specific and generic IR evaluation code** - `crates/rskim-bench/src/metrics.rs` (Confidence: 62%) -- The metrics module (MRR, Precision@K) is generic IR evaluation code that could be useful outside benchmarking. It currently lives inside rskim-bench, so any future crate that needs these metrics would need to either duplicate them or depend on the bench crate. This is minor given the crate's internal-only status.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | 0 |
| Should Fix | 0 | 0 | 3 | 1 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The crate demonstrates solid module decomposition, correct dependency direction (bench depends on search/core/research, never the reverse), proper use of traits in the harness layer (`SearchLayer`), and well-separated concerns. The blocking issue (hardcoded field count) introduces a fragile coupling that could cause silent correctness bugs if the upstream field set changes. The should-fix issues around code duplication in main.rs and Vec-typed fixed-size arrays are maintainability concerns that would benefit from cleanup before this crate sees wider use.
