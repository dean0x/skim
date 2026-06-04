# Security Review Report

**Branch**: feature/187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01
**Snyk SAST**: 0 issues found (full project scan, all severities)

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

## Detailed Analysis

### Bounds Guards (DoS / Resource Exhaustion)

The primary security surface for this PR is resource exhaustion from malicious or pathological input. The implementation handles this well:

1. **MAX_FILE_SIZE (100 KiB)** at `linearize.rs:50` -- Files exceeding 100 KiB return `Ok(LinearizeResult::default())` before any parsing occurs. This prevents memory amplification from large files reaching tree-sitter. Confidence: 95% -- correctly applied.

2. **MAX_AST_DEPTH (500)** centralized at `AstWalkConfig::DEFAULT_MAX_DEPTH` (`ast_walk.rs:72`) -- Prevents stack-equivalent exhaustion from deeply nested input. The bounds check at `ast_walk.rs:228` (`self.depth >= self.config.max_depth`) correctly skips subtrees rather than terminating the entire traversal, ensuring sibling branches are still processed. Confidence: 95%.

3. **MAX_AST_NODES (100,000)** centralized at `AstWalkConfig::DEFAULT_MAX_NODES` (`ast_walk.rs:78`) -- Caps total nodes yielded to prevent CPU exhaustion on wide/deep ASTs. The check at `ast_walk.rs:228` (`self.node_count >= self.config.max_nodes`) is correct and uses `saturating_add` at `ast_walk.rs:239` to prevent u32 overflow. Confidence: 95%.

4. **Vec pre-allocation cap** at `linearize.rs:238-241` -- `Vec::with_capacity` is capped via `.min(AstWalkConfig::DEFAULT_MAX_NODES as usize)` to prevent a malicious `descendant_count()` from causing excessive allocation. Confidence: 95%.

5. **Ancestor vec lazy growth** at `ast_extract.rs:137` -- Starts at capacity 64, grows only on demand via `resize()` at `ast_extract.rs:148-149`. This avoids pre-allocating 501 entries per file, protecting against memory waste in corpus-level extraction. Confidence: 90%.

### Integer Safety

1. **u32 saturating arithmetic** -- `ast_walk.rs:187` uses `saturating_add(1)` for depth increments, and `ast_walk.rs:239-241` uses `saturating_add(1)` for node/error counters. No overflow possible. Confidence: 95%.

2. **u32 -> u16 depth truncation** at `linearize.rs:256-257` -- Uses `item.depth.min(u32::from(u16::MAX)) as u16` to saturate rather than truncate. Since max_depth is 500 (well within u16 range), this is defense-in-depth. Confidence: 95%.

3. **kind_id u16 overflow guard** at `linearize.rs:155-158` -- `u16::try_from(kind_id)` correctly handles the (currently unreachable) case where a grammar has more than 65,535 kinds. Confidence: 95%.

4. **vocab_idx cast** at `linearize.rs:163` -- `vocab_idx as u16` is safe because `NODE_KIND_VOCABULARY.len() == 1740`, well within u16::MAX. The comment documents this assumption. Confidence: 90%.

### Input Validation

1. **Binary-like input** -- The test at `linearize_tests.rs:416-422` confirms that control characters (\x00\x01\x02\x03) are handled gracefully (tree-sitter produces ERROR nodes, no panic). Confidence: 90%.

2. **UTF-8 multibyte** -- The test at `linearize_tests.rs:400-406` confirms non-ASCII identifiers do not cause panics. Confidence: 90%.

3. **Empty/whitespace input** -- Tests at `linearize_tests.rs:120-128` and `linearize_tests.rs:409-413` confirm graceful handling. Confidence: 95%.

### LazyLock Initialization Safety

The `LANG_MAPS` static at `linearize.rs:108` uses `LazyLock` which is thread-safe by design (std::sync::LazyLock guarantees at-most-once initialization with synchronization). Grammar load failures during initialization are handled gracefully via `continue` (the language is simply omitted from the map). No panic path exists in the initialization closure. Confidence: 95%.

### Error Handling

1. **Grammar load errors** surface as `SearchError::Ast` at `linearize.rs:212` -- an unrecoverable configuration error, not a security issue. Parse errors produce empty results at `linearize.rs:217-218`. This separation is correct: file-level parse failures are not errors, grammar-level failures are. Confidence: 95%.

2. **No unwrap/expect in production code** -- The `#[deny(clippy::unwrap_used)]` lint at the crate level (`Cargo.toml:38`) prevents panics in non-test code. All `unwrap`/`expect` usage is correctly gated behind `#[cfg(test)]` or `#![allow(...)]` in test modules. Confidence: 95%.

### Supply Chain

1. **tree-sitter dependency** added to `rskim-search/Cargo.toml` at line 20 -- Uses workspace version pinning (consistent with the existing tree-sitter dependency in rskim-core). No new third-party crate is introduced beyond what the workspace already uses. Confidence: 95%.

2. **criterion dev-dependency** added at line 31 -- Dev-only dependency for benchmarks, no production exposure. Confidence: 95%.

### What Elevates This to 9/10 Rather Than 10/10

The only observation preventing a perfect score is not a finding but a design note: the `linearize_source` function accepts arbitrary `&str` from callers. Currently all callers pass file content that has already been read from disk (inherently bounded by filesystem limits and the MAX_FILE_SIZE guard). If a future caller were to pass network-sourced content directly, the existing guards would still protect against resource exhaustion -- but there is no explicit documentation at the public API boundary about trust assumptions. This is a minor hardening opportunity, not a vulnerability. (Confidence: 65% -- below reporting threshold, noted here for completeness only.)

### Decisions Context

- **ADR-001** (fix all noticed issues immediately): No security issues were found that require fixing. All bounds guards, overflow protections, and error handling patterns are correctly implemented. applies ADR-001 -- nothing to defer.
- **PF-002** (classifying findings as pre-existing to skip resolution): No findings in any category, so no risk of improper classification. avoids PF-002.
