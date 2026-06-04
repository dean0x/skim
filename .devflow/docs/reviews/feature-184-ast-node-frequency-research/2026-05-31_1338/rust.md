# Rust Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31T13:38

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**Inaccurate comment lists Markdown as non-tree-sitter language** - `crates/rskim-research/src/ast_extract.rs:77`
**Confidence**: 90%
- Problem: The comment says "Parser::new returns Err for non-tree-sitter languages (JSON, YAML, TOML, Markdown)" but `Language::Markdown` has a tree-sitter grammar (`to_tree_sitter()` returns `Some(tree_sitter_md::LANGUAGE.into())`), so `Parser::new(Language::Markdown)` succeeds. The code behaves correctly (Markdown files are extracted), but the comment misleads developers into thinking Markdown is skipped. The test `all_14_ts_languages_produce_output` at line 494 confirms Markdown does produce AST bigrams. Applies ADR-001 (fix noticed issues immediately).
- Fix:
```rust
// Parser::new returns Err for non-tree-sitter languages (JSON, YAML, TOML).
// We treat these as "no AST available" and return an empty result.
```

### MEDIUM

**`AST_VALID_LANGUAGES` uses `"Sql"` but `Language::name()` returns `"SQL"`** - `crates/rskim-research/src/config.rs:49`
**Confidence**: 82%
- Problem: `AST_VALID_LANGUAGES` contains `"Sql"` (capitalized), but `Language::Sql.name()` returns `"SQL"` (all uppercase). The corpus TOML `language` field is validated against `AST_VALID_LANGUAGES`, so a user writing `language = "SQL"` (matching the canonical name) would be rejected. Currently no repos in `ast-corpus.toml` use `language = "Sql"` directly (SQL files are extracted from polyglot repos by extension), so no data is lost, but the inconsistency creates a trap for future TOML entries. Applies ADR-001.
- Fix: Either change `AST_VALID_LANGUAGES` entry to `"SQL"` to match `Language::name()`, or document the mismatch explicitly.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`--trigrams` uses `default_value = "true"` instead of `default_value_t = true`** - `crates/rskim-research/src/main.rs:97` (Confidence: 65%) -- The `#[arg(long, default_value = "true")]` pattern on a `bool` field means the user must type `--trigrams false` to disable, rather than the more idiomatic `--no-trigrams`. Since this is an internal research binary, the impact is minimal, but `default_value_t = true` would be more conventional for clap.

- **`walk_tree` recursion at depth 500 may consume significant stack** - `crates/rskim-research/src/ast_extract.rs:21` (Confidence: 62%) -- `MAX_AST_DEPTH = 500` with the `walk_tree` recursive function. Each frame is small (~80 bytes for 5 args + cursor), so 500 * 80 = ~40 KiB is well within the default 8 MiB stack, but on rayon worker threads with smaller stacks, deeply nested pathological ASTs could be tighter. The current bounded depth + node count guards are adequate for production corpora.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 9/10
**Recommendation**: CHANGES_REQUESTED

The implementation is well-structured with strong Rust idioms throughout: proper `Result`/`?` propagation, `#[must_use]` annotations, borrowing over cloning, `debug_assert!` at appropriate invariants, bounded recursion with explicit depth/node limits, and comprehensive test coverage (103 tests passing, clippy clean with zero warnings). The type system design (vocabulary, packed bigram/trigram encoding, remap table) is clean and efficient. The two blocking items are minor documentation/naming inconsistencies that should be fixed inline (avoids PF-002 -- not deferring noticed issues).
