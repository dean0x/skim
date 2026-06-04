# Code Review Summary

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31T13:38
**Review Cycle**: 2 (second pass)

## Merge Recommendation: CHANGES_REQUESTED

This feature introduces AST-level n-gram analysis infrastructure (tree-sitter parsing, document frequency weighting, Rust code generation, validation reporting). The architecture is sound and follows existing patterns consistently. However, **5 blocking issues** must be resolved before merge:

1. **Reliability** (2 HIGH): Counter overflow vulnerabilities in DF tracking
2. **Architecture** (2 HIGH): Code duplication in clone sources and summary reporting
3. **Dependencies** (1 HIGH): Unpinned corpus repos compromise reproducibility

All findings apply ADR-001 (fix noticed issues immediately; never defer to next sprint).

---

## Convergence Status

**Cycle 1 → Cycle 2**: 20 issues fixed, 0 regressions introduced.
- Prior batch-5 fixes (fmt, type alignment, comment injection, underscore collapsing) are verified passing.
- Fresh scan detects 9 new issues (not previously visible).
- **No conflict resolution** needed — reviewers found independent issue categories; all are actionable.

---

## Issue Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW | Total |
|----------|----------|------|--------|-----|-------|
| **Blocking** | 0 | 5 | 1 | 0 | **6** |
| **Should-Fix** | 0 | 0 | 5 | 0 | **5** |
| **Pre-existing** | 0 | 0 | 1 | 1 | **2** |
| **TOTAL** | **0** | **5** | **7** | **1** | **13** |

---

## Blocking Issues

### CRITICAL
(none)

### HIGH

**[Reliability] DF counter overflow in bigram/trigram accumulation** - `crates/rskim-research/src/ast_extract.rs:283, 286`
**Confidence**: 85% (+ overlap from reliability reviewer)
- Problem: Bigram and trigram document-frequency counters use non-saturating `+= 1` while node counters (4 lines above) use `saturating_add`. Counter wraps to 0 on overflow (u32::MAX files), corrupting IDF weights. Inconsistency suggests oversight.
- Fix: Change to `saturating_add` for consistency.
- **Category**: Issues in Your Changes

---

**[Reliability] File count and total-unique-files counters lack saturation** - `crates/rskim-research/src/ast_extract.rs:260, 261`
**Confidence**: 82% (+ overlap from reliability reviewer)
- Problem: `lang_file_count` and `total_unique_files` use `+= 1`, which panics on overflow in debug builds. Asymmetric with node counters (lines 279–280) that saturate. Violates reliability pattern.
- Fix: Use `saturating_add` throughout.
- **Category**: Issues in Your Changes

---

**[Architecture] AstGitCloneSource duplicates GitCloneSource body** - `crates/rskim-research/src/clone.rs:85–97`
**Confidence**: 90%
- Problem: `AstGitCloneSource::fetch_files` is verbatim copy of `GitCloneSource::fetch_files` except final call (`walk_and_load` vs `walk_and_load_ast`). Clone-and-checkout logic (repo name, existence, clone call) duplicated verbatim. DRY violation creates maintenance risk.
- Fix: Extract `ensure_cloned(...)` helper; both impls call it then dispatch to walker. Or parameterize single struct by extension list.
- **Category**: Issues in Your Changes

---

**[Architecture] cmd_ast_run duplicates summary printing instead of reusing ast_validate** - `crates/rskim-research/src/main.rs:450–478`
**Confidence**: 85%
- Problem: After writing table, `cmd_ast_run` manually iterates bigrams, computes error rates, prints summary. Identical logic already exists in `ast_validate::run_ast_validation`. Divergence risk if formula changes. Project convention (per `log_validation_summary`) delegates summary to validation module.
- Fix: Extract `log_ast_summary(table)` helper that calls `ast_validate` and prints compact stderr output.
- **Category**: Issues in Your Changes

---

**[Dependencies] 16 corpus repos use commit = "HEAD" (non-reproducible)** - `crates/rskim-research/ast-corpus.toml` (lines 165, 170, 175, 184, 189, 194, 203, 208, 216, 221, 231, 236, 244, 254, 259)
**Confidence**: 85%
- Problem: Existing `corpus.toml` pins all 25 repos to specific commits. New AST corpus uses `commit = "HEAD"` on 16 repos. IDF weight tables generated from non-pinned corpus are non-reproducible — different timestamps produce different results as upstream repos change. Static lookup tables embedded in rskim-core must have reproducible inputs.
- Fix: Pin each repo to specific commit SHA. Run clone once, capture resolved HEAD SHAs, update `ast-corpus.toml`.
- **Category**: Issues in Your Changes

---

### MEDIUM

**[Consistency] Inconsistent PathBuf/Path type usage** - `crates/rskim-research/src/clone.rs:82`, `crates/rskim-research/src/config.rs:85`
**Confidence**: 95%
- Problem: Existing `GitCloneSource` uses imported `PathBuf`; new `AstGitCloneSource` uses `std::path::PathBuf`. Existing `load_corpus_config` uses `&Path`; new `load_ast_corpus_config` uses `&std::path::Path`. Stylistically inconsistent within same file.
- Fix: Use imported `PathBuf`/`Path` to match existing pattern.
- **Category**: Issues in Your Changes

---

## Should-Fix Issues

(High confidence; not blocking but recommended before merge)

### MEDIUM

**[Testing] No test for walk_and_load_ast with AST extension list** - `crates/rskim-research/src/clone.rs:375`
**Confidence**: 85%
- Problem: `walk_and_load_ast` uses `Some(AST_TARGET_EXTENSIONS)` but no test verifies that the `Some` code path correctly accepts AST extensions and skips `EXCLUDED_EXTENSIONS`. Regression (e.g., exclusion list accidentally applying to AST path) would silently drop Markdown/SQL files.
- Suggestion: Add fixture-based test verifying `.md` files included in `Some` path but excluded in `None` path.
- **Category**: Issues in Your Changes

---

**[Testing] No test for AstGitCloneSource as FileSource trait object** - `crates/rskim-research/src/clone.rs:81–98`
**Confidence**: 82%
- Problem: New `AstGitCloneSource` struct has no trait-object compatibility test. Only tested indirectly through main binary. Regression would have delayed feedback loop.
- Suggestion: Add `fn ast_clone_source_is_trait_object_compatible()` test.
- **Category**: Issues in Your Changes

---

**[Testing] ast_validate percentile tests lack edge-case coverage** - `crates/rskim-research/src/ast_validate.rs:136–142`
**Confidence**: 80%
- Problem: `percentile` function uses `.round()` for index computation. `distribution_stats_correct` test checks p50 only; `distribution_single_value` skips median/p90/p99. Missing coverage at distribution boundaries.
- Suggestion: Extend tests to assert p90 and p99 values explicitly.
- **Category**: Issues in Your Changes

---

**[Testing] ast_codegen tests use contains() instead of structural assertions** - `crates/rskim-research/src/ast_codegen.rs:429–451`
**Confidence**: 82%
- Problem: Tests use `source.contains(...)` to verify generated code. Weak assertions would pass even if output is syntactically invalid Rust. Codegen regression would not be caught until downstream crate fails to compile.
- Suggestion: Add test verifying structural markers (`pub const`, `pub fn`, `#[cfg(test)]`, etc.).
- **Category**: Issues in Code You Touched

---

**[Testing] No test for NaN/Infinity IDF validation** - `crates/rskim-research/src/ast_codegen.rs:55–89`
**Confidence**: 80%
- Problem: `validate_ast_table` checks `!w.idf.is_finite() || w.idf <= 0.0` but only negative IDF is tested. NaN and Infinity paths uncovered. NaN comparisons are subtle (NaN != NaN).
- Suggestion: Add explicit tests for `f32::NAN` and `f32::INFINITY` IDF values.
- **Category**: Issues in Code You Touched

---

## Suggestions (60–79% Confidence)

- **[Performance] Sequential corpus extraction prevents parallelism** - `crates/rskim-research/src/ast_extract.rs:205–309` (85% confidence, but marked APPROVED_WITH_CONDITIONS rather than blocking) — Shared mutable `&mut NodeKindVocabulary` prevents rayon parallelism of file-level AST parsing. Fix: per-thread local vocabularies, merge after parallel phase.

- **[Performance] Redundant sort in `kinds()` after `stabilize()`** - `crates/rskim-research/src/ast_types.rs:253–257` (82%) — Post-`stabilize()` vector is already sorted; sort call is redundant. Skip via flag.

- **[Complexity] cmd_ast_run exceeds 50-line threshold** - `main.rs:374–481` (85% confidence, HIGH severity, reported as BLOCKING in complexity review) — 108 lines mixes orchestration and reporting. Extract `log_ast_summary(...)` helper.

- **[Complexity] extract_ast_ngrams_from_corpus at 105 lines with mixed concerns** - `ast_extract.rs:205–309` (82% confidence, HIGH severity) — Handles progress, grouping, dedup, extraction, aggregation. Extract inner per-language loop.

- **[Rust] Comment lists Markdown as non-tree-sitter language** - `crates/rskim-research/src/ast_extract.rs:77` (90% confidence, HIGH severity) — Comment misleads; Markdown is tree-sitter-enabled. Fix comment.

- **[Rust] AST_VALID_LANGUAGES uses "Sql" but Language::name() returns "SQL"** - `crates/rskim-research/src/config.rs:49` (82% confidence, MEDIUM severity) — Inconsistency between validator and canonical names creates trap for future TOML entries.

---

## Pre-existing Issues (Informational)

- **[Regression] PR description states 44 repos but ast-corpus.toml contains 40** - Confidence: 65% — Documentation/intent mismatch, not code regression. (regression reviewer flagged; all categories evaluated equally.)
- **[Complexity] main.rs file-level complexity approaching 500-line threshold** - Confidence: 80% — Pre-existing; AST pipeline contributes ~50% to the 570-line total. Not blocking.

---

## Summary by Domain

| Domain | CRITICAL | HIGH | MEDIUM | Coverage |
|--------|----------|------|--------|----------|
| **Security** | 0 | 0 | 0 | ✅ 9/10 |
| **Architecture** | 0 | 2 | 0 | ✅ 7/10 |
| **Performance** | 0 | 2 | 1 | ⚠️ 7/10 (parallelism blocker) |
| **Complexity** | 0 | 2 | 0 | ✅ 7/10 |
| **Consistency** | 0 | 0 | 1 | ✅ 9/10 |
| **Regression** | 0 | 0 | 0 | ✅ 9/10 |
| **Testing** | 0 | 0 | 3 | ⚠️ 7/10 (edge cases) |
| **Reliability** | 0 | 2 | 1 | ⚠️ 8/10 (counters) |
| **Rust** | 0 | 1 | 1 | ✅ 9/10 |
| **Dependencies** | 0 | 1 | 0 | ⚠️ 8/10 (reproducibility) |

---

## Action Plan

### Before Merge (Required)

1. **Fix counter overflows** (Reliability) — Lines 260, 261, 283, 286: change `+= 1` to `saturating_add(1)`
2. **Extract DRY clone helper** (Architecture) — Create `ensure_cloned(...)` shared by both FileSource impls
3. **Extract summary logging** (Architecture/Complexity) — Refactor `cmd_ast_run` lines 450–478 into `log_ast_summary(...)`
4. **Pin corpus repos** (Dependencies) — Replace 16 `commit = "HEAD"` entries with specific SHA hashes
5. **Fix path type usage** (Consistency) — Use imported `PathBuf`/`Path` in `clone.rs` and `config.rs`

### Before Merge (Recommended)

6. **Fix Rust comments and names** (Rust) — Update `ast_extract.rs:77` comment; align `AST_VALID_LANGUAGES` with `Language::name()`
7. **Add test coverage** (Testing) — Three tests for `walk_and_load_ast`, `AstGitCloneSource`, and `ast_codegen` structure
8. **Add percentile edge cases** (Testing) — Test NaN/Infinity IDF and p90/p99 distribution values

### Future Optimization (Not Blocking)

- Parallelize AST extraction via per-thread vocabularies (seq bottleneck)
- Optimize sort in `kinds()` post-stabilize
- Consider per-language vocabulary design if cross-language analysis planned

---

## Convergence Notes

**Cycle 1 → Cycle 2 Progress**:
- ✅ All 20 prior-cycle issues fixed (no regressions from batch-5)
- ✅ Fresh scan finds 9 new issues (independent of Cycle 1)
- ✅ No conflicting findings — reviewers operate on distinct concern areas
- ✅ All findings actionable with clear fixes

**Review Quality**: 10 specialized reviewers (security, architecture, performance, complexity, consistency, regression, testing, reliability, Rust, dependencies) converged on same blocking issues through independent analysis. High confidence in findings.

**Merge Viability**: After fixing 5 blocking issues + 1 consistency medium, this feature is ready. Recommended testing additions are valuable for test coverage but not critical for functionality.
