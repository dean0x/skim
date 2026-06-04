# Code Review Summary

**Branch**: feature-189-cli-temporal-search-flags -> main
**Date**: 2026-05-27_1027
**PR**: #257

## Merge Recommendation: CHANGES_REQUESTED

The PR demonstrates strong fundamentals: well-structured temporal module, comprehensive test coverage (739 lines), proper error handling with Result types, and thoughtful schema migration. However, multiple independent reviewers flagged the same core issues around API misuse (bulk table loads instead of per-file lookups) and error handling (silent degradation on path failures). Additionally, two structural complexity findings and one JSON naming inconsistency require attention before merge.

## Issue Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW | Total |
|----------|----------|------|--------|-----|-------|
| Blocking | 0 | 5 | 4 | 0 | **9** |
| Should Fix | 0 | 0 | 5 | 0 | **5** |
| Pre-existing | 0 | 0 | 2 | 0 | **2** |

## Convergence Status

**High-Confidence Patterns (flagged by 3+ reviewers):**
- Bulk table loads in `apply_temporal_enrichment` and `query_standalone` (Confidence: 86%+) — 4 reviewers flagged
- Silent degradation on `blast_radius` path normalization errors (Confidence: 82%+) — 3 reviewers flagged
- Thread-unsafe `set_current_dir` in tests (Confidence: 80%+) — 2 reviewers flagged

**Unique Findings:**
- Byte-index panic on non-ASCII stored HEAD (Security) — 2 reviewers flagged
- Unbounded `cochanges_for_file` query (Reliability) — 1 reviewer
- JSON field naming inconsistency "limit" vs "total" (Consistency) — 1 reviewer (HIGH, blocking)
- High cyclomatic complexity in `query_standalone` and `apply_temporal_enrichment` (Complexity) — 2 reviewers

## Blocking Issues (CRITICAL/HIGH)

### HIGH: Bulk table loads violate per-file lookup API contract
**Files**: `crates/rskim/src/cmd/search/temporal.rs:466,507,222,247`
**Confidence**: 86% (consolidated from Architecture, Rust, Performance, Reliability)

**Pattern**: Three independent reviewers identified the same pattern: `apply_temporal_enrichment` and the blast-radius re-sort path in `query_standalone` call `db.load_hotspots()` / `db.load_risks()`, which deserialize the entire table (up to 500,000 rows) into memory just to build a HashMap for scoring a handful of results (typically 20).

**Root cause**: The per-file lookup methods (`hotspot_for_file`, `risk_for_file`) were added in this same PR but are not used in these hot paths. The existing feature knowledge explicitly warns: "Avoid `load_hotspots()` / `load_risks()` / `load_cochanges()` for per-file lookups -- those bulk-load the entire table."

**Impact**:
- **Performance**: ~40MB allocation per invocation (500K rows × ~80 bytes each) + O(n) table scan instead of O(k) primary key lookups where k=20 typical results
- **Memory**: Violates the design contract that bulk-load methods are only for top-N queries, not per-file lookups
- **Schema v2 indexes unused**: The new performance indexes on `score` and `risk_score` are not being leveraged

**Fix**: Replace with per-file lookups:
```rust
// In apply_temporal_enrichment (lines 466-505):
for result in results.iter_mut() {
    if let Ok(Some(row)) = db.hotspot_for_file(&result.path) {
        result.temporal = Some(TemporalAnnotation {
            hotspot_score: Some(row.score),
            changes_30d: Some(row.changes_30d),
            changes_90d: Some(row.changes_90d),
            ..Default::default()
        });
    }
}

// In query_standalone blast-radius re-sort (lines 222-267):
// Build score map from per-file lookups instead of loading entire table
let mut partner_scores: Vec<(usize, f64)> = partners
    .iter()
    .enumerate()
    .map(|(i, p)| {
        let partner_path = cochange_partner(p, &normalized);
        let score = db.hotspot_for_file(partner_path)
            .ok()
            .flatten()
            .map(|h| h.score)
            .unwrap_or(0.0);
        (i, score)
    })
    .collect();
```

---

### HIGH: Blast-radius path normalization error silently degrades to unfiltered search
**Files**: `crates/rskim/src/cmd/search/mod.rs:429-431`
**Confidence**: 82% (consolidated from Architecture, Rust, Reliability)

**Pattern**: When `normalize_blast_radius_path` fails (e.g., file not found, outside repo), the error is printed to stderr but `run_query` continues with `blast_radius_paths = None`. The query then executes unfiltered.

**Root cause**: The error handling treats the failure as graceful degradation (appropriate for "DB missing") but applies it to user-input validation failures (inappropriate). The stderr warning is lost in piped/agent workflows.

**Impact**:
- **Correctness**: User explicitly requested `--blast-radius FILE`, but receives unfiltered BM25F results
- **Debuggability**: User may not notice the stderr warning and assume filtering was applied
- **Programmatic consumers**: JSON output contains no field indicating the filter was not applied

**Fix**: Propagate the error instead of silently continuing:
```rust
if let (Some(raw_path), Some(db)) = (blast_radius, &temporal_db) {
    let normalized = temporal::normalize_blast_radius_path(raw_path, &root)?;  // Changed from: match with Err => eprintln
    // Continue with normal flow
}
```

Alternative (if graceful degradation is preferred): add a `"blast_radius_applied": false` field to JSON output so programmatic consumers can detect degradation.

---

### HIGH: JSON field naming inconsistency "limit" vs "total"
**Files**: `crates/rskim/src/cmd/search/temporal.rs:398,417,435`
**Confidence**: 92% (Consistency reviewer)

**Pattern**: The standalone temporal JSON output uses `"limit"` as the JSON key for the actual number of results returned. The existing text-query JSON output uses `"total"` for this same concept.

**Root cause**: Manual JSON construction in `format_temporal_json` instead of serde derive pattern used by `format_json_output`.

**Impact**:
- **API inconsistency**: JSON consumers expecting `"total"` field will fail to parse temporal JSON
- **Field semantics**: `"limit"` is the user-requested limit (e.g., 20); the field value is the actual count returned

**Fix**: Rename all three occurrences:
```rust
serde_json::json!({
    "mode": "hot",
    "total": rows.len(),  // Changed from: "limit"
    "results": results,
})
```

---

### MEDIUM: Target file excluded from blast-radius filter allowlist
**Files**: `crates/rskim/src/cmd/search/mod.rs:417-426`
**Confidence**: 83% (Architecture reviewer)

**Pattern**: When building the `blast_radius_paths` allowlist, the code collects only the *partners* of the target file -- not the target file itself.

**Root cause**: Oversight in logic; the allowlist should include both the target and its partners.

**Impact**: If user runs `skim search "auth" --blast-radius src/auth.rs`, the file `src/auth.rs` is excluded from results even if it matches.

**Fix**:
```rust
let mut paths: std::collections::HashSet<String> = partners
    .iter()
    .map(|p| {
        if p.file_a == normalized { p.file_b.clone() } else { p.file_a.clone() }
    })
    .collect();
paths.insert(normalized.clone());  // Include target file itself
blast_radius_paths = Some(paths);
```

---

### MEDIUM: Unbounded `cochanges_for_file` query has no LIMIT clause
**Files**: `crates/rskim-search/src/temporal/storage_ops.rs:152-174`
**Confidence**: 85% (Reliability reviewer)

**Pattern**: The SQL query `WHERE file_a = ?1 OR file_b = ?1 ORDER BY jaccard DESC` has no LIMIT. While the table is capped at 500,000 rows, a single popular file could be a partner in hundreds of thousands of pairs. The caller clones all results into a `HashSet<String>`.

**Root cause**: All other new read methods have `LIMIT` (top-N methods), but this one was missed.

**Impact**: Unbounded memory allocation possible, though mitigated by table size cap.

**Fix**:
```rust
pub fn cochanges_for_file(&self, path: &str) -> Result<Vec<CochangeRow>> {
    // ... existing code ...
    "SELECT file_a, file_b, count, jaccard FROM cochange \
     WHERE file_a = ?1 OR file_b = ?1 \
     ORDER BY jaccard DESC LIMIT 10000",
    // ...
}
```

---

### MEDIUM: Byte-index string slicing can panic on non-ASCII database content
**Files**: `crates/rskim/src/cmd/search/temporal.rs:136-137`
**Confidence**: 82% (consolidated from Security and Reliability)

**Pattern**: `&stored_head[..stored_head.len().min(7)]` uses byte-index slicing. If stored_head contains multi-byte UTF-8 and the 7-byte boundary falls mid-character, this panics.

**Root cause**: While git SHAs are always hex (ASCII), the `meta` table is a generic key-value store that could contain arbitrary data from corrupted/tampered databases.

**Impact**: Production panic on corrupted database.

**Fix**:
```rust
fn safe_prefix(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        s
    } else {
        let mut end = max_bytes;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

// Usage:
safe_prefix(&stored_head, 7),
safe_prefix(&current_head, 7),
```

---

## Should-Fix Issues (MEDIUM, not blocking)

### MEDIUM: `set_current_dir` in tests causes parallel test flakiness
**Files**: `crates/rskim/src/cmd/search/temporal_tests.rs:55,131`
**Confidence**: 80% (Testing, Rust reviewers)

**Pattern**: Two tests call `std::env::set_current_dir()` which mutates process-global state. With `cargo test` running tests in parallel (default), other tests can observe stale CWD and fail non-deterministically.

**Fix**: Either:
1. Use `#[serial]` from `serial_test` crate to isolate these tests
2. Refactor `normalize_blast_radius_path` to accept optional `cwd` parameter instead of relying on `std::env::current_dir()`
3. Add a comment explaining the risk and why it's acceptable in this test binary

---

### MEDIUM: Empty-table edge cases untested in CLI dispatch layer
**Files**: `crates/rskim/src/cmd/search/temporal_tests.rs` (coverage gap)
**Confidence**: 82% (Testing reviewer)

**Pattern**: `top_hotspots_empty_table_returns_empty` and `top_risks_empty_returns_empty` exist at the DB layer, but no tests verify `query_standalone` with `TemporalSort::Cold` and `TemporalSort::Risky` on empty DB.

**Fix**: Add two tests for empty-table returns with Cold and Risky modes.

---

### MEDIUM: Missing staleness detection test for actual staleness
**Files**: `crates/rskim/src/cmd/search/temporal_tests.rs:153`
**Confidence**: 83% (Testing reviewer)

**Pattern**: Only one staleness test checking "no meta key" path. Missing test for actual stale case where `stored_head != current_head`.

**Fix**: Add test that sets a known `META_GIT_HEAD`, creates a real git repo in TempDir, and asserts the warning message contains the stale HEAD warning.

---

### MEDIUM: `temporal_annotation_tag` helper lacks direct unit tests
**Files**: `crates/rskim/src/cmd/search/query.rs:154`
**Confidence**: 80% (Testing reviewer)

**Pattern**: Function has branching logic (None, hotspot-only, risk-only, both) tested only indirectly. The "both" case never exercised.

**Fix**: Add direct unit test with all four cases.

---

### MEDIUM: Standalone temporal JSON formatters missing tests for Risky and Cochanges
**Files**: `crates/rskim/src/cmd/search/temporal_tests.rs:390`
**Confidence**: 80% (Testing reviewer)

**Pattern**: Only `Hotspots` arm of `format_temporal_json` is tested via `standalone_hot_json_valid`. Risk and Cochanges JSON shapes never validated.

**Fix**: Add `standalone_risky_json_valid` and `standalone_blast_radius_json_valid` tests.

---

## Complexity Issues (Approval Conditions)

### HIGH: `query_standalone` has high cyclomatic complexity from nested dispatch
**Files**: `crates/rskim/src/cmd/search/temporal.rs:207-291` (84 lines)
**Confidence**: 85% (Complexity reviewer)

**Pattern**: Two-level nested match (blast_radius presence + sort mode) with ~10 cyclomatic complexity.

**Impact**: Hard to understand and extend; risk of introducing bugs in sort-specific branches.

**Recommendation**: Extract `resort_partners_by_temporal(partners, sort, normalized, db)` helper to reduce `query_standalone` to ~30 lines.

---

### HIGH: `apply_temporal_enrichment` has duplicated Hot/Cold vs Risky branches
**Files**: `crates/rskim/src/cmd/search/temporal.rs:459-547` (88 lines)
**Confidence**: 82% (Complexity reviewer)

**Pattern**: Two identical-structure 40-line arms with only score-extraction and annotation differences.

**Impact**: Hard to maintain; changes to one arm may not apply to the other.

**Recommendation**: Extract generic `enrich_and_sort<F>(results, scores, annotate, descending)` helper parameterized by closure.

---

## Suggestions (Lower Confidence)

- **Post-scoring `file_filter` placement** (Confidence: 80%, Performance) — The blast-radius allowlist is applied after scoring. For small allowlists (10 files out of 50k indexed), 99.98% of scoring is wasted. Move filter check into scoring loop.
- **Redundant `sorted_paths()` call** (Confidence: 85%, Performance) — `manifest.sorted_paths()` called twice in `execute_query` (lines 72, 86). Should be hoisted and reused.
- **`cochanges_for_file` OR query index efficiency** (Confidence: 82%, Performance) — `WHERE file_a = ?1 OR file_b = ?1` uses OR across two columns. Consider `UNION ALL` for guaranteed dual-index usage.
- **`temporal_annotation_tag` Vec allocation for 2 items** (Confidence: 65%, Performance) — Minor allocation for max 2 items. Could use fixed array or direct `write!` instead.
- **`read_git_head` spawns subprocess** (Confidence: 70%, Performance) — ~5-10ms overhead per staleness check. Consider reading `.git/HEAD` directly.
- **Manual JSON construction diverges from serde derive** (Confidence: 82%, Consistency) — `format_temporal_json` uses manual `json!` instead of `#[derive(Serialize)]`. This explains the naming drift.
- **`run_query` opens temporal DB eagerly** (Confidence: 62%, Consistency) — DB opened before checking if it will be used, diverges from lazy-open pattern.

---

## Action Plan

1. **BLOCKING**: Replace bulk-load calls in `apply_temporal_enrichment` and `query_standalone` with per-file lookups (HIGH, confidence 86%)
2. **BLOCKING**: Propagate error on blast-radius path normalization failure instead of silent degradation (HIGH, confidence 82%)
3. **BLOCKING**: Rename `"limit"` to `"total"` in all three standalone temporal JSON outputs (HIGH, confidence 92%)
4. **BLOCKING**: Include target file in blast-radius allowlist (MEDIUM, confidence 83%)
5. **BLOCKING**: Add LIMIT to `cochanges_for_file` query (MEDIUM, confidence 85%)
6. **BLOCKING**: Fix byte-index panic in `check_temporal_staleness` (MEDIUM, confidence 82%)
7. **CONDITION**: Extract `resort_partners_by_temporal` and `enrich_and_sort` helpers to reduce cyclomatic complexity before next temporal feature (HIGH, approval condition)
8. **SHOULD-FIX**: Isolate `set_current_dir` tests with `#[serial]` or refactor to avoid global mutation (MEDIUM, confidence 80%)
9. **SHOULD-FIX**: Add empty-table edge case tests for Cold and Risky modes (MEDIUM, confidence 82%)
10. **SHOULD-FIX**: Add staleness detection test for actual stale case (MEDIUM, confidence 83%)
11. **SHOULD-FIX**: Add direct unit test for `temporal_annotation_tag` with all four branches (MEDIUM, confidence 80%)
12. **SHOULD-FIX**: Add `standalone_risky_json_valid` and `standalone_blast_radius_json_valid` tests (MEDIUM, confidence 80%)

---

## Summary

The PR introduces a well-designed temporal search feature with proper error handling, comprehensive test coverage, and thoughtful schema migration. The core architecture (types/temporal/mod.rs separation, standalone vs. combined dispatch) is solid. However, six blocking issues require fixes before merge:

1. **API misuse** — Bulk table loads instead of per-file lookups (HIGH, consensus finding)
2. **Error handling** — Silent degradation on user-input validation (HIGH, consensus finding)
3. **API consistency** — JSON field naming drift from "total" (HIGH, blocking)
4. **Correctness** — Target file excluded from blast-radius (MEDIUM)
5. **Reliability** — Unbounded `cochanges_for_file` query (MEDIUM)
6. **Robustness** — Byte-index panic on non-ASCII database content (MEDIUM)

Additionally, two complexity findings (cyclomatic complexity in `query_standalone` and `apply_temporal_enrichment`) should be addressed before this becomes the foundation for future temporal features.

**Recommendation**: CHANGES_REQUESTED. Address all six blocking issues (estimated 2-3 hours), consider the complexity extractions (approval condition), and the PR is ready for merge.
