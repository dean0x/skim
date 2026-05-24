# Code Review Summary

**Branch**: feat-176-empirical-sparse-ngram-weights -> main
**Date**: 2026-05-13T09:30
**PR**: #220 (Empirical Sparse Bigram IDF Weights - Wave 0 Foundation)

## Merge Recommendation: CHANGES_REQUESTED

**Rationale**: Three CRITICAL/HIGH blocking issues must be resolved before merge:
1. **Unbounded git subprocess execution** (CRITICAL reliability) — processes can hang indefinitely
2. **`is_border_bigram` overly broad matching** (HIGH across 6 reviewers) — makes border-weighted scoring unreliable
3. **Path traversal in `extract_repo_name`** (HIGH security) — typos in corpus.toml URLs could escape sandbox

These are not speculative edge cases but real defects in a developer-facing tool. The architecture is sound, crate boundaries are clean, and the core IDF/bigram algorithms are solid. Fix these three items and the PR is merge-ready.

---

## Issue Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| **Blocking** (Your Changes) | 1 | 5 | 4 | 0 |
| **Should Fix** (Code You Touched) | 0 | 0 | 6 | 0 |
| **Pre-existing** (Not Blocking) | 0 | 0 | 0 | 0 |

**Total Unique Issues**: 16 (after deduplication across 10 reviewers)

---

## Blocking Issues (Must Fix Before Merge)

### CRITICAL

**Unbounded git subprocess execution in `clone_repo`** — `crates/rskim-research/src/clone.rs:65-113`
- **Reviewers**: reliability (95% confidence)
- **Problem**: Four `git clone` calls spawn with no timeout. Network stalls/hangs block indefinitely. With 25 repos in parallel, a single hung clone starves the thread pool permanently.
- **Impact**: Violates project's reliability principle ("every loop, retry, and resource has an explicit bound"). Running `rskim-research run` could hang forever with no recovery.
- **Fix**: Use `Command::spawn()` + `wait_timeout()` or thread-based timeout wrapper. Add `const CLONE_TIMEOUT = Duration::from_secs(300)`.

---

### HIGH (5 items, confidence boosted across reviewers)

**`is_border_bigram` has overly broad byte-value matching** — `crates/rskim-research/src/validate.rs:76-98`
- **Reviewers**: architecture (85%), performance (80%), testing (88%), reliability (82%), rust (85%), complexity (82%)
- **Confidence**: 87% (6 reviewers flagged the same logic error)
- **Problem**: Line 87-88 condition `window[0] == first2[0] || window[0] == last2[0]` matches ANY bigram whose first byte equals the first/last byte of ANY token. For "fn parse", nearly every bigram gets the 3.5x border multiplier. This defeats the selectivity distinction between uniform and border-weighted strategies.
- **Impact**: Validation reports are misleading — border weighting applies almost universally instead of just at token boundaries. The research tool's core output (the selectivity comparison) becomes unreliable.
- **Fix**: Remove lines 87-88 and rely on `window == first2 || window == last2` exact match, OR rewrite with position-tracking to check actual byte offsets in the query string against token boundary positions.

**Path traversal via `extract_repo_name`** — `crates/rskim-research/src/clone.rs:51-57`
- **Reviewers**: security (85%), architecture (suggestion 70%)
- **Problem**: Extracts repo name from URL via `rsplit('/')` then joins with `corpus_dir.join(&repo_name)`. A URL ending in `..` or containing `..` segments would produce `repo_name = ".."`, escaping the corpus directory. While corpus.toml is developer-controlled, a typo or copy-paste error could cause unintended filesystem writes.
- **Impact**: Potential to write outside the intended sandbox directory.
- **Fix**: Reject path traversal components after extraction. Add: `if name == "." || name == ".." || name.contains('/') || name.contains('\\') { bail!(...) }`

**`compute_idf` produces `NEG_INFINITY` when `total_docs == 0`** — `crates/rskim-research/src/idf.rs:12-14`
- **Reviewers**: reliability (90%)
- **Problem**: No precondition assertion. `compute_idf(df, 0)` produces `f32::NEG_INFINITY`. While this edge case is rare (requires all repos to fail to clone), the result corrupts the weight table and violates the documented contract ("Returns a value >= 1.0").
- **Impact**: Silent corruption path if corpus extraction fails completely.
- **Fix**: Add `assert!(total_docs > 0)` at function entry. Add guard in `compute_weight_table` to return empty vec if `total_docs == 0`.

**No NaN validation on IDF values in `codegen.rs`** — `crates/rskim-research/src/codegen.rs:57-65`
- **Reviewers**: security (62%), reliability (85%)
- **Confidence**: 85% (reliability + security agreement)
- **Problem**: Validation loop checks `w.idf <= 0.0` but `f32::NAN <= 0.0` is always `false` in IEEE 754. NaN values pass validation and get written into generated code.
- **Impact**: NaN in the const table corrupts all downstream binary search lookups and selectivity computations.
- **Fix**: Change condition to `if !w.idf.is_finite() || w.idf <= 0.0 { bail!(...) }`

**Inconsistent workspace `clap` dependency** — `Cargo.toml:41` + `crates/rskim/Cargo.toml:17`
- **Reviewers**: consistency (85%), regression (82%), dependencies (90%)
- **Confidence**: 86% (3 reviewers, high confidence)
- **Problem**: PR adds `clap` to workspace dependencies for `rskim-research`, but existing `rskim` crate still uses inline `clap = { version = "4.5", ... }` instead of `{ workspace = true }`. Creates a split pattern.
- **Impact**: Maintenance risk — future version updates could cause drift between crates.
- **Fix**: Update `crates/rskim/Cargo.toml` line 17 to `clap = { workspace = true }`

---

### MEDIUM (4 blocking items)

**Missing Cargo.toml metadata in `rskim-research`** — `crates/rskim-research/Cargo.toml`
- **Reviewers**: consistency (92%)
- **Problem**: Missing `authors`, `license`, `description`, `repository` fields. All sibling crates include these, even `rskim-search` which is also `publish = false`.
- **Fix**: Add standard metadata to match pattern.

**Const table bloat: 9596-entry `BIGRAM_WEIGHTS` embeds 56 KiB per binary** — `crates/rskim-search/src/weights.rs:15`
- **Reviewers**: performance (82%)
- **Problem**: 350 KiB source file (`weights.rs`) adds significant compile time. The 56 KiB runtime data is reasonable, but the parsing/type-checking cost of 9596 const entries is measurable.
- **Fix**: Consider `include_bytes!` + bytemuck or lazy_static for binary format instead of const array literal. Defer to Wave 1 when real corpus data replaces synthetic data.

**Large generated JSON artifact checked into version control** — `crates/rskim-search/data/bigram_weights.json` (565 KB)
- **Reviewers**: architecture (85%)
- **Problem**: 38K-line JSON is a generated intermediate artifact. Both JSON source and `.rs` target are checked in, creating two representations of the same data that could drift.
- **Fix**: Option A: Treat JSON as build artifact (add to .gitignore). Option B: Add CI check verifying `weights.rs` matches what codegen would produce from the JSON.

---

## Should Fix (High/Medium issues in code you touched)

### HIGH (0 items)

(none flagged in code you touched, only in your changes)

### MEDIUM (6 items)

**Two functions exceed complexity threshold** — Multiple locations
- **Complexity**: `build_weights_rs` (136 lines, single `writeln!` sequence) and `cmd_run` (121 lines, 7 sequential responsibilities)
- **Fix**: Extract into helpers (write_header, write_tests, resolve_corpus_dir, write_weight_table)

**`idf::selectivity` function misnamed and misdocumented** — `crates/rskim-research/src/idf.rs:43-46`
- **Reviewers**: architecture (80%), rust (95%)
- **Problem**: Doc says "Look up IDF weight... Returns None if not in table", but function sums all matching bigrams and returns `f64`. Doc is copy-pasted from a removed single-lookup function.
- **Fix**: Update doc comment to describe cumulative IDF computation, not lookup.

**`is_border_bigram` has zero direct unit tests** — `crates/rskim-research/src/validate.rs:76-98`
- **Reviewers**: testing (88%)
- **Problem**: Function with 3 distinct branches is only tested indirectly. The overly-broad matching (line 87) makes nearly all bigrams border bigrams, but tests pass trivially because border always exceeds uniform.
- **Fix**: Add focused unit tests for interior bigrams, first-2 match, last-2 match cases.

**`higher_idf_bigrams_preferred` test doesn't verify ordering** — `crates/rskim-research/src/validate.rs:246-259`
- **Reviewers**: testing (85%)
- **Problem**: Test named "higher_idf_bigrams_preferred" only checks that selected bigrams exist, not that they're ordered by IDF. The greedy heuristic's core property is untested.
- **Fix**: Assert that selected bigrams are ordered highest-IDF-first, or at minimum include the highest-IDF bigrams.

**`_temp_dir_guard` uninitialized on `Some` branch risks confusion** — `crates/rskim-research/src/main.rs:104-112`
- **Reviewers**: reliability (80%)
- **Problem**: Variable declared but assigned only in `None` arm. While safe, the pattern is fragile and non-obvious.
- **Fix**: Use `let (corpus_dir, _temp_guard) = match corpus_dir { Some(p) => (p, None), None => { let td = tempdir()?; (td.path().to_path_buf(), Some(td)) } };`

**`codegen` missing test for negative IDF values and version == 0 rejection** — `crates/rskim-research/src/codegen.rs:51-65`
- **Reviewers**: testing (85%)
- **Problem**: Error paths (negative IDF, version 0) are specified but not tested.
- **Fix**: Add unit tests exercising both validation paths.

---

## Suggestions (Lower Confidence, 60-79%)

Noted but not blocking. Recommended for future consideration:

| Issue | Location | Confidence | Note |
|-------|----------|------------|------|
| Symlink following in `walk_and_load` | clone.rs:122-124 | 65% | Consider `.follow_links(false)` |
| `covering_set_heuristic` O(n²) coverage check | validate.rs:174 | 80% | Track `remaining_uncovered` counter instead of repeated scan |
| `extract_bigrams_from_corpus` single-threaded | extract.rs:65-115 | 65% | Consider two-pass parallel extraction (research tool, acceptable at current scale) |
| `gen_synthetic.rs` hardcoded language breakdown | gen_synthetic.rs:188-201 | 82% | Compute from actual fixture files instead of hardcoding |
| Weak test assertions (`>= 0.0` instead of `> 0.0`) | validate.rs:278-285 | 82% | Tests should assert strictly positive for uniform selectivity |
| Missing integration tests for command handlers | main.rs:92-286 | 80% | `cmd_codegen` and `cmd_validate` lack integration tests |
| `.gitattributes` for generated files | repo root | 65% | Mark `weights.rs` and `bigram_weights.json` as linguist-generated |

---

## Key Strengths

✅ **Clean crate architecture** — `rskim-research` is properly isolated as `publish = false` with correct dependency graph (depends on `rskim-core`, not `rskim-search`).

✅ **Strong trait-based testing** — `FileSource` trait with `GitCloneSource` and `FixtureSource` enables clean unit tests without network.

✅ **Clippy deny lints enforced** — `unwrap_used`, `expect_used`, `panic` all denied, matching project standards.

✅ **Well-factored modules** — config, clone, extract, idf, codegen, validate each have single clear responsibility.

✅ **34 existing unit tests** — Core algorithms (IDF, bigram encoding, deduplication) well-covered.

✅ **Zero new transitive dependencies** — All 10 deps were already in workspace.

✅ **No regression in main binary** — Release workflow explicitly builds `-p rskim` only.

---

## Risk Summary

| Risk | Severity | Mitigation |
|------|----------|-----------|
| Unbounded git clone hangs | CRITICAL | Add timeout wrapper |
| Border-weighted scoring unreliable | HIGH | Fix positional matching logic |
| Path traversal in URL parsing | HIGH | Reject `..`, `/`, `\` in repo names |
| NaN in weight table | HIGH | Add `is_finite()` check to validation |
| Workspace dependency inconsistency | HIGH | Migrate `rskim` to workspace ref |
| Overly complex functions | MEDIUM | Extract into named helpers |
| Missing test coverage | MEDIUM | Add focused unit tests for `is_border_bigram` and error paths |
| Large generated files | MEDIUM | Consider binary format for Wave 1 |

---

## Action Plan

**Before Merge (Required)**:
1. Fix CRITICAL timeout in `clone_repo` — add `wait_timeout` wrapper with 300s limit
2. Fix HIGH logic error in `is_border_bigram` — remove byte-value matching, use positional comparison or keep only exact 2-byte match
3. Fix HIGH path traversal in `extract_repo_name` — reject `..` and path separators
4. Fix HIGH NaN guard in `codegen` — change condition to `!w.idf.is_finite() || w.idf <= 0.0`
5. Fix HIGH clap dependency split — update `rskim/Cargo.toml` to use workspace ref
6. Fix HIGH precondition in `compute_idf` — add `assert!(total_docs > 0)`

**Strongly Recommended (Before Merge)**:
7. Add unit tests for `is_border_bigram` boundary conditions
8. Update `selectivity` doc comment to match actual behavior
9. Add Cargo.toml metadata to `rskim-research`

**Nice-to-Have (Can Follow in Next PR)**:
10. Refactor `build_weights_rs` and `cmd_run` for readability
11. Add integration tests for `cmd_codegen` and `cmd_validate`
12. Fix weak test assertions (`>= 0.0` → `> 0.0`)
13. Add `.gitattributes` for generated files

---

## Confidence Scoring

| Category | Score | Notes |
|----------|-------|-------|
| CRITICAL blocking | 95% | Unbounded subprocess is unambiguous |
| `is_border_bigram` logic | 87% | 6 reviewers independently flagged |
| Path traversal | 85% | Security reviewer + architecture |
| NaN handling | 85% | Reliability + security agreement |
| Clap dependency | 86% | 3 reviewers, identical findings |
| Overall recommendation | 92% | High confidence CHANGES_REQUESTED is appropriate |

---

## Files Affected

**New/Modified**:
- `crates/rskim-research/` (entire new crate)
- `crates/rskim-search/src/weights.rs` (9,659 lines, generated)
- `crates/rskim-search/data/bigram_weights.json` (565 KB, generated)
- `Cargo.toml` (workspace additions)
- `crates/rskim/Cargo.toml` (clap dependency to be updated)

**No breaking changes to public API of published crates** — all changes additive.

---

## Next Steps

1. Author responds with fixes for the 6 CRITICAL/HIGH items + recommended tests
2. Schedule follow-up review to verify fixes
3. Merge once all blocking issues resolved
4. Plan Wave 1 work (real corpus data, binary weight format optimization, production integration)
