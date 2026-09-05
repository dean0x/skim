# Wave-4 Search: Test-Hardening Plan

> **PLAN-ONLY — no gap-test code landed this pass.**
> Implementing these gap tests and the GCI.1 git-guard fixes is tracked separately.
> This document records feasibility notes, Evaluator-verifiable Acceptance Criteria,
> and three-dimension Test Plans for every identified gap.

Date: 2026-06-22
Author: wave4-completion-docs-buildlock-tests (Coder agent)
Related tickets: #200 (composite ranking), #201 (result formatting), #202 (full CLI
integration), #283 (single-node queries), #289 (temporal auto-refresh), #290 (AST bench)

---

## Design Notes (Locked This Session)

- `--weights` applies only to the `--blast-radius` composite ranking path. The 3 extended
  signals (import_graph, dir_proximity, structural_coupling) are fixed at 0.0 until a
  relative-lift benchmark confirms positive marginal lift (ADR-003).
- `acquire_bounded` is private to `build_lock.rs`; `acquire` is the only public surface.
  The bounded variant is exposed to unit tests via `super::acquire_bounded` (Rust's
  `pub(super)` is not used — test module `use super::acquire_bounded` already works
  because the `#[cfg(test)]` module is a child of build_lock).
- The lock error reports `deadline_after` via `{:?}` (e.g. "120s", "60ms") — truthful at
  both production and sub-second test scales.
- Single-cochange-source behavioral AC (G200.3): partner set == injected blast_radius_paths.
  A soft `include_str!` grep tripwire (assert no second `CochangeMatrixReader`) supplements
  the behavioral check without hardening to implementation internals.
- #201 re-parse bound (G201.5): `results.len() <= limit` proxy is lowest priority;
  a true re-parse counter via `&AtomicUsize` is optional follow-up if ever wanted.
- The non-discriminating `sync_rejects_over_capacity` (storage_tests.rs:343) must be
  replaced (G289.1) — the current test passes even if the DB is partially corrupted
  because it does not check prior-row survival or META_GIT_HEAD integrity.
- Internal `#202` provenance comments in source files are intentionally left untouched;
  only user-facing prose (CLAUDE.md, README.md, help text) removes the `#202` limitation
  language now that `--ast` + temporal is fully implemented.
- Performance dimension: `ast_query_bench` stays informational; no hard perf gate added
  per locked decision.

---

## GCI.1 — Git-less CI Robustness (Two Hard-Fail Helpers)

### Problem

Two test helpers call git subprocesses unconditionally. When git is unavailable (or has
no configured identity) the test panics instead of SKIPping. This causes spurious CI
failures in sandboxed environments.

**Affected helpers:**

1. `staleness_tests.rs:631` — `create_real_git_repo_for_staleness`
   (callers: lines ~693, ~772)
2. `temporal_build_tests.rs:334/356` — `init_git_repo` / `create_real_git_repo`

### Fix Pattern

Adopt the `git_parser_tests.rs` Option/`git_available()` skip pattern:
- Run `git --version` (or `git init` as a probe)
- If unavailable, `eprintln!("SKIP ...: git not available")` and `return`
- Preserve all positive assertions when git IS present

### Feasibility

Straightforward. The pattern is already used in `temporal_tests.rs` (lines ~1046-1050).
Risk: very low — only changes error handling in test helpers, not behavior.

### Acceptance Criterion (Evaluator-verifiable)

AC-GCI.1: In a CI environment where `git --version` fails, all tests in
`staleness_tests.rs` and `temporal_build_tests.rs` that call the affected helpers must
exit with a "SKIP" eprintln and status `ok` (not `FAILED`). In an environment where git
is available, the positive assertions must all pass unchanged.

### Test Plan

| Dimension | Test |
|-----------|------|
| Functionality | Verify SKIP + return when git unavailable (mock via PATH override) |
| API contract | Positive assertions preserved when git IS present (existing tests) |
| Performance | N/A — guard is a single `Command::output()` call |

---

## G200 — Composite Ranking (#200)

### G200.1 — Bad `--weights` → non-zero exit + actionable error

**Feasibility:** Straightforward assert_cmd test. The CLI already validates weights via
`parse_weights_flag` → `validate()`. Need to confirm stderr carries `--weights` and a
reason token.

**AC (Evaluator-verifiable):**
- `skim search --weights 1,2,-3` exits non-zero (status.failure())
- stderr contains `--weights` AND a reason token (e.g. "negative", "invalid")
- stdout is empty
- output does not contain "panicked"

**Test Plan (Home: cli_search_compose.rs)**

| Dimension | Test |
|-----------|------|
| Functionality | bad weights → non-zero exit; good weights → exit 0 |
| API contract | stderr contains `--weights` + reason; stdout empty; no panic |
| Performance | N/A |

### G200.2 — `parse_flags` CLI coverage for `--weights`

**Feasibility:** Unit test on the existing `parse_flags` function in `mod.rs`. The `=`
form (`--weights=0.5,0.3,0.2`), space form, missing value, and invalid prefix should all
be exercised. This is parse-layer coverage; no binary needed.

**AC (Evaluator-verifiable):**
- `--weights 0.5,0.3,0.2` → `weights == Some([0.5, 0.3, 0.2])`
- `--weights=0.5,0.3,0.2` → same
- Omission → `weights == None`
- `--weights: ` (invalid prefix) → parse error
- Extended signals stay 0.0 regardless of `--weights`

**Test Plan (Home: mod.rs parse_flags test module)**

| Dimension | Test |
|-----------|------|
| Functionality | Valid parses; `None` on omission |
| API contract | Extended signals untouched; `=` form handled |
| Performance | N/A |

### G200.3 — Composite partners from `temporal::resolve_blast_radius_*` only

**Feasibility:** Medium. Requires injecting a synthetic `blast_radius_paths` set and
asserting the partner set matches exactly. The soft grep tripwire (no second
`CochangeMatrixReader` in the call graph) supplements the behavioral check.

**AC (Evaluator-verifiable):**
- Inject blast_radius_paths = {A, B}; assert partner set == {A, B}
- `grep -rn "CochangeMatrixReader"` in the composite query call path finds exactly one
  instance (the `temporal::resolve_blast_radius_*` call site)

**Test Plan (Home: query_tests.rs)**

| Dimension | Test |
|-----------|------|
| Functionality | partner set == injected blast_radius_paths |
| API contract | soft include_str! grep tripwire (no second CochangeMatrixReader) |
| Performance | N/A |

### G200.4 — `run_blast_radius_composite_query` with non-None weights

**Feasibility:** Medium. Requires a fixture with a co-change-only file (not in lexical
results). Lexical-heavy vs temporal-heavy weights must produce different orderings.

**AC (Evaluator-verifiable):**
- Lexical-heavy weights (0.8,0.1,0.1): co-change-only file ranks lower
- Temporal-heavy weights (0.2,0.2,0.6): co-change-only file ranks higher
- RRF score in `[0.0, 1.0]` range for all results
- `co_change_partner` field present in results

**Test Plan (Home: query_tests.rs)**

| Dimension | Test |
|-----------|------|
| Functionality | reordering under different weight profiles |
| API contract | RRF score range; co_change_partner field present |
| Performance | informational (record numbers; no hard gate) |

---

## G201 — Result Formatting (#201, AST Path, Git-less)

### G201.1 — Exact `:line` (+snippet) for known-line fixture

**Feasibility:** Low complexity. Add one small single-pattern fixture file with a known
line number. Assert 1-indexed, never 0.

**AC (Evaluator-verifiable):**
- Given fixture `fixture_try_catch.rs` with a match-arm at line 5:
  output contains `:5` (1-indexed) and a snippet substring
- Output never contains `:0`

**Test Plan (Home: integration test against fixture)**

| Dimension | Test |
|-----------|------|
| Functionality | exact line recovered; snippet present |
| API contract | 1-indexed (never 0) |
| Performance | N/A |

### G201.2 — Two-run byte-identical AST output (text + JSON)

**Feasibility:** Low. Run the same AST query twice on the same index; byte-compare stdout.

**AC (Evaluator-verifiable):**
- Text output: run1 == run2 (byte-identical)
- JSON output: `--json` run1 == run2 (byte-identical)
- Stability holds under `--limit`

**Test Plan (Home: cli_search_compose.rs)**

| Dimension | Test |
|-----------|------|
| Functionality | deterministic output across runs |
| API contract | JSON parses on both runs |
| Performance | N/A |

### G201.3 — `--limit K` on AST-only path returns exactly K

**Feasibility:** Low. Need a fixture with N > K files matching the pattern.

**AC (Evaluator-verifiable):**
- `--ast try-catch --limit 1` → exactly 1 result
- `--ast try-catch --limit 1 --hot` → 1 result, the hottest (from fixture with known
  hotspot ordering)

**Test Plan (Home: cli_search_compose.rs)**

| Dimension | Test |
|-----------|------|
| Functionality | exactly K results returned |
| API contract | with --hot, hottest file is the single result |
| Performance | N/A |

### G201.4 — Compound text+AST → `layers_matched` ordering

**Feasibility:** Medium. Requires a JSON output assertion on `layers_matched` field.

**AC (Evaluator-verifiable):**
- `skim search "error" --ast try-catch --json` → every result has
  `"layers_matched": ["lexical", "ast"]` (exact, ordered)
- Pure lexical result omits the `layers_matched` key (or it equals `["lexical"]`)

**Test Plan (Home: cli_search_compose.rs primary + query_tests.rs supplement)**

| Dimension | Test |
|-----------|------|
| Functionality | layers_matched present and ordered correctly |
| API contract | pure lexical omits or uses single-element list |
| Performance | N/A |

### G201.5 — Re-parse bound proxy (lowest priority)

**Feasibility:** Low complexity but low value. Proxy: `results.len() <= limit`.
No internal counter seam exists; defer a true `&AtomicUsize` counter to follow-up.

**AC (Evaluator-verifiable):**
- For any `--limit K`, `results.len() <= K`

**Test Plan (Home: cli_search_compose.rs)**

| Dimension | Test |
|-----------|------|
| Functionality | result count never exceeds limit |
| API contract | proxy (len check); no internal counter |
| Performance | N/A |

---

## G202 — Full CLI Integration (#202)

### G202.1 — Hottest file beyond `--limit K` survives

**Feasibility:** Medium. Requires hermetic `enrich_ast_results` seam with synthetic
results (no git). `Cold` must invert ordering.

**AC (Evaluator-verifiable):**
- With `--hot --limit 1`: the returned file has the highest hotspot score in the fixture
- With `--cold --limit 1`: the returned file has the lowest hotspot score
- Lookup-count field matches documented fallback (AC-P1), NOT a planned value

**Design note:** AC-P1 lookup-count = accepted documented fallback, not planned value.
This is a hermetic seam test — no git subprocess needed.

**Test Plan (Home: hermetic unit via enrich_ast_results seam)**

| Dimension | Test |
|-----------|------|
| Functionality | hottest/coldest file selected under --limit |
| API contract | lookup-count = documented fallback value |
| Performance | N/A |

---

## G289 — Temporal Auto-Refresh (#289)

### G289.1 — Replace non-discriminating `sync_rejects_over_capacity`

**Feasibility:** Medium. The current test (`storage_tests.rs:343`) asserts `Err(CapacityExceeded)`
but does NOT check prior-row survival or META_GIT_HEAD integrity. A partial write could
pass the current test.

**The capacity check precedes the transaction at `storage_ops.rs:569-579`**, so the fix
is atomic (rows are not modified if capacity is exceeded).

**AC (Evaluator-verifiable):**
- Pre-seed rows R1..RN + META_GIT_HEAD = "abc"
- Force over-capacity `sync`
- Returns `Err(CapacityExceeded)`
- R1..RN still present (no rows deleted)
- row count unchanged
- META_GIT_HEAD still == "abc" (no partial write)

**Test Plan (Home: storage_tests.rs — replace test at :343)**

| Dimension | Test |
|-----------|------|
| Functionality | Err(CapacityExceeded) returned |
| API contract | prior rows + META_GIT_HEAD survive unchanged (no partial write) |
| Performance | N/A |

---

## Performance Dimension (Locked Decision)

- `ast_query_bench` (criterion) stays **informational**: the Tester checklist records
  "run bench, record numbers" as a NON-BLOCKING item.
- Strengthen existing `ast_reextracted` count-proxy (#290): touch one file →
  `reextracted == 1 < file_count`. This is already partially in place.
- AC13 size-bound from #290 remains as-is.
- **No hard perf gate** is added this wave.

---

## Optional Follow-up

- True re-parse counter via `&AtomicUsize` in the re-parse path (G201.5 upgrade).
- Promote GCI.1 to blocking if CI sandboxing becomes the default environment.
