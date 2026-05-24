---
title: "Empirical Sparse N-gram Weight Table"
issue: "#176"
wave: "1a"
status: "proposed"
created: "2026-05-12"
scope: "global byte-level bigram IDF (per-field deferred to Wave 1c)"
---

# Implementation Plan: Issue #176 — Empirical Sparse N-gram Weight Table

## 1. Goal & Scope

Build a developer-only research binary (`rskim-research`) that derives an empirical
character bigram IDF weight table by analyzing 25 open-source codebases. The output
powers the sparse n-gram search index (Wave 1).

**In scope**: Global byte-level bigram IDF weights, border-weight validation,
covering set heuristic testing, JSON canonical output, checked-in const array.

**Out of scope (deferred to Wave 1c)**: Per-field BM25F bigram weights,
FieldClassifier implementation, node_kind_info export, NodeInfo::from_ts_node bridge.

## 2. Architecture

```
crates/rskim-research/       (NEW — publish=false, [[bin]] target)
  ├── corpus.toml             (25 repo URLs, per-language)
  ├── src/
  │   ├── main.rs             (CLI: run | codegen | validate)
  │   ├── types.rs            (BigramWeight, WeightTable, CorpusStats)
  │   ├── config.rs           (TOML corpus config parsing)
  │   ├── clone.rs            (FileSource trait + GitCloneSource)
  │   ├── extract.rs          (byte bigram extraction, pure)
  │   ├── idf.rs              (IDF computation, pure)
  │   ├── validate.rs         (border vs uniform comparison)
  │   └── codegen.rs          (JSON → Rust const array)
  └── tests/fixtures/         (small source samples for unit tests)

crates/rskim-search/          (MODIFIED — 2 new files)
  ├── data/bigram_weights.json (canonical JSON, checked in)
  └── src/weights.rs           (checked-in const array, generated)
```

**Dependency graph**: `rskim-research → rskim-core` only (for Language detection).
Does NOT depend on rskim-search. No circular risk.

**Why separate crate**: rskim-search has explicit "LIBRARY with NO I/O" contract.
Adding a [[bin]] target would violate this, pulling I/O deps into the library crate.

## 3. Key Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Bigram encoding | `u16` (byte1 << 8 \| byte2) | Byte-level, fast, covers ASCII code |
| Weight table format | Sorted `&[(u16, f32)]` const slice | O(log n) binary search, const-friendly |
| Codegen approach | Checked-in `.rs` file (no build.rs) | No fragile build dependency |
| Cloning strategy | `git clone --depth 1` via Command | Consistent with heatmap pattern |
| Test strategy | Pure fn unit tests + FileSource trait | Full corpus as `#[ignore]` |
| File size limit | Skip files >100KB | Prevents generated files skewing IDF |
| Binary detection | Skip files with null bytes | Simple, effective |

## 4. File Specifications

### 4.1 NEW: `crates/rskim-research/Cargo.toml`

```toml
[package]
name = "rskim-research"
version = "0.1.0"
edition = "2024"
publish = false

[[bin]]
name = "rskim-research"
path = "src/main.rs"

[dependencies]
rskim-core = { version = "2.9.0", path = "../rskim-core" }
serde = { workspace = true }
serde_json = { workspace = true }
toml = { workspace = true }
rayon = { workspace = true }
ignore = { workspace = true }
tempfile = { workspace = true }
anyhow = { workspace = true }

[dev-dependencies]
insta = { workspace = true }

[lints.clippy]
unwrap_used = "deny"
expect_used = "deny"
panic = "deny"
todo = "warn"
```

### 4.2 MODIFIED: Root `Cargo.toml`

Add `"crates/rskim-research"` to `[workspace] members`.

### 4.3 NEW: `crates/rskim-research/corpus.toml`

25 repos (5 per language):

- **Rust**: ripgrep, serde, tokio, axum, clap
- **TypeScript**: vscode, typescript-eslint, next.js, zod, trpc
- **Python**: flask, django, fastapi, httpx, pydantic
- **Go**: hugo, gin, cobra, prometheus, minio
- **Java**: spring-boot, guava, jackson, netty, junit5

### 4.4 NEW: `crates/rskim-research/src/types.rs`

```rust
pub struct BigramWeight { pub bigram: u16, pub idf: f32 }
pub struct SourceFile { pub path: PathBuf, pub language: Language, pub content: String }
pub struct CorpusStats { pub total_files: u32, pub total_bigrams: u64, pub unique_bigrams: usize, pub language_breakdown: Vec<LanguageCount> }
pub struct LanguageCount { pub language: String, pub file_count: u32 }
pub struct ValidationResult { pub uniform_selectivity: f64, pub border_weighted_selectivity: f64, pub improvement_pct: f64 }
pub struct WeightTable { pub version: u8, pub generated_at: String, pub corpus_stats: CorpusStats, pub weights: Vec<BigramWeight> }
```

### 4.5 NEW: `crates/rskim-research/src/extract.rs`

Pure functions:
- `encode_bigram(b1: u8, b2: u8) -> u16`
- `decode_bigram(u16) -> (u8, u8)`
- `extract_bigrams(content: &str) -> HashMap<u16, u32>` (frequency per file)
- `extract_bigrams_from_corpus(files: &[SourceFile]) -> (HashMap<u16, u32>, u32)` (document frequency + doc count)

### 4.6 NEW: `crates/rskim-research/src/idf.rs`

Pure functions:
- `compute_idf(df: u32, total_docs: u32) -> f32` — `ln(N / (df + 1)) + 1.0`
- `compute_weight_table(df_map, total_docs) -> Vec<BigramWeight>` — filter, sort, return
- `selectivity(query: &str, weights: &[(u16, f32)]) -> f64`

### 4.7 NEW: `crates/rskim-research/src/clone.rs`

```rust
pub(crate) trait FileSource: Send + Sync {
    fn fetch_files(&self, repo: &RepoEntry) -> anyhow::Result<Vec<SourceFile>>;
}
```

`GitCloneSource`: shells out to `git clone --depth 1`, walks with `ignore::WalkBuilder`,
skips >100KB / null-byte / non-UTF8 / non-target-language files.

`FixtureSource`: reads from `tests/fixtures/` for unit testing.

### 4.8 NEW: `crates/rskim-research/src/validate.rs`

- `border_weighted_selectivity(query, weights) -> f64` — 3.5x for first/last 2 chars of tokens
- `uniform_selectivity(query, weights) -> f64` — no positional bonus
- `run_validation(weights, test_queries) -> ValidationResult`
- `covering_set_heuristic(query, weights) -> Vec<u16>` — greedy minimum cover

### 4.9 NEW: `crates/rskim-research/src/codegen.rs`

- `generate_weights_rs(json_path, output_path) -> Result<()>`
- Produces `pub const BIGRAM_WEIGHTS: &[(u16, f32)]` with inline bigram comments
- Produces `pub fn bigram_weight(bigram: u16) -> Option<f32>` binary search lookup

### 4.10 NEW: `crates/rskim-search/src/weights.rs`

Checked-in generated file. Contains const array + lookup function + unit tests
(sorted invariant, known lookup, no duplicates).

### 4.11 MODIFIED: `crates/rskim-search/src/lib.rs`

Add: `pub mod weights;` and re-export `BIGRAM_WEIGHTS`, `bigram_weight`.

## 5. Implementation Steps

1. Scaffold crate (directory, Cargo.toml, workspace member)
2. Types + config (data types, TOML corpus parsing, unit tests)
3. Extraction (byte bigram encoding/extraction, unit tests)
4. IDF computation (formula, weight table, selectivity, unit tests)
5. Clone infrastructure (FileSource trait, GitCloneSource, FixtureSource)
6. Validation (border vs uniform, covering set, unit tests)
7. Codegen (JSON → Rust const array, snapshot tests)
8. CLI orchestration (main.rs with run/codegen/validate subcommands)
9. Full corpus run (execute, verify JSON, generate weights.rs)
10. Integration (add weights module to rskim-search, verify all tests)

## 6. Test Strategy

### Unit tests (inline `#[cfg(test)]`)
- extract: encode/decode roundtrip, empty/single-char edge cases, UTF-8 handling, DF counting
- idf: universal vs rare bigram IDF, always-positive invariant, table sorting
- validate: border > uniform, covering set coverage, high-IDF preference
- config: TOML parsing, invalid language rejection
- codegen: generated format, comment inclusion

### Fixture tests
- Rust/Python sample files → verify known bigrams present
- Binary file → verify skip detection

### Integration (`#[ignore]`)
- Full 25-repo pipeline end-to-end

## 7. Gap Analysis Summary

### Resolved by this plan
- Binary placement → separate crate
- No build.rs fragility → checked-in const array
- JSON schema → WeightTable struct with version, stats, weights

### Deferred to Wave 1c (#8)
- Per-field BM25F bigram weights
- FieldClassifier production implementation
- node_kind_info() public export from rskim-core
- NodeInfo::from_ts_node bridge

## 8. Risks

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| Network flakiness | Medium | 3 retries, partial corpus (min 3/5 per lang) |
| Border weighting no improvement | Low | Valid research finding, IDF table still useful |
| Large repos slow | Medium | --depth 1, skip >100KB files |
| Bigram table too large | Low | IDF threshold >1.5 for sparsity |

## 9. Design Review Notes

- Consider parallel repo cloning (4-5 concurrent) to reduce wall-clock time
- Add `--corpus-dir` flag for persistent clone cache during iteration
- Verify `indicatif` in workspace deps; if absent, use stderr progress printing
- Codegen subcommand should validate JSON schema before generating Rust source

## 10. Acceptance Criteria

- JSON weight table with ≥1000 unique bigrams, IDF scores in valid range
- ≥20 repos across 5 languages successfully processed
- Border-weighted selectivity > uniform selectivity for code search queries
- Covering set heuristic produces non-empty output for test queries
- `cargo test --workspace` passes with new crate
- `cargo clippy --workspace -- -D warnings` clean

## 11. PR Description Guidance

**Problem**: Wave 1 needs empirical bigram IDF weights to power sparse n-gram
candidate generation. Without measured selectivity data, the search index
cannot prioritize discriminating bigrams over universal ones.

**Key Changes**: New `rskim-research` crate (developer tool), new `weights.rs`
module in rskim-search with const lookup table.

**Breaking Changes**: None.

**Reviewer Focus**: Bigram encoding correctness (extract.rs), IDF formula
(idf.rs), border-weight validation methodology (validate.rs).
