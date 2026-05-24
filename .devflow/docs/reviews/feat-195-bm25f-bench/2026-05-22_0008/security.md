# Security Review Report

**Branch**: feat/195-bm25f-bench -> main
**Date**: 2026-05-22

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

- **Unbounded recursive AST walk could stack overflow on pathological input** - `crates/rskim-bench/src/extract/mod.rs:90-111` (Confidence: 65%) -- The `walk_nodes` function recurses on AST depth with no explicit bound. tree-sitter grammars produce bounded trees for well-formed code, and the 100 KiB MAX_FILE_SIZE cap in `clone.rs` limits practical depth. However, a maliciously crafted source file (e.g., deeply nested expressions) could theoretically produce a deep enough AST to overflow the stack. This is mitigated by the corpus being pinned to known commits with validated SHA hashes, making adversarial input unlikely. Consider converting to an iterative traversal or adding a depth guard if the corpus is ever opened to untrusted input.

- **Deserialized JSON input from disk not size-bounded** - `crates/rskim-bench/src/main.rs:460-463` (Confidence: 62%) -- `run_report` reads an entire file into memory via `std::fs::read_to_string` and deserializes it with `serde_json::from_str`. A very large crafted JSON file could cause excessive memory allocation. This is a developer tool (not a service), so the attack surface is effectively zero -- the user supplies their own files. If this tool were ever exposed to untrusted input, a size check before deserialization would be appropriate.

- **`git checkout` with user-controlled commit SHA has limited blast radius** - `crates/rskim-research/src/clone.rs:151-154,176-179` (Confidence: 60%) -- The commit SHA is validated as exactly 40 hex characters in `config.rs:58`, which prevents command injection. However, the `git checkout` calls do not use `--` to separate the commit argument from potential file paths. In practice this is safe because a 40-char hex string cannot collide with git options, but `--` is a defensive best practice. This code is pre-existing (not changed in this PR).

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Security Score**: 9/10
**Recommendation**: APPROVED
