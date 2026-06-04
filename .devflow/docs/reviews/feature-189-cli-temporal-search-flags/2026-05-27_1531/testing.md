# Testing Review Report

**Branch**: feature/189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T15:31

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Missing cold JSON format test** - `crates/rskim/src/cmd/search/temporal_tests.rs`
**Confidence**: 85%
- Problem: `format_temporal_json` for the `Coldspots` variant is never tested. The hot JSON path is tested (`standalone_hot_json_valid`), and the risky JSON path is tested (`standalone_risky_json_valid`), but `cold` shares the hotspot/coldspot match arm in `format_temporal_json` (line 393 of `temporal.rs`). While the arm is shared, the mode discriminant string (`"cold"` vs `"hot"`) is computed at runtime and never validated in a cold context.
- Fix: Add a `standalone_cold_json_valid` test that stores coldspot data, queries via `TemporalSort::Cold`, serializes to JSON, and asserts `v["mode"] == "cold"`:
```rust
#[test]
fn standalone_cold_json_valid() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let (_db_dir, db) = temp_db();

    db.store_hotspots(&[HotspotRow {
        file_path: "src/cold.rs".to_string(),
        score: 0.05,
        changes_30d: 0,
        changes_90d: 1,
    }]).unwrap();

    let output = query_standalone(Some(TemporalSort::Cold), None, 10, &db, &root).unwrap();
    let mut buf = BufWriter::new(Vec::new());
    format_temporal_json(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&s).expect("must be valid JSON");
    assert_eq!(v["mode"], "cold", "cold JSON must use mode 'cold', not 'hot'");
}
```

**Missing empty cochange text format test** - `crates/rskim/src/cmd/search/temporal_tests.rs`
**Confidence**: 83%
- Problem: The `format_temporal_text` function has an empty-data early-return branch for each variant. The `Hotspots` empty case gets implicit coverage via the cold empty test (both call the same code pattern), `Coldspots` has `standalone_cold_empty_db_text_format`, `Risks` has `standalone_risky_empty_db_text_format`, but `Cochanges` empty branch (line 366-368 of `temporal.rs`) is never tested. This branch prints `"No co-change data for {target:?}."` which is a user-facing message that should be validated.
- Fix: Add a test that calls `format_temporal_text` with a `Cochanges` variant containing zero partners and asserts the no-data message:
```rust
#[test]
fn standalone_blast_radius_empty_db_text_format() {
    let output = TemporalQueryOutput::Cochanges {
        target: "src/orphan.rs".to_string(),
        partners: vec![],
    };
    let mut buf = BufWriter::new(Vec::new());
    format_temporal_text(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    assert!(
        s.contains("No co-change data"),
        "empty cochange must print no-data message, got: {s:?}"
    );
}
```

**Missing empty hotspot text format test** - `crates/rskim/src/cmd/search/temporal_tests.rs`
**Confidence**: 80%
- Problem: The `Hotspots` empty-table early-return branch (line 311-313 of `temporal.rs`) is never directly tested. The `standalone_cold_empty_db_text_format` tests the `Coldspots` variant, not `Hotspots`. These are different match arms with different messages ("No hotspot data available" vs "No coldspot data available").
- Fix: Add `standalone_hot_empty_db_text_format`:
```rust
#[test]
fn standalone_hot_empty_db_text_format() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let (_db_dir, db) = temp_db();

    let output = query_standalone(Some(TemporalSort::Hot), None, 10, &db, &root).unwrap();
    let mut buf = BufWriter::new(Vec::new());
    format_temporal_text(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    assert!(
        s.contains("No hotspot data available"),
        "empty hot table must print no-data message, got: {s:?}"
    );
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`staleness_warns_when_stored_head_differs_from_current` silently skips on git failure** - `crates/rskim/src/cmd/search/temporal_tests.rs:809-867`
**Confidence**: 82%
- Problem: The test uses early `return` on git init or commit failure (lines 822 and 843), which means in CI environments where git user config isn't set, the test silently passes without executing the core assertion. While the test does configure git identity (lines 827-831), the early-return guards before the commit could mask silent skips. This is a known flaky-test anti-pattern -- the test appears green but didn't actually run.
- Fix: Consider using `#[ignore]` with a descriptive message instead of silent return, or at minimum log a message to stderr on skip so CI visibility is preserved. Alternatively, assert the git setup succeeded rather than silently returning:
```rust
// Instead of silent return:
if commit_result.map(|o| !o.status.success()).unwrap_or(true) {
    eprintln!("SKIP: staleness_warns_when_stored_head_differs_from_current (git commit failed)");
    return;
}
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Missing `resort_partners_by_temporal` Cold sort test** - `crates/rskim/src/cmd/search/temporal.rs:247-298` (Confidence: 70%) -- The `blast_radius_with_risky_sorts_by_risk` test covers the `Risky` sort path through `resort_partners_by_temporal`, but the `Cold` path (ascending sort of blast-radius partners by hotspot score) is never tested. The `Hot` path shares the same code with a boolean flip. A test with `TemporalSort::Cold` and blast-radius could catch a regression if the ascending/descending logic is accidentally swapped.

- **No test for `TemporalAnnotation` serde `skip_serializing_if` behavior** - `crates/rskim/src/cmd/search/types.rs:44-58` (Confidence: 65%) -- `TemporalAnnotation` uses `#[serde(skip_serializing_if = "Option::is_none")]` on every field. The JSON output tests verify presence of populated fields but never assert that absent fields are truly omitted from the JSON output (i.e., that `cochange_jaccard`, `fix_density`, `changes_30d`, `changes_90d` don't appear when not set).

- **No negative test for `--cold --risky` mutual exclusion** - `crates/rskim/src/cmd/search/temporal_tests.rs` (Confidence: 62%) -- `parse_hot_cold_conflict_error` and `parse_hot_risky_conflict_error` are tested, but `--cold --risky` conflict is not. All three pairwise conflicts route through the same code path so coverage is implicit, but for completeness a third test would close the matrix.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 3 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The test suite is strong overall -- 46 temporal-specific tests covering flag parsing, standalone dispatch, enrichment sorting, JSON/text formatting, staleness detection, path normalization, empty-table handling, and integration-level blast-radius filtering. The prior resolution cycle already addressed the major gaps (empty-table tests, staleness detection, CWD mutation). The remaining issues are coverage completeness items: three format branches lack direct tests (cold JSON, empty hotspot text, empty cochange text) and one test can silently skip in CI. None are blocking but all should be addressed before merge for defense-in-depth.
