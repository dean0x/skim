# Code Review Summary

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31_1532
**Cycle**: 3 (prior resolution: 18 issues all fixed)

## Merge Recommendation: CHANGES_REQUESTED

The feature is architecturally sound with strong reliability and regression safeguards, but **7 actionable blocking issues** (5 HIGH, 2 MEDIUM) must be resolved before merge. All findings represent genuine code correctness or quality concerns, not pre-existing debt. The most critical finding (HIGH) is a security injection vulnerability in codegen that affects release builds.

---

## Convergence Status

This is cycle 3 review following comprehensive prior resolution (cycle 2: 18 issues, all fixed). The current batch introduces fresh blocking issues from 5 of the 10 reviewers:

- **Security** (1 HIGH): codegen injection vulnerability
- **Performance** (1 HIGH): cross-language dedup scope
- **Complexity** (1 HIGH, 2 MEDIUM): large orchestrator functions and duplication
- **Architecture** (2 MEDIUM): orchestration patterns and sort contracts
- **Testing** (1 HIGH, 2 MEDIUM): missing integration tests and test gaps
- **Rust** (1 HIGH, 2 MEDIUM): recursive stack risk, comment convention, missing `#[must_use]`
- **Consistency** (2 MEDIUM): config comment style and validation gaps
- **Regression**, **Reliability**, **Dependencies**: All APPROVED

No findings converge across multiple reviewers (no double-coverage dedup required). Issues are distributed across concerns.

---

## Issue Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 5 | 2 | - |
| Should Fix | - | 0 | 7 | - |
| Pre-existing | - | - | 0 | 0 |

**Total Issues**: 14 blocking issues (7 must fix before merge, 7 should fix while here)

---

## Blocking Issues

### CRITICAL

(none)

### HIGH - Must Fix Before Merge

#### 1. Code Injection via `lang_to_ident` Debug Assertion (Security)
**File**: `crates/rskim-research/src/ast_codegen.rs:177-190`
**Confidence**: 85%
**Impact**: Release-mode vulnerability allows code injection through crafted `ast_weights.json`

**Problem**: Uses `debug_assert!` (compiled away in release) instead of `assert!` for identifier validation. A manually crafted language name in `ast_weights.json` could inject arbitrary Rust code into generated `ast_weights.rs`. While upstream config validation constrains this during `ast-run`, the intermediate JSON is not re-validated before codegen.

**Fix**: Replace both `debug_assert!` blocks with explicit `anyhow::bail!`:
```rust
if !result.chars().next().map(|c| c.is_ascii_alphabetic() || c == '_').unwrap_or(false) {
    anyhow::bail!(
        "lang_to_ident produced an empty or non-identifier-starting string for input: {lang:?}"
    );
}
if !result.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
    anyhow::bail!(
        "lang_to_ident produced a non-identifier character for input: {lang:?}"
    );
}
```

#### 2. Per-Language Deduplication Misses Cross-Language Duplicates (Performance)
**File**: `crates/rskim-research/src/ast_extract.rs:225`
**Confidence**: 82%
**Impact**: Redundant SHA-256 hashing and AST extraction for polyglot files (e.g., .h files in both C and Cpp groups)

**Problem**: `process_language_files` creates fresh `seen_hashes` per language. If a file appears in multiple language groups, it's hashed and processed twice. With 44 repos (many polyglot), this wastes both hashing and AST extraction time. Corpus-level `total_files` / `deduplicated_files` counts are also inaccurate.

**Fix**: Hoist `seen_hashes` to corpus level before the language loop and pass it down:
```rust
let mut global_seen_hashes: HashSet<[u8; 32]> = HashSet::new();
// In language loop:
fn process_language_files(
    // ... existing params ...
    global_seen: &mut HashSet<[u8; 32]>,
) -> LangProcessResult {
    // Use global_seen instead of local seen_hashes
}
```

#### 3. `cmd_ast_run` Exceeds Function Length (Complexity)
**File**: `crates/rskim-research/src/main.rs:387-465`
**Confidence**: 85%
**Impact**: Large orchestrator harder to maintain; 79 lines, 6 concerns

**Problem**: Spans 79 lines with 5 parameters, orchestrating config loading, file fetching, vocabulary creation, extraction, stabilization, re-keying, IDF computation (2 n-gram types), table assembly, serialization, and logging. Exceeds 50-line critical threshold and makes pipeline modification harder.

**Fix**: Extract the pipeline into a dedicated function `build_ast_weight_table(files, threshold, collect_trigrams)` that encapsulates extract → stabilize → rekey → IDF → assemble sequence. Command handler becomes: load config, fetch files, build table, write output (30 lines).

#### 4. `walk_tree` Recursive with MAX_AST_DEPTH=500 Stack Overflow Risk (Rust)
**File**: `crates/rskim-research/src/ast_extract.rs:142`
**Confidence**: 82%
**Impact**: Stack frame scaling; currently safe within bounds (24-32 KB at 500 depth) but deviates from documented iterative pattern

**Problem**: Recursive function with 500 depth limit. Current stack usage is ~48-64 bytes per frame, totaling 24-32 KB (well within 8 MB default stack). However, code is vulnerable to future changes to depth limits or execution contexts (e.g., if called from rayon thread pools). FEATURE_KNOWLEDGE documents "Iterative TreeCursor traversal" as the expected pattern.

**Fix**: Convert to iterative loop using manual stack `Vec<(Option<NodeKindId>, Option<NodeKindId>)>` to track parent/grandparent IDs. Use `TreeCursor::goto_first_child`, `goto_next_sibling`, `goto_parent` for stateful traversal.

#### 5. No Integration Tests for AST Subcommand Handlers (Testing)
**File**: `crates/rskim-research/src/main.rs:387-551`
**Confidence**: 85%
**Impact**: Orchestration logic untested; config loading, vocabulary stabilization, IDF computation sequencing, JSON serialization, codegen output all only manually validated

**Problem**: Three new AST subcommand handlers (`cmd_ast_run`, `cmd_ast_codegen`, `cmd_ast_validate`) and shared helpers have zero test coverage. While individual modules are well-tested, the integration/orchestration logic (correct sequencing of stabilize → rekey → IDF, default path resolution, `write_json_table` label strings) is only validated by manual runs. Existing lexical handlers also lack tests (pre-existing), but new handlers introduce the same gap in fresh code (applies ADR-001).

**Fix**: Add at least one integration test per AST subcommand with fixture data:
```rust
#[test]
fn ast_pipeline_end_to_end_with_fixtures() {
    // 1. Load fixture files via FixtureSource or walk_and_load_ast
    // 2. extract_ast_ngrams_from_corpus
    // 3. stabilize + rekey
    // 4. compute IDF weights
    // 5. Build AstWeightTable, serialize to JSON, deserialize back
    // 6. Assert vocabulary, weights, and stats are non-empty and consistent
}
```

---

### MEDIUM - Must Fix Before Merge

#### 6. `cmd_ast_run` Orchestration in main.rs (Architecture)
**File**: `crates/rskim-research/src/main.rs:387-465`
**Confidence**: 82%
**Impact**: Maintenance burden as new n-gram types are added; duplication with lexical pipeline

**Problem**: Direct orchestration of full extract-stabilize-rekey-IDF-serialize pipeline in ~80 lines of inline procedural code inside `main.rs`. Lexical `cmd_run` has same pattern at ~50 lines. Both embed domain logic in CLI binary rather than exposing composable library entry point. Future consumers (benchmark harness, test, second binary) must duplicate stabilize-then-rekey-then-compute-IDF sequencing (the remap bug in commit 605203a already demonstrated this error-proneness).

**Fix**: Extract `pub fn build_ast_weight_table(files: &[SourceFile], collect_trigrams: bool, threshold: f32) -> AstWeightTable` in library (e.g., `ast_extract.rs` or new `ast_pipeline.rs`) encapsulating extract → stabilize → rekey → IDF → assemble sequence. Parallels how well-structured CLI tools keep `main.rs` as thin dispatch layer. (Note: overlaps with issue #3 but distinct concern — this is about API reusability, #3 is about code length.)

#### 7. `AstWeightTable.bigram_weights` Sorted Inconsistently Across Modules (Architecture)
**File**: `crates/rskim-research/src/ast_idf.rs:54`, `ast_codegen.rs:204`, `ast_validate.rs:60-61`
**Confidence**: 83%
**Impact**: Implicit ordering contracts; silent breakage if sort order changes or steps reorder

**Problem**: `compute_ast_bigram_weights` sorts weights by IDF descending. `write_language_bigram_arrays` re-sorts by bigram key ascending. `run_ast_validation` assumes IDF-descending order to pick top-20 via `.take(20)`. Weight data flows through multiple modules with implicit ordering assumptions. If future change moves sort or inserts step between IDF and codegen, binary-search tables could silently break or validation report shows wrong "top" entries.

**Fix**: Document sort contract explicitly at type level. Add `#[doc]` comment on `AstBigramWeight`:
```rust
/// Weight entry for an AST bigram.
/// **Contract**: When returned from `compute_ast_bigram_weights`, sorted by IDF descending (highest weight first).
/// Do not re-sort unless explicitly documented. Validation reports assume this ordering for `.take(20)` top-N.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstBigramWeight { ... }
```

---

## Should-Fix Issues (Lower Priority)

### MEDIUM (Can fix in follow-up but recommended while touching related code)

#### 1. String Cloning in `stabilize` (Performance)
**File**: `crates/rskim-research/src/ast_types.rs:242-244`
**Confidence**: 80%
**Issue**: Rebuilds `kind_to_id` with O(n) clones after zero-copy rebuild of `id_to_kind`. ~1,400 small-string clones across 14 languages, architecturally inconsistent with zero-copy intent.
**Fix**: Accept as negligible (offline tool) or build `kind_to_id` from sorted indices without cloning.

#### 2. `vocab.kinds()` Allocates Unnecessary Vec (Performance)
**File**: `crates/rskim-research/src/main.rs:455`
**Confidence**: 85%
**Issue**: `vocab.kinds().into_iter().map(str::to_string).collect()` allocates `Vec<&str>` then maps to `Vec<String>`. Intermediate allocation unnecessary.
**Fix**: Return slice directly: `pub fn kinds(&self) -> &[String] { &self.id_to_kind }`

#### 3. Linear Extension Search in `walk_and_load` (Performance)
**File**: `crates/rskim-research/src/clone.rs:321`
**Confidence**: 80%
**Issue**: `allowed.contains(&ext.as_str())` is O(k) where k=21, called once per filesystem entry. Dominated by I/O but worth noting.
**Fix**: Convert extension list to `HashSet<&str>` before walk loop. Low priority but easy win.

#### 4. Comment Style Inconsistency (Consistency)
**File**: `crates/rskim-research/ast-corpus.toml:15-17`
**Confidence**: 85%
**Issue**: `corpus.toml` uses `# ---- Rust ----` (4 dashes). `ast-corpus.toml` uses 65-char box-drawing style. Cosmetic but inconsistent.
**Fix**: Adopt same style as `corpus.toml`:
```toml
# ---- Rust (5 repos -- reused from corpus.toml) ----
```

#### 5. Missing Bigrams Empty Check (Consistency)
**File**: `crates/rskim-research/src/ast_codegen.rs:56`
**Confidence**: 82%
**Issue**: Lexical `validate_ast_table` checks `table.weights.is_empty()`. AST version checks vocabulary but not `bigram_weights`. Asymmetrical validation.
**Fix**: Add empty-bigrams check:
```rust
if table.bigram_weights.is_empty() {
    anyhow::bail!("AST weight table has no bigram weights");
}
```

#### 6. `get_or_insert()` Missing `#[must_use]` (Consistency)
**File**: `crates/rskim-research/src/ast_types.rs:151`
**Confidence**: 82%
**Issue**: Return value (`NodeKindId`) always used by callers, but annotation missing. Lexical pipeline has consistent `#[must_use]` on similar encode/decode functions.
**Fix**: Add `#[must_use]` attribute (note: side effect of insertion may make this debatable, but callers always need return value).

#### 7. `SAFETY:` Comment Misuse (Rust)
**File**: `crates/rskim-research/src/ast_types.rs:234`
**Confidence**: 88%
**Issue**: `SAFETY:` prefix reserved for `unsafe` blocks by Rust convention. Comment describes invariant, not unsafe justification.
**Fix**: Change to `// INVARIANT: sorted_indices is a permutation...`

#### 8. Main.rs Approaches File Length Warning (Complexity)
**File**: `crates/rskim-research/src/main.rs` (562 lines)
**Confidence**: 80%
**Issue**: Grown to 562 lines, all production code. Contains 16 functions for two distinct pipelines (lexical and AST). Past 500-line warning threshold.
**Fix**: Extract AST subcommand handlers into `ast_commands.rs` module. Keeps CLI entry point thin.

#### 9. Structural Duplication in Code Generation (Complexity)
**File**: `crates/rskim-research/src/ast_codegen.rs:195,229,263,297`
**Confidence**: 82%
**Issue**: Four pairs of functions (bigram/trigram arrays and lookups) differ only in type parameters and string literals. ~90% identical code.
**Fix**: Use generic helper trait/closure to parameterize type-specific parts (key type, format width, field accessor). Code-generation context makes this more tolerable than business logic, but still maintenance burden.

#### 10. Structural Duplication Between Validators (Complexity)
**File**: `crates/rskim-research/src/config.rs:99,133`
**Confidence**: 80%
**Issue**: `validate_repo` and `validate_ast_repo` share identical URL validation and nearly identical commit validation (only `"HEAD"` acceptance differs).
**Fix**: Extract common validation into helper that takes valid-languages list and commit validator function.

### Test Gap Issues

#### 11. `top_bigrams` Test Asserts Sample Size Not Cap (Testing)
**File**: `crates/rskim-research/src/ast_validate.rs:318-323`
**Confidence**: 82%
**Issue**: Test name promises "capped at 20" but never verifies cap is applied — only checks inputs < 20 pass through.
**Fix**: Add test case with >20 bigrams, assert output is exactly 20.

#### 12. No Test for Bigram Merge-on-Collision (Testing)
**File**: `crates/rskim-research/src/ast_types.rs:96-107`
**Confidence**: 80%
**Issue**: `rekey_bigram_df_map` uses `*entry += count` for collisions, but no test exercises merge behavior. Existing test uses 1:1 remap only.
**Fix**: Add test where two old bigrams remap to same new key, verify counts sum.

#### 13. Percentile Boundary Cases Untested (Testing)
**File**: `crates/rskim-research/src/ast_validate.rs:136-146`
**Confidence**: 82%
**Issue**: No test for `percentile(sorted, 0.0)` or `percentile(sorted, 100.0)` despite these being valid inputs. Edge behavior at array boundaries untested.
**Fix**: Add boundary test cases for p0 and p100.

#### 14. `empty_source_returns_empty_result` Incomplete Assertion (Testing)
**File**: `crates/rskim-research/src/ast_extract.rs:384-390`
**Confidence**: 80%
**Issue**: Checks `bigrams.is_empty()`, `trigrams.is_empty()`, `error_node_count == 0` but not `node_count == 0`. Inconsistent with other empty-input tests.
**Fix**: Add `assert_eq!(result.node_count, 0);`

---

## Recommendations by Priority

### Tier 1 — Critical Security/Correctness (Fix Immediately)
1. Issue #1: Codegen injection vulnerability (HIGH)
2. Issue #2: Cross-language dedup scope (HIGH)
3. Issue #3: `cmd_ast_run` function length (HIGH)
4. Issue #4: Recursive stack risk (HIGH)
5. Issue #5: Missing integration tests (HIGH)

### Tier 2 — Architectural Soundness (Fix Before Merge)
6. Issue #6: Orchestration reusability (MEDIUM)
7. Issue #7: Sort contract documentation (MEDIUM)

### Tier 3 — Code Quality (Recommended in Same PR)
Issues #8-10 (code organization), #11-14 (test coverage)

---

## Summary by Reviewer

| Reviewer | Score | Recommendation | Key Finding |
|----------|-------|-----------------|------------|
| Security | 8/10 | CHANGES_REQUESTED | Codegen injection (HIGH) |
| Architecture | 8/10 | APPROVED_WITH_CONDITIONS | Orchestration duplication (MEDIUM) |
| Performance | 8/10 | APPROVED_WITH_CONDITIONS | Cross-lang dedup (HIGH) |
| Complexity | 7/10 | CHANGES_REQUESTED | Function length, duplication |
| Consistency | 8/10 | APPROVED_WITH_CONDITIONS | Config style, validation gaps |
| Regression | 9/10 | APPROVED | Clean refactoring, no breaks |
| Testing | 7/10 | CHANGES_REQUESTED | Missing integration tests |
| Reliability | 9/10 | APPROVED | Strong bounds, assertions |
| Rust | 8/10 | CHANGES_REQUESTED | Recursive risk, comment style |
| Dependencies | 10/10 | APPROVED | Clean, justified additions |

**Consensus**: 7 blockers (5 HIGH, 2 MEDIUM) + 7 should-fix (all MEDIUM). Addresses all 3 pre-existing issues (security injection, orchestration maintenance, sort contracts) identified as needing resolution (applies ADR-001).

---

## Action Plan

1. **Fix codegen injection vulnerability** — Replace `debug_assert!` with `anyhow::bail!` (1 file, 2 locations)
2. **Lift dedup scope to corpus level** — Hoist `seen_hashes` to corpus before language loop (1 file, 1 function signature)
3. **Extract pipeline into library function** — Create `build_ast_weight_table(files, threshold, collect_trigrams)` and call from `cmd_ast_run` (1-2 files)
4. **Convert recursive walk to iterative** — Replace `walk_tree` recursion with manual stack + `TreeCursor` navigation (1 file, 1 function)
5. **Add integration tests** — Test full AST pipeline with fixture data, round-trip serialization (1 file, 3 tests)
6. **Document sort contracts** — Add `#[doc]` comment to `AstBigramWeight` clarifying IDF-descending ordering (1 file)
7. **Fix security validation** — Add empty-bigrams check in `validate_ast_table` (1 file, 1 check)

All 7 blockers are fixable in 1-2 hours. Test additions and refactors can follow if time permits.
