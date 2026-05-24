# Complexity Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**`dns.rs` exceeds file length threshold (1034 lines)** - `crates/rskim/src/cmd/infra/dns.rs`
**Confidence**: 92%
- Problem: At 1034 lines this file is more than double the 500-line critical threshold. It contains two independent parser chains (dig and nslookup), each with their own regex definitions, three-tier parse logic, helper functions, and a full test suite -- all in one file. The two parsers share almost no logic beyond `combine_stdout_stderr`, so the coupling that justifies co-location is minimal.
- Fix: Split into three files: `dns/mod.rs` (re-exports, shared regex utilities), `dns/dig.rs` (dig config, regex, parse chain, helpers), `dns/nslookup.rs` (nslookup config, regex, parse chain, helpers). Move tests into `dns/dig_tests.rs` and `dns/nslookup_tests.rs`. This mirrors the existing `docker/mod.rs` and `gh/mod.rs` patterns already in the infra module.

### MEDIUM

**`walk_and_read` function length (85 lines, L123-L253)** - `crates/rskim/src/cmd/search/walk.rs:123`
**Confidence**: 82%
- Problem: `walk_and_read` is the longest function in the search module at 85 lines (after excluding comments/whitespace, roughly 60 lines of logic). It handles walker construction, entry iteration, file type filtering, language detection, size pre-screening, file reading, minification checking, SHA-256 computation, and relative path construction -- all in a single function with 4 levels of nesting (for loop > match > if > match on `ReadOutcome`). Each skip condition adds an independent `continue` branch, making the control flow wide.
- Fix: Extract the per-file processing body into a helper:
```rust
enum FileDecision {
    Accept(ReadFile),
    Skip(SkipReason),
    Ignore, // non-file entry, no action
}

fn classify_entry(
    entry: &ignore::DirEntry,
    root: &Path,
) -> FileDecision { ... }
```
This reduces `walk_and_read` to walker setup + a loop that calls `classify_entry` and pushes to the appropriate vec.

## Issues in Code You Touched (Should Fix)

_No issues found._

## Pre-existing Issues (Not Blocking)

_No issues found._

## Suggestions (Lower Confidence)

- **`try_parse_nslookup_structured` has moderate nesting** - `crates/rskim/src/cmd/infra/dns.rs:369` (Confidence: 65%) -- The function branches on NXDOMAIN vs. success and assembles domain names from multiple fallback sources (Name line, MX domain, "unknown"). While the logic is sound, the layered `or_else` domain extraction could be extracted into a `resolve_queried_domain(text)` helper for clarity.

- **`build_index` function length (93 lines, L150-L243)** - `crates/rskim/src/cmd/search/index.rs:150` (Confidence: 70%) -- At 93 lines `build_index` is the pipeline orchestrator and does walk, classify, build, and manifest write in sequence. The step numbering in comments helps readability, but the function is at the upper edge of the 50-line warning. The empty-files early return (L165-L175) could become a separate function. Not urgent because the pipeline steps are linear and well-commented.

- **`extract_dig_question_domain` state machine complexity** - `crates/rskim/src/cmd/infra/dns.rs:243` (Confidence: 62%) -- This function tracks an `in_question` boolean and iterates lines with multiple termination conditions (empty line, `;;` line, semicolon-prefixed data line). The state machine is small (2 states) but the conditions are subtle enough to warrant a clarifying comment about why single-semicolon lines before QUESTION SECTION are ignored.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Complexity Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The search pipeline module (`index.rs`, `walk.rs`, `manifest.rs`, `types.rs`) is well-decomposed: types are separated from I/O, the pipeline is linear and clearly documented, and functions stay within reasonable bounds. The `walk_and_read` function is the only one that warrants extraction of a per-entry helper to flatten the nesting.

The `dns.rs` file is the main complexity concern. At 1034 lines it contains two fully independent parser implementations that should live in separate files, following the directory-module pattern already used by `docker/` and `gh/` in the same parent module. This is a structural issue, not a logic issue -- the individual functions within dns.rs are well-factored and readable.

No cyclomatic complexity thresholds are exceeded in any individual function. Nesting stays at 4 levels maximum. No unbounded loops exist -- the walker is bounded by `max_files` and the ancestor traversal by `MAX_ANCESTORS`. Overall complexity is well-managed.
