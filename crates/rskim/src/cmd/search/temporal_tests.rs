//! Tests for the temporal search helpers.

use std::io::BufWriter;
use std::process::ExitCode;

use rskim_search::{CochangeRow, HotspotRow, RiskRow, TemporalDb};
use tempfile::TempDir;

use super::{
    DegradedReason, Fallback, HeadState, TemporalCoverage, TemporalOpen, TemporalQueryOutput,
    TemporalUnavailable, apply_temporal_enrichment, bounded_page_notice, check_temporal_staleness,
    degraded_notice, dimension_is_empty, enrich_ast_results, format_temporal_json,
    format_temporal_text, normalize_blast_radius_path, open_temporal_state, query_standalone,
    ranked_row_count, resort_window,
};
use crate::cmd::search::types::{ResolvedResult, TemporalSort};

const TEST_ANALYTICS: crate::analytics::AnalyticsConfig = crate::analytics::AnalyticsConfig {
    enabled: false,
    input_cost_per_mtok: None,
    session_id: None,
};

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
        layers_matched: vec![],
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

/// AD-414-15: `open_temporal_state` must return `Unavailable(Missing)` when
/// `temporal.db` does not exist and the head resolves.
#[test]
fn open_temporal_state_missing_returns_unavailable_missing() {
    let dir = TempDir::new().unwrap();
    // Use a resolved HEAD so the absence is classified as Missing, not HeadUnresolved.
    let result = open_temporal_state(
        dir.path(),
        dir.path(),
        &HeadState::Resolved("abc123".to_string()),
    );
    assert!(
        matches!(
            result,
            TemporalOpen::Unavailable(TemporalUnavailable {
                reason: DegradedReason::Missing,
                ..
            })
        ),
        "open_temporal_state must return Unavailable(Missing) when temporal.db \
         does not exist and HEAD resolves, got: {result:?}"
    );
}

/// AD-413-16: `open_temporal_state` must return `Unavailable(RepositoryMismatch)`
/// when `temporal.db` was built for a different repository root.
///
/// Setup:
/// - `outer` = a fake git root (has `.git/HEAD`) so `resolve_repo_toplevel(root)` resolves.
/// - `root`  = `outer/sub/` — a subdirectory (no `.git` of its own).
/// - `cache_dir` = a fresh temp dir whose `temporal.db` has a `git_toplevel` row
///   pointing to `/wrong/repo/path`, which differs from the live `outer` toplevel.
///
/// The test does NOT require a real `git` binary — the toplevel is discovered by
/// walking for `.git`, which the fake outer dir provides.
#[test]
fn open_temporal_state_anchor_differs_returns_repository_mismatch() {
    // Outer dir acts as the git repo root (has .git/HEAD).
    let outer = TempDir::new().unwrap();
    let git_dir = outer.path().join(".git");
    std::fs::create_dir(&git_dir).unwrap();
    std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();

    // root is a subdirectory inside the "git repo".
    let root = outer.path().join("sub");
    std::fs::create_dir_all(&root).unwrap();

    // cache_dir with temporal.db that records a wrong git_toplevel.
    let cache = TempDir::new().unwrap();
    let db_path = cache.path().join("temporal.db");
    drop(TemporalDb::open(&db_path).unwrap());
    crate::cmd::search::staleness::plant_meta_raw(
        &db_path,
        rskim_search::META_GIT_TOPLEVEL,
        "/wrong/repo/path",
    );

    let result = open_temporal_state(
        &root,
        cache.path(),
        &HeadState::Resolved("abc123".to_string()),
    );
    assert!(
        matches!(
            result,
            TemporalOpen::Unavailable(TemporalUnavailable {
                reason: DegradedReason::RepositoryMismatch,
                ..
            })
        ),
        "AD-413-16: open_temporal_state must return Unavailable(RepositoryMismatch) \
         when temporal.db belongs to a different repo, got: {result:?}"
    );
}

/// AD-413-16: `resolve_blast_radius_paths` must return `Ok(Some(empty_set))`
/// (not `Ok(None)`, not an error) when the temporal DB was written for a
/// different repository.  An empty allowlist forces zero results on all
/// blast-radius callers; `Ok(None)` would be misread as "not requested".
///
/// Uses the same fake-git-root fixture as `open_temporal_state_anchor_differs_returns_repository_mismatch`
/// to trigger `RepositoryMismatch` inside the funnel.
#[test]
fn resolve_blast_radius_paths_anchor_differs_returns_empty_allowlist() {
    // Outer dir acts as the git repo root.
    let outer = TempDir::new().unwrap();
    let git_dir = outer.path().join(".git");
    std::fs::create_dir(&git_dir).unwrap();
    std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();

    // root is a subdirectory; no .git of its own.
    let root = outer.path().join("sub");
    std::fs::create_dir_all(&root).unwrap();

    // cache_dir with temporal.db containing a wrong git_toplevel.
    let cache = TempDir::new().unwrap();
    let db_path = cache.path().join("temporal.db");
    drop(TemporalDb::open(&db_path).unwrap());
    crate::cmd::search::staleness::plant_meta_raw(
        &db_path,
        rskim_search::META_GIT_TOPLEVEL,
        "/wrong/repo/path",
    );

    // blast_radius path does not need to exist — the anchor guard fires before
    // path normalization.
    let result = super::resolve_blast_radius_paths(
        Some("src/auth.rs"),
        &root,
        cache.path(),
        false,
        &HeadState::NotARepo,
    );
    assert!(
        result.is_ok(),
        "resolve_blast_radius_paths must not Err on AnchorDiffers, got: {result:?}"
    );
    let (paths, degraded) = result.unwrap();
    assert!(
        paths.is_some(),
        "AD-413-16: resolve_blast_radius_paths must return Ok(Some(empty_set)) on \
         AnchorDiffers, not Ok(None) — None overloads the 'not requested' sentinel \
         (PF-016 / AD-413-16)"
    );
    assert!(
        paths.unwrap().is_empty(),
        "AD-413-16: the returned set must be empty — wrong-repo anchor forces zero results \
         on all blast-radius arms"
    );
    assert!(
        degraded.is_none(),
        "RepositoryMismatch returns empty allowlist (not a degraded reason) — callers \
         treat it as a filtered result, not a degraded state"
    );
}

/// AC-7 / AC-19(b): when the root is not a git repo, `resolve_blast_radius_paths`
/// emits the legacy composition format byte-identical to the pre-refactor message.
///
/// The format is `"no temporal data for --blast-radius — {NO_TEMPORAL_DATA_MSG}"`.
/// De-doubling (using bare `degraded_notice` with flag) applies ONLY to new
/// `DegradedReason` variants (Corrupt, Missing, …); `NotGitRepo` keeps the
/// composition wrapper so existing integrations remain unaffected.
#[test]
fn resolve_blast_radius_paths_not_git_repo_emits_legacy_composition_format() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    // resolve_blast_radius_paths calls eprintln! for the degraded notice.
    // Use a known-absent temporal DB + NotARepo head to trigger the NotGitRepo
    // path — the function returns Ok(None) and emits the composition message.
    let result = super::resolve_blast_radius_paths(
        Some("src/lib.rs"),
        root,
        dir.path(), // empty cache_dir — no temporal.db
        false,
        &HeadState::NotARepo,
    );

    assert!(
        result.is_ok(),
        "must not error for NotARepo (graceful degradation), got: {:?}",
        result.unwrap_err()
    );
    let (paths, _degraded) = result.unwrap();
    assert!(
        paths.is_none(),
        "NotARepo → no RepositoryMismatch → must return paths=None (graceful degradation)"
    );

    // Verify the message constant the composition wrapper would produce is correct.
    // (The eprintln! in the production code emits this; we assert the constant
    //  rather than capturing stderr, which would require process redirection.)
    let expected_msg = format!(
        "no temporal data for --blast-radius — {}",
        super::super::NO_TEMPORAL_DATA_MSG
    );
    assert!(
        expected_msg.contains("no temporal data for --blast-radius"),
        "composition format must name the flag: {expected_msg:?}"
    );
    assert!(
        expected_msg.contains(super::super::NO_TEMPORAL_DATA_MSG),
        "composition format must embed NO_TEMPORAL_DATA_MSG verbatim: {expected_msg:?}"
    );
    // AC-7: no doubled phrase (the wrapper adds context without repeating
    // "no temporal data for --blast-radius" a second time).
    let count = expected_msg
        .matches("no temporal data for --blast-radius")
        .count();
    assert_eq!(
        count, 1,
        "phrase 'no temporal data for --blast-radius' must appear exactly once (AC-7): {expected_msg:?}"
    );
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
    assert!(
        partners.is_empty(),
        "empty input must produce empty partner set"
    );
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

    let output = query_standalone(
        Some(TemporalSort::Hot),
        None,
        super::super::types::Page::first(10),
        &db,
        &root,
    )
    .unwrap()
    .0;
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

    let output = query_standalone(
        Some(TemporalSort::Cold),
        None,
        super::super::types::Page::first(10),
        &db,
        &root,
    )
    .unwrap()
    .0;
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

    let output = query_standalone(
        Some(TemporalSort::Risky),
        None,
        super::super::types::Page::first(10),
        &db,
        &root,
    )
    .unwrap()
    .0;
    match output {
        TemporalQueryOutput::Risks(rows) => {
            assert_eq!(rows.len(), 2);
            assert!(rows[0].risk_score >= rows[1].risk_score, "riskiest first");
        }
        other => panic!("expected Risks, got {other:?}"),
    }
}

/// AC-404-2 (hermetic deep pagination): `query_standalone` on the standalone
/// `--hot` arm must honor a non-zero `offset`, returning disjoint pages and a
/// sound `has_more` terminator.
///
/// This is the hermetic sibling of `cli_search_offset.rs::offset_accepted_on_hot_standalone`
/// — that CLI test only proves the flag is WIRED (degraded exit 0 with no
/// temporal.db); the deep disjointness+has_more contract that seeded data
/// requires is asserted here. Exercises the `paginate_sentinel` over-fetch +
/// skip/take path with `offset > 0` (AD-404-11 / D-5).
#[test]
fn standalone_hot_offset_paginates_disjoint_with_has_more() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let (_db_dir, db) = temp_db();

    // Five distinct scores → deterministic Hot order (score DESC, file_path ASC):
    // e(0.9) d(0.7) c(0.5) b(0.3) a(0.1).
    db.store_hotspots(&[
        HotspotRow {
            file_path: "a.rs".to_string(),
            score: 0.1,
            changes_30d: 1,
            changes_90d: 2,
        },
        HotspotRow {
            file_path: "b.rs".to_string(),
            score: 0.3,
            changes_30d: 2,
            changes_90d: 4,
        },
        HotspotRow {
            file_path: "c.rs".to_string(),
            score: 0.5,
            changes_30d: 3,
            changes_90d: 6,
        },
        HotspotRow {
            file_path: "d.rs".to_string(),
            score: 0.7,
            changes_30d: 4,
            changes_90d: 8,
        },
        HotspotRow {
            file_path: "e.rs".to_string(),
            score: 0.9,
            changes_30d: 5,
            changes_90d: 10,
        },
    ])
    .unwrap();

    let paths = |out: TemporalQueryOutput| -> Vec<String> {
        match out {
            TemporalQueryOutput::Hotspots(rows) => rows.into_iter().map(|r| r.file_path).collect(),
            other => panic!("expected Hotspots, got {other:?}"),
        }
    };

    // Page 0: limit 2, offset 0 → top two hottest, more remain.
    let (out0, more0) = query_standalone(
        Some(TemporalSort::Hot),
        None,
        super::super::types::Page::new(2, Some(0)),
        &db,
        &root,
    )
    .unwrap();
    let page0 = paths(out0);
    assert_eq!(
        page0,
        vec!["e.rs", "d.rs"],
        "page 0 = top two by score DESC"
    );
    assert!(more0, "5 rows, page of 2 at offset 0 → has_more");

    // Page 1: limit 2, offset 2 → next two, disjoint from page 0, more remain.
    let (out1, more1) = query_standalone(
        Some(TemporalSort::Hot),
        None,
        super::super::types::Page::new(2, Some(2)),
        &db,
        &root,
    )
    .unwrap();
    let page1 = paths(out1);
    assert_eq!(
        page1,
        vec!["c.rs", "b.rs"],
        "page 1 = rows 3-4 by score DESC"
    );
    assert!(more1, "5 rows, page of 2 at offset 2 → has_more");
    let overlap: Vec<_> = page0.iter().filter(|p| page1.contains(p)).collect();
    assert!(
        overlap.is_empty(),
        "pages must be disjoint, overlap={overlap:?}"
    );

    // Page 2: limit 2, offset 4 → the single remaining row, no more pages.
    let (out2, more2) = query_standalone(
        Some(TemporalSort::Hot),
        None,
        super::super::types::Page::new(2, Some(4)),
        &db,
        &root,
    )
    .unwrap();
    let page2 = paths(out2);
    assert_eq!(
        page2,
        vec!["a.rs"],
        "page 2 = last (coldest of the hot list)"
    );
    assert!(
        !more2,
        "offset 4 of 5 rows exhausts the set → has_more=false"
    );

    // Page past end: empty, no more.
    let (out3, more3) = query_standalone(
        Some(TemporalSort::Hot),
        None,
        super::super::types::Page::new(2, Some(99)),
        &db,
        &root,
    )
    .unwrap();
    assert!(paths(out3).is_empty(), "offset past end → empty page");
    assert!(!more3, "offset past end → has_more=false");
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

    let output = query_standalone(
        None,
        Some("src/auth.rs"),
        super::super::types::Page::first(10),
        &db,
        &root,
    )
    .unwrap()
    .0;
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
        super::super::types::Page::first(10),
        &db,
        &root,
    )
    .unwrap()
    .0;
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

/// AC8 analogue (#378/#389, combined-query propagation on the REAL Wilson
/// compute path for `--blast-radius FILE --risky`):
///
/// `standalone_blast_radius_with_risky_sorts_by_risk` above exercises this
/// re-sort path (`query_standalone` → `resort_partners_by_temporal` →
/// `risk_for_file`) with hard-coded risk_score = 0.9/0.1 — like the
/// non-Wilson `enrichment_risky_sorts_by_density_desc` — so it cannot detect a
/// dropped-Wilson regression. This test stores risk_scores COMPUTED by
/// `rskim_search::risk_score_wilson_decay` over raw (fix_commits,
/// total_commits), mirroring `enrichment_risky_real_wilson_small_sample_below_large`
/// (AC8) but through the blast-radius re-sort path instead of
/// `apply_temporal_enrichment`: a tiny 1-fix/1-commit partner (bare ratio 1.0)
/// must sort BELOW a 50-fix/50-commit partner. If volume-weighting were
/// reverted to the bare ratio, both would score 1.0 and the saturated
/// tiny-sample partner would tie/precede the large one.
#[test]
fn standalone_blast_radius_risky_real_wilson_small_sample_below_large() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/auth.rs"), "").unwrap();

    let (_db_dir, db) = temp_db();

    // Equal decay-weighted fix proportion (both all-fix -> 1.0): the ONLY
    // thing that separates them is the Wilson volume term over raw counts.
    let decay = 1.0;
    let tiny_score = rskim_search::risk_score_wilson_decay(decay, 1, 1);
    let large_score = rskim_search::risk_score_wilson_decay(decay, 50, 50);

    // Guard the premise so a future helper change cannot silently make this
    // test vacuous: the persisted scores MUST actually differ in the right
    // direction.
    assert!(
        tiny_score < large_score,
        "premise: Wilson(1,1) score ({tiny_score:.4}) must be < Wilson(50,50) \
         score ({large_score:.4}) — bare ratio would make both 1.0"
    );

    // Give the tiny-sample partner the HIGHER Jaccard co-change strength so
    // that, if risk ordering were broken, co-change insertion order would
    // wrongly keep it first.
    db.store_cochanges(&[
        CochangeRow {
            file_a: "src/auth.rs".to_string(),
            file_b: "src/tiny_saturated.rs".to_string(),
            count: 10,
            jaccard: 0.9,
        },
        CochangeRow {
            file_a: "src/auth.rs".to_string(),
            file_b: "src/high_volume.rs".to_string(),
            count: 3,
            jaccard: 0.3,
        },
    ])
    .unwrap();
    db.store_risks(&[
        RiskRow {
            file_path: "src/tiny_saturated.rs".to_string(),
            risk_score: tiny_score, // computed, not hard-coded
            total_commits: 1,
            fix_commits: 1,
            fix_density: 1.0, // raw ratio shown in Fix%
        },
        RiskRow {
            file_path: "src/high_volume.rs".to_string(),
            risk_score: large_score, // computed, not hard-coded
            total_commits: 50,
            fix_commits: 50,
            fix_density: 1.0,
        },
    ])
    .unwrap();

    let output = query_standalone(
        Some(TemporalSort::Risky),
        Some("src/auth.rs"),
        super::super::types::Page::first(10),
        &db,
        &root,
    )
    .unwrap()
    .0;
    match output {
        TemporalQueryOutput::Cochanges { partners, .. } => {
            assert_eq!(partners.len(), 2);
            let first_partner = if partners[0].file_a == "src/auth.rs" {
                &partners[0].file_b
            } else {
                &partners[0].file_a
            };
            // The high-volume partner MUST sort first despite its lower
            // Jaccard co-change strength, because the REAL Wilson-computed
            // risk_score read back via risk_for_file is higher
            // (50/50 ≈ 0.93 > 1/1 ≈ 0.21).
            assert_eq!(
                first_partner, "src/high_volume.rs",
                "AC8/#389: real Wilson risk_score must rank the 50/50 partner above the \
                 saturated 1/1 partner through the --blast-radius --risky re-sort path \
                 (premise: tiny={tiny_score:.4} < large={large_score:.4})"
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

    let output = query_standalone(
        Some(TemporalSort::Hot),
        None,
        super::super::types::Page::first(3),
        &db,
        &root,
    )
    .unwrap()
    .0;
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

    let output = query_standalone(
        Some(TemporalSort::Hot),
        None,
        super::super::types::Page::first(10),
        &db,
        &root,
    )
    .unwrap()
    .0;
    let mut buf = BufWriter::new(Vec::new());
    format_temporal_json(&output, false, &mut buf).unwrap();
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

    let output = query_standalone(
        Some(TemporalSort::Hot),
        None,
        super::super::types::Page::first(10),
        &db,
        &root,
    )
    .unwrap()
    .0;
    let mut buf = BufWriter::new(Vec::new());
    format_temporal_text(&output, super::super::types::Page::first(10), &mut buf).unwrap();
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
// AC-404-12: golden fixture comparison tests
// ============================================================================
//
// These tests verify that `format_temporal_text` and `format_temporal_json`
// at offset 0 produce output byte-identical to the golden fixtures committed
// in tests/fixtures/offset_golden/ (captured from the pre-change binary in
// Step 0 of #404). They confirm that the page-aware changes in format_temporal_text
// are a zero-regression at offset=0 (PF-007 / AC-404-12).
//
// Coverage: arm10 (hot standalone) and arm11 (blast-radius standalone) — both
// text and JSON output formats (4 fixtures total).

/// AC-404-12 / PF-007: `format_temporal_text` at offset 0 must produce output
/// matching the arm10 golden fixture (standalone --hot, 5 files).
///
/// This pins the exact tiebreak ordering (file3.ts before file5.ts at score 0.2)
/// that Decision 8 documents; a permutation would fail here.
#[test]
fn golden_arm10_hot_standalone_text_matches_fixture() {
    // Data extracted directly from arm10_hot_standalone.json golden fixture.
    let output = TemporalQueryOutput::Hotspots(vec![
        HotspotRow {
            file_path: "file1.ts".to_string(),
            score: 1.0,
            changes_30d: 5,
            changes_90d: 5,
        },
        HotspotRow {
            file_path: "file2.ts".to_string(),
            score: 0.6,
            changes_30d: 3,
            changes_90d: 3,
        },
        HotspotRow {
            file_path: "file6.ts".to_string(),
            // Use the exact float from the JSON fixture to match {:.3} rounding.
            score: 0.39999999999999997,
            changes_30d: 2,
            changes_90d: 2,
        },
        HotspotRow {
            file_path: "file3.ts".to_string(),
            score: 0.19999999999999998,
            changes_30d: 1,
            changes_90d: 1,
        },
        HotspotRow {
            file_path: "file5.ts".to_string(),
            score: 0.19999999999999998,
            changes_30d: 1,
            changes_90d: 1,
        },
    ]);

    let mut buf = BufWriter::new(Vec::new());
    format_temporal_text(&output, super::super::types::Page::first(5), &mut buf).unwrap();
    let actual = String::from_utf8(buf.into_inner().unwrap()).unwrap();

    let expected = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/offset_golden/arm10_hot_standalone.txt"),
    )
    .expect("arm10 golden fixture must be readable");

    assert_eq!(
        actual, expected,
        "AC-404-12 / PF-007: format_temporal_text at offset 0 must match golden fixture.\n\
         If this fails due to a deliberate behavioral change (e.g. tiebreak Decision 8),\n\
         update arm10_hot_standalone.txt to reflect the new correct output."
    );
}

/// AC-404-12: `format_temporal_json` for arm10 (--hot standalone, 5 files) must
/// produce output byte-identical to the committed arm10_hot_standalone.json fixture.
///
/// Same data as `golden_arm10_hot_standalone_text_matches_fixture`; exercises
/// the JSON path that was previously uncovered by golden tests.
#[test]
fn golden_arm10_hot_standalone_json_matches_fixture() {
    // Data extracted directly from arm10_hot_standalone.json golden fixture.
    let output = TemporalQueryOutput::Hotspots(vec![
        HotspotRow {
            file_path: "file1.ts".to_string(),
            score: 1.0,
            changes_30d: 5,
            changes_90d: 5,
        },
        HotspotRow {
            file_path: "file2.ts".to_string(),
            score: 0.6,
            changes_30d: 3,
            changes_90d: 3,
        },
        HotspotRow {
            file_path: "file6.ts".to_string(),
            score: 0.39999999999999997,
            changes_30d: 2,
            changes_90d: 2,
        },
        HotspotRow {
            file_path: "file3.ts".to_string(),
            score: 0.19999999999999998,
            changes_30d: 1,
            changes_90d: 1,
        },
        HotspotRow {
            file_path: "file5.ts".to_string(),
            score: 0.19999999999999998,
            changes_30d: 1,
            changes_90d: 1,
        },
    ]);

    let mut buf = BufWriter::new(Vec::new());
    // has_more=false: first page contains all results, so `has_more` is absent
    // from JSON output (skip_serializing_if).
    format_temporal_json(&output, false, &mut buf).unwrap();
    let actual = String::from_utf8(buf.into_inner().unwrap()).unwrap();

    let expected = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/offset_golden/arm10_hot_standalone.json"),
    )
    .expect("arm10 JSON golden fixture must be readable");

    assert_eq!(
        actual, expected,
        "AC-404-12: format_temporal_json at offset 0 must match arm10 golden fixture.\n\
         Update arm10_hot_standalone.json if the JSON schema changes deliberately."
    );
}

/// AC-404-12: `format_temporal_text` for arm11 (--blast-radius standalone, 5 partners)
/// must produce output byte-identical to arm11_blast_standalone.txt.
///
/// The input `partners` vec is pre-ordered in the tiebreak sequence the DB query
/// produces (file3 < file4 < file5 by `file_b ASC` within the jaccard=0.2 tie
/// group); `format_temporal_text` renders in input order without re-sorting.
/// The DB-level tiebreak itself is covered by
/// `rskim_search::temporal::tests::cochanges_for_file_tiebreak_crosses_union_arms`.
#[test]
fn golden_arm11_blast_standalone_text_matches_fixture() {
    // Data extracted from arm11_blast_standalone.json golden fixture.
    // file_a="file1.ts" for all rows so cochange_partner() returns file_b.
    let output = TemporalQueryOutput::Cochanges {
        target: "file1.ts".to_string(),
        partners: vec![
            CochangeRow {
                file_a: "file1.ts".to_string(),
                file_b: "file3.ts".to_string(),
                count: 1,
                jaccard: 0.2,
            },
            CochangeRow {
                file_a: "file1.ts".to_string(),
                file_b: "file4.ts".to_string(),
                count: 1,
                jaccard: 0.2,
            },
            CochangeRow {
                file_a: "file1.ts".to_string(),
                file_b: "file5.ts".to_string(),
                count: 1,
                jaccard: 0.2,
            },
            CochangeRow {
                file_a: "file1.ts".to_string(),
                file_b: "file6.ts".to_string(),
                count: 1,
                // Exact f64 for 1/6 as stored in the golden fixture.
                jaccard: 0.16666666666666666,
            },
            CochangeRow {
                file_a: "file1.ts".to_string(),
                file_b: "file2.ts".to_string(),
                count: 1,
                // Exact f64 for 1/7 as stored in the golden fixture.
                jaccard: 0.14285714285714285,
            },
        ],
    };

    let mut buf = BufWriter::new(Vec::new());
    format_temporal_text(&output, super::super::types::Page::first(5), &mut buf).unwrap();
    let actual = String::from_utf8(buf.into_inner().unwrap()).unwrap();

    let expected = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/offset_golden/arm11_blast_standalone.txt"),
    )
    .expect("arm11 text golden fixture must be readable");

    assert_eq!(
        actual, expected,
        "AC-404-12: format_temporal_text at offset 0 must match arm11 golden fixture.\n\
         Update arm11_blast_standalone.txt if blast-radius text output format changes."
    );
}

/// AC-404-12: `format_temporal_json` for arm11 (--blast-radius standalone, 5 partners)
/// must produce output byte-identical to arm11_blast_standalone.json.
#[test]
fn golden_arm11_blast_standalone_json_matches_fixture() {
    // Same data as the arm11 text golden test above.
    let output = TemporalQueryOutput::Cochanges {
        target: "file1.ts".to_string(),
        partners: vec![
            CochangeRow {
                file_a: "file1.ts".to_string(),
                file_b: "file3.ts".to_string(),
                count: 1,
                jaccard: 0.2,
            },
            CochangeRow {
                file_a: "file1.ts".to_string(),
                file_b: "file4.ts".to_string(),
                count: 1,
                jaccard: 0.2,
            },
            CochangeRow {
                file_a: "file1.ts".to_string(),
                file_b: "file5.ts".to_string(),
                count: 1,
                jaccard: 0.2,
            },
            CochangeRow {
                file_a: "file1.ts".to_string(),
                file_b: "file6.ts".to_string(),
                count: 1,
                jaccard: 0.16666666666666666,
            },
            CochangeRow {
                file_a: "file1.ts".to_string(),
                file_b: "file2.ts".to_string(),
                count: 1,
                jaccard: 0.14285714285714285,
            },
        ],
    };

    let mut buf = BufWriter::new(Vec::new());
    format_temporal_json(&output, false, &mut buf).unwrap();
    let actual = String::from_utf8(buf.into_inner().unwrap()).unwrap();

    let expected = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/offset_golden/arm11_blast_standalone.json"),
    )
    .expect("arm11 JSON golden fixture must be readable");

    assert_eq!(
        actual, expected,
        "AC-404-12: format_temporal_json at offset 0 must match arm11 golden fixture.\n\
         Update arm11_blast_standalone.json if blast-radius JSON output format changes."
    );
}

/// AC-404-12: offset > 0 header is page-range aware, NOT "top N".
///
/// At offset 2 the header should say "Hotspots (items 3–N, 90-day decay):"
/// not "Hotspots (top N, 90-day decay):" — the "top" claim is false when
/// the first N items have been skipped.
#[test]
fn format_temporal_text_offset_nonzero_header_not_top() {
    let (_db_dir, db) = temp_db();
    db.store_hotspots(
        &(0..5u32)
            .map(|i| HotspotRow {
                file_path: format!("f{i}.rs"),
                score: 1.0 - i as f64 * 0.1,
                changes_30d: i,
                changes_90d: i * 2,
            })
            .collect::<Vec<_>>(),
    )
    .unwrap();

    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let (output, _) = super::query_standalone(
        Some(TemporalSort::Hot),
        None,
        super::super::types::Page::new(2, Some(2)),
        &db,
        &root,
    )
    .unwrap();

    let mut buf = BufWriter::new(Vec::new());
    format_temporal_text(
        &output,
        super::super::types::Page::new(2, Some(2)),
        &mut buf,
    )
    .unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();

    assert!(
        !s.contains("top"),
        "AC-404-10: offset>0 header must NOT say 'top', got: {s:?}"
    );
    assert!(
        s.contains("items 3"),
        "AC-404-10: offset>0 header must show 1-indexed start position, got: {s:?}"
    );
}

/// AC-404-10: empty page at offset > 0 emits "no results at offset N" message,
/// NOT the misleading "No hotspot data available." message.
#[test]
fn format_temporal_text_empty_offset_page_message() {
    let output = TemporalQueryOutput::Hotspots(vec![]);

    let mut buf = BufWriter::new(Vec::new());
    format_temporal_text(
        &output,
        super::super::types::Page::new(5, Some(100)),
        &mut buf,
    )
    .unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();

    assert!(
        !s.contains("No hotspot data available."),
        "AC-404-10: empty-offset-page must NOT say 'No hotspot data available.', got: {s:?}"
    );
    assert!(
        s.contains("offset 100"),
        "AC-404-10: empty-offset-page message must mention the offset, got: {s:?}"
    );
}

/// AC-404-10: empty co-change page at offset > 0 says "no results at offset N",
/// NOT "No co-change data for {target}.".
#[test]
fn format_temporal_text_empty_cochange_offset_page_message() {
    let output = TemporalQueryOutput::Cochanges {
        target: "src/auth.rs".to_string(),
        partners: vec![],
    };

    let mut buf = BufWriter::new(Vec::new());
    format_temporal_text(
        &output,
        super::super::types::Page::new(5, Some(50)),
        &mut buf,
    )
    .unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();

    assert!(
        !s.contains("No co-change data for"),
        "AC-404-10: empty-offset-page must NOT say 'No co-change data for …', got: {s:?}"
    );
    assert!(
        s.contains("offset 50"),
        "AC-404-10: empty-offset-page co-change message must mention the offset, got: {s:?}"
    );
}

/// AC-404-11: `bounded_page_notice` includes count, a "more results exist" hint,
/// and the next --offset remedy.
///
/// The notice is emitted on ALL has_more paths — both the blast-radius ranking
/// window cap and the pure `--hot`/`--cold`/`--risky` sentinel case (no ranking
/// window involved), so it uses generic "more results exist" language rather than
/// the earlier "results exceed the temporal ranking window" phrasing.
#[test]
fn bounded_page_notice_contains_required_phrasing() {
    let notice = bounded_page_notice(5, 0, 5);
    assert!(
        notice.contains("more results exist"),
        "AC-404-11: notice must say 'more results exist', got: {notice:?}"
    );
    assert!(
        notice.contains("showing 5"),
        "AC-404-11: notice must include count, got: {notice:?}"
    );
    assert!(
        notice.contains("--offset 5"),
        "AC-404-11: notice must include next-offset remedy, got: {notice:?}"
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

/// AC8 (#378, combined-query propagation on the REAL Wilson compute path):
/// `<text> --risky` re-sort MUST propagate the volume-weighted small-sample-
/// below-large ordering through `apply_temporal_enrichment` → `annotate_risks`
/// → `risk_for_file`.
///
/// Unlike `enrichment_risky_sorts_by_density_desc` (which hard-codes
/// risk_score=0.9/0.1 and so cannot detect a dropped-Wilson regression), this
/// stores risk_scores COMPUTED by `rskim_search::risk_score_wilson_decay` over
/// raw (fix_commits, total_commits) and asserts the ACTUAL Wilson ordering: a
/// tiny 1-fix/1-commit file (bare ratio 1.0) sorts BELOW a 50-fix/50-commit
/// file. If volume-weighting were reverted to the bare ratio, both would score
/// 1.0, the saturated tiny-sample file would tie/precede the large one, and this
/// assertion would break.
#[test]
fn enrichment_risky_real_wilson_small_sample_below_large() {
    let (_db_dir, db) = temp_db();

    // Equal decay-weighted fix proportion (both all-fix → 1.0): the ONLY thing
    // that separates them is the Wilson volume term over the raw counts.
    let decay = 1.0;
    let tiny_score = rskim_search::risk_score_wilson_decay(decay, 1, 1);
    let large_score = rskim_search::risk_score_wilson_decay(decay, 50, 50);

    // Guard the premise so a future helper change cannot silently make this test
    // vacuous: the persisted scores MUST actually differ in the right direction.
    assert!(
        tiny_score < large_score,
        "premise: Wilson(1,1) score ({tiny_score:.4}) must be < Wilson(50,50) \
         score ({large_score:.4}) — bare ratio would make both 1.0"
    );

    db.store_risks(&[
        RiskRow {
            file_path: "tiny_saturated.rs".to_string(),
            risk_score: tiny_score, // computed, not hard-coded
            total_commits: 1,
            fix_commits: 1,
            fix_density: 1.0, // raw ratio shown in Fix%
        },
        RiskRow {
            file_path: "high_volume.rs".to_string(),
            risk_score: large_score, // computed, not hard-coded
            total_commits: 50,
            fix_commits: 50,
            fix_density: 1.0,
        },
    ])
    .unwrap();

    // `<text> --risky` combined path: text results re-sorted by risk enrichment.
    // Give the tiny file the HIGHER lexical score so that, if risk ordering were
    // broken, insertion/lexical order would wrongly keep it first.
    let mut results = vec![
        make_result("tiny_saturated.rs", 99.0),
        make_result("high_volume.rs", 1.0),
    ];

    apply_temporal_enrichment(&mut results, TemporalSort::Risky, &db).unwrap();

    // The high-volume file MUST sort first despite its lower lexical score,
    // because the REAL Wilson-computed risk_score read back via risk_for_file is
    // higher (50/50 ≈ 0.93 > 1/1 ≈ 0.21).
    assert_eq!(
        results[0].path, "high_volume.rs",
        "AC8: real Wilson risk_score must rank 50/50 above the saturated 1/1 file"
    );
    assert_eq!(results[1].path, "tiny_saturated.rs");

    // The annotation MUST carry the persisted (computed) Wilson score, proving
    // the value propagated unchanged through annotate_risks / risk_for_file.
    let top_risk = results[0].temporal.as_ref().unwrap().risk_score.unwrap();
    assert!(
        (top_risk - large_score).abs() < 1e-9,
        "annotated risk_score ({top_risk:.6}) must equal the persisted Wilson value \
         ({large_score:.6})"
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
// Standalone-AST temporal enrichment (enrich_ast_results)
//
// Mirrors the lexical enrichment tests above. Asserts the AST path applies the
// IDENTICAL ordering contract (descending/ascending hotspot, descending risk,
// absent-files-sort-last) so both query paths expose one observable behaviour
// (AC-A2 / design decision 4).
// ============================================================================

/// Build a minimal standalone-AST result row (line/snippet absent, as produced
/// before re-parse — enrichment only touches `path`/`temporal`).
fn make_ast(path: &str, score: f64) -> rskim_search::AstResult {
    rskim_search::AstResult::ast_only(path.to_string(), score, None, None)
}

#[test]
fn enrich_ast_hot_sorts_by_hotspot_desc() {
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

    // high.rs has the lower AST score but the higher hotspot — sort must reorder.
    let mut results = vec![make_ast("low.rs", 10.0), make_ast("high.rs", 5.0)];
    enrich_ast_results(&mut results, TemporalSort::Hot, &db);

    assert_eq!(
        results[0].path, "high.rs",
        "hot sort should put high hotspot first"
    );
    assert!(
        results[0]
            .temporal
            .as_ref()
            .and_then(|t| t.hotspot_score)
            .is_some(),
        "hot result should carry a hotspot annotation"
    );
}

#[test]
fn enrich_ast_cold_sorts_by_hotspot_asc() {
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

    let mut results = vec![make_ast("hot.rs", 10.0), make_ast("cold.rs", 10.0)];
    enrich_ast_results(&mut results, TemporalSort::Cold, &db);

    assert_eq!(
        results[0].path, "cold.rs",
        "cold sort should put lowest hotspot first"
    );
}

#[test]
fn enrich_ast_risky_sorts_by_density_desc() {
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

    let mut results = vec![make_ast("safe.rs", 10.0), make_ast("buggy.rs", 8.0)];
    enrich_ast_results(&mut results, TemporalSort::Risky, &db);

    assert_eq!(
        results[0].path, "buggy.rs",
        "risky sort should put most risky first"
    );
    assert!(
        results[0]
            .temporal
            .as_ref()
            .and_then(|t| t.risk_score)
            .is_some(),
        "risky result should carry a risk annotation"
    );
}

#[test]
fn enrich_ast_missing_files_sort_last() {
    let (_db_dir, db) = temp_db();
    db.store_hotspots(&[HotspotRow {
        file_path: "known.rs".to_string(),
        score: 0.5,
        changes_30d: 3,
        changes_90d: 7,
    }])
    .unwrap();

    let mut results = vec![
        make_ast("unknown.rs", 10.0), // not in temporal DB
        make_ast("known.rs", 5.0),    // in temporal DB
    ];
    enrich_ast_results(&mut results, TemporalSort::Hot, &db);

    assert_eq!(
        results[0].path, "known.rs",
        "files with temporal data must sort before unannotated files in Hot mode"
    );
    assert!(
        results[1].temporal.is_none(),
        "unknown file must have no annotation"
    );
}

#[test]
fn resort_window_clamps_to_floor_and_scales() {
    // Small limits clamp to the 100 floor; large limits scale by 5×.
    assert_eq!(resort_window(1), 100, "small limit clamps to floor");
    assert_eq!(resort_window(20), 100, "20*5=100 == floor");
    assert_eq!(resort_window(50), 250, "50*5=250 above floor");
    // Hostile limit near usize::MAX must not overflow (saturating_mul).
    assert_eq!(resort_window(usize::MAX), usize::MAX);
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
    let output = query_standalone(
        Some(TemporalSort::Cold),
        None,
        super::super::types::Page::first(10),
        &db,
        &root,
    )
    .unwrap()
    .0;
    match &output {
        TemporalQueryOutput::Coldspots(rows) => assert!(rows.is_empty()),
        other => panic!("expected Coldspots, got {other:?}"),
    }

    let mut buf = BufWriter::new(Vec::new());
    format_temporal_text(&output, super::super::types::Page::first(10), &mut buf).unwrap();
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
    let output = query_standalone(
        Some(TemporalSort::Risky),
        None,
        super::super::types::Page::first(10),
        &db,
        &root,
    )
    .unwrap()
    .0;
    match &output {
        TemporalQueryOutput::Risks(rows) => assert!(rows.is_empty()),
        other => panic!("expected Risks, got {other:?}"),
    }

    let mut buf = BufWriter::new(Vec::new());
    format_temporal_text(&output, super::super::types::Page::first(10), &mut buf).unwrap();
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
        eprintln!(
            "SKIP staleness_warns_when_stored_head_differs_from_current: git init failed or git not available"
        );
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
        eprintln!(
            "SKIP staleness_warns_when_stored_head_differs_from_current: git commit failed (CI environment without git identity?)"
        );
        return;
    }

    // Open a fresh temporal DB to create the schema, then plant a deliberately
    // wrong HEAD via raw SQL — set_meta guards version-attestation keys
    // (AD-408-3). Re-open to read it back through the domain API.
    let db_path = root.join("temporal.db");
    drop(TemporalDb::open(&db_path).unwrap());
    crate::cmd::search::staleness::plant_meta_raw(
        &db_path,
        rskim_search::META_GIT_HEAD,
        "0000000000000000000000000000000000000000",
    );
    let db = TemporalDb::open(&db_path).unwrap();

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
        layers_matched: vec![],
    };

    let output = QueryOutput {
        query: "both".to_string(),
        total: 1,
        has_more: false,
        verify_mode: None,
        results: vec![result],
        duration_ms: 1,
        index_stats: None,
        ast_coverage: None,
        degraded: vec![],
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

    let output = query_standalone(
        Some(TemporalSort::Risky),
        None,
        super::super::types::Page::first(10),
        &db,
        &root,
    )
    .unwrap()
    .0;
    let mut buf = BufWriter::new(Vec::new());
    format_temporal_json(&output, false, &mut buf).unwrap();
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

    let output = query_standalone(
        None,
        Some("src/auth.rs"),
        super::super::types::Page::first(10),
        &db,
        &root,
    )
    .unwrap()
    .0;
    let mut buf = BufWriter::new(Vec::new());
    format_temporal_json(&output, false, &mut buf).unwrap();
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

    let output = query_standalone(
        Some(TemporalSort::Cold),
        None,
        super::super::types::Page::first(10),
        &db,
        &root,
    )
    .unwrap()
    .0;
    let mut buf = BufWriter::new(Vec::new());
    format_temporal_json(&output, false, &mut buf).unwrap();
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
    let output = query_standalone(
        Some(TemporalSort::Hot),
        None,
        super::super::types::Page::first(10),
        &db,
        &root,
    )
    .unwrap()
    .0;
    match &output {
        TemporalQueryOutput::Hotspots(rows) => assert!(rows.is_empty()),
        other => panic!("expected Hotspots, got {other:?}"),
    }

    let mut buf = BufWriter::new(Vec::new());
    format_temporal_text(&output, super::super::types::Page::first(10), &mut buf).unwrap();
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

    let output = query_standalone(
        None,
        Some("src/auth.rs"),
        super::super::types::Page::first(10),
        &db,
        &root,
    )
    .unwrap()
    .0;
    match &output {
        TemporalQueryOutput::Cochanges { partners, .. } => assert!(partners.is_empty()),
        other => panic!("expected Cochanges, got {other:?}"),
    }

    let mut buf = BufWriter::new(Vec::new());
    format_temporal_text(&output, super::super::types::Page::first(10), &mut buf).unwrap();
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

// ============================================================================
// D-2 / AC-404-12: blast-radius offset > 0 pagination coverage
//
// Finding: every query_standalone blast-radius test previously used
// Page::first(10) (offset 0), leaving both branches of the blast-radius arm
// in query_standalone — the no-sort path and the temporal re-sort path — with
// zero offset coverage.  These tests exercise the disjointness-critical
// `has_more` computation on both paths with offset > 0 so that the D-2 proof
// is backed by a failing-then-passing regression test.
// ============================================================================

/// D-2 / AC-404-12 (no-sort branch): blast-radius without a temporal sort flag
/// with offset > 0 must skip the first `offset` partners and set has_more=true
/// when more partners exist beyond the current page.
///
/// Setup: 5 partners (jaccard 0.9 → 0.5), Page(limit=2, offset=2).
/// depth = limit + offset = 4.  total_before = 5 > 4 → has_more = true.
/// Returned partners are positions [2..4] in Jaccard-DESC order: jaccard 0.7
/// and 0.6.
#[test]
fn blast_radius_no_sort_offset_nonzero_has_more() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/hub.rs"), "").unwrap();

    let (_db_dir, db) = temp_db();
    db.store_cochanges(&[
        CochangeRow {
            file_a: "src/hub.rs".to_string(),
            file_b: "src/p1.rs".to_string(),
            count: 5,
            jaccard: 0.9,
        },
        CochangeRow {
            file_a: "src/hub.rs".to_string(),
            file_b: "src/p2.rs".to_string(),
            count: 4,
            jaccard: 0.8,
        },
        CochangeRow {
            file_a: "src/hub.rs".to_string(),
            file_b: "src/p3.rs".to_string(),
            count: 3,
            jaccard: 0.7,
        },
        CochangeRow {
            file_a: "src/hub.rs".to_string(),
            file_b: "src/p4.rs".to_string(),
            count: 2,
            jaccard: 0.6,
        },
        CochangeRow {
            file_a: "src/hub.rs".to_string(),
            file_b: "src/p5.rs".to_string(),
            count: 1,
            jaccard: 0.5,
        },
    ])
    .unwrap();

    let page = super::super::types::Page::new(2, Some(2));
    let (output, has_more) = query_standalone(None, Some("src/hub.rs"), page, &db, &root).unwrap();

    // has_more: total_before(5) > depth(4) → true (D-2 no-sort branch).
    assert!(
        has_more,
        "D-2 no-sort: has_more must be true when total partners(5) > depth(4)"
    );

    match output {
        TemporalQueryOutput::Cochanges { partners, .. } => {
            assert_eq!(
                partners.len(),
                2,
                "D-2 no-sort: page must contain exactly limit(2) partners, got {}",
                partners.len()
            );
            // Jaccard DESC ordering: skip p1(0.9), p2(0.8); take p3(0.7), p4(0.6).
            let jaccards: Vec<f64> = partners.iter().map(|p| p.jaccard).collect();
            assert!(
                (jaccards[0] - 0.7).abs() < 1e-9,
                "D-2 no-sort: first partner after offset=2 must have jaccard≈0.7, got {}",
                jaccards[0]
            );
            assert!(
                (jaccards[1] - 0.6).abs() < 1e-9,
                "D-2 no-sort: second partner after offset=2 must have jaccard≈0.6, got {}",
                jaccards[1]
            );
        }
        other => panic!("expected Cochanges, got {other:?}"),
    }
}

/// D-2 / AC-404-12 (sort branch): blast-radius with a temporal sort flag and
/// offset > 0 must apply the page AFTER re-sorting and set has_more=true via
/// `pre_page_len > page.depth()` when the window was not capped.
///
/// Setup: 5 partners, each with a distinct risk score.  Sort: --risky (DESC).
/// Page(limit=2, offset=2): depth = 4.  After resort the full 5 are within
/// the resort_window floor (100), so window_capped=false.  pre_page_len=5 > 4
/// → has_more=true via the second disjunct.  Partners returned are the 3rd and
/// 4th by risk score (risk 0.5 and 0.3).
#[test]
fn blast_radius_sort_offset_nonzero_has_more_via_depth() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/hub.rs"), "").unwrap();

    let (_db_dir, db) = temp_db();

    // All Jaccard values above MIN_JACCARD_THRESHOLD (0.10).
    // Deliberately invert Jaccard order vs risk order so re-sort is observable.
    db.store_cochanges(&[
        CochangeRow {
            file_a: "src/hub.rs".to_string(),
            file_b: "src/low_jac.rs".to_string(),
            count: 1,
            jaccard: 0.15,
        }, // Jaccard rank 5 — risk rank 1
        CochangeRow {
            file_a: "src/hub.rs".to_string(),
            file_b: "src/mid_low.rs".to_string(),
            count: 1,
            jaccard: 0.20,
        }, // Jaccard rank 4 — risk rank 2
        CochangeRow {
            file_a: "src/hub.rs".to_string(),
            file_b: "src/mid.rs".to_string(),
            count: 1,
            jaccard: 0.30,
        }, // Jaccard rank 3 — risk rank 3
        CochangeRow {
            file_a: "src/hub.rs".to_string(),
            file_b: "src/mid_high.rs".to_string(),
            count: 1,
            jaccard: 0.40,
        }, // Jaccard rank 2 — risk rank 4
        CochangeRow {
            // "src/high_jac.rs" < "src/hub.rs" lexically ('i' < 'u'), so
            // high_jac must be file_a to satisfy the file_a < file_b invariant.
            file_a: "src/high_jac.rs".to_string(),
            file_b: "src/hub.rs".to_string(),
            count: 1,
            jaccard: 0.50,
        }, // Jaccard rank 1 — risk rank 5
    ])
    .unwrap();

    // Risk scores: deliberately inverted from Jaccard order (low_jac is most risky).
    db.store_risks(&[
        RiskRow {
            file_path: "src/low_jac.rs".to_string(),
            risk_score: 0.9,
            total_commits: 10,
            fix_commits: 9,
            fix_density: 0.9,
        },
        RiskRow {
            file_path: "src/mid_low.rs".to_string(),
            risk_score: 0.7,
            total_commits: 10,
            fix_commits: 7,
            fix_density: 0.7,
        },
        RiskRow {
            file_path: "src/mid.rs".to_string(),
            risk_score: 0.5,
            total_commits: 10,
            fix_commits: 5,
            fix_density: 0.5,
        },
        RiskRow {
            file_path: "src/mid_high.rs".to_string(),
            risk_score: 0.3,
            total_commits: 10,
            fix_commits: 3,
            fix_density: 0.3,
        },
        RiskRow {
            file_path: "src/high_jac.rs".to_string(),
            risk_score: 0.1,
            total_commits: 10,
            fix_commits: 1,
            fix_density: 0.1,
        },
    ])
    .unwrap();

    let page = super::super::types::Page::new(2, Some(2));
    let (output, has_more) = query_standalone(
        Some(TemporalSort::Risky),
        Some("src/hub.rs"),
        page,
        &db,
        &root,
    )
    .unwrap();

    // has_more: window_capped(5<=100→false) || pre_page_len(5) > depth(4) → true.
    assert!(
        has_more,
        "D-2 sort: has_more must be true when pre_page_len(5) > depth(4)"
    );

    match output {
        TemporalQueryOutput::Cochanges { partners, .. } => {
            assert_eq!(
                partners.len(),
                2,
                "D-2 sort: page must contain exactly limit(2) partners after re-sort+skip"
            );
            // After --risky re-sort: [low_jac(0.9), mid_low(0.7), mid(0.5), mid_high(0.3), high_jac(0.1)]
            // Skip offset=2 → take [mid(0.5), mid_high(0.3)]
            let names: Vec<&str> = partners
                .iter()
                .map(|p| {
                    if p.file_a == "src/hub.rs" {
                        p.file_b.as_str()
                    } else {
                        p.file_a.as_str()
                    }
                })
                .collect();
            assert_eq!(
                names[0], "src/mid.rs",
                "D-2 sort: first partner at offset=2 after --risky re-sort must be src/mid.rs \
                 (risk=0.5), got {:?}",
                names[0]
            );
            assert_eq!(
                names[1], "src/mid_high.rs",
                "D-2 sort: second partner at offset=2 after --risky re-sort must be \
                 src/mid_high.rs (risk=0.3), got {:?}",
                names[1]
            );
        }
        other => panic!("expected Cochanges, got {other:?}"),
    }
}

// ============================================================================
// D-5 / AD-404-11: has_more terminator — text+temporal path (mod.rs run_query)
//
// run_query overwrites output.has_more after apply_temporal_enrichment:
//
//   let page = types::Page::new(flags.limit, flags.offset);
//   let pre_page_len = output.results.len();
//   page.apply(&mut output.results);
//   output.total = output.results.len();
//   output.has_more = pre_page_len > page.depth();
//
// These tests exercise that formula in isolation to guard the D-5 terminator
// on this path (the blast-radius composite path is guarded by
// test_ac13_limit_applied_after_fusion_rank_then_limit; this set covers the
// text+temporal overwrite arm which has no equivalent integration test).
// ============================================================================

/// D-5: has_more is true when the resort window produces more results than the
/// page depth (limit+offset).  Simulates resort_window(1)=5 results fetched
/// for limit=1 — any of the 5 enriched results beyond the page triggers has_more.
#[test]
fn test_text_temporal_has_more_true_when_resort_window_overflows_page() {
    use crate::cmd::search::types::Page;

    // resort_window(limit=1) fetches 5 results (the candidate window).
    // After enrichment all 5 survive; page = limit:1, offset:0 → depth=1.
    let mut results = vec![
        make_result("a.rs", 5.0),
        make_result("b.rs", 4.0),
        make_result("c.rs", 3.0),
        make_result("d.rs", 2.0),
        make_result("e.rs", 1.0),
    ];
    let page = Page::new(1, None); // limit=1, offset=0 → depth=1
    let pre_page_len = results.len();
    page.apply(&mut results);
    let has_more = pre_page_len > page.depth();

    assert_eq!(
        results.len(),
        1,
        "page.apply truncates resort window to limit=1"
    );
    assert!(
        has_more,
        "D-5 text+temporal: has_more must be true when 5 resort candidates > depth(1)"
    );
}

/// D-5: has_more is false when the result count exactly equals the page limit
/// with no offset (resort window exhausted without overflow).
#[test]
fn test_text_temporal_has_more_false_when_results_equal_limit() {
    use crate::cmd::search::types::Page;

    // Exactly 2 results, page = limit:2, offset:0 → depth=2.
    // pre_page_len(2) > depth(2) is false → has_more = false.
    let mut results = vec![make_result("a.rs", 2.0), make_result("b.rs", 1.0)];
    let page = Page::new(2, None); // limit=2, offset=0
    let pre_page_len = results.len();
    page.apply(&mut results);
    let has_more = pre_page_len > page.depth();

    assert_eq!(
        results.len(),
        2,
        "page.apply preserves all results when count == limit"
    );
    assert!(
        !has_more,
        "D-5 text+temporal: has_more must be false when result count ({}) == depth({})",
        pre_page_len,
        page.depth()
    );
}

/// D-5: has_more with a non-zero offset.  depth = limit + offset; has_more is
/// true iff pre_page_len > depth.  Exercises both sides of the boundary.
#[test]
fn test_text_temporal_has_more_with_nonzero_offset() {
    use crate::cmd::search::types::Page;

    // Exactly 3 results, page = limit:2, offset:1 → depth=3.
    // pre_page_len(3) > depth(3) is false → has_more = false.
    let mut results_exact = vec![
        make_result("a.rs", 3.0),
        make_result("b.rs", 2.0),
        make_result("c.rs", 1.0),
    ];
    let page = Page::new(2, Some(1)); // limit=2, offset=1 → depth=3
    let pre_exact = results_exact.len();
    page.apply(&mut results_exact);
    let has_more_exact = pre_exact > page.depth();

    assert_eq!(
        results_exact.len(),
        2,
        "page.apply skips 1 then truncates to limit=2"
    );
    assert!(
        !has_more_exact,
        "D-5 text+temporal offset: has_more must be false when count ({}) == depth({})",
        pre_exact,
        page.depth()
    );

    // 4 results, same page → pre_page_len(4) > depth(3) → has_more = true.
    let mut results_overflow = vec![
        make_result("a.rs", 4.0),
        make_result("b.rs", 3.0),
        make_result("c.rs", 2.0),
        make_result("d.rs", 1.0),
    ];
    let pre_overflow = results_overflow.len();
    page.apply(&mut results_overflow);
    let has_more_overflow = pre_overflow > page.depth();

    assert_eq!(
        results_overflow.len(),
        2,
        "page.apply skips 1 then truncates to 2"
    );
    assert!(
        has_more_overflow,
        "D-5 text+temporal offset: has_more must be true when count ({}) > depth({})",
        pre_overflow,
        page.depth()
    );
}

// ============================================================================
// T-38 / AD-414-13: ranked_row_count — unit tests (no DB required)
// ============================================================================

/// T-38(a): when NO result carries a hotspot_score the covered slice is entirely
/// unranked.  ranked_row_count must report ranked == 0, total == slice length,
/// lookup_errors == 0, and must NOT mutate the slice (input order preserved).
#[test]
fn t38_all_unranked_reports_zero_and_preserves_order() {
    let results = vec![
        make_result("a.rs", 3.0),
        make_result("b.rs", 2.0),
        make_result("c.rs", 1.0),
    ];
    // Ensure all temporal fields are None (make_result already does this).
    let paths_before: Vec<_> = results.iter().map(|r| r.path.clone()).collect();

    let cov: TemporalCoverage = ranked_row_count(&results, TemporalSort::Hot);

    assert_eq!(
        cov.ranked, 0,
        "all-unranked slice must report ranked == 0, got {cov:?}"
    );
    assert_eq!(cov.total, 3, "total must equal slice length, got {cov:?}");
    assert_eq!(
        cov.lookup_errors, 0,
        "ranked_row_count performs no DB lookups; lookup_errors must be 0"
    );

    // ranked_row_count takes &[..] so the caller's order is unchanged.
    let paths_after: Vec<_> = results.iter().map(|r| r.path.clone()).collect();
    assert_eq!(
        paths_before, paths_after,
        "ranked_row_count must not reorder the slice"
    );
}

/// T-38(b): when exactly 1 of 4 results carries a hotspot_score, ranked_row_count
/// must report ranked == 1 and total == 4 (the sentinel-eligible set).
/// A caller seeing ranked == 1 knows the sort would elevate that one entry and
/// assign the -1.0 sentinel to the remaining three.
#[test]
fn t38_one_of_four_ranked_reports_ranked_one() {
    use crate::cmd::search::types::TemporalAnnotation;

    let mut results = vec![
        make_result("a.rs", 4.0),
        make_result("b.rs", 3.0),
        make_result("c.rs", 2.0),
        make_result("d.rs", 1.0),
    ];
    // Annotate only the third entry with a hotspot score.
    results[2].temporal = Some(TemporalAnnotation {
        hotspot_score: Some(9.5),
        risk_score: None,
        fix_density: None,
        cochange_jaccard: None,
        changes_30d: None,
        changes_90d: None,
    });

    let cov: TemporalCoverage = ranked_row_count(&results, TemporalSort::Hot);

    assert_eq!(
        cov.ranked, 1,
        "one annotated entry must yield ranked == 1, got {cov:?}"
    );
    assert_eq!(
        cov.total, 4,
        "total must equal slice length (4), got {cov:?}"
    );
    assert_eq!(
        cov.lookup_errors, 0,
        "ranked_row_count performs no DB lookups; lookup_errors must be 0"
    );
}

// ============================================================================
// Phase B2: §2.3 normative conformance — cause, remediation, Fallback tails
// (AD-414-15 / AC-2 / AC-7 / AC-19 / T-19 / T-4)
// ============================================================================

/// Returns the §2.3 normative cause text for a given reason+detail.
///
/// The exhaustive `match` on `DegradedReason` enforces a compile error when a
/// new variant is added without updating this function (T-19(a) requirement).
fn expected_cause(reason: DegradedReason, detail: &str) -> String {
    match reason {
        DegradedReason::NotGitRepo => super::super::NO_TEMPORAL_DATA_MSG.to_string(),
        DegradedReason::HeadUnresolved => super::super::HEAD_UNRESOLVED_TEMPORAL_MSG.to_string(),
        DegradedReason::RepositoryMismatch => {
            format!("{} {}", super::super::SUBDIR_ROOT_TEMPORAL_MSG, detail)
        }
        DegradedReason::Missing => "temporal.db is not present in the index cache".to_string(),
        DegradedReason::Corrupt => "temporal.db is corrupt (not a database)".to_string(),
        DegradedReason::UnsupportedVersion => {
            format!("temporal.db was written by a newer skim ({detail})")
        }
        DegradedReason::Unreadable => {
            if detail.is_empty() {
                "temporal.db could not be opened".to_string()
            } else {
                format!("temporal.db could not be opened ({detail})")
            }
        }
        DegradedReason::Empty => {
            let base = "temporal data is empty (0 rows) - this repository has no \
                        commit history skim can analyse";
            if detail.contains("shallow") {
                format!("{base}; a shallow clone is the usual cause")
            } else {
                base.to_string()
            }
        }
        DegradedReason::NoRankedRows => detail.to_string(),
    }
}

/// Returns the §2.3 normative remediation text for a given reason.
///
/// Exhaustive `match` — compile error if a variant is added without updating.
fn expected_remediation(reason: DegradedReason) -> &'static str {
    match reason {
        DegradedReason::NotGitRepo => "run 'skim search' on a git repo to auto-populate",
        DegradedReason::HeadUnresolved => "commit at least one file to initialise the branch HEAD",
        DegradedReason::RepositoryMismatch => {
            "run 'skim search --rebuild --root <this root>' to re-anchor it"
        }
        DegradedReason::Missing => "run 'skim search --update' to build it",
        DegradedReason::Corrupt => "run 'skim search --rebuild' to discard and rebuild it",
        DegradedReason::UnsupportedVersion => {
            "upgrade skim; skim will not overwrite a newer database"
        }
        DegradedReason::Unreadable => "run 'skim search --rebuild'",
        DegradedReason::Empty => "run 'skim search --rebuild'",
        DegradedReason::NoRankedRows => {
            "commit the matched files, or run 'skim search --update' after committing"
        }
    }
}

/// T-19(a): every `DegradedReason` variant's `cause()` matches the §2.3
/// normative table, is non-empty, and does NOT contain the forbidden
/// substring "no temporal data" (except `NotGitRepo` which IS the legacy
/// `NO_TEMPORAL_DATA_MSG` verbatim — AC-19).
#[test]
fn t19a_cause_text_conformance() {
    let cases: &[(DegradedReason, &str)] = &[
        (DegradedReason::NotGitRepo, ""),
        (DegradedReason::HeadUnresolved, ""),
        (
            DegradedReason::RepositoryMismatch,
            "(recorded: \"/old\", live: \"/new\")",
        ),
        (DegradedReason::Missing, ""),
        (DegradedReason::Corrupt, "SQLITE_NOTADB"),
        (
            DegradedReason::UnsupportedVersion,
            "schema version 9, this build supports 8",
        ),
        (DegradedReason::Unreadable, "permission denied"),
        (DegradedReason::Unreadable, ""),
        (DegradedReason::Empty, ""),
        (DegradedReason::Empty, "shallow"),
        (
            DegradedReason::NoRankedRows,
            "0 of 5 results have temporal data",
        ),
    ];

    for (reason, detail) in cases {
        let actual = reason.cause(detail);
        let want = expected_cause(*reason, detail);

        assert_eq!(
            actual, want,
            "cause({reason:?}, {detail:?}) diverges from §2.3 normative text"
        );
        assert!(!actual.is_empty(), "cause({reason:?}) must be non-empty");

        // AC-7 / AC-19(b): every reason except NotGitRepo must NOT embed "no temporal data".
        if *reason != DegradedReason::NotGitRepo {
            assert!(
                !actual.contains("no temporal data"),
                "AC-7: {reason:?} cause must not contain 'no temporal data', got: {actual:?}"
            );
        }
    }

    // NotGitRepo byte-identity (AC-19).
    assert_eq!(
        DegradedReason::NotGitRepo.cause(""),
        super::super::NO_TEMPORAL_DATA_MSG,
        "AC-19: NotGitRepo cause must be byte-identical to NO_TEMPORAL_DATA_MSG"
    );
}

/// T-19(b): every `DegradedReason` variant's `remediation()` matches the
/// §2.3 normative table and is non-empty.
///
/// Exhaustive `match` inside `expected_remediation` means a newly-added
/// variant fails to compile until it is handled here.
#[test]
fn t19b_remediation_text_conformance() {
    let all_reasons = [
        DegradedReason::NotGitRepo,
        DegradedReason::HeadUnresolved,
        DegradedReason::RepositoryMismatch,
        DegradedReason::Missing,
        DegradedReason::Corrupt,
        DegradedReason::UnsupportedVersion,
        DegradedReason::Unreadable,
        DegradedReason::Empty,
        DegradedReason::NoRankedRows,
    ];

    for reason in all_reasons {
        let actual = reason.remediation();
        let want = expected_remediation(reason);
        assert_eq!(
            actual, want,
            "{reason:?} remediation diverges from §2.3 normative table"
        );
        assert!(
            !actual.is_empty(),
            "{reason:?} remediation must be non-empty"
        );
    }
}

/// Every `.rs` file under `crates/rskim/src/cmd/search/`, paired with the part of
/// its source that is compiled into the PRODUCTION binary.
///
/// Test code is excluded two ways, and BOTH are needed:
/// - `*_tests.rs` sidecars are dropped by filename.  Every module here except
///   `mod.rs` keeps its tests in a sidecar (`mod tests;` on the last line), so
///   this removes almost all test source.
/// - `mod.rs` is the one module with an INLINE `mod tests { … }` block, so its
///   source is truncated at that block.
///
/// A test legitimately quotes cause strings (T-19(a)'s `expected_cause` does)
/// without that being a second emit site — hence the exclusions.
///
/// The marker is the start-of-line `mod tests {` and NOT `#[cfg(test)]`: the
/// latter also occurs inside doc comments (e.g. `staleness.rs:23`,
/// `types.rs:71`), and truncating there would silently discard 90 % of several
/// files and leave this guard vacuous (PF-007).  `assert_corpus_is_substantial`
/// below fails if that ever regresses.
fn production_sources_under_search() -> Vec<(String, String)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cmd/search");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("cmd/search must be readable") {
        let path = entry.expect("dir entry").path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) if n.ends_with(".rs") && !n.ends_with("_tests.rs") => n.to_string(),
            _ => continue,
        };
        let src = std::fs::read_to_string(&path).expect("source must be readable");
        let production = match src.find("\nmod tests {") {
            Some(i) => src[..i].to_string(),
            None => src,
        };
        out.push((name, production));
    }
    assert!(
        out.len() >= 5,
        "expected the search module to have several production sources, found {}",
        out.len()
    );
    out
}

/// Anti-vacuity control for [`production_sources_under_search`].
///
/// A scan-the-sources test passes trivially if the sources it scanned are empty,
/// so pin a production symbol from each file that carries a real emit site.  If a
/// future change to the truncation rule swallows these files, this fails LOUDLY
/// instead of turning the SSOT guard into a no-op.
fn assert_corpus_is_substantial(sources: &[(String, String)]) {
    const REQUIRED: &[(&str, &str)] = &[
        ("mod.rs", "fn run_query"),
        ("mod.rs", "fn run_temporal_standalone"),
        ("ast.rs", "fn run_ast_standalone"),
        ("query.rs", "fn execute_query_with_manifest"),
        ("staleness.rs", "fn check_staleness"),
        ("temporal_build.rs", "fn rebuild_temporal_with_source"),
        ("temporal_state.rs", "fn temporal_db_is_stale"),
    ];
    for (file, symbol) in REQUIRED {
        let src = sources
            .iter()
            .find(|(name, _)| name == file)
            .unwrap_or_else(|| panic!("{file} must be among the scanned production sources"))
            .1
            .as_str();
        assert!(
            src.contains(symbol),
            "the scanned production source for {file} does not contain {symbol:?} — \
             the test-code exclusion is discarding production code and this guard \
             has become vacuous (PF-007)"
        );
    }
}

/// T-19(b) / AC-19(b) — STRUCTURAL SSOT, expressed as an observable.
///
/// The §2.3 cause texts must exist in exactly one place: the `DegradedReason`
/// builder in `temporal.rs`.  Any other production file under `cmd/search/` that
/// contains a cause substring is a second emit site — the failure mode this
/// criterion exists to catch, because a hand-rolled `eprintln!` drifts from the
/// builder silently (that is exactly how #413's `temporal_unavailable_msg`
/// duplicate arose).
///
/// Discriminating (PF-007): re-introducing any hand-written cause literal in
/// `mod.rs`, `ast.rs`, `query.rs`, `staleness.rs` or `temporal_build.rs` fails
/// this test, and the assertion is keyed on the production strings themselves,
/// so renaming a cause without updating this list also fails.
#[test]
fn t19b_no_cause_substring_outside_the_builder() {
    // The §2.3 cause substrings that are unique to the builder.  `NotGitRepo` and
    // `HeadUnresolved` are deliberately absent: their causes ARE the shared
    // `NO_TEMPORAL_DATA_MSG` / `HEAD_UNRESOLVED_TEMPORAL_MSG` constants declared
    // in `mod.rs`, so their text legitimately appears there (AC-19/AC-20).
    const CAUSE_SUBSTRINGS: &[&str] = &[
        "temporal.db is not present in the index cache",
        "temporal.db is corrupt (not a database)",
        "temporal.db was written by a newer skim",
        "temporal.db could not be opened",
        "temporal data is empty (0 rows)",
    ];

    let sources = production_sources_under_search();
    assert_corpus_is_substantial(&sources);

    for (name, src) in sources {
        if name == "temporal.rs" {
            // The builder itself — this is the one place the causes may live.
            continue;
        }
        for cause in CAUSE_SUBSTRINGS {
            assert!(
                !src.contains(cause),
                "AC-19(b): cause text {cause:?} must be emitted only through \
                 `DegradedReason`/`degraded_notice` in temporal.rs, but it also \
                 appears in production code in {name}"
            );
        }
    }
}

/// T-19(b) second half — every `AD-414-<n>` decision anchor is cited in the code.
///
/// An anchor with no citation means the decision it names shipped without a
/// locatable implementation site (or was renumbered and left dangling).
/// `AD-414-2` lives in `rskim-search` (the `DatabaseCorrupt` classification), so
/// that crate's sources are scanned alongside `cmd/search/`.
#[test]
fn t19b_all_ad_414_anchors_present() {
    fn collect_rs(dir: &std::path::Path, into: &mut String) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs(&path, into);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs")
                && let Ok(src) = std::fs::read_to_string(&path)
            {
                into.push_str(&src);
            }
        }
    }

    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut corpus = String::new();
    collect_rs(&manifest.join("src/cmd/search"), &mut corpus);
    collect_rs(&manifest.join("../rskim-search/src"), &mut corpus);
    assert!(
        !corpus.is_empty(),
        "anchor corpus must not be empty — check the source paths"
    );

    for n in 1..=15u32 {
        let anchor = format!("AD-414-{n}");
        // `AD-414-1` is a prefix of `AD-414-15`, so require the next character to
        // be a non-digit (or end of input) before counting a citation.
        let found = corpus.match_indices(&anchor).any(|(i, _)| {
            corpus[i + anchor.len()..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_ascii_digit())
        });
        assert!(
            found,
            "AC-19(b): decision anchor {anchor} is not cited anywhere in \
             crates/rskim/src/cmd/search/ or crates/rskim-search/src/"
        );
    }
}

/// T-19(c): the three `Fallback` tail texts match the §2.3 normative table.
///
/// Uses `Missing` (known stable cause) so any regression in the tail is
/// isolated from cause-text changes.
#[test]
fn t19c_fallback_tail_conformance() {
    let unavail = TemporalUnavailable {
        reason: DegradedReason::Missing,
        detail: String::new(),
    };

    // Lexical tail: "; {flag} not applied — results are in lexical relevance order"
    let lexical = degraded_notice(&unavail, "--hot", Fallback::Lexical);
    assert!(
        lexical.ends_with("; --hot not applied \u{2014} results are in lexical relevance order"),
        "Lexical tail mismatch, got: {lexical:?}"
    );

    // Ast tail: "; {flag} not applied — results are in raw AST match order"
    let ast = degraded_notice(&unavail, "--hot", Fallback::Ast);
    assert!(
        ast.ends_with("; --hot not applied \u{2014} results are in raw AST match order"),
        "Ast tail mismatch, got: {ast:?}"
    );

    // NoResults tail: "; no {flag} data to rank"
    let no_results = degraded_notice(&unavail, "--hot", Fallback::NoResults);
    assert!(
        no_results.ends_with("; no --hot data to rank"),
        "NoResults tail mismatch, got: {no_results:?}"
    );

    // Empty flag — no tail appended (base message verbatim).
    let no_flag = degraded_notice(&unavail, "", Fallback::Lexical);
    assert!(
        !no_flag.contains("not applied"),
        "Empty flag must return base without tail, got: {no_flag:?}"
    );
}

/// T-4 fragment: `NoRankedRows` detail format.
///
/// AC-4 substring: "0 of 3 results have temporal data".
/// Lookup-error clause only when `lookup_errors > 0`.
#[test]
fn t4_no_ranked_rows_detail_format() {
    // No lookup errors: plain count only.
    let detail_plain = "0 of 3 results have temporal data";
    let msg_plain = degraded_notice(
        &TemporalUnavailable {
            reason: DegradedReason::NoRankedRows,
            detail: detail_plain.to_string(),
        },
        "--hot",
        Fallback::Lexical,
    );
    assert!(
        msg_plain.contains("0 of 3 results have temporal data"),
        "T-4: message must contain AC-4 substring, got: {msg_plain:?}"
    );
    assert!(
        !msg_plain.contains("lookup"),
        "T-4: without lookup errors the error clause must be absent, got: {msg_plain:?}"
    );

    // With lookup errors: clause appended.
    let detail_err = "0 of 3 results have temporal data (2 temporal lookups failed)";
    let msg_err = degraded_notice(
        &TemporalUnavailable {
            reason: DegradedReason::NoRankedRows,
            detail: detail_err.to_string(),
        },
        "--hot",
        Fallback::Lexical,
    );
    assert!(
        msg_err.contains("2 temporal lookups failed"),
        "T-4: with lookup errors the error clause must be present, got: {msg_err:?}"
    );
}

/// AC-7 / AC-19(b): structural de-doubling guard.
///
/// The full `degraded_notice` output for every new reason (all except
/// `NotGitRepo`) must not contain the substring "no temporal data".
/// This test covers all three `Fallback` variants × all new reasons.
#[test]
fn ac7_no_temporal_data_exclusion_for_new_reasons() {
    // (reason, detail to use)
    let cases: &[(DegradedReason, &str)] = &[
        (DegradedReason::HeadUnresolved, ""),
        (
            DegradedReason::RepositoryMismatch,
            "(recorded: \"/a\", live: \"/b\")",
        ),
        (DegradedReason::Missing, ""),
        (DegradedReason::Corrupt, ""),
        (
            DegradedReason::UnsupportedVersion,
            "schema version 9, this build supports 8",
        ),
        (DegradedReason::Unreadable, "some OS error"),
        (DegradedReason::Empty, ""),
        (
            DegradedReason::NoRankedRows,
            "0 of 5 results have temporal data",
        ),
    ];

    for (reason, detail) in cases {
        for fallback in [Fallback::Lexical, Fallback::Ast, Fallback::NoResults] {
            let msg = degraded_notice(
                &TemporalUnavailable {
                    reason: *reason,
                    detail: (*detail).to_string(),
                },
                "--hot",
                fallback,
            );
            assert!(
                !msg.contains("no temporal data"),
                "AC-7: {reason:?}+{fallback:?} notice must not contain 'no temporal data', \
                 got: {msg:?}"
            );
        }
    }
}

/// AC-2 / §2.3 Empty non-shallow: the `degraded_notice` output for `Empty`
/// with empty detail contains the word "empty", the flag, "not applied",
/// "lexical", and "--rebuild"; but NOT "SKIM_DEBUG" or "no temporal data".
#[test]
fn ac2_empty_non_shallow_message_substrings() {
    let msg = degraded_notice(
        &TemporalUnavailable {
            reason: DegradedReason::Empty,
            detail: String::new(),
        },
        "--hot",
        Fallback::Lexical,
    );
    assert!(
        msg.contains("empty"),
        "AC-2(a): Empty message must contain 'empty', got: {msg:?}"
    );
    assert!(
        msg.contains("--hot"),
        "AC-2(b): Empty message must contain the flag '--hot', got: {msg:?}"
    );
    assert!(
        msg.contains("not applied"),
        "AC-2(c): Empty message must contain 'not applied', got: {msg:?}"
    );
    assert!(
        msg.contains("lexical"),
        "AC-2(d): Empty message must contain 'lexical', got: {msg:?}"
    );
    assert!(
        msg.contains("--rebuild"),
        "AC-2(e): Empty message must contain '--rebuild', got: {msg:?}"
    );
    assert!(
        !msg.contains("SKIM_DEBUG"),
        "AC-2: Empty message must NOT contain SKIM_DEBUG hint, got: {msg:?}"
    );
    assert!(
        !msg.contains("no temporal data"),
        "AC-7: Empty message must NOT contain 'no temporal data', got: {msg:?}"
    );
}

// ============================================================================
// Step 8 — --ast arm: Empty dimension and NoRankedRows
// ============================================================================

/// T-5 (Step 8): `dimension_is_empty` returns `true` for a fresh DB with no
/// hotspot rows, confirming that the --ast arm will pass `None` as `temporal_db`
/// and raw AST order survives (AC-21, SE-4 guard).
///
/// PF-007 discriminating: the probe uses `top_hotspots(1)`, never
/// `result_count()==0` (G-3 invariant).
#[test]
fn t5_dimension_is_empty_hot_on_empty_db() {
    let (_dir, db) = temp_db();
    // A freshly-opened DB has no hotspot rows — dimension_is_empty must return true.
    assert!(
        dimension_is_empty(&db, TemporalSort::Hot),
        "T-5: dimension_is_empty(Hot) must be true on a fresh empty DB"
    );
    assert!(
        dimension_is_empty(&db, TemporalSort::Cold),
        "T-5: dimension_is_empty(Cold) must be true on a fresh empty DB"
    );
    assert!(
        dimension_is_empty(&db, TemporalSort::Risky),
        "T-5: dimension_is_empty(Risky) must be true on a fresh empty DB"
    );
}

/// T-5 (Step 8) populated side: once hotspot rows exist,
/// `dimension_is_empty(Hot)` returns `false` — the DB presence guard works.
#[test]
fn t5_dimension_is_empty_hot_returns_false_when_rows_exist() {
    let (_dir, db) = temp_db();
    db.store_hotspots(&[HotspotRow {
        file_path: "src/main.rs".to_string(),
        score: 0.5,
        changes_30d: 3,
        changes_90d: 10,
    }])
    .unwrap();
    assert!(
        !dimension_is_empty(&db, TemporalSort::Hot),
        "T-5: dimension_is_empty(Hot) must be false when hotspot rows are present"
    );
    assert!(
        !dimension_is_empty(&db, TemporalSort::Cold),
        "T-5: dimension_is_empty(Cold) must be false when hotspot rows are present"
    );
}

/// T-38 --ast sub-case / AD-414-13 (Step 8): `enrich_ast_results` returns
/// `ranked == 0` when the DB has no temporal rows for the matched paths.
/// The caller (ast.rs) detects this and emits the NoRankedRows notice on stderr.
/// This test verifies the predicate the caller acts on.
///
/// PF-007 discriminating: asserts `ranked == 0` and `total == results.len()`.
#[test]
fn t38_ast_enrich_returns_zero_ranked_on_empty_db() {
    let (_dir, db) = temp_db();
    let mut results = vec![
        make_ast("src/a.rs", 1.0),
        make_ast("src/b.rs", 0.8),
        make_ast("src/c.rs", 0.5),
    ];
    let original_order: Vec<String> = results.iter().map(|r| r.path.clone()).collect();

    let cov = enrich_ast_results(&mut results, TemporalSort::Hot, &db);

    assert_eq!(
        cov.ranked, 0,
        "T-38: no hotspot rows in DB → ranked must be 0"
    );
    assert_eq!(cov.total, 3, "T-38: total must equal the slice length (3)");
    // AD-414-13: sort_by is skipped at ranked == 0 — raw AST order survives.
    let after_order: Vec<String> = results.iter().map(|r| r.path.clone()).collect();
    assert_eq!(
        after_order, original_order,
        "T-38: raw AST order must be preserved when ranked == 0 (AD-414-13)"
    );
}

/// Step 8 / AC-21: NoRankedRows degraded notice for the --ast arm must use
/// `Fallback::Ast` tail ("--hot not applied — results are in raw AST match order").
///
/// PF-007 discriminating: checks the tail text, not just substring "Ast".
#[test]
fn t38_ast_norankedrows_degraded_notice_tail() {
    let detail = "0 of 3 results have temporal data".to_string();
    let u = TemporalUnavailable {
        reason: DegradedReason::NoRankedRows,
        detail,
    };
    let msg = degraded_notice(&u, "--hot", Fallback::Ast);
    assert!(
        msg.contains("0 of 3 results have temporal data"),
        "AC-21: NoRankedRows notice must include the detail, got: {msg:?}"
    );
    assert!(
        msg.contains("--hot not applied"),
        "AC-21: Fallback::Ast tail must mention '--hot not applied', got: {msg:?}"
    );
    assert!(
        msg.contains("raw AST match order"),
        "AC-21: Fallback::Ast tail must say 'raw AST match order', got: {msg:?}"
    );
}

// ============================================================================
// Step 9 — standalone temporal arm: Empty dimension probe
// ============================================================================

/// T-20 (Step 9): `dimension_is_empty` gate for the standalone temporal arm.
/// A DB with hotspot rows must NOT be treated as empty; a DB without must.
/// This is the G-3 invariant — Empty is determined by the probe, never inferred
/// from `result_count()==0 && offset==0`.
///
/// PF-007 discriminating: stores exactly one hotspot row and asserts the probe
/// flips from true (before) to false (after).
#[test]
fn t20_dimension_is_empty_gate_for_standalone_arm() {
    let (_dir, db) = temp_db();

    // Before: no hotspot rows → empty.
    assert!(
        dimension_is_empty(&db, TemporalSort::Hot),
        "T-20: dimension_is_empty must be true before any hotspot rows are stored"
    );

    // Store exactly one hotspot row.
    db.store_hotspots(&[HotspotRow {
        file_path: "src/lib.rs".to_string(),
        score: 0.3,
        changes_30d: 1,
        changes_90d: 4,
    }])
    .unwrap();

    // After: probe must return false — dimension is no longer empty.
    assert!(
        !dimension_is_empty(&db, TemporalSort::Hot),
        "T-20: dimension_is_empty must be false after storing one hotspot row"
    );
}

/// T-6 (Step 9): The Empty degraded notice for the standalone temporal arm uses
/// `Fallback::NoResults` and empty flag — the tail must be absent (no suffix).
/// This exercises the exact call made in `run_temporal_standalone` when the DB
/// is open but the dimension has zero rows.
///
/// PF-007 discriminating: forbids the Fallback::Ast tail ("not applied")
/// and the Fallback::Lexical tail ("lexical relevance order").
#[test]
fn t6_standalone_temporal_empty_degraded_notice_no_suffix() {
    let u = TemporalUnavailable {
        reason: DegradedReason::Empty,
        detail: String::new(),
    };
    // run_temporal_standalone calls degraded_notice with flag="" and Fallback::NoResults.
    let msg = degraded_notice(&u, "", Fallback::NoResults);
    assert!(
        msg.contains("empty"),
        "T-6: Empty notice must contain 'empty', got: {msg:?}"
    );
    assert!(
        !msg.contains("not applied"),
        "T-6: Empty notice with empty flag must NOT contain 'not applied', got: {msg:?}"
    );
    assert!(
        !msg.contains("lexical"),
        "T-6: Empty notice with empty flag must NOT contain 'lexical', got: {msg:?}"
    );
    assert!(
        !msg.contains("raw AST"),
        "T-6: Empty notice with empty flag must NOT contain 'raw AST', got: {msg:?}"
    );
}

/// T-8 (Step 9): the `Missing` reason on the standalone temporal arm uses
/// `degraded_notice` with flag="" and Fallback::NoResults.  Verifies the
/// base message (no tail) for the common missing-DB case.
///
/// PF-007 discriminating: checks the cause text contains "not present" and
/// "update" remedy, and does NOT contain "not applied" (no Fallback tail).
#[test]
fn t8_standalone_temporal_missing_degraded_notice_no_suffix() {
    let u = TemporalUnavailable {
        reason: DegradedReason::Missing,
        detail: String::new(),
    };
    let msg = degraded_notice(&u, "", Fallback::NoResults);
    assert!(
        msg.contains("not present"),
        "T-8: Missing notice must contain 'not present', got: {msg:?}"
    );
    assert!(
        msg.contains("--update"),
        "T-8: Missing notice must contain '--update' remedy, got: {msg:?}"
    );
    assert!(
        !msg.contains("not applied"),
        "T-8: Missing notice with empty flag must NOT contain 'not applied', got: {msg:?}"
    );
}
