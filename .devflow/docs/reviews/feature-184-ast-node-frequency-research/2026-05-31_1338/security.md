# Security Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31T13:38

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

(none)

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Security Score**: 9/10
**Recommendation**: APPROVED

## Analysis Notes

This PR adds AST-level n-gram analysis infrastructure to the `rskim-research` crate -- a build-time research/codegen tool, not a production runtime component. The security posture is strong across all categories examined:

**Input Validation (OWASP A03 - Injection)**
- The `validate_ast_repo` function properly validates all config inputs: HTTPS-only URLs, commit format restricted to "HEAD" or 40-char hex SHA, and language values validated against a strict allowlist. This matches the existing `validate_repo` pattern.
- The `commit` parameter (including the newly accepted "HEAD" literal) is passed to git subprocesses via `std::process::Command::args()`, which does not invoke a shell -- no shell injection vector exists.
- `extract_repo_name` already guards against path traversal (`..`, `.`, `/`, `\`).

**Resource Exhaustion / DoS Defenses**
- `MAX_AST_DEPTH` (500) prevents stack overflow from pathological AST inputs.
- `MAX_AST_NODES` (100,000) caps traversal per file.
- `MAX_FILE_SIZE` (100 KiB) limits memory consumption per file.
- `MAX_TRIGRAMS_PER_FILE` (50,000) bounds HashSet memory growth.
- `NodeKindVocabulary::get_or_insert` asserts against u16 overflow (65,535 kinds) -- prevents silent truncation to 0 which would corrupt DF maps.

**Subprocess Security**
- Git clone commands include `credential.helper=` (suppress credential prompts) and `transfer.fsckObjects=true` (reject corrupted/malicious objects) -- both pre-existing hardening.
- All git subprocesses have a 300-second timeout with SIGKILL enforcement.

**Data Integrity**
- SHA-256 content deduplication prevents double-counting in DF maps.
- Binary file detection (null-byte probe in first 8 KiB) prevents binary data from entering the parser.
- UTF-8 validation on all file content before processing.

**Codegen Safety**
- `validate_ast_table` rejects version 0, empty vocabulary, and non-finite/non-positive IDF values before generating Rust source code.
- `lang_to_ident` sanitizes language names to ASCII-only identifiers with debug assertions validating the output.
- Generated code uses `writeln!` with Rust's debug-format quoting (`{:?}`) for string literals, preventing injection into generated source files.

**Score rationale**: 9/10 rather than 10/10 because the crate reads and processes untrusted source code from third-party repositories (the corpus), which is inherently a higher-risk context than typical application code. The existing defenses (size limits, depth limits, binary detection, tree-sitter's error tolerance) are appropriate and comprehensive for a research tool.
