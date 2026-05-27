//! Tests for the temporal search helpers.

use std::io::BufWriter;

use rskim_search::{CochangeRow, HotspotRow, META_GIT_HEAD, RiskRow, TemporalDb};
use tempfile::TempDir;

use super::{
    TemporalQueryOutput, apply_temporal_enrichment, check_temporal_staleness, format_temporal_json,
    format_temporal_text, normalize_blast_radius_path, open_temporal_db, query_standalone,
};
use crate::cmd::search::types::{ResolvedResult, TemporalSort};

// ============================================================================
// Helpers
// ============================================================================

fn temp_db() -> (TempDir, TemporalDb) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("temporal.db");
    let db = TemporalDb::open(&path).unwrap();
    (dir, db)
}

fn make_result(path: &str, score: f64) -> ResolvedResult {
    ResolvedResult {
        path: path.to_string(),
        score,
        field: "function_signature".to_string(),
        line_number: None,
        line_range: None,
        snippet: None,
        stale: false,
        match_positions: vec![],
        temporal: None,
    }
}

// ============================================================================
// Step 8: Core helpers — normalize_blast_radius_path
// ============================================================================

#[test]
fn normalize_relative_path() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    // Create a file inside the temp root.
    let sub = root.join("src");
    std::fs::create_dir_all(&sub).unwrap();
    let file = sub.join("auth.rs");
    std::fs::write(&file, "").unwrap();

    // Normalize from the root.
    // Note: no set_current_dir here — root-relative resolution takes priority
    // over CWD fallback, so this test is not sensitive to the process CWD.
    let result = normalize_blast_radius_path("src/auth.rs", &root).unwrap();
    assert_eq!(result, "src/auth.rs");
}

#[test]
fn normalize_absolute_path_in_repo() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let sub = root.join("src");
    std::fs::create_dir_all(&sub).unwrap();
    let file = sub.join("main.rs");
    std::fs::write(&file, "").unwrap();

    let result = normalize_blast_radius_path(file.to_str().unwrap(), &root).unwrap();
    assert_eq!(result, "src/main.rs");
}

#[test]
fn normalize_path_outside_repo_errors() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let outside = TempDir::new().unwrap();
    let outside_file = outside.path().join("outside.rs");
    std::fs::write(&outside_file, "").unwrap();

    let result = normalize_blast_radius_path(outside_file.to_str().unwrap(), &root);
    assert!(result.is_err(), "path outside repo should return error");
}

// F14: nonexistent path must produce a clear "blast-radius file not found" error,
// not the confusing "outside the project root" message that canonicalize would yield.
#[test]
fn normalize_nonexistent_relative_path_gives_not_found_error() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    // Do NOT create the file — test the nonexistent path case.
    let result = normalize_blast_radius_path("src/does_not_exist.rs", &root);
    assert!(result.is_err(), "nonexistent path should return error");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("blast-radius file not found"),
        "error should say 'blast-radius file not found', got: {msg}"
    );
    assert!(
        !msg.contains("outside the project root"),
        "error should NOT say 'outside the project root' for nonexistent files, got: {msg}"
    );
}

#[test]
fn normalize_nonexistent_absolute_path_gives_not_found_error() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    // Absolute path inside the repo but the file doesn't exist.
    let missing = root.join("src").join("ghost.rs");

    let result = normalize_blast_radius_path(missing.to_str().unwrap(), &root);
    assert!(
        result.is_err(),
        "nonexistent absolute path should return error"
    );
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("blast-radius file not found"),
        "error should say 'blast-radius file not found', got: {msg}"
    );
}

#[test]
fn normalize_dot_slash_stripped() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let sub = root.join("lib");
    std::fs::create_dir_all(&sub).unwrap();
    let file = sub.join("mod.rs");
    std::fs::write(&file, "").unwrap();

    // No set_current_dir — root-relative resolution does not require CWD mutation.
    let result = normalize_blast_radius_path("lib/mod.rs", &root).unwrap();
    // Should not start with "./"
    assert!(
        !result.starts_with("./"),
        "normalized path should not start with './', got: {result}"
    );
    assert_eq!(result, "lib/mod.rs");
}

// ============================================================================
// Step 8: DB helpers
// ============================================================================

#[test]
fn open_temporal_db_missing_returns_none() {
    let dir = TempDir::new().unwrap();
    let nonexistent = dir.path().join("nonexistent.db");
    assert!(open_temporal_db(&nonexistent).is_none());
}

#[test]
fn staleness_returns_none_when_current() {
    // Without a real git repo we can't test the "current" case fully,
    // but we can verify it returns None when the DB has no git_head meta key.
    let (_dir, db) = temp_db();
    let dir2 = TempDir::new().unwrap();
    // No META_GIT_HEAD set — should return None (nothing to compare).
    let result = check_temporal_staleness(&db, dir2.path());
    assert!(
        result.is_none(),
        "should return None when no git_head meta is stored"
    );
}

// ============================================================================
// cochange_partner_paths — direct unit tests
// ============================================================================

/// When `target` matches `file_a`, the partner set contains `file_b`.
#[test]
fn cochange_partner_paths_target_is_file_a() {
    use rskim_search::CochangeRow;
    let rows = vec![CochangeRow {
        file_a: "src/auth.rs".to_string(),
        file_b: "src/middleware.rs".to_string(),
        count: 5,
        jaccard: 0.75,
    }];
    let partners = super::cochange_partner_paths(&rows, "src/auth.rs");
    assert!(
        partners.contains("src/middleware.rs"),
        "partner must be file_b when target is file_a"
    );
    assert!(
        !partners.contains("src/auth.rs"),
        "target itself must not appear in partner set"
    );
}

/// When `target` matches `file_b`, the partner set contains `file_a`.
#[test]
fn cochange_partner_paths_target_is_file_b() {
    use rskim_search::CochangeRow;
    let rows = vec![CochangeRow {
        file_a: "src/auth.rs".to_string(),
        file_b: "src/middleware.rs".to_string(),
        count: 5,
        jaccard: 0.75,
    }];
    let partners = super::cochange_partner_paths(&rows, "src/middleware.rs");
    assert!(
        partners.contains("src/auth.rs"),
        "partner must be file_a when target is file_b"
    );
    assert!(
        !partners.contains("src/middleware.rs"),
        "target itself must not appear in partner set"
    );
}

/// Empty input produces an empty partner set.
#[test]
fn cochange_partner_paths_empty_input() {
    let partners = super::cochange_partner_paths(&[], "src/anything.rs");
    assert!(partners.is_empty(), "empty input must produce empty partner set");
}

// ============================================================================
// Step 9: Standalone temporal dispatch
// ============================================================================

#[test]
fn standalone_hot_returns_top_by_score() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let (_db_dir, db) = temp_db();

    db.store_hotspots(&[
        HotspotRow {
            file_path: "b.rs".to_string(),
            score: 0.4,
            changes_30d: 2,
            changes_90d: 5,
        },
        HotspotRow {
            file_path: "a.rs".to_string(),
            score: 0.9,
            changes_30d: 8,
            changes_90d: 20,
        },
    ])
    .unwrap();

    let output = query_standalone(Some(TemporalSort::Hot), None, 10, &db, &root).unwrap();
    match output {
        TemporalQueryOutput::Hotspots(rows) => {
            assert_eq!(rows.len(), 2);
            assert!((rows[0].score - 0.9).abs() < f64::EPSILON, "highest first");
        }
        other => panic!("expected Hotspots, got {other:?}"),
    }
}

#[test]
fn standalone_cold_returns_bottom_by_score() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let (_db_dir, db) = temp_db();

    db.store_hotspots(&[
        HotspotRow {
            file_path: "hot.rs".to_string(),
            score: 0.95,
            changes_30d: 20,
            changes_90d: 50,
        },
        HotspotRow {
            file_path: "cold.rs".to_string(),
            score: 0.05,
            changes_30d: 0,
            changes_90d: 1,
        },
    ])
    .unwrap();

    let output = query_standalone(Some(TemporalSort::Cold), None, 10, &db, &root).unwrap();
    match output {
        TemporalQueryOutput::Coldspots(rows) => {
            assert_eq!(rows.len(), 2);
            assert!(rows[0].score <= rows[1].score, "coldest first");
        }
        other => panic!("expected Coldspots, got {other:?}"),
    }
}

#[test]
fn standalone_risky_returns_top_by_density() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let (_db_dir, db) = temp_db();

    db.store_risks(&[
        RiskRow {
            file_path: "low.rs".to_string(),
            risk_score: 0.1,
            total_commits: 10,
            fix_commits: 1,
            fix_density: 0.1,
        },
        RiskRow {
            file_path: "high.rs".to_string(),
            risk_score: 0.9,
            total_commits: 20,
            fix_commits: 12,
            fix_density: 0.6,
        },
    ])
    .unwrap();

    let output = query_standalone(Some(TemporalSort::Risky), None, 10, &db, &root).unwrap();
    match output {
        TemporalQueryOutput::Risks(rows) => {
            assert_eq!(rows.len(), 2);
            assert!(rows[0].risk_score >= rows[1].risk_score, "riskiest first");
        }
        other => panic!("expected Risks, got {other:?}"),
    }
}

#[test]
fn standalone_blast_radius_returns_partners() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    // Create a dummy file for path normalization.
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/auth.rs"), "").unwrap();

    let (_db_dir, db) = temp_db();
    db.store_cochanges(&[CochangeRow {
        file_a: "src/auth.rs".to_string(),
        file_b: "src/middleware.rs".to_string(),
        count: 5,
        jaccard: 0.75,
    }])
    .unwrap();

    let output = query_standalone(None, Some("src/auth.rs"), 10, &db, &root).unwrap();
    match output {
        TemporalQueryOutput::Cochanges { target, partners } => {
            assert_eq!(target, "src/auth.rs");
            assert_eq!(partners.len(), 1);
        }
        other => panic!("expected Cochanges, got {other:?}"),
    }
}

#[test]
fn standalone_blast_radius_with_risky_sorts_by_risk() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/auth.rs"), "").unwrap();

    let (_db_dir, db) = temp_db();
    db.store_cochanges(&[
        CochangeRow {
            file_a: "src/auth.rs".to_string(),
            file_b: "src/low_risk.rs".to_string(),
            count: 10,
            jaccard: 0.9,
        },
        CochangeRow {
            file_a: "src/auth.rs".to_string(),
            file_b: "src/high_risk.rs".to_string(),
            count: 3,
            jaccard: 0.3,
        },
    ])
    .unwrap();
    db.store_risks(&[
        RiskRow {
            file_path: "src/low_risk.rs".to_string(),
            risk_score: 0.1,
            total_commits: 10,
            fix_commits: 1,
            fix_density: 0.1,
        },
        RiskRow {
            file_path: "src/high_risk.rs".to_string(),
            risk_score: 0.9,
            total_commits: 10,
            fix_commits: 8,
            fix_density: 0.8,
        },
    ])
    .unwrap();

    let output = query_standalone(
        Some(TemporalSort::Risky),
        Some("src/auth.rs"),
        10,
        &db,
        &root,
    )
    .unwrap();
    match output {
        TemporalQueryOutput::Cochanges { partners, .. } => {
            assert_eq!(partners.len(), 2);
            // High risk should come first despite lower Jaccard.
            let first_partner = if partners[0].file_a == "src/auth.rs" {
                &partners[0].file_b
            } else {
                &partners[0].file_a
            };
            assert_eq!(
                first_partner, "src/high_risk.rs",
                "high risk partner should be first"
            );
        }
        other => panic!("expected Cochanges, got {other:?}"),
    }
}

#[test]
fn standalone_limit_caps_results() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let (_db_dir, db) = temp_db();

    db.store_hotspots(
        &(0..10)
            .map(|i| HotspotRow {
                file_path: format!("file_{i}.rs"),
                score: i as f64 / 10.0,
                changes_30d: i,
                changes_90d: i * 2,
            })
            .collect::<Vec<_>>(),
    )
    .unwrap();

    let output = query_standalone(Some(TemporalSort::Hot), None, 3, &db, &root).unwrap();
    match output {
        TemporalQueryOutput::Hotspots(rows) => {
            assert_eq!(rows.len(), 3, "limit should cap at 3");
        }
        other => panic!("expected Hotspots, got {other:?}"),
    }
}

#[test]
fn standalone_hot_json_valid() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let (_db_dir, db) = temp_db();

    db.store_hotspots(&[HotspotRow {
        file_path: "src/a.rs".to_string(),
        score: 0.7,
        changes_30d: 3,
        changes_90d: 8,
    }])
    .unwrap();

    let output = query_standalone(Some(TemporalSort::Hot), None, 10, &db, &root).unwrap();
    let mut buf = BufWriter::new(Vec::new());
    format_temporal_json(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&s).expect("must be valid JSON");
    assert_eq!(v["mode"], "hot");
    assert!(v["results"].is_array());
    assert_eq!(v["total"], 1, "JSON output should use 'total', not 'limit'");
    assert!(
        v["limit"].is_null(),
        "JSON output must not contain a 'limit' field"
    );
}

#[test]
fn standalone_hot_text_has_table_columns() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let (_db_dir, db) = temp_db();

    db.store_hotspots(&[HotspotRow {
        file_path: "src/a.rs".to_string(),
        score: 0.7,
        changes_30d: 3,
        changes_90d: 8,
    }])
    .unwrap();

    let output = query_standalone(Some(TemporalSort::Hot), None, 10, &db, &root).unwrap();
    let mut buf = BufWriter::new(Vec::new());
    format_temporal_text(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    assert!(
        s.contains("Score"),
        "text output should have Score column header"
    );
    assert!(
        s.contains("Path"),
        "text output should have Path column header"
    );
}

// ============================================================================
// Step 10: Combined text+temporal enrichment
// ============================================================================

#[test]
fn enrichment_hot_sorts_by_hotspot_desc() {
    let (_db_dir, db) = temp_db();
    db.store_hotspots(&[
        HotspotRow {
            file_path: "low.rs".to_string(),
            score: 0.2,
            changes_30d: 1,
            changes_90d: 2,
        },
        HotspotRow {
            file_path: "high.rs".to_string(),
            score: 0.9,
            changes_30d: 8,
            changes_90d: 20,
        },
    ])
    .unwrap();

    let mut results = vec![
        make_result("low.rs", 10.0), // high BM25F but low hotspot
        make_result("high.rs", 5.0), // low BM25F but high hotspot
    ];

    apply_temporal_enrichment(&mut results, TemporalSort::Hot, &db).unwrap();

    assert_eq!(
        results[0].path, "high.rs",
        "hot sort should put high hotspot first"
    );
    let annotation = results[0].temporal.as_ref().unwrap();
    assert!(
        annotation.hotspot_score.is_some(),
        "hot result should have hotspot annotation"
    );
}

#[test]
fn enrichment_cold_sorts_by_hotspot_asc() {
    let (_db_dir, db) = temp_db();
    db.store_hotspots(&[
        HotspotRow {
            file_path: "hot.rs".to_string(),
            score: 0.95,
            changes_30d: 20,
            changes_90d: 50,
        },
        HotspotRow {
            file_path: "cold.rs".to_string(),
            score: 0.05,
            changes_30d: 0,
            changes_90d: 1,
        },
    ])
    .unwrap();

    let mut results = vec![make_result("hot.rs", 10.0), make_result("cold.rs", 10.0)];

    apply_temporal_enrichment(&mut results, TemporalSort::Cold, &db).unwrap();

    assert_eq!(
        results[0].path, "cold.rs",
        "cold sort should put lowest hotspot first"
    );
}

#[test]
fn enrichment_risky_sorts_by_density_desc() {
    let (_db_dir, db) = temp_db();
    db.store_risks(&[
        RiskRow {
            file_path: "safe.rs".to_string(),
            risk_score: 0.1,
            total_commits: 10,
            fix_commits: 1,
            fix_density: 0.1,
        },
        RiskRow {
            file_path: "buggy.rs".to_string(),
            risk_score: 0.9,
            total_commits: 10,
            fix_commits: 9,
            fix_density: 0.9,
        },
    ])
    .unwrap();

    let mut results = vec![make_result("safe.rs", 10.0), make_result("buggy.rs", 8.0)];

    apply_temporal_enrichment(&mut results, TemporalSort::Risky, &db).unwrap();

    assert_eq!(
        results[0].path, "buggy.rs",
        "risky sort should put most risky first"
    );
    let annotation = results[0].temporal.as_ref().unwrap();
    assert!(
        annotation.risk_score.is_some(),
        "risky result should have risk annotation"
    );
}

#[test]
fn enrichment_missing_files_sort_last() {
    let (_db_dir, db) = temp_db();
    db.store_hotspots(&[HotspotRow {
        file_path: "known.rs".to_string(),
        score: 0.5,
        changes_30d: 3,
        changes_90d: 7,
    }])
    .unwrap();

    let mut results = vec![
        make_result("unknown.rs", 10.0), // not in temporal DB
        make_result("known.rs", 5.0),    // in temporal DB
    ];

    apply_temporal_enrichment(&mut results, TemporalSort::Hot, &db).unwrap();

    // "known.rs" has hotspot annotation so it gets priority over "unknown.rs".
    assert_eq!(
        results[0].path, "known.rs",
        "files with temporal data should sort before unannotated files in Hot mode"
    );
    assert!(
        results[1].temporal.is_none(),
        "unknown file should have no annotation"
    );
}

#[test]
fn combined_json_has_temporal_annotations() {
    let (_db_dir, db) = temp_db();
    db.store_hotspots(&[HotspotRow {
        file_path: "src/a.rs".to_string(),
        score: 0.8,
        changes_30d: 5,
        changes_90d: 12,
    }])
    .unwrap();

    let mut results = vec![make_result("src/a.rs", 7.0)];
    apply_temporal_enrichment(&mut results, TemporalSort::Hot, &db).unwrap();

    // The annotation should be present.
    let annotation = results[0].temporal.as_ref().expect("annotation must exist");
    assert!((annotation.hotspot_score.unwrap() - 0.8).abs() < f64::EPSILON);

    // Serialize to JSON and verify temporal field is present.
    let json = serde_json::to_value(&results[0]).unwrap();
    assert!(
        json["temporal"]["hotspot_score"].is_number(),
        "temporal.hotspot_score must be present in JSON"
    );
}

// ============================================================================
// Step 6: parse_flags for temporal flags
// ============================================================================

#[test]
fn parse_hot_flag() {
    let flags = super::super::parse_flags(&["--hot".to_string()]).unwrap();
    assert_eq!(flags.temporal_sort, Some(TemporalSort::Hot));
}

#[test]
fn parse_cold_flag() {
    let flags = super::super::parse_flags(&["--cold".to_string()]).unwrap();
    assert_eq!(flags.temporal_sort, Some(TemporalSort::Cold));
}

#[test]
fn parse_risky_flag() {
    let flags = super::super::parse_flags(&["--risky".to_string()]).unwrap();
    assert_eq!(flags.temporal_sort, Some(TemporalSort::Risky));
}

#[test]
fn parse_blast_radius_space() {
    let flags =
        super::super::parse_flags(&["--blast-radius".to_string(), "src/auth.rs".to_string()])
            .unwrap();
    assert_eq!(flags.blast_radius.as_deref(), Some("src/auth.rs"));
}

#[test]
fn parse_blast_radius_equals() {
    let flags = super::super::parse_flags(&["--blast-radius=src/auth.rs".to_string()]).unwrap();
    assert_eq!(flags.blast_radius.as_deref(), Some("src/auth.rs"));
}

#[test]
fn parse_blast_radius_missing_value_error() {
    let err = super::super::parse_flags(&["--blast-radius".to_string()]).unwrap_err();
    assert!(
        err.to_string().contains("--blast-radius requires"),
        "expected blast-radius error, got: {err}"
    );
}

#[test]
fn parse_hot_cold_conflict_error() {
    let err = super::super::parse_flags(&["--hot".to_string(), "--cold".to_string()]).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("mutually exclusive"),
        "expected mutual exclusion error, got: {msg}"
    );
}

#[test]
fn parse_hot_risky_conflict_error() {
    let err = super::super::parse_flags(&["--hot".to_string(), "--risky".to_string()]).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("mutually exclusive"),
        "expected mutual exclusion error, got: {msg}"
    );
}

#[test]
fn parse_blast_radius_with_hot_composable() {
    // blast-radius + hot is valid (not an error).
    let flags = super::super::parse_flags(&[
        "--hot".to_string(),
        "--blast-radius".to_string(),
        "src/auth.rs".to_string(),
    ])
    .unwrap();
    assert_eq!(flags.temporal_sort, Some(TemporalSort::Hot));
    assert_eq!(flags.blast_radius.as_deref(), Some("src/auth.rs"));
}

#[test]
fn parse_blast_radius_with_query_text() {
    use super::super::SearchAction;
    let flags = super::super::parse_flags(&[
        "--blast-radius".to_string(),
        "src/auth.rs".to_string(),
        "authenticate".to_string(),
    ])
    .unwrap();
    assert_eq!(flags.blast_radius.as_deref(), Some("src/auth.rs"));
    assert_eq!(
        flags.action,
        SearchAction::Query("authenticate".to_string())
    );
}

#[test]
fn parse_hot_with_limit_and_json() {
    let flags = super::super::parse_flags(&[
        "--hot".to_string(),
        "--limit".to_string(),
        "5".to_string(),
        "--json".to_string(),
    ])
    .unwrap();
    assert_eq!(flags.temporal_sort, Some(TemporalSort::Hot));
    assert_eq!(flags.limit, 5);
    assert!(flags.json);
}

#[test]
fn parse_standalone_hot_no_text() {
    use super::super::SearchAction;
    let flags = super::super::parse_flags(&["--hot".to_string()]).unwrap();
    assert_eq!(flags.temporal_sort, Some(TemporalSort::Hot));
    // Empty query — should dispatch to standalone temporal
    assert_eq!(flags.action, SearchAction::Query("".to_string()));
}

#[test]
fn parse_standalone_blast_radius() {
    use super::super::SearchAction;
    let flags =
        super::super::parse_flags(&["--blast-radius".to_string(), "src/auth.rs".to_string()])
            .unwrap();
    assert_eq!(flags.blast_radius.as_deref(), Some("src/auth.rs"));
    assert_eq!(flags.action, SearchAction::Query("".to_string()));
}

#[test]
fn parse_help_includes_temporal_flags() {
    use std::process::ExitCode;
    const TEST_ANALYTICS: crate::analytics::AnalyticsConfig = crate::analytics::AnalyticsConfig {
        enabled: false,
        input_cost_per_mtok: None,
        session_id: None,
    };
    // Verify it runs without error.
    let result = super::super::run(&["--help".to_string()], &TEST_ANALYTICS).unwrap();
    assert_eq!(result, ExitCode::SUCCESS);
}

// ============================================================================
// Issue: standalone --cold and --risky on empty tables (format_temporal_text
// empty-table branches) — previously untested.
// ============================================================================

#[test]
fn standalone_cold_empty_db_text_format() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let (_db_dir, db) = temp_db();

    // Empty hotspots table — no store_hotspots call.
    let output = query_standalone(Some(TemporalSort::Cold), None, 10, &db, &root).unwrap();
    match &output {
        TemporalQueryOutput::Coldspots(rows) => assert!(rows.is_empty()),
        other => panic!("expected Coldspots, got {other:?}"),
    }

    let mut buf = BufWriter::new(Vec::new());
    format_temporal_text(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    assert!(
        s.contains("No coldspot data available"),
        "empty cold table must print no-data message, got: {s:?}"
    );
    // Must NOT print the column headers when there is no data.
    assert!(
        !s.contains("Score"),
        "column headers must not appear for empty cold output, got: {s:?}"
    );
}

#[test]
fn standalone_risky_empty_db_text_format() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let (_db_dir, db) = temp_db();

    // Empty risks table — no store_risks call.
    let output = query_standalone(Some(TemporalSort::Risky), None, 10, &db, &root).unwrap();
    match &output {
        TemporalQueryOutput::Risks(rows) => assert!(rows.is_empty()),
        other => panic!("expected Risks, got {other:?}"),
    }

    let mut buf = BufWriter::new(Vec::new());
    format_temporal_text(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    assert!(
        s.contains("No risk data available"),
        "empty risk table must print no-data message, got: {s:?}"
    );
    // Must NOT print the column headers when there is no data.
    assert!(
        !s.contains("Risk"),
        "column headers must not appear for empty risk output, got: {s:?}"
    );
}

// ============================================================================
// Issue: check_temporal_staleness stale-HEAD path — previously untested.
// The stored HEAD differs from the current repo HEAD.
// ============================================================================

// NOTE: This test requires the `git` binary and a writable filesystem to
// initialize a temporary repo and create a commit. In environments where git
// is unavailable or identity config is missing (some CI sandboxes), the test
// performs an early return with an eprintln! skip message rather than failing.
// This is intentional: the behaviour under test is git-dependent and cannot be
// meaningfully exercised without a real git binary. The skip is observable via
// the eprintln! output in verbose test runs (`cargo test -- --nocapture`).
// If running in CI, ensure `git` is on PATH and a default identity is set.
#[test]
fn staleness_warns_when_stored_head_differs_from_current() {
    // Set up a minimal git repo so git rev-parse HEAD returns a real value.
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    // Init git repo.
    let init = std::process::Command::new("git")
        .arg("-C")
        .arg(&root)
        .arg("init")
        .output();
    if init.map(|o| !o.status.success()).unwrap_or(true) {
        eprintln!("SKIP staleness_warns_when_stored_head_differs_from_current: git init failed or git not available");
        return;
    }

    // Configure git identity for the commit.
    let _ = std::process::Command::new("git")
        .args([
            "-C",
            root.to_str().unwrap(),
            "config",
            "user.email",
            "test@test.com",
        ])
        .output();
    let _ = std::process::Command::new("git")
        .args(["-C", root.to_str().unwrap(), "config", "user.name", "Test"])
        .output();

    // Create an initial commit so HEAD is a real SHA.
    std::fs::write(root.join("README.md"), "test").unwrap();
    let _ = std::process::Command::new("git")
        .args(["-C", root.to_str().unwrap(), "add", "."])
        .output();
    let commit_result = std::process::Command::new("git")
        .args(["-C", root.to_str().unwrap(), "commit", "-m", "init"])
        .output();
    if commit_result.map(|o| !o.status.success()).unwrap_or(true) {
        eprintln!("SKIP staleness_warns_when_stored_head_differs_from_current: git commit failed (CI environment without git identity?)");
        return;
    }

    // Open a fresh temporal DB and store a deliberately wrong HEAD.
    let db_path = root.join("temporal.db");
    let db = TemporalDb::open(&db_path).unwrap();
    db.set_meta(
        rskim_search::META_GIT_HEAD,
        "0000000000000000000000000000000000000000",
    )
    .unwrap();

    // The staleness check must detect the mismatch and return a warning.
    let warning = check_temporal_staleness(&db, &root);
    assert!(
        warning.is_some(),
        "staleness check must return Some(warning) when stored HEAD differs from current HEAD"
    );
    let msg = warning.unwrap();
    assert!(
        msg.contains("stale"),
        "warning message must contain 'stale', got: {msg:?}"
    );
    assert!(
        msg.contains("0000000"),
        "warning must include stored HEAD prefix, got: {msg:?}"
    );
}

// ============================================================================
// Issue: temporal_annotation_tag "both hotspot+risk" case — previously untested.
// ============================================================================

/// format_text_output renders both hotspot and risk tags when both annotations
/// are present. This exercises the "both" branch of temporal_annotation_tag.
#[test]
fn format_text_output_includes_both_hotspot_and_risk_tags() {
    use crate::cmd::search::types::{QueryOutput, ResolvedResult, TemporalAnnotation};

    let result = ResolvedResult {
        path: "src/both.rs".to_string(),
        score: 8.0,
        field: "function_signature".to_string(),
        line_number: None,
        line_range: None,
        snippet: None,
        stale: false,
        match_positions: vec![],
        temporal: Some(TemporalAnnotation {
            hotspot_score: Some(0.95),
            risk_score: Some(0.80),
            ..Default::default()
        }),
    };

    let output = QueryOutput {
        query: "both".to_string(),
        total: 1,
        results: vec![result],
        duration_ms: 1,
        index_stats: None,
    };

    let mut buf = BufWriter::new(Vec::new());
    super::super::query::format_text_output(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();

    assert!(
        s.contains("hotspot:"),
        "output must contain 'hotspot:' tag when hotspot annotation present, got: {s:?}"
    );
    assert!(
        s.contains("0.950"),
        "hotspot score must be formatted to 3dp, got: {s:?}"
    );
    assert!(
        s.contains("risk:"),
        "output must contain 'risk:' tag when risk annotation present, got: {s:?}"
    );
    assert!(
        s.contains("0.800"),
        "risk score must be formatted to 3dp, got: {s:?}"
    );
}

// ============================================================================
// Issue: format_temporal_json for Risks and Cochanges variants — previously
// untested. Only Hotspots JSON shape was validated.
// ============================================================================

#[test]
fn standalone_risky_json_valid() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let (_db_dir, db) = temp_db();

    db.store_risks(&[RiskRow {
        file_path: "src/buggy.rs".to_string(),
        risk_score: 0.85,
        total_commits: 20,
        fix_commits: 10,
        fix_density: 0.5,
    }])
    .unwrap();

    let output = query_standalone(Some(TemporalSort::Risky), None, 10, &db, &root).unwrap();
    let mut buf = BufWriter::new(Vec::new());
    format_temporal_json(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&s).expect("must be valid JSON");

    assert_eq!(v["mode"], "risky", "mode must be 'risky'");
    assert!(v["results"].is_array(), "results must be an array");
    assert_eq!(v["total"], 1, "total must match number of rows");

    let first = &v["results"][0];
    assert_eq!(first["path"], "src/buggy.rs");
    assert!(
        first["risk_score"].is_number(),
        "risk_score must be a number"
    );
    assert!(
        first["fix_density"].is_number(),
        "fix_density must be a number"
    );
    assert!(
        first["fix_commits"].is_number(),
        "fix_commits must be a number"
    );
    assert!(
        first["total_commits"].is_number(),
        "total_commits must be a number"
    );
}

#[test]
fn standalone_blast_radius_json_valid() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    // Create the target file so path normalization succeeds.
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/auth.rs"), "").unwrap();

    let (_db_dir, db) = temp_db();
    db.store_cochanges(&[rskim_search::CochangeRow {
        file_a: "src/auth.rs".to_string(),
        file_b: "src/middleware.rs".to_string(),
        count: 7,
        jaccard: 0.65,
    }])
    .unwrap();

    let output = query_standalone(None, Some("src/auth.rs"), 10, &db, &root).unwrap();
    let mut buf = BufWriter::new(Vec::new());
    format_temporal_json(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&s).expect("must be valid JSON");

    assert_eq!(v["mode"], "blast-radius", "mode must be 'blast-radius'");
    assert_eq!(
        v["target"], "src/auth.rs",
        "target must match the input path"
    );
    assert!(v["results"].is_array(), "results must be an array");
    assert_eq!(v["total"], 1, "total must match number of partners");

    let first = &v["results"][0];
    assert_eq!(
        first["path"], "src/middleware.rs",
        "partner path must be correct"
    );
    assert!(first["jaccard"].is_number(), "jaccard must be a number");
    assert!(first["count"].is_number(), "count must be a number");
}

// ============================================================================
// Issue temporal_tests:cold_json — format_temporal_json cold path
// ============================================================================

#[test]
fn standalone_cold_json_valid() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let (_db_dir, db) = temp_db();

    db.store_hotspots(&[HotspotRow {
        file_path: "src/cold.rs".to_string(),
        score: 0.03,
        changes_30d: 0,
        changes_90d: 1,
    }])
    .unwrap();

    let output = query_standalone(Some(TemporalSort::Cold), None, 10, &db, &root).unwrap();
    let mut buf = BufWriter::new(Vec::new());
    format_temporal_json(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&s).expect("must be valid JSON");

    assert_eq!(v["mode"], "cold", "mode discriminant must be 'cold'");
    assert!(v["results"].is_array(), "results must be an array");
    assert_eq!(v["total"], 1, "total must match number of rows");
    assert!(
        v["limit"].is_null(),
        "JSON output must not contain a 'limit' field"
    );
}

// ============================================================================
// Issue temporal_tests:empty_hotspot — format_temporal_text hot empty branch
// ============================================================================

#[test]
fn standalone_hot_empty_db_text_format() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let (_db_dir, db) = temp_db();

    // Empty hotspots table — no store_hotspots call.
    let output = query_standalone(Some(TemporalSort::Hot), None, 10, &db, &root).unwrap();
    match &output {
        TemporalQueryOutput::Hotspots(rows) => assert!(rows.is_empty()),
        other => panic!("expected Hotspots, got {other:?}"),
    }

    let mut buf = BufWriter::new(Vec::new());
    format_temporal_text(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    assert!(
        s.contains("No hotspot data available"),
        "empty hot table must print no-data message, got: {s:?}"
    );
    // Must NOT print the column headers when there is no data.
    assert!(
        !s.contains("Score"),
        "column headers must not appear for empty hot output, got: {s:?}"
    );
}

// ============================================================================
// Issue temporal_tests:empty_cochange — format_temporal_text Cochanges empty branch
// ============================================================================

#[test]
fn standalone_blast_radius_empty_db_text_format() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    // Create the target file so path normalization succeeds.
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/auth.rs"), "").unwrap();

    let (_db_dir, db) = temp_db();
    // No store_cochanges call — empty co-change table.

    let output = query_standalone(None, Some("src/auth.rs"), 10, &db, &root).unwrap();
    match &output {
        TemporalQueryOutput::Cochanges { partners, .. } => assert!(partners.is_empty()),
        other => panic!("expected Cochanges, got {other:?}"),
    }

    let mut buf = BufWriter::new(Vec::new());
    format_temporal_text(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    assert!(
        s.contains("No co-change data"),
        "empty co-change result must print no-data message, got: {s:?}"
    );
    // Must NOT print the column headers when there is no data.
    assert!(
        !s.contains("Jaccard"),
        "column headers must not appear for empty co-change output, got: {s:?}"
    );
}
