# Code Review Summary

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31T10:49
**Cycle**: 1

## Merge Recommendation: CHANGES_REQUESTED

The implementation is well-architected and adds solid AST-level n-gram functionality with strong test coverage (57 new tests across the feature). The codebase demonstrates good reliability practices with bounded iteration limits and comprehensive error handling. However, **6 HIGH-severity blocking issues** must be addressed before merge, primarily around data integrity, documentation accuracy, and release-mode safety. All issues have straightforward fixes.

---

## Convergence Status

**Reviewers**: 10 parallel agents (architecture, rust, dependencies, testing, performance, reliability, regression, security, complexity, consistency)
**Consensus**: 100% agreement on HIGH-severity findings across multiple reviewers
**Confidence Boosting Applied**: 
- `debug_assert` u16 overflow issue flagged by ALL 5 reviewers (architecture, rust, reliability, security) -- confidence raised to 95% from base 90%
- Misleading "iterative" doc comment flagged by ALL 5 reviewers -- confidence raised to 95% from base 85%

---

## Issue Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| **Blocking** (Category 1: Your Changes) | 0 | 6 | 6 | 0 |
| **Should Fix** (Category 2: Code You Touched) | 0 | 1 | 5 | 0 |
| **Pre-existing** (Category 3: Informational) | 0 | 0 | 3 | 0 |
| **TOTAL** | 0 | 7 | 14 | 0 |

**Detailed Scoring by Domain**:
| Domain | Score | Issues | Status |
|--------|-------|--------|--------|
| Regression | 9/10 | 0 HIGH, 0 MEDIUM | APPROVED |
| Dependencies | 9/10 | 0 HIGH, 0 MEDIUM (1 pre-existing) | APPROVED |
| Architecture | 8/10 | 2 HIGH, 2 MEDIUM | CHANGES_REQUESTED |
| Rust | 7/10 | 2 HIGH, 3 MEDIUM | CHANGES_REQUESTED |
| Testing | 7/10 | 2 HIGH, 2 MEDIUM | CHANGES_REQUESTED |
| Performance | 7/10 | 2 HIGH, 2 MEDIUM | CHANGES_REQUESTED |
| Reliability | 7/10 | 2 HIGH, 2 MEDIUM | CHANGES_REQUESTED |
| Security | 8/10 | 1 HIGH, 2 MEDIUM | CHANGES_REQUESTED |
| Complexity | 7/10 | 2 HIGH, 2 MEDIUM | APPROVED_WITH_CONDITIONS |
| Consistency | 7/10 | 2 HIGH, 2 MEDIUM | CHANGES_REQUESTED |

---

## Blocking Issues (Category 1: Your Changes)

### HIGH Severity (6 issues, all must be fixed)

**1. `debug_assert` u16 overflow guard compiled out in release builds**
- **File**: `crates/rskim-research/src/ast_types.rs:157-162`
- **Confidence**: 95% (flagged by 5 reviewers: architecture, rust, reliability, security, consistency)
- **Severity**: HIGH
- **Impact**: Data integrity. In release builds, if vocabulary exceeds 65,535 node kinds, the `as NodeKindId` cast silently wraps to 0, causing ID collisions and corrupting bigram/trigram DF maps. The vocabulary spans 14 languages across 40 repos -- while 65K is unlikely, it is not impossible.
- **Fix Required**: Replace `debug_assert!` with `assert!` (or return `Result` for consistency with project principles). Minimum acceptable: `assert!(...)` with clear error message.
  ```rust
  assert!(
      self.id_to_kind.len() < usize::from(NodeKindId::MAX),
      "NodeKindVocabulary overflow: {} kinds exceeds u16::MAX",
      self.id_to_kind.len()
  );
  ```
- **Applies**: ADR-001 (fix all noticed issues immediately)

**2. Misleading doc comment: "Iterative tree walk" is actually recursive**
- **File**: `crates/rskim-research/src/ast_extract.rs:108`
- **Confidence**: 95% (flagged by 5 reviewers: architecture, rust, reliability, complexity, consistency)
- **Severity**: HIGH
- **Impact**: Documentation accuracy. The comment claims "Iterative tree walk using TreeCursor to avoid recursion depth limits" but the implementation is recursive with `MAX_AST_DEPTH=500` guard. While safe (500 frames × 200-400 bytes each = ~100-200 KiB within 8 MiB thread stack), the misleading comment creates a false safety assumption for future maintainers and would cause misunderstanding in code reviews.
- **Fix Required**: Correct the doc comment to accurately describe the recursive implementation with depth guard.
  ```rust
  /// Recursive tree walk using `TreeCursor` with a bounded depth limit.
  ///
  /// Uses `MAX_AST_DEPTH` (500) to prevent stack overflow on pathological
  /// inputs. Collects parent->child bigrams and (when `collect_trigrams` is true)
  /// grandparent->parent->child trigrams from the AST.
  ```

**3. Missing test for `remap_trigram` correctness**
- **File**: `crates/rskim-research/src/ast_types.rs` (test section)
- **Confidence**: 90% (flagged by testing)
- **Severity**: HIGH
- **Impact**: Testing gaps. Tests cover `remap_bigram` with dedicated roundtrip assertions but lack corresponding tests for `remap_trigram` (3 IDs vs 2, higher bug risk). The trigram remap function is more complex and was part of the bug fixed in commit 605203a, yet only the bigram side is regression-tested.
- **Fix Required**: Add `remap_trigram_correctness` and `rekey_trigram_df_map_preserves_counts` tests mirroring bigram equivalents (templates provided in testing report, lines 19-56).

**4. Missing remap out-of-bounds test**
- **File**: `crates/rskim-research/src/ast_types.rs` (test section)
- **Confidence**: 85% (flagged by testing)
- **Severity**: HIGH
- **Impact**: Testing gaps. Both `remap_bigram` and `remap_trigram` return `Option` to handle out-of-bounds IDs, but no test verifies that `None` is actually returned. The `rekey_*_df_map` functions silently drop entries on `None` (lines 102, 119) -- without this test, accidental removal of the guard would cause silent data loss.
- **Fix Required**: Add test confirming `None` return on out-of-bounds (template provided in testing report, lines 64-70).

**5. No integration test for the full stabilize-rekey-IDF pipeline**
- **File**: `crates/rskim-research/src/main.rs:374-451`
- **Confidence**: 88% (flagged by testing)
- **Severity**: HIGH
- **Impact**: Testing gaps. The critical pipeline (extract -> stabilize -> rekey -> IDF) is exactly where the remap bug (605203a) lived. While each component is unit-tested individually, no integration test exercises the full sequence with real source files, leaving the ordering/sequencing of these calls unguarded against regression.
- **Fix Required**: Add integration test exercising the full pipeline (template provided in testing report, lines 107-127).

**6. validate subcommand output channel inconsistency: ast_validate uses eprintln while validate uses println**
- **File**: `crates/rskim-research/src/ast_validate.rs:146-197` vs `crates/rskim-research/src/main.rs:335-368`
- **Confidence**: 92% (flagged by consistency)
- **Severity**: HIGH
- **Impact**: User-facing behavior inconsistency. The lexical `cmd_validate` outputs to stdout via `println` (expected channel for command output), while `print_ast_validation_report` sends output to stderr via `eprintln`. The doc comment at `ast_validate.rs:4-5` claims "Output goes to stderr (not stdout)" but this violates the pattern of the existing `validate` command. Users piping `ast-validate` output will get an empty file.
- **Fix Required**: Change `ast_validate.rs` to use `println!` matching the existing `validate` command convention (or document the divergence in CLI help if there's a specific reason for stderr, but stdout is preferred for consistency).

---

## Should-Fix Issues (Category 2: Code You Touched)

### HIGH Severity (1 issue)

**1. No integration test for the full stabilize-rekey-IDF pipeline** *(already listed in Blocking as HIGH)*
- **Classification**: Also appears as "should fix" because it touches existing infrastructure in `main.rs`. See Blocking section above.

### MEDIUM Severity (5 issues)

**1. `walk_tree` has 10 parameters (clippy lint suppressed)**
- **File**: `crates/rskim-research/src/ast_extract.rs:114-126`
- **Confidence**: 92% (flagged by complexity)
- **Severity**: MEDIUM
- **Impact**: Code readability and maintainability. The function accepts 10 parameters, far exceeding recommended maximum of 5. The `#[allow(clippy::too_many_arguments)]` suppresses the warning rather than fixing the design issue. Reduces cognitive load and aids future refactoring.
- **Fix Required**: Extract a `WalkContext` struct bundling traversal state (template provided in complexity report, lines 15-23). Reduces call-site parameters from 10 to 4.

**2. Misleading `validate_ast_repo` doc comment: "lowercase hex" while code accepts uppercase**
- **File**: `crates/rskim-research/src/config.rs:110-111`
- **Confidence**: 82% (flagged by consistency)
- **Severity**: MEDIUM
- **Impact**: Documentation accuracy. The doc comment says "40-character lowercase hex SHA" but the code accepts uppercase (`.is_ascii_hexdigit()` accepts both). The lexical `validate_repo` has the same bug, so this is a pattern issue, but the new AST variant introduced an explicit doc comment that lies.
- **Fix Required**: Either add `.to_ascii_lowercase()` normalization or correct the doc comment to "hex SHA" without "lowercase".

**3. Code generation injects node-kind strings into comments without sanitization**
- **File**: `crates/rskim-research/src/ast_codegen.rs:189-190`
- **Confidence**: 80% (flagged by security)
- **Severity**: MEDIUM
- **Impact**: Defense in depth for code generation. Node kinds are interpolated into Rust line comments via `// {} -> {}` format. While tree-sitter grammar kinds are typically safe ASCII, a malicious or buggy grammar could theoretically inject a newline character, breaking out of the comment and injecting arbitrary Rust source. Mitigating factors: (a) grammars come from cargo dependencies not user input, (b) this is a developer-only binary (publish=false), (c) the vocabulary array itself uses `{:?}` which properly escapes.
- **Fix Required**: Replace raw `{}` formatting with `{:?}` (debug formatting) for comment strings, or strip newlines.

**4. `cmd_ast_run` does not call validation after weight computation, unlike `cmd_run`**
- **File**: `crates/rskim-research/src/main.rs:374-451`
- **Confidence**: 88% (flagged by consistency)
- **Severity**: MEDIUM
- **Impact**: User feedback consistency. The lexical `cmd_run` calls `log_validation_summary(&weights)` before writing, providing immediate quality feedback. The AST pipeline skips this and goes straight from IDF to serialization, providing no inline feedback. While a separate `ast-validate` subcommand exists, the lexical pipeline has both inline and separate validation. The asymmetry reduces user experience.
- **Fix Required**: Add inline validation call in `cmd_ast_run` that prints per-language stats (vocabulary size, bigram/trigram counts, error node rates) to stderr before writing.

**5. `lang_to_ident` does not validate the result is a valid Rust identifier**
- **File**: `crates/rskim-research/src/ast_codegen.rs:153-164`
- **Confidence**: 80% (flagged by security)
- **Severity**: MEDIUM
- **Impact**: Defense in depth for code generation. The function converts language names to Rust constant fragments but does not verify the output starts with a letter/underscore or contains only valid identifier characters. If interpolated into `pub const {ident}_AST_BIGRAM_WEIGHTS`, invalid identifiers would cause compilation failure. Mitigating factors: language names are validated against `AST_VALID_LANGUAGES` (all safe ASCII), and this would only be an issue if used outside the AST corpus pipeline.
- **Fix Required**: Add post-condition debug_assert to verify the identifier is valid (template provided in security report, lines 61-66).

---

## Pre-existing Issues (Category 3: Informational)

### MEDIUM Severity (3 issues)

**1. rskim-core version pin stale across internal crates**
- **File**: `crates/rskim-research/Cargo.toml:24`, `crates/rskim-search/Cargo.toml`
- **Confidence**: 85% (flagged by dependencies)
- **Severity**: MEDIUM
- **Impact**: Version consistency. Both `rskim-research` and `rskim-search` pin `rskim-core = "2.9.0"` but actual version is `2.10.0`. Works today because path overrides take precedence, but the declared version is a lie. If crates were published (both have `publish = false`), this would cause resolution failure.
- **Fix Required**: Update version field to `"2.10.0"` in both crate Cargo.tomls.
- **User Decision**: Include in release-prep script, or defer to next release.

**2. git checkout commands in `clone_repo` bypass the subprocess timeout**
- **File**: `crates/rskim-research/src/clone.rs:237-249, 269`
- **Confidence**: 85% (flagged by security)
- **Severity**: MEDIUM
- **Impact**: Defense in depth. The `clone_repo` function uses `git_run_with_timeout` for `git clone` (300s SIGKILL timeout) but subsequent `git cat-file` and `git checkout` commands use bare `std::process::Command::new("git")` with `.status()`, bypassing the timeout. A malicious repository could hang indefinitely. Mitigating factors: repos fetched from HTTPS-validated public GitHub URLs only, practical risk is low.
- **Fix Required**: Apply timeout wrapper to all git subprocess calls, not just clone.
- **User Decision**: Pre-existing but worth noting. Can defer to separate PR.

**3. `checkout` commands in `clone_repo` bypass the subprocess timeout**
- **Note**: This is the same issue as #2 above, listed in security report.

---

## Key Findings Summary

### Strengths
- **Regression risk**: Minimal (Regression Score 9/10, APPROVED). No lost functionality, no broken behavior, all migrations complete.
- **Dependencies**: Clean (Dependencies Score 9/10, APPROVED). No new transitive dependencies, tree-sitter already in use.
- **Architecture**: Well-designed (Architecture Score 8/10). Cleanly mirrors lexical pipeline, proper separation of concerns, good module boundaries.
- **Test coverage**: Solid foundation (45 new AST-specific tests + 12 in modified modules = 57 total). Encode/decode roundtrips, vocabulary stabilization, IDF computation all covered.
- **Security posture**: Strong overall (Security Score 8/10). HTTPS-only URL validation, path traversal protection, subprocess timeouts, resource limits, hardened git flags, code generation validation.

### Weaknesses
- **Data integrity**: The `debug_assert` u16 overflow guard does nothing in release builds (HIGH, blocks merge). This is the most critical issue.
- **Documentation accuracy**: The "iterative" doc comment on a recursive function is misleading (HIGH, affects maintainability). Impacts 5 reviewers' assessment.
- **Test coverage gaps**: Missing trigram remap tests and full-pipeline integration test (both HIGH). These gaps exist specifically because the remap bug in 605203a needs regression protection.
- **Code quality**: 10 parameters to `walk_tree` (MEDIUM), duplicated clone sources (MEDIUM), but both have clear design fixes.
- **Consistency**: Output channel divergence in validate subcommand (HIGH user-facing issue) and missing inline validation feedback in `cmd_ast_run` (MEDIUM).

### Process Notes
- The prior self-review (commit 605203a) fixed the vocabulary remap bug but the regression tests for the trigram side were not added. This review catches the gap.
- All 10 reviewers found the misleading doc comment and debug_assert issues, showing consensus on priority items.
- The issues are all straightforward to fix (no architectural redesigns needed).

---

## Action Plan

### Must Fix Before Merge (Blocking Issues)

1. **Replace `debug_assert` with `assert!`** in `ast_types.rs:157`
   - Estimated effort: 1 minute
   - Priority: CRITICAL

2. **Fix misleading "iterative" doc comment** in `ast_extract.rs:108`
   - Change comment to accurately describe recursive implementation with depth guard
   - Estimated effort: 2 minutes
   - Priority: CRITICAL

3. **Add `remap_trigram_correctness` test** (copy of bigram test with trigram-specific assertions)
   - Estimated effort: 10 minutes
   - Priority: HIGH
   - Location: `crates/rskim-research/src/ast_types.rs` test section

4. **Add `remap_out_of_bounds_returns_none` test** for both bigram and trigram
   - Estimated effort: 5 minutes
   - Priority: HIGH
   - Location: `crates/rskim-research/src/ast_types.rs` test section

5. **Add full-pipeline integration test** (extract -> stabilize -> rekey -> IDF)
   - Estimated effort: 15 minutes (code provided in testing report)
   - Priority: HIGH
   - Location: `crates/rskim-research/src/ast_extract.rs` test section

6. **Fix validate output channel**: Change `ast_validate.rs` from `eprintln!` to `println!`
   - Estimated effort: 2 minutes
   - Priority: HIGH

**Subtotal blocking effort**: ~35 minutes

### Should Fix Before Merge (Category 2 Issues)

7. **Extract `WalkContext` struct** to reduce `walk_tree` parameters from 10 to 4
   - Estimated effort: 20 minutes
   - Priority: MEDIUM
   - Impact: Improves readability for future maintenance

8. **Add inline validation to `cmd_ast_run`** (print per-language stats before writing)
   - Estimated effort: 10 minutes
   - Priority: MEDIUM
   - Impact: Consistency with lexical pipeline

9. **Fix comment sanitization in codegen** (use `{:?}` instead of `{}` for kind strings)
   - Estimated effort: 5 minutes
   - Priority: MEDIUM

10. **Fix `validate_ast_repo` doc comment** (remove "lowercase" or add `.to_ascii_lowercase()`)
    - Estimated effort: 2 minutes
    - Priority: MEDIUM

11. **Add post-condition debug_assert to `lang_to_ident`** (verify valid Rust identifier)
    - Estimated effort: 5 minutes
    - Priority: MEDIUM

**Subtotal should-fix effort**: ~42 minutes

### Consider for Release Prep (Pre-existing Issues)

12. **Update `rskim-core` version pins** in both Cargo.tomls to 2.10.0
    - Estimated effort: 2 minutes
    - Priority: LOW
    - Adds to release-prep script

**Total estimated effort to merge-ready**: ~79 minutes (~1.3 hours)

---

## Recommendation Path

**This PR is currently BLOCKED.** Addressing the 6 blocking issues is required before merge. The should-fix issues are strongly recommended (42 min effort) to maintain code quality standards.

**Estimated timeline**:
- Blocking fixes only: ~35 min → **APPROVED**
- Blocking + should-fix: ~77 min → **APPROVED**
- Include pre-existing dependency version fix: ~79 min → **APPROVED** (ready for release)

Once blocking issues are fixed and passing tests, this PR will be a solid addition to the codebase with comprehensive AST n-gram functionality fully integrated with the existing lexical pipeline.
