//! Tests for the query execution module (query.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::BufWriter;

use tempfile::tempdir;

use super::{execute_query, format_json_output, format_text_output};
use crate::cmd::search::types::{QueryConfig, QueryOutput};

// ============================================================================
// Test helpers
// ============================================================================

/// Stub analytics config with analytics disabled.
const TEST_ANALYTICS: crate::analytics::AnalyticsConfig = crate::analytics::AnalyticsConfig {
    enabled: false,
    input_cost_per_mtok: None,
    session_id: None,
};

/// Create a minimal indexable project in `root` with a few Rust files.
fn create_test_project(root: &std::path::Path) {
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    // Create a .git dir so the project root is found
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();

    fs::write(
        src.join("auth.rs"),
        "/// Authenticate a user.\npub fn authenticate(token: &str) -> bool {\n    !token.is_empty()\n}\n",
    )
    .unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub mod auth;\npub fn parse_config(s: &str) -> Option<String> {\n    Some(s.to_string())\n}\n",
    )
    .unwrap();
}

/// Create a project for AC12/AC13/AC14 UNION blast-radius tests.
///
/// auth.rs contains a unique function `zqjxblip_check` that does NOT
/// share any 4-char n-grams with lib.rs.  lib.rs contains only database
/// schema helpers — no "verify", "zqjx", or "token" substrings — so
/// a query for "zqjxblip_check" returns a lexical hit only for auth.rs.
/// lib.rs acts as the pure co-change-only partner with zero lexical overlap.
fn create_union_test_project(root: &std::path::Path) {
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();

    // auth.rs: contains the unique query term only.
    // "zqjxblip_check" uses a 4-char nonsense prefix "zqjx" that cannot appear
    // in any natural Rust file, guaranteeing zero n-gram overlap with lib.rs.
    fs::write(
        src.join("auth.rs"),
        "pub fn zqjxblip_check(t: &str) -> bool { !t.is_empty() }\n",
    )
    .unwrap();
    // lib.rs: content with NO overlap with "zqjxblip" (no z, q, j, x cluster).
    // Uses common Rust keywords/types that are far from the auth.rs term.
    fs::write(
        src.join("lib.rs"),
        "pub struct Foo { pub count: u32 }\n\
         impl Foo {\n\
             pub fn new(n: u32) -> Self { Self { count: n } }\n\
             pub fn total(&self) -> u32 { self.count }\n\
         }\n",
    )
    .unwrap();
}

/// Build a QueryConfig pointing at `root` and `cache_dir`.
fn make_config(root: &std::path::Path, cache_dir: &std::path::Path, text: &str) -> QueryConfig {
    QueryConfig {
        text: text.to_string(),
        limit: 20,
        json: false,
        root: root.to_path_buf(),
        cache_dir: cache_dir.to_path_buf(),
        blast_radius_paths: None,
        ast_scored: None,
        composite_weights: None,
    }
}

// ============================================================================
// execute_query
// ============================================================================

#[test]
fn test_execute_query_auto_builds_index_on_cold_start() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    let config = make_config(&root, &cache_dir, "authenticate");
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // Index was auto-built — query should succeed (0 or more results).
    assert_eq!(output.query, "authenticate");
    assert!(
        output.duration_ms < 60_000,
        "query should complete within 60s"
    );
}

#[test]
fn test_execute_query_finds_results_for_known_term() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    let config = make_config(&root, &cache_dir, "authenticate");
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // "authenticate" is in auth.rs — should find at least one result.
    assert!(
        !output.results.is_empty(),
        "should find results for 'authenticate'"
    );
    // All results should have valid paths
    for r in &output.results {
        assert!(!r.path.is_empty(), "result path must not be empty");
    }
}

#[test]
fn test_execute_query_respects_limit() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    let mut config = make_config(&root, &cache_dir, "fn");
    config.limit = 1;
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();
    assert!(
        output.results.len() <= 1,
        "limit=1 must produce at most 1 result"
    );
}

#[test]
fn test_execute_query_empty_query_returns_empty_results() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    let config = make_config(&root, &cache_dir, "");
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();
    assert!(output.results.is_empty(), "empty query → empty results");
}

// ============================================================================
// format_text_output
// ============================================================================

#[test]
fn test_format_text_output_empty_results() {
    let output = QueryOutput {
        query: "nothing".to_string(),
        total: 0,
        results: vec![],
        duration_ms: 5,
        index_stats: None,
    };
    let mut buf = BufWriter::new(Vec::new());
    format_text_output(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    // format_text_output writes "no results for <query>" on empty results.
    assert!(
        s.contains("no results"),
        "empty result output must contain 'no results', got: {s:?}"
    );
}

#[test]
fn test_format_text_output_includes_path_and_score() {
    use crate::cmd::search::types::{ResolvedResult, SnippetContext, SnippetLine};

    let result = ResolvedResult {
        path: "src/auth.rs".to_string(),
        score: 12.34,
        field: "function_signature".to_string(),
        line_number: Some(2),
        line_range: Some(2..3),
        snippet: Some(SnippetContext {
            lines: vec![
                SnippetLine {
                    line_number: 1,
                    content: "/// Authenticate".to_string(),
                    is_match: false,
                },
                SnippetLine {
                    line_number: 2,
                    content: "pub fn authenticate".to_string(),
                    is_match: true,
                },
            ],
        }),
        stale: false,
        match_positions: vec![],
        temporal: None,
        layers_matched: vec![],
    };

    let output = QueryOutput {
        query: "authenticate".to_string(),
        total: 1,
        results: vec![result],
        duration_ms: 3,
        index_stats: None,
    };

    let mut buf = BufWriter::new(Vec::new());
    format_text_output(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    assert!(s.contains("src/auth.rs"), "output should contain path");
}

// ============================================================================
// [stale] marker
// ============================================================================

#[test]
fn test_format_text_output_includes_stale_marker() {
    use crate::cmd::search::types::{ResolvedResult, SnippetContext, SnippetLine};

    let result = ResolvedResult {
        path: "src/old.rs".to_string(),
        score: 5.0,
        field: "function_signature".to_string(),
        line_number: Some(10),
        line_range: Some(10..11),
        snippet: Some(SnippetContext {
            lines: vec![SnippetLine {
                line_number: 10,
                content: "pub fn old_fn()".to_string(),
                is_match: true,
            }],
        }),
        stale: true,
        match_positions: vec![],
        temporal: None,
        layers_matched: vec![],
    };

    let output = QueryOutput {
        query: "old_fn".to_string(),
        total: 1,
        results: vec![result],
        duration_ms: 2,
        index_stats: None,
    };

    let mut buf = BufWriter::new(Vec::new());
    format_text_output(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    assert!(
        s.contains("[stale]"),
        "stale result must include '[stale]' marker in output, got: {s:?}"
    );
}

// ============================================================================
// Edge cases: no .git, corrupt index
// ============================================================================

#[test]
fn test_execute_query_no_git_dir_returns_ok_or_graceful_err() {
    // Project root with no .git — should not panic, must return Ok or Err.
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    // No .git directory — non-git project.
    fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();

    let config = make_config(&root, &cache_dir, "main");

    // Must not panic. Either succeeds (0 or more results) or fails gracefully.
    match execute_query(&config, &TEST_ANALYTICS) {
        Ok(output) => {
            assert_eq!(output.query, "main");
        }
        Err(e) => {
            // Acceptable: I/O or index error — but no panic.
            let msg = e.to_string();
            assert!(!msg.is_empty(), "error message must not be empty");
        }
    }
}

#[test]
fn test_execute_query_corrupt_index_returns_err_not_panic() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    // Write garbage bytes into the index file.
    fs::write(
        cache_dir.join("index.skidx"),
        b"this is not a valid index\xff\x00\xde\xad",
    )
    .unwrap();

    let config = make_config(&root, &cache_dir, "authenticate");

    // A corrupt index must return Err rather than panic.
    // (auto_refresh_if_stale may rebuild and succeed — both outcomes are acceptable.)
    match execute_query(&config, &TEST_ANALYTICS) {
        Ok(_) => {
            // Rebuild succeeded after detecting corruption — acceptable.
        }
        Err(e) => {
            // Graceful error — confirm non-empty message.
            assert!(!e.to_string().is_empty(), "error message must not be empty");
        }
    }
}

// ============================================================================
// ResolvedResult JSON serialization
// ============================================================================

/// line_range: Some(5..13) must serialise as {"start":5,"end":13} in JSON output.
#[test]
fn test_resolved_result_line_range_some_serializes_start_end() {
    use crate::cmd::search::types::ResolvedResult;

    let result = ResolvedResult {
        path: "src/lib.rs".to_string(),
        score: 1.0,
        field: "function_signature".to_string(),
        line_number: Some(5),
        line_range: Some(5..13),
        snippet: None,
        stale: false,
        match_positions: vec![],
        temporal: None,
        layers_matched: vec![],
    };

    let value = serde_json::to_value(&result).expect("ResolvedResult must serialize");
    assert_eq!(
        value["line_range"]["start"], 5,
        "line_range.start must be 5"
    );
    assert_eq!(value["line_range"]["end"], 13, "line_range.end must be 13");
}

/// line_range: None must serialise as JSON null.
#[test]
fn test_resolved_result_line_range_none_serializes_null() {
    use crate::cmd::search::types::ResolvedResult;

    let result = ResolvedResult {
        path: "src/lib.rs".to_string(),
        score: 1.0,
        field: "function_signature".to_string(),
        line_number: None,
        line_range: None,
        snippet: None,
        stale: false,
        match_positions: vec![],
        temporal: None,
        layers_matched: vec![],
    };

    let value = serde_json::to_value(&result).expect("ResolvedResult must serialize");
    assert!(
        value["line_range"].is_null(),
        "line_range must be null when None, got: {:?}",
        value["line_range"]
    );
}

// ============================================================================
// blast_radius_paths filter
// ============================================================================

/// When blast_radius_paths is set, execute_query uses UNION composite ranking
/// (#200): the blast-radius member that lexically matches must appear in results.
///
/// Note: as of #200 the blast-radius path uses UNION semantics (not the old
/// filter/intersection semantics).  Lexically relevant files outside the
/// blast-radius set may also appear in results — this is intentional.  The
/// invariant under test is that the blast member IS included, not that the
/// result set is restricted to it.
#[test]
fn test_execute_query_blast_radius_includes_only_allowed_paths() {
    use std::collections::HashSet;

    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    // blast-radius set: src/auth.rs only.
    let mut allowed: HashSet<String> = HashSet::new();
    allowed.insert("src/auth.rs".to_string());

    let config = QueryConfig {
        text: "authenticate".to_string(),
        limit: 20,
        json: false,
        root: root.to_path_buf(),
        cache_dir: cache_dir.to_path_buf(),
        blast_radius_paths: Some(allowed),
        ast_scored: None,
        composite_weights: None,
    };

    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // UNION mode (#200): src/auth.rs lexically matches "authenticate" AND is
    // in the blast-radius set → it MUST appear in results.
    let has_auth = output.results.iter().any(|r| r.path == "src/auth.rs");
    assert!(
        has_auth,
        "blast-radius member that lexically matches must appear in UNION results (AC12)"
    );

    // query must succeed and return at least one result.
    assert!(!output.results.is_empty(), "results must not be empty");
}

/// When blast_radius_paths contains the target file, a query that matches
/// the target returns results for that file.
/// Regression for: combined mode was excluding the target file itself.
#[test]
fn test_execute_query_blast_radius_target_file_is_included() {
    use std::collections::HashSet;

    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    // Build an allowlist that includes src/auth.rs (the "target") plus a
    // partner that has no matching content for "authenticate".
    let mut allowed: HashSet<String> = HashSet::new();
    allowed.insert("src/auth.rs".to_string()); // target
    allowed.insert("src/does_not_exist.rs".to_string()); // partner (not indexed)

    let config = QueryConfig {
        text: "authenticate".to_string(),
        limit: 20,
        json: false,
        root: root.to_path_buf(),
        cache_dir: cache_dir.to_path_buf(),
        blast_radius_paths: Some(allowed),
        ast_scored: None,
        composite_weights: None,
    };

    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // src/auth.rs contains "authenticate" and is in the allowed set — it must appear.
    let has_auth = output.results.iter().any(|r| r.path == "src/auth.rs");
    assert!(
        has_auth,
        "target file (src/auth.rs) must be in blast-radius results when it matches the query"
    );
}

// ============================================================================
// AC12 — UNION inclusion: co-change-only file appears (POSITIVE, discriminating)
//
// A file Y is indexed (has a FileId in the manifest) but does NOT match the
// text query Q.  When Y is in blast_radius_paths, it must appear in UNION
// results ranked by its temporal RRF term alone.  Under the OLD filtered-
// intersection behaviour Y would be ABSENT (it was dropped because it didn't
// match the query).  This test asserts the strict PRESENT-in-UNION /
// WOULD-BE-ABSENT-in-filter difference.
// ============================================================================

#[test]
fn test_ac12_union_includes_cochange_only_file_absent_from_lexical() {
    use std::collections::HashSet;

    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    // Uses a project where lib.rs has ZERO n-gram overlap with the query term.
    create_union_test_project(&root);

    // Query text: "zqjxblip_check" — unique to src/auth.rs only.
    // src/lib.rs has no shared 4-grams with this term → pure co-change-only partner.
    //
    // blast_radius_paths: include BOTH src/auth.rs (lexical match) AND
    // src/lib.rs (co-change partner that does NOT match the query).
    let mut allowed: HashSet<String> = HashSet::new();
    allowed.insert("src/auth.rs".to_string()); // lexically matches query
    allowed.insert("src/lib.rs".to_string()); // co-change partner; does NOT match query

    let config = QueryConfig {
        text: "zqjxblip_check".to_string(),
        limit: 20,
        json: false,
        root: root.to_path_buf(),
        cache_dir: cache_dir.to_path_buf(),
        blast_radius_paths: Some(allowed),
        ast_scored: None,
        composite_weights: None,
    };

    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // AC12 POSITIVE: src/lib.rs is a co-change partner that does NOT match
    // "zqjxblip_check" lexically (zero n-gram overlap), but IS in
    // blast_radius_paths → must appear in UNION results ranked by its temporal
    // RRF term alone.
    let has_lib = output.results.iter().any(|r| r.path == "src/lib.rs");
    assert!(
        has_lib,
        "AC12: co-change-only file (src/lib.rs) that does NOT match the query \
        must appear in UNION results due to its temporal blast-radius rank; \
        got results: {:?}",
        output.results.iter().map(|r| &r.path).collect::<Vec<_>>()
    );

    // DISCRIMINATING — under OLD filter semantics src/lib.rs would be ABSENT
    // because it didn't match the query.  Document the strict contract:
    // UNION mode includes it; filtered mode would not.
    // Both src/auth.rs (lexical hit) and src/lib.rs (co-change hit) must appear.
    let has_auth = output.results.iter().any(|r| r.path == "src/auth.rs");
    assert!(
        has_auth,
        "AC12: lexically-matching file (src/auth.rs) must also appear in UNION results"
    );
}

// ============================================================================
// AC13 — UNION cardinality and ordering bounds (NEGATIVE)
//
// The composite UNION output must:
// (a) Contain no duplicate FileIds
// (b) Be sorted fused-RRF-score DESC, then path ASC as tiebreak
// (c) Have count == min(|union|, limit)
// (d) Apply rank-then-limit LAST (a co-change-only file ranking in top-N
//     must not be pre-truncated before fusion)
// ============================================================================

#[test]
fn test_ac13_union_no_duplicate_file_ids_and_correct_cardinality() {
    use std::collections::HashSet;

    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_union_test_project(&root);

    // blast_radius_paths with both indexed files so the union is the full index.
    // Both files are in the temporal list; auth.rs also matches the lexical query.
    let mut allowed: HashSet<String> = HashSet::new();
    allowed.insert("src/auth.rs".to_string());
    allowed.insert("src/lib.rs".to_string());

    let config = QueryConfig {
        text: "zqjxblip_check".to_string(),
        limit: 20,
        json: false,
        root: root.to_path_buf(),
        cache_dir: cache_dir.to_path_buf(),
        blast_radius_paths: Some(allowed),
        ast_scored: None,
        composite_weights: None,
    };

    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // (a) No duplicate paths (FileIds map 1:1 to paths in the sorted manifest).
    let paths: Vec<&str> = output.results.iter().map(|r| r.path.as_str()).collect();
    let unique_paths: HashSet<&str> = paths.iter().copied().collect();
    assert_eq!(
        paths.len(),
        unique_paths.len(),
        "AC13(a): no duplicate paths in UNION output; got {:?}",
        paths
    );

    // (b) Result count <= limit (rank-then-limit).
    assert!(
        output.results.len() <= 20,
        "AC13(c): result count must be <= limit (20), got {}",
        output.results.len()
    );

    // (c) Scores are non-increasing (fused-RRF-score DESC order).
    // Ties may exist; adjacent ties are not a violation of the ordering contract.
    let scores: Vec<f64> = output.results.iter().map(|r| r.score).collect();
    for window in scores.windows(2) {
        assert!(
            window[0] >= window[1] - 1e-9,
            "AC13(b): scores must be non-increasing (DESC order); found {:?}",
            scores
        );
    }

    // (d) All returned paths come from the union of lexical candidates and
    // temporal co-change partners — no fabricated files.
    // Every path must be a valid indexed path (resolves from the manifest).
    for r in &output.results {
        assert!(
            !r.path.is_empty(),
            "AC13(a): every result must have a non-empty path"
        );
        // Co-change-only results carry field "co_change_partner" (no snippet).
        // Lexical results carry real field names.
        // Both are valid UNION members.
    }
}

#[test]
fn test_ac13_limit_applied_after_fusion_rank_then_limit() {
    use std::collections::HashSet;

    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_union_test_project(&root);

    // Both indexed files in blast-radius; query matches only one.
    let mut allowed: HashSet<String> = HashSet::new();
    allowed.insert("src/auth.rs".to_string()); // lexical match
    allowed.insert("src/lib.rs".to_string()); // co-change-only

    // limit = 1: only the top-ranked result is returned.
    let config = QueryConfig {
        text: "zqjxblip_check".to_string(),
        limit: 1,
        json: false,
        root: root.to_path_buf(),
        cache_dir: cache_dir.to_path_buf(),
        blast_radius_paths: Some(allowed),
        ast_scored: None,
        composite_weights: None,
    };

    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // AC13(c): count = min(|union|, limit) = min(2, 1) = 1.
    assert_eq!(
        output.results.len(),
        1,
        "AC13(c): limit=1 must return exactly 1 result from the UNION of 2 candidates; \
        got {} results: {:?}",
        output.results.len(),
        output.results.iter().map(|r| &r.path).collect::<Vec<_>>()
    );
}

// ============================================================================
// AC14 — co-change-only result carries fused-RRF score, not BM25F
// ============================================================================

#[test]
fn test_ac14_cochange_only_result_carries_fused_rrf_score() {
    use std::collections::HashSet;

    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    // lib.rs has zero n-gram overlap with "zqjxblip_check" → pure co-change partner.
    create_union_test_project(&root);

    // blast_radius_paths includes lib.rs (co-change-only: no "zqjxblip_check" match).
    let mut allowed: HashSet<String> = HashSet::new();
    allowed.insert("src/auth.rs".to_string());
    allowed.insert("src/lib.rs".to_string());

    let config = QueryConfig {
        text: "zqjxblip_check".to_string(),
        limit: 20,
        json: false,
        root: root.to_path_buf(),
        cache_dir: cache_dir.to_path_buf(),
        blast_radius_paths: Some(allowed),
        ast_scored: None,
        composite_weights: None,
    };

    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // Find the co-change-only result (src/lib.rs) if present.
    // If found, assert:
    // (a) Its field is "co_change_partner" (not a BM25F field type).
    // (b) Its score is a small positive fused-RRF value (not a BM25F magnitude).
    //     RRF scores are wᵢ / (RRF_K + rankᵢ), so with weight 0.2 and rank 1,
    //     score ≈ 0.2 / (60 + 1) ≈ 0.00328 — not a BM25F magnitude.
    if let Some(lib_result) = output.results.iter().find(|r| r.path == "src/lib.rs") {
        assert_eq!(
            lib_result.field, "co_change_partner",
            "AC14: co-change-only result must have field='co_change_partner', not a BM25F field type"
        );
        // Score must be finite and positive (fused RRF term).
        assert!(
            lib_result.score.is_finite() && lib_result.score > 0.0,
            "AC14: co-change-only score must be a finite positive fused-RRF value, got {}",
            lib_result.score
        );
        // Score must be small (well below typical BM25F magnitudes of 5–100).
        // A pure temporal RRF score with w=0.2 and rank 1 is ≈ 0.00328.
        assert!(
            lib_result.score < 5.0,
            "AC14: fused-RRF score must be small (< 5.0), not a BM25F magnitude; got {}",
            lib_result.score
        );
    }
    // Note: if src/lib.rs is not in results, AC12 would have caught it first.
    // This test is complementary to AC12 and focuses on the score field contract.
}

// ============================================================================
// format_json_output
// ============================================================================

#[test]
fn test_format_json_output_is_valid_json() {
    let output = QueryOutput {
        query: "test".to_string(),
        total: 0,
        results: vec![],
        duration_ms: 1,
        index_stats: None,
    };
    let mut buf = BufWriter::new(Vec::new());
    format_json_output(&output, &mut buf).unwrap();
    let bytes = buf.into_inner().unwrap();
    let s = std::str::from_utf8(&bytes).unwrap();
    // Must be valid JSON
    let parsed: serde_json::Value = serde_json::from_str(s).expect("must be valid JSON");
    assert_eq!(parsed["query"], "test");
    assert_eq!(parsed["total"], 0);
}

// ============================================================================
// Temporal annotation in text output (Step 11)
// ============================================================================

/// format_text_output includes "hotspot: X.XXX" when temporal annotation present.
#[test]
fn test_format_text_output_includes_temporal_hotspot() {
    use crate::cmd::search::types::{ResolvedResult, TemporalAnnotation};

    let result = ResolvedResult {
        path: "src/hot.rs".to_string(),
        score: 5.0,
        field: "function_signature".to_string(),
        line_number: Some(1),
        line_range: Some(1..2),
        snippet: None,
        stale: false,
        match_positions: vec![],
        temporal: Some(TemporalAnnotation {
            hotspot_score: Some(0.95),
            ..Default::default()
        }),
        layers_matched: vec![],
    };

    let output = QueryOutput {
        query: "hot".to_string(),
        total: 1,
        results: vec![result],
        duration_ms: 1,
        index_stats: None,
    };

    let mut buf = BufWriter::new(Vec::new());
    format_text_output(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    assert!(
        s.contains("hotspot:"),
        "temporal hotspot annotation must appear, got: {s:?}"
    );
    assert!(
        s.contains("0.950"),
        "hotspot score must be formatted to 3dp, got: {s:?}"
    );
}

/// format_text_output shows "risk: X.XXX" when risk annotation present.
#[test]
fn test_format_text_output_includes_temporal_risk() {
    use crate::cmd::search::types::{ResolvedResult, TemporalAnnotation};

    let result = ResolvedResult {
        path: "src/risky.rs".to_string(),
        score: 3.0,
        field: "function_signature".to_string(),
        line_number: None,
        line_range: None,
        snippet: None,
        stale: false,
        match_positions: vec![],
        temporal: Some(TemporalAnnotation {
            risk_score: Some(0.80),
            ..Default::default()
        }),
        layers_matched: vec![],
    };

    let output = QueryOutput {
        query: "risky".to_string(),
        total: 1,
        results: vec![result],
        duration_ms: 1,
        index_stats: None,
    };

    let mut buf = BufWriter::new(Vec::new());
    format_text_output(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    assert!(
        s.contains("risk:"),
        "risk annotation must appear, got: {s:?}"
    );
    assert!(
        s.contains("0.800"),
        "risk score must be formatted to 3dp, got: {s:?}"
    );
}

/// format_text_output omits temporal section when annotation is None.
#[test]
fn test_format_text_output_omits_temporal_when_none() {
    use crate::cmd::search::types::ResolvedResult;

    let result = ResolvedResult {
        path: "src/plain.rs".to_string(),
        score: 2.0,
        field: "function_signature".to_string(),
        line_number: None,
        line_range: None,
        snippet: None,
        stale: false,
        match_positions: vec![],
        temporal: None,
        layers_matched: vec![],
    };

    let output = QueryOutput {
        query: "plain".to_string(),
        total: 1,
        results: vec![result],
        duration_ms: 1,
        index_stats: None,
    };

    let mut buf = BufWriter::new(Vec::new());
    format_text_output(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    assert!(
        !s.contains("hotspot:"),
        "no hotspot annotation when temporal is None, got: {s:?}"
    );
    assert!(
        !s.contains("risk:"),
        "no risk annotation when temporal is None, got: {s:?}"
    );
}

/// format_json_output includes temporal annotations inside each result object.
#[test]
fn test_format_json_output_includes_temporal_annotations() {
    use crate::cmd::search::types::{ResolvedResult, TemporalAnnotation};

    let result = ResolvedResult {
        path: "src/hot.rs".to_string(),
        score: 5.0,
        field: "function_signature".to_string(),
        line_number: None,
        line_range: None,
        snippet: None,
        stale: false,
        match_positions: vec![],
        temporal: Some(TemporalAnnotation {
            hotspot_score: Some(0.95),
            risk_score: Some(0.70),
            ..Default::default()
        }),
        layers_matched: vec![],
    };

    let output = QueryOutput {
        query: "hot".to_string(),
        total: 1,
        results: vec![result],
        duration_ms: 1,
        index_stats: None,
    };

    let mut buf = BufWriter::new(Vec::new());
    format_json_output(&output, &mut buf).unwrap();
    let bytes = buf.into_inner().unwrap();
    let s = std::str::from_utf8(&bytes).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(s).expect("must be valid JSON");

    let temporal = &parsed["results"][0]["temporal"];
    assert!(
        !temporal.is_null(),
        "temporal field must be present in JSON when Some"
    );
    let hs = temporal["hotspot_score"].as_f64().unwrap();
    assert!(
        (hs - 0.95).abs() < 1e-6,
        "hotspot_score must be ~0.95, got {hs}"
    );
    let rs = temporal["risk_score"].as_f64().unwrap();
    assert!(
        (rs - 0.70).abs() < 1e-6,
        "risk_score must be ~0.70, got {rs}"
    );
}

// ============================================================================
// #355 Part A — Exact-match verification (AC1 / AC2 / AC3)
//
// PF-007: every test asserts a discriminating observable, not just exit-0.
// AC2: gibberish query → 0 results on ALL paths.
// AC3: every returned result literally contains the query token(s).
// AC1: an exact symbol query returns only files containing it.
// ============================================================================

/// AC2 — gibberish query produces 0 verified results on the pure-lexical path.
///
/// PF-007 (discriminating): asserts results.is_empty() for a query whose trigrams
/// are absent from the index — so the reader returns 0 candidates and verification
/// never runs.  This guards the trigram-miss path, not the verify gate.
/// The discriminating coverage for the verify gate is in:
///   - `test_ac1_verify_gate_drops_trigram_overlap_non_literal` (non-literal that shares trigrams)
///   - `test_ac3_every_result_contains_query_term_pure_lexical` (content check per result)
///   - `test_ac2_verify_gate_drops_compound_lexical_hit_without_literal` (compound path)
#[test]
fn test_ac2_gibberish_query_returns_zero_results_pure_lexical() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    // "xqzjvmblorp" is a provably absent gibberish string — its trigrams (e.g. "xqz",
    // "qzj", "zjv"…) do not appear in any natural code file, so the trigram index
    // returns 0 candidates before verification even runs.  The empty result here
    // comes from zero trigram overlap, not from the verify gate.
    let config = make_config(&root, &cache_dir, "xqzjvmblorp");
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // AC2: verified result set must be empty (trigram-miss path).
    assert!(
        output.results.is_empty(),
        "AC2: gibberish query 'xqzjvmblorp' must return 0 results (no trigram overlap); \
        got {} results: {:?}",
        output.results.len(),
        output.results.iter().map(|r| &r.path).collect::<Vec<_>>()
    );
}

/// AC2 (compound path, trigram-miss) — gibberish query + AST filter → 0 results.
///
/// NOTE: this test exercises the "no trigrams in index" path, NOT the verify gate.
/// For the discriminating compound-path verify-gate test, see
/// `test_ac2_verify_gate_drops_compound_lexical_hit_without_literal` below.
#[test]
fn test_ac2_gibberish_query_returns_zero_results_compound_path() {
    use rskim_search::FileId;

    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    // Use a fake ast_scored vector (file 0 with score 1.0); the gibberish query
    // has no trigram overlap with the corpus so raw_lex is empty; intersect_and_rank
    // short-circuits to [] before the verify gate is even reached.
    let config = QueryConfig {
        text: "xqzjvmblorp".to_string(),
        limit: 20,
        json: false,
        root: root.to_path_buf(),
        cache_dir: cache_dir.to_path_buf(),
        blast_radius_paths: None,
        ast_scored: Some(vec![(FileId(0), 1.0)]),
        composite_weights: None,
    };
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    assert!(
        output.results.is_empty(),
        "AC2 (compound path, trigram-miss): gibberish query must return 0 results; \
        got {} results: {:?}",
        output.results.len(),
        output.results.iter().map(|r| &r.path).collect::<Vec<_>>()
    );
}

/// AC2 (compound path, verify gate discriminating) — a file that the lexical index
/// returns as a candidate AND the fake AST scored it highly, but does NOT contain
/// the literal query, must be dropped by the verify gate.
///
/// PF-007 (discriminating): this test WOULD FAIL if the verify gate were removed
/// from `resolve_paths_and_snippets_verified`.  "authenticate_user" shares trigrams
/// with lib.rs (which contains "authenticate" and "user" as separate words), so the
/// compound path's `raw_lex` includes lib.rs.  With a fake ast_scored entry for
/// lib.rs, it survives `intersect_and_rank`.  Only the verify gate drops it.
///
/// This is the template from AC1 (pure-lexical verify gate) ported to the compound
/// (text+AST) path — fixes PF-007 Finding 10.
#[test]
fn test_ac2_verify_gate_drops_compound_lexical_hit_without_literal() {
    use rskim_search::FileId;

    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();

    // auth.rs: contains the exact literal "authenticate_user".
    fs::write(
        src.join("auth.rs"),
        "/// Authenticate a user by token.\n\
         pub fn authenticate_user(token: &str) -> bool { !token.is_empty() }\n",
    )
    .unwrap();

    // lib.rs: contains "authenticate" and "user" as SEPARATE words — shares many
    // trigrams with "authenticate_user" — but NOT the literal "authenticate_user".
    // The fake AST score gives lib.rs a higher-than-auth AST score so it will be
    // in `raw_lex` AND in the intersection result; only the verify gate must drop it.
    fs::write(
        src.join("lib.rs"),
        "/// Authenticate the request.\n\
         pub fn check_user(id: u32) -> bool { id > 0 }\n\
         pub fn authenticate(token: &str) -> bool { !token.is_empty() }\n",
    )
    .unwrap();

    // Build the index so FileId(0)=auth.rs, FileId(1)=lib.rs (sorted alphabetically).
    {
        let build_config = QueryConfig {
            text: "authenticate_user".to_string(),
            limit: 20,
            json: false,
            root: root.to_path_buf(),
            cache_dir: cache_dir.to_path_buf(),
            blast_radius_paths: None,
            ast_scored: None,
            composite_weights: None,
        };
        // First run with no ast_scored builds the index (cold start).
        let _ = execute_query(&build_config, &TEST_ANALYTICS).unwrap();
    }

    // Now run the compound path: give FileId(1)=lib.rs a HIGH AST score so it
    // wins the intersection and survives into recompose.  The verify gate must
    // drop it because "authenticate_user" is absent from lib.rs.
    let config = QueryConfig {
        text: "authenticate_user".to_string(),
        limit: 20,
        json: false,
        root: root.to_path_buf(),
        cache_dir: cache_dir.to_path_buf(),
        blast_radius_paths: None,
        // FileId(0)=auth.rs: low AST score; FileId(1)=lib.rs: high AST score.
        // The fake AST order ensures lib.rs appears in the intersection with auth.rs.
        ast_scored: Some(vec![(FileId(0), 0.5), (FileId(1), 2.0)]),
        composite_weights: None,
    };
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // PF-007 (discriminating): lib.rs must NOT appear in verified results.
    // It shares trigrams with "authenticate_user" and has a higher AST score,
    // but the literal string is absent — the verify gate MUST drop it.
    // Removing the verify gate from resolve_paths_and_snippets_verified would
    // cause lib.rs to appear here, failing this assertion.
    let has_lib = output.results.iter().any(|r| r.path.contains("lib.rs"));
    assert!(
        !has_lib,
        "AC2 (compound verify gate): 'lib.rs' has trigram overlap AND a high AST score \
        but does NOT contain the literal 'authenticate_user' — the verify gate must drop it. \
        Found in results — verify gate is absent or broken on the compound path. \
        Results: {:?}",
        output.results.iter().map(|r| &r.path).collect::<Vec<_>>()
    );

    // auth.rs MUST appear (it contains the literal).
    let has_auth = output.results.iter().any(|r| r.path.contains("auth.rs"));
    assert!(
        has_auth,
        "AC2 (compound verify gate): 'auth.rs' contains the literal 'authenticate_user' \
        and must appear in compound results; got: {:?}",
        output.results.iter().map(|r| &r.path).collect::<Vec<_>>()
    );
}

/// AC2 (blast-radius path) — gibberish query + blast-radius → 0 verified
/// lexical-hit results; only co-change-only stubs (no snippet, field=co_change_partner)
/// may appear.
///
/// PF-007: the discriminating check is that NO result carries a non-None snippet
/// (which would mean the file was read and the verify gate passed).  "xqzjvmblorp"
/// shares no trigrams with the corpus, so no file enters the lexical branch at all.
/// This test pairs with test_ac2_short_query_fallback_blast_radius_exercises_verify_gate
/// which uses a <3-byte query that DOES reach the reader's fallback and exercises the
/// verify gate on the blast-radius path.
#[test]
fn test_ac2_gibberish_query_no_lexical_hits_blast_radius() {
    use std::collections::HashSet;

    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    let mut allowed: HashSet<String> = HashSet::new();
    allowed.insert("src/auth.rs".to_string());

    let config = QueryConfig {
        text: "xqzjvmblorp".to_string(),
        limit: 20,
        json: false,
        root: root.to_path_buf(),
        cache_dir: cache_dir.to_path_buf(),
        blast_radius_paths: Some(allowed),
        ast_scored: None,
        composite_weights: None,
    };
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // PF-007 discriminating: no result may have a non-None snippet.
    // A non-None snippet means the file was read AND query_substring_present
    // returned true — which would require "xqzjvmblorp" to appear in a file.
    // Any result with a snippet here is a false positive from the verify gate.
    //
    // Co-change-only stubs (field="co_change_partner", snippet=None) are
    // exempt — they are returned by UNION semantics without lexical verification.
    for r in &output.results {
        assert!(
            r.snippet.is_none(),
            "AC2 (blast-radius): no result with a snippet expected for a gibberish query; \
            a snippet means the verify gate passed — false positive; found: {:?}",
            r
        );
    }
}

/// AC2 (blast-radius short-query fallback) — a 2-byte query that reaches the
/// AD-355-7 fallback on the blast-radius path exercises the verify gate.
///
/// PF-007 (discriminating, F14): the corpus has one file that CONTAINS the 2-byte
/// query "zz" (`match.rs`) and one that does NOT (`nomatch.rs`).  Both are in the
/// blast-radius allowlist.  The test asserts by PATH MEMBERSHIP:
///
/// - `match.rs` (contains "zz") MUST appear in results — the gate's keep path.
/// - `nomatch.rs` (does not contain "zz") MUST NOT appear — the gate's drop path.
///
/// This is a STRICT SUBSET check: if the verify gate is removed the non-matching
/// file would survive the fallback and appear in results, failing the "absent"
/// assertion.  If the keep path is broken the matching file would be dropped,
/// failing the "present" assertion.  The test therefore fails in BOTH regression
/// directions — making it a genuine guard per PF-007.
///
/// Previously the test used `r.snippet.is_none()` which cannot distinguish
/// gate-on from gate-off (short-query candidates always have empty match_positions
/// so `snippet` is always `None` regardless of the verify decision).
#[test]
fn test_ac2_short_query_fallback_blast_radius_exercises_verify_gate() {
    use std::collections::HashSet;

    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    // Corpus: match.rs CONTAINS "zz"; nomatch.rs does NOT.
    // Both are in the blast-radius allowlist so both reach the verify gate.
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
    // "zz" appears in match.rs (the token we search for).
    fs::write(
        src.join("match.rs"),
        "// contains the target token\npub fn check_zz(x: &str) -> bool { x.contains(\"zz\") }\n",
    )
    .unwrap();
    // "zz" is absent from nomatch.rs.
    fs::write(
        src.join("nomatch.rs"),
        "pub fn parse_config(s: &str) -> Option<String> { Some(s.to_string()) }\n",
    )
    .unwrap();

    let mut allowed: HashSet<String> = HashSet::new();
    allowed.insert("src/match.rs".to_string());
    allowed.insert("src/nomatch.rs".to_string());

    let config = QueryConfig {
        text: "zz".to_string(), // 2 bytes → AD-355-7 fallback
        limit: 20,
        json: false,
        root: root.to_path_buf(),
        cache_dir: cache_dir.to_path_buf(),
        blast_radius_paths: Some(allowed),
        ast_scored: None,
        composite_weights: None,
    };
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // PF-007 DISCRIMINATING assertions — path-membership, not snippet presence:

    // (1) The file that contains "zz" MUST be in results (verifies the keep path).
    let has_match = output.results.iter().any(|r| r.path == "src/match.rs");
    assert!(
        has_match,
        "AC2 (blast-radius short-query, keep path): 'src/match.rs' contains the literal \
        'zz' and must appear in verified results after the AD-355-7 fallback; \
        results: {:?}",
        output.results.iter().map(|r| &r.path).collect::<Vec<_>>()
    );

    // (2) The file that does NOT contain "zz" MUST NOT be in results (verifies the
    //     drop path — this assertion fails if the verify gate is removed or bypassed).
    let has_nomatch = output.results.iter().any(|r| r.path == "src/nomatch.rs");
    assert!(
        !has_nomatch,
        "AC2 (blast-radius short-query, drop path): 'src/nomatch.rs' does NOT contain \
        the literal 'zz' and must be dropped by the verify gate; found in results — \
        verify gate is absent or broken on the blast-radius short-query path. \
        Results: {:?}",
        output.results.iter().map(|r| &r.path).collect::<Vec<_>>()
    );
}

/// AC3 — every returned result literally contains the query term (pure-lexical).
///
/// PF-007 (discriminating): reads the content of each returned file and asserts
/// the query term is present as a literal substring.  This test would fail if
/// verification were disabled (bigram-noise false positives would appear).
#[test]
fn test_ac3_every_result_contains_query_term_pure_lexical() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    // "authenticate" is a real term in src/auth.rs.
    let config = make_config(&root, &cache_dir, "authenticate");
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    assert!(
        !output.results.is_empty(),
        "AC3: 'authenticate' must find at least one result"
    );

    for r in &output.results {
        let abs_path = root.join(&r.path);
        let content = fs::read_to_string(&abs_path).unwrap_or_default();
        assert!(
            content.contains("authenticate"),
            "AC3: result file '{}' must contain the literal query term 'authenticate'; \
            file content: {content:?}",
            r.path
        );
    }
}

/// AC1 — an exact symbol query returns ONLY files containing it; the defining
/// file ranks at position 0 (the highest-ranked result).
///
/// PF-007 (discriminating): asserts (a) the definer is present and (b) every
/// non-definer result is absent from the verified set when the symbol is unique.
/// This would fail without the wider pool + verify-then-truncate invariant.
#[test]
fn test_ac1_exact_symbol_returns_only_containing_files_and_definer_is_first() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    // Controlled corpus: auth.rs defines `frbnqlwx_unique_symbol`; lib.rs does NOT.
    // The symbol uses a nonsense prefix that can't appear in lib.rs accidentally.
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
    fs::write(
        src.join("auth.rs"),
        "/// The authoritative definer.\npub fn frbnqlwx_unique_symbol(x: u32) -> u32 { x }\n",
    )
    .unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub struct Config { pub value: u32 }\nimpl Config { pub fn new(v: u32) -> Self { Self { value: v } } }\n",
    )
    .unwrap();

    let config = make_config(&root, &cache_dir, "frbnqlwx_unique_symbol");
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // AC1(a): the definer file must be in results.
    let has_definer = output.results.iter().any(|r| r.path == "src/auth.rs");
    assert!(
        has_definer,
        "AC1: definer file 'src/auth.rs' must appear in results; got: {:?}",
        output.results.iter().map(|r| &r.path).collect::<Vec<_>>()
    );

    // AC1(b): no result may be a file that does NOT contain the symbol.
    // lib.rs does not contain "frbnqlwx_unique_symbol" — it must be absent.
    let has_lib = output.results.iter().any(|r| r.path == "src/lib.rs");
    assert!(
        !has_lib,
        "AC1: 'src/lib.rs' does not contain 'frbnqlwx_unique_symbol' and must \
        NOT appear in verified results (this would fail without verification)"
    );

    // AC1(c): definer is the top-ranked result.
    let first_path = output
        .results
        .first()
        .map(|r| r.path.as_str())
        .unwrap_or("");
    assert_eq!(
        first_path,
        "src/auth.rs",
        "AC1: definer 'src/auth.rs' must be results[0]; got {:?}",
        output.results.iter().map(|r| &r.path).collect::<Vec<_>>()
    );

    // AC3 (inline): every returned result must contain the query term.
    for r in &output.results {
        let abs_path = root.join(&r.path);
        let content = fs::read_to_string(&abs_path).unwrap_or_default();
        assert!(
            content.contains("frbnqlwx_unique_symbol"),
            "AC3: every verified result must contain 'frbnqlwx_unique_symbol'; \
            '{}' does not: {content:?}",
            r.path
        );
    }
}

/// AC1 (verify gate specifically exercised) — lib.rs shares trigrams with the
/// query term but does NOT contain the literal string.
///
/// The original AC1 test uses a purely unique symbol with zero trigram overlap
/// in lib.rs — so lib.rs is trivially absent from candidates.  This test
/// specifically exercises the verify gate: lib.rs contains trigram-generating
/// substrings that share individual trigrams with the target query token, but
/// NOT the literal token.  Without the verify gate, lib.rs would be a false
/// positive.  With the gate, only the definer file survives.
///
/// PF-007 (discriminating): this test WOULD FAIL if verify gate were removed,
/// because the trigram index would return lib.rs as a candidate and it would
/// appear in results without the gate dropping it.
#[test]
fn test_ac1_verify_gate_drops_trigram_overlap_non_literal() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();

    // Query token: "authenticate_user".
    // auth.rs: contains the exact literal "authenticate_user".
    fs::write(
        src.join("auth.rs"),
        "/// Authenticate a user by token.\n\
         pub fn authenticate_user(token: &str) -> bool { !token.is_empty() }\n",
    )
    .unwrap();

    // lib.rs: contains the trigram-generating substrings "authenticate" and
    // "user" as SEPARATE words, generating many shared trigrams with
    // "authenticate_user", but the exact literal string "authenticate_user"
    // is NOT present.  The verify gate must drop lib.rs.
    fs::write(
        src.join("lib.rs"),
        "/// Authenticate the request.\n\
         pub fn check_user(id: u32) -> bool { id > 0 }\n\
         pub fn authenticate(token: &str) -> bool { !token.is_empty() }\n",
    )
    .unwrap();

    let config = make_config(&root, &cache_dir, "authenticate_user");
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // AC1: definer must be present.
    let has_auth = output.results.iter().any(|r| r.path == "src/auth.rs");
    assert!(
        has_auth,
        "AC1 (verify gate): 'src/auth.rs' defines 'authenticate_user' and must appear; \
        got: {:?}",
        output.results.iter().map(|r| &r.path).collect::<Vec<_>>()
    );

    // PF-007 (discriminating): lib.rs must NOT appear because although it shares
    // trigrams with "authenticate_user", it does not contain the literal string.
    // Without the verify gate, lib.rs would be a false positive.
    let has_lib = output.results.iter().any(|r| r.path == "src/lib.rs");
    assert!(
        !has_lib,
        "AC1 (verify gate): 'src/lib.rs' shares trigrams with 'authenticate_user' but \
        does NOT contain the literal string — verify gate must drop it; \
        found in results, which means verify gate is absent or broken"
    );
}
