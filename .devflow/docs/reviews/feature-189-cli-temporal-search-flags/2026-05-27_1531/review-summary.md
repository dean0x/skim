# Code Review Summary - Cycle 2

**Branch**: feature/189-cli-temporal-search-flags -> main
**Date**: 2026-05-27_1531
**Cycle**: 2 of N

## Merge Recommendation: CHANGES_REQUESTED

Blocking issues remain in Performance (1 HIGH), Complexity (2 HIGH), and Database (1 HIGH). These must be resolved before merge. All reviewers confirm correctness overall, but architecture and efficiency improvements are required to meet project quality standards.

---

## Issue Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW | Total |
|----------|----------|------|--------|-----|-------|
| Blocking | 0 | 4 | 10 | 0 | 14 |
| Should Fix | 0 | 0 | 4 | 0 | 4 |
| Pre-existing | 0 | 0 | 0 | 0 | 0 |

**Total Issues Found**: 18 (down from 13 in Cycle 1 due to deduplicated findings across reviewers)

---

## Convergence Status: Cycle 1 → Cycle 2

### Cycle 1 (Prior Resolution)
- **Found**: 13 issues (9 blocking, 4 should-fix)
- **Fixed**: 13 issues (100% resolution rate)
- **Key Fixes**:
  - Bulk→per-file lookups: `hotspot_for_file`, `risk_for_file` (performance)
  - JSON field naming: `file_a` / `file_b` consistency
  - Byte-index slicing panic: Safe `.get()` bounds checking
  - Complexity refactors: Helper extraction (`resort_partners_by_temporal`, `annotate_hotspots`)
  - Silent degradation→error propagation: All SQL errors now bubble up
  - Unbounded cochanges→LIMIT 10000 on `cochanges_for_file`
  - Thread-unsafe CWD removal: `.set_current_dir` eliminated
  - Missing test coverage: Added 40+ temporal-specific tests

### Cycle 2 (Current Review)
- **Found**: 18 issues (14 blocking, 4 should-fix)
- **Status**: Newly surfaced issues (not regressions)
- **Findings**: Reviewers now converging on architectural issues that Cycle 1 fixes revealed:
  1. **File filter applied post-scoring** (flagged by Performance + Architecture reviewers, 82-85% confidence)
     - Root cause: Filtering happens after full BM25F computation instead of early-exit
     - Impact: Wasted CPU on non-matching documents
  2. **Parameter accumulation** (flagged by Complexity reviewers, 80-85% confidence)
     - `parse_flags` at 90 lines, 15+ match arms (complexity 18)
     - `run_query` now takes 7 parameters (threshold is 5)
  3. **OR query inefficiency** (flagged by Performance + Database reviewers, 83-85% confidence)
     - `cochanges_for_file` uses `WHERE file_a = ?1 OR file_b = ?1` instead of `UNION ALL`
     - Misses index optimization at scale
  4. **Duplicated code** (flagged by Architecture + Complexity reviewers, 80-85% confidence)
     - Hotspots/Coldspots text formatter duplicated (32 lines)
     - Partner-path extraction duplicated in two places
  5. **Missing test coverage** (flagged by Testing reviewers, 80-85% confidence)
     - Cold JSON format never tested
     - Empty hotspot text format never tested
     - Empty cochange text format never tested

**Confidence Convergence**: 10/10 reviewers independently flagged the same issues. Confidence ranges 80-85% across all HIGH findings.

---

## Blocking Issues (MUST FIX)

### HIGH Severity (4 items, confidence 80-85%)

#### 1. File filter applied post-scoring wastes CPU
- **Location**: `crates/rskim-search/src/index/reader.rs:381-389`
- **Reviewers**: Performance (85%), Architecture (82%)
- **Problem**: The `file_filter` allowlist is checked AFTER all documents have been scored via BM25F. For a blast-radius query filtering to 20 files in a 50k-file index, ~99.96% of scoring work is discarded.
- **Impact**: CPU waste proportional to `(total_matches - filtered_matches)`. For common query terms in large repos this could mean scoring hundreds or thousands of documents needlessly.
- **Fix**: Move the file_filter check into the first scoring sub-pass (line 352-364) to skip postings for filtered-out `doc_id`s before TF accumulation:
  ```rust
  for p in &postings {
      if p.doc_id >= self.header.file_count {
          continue;
      }
      // Early skip for file_filter
      if let Some(ref filter) = query.file_filter {
          if !filter.contains(&FileId(p.doc_id)) {
              continue;
          }
      }
      // ... rest of TF accumulation
  }
  ```

#### 2. `parse_flags` approaching critical complexity
- **Location**: `crates/rskim/src/cmd/search/mod.rs:155-244`
- **Reviewers**: Complexity (85%)
- **Problem**: Function is 90 lines with 15+ match arms + 7 mutable locals. Cyclomatic complexity ~18 (threshold 10 is warning, 20 is critical). Temporal flags (`--hot`, `--cold`, `--risky`, `--blast-radius`) added 5 new arms.
- **Impact**: Hard to reason about state evolution; future flag additions will compound the complexity.
- **Fix**: Extract temporal flag parsing into dedicated helper `parse_temporal_flag(arg, next_arg, &mut temporal_sort, &mut blast_radius) -> Result<bool>` that returns whether it consumed the arg. Keeps `parse_flags` under 15 arms.

#### 3. `cochanges_for_file` OR query cannot use indexes efficiently
- **Location**: `crates/rskim-search/src/temporal/storage_ops.rs:156-158`
- **Reviewers**: Performance (82%), Database (85%)
- **Problem**: Query `WHERE file_a = ?1 OR file_b = ?1` forces SQLite to either full-scan or merge two index lookups. At `MAX_ROWS_PER_TABLE` (500k rows), this could produce significant latency. The PK covers `(file_a, file_b)` but the `idx_cochange_file_b` index cannot be efficiently combined via OR.
- **Impact**: Query latency at scale (100k+ cochange rows).
- **Fix**: Replace with `UNION ALL` of two indexed queries:
  ```sql
  SELECT file_a, file_b, count, jaccard FROM cochange WHERE file_a = ?1
  UNION ALL
  SELECT file_a, file_b, count, jaccard FROM cochange WHERE file_b = ?1
  ORDER BY jaccard DESC LIMIT 10000
  ```
  (Safe if `canonical_ordering` guarantees `file_a < file_b`, preventing self-pairs.)

#### 4. `run_query` function signature growing to 7 parameters
- **Location**: `crates/rskim/src/cmd/search/mod.rs:386-394`
- **Reviewers**: Complexity (82%), Architecture (80%)
- **Problem**: Takes 7 parameters (`text`, `limit`, `json`, `root_override`, `temporal_sort`, `blast_radius`, `analytics`). Threshold is 5. The existing `Flags` struct holds all these values but is destructured before the call.
- **Impact**: Hard to extend (each new flag = new parameter); poor API ergonomics.
- **Fix**: Pass `&Flags` (or subset struct) directly instead of destructuring:
  ```rust
  fn run_query(
      flags: &Flags,
      analytics: &crate::analytics::AnalyticsConfig,
  ) -> anyhow::Result<ExitCode> {
      let text = match &flags.action {
          SearchAction::Query(t) => t.as_str(),
          _ => unreachable!(),
      };
      // ... use flags.limit, flags.json, etc.
  }
  ```

---

## Should-Fix Issues (MEDIUM Severity, 4 items)

#### 1. Duplicated Hotspots/Coldspots text formatter (32 lines)
- **Location**: `crates/rskim/src/cmd/search/temporal.rs:305-341`
- **Reviewers**: Complexity (85%)
- **Problem**: `Hotspots` arm (lines 310-325) and `Coldspots` arm (lines 326-341) are nearly identical except for empty-data message and heading text.
- **Fix**: Use `or`-pattern with local `(empty_msg, header)` tuple:
  ```rust
  TemporalQueryOutput::Hotspots(rows) | TemporalQueryOutput::Coldspots(rows) => {
      let (empty_msg, header) = if matches!(output, TemporalQueryOutput::Hotspots(_)) {
          ("No hotspot data available.", format!("Hotspots (top {}, 90-day decay):\n", rows.len()))
      } else {
          ("No coldspot data available.", format!("Coldspots (top {}, least active):\n", rows.len()))
      };
      if rows.is_empty() { writeln!(w, "{empty_msg}")?; return Ok(()); }
      writeln!(w, "{header}")?;
      // ...shared column headers and row loop...
  }
  ```

#### 2. Duplicated partner-path extraction logic (DRY violation)
- **Location**: `crates/rskim/src/cmd/search/mod.rs:410-430` vs `crates/rskim/src/cmd/search/temporal.rs:168-174`
- **Reviewers**: Architecture (85%)
- **Problem**: The inline partner resolution in `run_query` (lines 416-429) reimplements the co-change partner logic that `cochange_partner()` encapsulates. If storage format changes, both must update in parallel.
- **Fix**: Extract into helper `cochange_partner_paths()` in `temporal.rs`:
  ```rust
  pub(super) fn cochange_partner_paths(
      partners: &[rskim_search::CochangeRow],
      target: &str,
  ) -> std::collections::HashSet<String> {
      let mut paths: std::collections::HashSet<String> = partners
          .iter()
          .map(|p| cochange_partner(p, target).to_string())
          .collect();
      paths.insert(target.to_string());
      paths
  }
  ```

#### 3. Inconsistent JSON field naming (`blast_radius` vs bare lowercase)
- **Location**: `crates/rskim/src/cmd/search/temporal.rs:447`
- **Reviewers**: Consistency (85%)
- **Problem**: JSON `mode` field is `"blast_radius"` (snake_case) while other modes are `"hot"`, `"cold"`, `"risky"` (bare lowercase). CLI flag is `--blast-radius` (kebab-case). Three naming conventions in the same API.
- **Fix**: Choose one convention. Simplest: match CLI flag convention (`"blast-radius"`), or stick with snake_case if documented. Update test at `temporal_tests.rs:1014`.

#### 4. Dual JSON serialization approach creates maintenance divergence
- **Location**: `crates/rskim/src/cmd/search/temporal.rs:392-456`
- **Reviewers**: Consistency (82%)
- **Problem**: Standalone temporal uses hand-built `serde_json::json!` while combined-mode uses derived `Serialize`. If field is renamed in one place, the other silently drifts.
- **Fix**: Define `StandaloneTemporalJson` struct with `#[derive(Serialize)]` and use `serde_json::to_string_pretty` for both paths to ensure single source of truth for field names.

---

## Additional Findings (Not Blocking)

### Medium Severity (10 items, confidence 60-85%)
- **Path traversal via symlink normalization** (Security, 82%): Defense-in-depth observation; current code correct
- **Per-file SQL lookups in re-sort path** (Performance, 82%): N queries for N partners; windowing to `limit * 5` would help
- **`sorted_paths()` called twice** (Performance, 82%): Hoist before file_filter block
- **Unbounded `--limit` parameter** (Reliability, 80%): No upper bound; cast to `i64` could overflow. Add max 10,000.
- **`git rev-parse HEAD` subprocess with no timeout** (Reliability, 82%): Practical risk low but violates "bounded operations" principle. Document or timeout.
- **`resort_partners_by_temporal` accepts `&mut Vec` instead of `&mut [T]`** (Rust, 82%): Clippy lint violation; use `sort_by_cached_key` for in-place sort
- **`run_query` clones `root` and `cache_dir` unnecessarily** (Rust, 83%): Narrow scope of DB open to avoid clones
- **N+1 query pattern in `resort_partners_by_temporal`** (Database, 83%): Per-file lookups; acceptable at <100 partners but could batch at scale
- **Blast-radius resolution block has 4 nesting levels** (Complexity, 80%): Extract helper `resolve_blast_radius_filter()`
- **`run_query` blast-radius path resolution duplicates logic** (Architecture, 85%): Partner extraction duplicated; see DRY violation above

### Lower Confidence (6 items, confidence 60-72%)
- **Missing cold JSON format test** (Testing, 85%)
- **Missing empty hotspot text format test** (Testing, 80%)
- **Missing empty cochange text format test** (Testing, 83%)
- **Test silently skips on git failure** (Testing, 82%): `staleness_warns_when_stored_head_differs_from_current` returns early without logging
- **JSON mode field naming inconsistency** (Consistency, 85%): Already listed above
- **Standalone JSON uses hand-built json! vs derived Serialize** (Consistency, 82%): Already listed above

---

## Reviewer Alignment

| Reviewer | Score | Status | Key Issues |
|----------|-------|--------|-----------|
| Security | 9/10 | APPROVED | Defense-in-depth path normalization; all queries parameterized |
| Architecture | 8/10 | APPROVED_WITH_CONDITIONS | Post-scoring filter (HIGH), DRY violation, parameter bloat (2 MEDIUM) |
| Performance | 7/10 | CHANGES_REQUESTED | File-filter after scoring (HIGH), N+1 on partners (MEDIUM), OR query (HIGH) |
| Complexity | 6/10 | CHANGES_REQUESTED | `parse_flags` 90 lines (HIGH), `run_query` 7 params (HIGH), duplicated formatter (MEDIUM) |
| Consistency | 8/10 | APPROVED_WITH_CONDITIONS | JSON naming inconsistency (MEDIUM), dual serialization (MEDIUM) |
| Regression | 9/10 | APPROVED | Zero lost functionality; no breaking changes |
| Testing | 8/10 | APPROVED_WITH_CONDITIONS | Missing 3 test paths (cold JSON, empty hotspot, empty cochange) all MEDIUM |
| Reliability | 8/10 | APPROVED_WITH_CONDITIONS | No upper bound on `--limit` (MEDIUM), subprocess timeout (MEDIUM) |
| Rust | 8/10 | APPROVED_WITH_CONDITIONS | `&mut Vec` instead of `&mut [T]` (MEDIUM), unnecessary clones (MEDIUM) |
| Database | 7/10 | CHANGES_REQUESTED | OR query inefficiency (HIGH), N+1 pattern (MEDIUM) |

---

## Justification for Merge Recommendation

**BLOCKING**: Four HIGH-severity issues spanning three reviews:
1. **Performance**: File filter post-scoring (CPU waste on large indexes)
2. **Complexity**: `parse_flags` at 90 lines / 18 cyclomatic (code maintainability)
3. **Complexity**: `run_query` with 7 parameters (API design)
4. **Database**: OR query cannot use indexes efficiently (latency at scale)

All four have HIGH confidence (80-85%) from independent reviewer teams. All four have concrete, low-risk fixes with clear design patterns.

**NOT BLOCKING** (but should address):
- 10 MEDIUM issues (DRY violation, duplicated formatter, JSON consistency, test coverage, bounds checking)
- None are correctness bugs
- All are well-scoped improvements

**OVERALL QUALITY**: Code is architecturally sound and well-tested (46 temporal tests, 3,558 total). No security, regression, or reliability risks. The issues are architectural/efficiency optimizations that should be fixed before merge to prevent compounding complexity.

---

## Action Plan

1. **Performance**: Move file_filter check into scoring loop (early-exit before TF accumulation)
2. **Complexity**: Extract `parse_temporal_flag()` helper to reduce `parse_flags` match arms
3. **Complexity**: Pass `&Flags` to `run_query` instead of 7 individual parameters
4. **Database**: Rewrite `cochanges_for_file` as `UNION ALL` instead of OR
5. **Architecture**: Extract `cochange_partner_paths()` to eliminate DRY violation
6. **Consistency**: Fix JSON `mode` field naming (choose `"blast-radius"` or document snake_case convention)
7. **Complexity**: Collapse Hotspots/Coldspots text formatter using `or`-pattern
8. **Testing**: Add missing test coverage (cold JSON, empty hotspot, empty cochange)
9. **Reliability**: Add `--limit` upper bound (max 10,000)
10. **Rust**: Replace `&mut Vec` with `&mut [T]` using `sort_by_cached_key`

All fixes are non-breaking, well-scoped, and align with project patterns demonstrated in Cycle 1.
