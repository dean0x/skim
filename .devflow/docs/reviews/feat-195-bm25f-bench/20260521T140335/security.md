---
focus: security
reviewer: Reviewer
timestamp: 2026-05-21T14:03:35Z
pr: 247
branch: feat/195-bm25f-bench
head: 101385eb
---

# Security Review

## Summary

The rskim-bench crate is an additive-only, internal benchmarking harness with no network-facing API surface. The crate operates on local files and pre-validated corpus configuration. Security-relevant input paths (git cloning, corpus config) are properly hardened in the upstream rskim-research dependency with HTTPS-only URLs, hex-only commit SHAs, and parameterised subprocess invocation. No critical or high-severity security issues were found.

## Findings

### informational — Deserialized JSON from disk rendered without sanitization in report subcommand

- **File:** `crates/rskim-bench/src/main.rs:356-358`, `crates/rskim-bench/src/report.rs:48-49`
- **Confidence:** 60%
- **Description:** The `report` subcommand reads a JSON file from a user-supplied `--input` path, deserialises it into `BenchResult`, then renders fields like `repo_url` directly into Markdown output (`format!("## Repo: {repo_name}\n\n")`). If a crafted JSON file contained malicious Markdown/HTML content in the `repo_url` field, it would pass through unescaped into the output. This is low-risk because: (a) the tool outputs to stdout, not a web context, (b) Markdown injection is only exploitable if the output is later rendered in a browser without sanitization, and (c) the input file is a local path the operator explicitly provides. This is an informational finding about defence-in-depth, not a practical vulnerability in the current usage model.
- **Suggestion:** No action required. If the report output is ever served in a web UI, sanitize string fields from deserialized data before rendering into HTML.

### informational — Recursive AST traversal without explicit depth bound

- **File:** `crates/rskim-bench/src/extract/rust_lang.rs:99-109`, `crates/rskim-bench/src/extract/python.rs:76-85`, `crates/rskim-bench/src/extract/go.rs:76-85`
- **Confidence:** 65%
- **Description:** The `walk_node` functions in all three language extractors recurse into child nodes without an explicit depth limit. Tree-sitter ASTs for deeply nested source files could theoretically cause stack overflow. However, tree-sitter grammars naturally bound nesting depth based on language grammar rules, and the upstream `rskim-research` crate limits accepted file size to 100 KiB (`MAX_FILE_SIZE`), which effectively bounds AST depth. Additionally, stack overflow in Rust results in a process abort (not memory corruption), so exploitability is nil. This is noted only as a defence-in-depth observation.
- **Suggestion:** If future changes remove the file size limit or accept untrusted file sources, consider adding a `max_depth` parameter to `walk_node`. Not needed currently.

## Issues in Your Changes (BLOCKING)

### CRITICAL
(none)

### HIGH
(none)

## Issues in Code You Touched (Should Fix)
(none)

## Pre-existing Issues (Not Blocking)
(none)

## Suggestions (Lower Confidence)

- **Deserialized fields rendered unsanitized** - `crates/rskim-bench/src/report.rs:48` (Confidence: 60%) -- `repo_url` from deserialized JSON is interpolated directly into Markdown output; not exploitable in current CLI context but worth noting for defence-in-depth if output is ever browser-rendered.
- **Recursive AST walk without depth bound** - `crates/rskim-bench/src/extract/rust_lang.rs:99` (Confidence: 65%) -- tree-sitter walk_node recurses without explicit limit; bounded in practice by 100 KiB file size limit and grammar structure.

## Positive Security Observations

The following patterns were reviewed and found to be well-implemented:

1. **No `unsafe` code** -- The entire crate contains zero `unsafe` blocks.
2. **No hardcoded secrets** -- No API keys, tokens, passwords, or credentials anywhere in the crate.
3. **No command injection surface** -- The crate delegates git operations to `rskim-research`, which uses `std::process::Command` with explicit args (not shell interpolation) and hardens git with `credential.helper=` and `transfer.fsckObjects=true`.
4. **Corpus config validation** -- Upstream `rskim-research::config::validate_repo` enforces HTTPS-only URLs, 40-char hex-only commit SHAs, and allowlisted languages, blocking malicious URL schemes and path traversal via commit strings.
5. **No `unwrap()`/`expect()` in production code** -- Clippy lints `unwrap_used = "deny"` and `expect_used = "deny"` are enforced at the crate level (`Cargo.toml:38-39`), with `#[allow]` only in `#[cfg(test)]` blocks.
6. **Temporary directory usage** -- All index directories use `tempfile::tempdir()` which creates directories in the OS temp path with random names, avoiding symlink attacks and path collisions.
7. **Input validation on BM25F parameters** -- `configs::tuned_8field` validates all parameters via `BM25FConfig::validate()` before returning, preventing NaN/Inf/negative values from propagating.
8. **Bounded iteration** -- The coordinate descent tuner has an explicit `MAX_PASSES = 3` bound, preventing unbounded computation. Candidate arrays are all compile-time constants with fixed sizes.
9. **`publish = false`** -- The crate is marked as non-publishable, preventing accidental release to crates.io.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Security Score**: 9/10
**Recommendation**: APPROVED
