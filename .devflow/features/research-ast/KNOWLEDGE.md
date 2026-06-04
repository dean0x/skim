---
feature: research-ast
name: AST Node Frequency Research
description: "Use when extending the AST n-gram pipeline, regenerating ast_weights.rs, adding new tree-sitter languages to the corpus, modifying the vocabulary stabilization step, or debugging IDF weight quality. Keywords: ast_weights, bigram, trigram, NodeKindVocabulary, stabilize, IDF, rskim-research, ast-corpus.toml, codegen."
category: architecture
directories: [crates/rskim-research/src/]
referencedFiles:
  - crates/rskim-research/src/ast_types.rs
  - crates/rskim-research/src/ast_extract.rs
  - crates/rskim-research/src/ast_idf.rs
  - crates/rskim-research/src/ast_codegen.rs
  - crates/rskim-research/src/ast_validate.rs
  - crates/rskim-research/src/ast_pipeline.rs
  - crates/rskim-research/src/ast_cmd.rs
  - crates/rskim-research/src/clone.rs
  - crates/rskim-research/src/config.rs
  - crates/rskim-research/src/main.rs
  - crates/rskim-research/src/lib.rs
created: 2026-05-31
updated: 2026-06-03
---

# AST Node Frequency Research

## Overview

`rskim-research` is a developer-only binary (`publish = false`) that builds empirical IDF weight tables from source code corpora. It drives two parallel pipelines: one for character bigrams (lexical) and one for AST node-kind bigrams and trigrams. This document covers the AST pipeline exclusively — the `ast_*` modules.

The AST pipeline walks real open-source repositories, extracts parent→child and grandparent→parent→child AST node-kind pairs via tree-sitter, computes corpus-level IDF scores, then code-generates a static Rust file (`ast_weights.rs`) embedded in `rskim-search`. All IDF weights are computed offline so the search engine at runtime pays only a `binary_search` lookup cost, not a full corpus pass.

## System Context

The pipeline feeds `crates/rskim-search/src/ast_weights.rs`. That generated file is NOT hand-written — it is produced by running `rskim-research ast-codegen` after a successful `ast-run`. Any manual edits to `ast_weights.rs` will be overwritten the next time codegen runs.

Workflow:
```
ast-corpus.toml → ast-run → ast_weights.json → ast-codegen → ast_weights.rs (in rskim-search)
                                               → ast-validate (QA check, does not modify files)
```

## Component Architecture

### `ast_types.rs` — Core Types and Vocabulary

Defines the integer-packing scheme that makes n-gram storage compact:

- `NodeKindId = u16` — compact ID for a tree-sitter node kind string
- `AstBigram = u32` — high 16 bits = parent ID, low 16 bits = child ID
- `AstTrigram = u64` — bits `[47:32]` = grandparent ID, `[31:16]` = parent ID, `[15:0]` = child ID

`NodeKindVocabulary` is a bidirectional string↔ID map. IDs are assigned incrementally as new node kinds are encountered during the corpus walk. After the entire corpus is processed, `stabilize()` must be called — it sorts all kind strings alphabetically, reassigns IDs, and returns a `Vec<NodeKindId>` remap table. The caller must then re-key all bigram/trigram DF maps through `rekey_bigram_df_map` / `rekey_trigram_df_map` before computing IDF scores.

This two-pass design (insert with temporary IDs, then stabilize) ensures that `ast_weights.rs` always encodes node kinds with the same IDs regardless of which corpus files were processed first. Without stabilization, the generated file would differ across runs depending on traversal order.

### `ast_extract.rs` — Tree Walk and DF Accumulation

`extract_ast_ngrams_from_corpus` groups files by language, SHA-256-deduplicates within each group (identical files across different repos count as one document), then calls `extract_ast_ngrams_from_file` per file.

`extract_ast_ngrams_from_file` hard-limits are:
- `MAX_FILE_SIZE = 100 KiB` — oversized files return an empty result silently
- `MAX_AST_DEPTH = 500` — guards against infinite recursion in pathological inputs
- `MAX_AST_NODES = 100_000` — per-file node count cap
- `MAX_TRIGRAMS_PER_FILE = 50_000` — memory guard for trigram collection

ERROR nodes from tree-sitter (parse failures) are counted but not included in bigram/trigram pairs. The error rate is surfaced in `AstCorpusStats.error_node_count` and reported by `ast-validate`.

Non-tree-sitter languages (JSON, YAML, TOML, Markdown for the serde path) are handled by checking that `rskim_core::Parser::new(language)` returns `Ok` — it returns `Err` for non-tree-sitter languages, so the file returns an empty result without error.

### `ast_idf.rs` — IDF Computation

Uses the same smoothed IDF formula as the lexical module: `idf = ln(N / (df + 1)) + 1.0`. Bigrams and trigrams present in more than `1/threshold` of the corpus documents are excluded. The default threshold is `1.5`. Results are sorted by IDF descending so the most discriminating pairs appear first in the generated file.

### `ast_codegen.rs` — Rust Code Generation

`generate_ast_weights_rs` reads `ast_weights.json`, validates it (non-zero version, all IDF values finite and positive), then writes a compilable `ast_weights.rs` containing:

- `NODE_KIND_VOCABULARY: &[&str]` — vocabulary indexed by `NodeKindId`
- Per-language `{LANG}_AST_BIGRAM_WEIGHTS: &[(u32, f32)]` sorted ascending by bigram key
- Per-language `{LANG}_AST_TRIGRAM_WEIGHTS: &[(u64, f32)]` sorted ascending by trigram key
- `pub fn ast_bigram_weight(lang: &str, bigram: u32) -> Option<f32>` using `binary_search_by_key`
- `pub fn ast_trigram_weight(lang: &str, trigram: u64) -> Option<f32>` using `binary_search_by_key`

Language name → Rust identifier conversion is done by `lang_to_ident`: `"TypeScript"` → `"TYPESCRIPT"`, `"Cpp"` → `"CPP"`, `"CSharp"` → `"CSHARP"`. Special characters (`+`, `#`, `-`, space) map to `_` with consecutive underscores collapsed.

### `ast_validate.rs` — Quality Reporting

`run_ast_validation` produces an `AstValidationReport` with per-language IDF distribution statistics (count, min, max, mean, median, p90, p99), top-20 most discriminating bigrams and trigrams, and the error node rate. Output goes to stderr only — it does not affect stdout or return an exit code. Use it to sanity-check weight quality before committing a newly generated `ast_weights.json`.

### `ast_pipeline.rs` — Orchestration

Extracted from `main.rs` in a refactor to keep `main.rs` below the 500-line threshold. Contains the end-to-end pipeline orchestration for `ast-run`: cloning repos, extracting n-grams, stabilizing vocabulary, computing IDF, and serializing to `ast_weights.json`. Called by `ast_cmd.rs`.

### `ast_cmd.rs` — CLI Subcommand Handlers

Contains `cmd_ast_run`, `cmd_ast_codegen`, and `cmd_ast_validate` handler functions. Dispatched from `main.rs`. Accepts `PathBuf` for corpus dir/output paths and delegates to `ast_pipeline`, `ast_codegen`, and `ast_validate` modules. Also imports from `clone`, `codegen`, `config`, `types` (shared helpers).

### `config.rs` — Corpus Configuration

Two distinct config loaders exist:
- `load_corpus_config` — lexical bigram pipeline, accepts only `["Rust", "TypeScript", "Python", "Go", "Java"]`
- `load_ast_corpus_config` — AST pipeline, accepts all 14 tree-sitter languages via `AST_VALID_LANGUAGES`

The AST loader also accepts `"HEAD"` as a commit reference in addition to 40-character hex SHAs — the lexical loader requires a pinned SHA. All URLs must use `https://` (not `git://` or `file://` — enforced as a security guard against injection via TOML config).

### `clone.rs` — File Loading and Git Operations

Two `FileSource` implementations:
- `GitCloneSource` — shallow clone to pinned commit, lexical extensions only
- `AstGitCloneSource` — shallow clone, then `walk_and_load_ast` with `AST_TARGET_EXTENSIONS`

Both share the `clone_repo` helper which tries a shallow clone first, falls back to a full clone if the pinned commit is unreachable in the shallow history, then does `git checkout <commit>`. All git subprocesses are killed via SIGKILL (Unix) / `taskkill /F` (Windows) after `GIT_SUBPROCESS_TIMEOUT_SECS = 300` seconds.

The `FileSource` trait (`fetch_files(&self, repo: &RepoEntry) -> anyhow::Result<Vec<SourceFile>>`) allows `FixtureSource` in tests to provide local files without network access.

## Component Interactions

The data flow across modules for a single `ast-run` invocation:

1. `config::load_ast_corpus_config` parses `ast-corpus.toml`
2. `clone::AstGitCloneSource::fetch_files` clones repos and loads source files
3. `ast_types::NodeKindVocabulary::new()` creates a shared vocabulary (mutable, passed to every file)
4. `ast_extract::extract_ast_ngrams_from_corpus` fills `BigramDfMap` and `TrigramDfMap` per language
5. `vocab.stabilize()` returns a remap table — this is the critical ordering fence
6. `ast_types::rekey_bigram_df_map` / `rekey_trigram_df_map` re-encode all DF keys with post-stabilize IDs
7. `ast_idf::compute_ast_bigram_weights` / `compute_ast_trigram_weights` apply threshold and sort
8. `AstWeightTable` is serialized to `ast_weights.json`
9. `ast_codegen::generate_ast_weights_rs` reads the JSON and writes `ast_weights.rs`

The vocabulary is shared across all languages in the corpus so that the same node kind string (e.g., `"identifier"`) maps to the same integer ID regardless of which language first introduced it.

## Anti-Patterns

- **Skipping `vocab.stabilize()` before re-keying**: Without stabilization, bigram/trigram keys computed during extraction use insertion-order IDs. The generated `ast_weights.rs` will encode different IDs on different runs, making the output non-reproducible. Always call `stabilize()` and apply the returned remap before computing IDF scores.

- **Editing `ast_weights.rs` by hand**: The file is generated and carries a "DO NOT EDIT MANUALLY" header. Hand edits are overwritten the next time `ast-codegen` runs. Add new language support by updating `ast-corpus.toml` and re-running the full pipeline.

- **Using `load_corpus_config` for the AST pipeline**: The lexical loader only accepts 5 languages and requires 40-character pinned SHAs. Using it for an AST corpus TOML file will silently reject valid languages like `"Cpp"` or `"CSharp"`.

- **Including JSON/YAML/TOML repos in `ast-corpus.toml`**: `config.rs` explicitly rejects `"Json"` as an AST language (enforced by `AST_VALID_LANGUAGES`). Even if bypassed, `extract_ast_ngrams_from_file` returns an empty result for serde-backed languages because `Parser::new()` returns `Err` for them.

- **Ignoring the `MAX_FILE_SIZE` skip log**: When files are silently skipped (>100 KiB), they are counted in `total_files_seen` but not in DF maps. If the corpus has many large files, the `total_docs` value passed to IDF computation will be inflated relative to the actual document count, shifting IDF scores down. Monitor the skip rate with `ast-validate`.

## Gotchas

- **Vocabulary IDs are only stable after `stabilize()`**: Any bigram/trigram key encoded with pre-stabilize IDs must be re-encoded via the returned remap table. The `remap_bigram` / `remap_trigram` functions return `None` if an ID is out of bounds — this indicates a bug in the remap table size, not normal operation.

- **`AstGitCloneSource` uses shallow clone even for AST corpora**: Deep clone is available via `RepoEntry::deep_clone` but `AstGitCloneSource` does not check it — it always does a shallow clone. Deep clone is used by `clone_with_history` for co-change analysis only.

- **Markdown is a tree-sitter language for AST extraction, but NOT for the lexical pipeline**: `"md"` and `"markdown"` appear in `EXCLUDED_EXTENSIONS` (lexical pipeline) but also in `AST_TARGET_EXTENSIONS`. This is intentional: tree-sitter-markdown provides an AST, so Markdown repos can contribute AST n-grams.

- **`lang_to_ident` must match `Language::name()` output**: The generated match arms in `ast_bigram_weight` use the exact string returned by `Language::name()` (e.g., `"Rust"`, `"TypeScript"`, `"Cpp"`). If `rskim-core` renames a language variant, both the corpus TOML and the generated file will need updates.

- **`ast-validate` output goes to stderr**: `print_ast_validation_report` uses `eprintln!` throughout. Piping `rskim-research ast-validate` to a file captures nothing — redirect stderr explicitly if needed.

- **Trigram collection is gated by `--trigrams` flag**: The `ast-run` subcommand has `--trigrams` defaulting to `true`. If trigrams are disabled at extraction time, the resulting `ast_weights.json` will have empty `trigram_weights` maps. `ast-codegen` will still emit the trigram lookup function — it will just match no language and always return `None`.

## Key Files

- `crates/rskim-research/src/ast_types.rs` — packed integer types, `NodeKindVocabulary`, `AstWeightTable` serde struct
- `crates/rskim-research/src/ast_extract.rs` — tree-sitter walk, DF accumulation, SHA-256 deduplication, all safety limits
- `crates/rskim-research/src/ast_idf.rs` — smoothed IDF formula, threshold filtering, sort-descending output
- `crates/rskim-research/src/ast_codegen.rs` — JSON→compilable Rust generation, binary-search lookup functions
- `crates/rskim-research/src/ast_validate.rs` — IDF distribution stats, error node rate, top-N reporting
- `crates/rskim-research/src/ast_pipeline.rs` — end-to-end ast-run pipeline orchestration (extracted from main.rs for line budget); calls clone → extract → stabilize → IDF → serialize
- `crates/rskim-research/src/ast_cmd.rs` — `cmd_ast_run`, `cmd_ast_codegen`, `cmd_ast_validate` CLI handlers; dispatched from main.rs
- `crates/rskim-research/src/config.rs` — two separate loaders with distinct language allowlists
- `crates/rskim-research/src/clone.rs` — `FileSource` trait, `GitCloneSource`, `AstGitCloneSource`, git subprocess timeout machinery
- `crates/rskim-research/src/main.rs` — subcommand dispatch entry point; delegates ast-* subcommands to `ast_cmd.rs`
- `crates/rskim-search/src/ast_weights.rs` — generated output (DO NOT EDIT, not tracked here)

## Related

- ADR-001: Fix all noticed issues immediately regardless of scope — applies when extending the AST pipeline or fixing extraction bugs
- Feature knowledge: `cochange` — shares the `clone.rs` infrastructure (`clone_with_history`, `git_output_with_timeout`) for full-history repo cloning used in co-change analysis
- Feature knowledge: `temporal-scoring` — the consumer side; `ast_weights.rs` feeds scoring signals into `rskim-search`
- `crates/rskim-search/data/ast_weights.json` — intermediate artifact between `ast-run` and `ast-codegen`
- `crates/rskim-search/src/ast_weights.rs` — final generated artifact consumed by the search engine
