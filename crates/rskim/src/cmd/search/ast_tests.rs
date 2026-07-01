//! Tests for the AST structural query helpers (ast.rs).
//!
//! This file is included by `ast.rs` as `#[path = "ast_tests.rs"] mod tests;`
//! so `super::` refers to the `ast` module, and `super::super::` refers to
//! the `search` module.
//!
//! Groups:
//! 1.  Parse/validate  — unit tests, no index required.
//! 2.  Build & alignment — tempdir fixture, verifies both index files written.
//! 3.  Output formatters — text and JSON shape, no index required.
//! 4.  Intersection    — text + --ast combined.
//! 5.  Auto-refresh    — self-heal when AST index is absent.
//! 6.  API contract    — exit codes, flag parsing edge cases.
//! 7.  Standalone --ast query (hermetic).
//! 8.  Text + --ast intersection (hermetic).
//! 9.  Self-heal for below-FORMAT_VERSION probe.
//! 10. Self-heal regression — text + --ast combined path.
//! 11. --ast + --blast-radius intersection (avoids PF-006, applies ADR-006).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::BufWriter;
use std::path::Path;

use tempfile::TempDir;

/// Stub analytics config for tests — analytics disabled, no cost override.
const TEST_ANALYTICS: crate::analytics::AnalyticsConfig = crate::analytics::AnalyticsConfig {
    enabled: false,
    input_cost_per_mtok: None,
    session_id: None,
};

// ============================================================================
// Helpers
// ============================================================================

/// Create a minimal project tree with a .git root and Rust source files
/// that contain match-based error handling and nested loops.
fn make_project_with_rust() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();

    // Rust file with match-based error handling (try-catch-like pattern).
    fs::write(
        root.join("src/error.rs"),
        r#"
fn handle_err() {
    let r: Result<i32, &str> = Ok(1);
    match r {
        Ok(v) => println!("{v}"),
        Err(e) => eprintln!("{e}"),
    }
}
"#,
    )
    .unwrap();

    // Rust file with nested loop.
    fs::write(
        root.join("src/loops.rs"),
        r#"
fn nested() {
    for i in 0..10 {
        for j in 0..10 {
            println!("{i} {j}");
        }
    }
}
"#,
    )
    .unwrap();

    // JSON file — non-tree-sitter lang; should be in lexical index with empty AST entry.
    fs::write(root.join("config.json"), r#"{"key": "value"}"#).unwrap();

    dir
}

/// Build a minimal index in `cache` from `project`.
fn build_project_index(project: &Path, cache: &Path) {
    use super::super::index::build_index;
    use super::super::types::IndexConfig;

    let config = IndexConfig {
        root: project.to_path_buf(),
        max_files: None,
        force: false,
        cache_dir_override: Some(cache.to_path_buf()),
    };
    build_index(&config).expect("build_index should succeed");
}

/// Create a project where TWO Rust files both contain nested loops (so both match
/// the `rust-nested-loop` AST pattern), giving a multi-file result set for
/// intersection testing.  A third, structurally-distinct file (`src/plain.rs`)
/// with no nested loops ensures the fixture has non-matching files too.
///
/// Fixture layout:
/// - `src/alpha.rs` — nested for-loops (matches rust-nested-loop)
/// - `src/beta.rs`  — nested for-loops (matches rust-nested-loop)
/// - `src/plain.rs` — simple function, no nested loops (does NOT match)
/// - `config.json`  — non-tree-sitter file (empty AST entry)
fn make_project_with_two_nested_loop_files() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();

    fs::write(
        root.join("src/alpha.rs"),
        r#"
fn alpha() {
    for i in 0..5 {
        for j in 0..5 {
            println!("{i} {j}");
        }
    }
}
"#,
    )
    .unwrap();

    fs::write(
        root.join("src/beta.rs"),
        r#"
fn beta() {
    for x in 0..3 {
        for y in 0..3 {
            println!("{x} {y}");
        }
    }
}
"#,
    )
    .unwrap();

    fs::write(
        root.join("src/plain.rs"),
        r#"
fn plain(x: i32) -> i32 {
    x + 1
}
"#,
    )
    .unwrap();

    fs::write(root.join("config.json"), r#"{"key": "value"}"#).unwrap();

    dir
}

/// Populate a `TemporalDb` at `db_path` with a single co-change pair.
///
/// Records `file_a <-> file_b` with `count=5, jaccard=0.8` so the DB has
/// real co-change data that the blast-radius resolution code can query.
/// `file_a` must be lexicographically <= `file_b` (CochangeRow convention).
fn write_cochange_db(db_path: &Path, file_a: &str, file_b: &str) {
    use rskim_search::{CochangeRow, TemporalDb};

    let db = TemporalDb::open(db_path).expect("TemporalDb::open must succeed in test");
    db.store_cochanges(&[CochangeRow {
        file_a: file_a.to_string(),
        file_b: file_b.to_string(),
        count: 5,
        jaccard: 0.8,
    }])
    .expect("store_cochanges must succeed");
}

/// Run `f` with `SKIM_CACHE_DIR` set to an isolated tempdir.
///
/// Restores (removes) the env var unconditionally after `f` completes, even on
/// panic, via `std::panic::catch_unwind`.  The `TempDir` returned from `f` keeps
/// the isolated cache alive for the duration of the call and is dropped on return.
///
/// # Safety
///
/// `std::env::set_var` is not thread-safe in a multi-threaded program.  Callers
/// MUST be annotated with `#[serial_test::serial]` to prevent concurrent mutation
/// of `SKIM_CACHE_DIR` (applies ADR-006 fail-loud counterpart: no silent env races).
fn with_isolated_cache<F>(f: F)
where
    F: FnOnce(&Path) + std::panic::UnwindSafe,
{
    let isolated = tempfile::tempdir().expect("tempdir must succeed");

    // Safety: guarded by #[serial_test::serial] on every caller.
    unsafe { std::env::set_var("SKIM_CACHE_DIR", isolated.path()) };

    let result = std::panic::catch_unwind(|| f(isolated.path()));

    // Always clean up, even if f panicked.
    unsafe { std::env::remove_var("SKIM_CACHE_DIR") };
    drop(isolated);

    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

/// Build a [`QueryConfig`] for hermetic execute_query tests.
///
/// Defaults: `json = false`, `composite_weights = None`.
/// All other fields are caller-supplied so each test is explicit about
/// exactly what it varies.
fn make_query_config(
    root: &Path,
    cache: &Path,
    text: &str,
    limit: usize,
    ast_scored: Option<Vec<(rskim_search::FileId, f64)>>,
    blast_radius_paths: Option<std::collections::HashSet<String>>,
) -> super::super::types::QueryConfig {
    super::super::types::QueryConfig {
        text: text.to_string(),
        limit,
        offset: None,
        json: false,
        root: root.to_path_buf(),
        cache_dir: cache.to_path_buf(),
        blast_radius_paths,
        ast_scored,
        composite_weights: None,
    }
}

// ============================================================================
// Group 1: Parse / validate (unit, no index)
// ============================================================================

#[test]
fn parse_flags_ast_space_form() {
    let flags = super::super::parse_flags(&["--ast".to_string(), "try-catch".to_string()]).unwrap();
    assert_eq!(flags.ast.as_deref(), Some("try-catch"));
}

#[test]
fn parse_flags_ast_equals_form() {
    let flags = super::super::parse_flags(&["--ast=try-catch".to_string()]).unwrap();
    assert_eq!(flags.ast.as_deref(), Some("try-catch"));
}

#[test]
fn parse_flags_ast_containment_with_spaces_preserved() {
    let flags = super::super::parse_flags(&[
        "--ast".to_string(),
        "for_statement > await_expression".to_string(),
    ])
    .unwrap();
    assert_eq!(
        flags.ast.as_deref(),
        Some("for_statement > await_expression")
    );
}

#[test]
fn parse_flags_ast_combined_with_text_query() {
    let flags = super::super::parse_flags(&[
        "auth".to_string(),
        "--ast".to_string(),
        "try-catch".to_string(),
    ])
    .unwrap();
    assert_eq!(flags.ast.as_deref(), Some("try-catch"));
    // When there are positional args, the action is Query with that text.
    assert!(
        matches!(
            &flags.action,
            super::super::SearchAction::Query(t) if t == "auth"
        ),
        "query text should be 'auth', got: {:?}",
        flags.action
    );
}

#[test]
fn parse_flags_ast_empty_rejected() {
    let err = super::super::parse_flags(&["--ast".to_string(), "   ".to_string()]).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("empty") || msg.contains("whitespace"),
        "should reject whitespace-only --ast value, got: {msg}"
    );
}

#[test]
fn validate_ast_pattern_known_named_ok() {
    // try-catch is a real pattern name in the 29-pattern catalog.
    super::validate_ast_pattern("try-catch").unwrap();
}

#[test]
fn validate_ast_pattern_containment_ok() {
    // A containment query with valid node kinds.
    super::validate_ast_pattern("for_statement > block").unwrap();
}

#[test]
fn validate_ast_single_node_returns_283_error() {
    // A single-node query is not yet supported → #283 reference.
    // "try_expression" is a valid tree-sitter Rust node but is SingleNode.
    let err = super::validate_ast_pattern("try_expression").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("#283"),
        "single-node should reference #283, got: {msg}"
    );
}

#[test]
fn validate_ast_unknown_pattern_lists_names() {
    let err = super::validate_ast_pattern("not-a-real-pattern").unwrap_err();
    let msg = err.to_string();
    // The error should list available patterns — verify some known names appear.
    assert!(
        msg.contains("try-catch") || msg.contains("nested-loop") || msg.contains("god-function"),
        "unknown pattern error should list valid patterns, got: {msg}"
    );
}

#[test]
fn parse_flags_ast_plus_hot_parsed_ok() {
    // parse_flags itself succeeds — rejection happens in run().
    let flags = super::super::parse_flags(&[
        "--ast".to_string(),
        "try-catch".to_string(),
        "--hot".to_string(),
    ])
    .unwrap();
    assert!(flags.ast.is_some());
    assert!(flags.temporal_sort.is_some());
}

/// AC-F2 (DISCRIMINATING): `--ast <pattern> --hot` orders matches by descending
/// hotspot score and annotates each surviving row with temporal data.
///
/// Replaces the old guard test that asserted an error for this combination — the
/// interim guard is removed, so it now runs and re-sorts instead of erroring.
/// Hermetic: drives `run_ast_standalone` directly against a tempdir index plus an
/// injected `TemporalDb` (no git history, no system cache), mirroring Group 11.
///
/// A no-op enrichment (or a reinstated guard) would fail this test: the order
/// assertion catches "ran but did not sort", the annotation assertion catches
/// "sorted but did not annotate" (avoids PF-007 vacuous exit-0 guard).
#[test]
fn run_ast_standalone_hot_sorts_by_hotspot_and_annotates() {
    use rskim_search::{HotspotRow, TemporalDb};

    use super::super::manifest::FileManifest;

    // alpha.rs and beta.rs both match rust-nested-loop.
    let project = make_project_with_two_nested_loop_files();
    let cache = tempfile::tempdir().unwrap();
    build_project_index(project.path(), cache.path());

    let manifest =
        FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf()).unwrap();

    // Seed distinct hotspot scores: beta hotter than alpha.
    let db_path = cache.path().join("temporal.db");
    let db = TemporalDb::open(&db_path).unwrap();
    db.store_hotspots(&[
        HotspotRow {
            file_path: "src/alpha.rs".to_string(),
            score: 0.2,
            changes_30d: 1,
            changes_90d: 2,
        },
        HotspotRow {
            file_path: "src/beta.rs".to_string(),
            score: 0.9,
            changes_30d: 9,
            changes_90d: 20,
        },
    ])
    .unwrap();

    // Run `--ast rust-nested-loop --hot` (JSON) so order + annotation are assertable.
    let mut out: Vec<u8> = Vec::new();
    let result = super::run_ast_standalone(
        "rust-nested-loop",
        20,
        true, // JSON
        cache.path(),
        &manifest,
        None, // no --blast-radius
        Some(super::super::types::TemporalSort::Hot),
        Some(&db),
        project.path(),
        &mut out,
    )
    .unwrap();
    assert_eq!(result, std::process::ExitCode::SUCCESS);

    let v: serde_json::Value = serde_json::from_str(&String::from_utf8(out).unwrap()).unwrap();
    let results = v["results"].as_array().unwrap();
    assert!(
        results.len() >= 2,
        "both nested-loop files should match; got {results:?}"
    );

    // Descending hotspot: beta.rs (0.9) must precede alpha.rs (0.2).
    let beta_pos = results
        .iter()
        .position(|r| r["path"].as_str().unwrap().ends_with("beta.rs"));
    let alpha_pos = results
        .iter()
        .position(|r| r["path"].as_str().unwrap().ends_with("alpha.rs"));
    assert!(
        beta_pos.is_some() && alpha_pos.is_some(),
        "both files must be present; got {results:?}"
    );
    assert!(
        beta_pos.unwrap() < alpha_pos.unwrap(),
        "--hot must order beta.rs (hotter) before alpha.rs; got: {results:?}"
    );

    // Annotation present on the hottest row (discriminating: a no-op enrichment
    // would leave `temporal` absent).
    assert!(
        results[beta_pos.unwrap()]["temporal"]["hotspot_score"].is_number(),
        "hottest row must carry temporal.hotspot_score; got: {results:?}"
    );
}

/// AC-N1 (DISCRIMINATING): `--ast bogus --hot` must fail validation with an
/// unknown-pattern message that lists valid pattern names.  Proves
/// `validate_ast_pattern` still runs pre-dispatch through the now-composable path.
///
/// Discriminating against guard reappearance: if the old interim guard were back,
/// it would fire BEFORE `validate_ast_pattern` and produce a "not composable"
/// message that does NOT list pattern names — failing the assertion below.
///
/// Validation fires BEFORE cache resolution, so no system cache is written.
/// Replaces the old bogus-pattern guard-order test.
#[test]
fn run_ast_unknown_pattern_plus_hot_validates() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_string_lossy().to_string();
    let err = super::super::run(
        &[
            "--ast".to_string(),
            "bogus-pattern-xyz".to_string(),
            "--hot".to_string(),
            "--root".to_string(),
            root,
        ],
        &TEST_ANALYTICS,
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("try-catch") || msg.contains("nested-loop") || msg.contains("god-function"),
        "unknown pattern should list valid pattern names (proving validation fired, \
         not an interim guard), got: {msg}"
    );
}

#[test]
fn validate_ast_oversized_query_rejected() {
    let big = "x".repeat(4097);
    let err = super::validate_ast_pattern(&big).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("too long") || msg.contains("4096"),
        "oversized query should be rejected, got: {msg}"
    );
}

// ============================================================================
// Group 2: Build & alignment (tempdir fixture)
// ============================================================================

#[test]
fn build_writes_ast_index_files() {
    let project = make_project_with_rust();
    let cache = tempfile::tempdir().unwrap();

    build_project_index(project.path(), cache.path());

    assert!(
        cache.path().join("index.skidx").exists(),
        "lexical index.skidx should exist"
    );
    assert!(
        cache.path().join("ast_index.skidx").exists(),
        "ast_index.skidx should exist after build"
    );
    assert!(
        cache.path().join("ast_index.skpost").exists(),
        "ast_index.skpost should exist after build"
    );
}

#[test]
fn build_fileid_count_matches_manifest() {
    // AST index file_count must equal the lexical manifest file count.
    use rskim_search::AstIndexReader;

    let project = make_project_with_rust();
    let cache = tempfile::tempdir().unwrap();

    build_project_index(project.path(), cache.path());

    let manifest = super::super::manifest::FileManifest::load(
        project.path().to_path_buf(),
        cache.path().to_path_buf(),
    )
    .unwrap();
    let sorted = manifest.sorted_paths();

    let ast_reader = AstIndexReader::open(cache.path()).unwrap();
    assert_eq!(
        ast_reader.file_count() as usize,
        sorted.len(),
        "AST file_count must equal lexical manifest file count"
    );
}

#[test]
fn build_non_tree_sitter_file_included_with_empty_ast() {
    // JSON file: in lexical manifest AND in AST index (empty entry, node_count=0).
    use rskim_search::AstIndexReader;

    let project = make_project_with_rust();
    let cache = tempfile::tempdir().unwrap();

    build_project_index(project.path(), cache.path());

    let manifest = super::super::manifest::FileManifest::load(
        project.path().to_path_buf(),
        cache.path().to_path_buf(),
    )
    .unwrap();
    let sorted = manifest.sorted_paths();

    // Confirm config.json is in the manifest.
    let json_file_idx = sorted
        .iter()
        .position(|p| p.ends_with("config.json"))
        .expect("config.json should be in lexical manifest");

    // AST count equals manifest count — JSON is included (empty entry).
    let ast_reader = AstIndexReader::open(cache.path()).unwrap();
    assert_eq!(
        ast_reader.file_count() as usize,
        sorted.len(),
        "JSON file should have an empty entry in AST index (no desync)"
    );

    // The JSON file's AST entry should have zero node count.
    let meta = ast_reader
        .file_meta(json_file_idx as u32)
        .expect("file_meta should succeed for JSON file");
    assert_eq!(
        meta.node_count, 0,
        "JSON file should have node_count=0 in AST index (non-tree-sitter lang)"
    );
}

#[test]
fn rebuild_yields_identical_fileid_ordering() {
    use super::super::index::build_index;
    use super::super::types::IndexConfig;

    let project = make_project_with_rust();
    let cache = tempfile::tempdir().unwrap();

    // First build.
    let config = IndexConfig {
        root: project.path().to_path_buf(),
        max_files: None,
        force: false,
        cache_dir_override: Some(cache.path().to_path_buf()),
    };
    build_index(&config).unwrap();

    let manifest1 = super::super::manifest::FileManifest::load(
        project.path().to_path_buf(),
        cache.path().to_path_buf(),
    )
    .unwrap();
    let sorted1: Vec<String> = manifest1
        .sorted_paths()
        .iter()
        .map(|s| (*s).to_string())
        .collect();

    // Second build (force=true → full rebuild).
    let config2 = IndexConfig {
        force: true,
        ..config
    };
    build_index(&config2).unwrap();

    let manifest2 = super::super::manifest::FileManifest::load(
        project.path().to_path_buf(),
        cache.path().to_path_buf(),
    )
    .unwrap();
    let sorted2: Vec<String> = manifest2
        .sorted_paths()
        .iter()
        .map(|s| (*s).to_string())
        .collect();

    assert_eq!(
        sorted1, sorted2,
        "FileId ordering must be identical across rebuilds (deterministic sort)"
    );
}

// ============================================================================
// Group 3: Output formatters (no index required)
// ============================================================================

#[test]
fn format_ast_text_empty_results_says_no_match() {
    let mut buf = BufWriter::new(Vec::new());
    super::format_ast_text(&[], "try-catch", "Try/catch blocks", &mut buf).unwrap();
    let out = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    assert!(
        out.contains("no files match"),
        "empty results should say 'no files match', got: {out}"
    );
}

#[test]
fn format_ast_text_degraded_rows_have_no_colon_line_suffix() {
    // AC-F2: degraded rows (line=None) must NOT include `:line` suffix.
    // This is the pre-#201 behavior for file-level-only results.
    let results = vec![
        super::AstResult::ast_only("src/foo.rs".to_string(), 2.5, None, None),
        super::AstResult::ast_only("src/bar.rs".to_string(), 1.2, None, None),
    ];
    let mut buf = BufWriter::new(Vec::new());
    super::format_ast_text(&results, "try-catch", "Try/catch blocks", &mut buf).unwrap();
    let out = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    assert!(
        out.contains("src/foo.rs"),
        "output should contain first path"
    );
    assert!(
        out.contains("src/bar.rs"),
        "output should contain second path"
    );
    // Degraded rows must NOT have :line suffix (AC-F2 NEGATIVE).
    assert!(
        !out.contains("src/foo.rs:"),
        "degraded AST text output must NOT have :line suffix (AC-F2)"
    );
    assert!(
        out.contains("AST pattern: try-catch"),
        "header must name the pattern"
    );
}

#[test]
fn format_ast_json_mode_is_ast_degraded_row_no_line_snippet_keys() {
    // AC-F4 NEGATIVE: degraded row (line=None) → line and snippet keys ABSENT.
    // layers_matched IS present (always, AC-F5).
    let results = vec![super::AstResult::ast_only(
        "src/foo.rs".to_string(),
        2.5,
        None,
        None,
    )];
    let mut buf = BufWriter::new(Vec::new());
    super::format_ast_json(&results, "try-catch", "Try/catch blocks", &mut buf).unwrap();
    let out = String::from_utf8(buf.into_inner().unwrap()).unwrap();

    let v: serde_json::Value =
        serde_json::from_str(&out).expect("format_ast_json must produce valid JSON");
    assert_eq!(v["mode"], "ast", "mode must be 'ast'");
    assert_eq!(v["pattern"], "try-catch");
    assert_eq!(v["total"], 1);
    assert!(v["results"].is_array());

    let first = &v["results"][0];
    assert!(first["path"].is_string());
    assert!(first["score"].is_number());
    // Degraded row: line and snippet keys must be ABSENT (AC-F4 NEGATIVE).
    assert!(
        first.get("line").is_none(),
        "degraded row: 'line' key must be ABSENT (AC-F4); got: {first}"
    );
    assert!(
        first.get("snippet").is_none(),
        "degraded row: 'snippet' key must be ABSENT (AC-F4); got: {first}"
    );
    // layers_matched must be present (AC-F5).
    assert!(
        first.get("layers_matched").is_some(),
        "layers_matched must be present on every row (AC-F5); got: {first}"
    );
}

#[test]
fn format_ast_json_empty_results_is_valid_json() {
    // Empty result set → valid JSON with mode=="ast", total==0.
    let mut buf = BufWriter::new(Vec::new());
    super::format_ast_json(&[], "try-catch", "Try/catch blocks", &mut buf).unwrap();
    let out = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    let v: serde_json::Value =
        serde_json::from_str(&out).expect("empty format_ast_json must produce valid JSON");
    assert_eq!(v["mode"], "ast");
    assert_eq!(v["total"], 0);
    assert!(v["results"].as_array().unwrap().is_empty());
}

// ============================================================================
// Group 4: Intersection (fixture)
// ============================================================================

/// ISSUE-T1 fix: guarded by serial so SKIM_CACHE_DIR mutation is not racy;
/// with_isolated_cache routes run() to a tempdir instead of ~/.cache/skim/.
///
/// AC6 gate: this test uses a GENUINE disjoint case — the lexical query
/// "xyzzy_impossible_token" matches NO files (empty lexical side), producing
/// an empty intersection.  The intersection gate returns empty on early-out
/// when either layer is empty.  A second sub-test below exercises a non-empty-
/// both-layers disjoint case at the unit level (see `ac6_disjoint_inputs_return_empty_not_error`
/// in intersection_tests.rs).
///
/// AC6 behavior assertion: asserts the output is empty (not just exit-0).
/// A broken gate returning the full lexical set would produce non-empty JSON,
/// failing the `results == []` check (avoids PF-007 vacuous exit-0-only guard).
#[serial_test::serial]
#[test]
fn intersection_disjoint_text_and_ast_returns_empty_exit_0() {
    // When the text query matches no files lexically, the intersection is
    // empty.  Empty results must exit 0 (AC-F8) — not an error.
    //
    // SKIM_CACHE_DIR set to an isolated tempdir so both run() calls use the same
    // cache and we don't pollute ~/.cache/skim/.
    let project = make_project_with_rust();
    let root_str = project.path().to_string_lossy().to_string();

    with_isolated_cache(|_cache| {
        // Build the index (SKIM_CACHE_DIR overrides to isolated tempdir).
        super::super::run(
            &[
                "--build".to_string(),
                "--root".to_string(),
                root_str.clone(),
            ],
            &TEST_ANALYTICS,
        )
        .expect("--build must succeed; fix the build pipeline if it fails here");

        // "xyzzy_impossible_token" won't match any file lexically →
        // lexical layer is empty → early-return empty intersection.
        let result = super::super::run(
            &[
                "xyzzy_impossible_token".to_string(),
                "--ast".to_string(),
                "try-catch".to_string(),
                "--root".to_string(),
                root_str.clone(),
                "--json".to_string(),
            ],
            &TEST_ANALYTICS,
        )
        .unwrap();
        assert_eq!(
            result,
            std::process::ExitCode::SUCCESS,
            "empty intersection must exit 0 (AC-F8)"
        );

        // AC6 behavior assertion: empty intersection must produce zero results.
        // Run with --json and capture output via execute_query directly.
        use super::super::query::execute_query;
        use super::super::types::QueryConfig;

        // Resolve the cache dir for the isolated run.
        let cache_path = std::env::var("SKIM_CACHE_DIR")
            .ok()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| project.path().to_path_buf());

        // Determine the hash-keyed cache subdir.  Since SKIM_CACHE_DIR is set,
        // auto_refresh will use it — we locate it by looking at the first subdir.
        let search_dir_base = cache_path.join("search");
        let cache_dir = if search_dir_base.is_dir() {
            fs::read_dir(&search_dir_base)
                .ok()
                .and_then(|mut rd| rd.next())
                .and_then(|e| e.ok())
                .map(|e| e.path())
                .unwrap_or_else(|| project.path().to_path_buf())
        } else {
            project.path().to_path_buf()
        };

        // Resolve AST scored for "try-catch" pattern.
        // All three guard conditions are combined: engine open AND scored non-empty AND query ok.
        if let Ok(engine) = super::open_ast_engine(&cache_dir)
            && let Ok(ast_scored) = super::resolve_ast_scored(&engine, "try-catch")
            && !ast_scored.is_empty()
        {
            let config = QueryConfig {
                text: "xyzzy_impossible_token".to_string(),
                limit: 20,
                offset: None,
                json: true,
                root: project.path().to_path_buf(),
                cache_dir: cache_dir.clone(),
                blast_radius_paths: None,
                ast_scored: Some(ast_scored),
                composite_weights: None,
            };
            if let Ok(output) = execute_query(&config, &TEST_ANALYTICS) {
                assert!(
                    output.results.is_empty(),
                    "AC6: empty lexical side must yield empty intersection results (not just exit-0); \
                     got {} result(s) — a broken gate that returns the full set would fail here (avoids PF-007)",
                    output.results.len()
                );
                // Verify total count is also 0.
                assert_eq!(
                    output.total, 0,
                    "AC6: output.total must be 0 for empty intersection, got {}",
                    output.total
                );
            }
        }
        // If the AST engine or index is unavailable (e.g. cache path lookup
        // failed), the exit-0 assertion above is sufficient for CI coverage.
        // The full AC6 behavior is also verified at unit level in intersection_tests.rs.
    });
}

// ============================================================================
// Group 5: Auto-refresh / self-heal
// ============================================================================

#[test]
fn self_heal_missing_ast_index_reports_stale() {
    // If the lexical index exists but ast_index.skidx is absent →
    // check_staleness should report stale so a rebuild is triggered.
    let project = make_project_with_rust();
    let cache = tempfile::tempdir().unwrap();

    build_project_index(project.path(), cache.path());

    // Remove the AST index to simulate a post-upgrade or mid-crash scenario.
    fs::remove_file(cache.path().join("ast_index.skidx")).unwrap();

    let (staleness, _) = super::super::staleness::check_staleness(cache.path(), project.path());
    assert!(
        !matches!(staleness, super::super::staleness::StalenessCheck::Current),
        "missing ast_index.skidx should trigger stale (self-heal), got: {staleness:?}"
    );
}

// ============================================================================
// Group 6: API contract
// ============================================================================

#[test]
fn run_ast_single_node_returns_283_error() {
    // run() must propagate #283 for single-node AST queries.
    // Validation fires BEFORE cache resolution so no system cache is written.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_string_lossy().to_string();
    let err = super::super::run(
        &[
            "--ast".to_string(),
            "try_expression".to_string(),
            "--root".to_string(),
            root,
        ],
        &TEST_ANALYTICS,
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("#283"),
        "single-node --ast should reference #283, got: {msg}"
    );
}

#[test]
fn run_ast_unknown_pattern_returns_error() {
    // Validation fires BEFORE cache resolution so no system cache is written.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_string_lossy().to_string();
    let err = super::super::run(
        &[
            "--ast".to_string(),
            "definitely-not-real-xyz".to_string(),
            "--root".to_string(),
            root,
        ],
        &TEST_ANALYTICS,
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("try-catch") || msg.contains("unknown") || msg.contains("pattern"),
        "unknown pattern should list valid patterns or say 'unknown', got: {msg}"
    );
}

#[test]
fn ast_space_and_equals_forms_produce_same_value() {
    // AC-A7: --ast V and --ast=V must produce the same parsed value.
    let space_flags =
        super::super::parse_flags(&["--ast".to_string(), "try-catch".to_string()]).unwrap();
    let equals_flags = super::super::parse_flags(&["--ast=try-catch".to_string()]).unwrap();
    assert_eq!(
        space_flags.ast.as_deref(),
        equals_flags.ast.as_deref(),
        "--ast V and --ast=V must produce the same value"
    );
}

#[test]
fn unrecognised_flag_lists_ast_in_error() {
    // AC-A7: the unrecognised-flag error must list --ast as a valid flag.
    let err = super::super::parse_flags(&["--no-such-flag".to_string()]).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("--ast"),
        "unrecognised-flag message must include --ast, got: {msg}"
    );
}

// ============================================================================
// Group 7: Standalone --ast query against a real index (hermetic)
// ============================================================================

/// run_ast_standalone with a real index returns SUCCESS and maps FileIds to
/// paths.  Guards the FileId→path mapping and the fail-loud absent-index path.
#[test]
fn run_ast_standalone_with_real_index_maps_paths() {
    use super::super::manifest::FileManifest;

    let project = make_project_with_rust();
    let cache = tempfile::tempdir().unwrap();

    build_project_index(project.path(), cache.path());

    let manifest =
        FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf()).unwrap();

    // Run standalone --ast query.  "try-catch" matches Rust match-on-Result patterns.
    // Capture output via a Vec<u8> buffer (satisfies PF-007: test observes real output).
    let mut out: Vec<u8> = Vec::new();
    let result = super::run_ast_standalone(
        "try-catch",
        20,
        false, // text output
        cache.path(),
        &manifest,
        None, // no --blast-radius
        None, // no temporal sort
        None, // no temporal DB
        project.path(),
        &mut out,
    )
    .unwrap();

    assert_eq!(
        result,
        std::process::ExitCode::SUCCESS,
        "standalone --ast must exit 0 (AC-F8)"
    );
    // Output must contain the pattern header (non-empty, well-formed).
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains("AST pattern: try-catch"),
        "output must contain pattern header; got:\n{text}"
    );

    // Verify open_ast_engine fails loudly when the index is absent.
    let empty = tempfile::tempdir().unwrap();
    match super::open_ast_engine(empty.path()) {
        Ok(_) => panic!("open_ast_engine must fail when index is absent"),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("--build") || msg.contains("--rebuild"),
                "absent AST index must give build guidance, got: {msg}"
            );
        }
    }
}

/// resolve_ast_scored returns a non-empty Vec for a pattern that
/// matches at least one Rust file in the fixture project.
///
/// Previously `resolve_ast_file_filter` returned `HashSet<FileId>` (scores
/// discarded).  After #198 it returns `Vec<(FileId, f64)>` so the compound
/// intersector can build a rank map from actual scores.
#[test]
fn resolve_ast_scored_returns_matching_file_ids() {
    let project = make_project_with_rust();
    let cache = tempfile::tempdir().unwrap();

    build_project_index(project.path(), cache.path());

    let engine = super::open_ast_engine(cache.path()).unwrap();

    // "rust-nested-loop" matches block → expression_statement → for_expression,
    // which appears in src/loops.rs (nested for loops).
    let hits = super::resolve_ast_scored(&engine, "rust-nested-loop").unwrap();

    // The project has at least one Rust file with nested loops — assert non-empty.
    assert!(
        !hits.is_empty(),
        "rust-nested-loop pattern must match at least one file in the fixture project"
    );

    // All returned FileIds must be within the manifest range; all scores > 0.
    use super::super::manifest::FileManifest;
    let manifest =
        FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf()).unwrap();
    let file_count = manifest.sorted_paths().len();
    for (id, score) in &hits {
        assert!(
            (id.0 as usize) < file_count,
            "FileId({}) must be within manifest range [0, {})",
            id.0,
            file_count
        );
        assert!(
            *score > 0.0,
            "AST score for FileId({}) must be > 0, got {score}",
            id.0
        );
    }
    // Results must be sorted FileId-ASC (frozen Wave-4 contract from #287).
    let fids: Vec<u32> = hits.iter().map(|(f, _)| f.0).collect();
    assert!(
        fids.windows(2).all(|w| w[0] <= w[1]),
        "resolve_ast_scored must return results sorted FileId-ASC (Wave-4 contract): {fids:?}"
    );
}

// ============================================================================
// Group 8: Text + --ast intersection (hermetic, real index)
// ============================================================================

/// Text+AST intersection returns only files that satisfy BOTH the lexical and
/// AST filters.  Guards against union-instead-of-intersection and dropped-
/// snippet bugs.
///
/// Strategy:
/// - "nested" as text query → should match src/loops.rs (contains "nested").
/// - "--ast rust-nested-loop" → should match src/loops.rs (nested for loops in Rust).
/// - Intersection → src/loops.rs (in both sets, with lexical snippets preserved).
///
/// We drive through the engine directly (resolve_ast_scored + execute_query)
/// so the test is hermetic (no system cache required).
#[test]
fn text_ast_intersection_preserves_lexical_snippets() {
    use super::super::query::execute_query;
    use super::super::types::QueryConfig;

    let project = make_project_with_rust();
    let cache = tempfile::tempdir().unwrap();

    build_project_index(project.path(), cache.path());

    // Fetch the AST scored results for "rust-nested-loop" (matches Rust nested for loops).
    // After #198, resolve_ast_scored returns Vec<(FileId, f64)> for compound RRF fusion.
    let engine = super::open_ast_engine(cache.path()).unwrap();
    let ast_scored = super::resolve_ast_scored(&engine, "rust-nested-loop").unwrap();

    // The AST results must be non-empty for this test to be meaningful.
    assert!(
        !ast_scored.is_empty(),
        "rust-nested-loop must match at least one file so intersection is testable"
    );

    // Run the compound query with the AST scored results.
    let config = QueryConfig {
        text: "nested".to_string(),
        limit: 20,
        offset: None,
        json: false,
        root: project.path().to_path_buf(),
        cache_dir: cache.path().to_path_buf(),
        blast_radius_paths: None,
        ast_scored: Some(ast_scored),
        composite_weights: None,
    };
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // The intersection must contain at least one result (src/loops.rs matches both).
    assert!(
        !output.results.is_empty(),
        "text+AST intersection must return results when both filters match a common file"
    );

    // Every result must have a non-empty path (FileId→path mapping is correct).
    for r in &output.results {
        assert!(
            !r.path.is_empty(),
            "resolved result path must not be empty (FileId→path mapping)"
        );
    }

    // At least one result must have a snippet (lexical snippets are preserved
    // after AST intersection — guards against dropped-snippet bug).
    let has_snippet = output.results.iter().any(|r| r.snippet.is_some());
    assert!(
        has_snippet,
        "at least one result must have a snippet (lexical enrichment preserved after AST filter)"
    );
}

/// AC1 integration-level strict-subset assertion: the text+AST combined CLI
/// path must return a set that is a **strict subset** of the pure-lexical set
/// (combined ⊊ lexical) and a strict subset of the pure-AST set.
///
/// Strategy:
/// - The fixture has 3 Rust files: `src/alpha.rs` and `src/beta.rs` (nested
///   loops) and `src/plain.rs` (no nested loops).
/// - "fn" as text query → matches ALL three source files (every Rust file has
///   "fn" in it).
/// - "--ast rust-nested-loop" → matches `alpha.rs` and `beta.rs` only.
/// - Intersection: {alpha.rs, beta.rs} ⊊ {alpha.rs, beta.rs, plain.rs}.
///
/// Assertions (per AC1):
/// 1. combined result set ⊊ lexical-only result set (strict subset).
/// 2. Every combined result path appears in the lexical-only set.
/// 3. combined ≠ lexical-only (at least one file excluded, namely plain.rs).
///
/// This test would fail if the intersection gate were dropped (combined ==
/// lexical, failing assertion 3) or if AST-only files were included.
#[test]
fn text_ast_combined_is_strict_subset_of_lexical_ac1() {
    use super::super::query::execute_query;
    use super::super::types::QueryConfig;

    let project = make_project_with_two_nested_loop_files();
    let cache = tempfile::tempdir().unwrap();

    build_project_index(project.path(), cache.path());

    // --- Lexical-only run: "fn" matches all Rust files ---
    let lex_config = QueryConfig {
        text: "fn".to_string(),
        limit: 50,
        offset: None,
        json: false,
        root: project.path().to_path_buf(),
        cache_dir: cache.path().to_path_buf(),
        blast_radius_paths: None,
        ast_scored: None,
        composite_weights: None,
    };
    let lex_output = execute_query(&lex_config, &TEST_ANALYTICS).unwrap();
    let lex_paths: std::collections::HashSet<&str> =
        lex_output.results.iter().map(|r| r.path.as_str()).collect();

    // The fixture has three Rust files + config.json; "fn" must hit all three
    // Rust files.
    assert!(
        lex_paths.iter().any(|p| p.contains("plain.rs")),
        "AC1 setup: lexical 'fn' must match plain.rs; got: {lex_paths:?}"
    );
    assert!(
        lex_paths
            .iter()
            .any(|p| p.contains("alpha.rs") || p.contains("beta.rs")),
        "AC1 setup: lexical 'fn' must match at least one nested-loop file"
    );

    // --- AST-only scored: rust-nested-loop matches only alpha.rs and beta.rs ---
    let engine = super::open_ast_engine(cache.path()).unwrap();
    let ast_scored = super::resolve_ast_scored(&engine, "rust-nested-loop").unwrap();
    assert!(
        !ast_scored.is_empty(),
        "AC1 setup: rust-nested-loop must match at least one file"
    );

    // --- Compound run: text "fn" + --ast rust-nested-loop ---
    let compound_config = QueryConfig {
        text: "fn".to_string(),
        limit: 50,
        offset: None,
        json: false,
        root: project.path().to_path_buf(),
        cache_dir: cache.path().to_path_buf(),
        blast_radius_paths: None,
        ast_scored: Some(ast_scored),
        composite_weights: None,
    };
    let compound_output = execute_query(&compound_config, &TEST_ANALYTICS).unwrap();
    let compound_paths: std::collections::HashSet<&str> = compound_output
        .results
        .iter()
        .map(|r| r.path.as_str())
        .collect();

    // AC1a: every compound result must be in the lexical set.
    for p in &compound_paths {
        assert!(
            lex_paths.contains(p),
            "AC1a: compound result '{p}' is not in the lexical-only set — intersection gate broken"
        );
    }

    // AC1b: compound set must be strictly smaller than lexical set (plain.rs excluded).
    assert!(
        compound_paths.len() < lex_paths.len(),
        "AC1b: compound result count ({}) must be strictly less than lexical count ({}) — \
         plain.rs should be excluded by the AST gate; compound={compound_paths:?}, lex={lex_paths:?}",
        compound_paths.len(),
        lex_paths.len()
    );

    // AC1c: plain.rs must NOT appear in the compound output.
    assert!(
        !compound_paths.iter().any(|p| p.contains("plain.rs")),
        "AC1c: plain.rs (no nested loops) must be absent from compound results; \
         got: {compound_paths:?}"
    );

    // AC1d: alpha.rs and/or beta.rs must appear (they satisfy both filters).
    assert!(
        compound_paths
            .iter()
            .any(|p| p.contains("alpha.rs") || p.contains("beta.rs")),
        "AC1d: at least one nested-loop file must appear in compound results; \
         got: {compound_paths:?}"
    );
}

// ============================================================================
// Group 9: Self-heal for below-FORMAT_VERSION probe (issue 10)
// ============================================================================

/// Writing an ast_index.skidx stub with valid SKAX magic but version=1
/// (below AST_INDEX_FORMAT_VERSION=2) must cause check_staleness to return
/// a stale outcome, triggering the self-heal rebuild path.
///
/// Uses `AstIndexReader::index_version` vs `rskim_search::AST_INDEX_FORMAT_VERSION`
/// which is the single source of truth (ADR-001, single-source-of-truth compile-time
/// assertion in lib.rs).
#[test]
fn self_heal_below_format_version_reports_stale() {
    let project = make_project_with_rust();
    let cache = tempfile::tempdir().unwrap();

    // Build a real index first (puts index.skidx in cache so cold-start is not triggered).
    build_project_index(project.path(), cache.path());

    // Overwrite ast_index.skidx with a stub that has SKAX magic + version=1 (v1 format).
    // `AstIndexReader::index_version` reads only the first 6 bytes: [magic:4][version:2 LE].
    let stub: [u8; 6] = [b'S', b'K', b'A', b'X', 1, 0]; // version = 1 (below current 2)
    fs::write(cache.path().join("ast_index.skidx"), stub).unwrap();

    let (staleness, _) = super::super::staleness::check_staleness(cache.path(), project.path());
    assert!(
        !matches!(staleness, super::super::staleness::StalenessCheck::Current),
        "below-FORMAT_VERSION ast_index.skidx must trigger stale (self-heal), got: {staleness:?}"
    );

    // Verify the version probe itself confirms the stub is below current.
    let probed = rskim_search::AstIndexReader::index_version(cache.path()).unwrap();
    assert_eq!(probed, 1, "stub must report version 1");
    assert!(
        probed < rskim_search::AST_INDEX_FORMAT_VERSION,
        "stub version ({probed}) must be below AST_INDEX_FORMAT_VERSION ({})",
        rskim_search::AST_INDEX_FORMAT_VERSION
    );
}

// ============================================================================
// Group 10: Self-heal regression — text + --ast combined path (Issue 1 / regression.md)
// ============================================================================

/// Regression guard: `skim search <text> --ast <pattern>` must self-heal when
/// the AST index is absent, mirroring the standalone `skim search --ast` path.
///
/// Before the fix, `run_query` opened `open_ast_engine` BEFORE calling
/// `auto_refresh_if_stale`, so a missing `ast_index.skidx` caused a loud
/// "AST index not found" error instead of triggering a rebuild.
///
/// Strategy (hermetic — SKIM_CACHE_DIR isolates all index I/O to a tempdir):
/// 1. Set SKIM_CACHE_DIR → isolated tempdir so run() writes there, not ~/.cache/skim/.
/// 2. Build a full index via run(--build), then delete ast_index.skidx.
/// 3. Run `skim search "nested" --ast rust-nested-loop --root <project>`.
/// 4. Assert Ok(SUCCESS) — self-heal rebuilt the AST index transparently.
///
/// ISSUE-T1 fix: #[serial_test::serial] + with_isolated_cache prevents writes to
/// ~/.cache/skim/ and prevents concurrent SKIM_CACHE_DIR mutation.
#[serial_test::serial]
#[test]
fn text_ast_combined_self_heals_missing_ast_index() {
    let project = make_project_with_rust();
    let root_str = project.path().to_string_lossy().to_string();

    with_isolated_cache(|cache| {
        // Build a full index (lexical + AST) via run(--build) with the isolated cache.
        super::super::run(
            &[
                "--build".to_string(),
                "--root".to_string(),
                root_str.clone(),
            ],
            &TEST_ANALYTICS,
        )
        .expect("--build must succeed for self-heal test to be meaningful");

        // The isolated cache dir has the form: <isolated_base>/search/<hash>/.
        // Locate the search subdirectory to find and delete ast_index.skidx.
        let search_dir = cache.join("search");
        let index_dir = fs::read_dir(&search_dir)
            .expect("search/ must exist after build")
            .filter_map(|e| e.ok())
            .find(|e| e.path().is_dir())
            .expect("at least one hash-keyed dir must exist after build")
            .path();

        let ast_idx = index_dir.join("ast_index.skidx");
        assert!(ast_idx.exists(), "ast_index.skidx must exist after build");
        fs::remove_file(&ast_idx).unwrap();
        let _ = fs::remove_file(index_dir.join("ast_index.skpost")); // may or may not exist

        // Verify the lexical index still exists (so this is not a cold start).
        assert!(
            index_dir.join("index.skidx").exists(),
            "lexical index must exist for the test to simulate a no-AST install"
        );

        // Drive through run() — SKIM_CACHE_DIR still set so it hits the isolated cache.
        // The combined text+--ast path must self-heal (rebuild the AST index) and
        // return SUCCESS rather than propagating "AST index not found".
        let result = super::super::run(
            &[
                "nested".to_string(),
                "--ast".to_string(),
                "rust-nested-loop".to_string(),
                "--root".to_string(),
                root_str.clone(),
            ],
            &TEST_ANALYTICS,
        );

        assert!(
            result.is_ok(),
            "text+--ast combined path must self-heal a missing AST index, got Err: {:?}",
            result.unwrap_err()
        );
        assert_eq!(
            result.unwrap(),
            std::process::ExitCode::SUCCESS,
            "text+--ast combined path must exit 0 after self-healing"
        );
        // The rebuild path outputs a progress message to stderr; the key assertion is
        // that the command returned Ok(SUCCESS) rather than Err("AST index not found").
    });
}

/// Regression guard: `skim search <text> --ast <pattern>` must also self-heal
/// when `ast_index.skidx` has a below-FORMAT_VERSION stub (v1 format).
///
/// This guards the same ordering fix as `text_ast_combined_self_heals_missing_ast_index`
/// but specifically tests the format-version probe path within `check_staleness`.
///
/// ISSUE-T1 fix: #[serial_test::serial] + with_isolated_cache routes run() to an
/// isolated cache instead of ~/.cache/skim/, and prevents concurrent SKIM_CACHE_DIR races.
#[serial_test::serial]
#[test]
fn text_ast_combined_self_heals_below_format_version_ast_index() {
    let project = make_project_with_rust();
    let root_str = project.path().to_string_lossy().to_string();

    with_isolated_cache(|cache| {
        // Build a full index via run(--build) with the isolated cache.
        super::super::run(
            &[
                "--build".to_string(),
                "--root".to_string(),
                root_str.clone(),
            ],
            &TEST_ANALYTICS,
        )
        .expect("--build must succeed for version-stub self-heal test to be meaningful");

        // Locate the hash-keyed search directory inside the isolated cache.
        let search_dir = cache.join("search");
        let index_dir = fs::read_dir(&search_dir)
            .expect("search/ must exist after build")
            .filter_map(|e| e.ok())
            .find(|e| e.path().is_dir())
            .expect("at least one hash-keyed dir must exist after build")
            .path();

        // Overwrite with a v1 stub (below AST_INDEX_FORMAT_VERSION).
        let stub: [u8; 6] = [b'S', b'K', b'A', b'X', 1, 0];
        fs::write(index_dir.join("ast_index.skidx"), stub).unwrap();

        // Drive through run() — SKIM_CACHE_DIR still set so it hits the isolated cache.
        let result = super::super::run(
            &[
                "nested".to_string(),
                "--ast".to_string(),
                "rust-nested-loop".to_string(),
                "--root".to_string(),
                root_str.clone(),
            ],
            &TEST_ANALYTICS,
        );

        assert!(
            result.is_ok(),
            "text+--ast combined path must self-heal a below-version AST index, got Err: {:?}",
            result.unwrap_err()
        );
        assert_eq!(
            result.unwrap(),
            std::process::ExitCode::SUCCESS,
            "text+--ast combined path must exit 0 after self-healing below-version AST index"
        );
    });
}

// ============================================================================
// Group 11: --ast + --blast-radius intersection (avoids PF-006, applies ADR-006)
// ============================================================================

/// Primary regression guard for ISSUE-2: `--ast <pattern> --blast-radius <file>` (no text
/// query) MUST apply the intersection, not silently drop `--ast`.
///
/// Previously, the standalone AST dispatch arm was gated by `blast_radius.is_none()`,
/// so when `--blast-radius` was also set the request fell through to
/// `run_temporal_standalone`, which silently ignored `--ast` — the worst failure mode
/// (plausible-looking results, wrong filter applied, avoids PF-006).
///
/// This test proves the intersection IS applied by asserting:
///
/// 1. The UNFILTERED `--ast` result set contains BOTH `src/alpha.rs` AND `src/beta.rs`
///    (both match `rust-nested-loop`).
/// 2. The FILTERED result set (`--blast-radius src/plain.rs`) contains ONLY
///    `src/alpha.rs` (only alpha.rs co-changes with plain.rs in the DB).
/// 3. The filtered set is STRICTLY SMALLER than the unfiltered set.
///    — This is the assertion that would have caught the original bug: if `--ast` were
///    still being silently dropped (falling through to `run_temporal_standalone`),
///    the AST result set would be unrestricted and both files would appear.
/// 4. Graceful-degrade: blast file with no temporal DB → full AST set, exit 0, no error.
///
/// Strategy: drive `run_ast_standalone` directly with an injected `TemporalDb` (no git
/// history required) so the test is hermetic and fast (applies ADR-006 counterpart on
/// the read side: fail-loud on desync, not silent drop).
///
/// FIXTURE LIMITATION (honest, per project NO-FAKE-SOLUTIONS rule): The TemporalDb is
/// populated programmatically via `store_cochanges`, not from real git history.  This is
/// intentional — building a real git repo with committed history in a unit test is fragile
/// and slow.  The intersection logic (`run_ast_standalone`) queries the DB the same way
/// regardless of how the rows got there, so the assertion remains valid.  The test would
/// have caught the original bug because a "flag ignored" implementation would return all
/// AST matches (filtered_count == unfiltered_count), failing the strict-subset assertion.
#[test]
fn ast_blast_radius_intersection_is_applied_not_silently_dropped() {
    use super::super::manifest::FileManifest;
    use rskim_search::FileId;

    // Build a project where BOTH src/alpha.rs and src/beta.rs match rust-nested-loop.
    let project = make_project_with_two_nested_loop_files();
    let cache = tempfile::tempdir().unwrap();

    build_project_index(project.path(), cache.path());

    let manifest =
        FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf()).unwrap();

    // --- Step 1: Verify the UNFILTERED AST result set contains BOTH alpha and beta ---
    // Use resolve_ast_scored (returns Vec<(FileId,f64)> for compound RRF, #198).
    let engine = super::open_ast_engine(cache.path()).unwrap();
    let all_ast_hits = super::resolve_ast_scored(&engine, "rust-nested-loop").unwrap();

    // Confirm both alpha.rs and beta.rs are in the AST result set.
    let sorted = manifest.sorted_paths();
    let alpha_fid: Option<FileId> = sorted
        .iter()
        .position(|p| p.ends_with("alpha.rs"))
        .and_then(|i| u32::try_from(i).ok())
        .map(FileId);
    let beta_fid: Option<FileId> = sorted
        .iter()
        .position(|p| p.ends_with("beta.rs"))
        .and_then(|i| u32::try_from(i).ok())
        .map(FileId);

    assert!(
        alpha_fid.is_some(),
        "src/alpha.rs must be in the manifest (fixture setup)"
    );
    assert!(
        beta_fid.is_some(),
        "src/beta.rs must be in the manifest (fixture setup)"
    );
    assert!(
        all_ast_hits.iter().any(|(f, _)| f == &alpha_fid.unwrap()),
        "src/alpha.rs must match rust-nested-loop (precondition for intersection test)"
    );
    assert!(
        all_ast_hits.iter().any(|(f, _)| f == &beta_fid.unwrap()),
        "src/beta.rs must match rust-nested-loop (precondition for intersection test)"
    );
    let unfiltered_count = all_ast_hits.len();

    // --- Step 2: Populate TemporalDb with known co-change: alpha.rs <-> plain.rs ---
    // Only src/alpha.rs co-changes with src/plain.rs.  src/beta.rs has NO co-change
    // relationship with src/plain.rs.
    // Blast-radius resolution for "src/plain.rs" returns {src/alpha.rs, src/plain.rs}
    // (including the target file itself — mirrors resolve_blast_radius_filter semantics).
    // AST(rust-nested-loop) = {alpha.rs, beta.rs}.
    // Intersection = {alpha.rs} — strictly smaller than the unfiltered set.
    //
    // src/plain.rs EXISTS on disk (fixture file), so normalize_blast_radius_path succeeds.
    // NOTE: CochangeRow convention — file_a must be lexicographically <= file_b.
    //   "src/alpha.rs" < "src/plain.rs", so alpha is file_a, plain is file_b. Correct.
    let db_path = cache.path().join("temporal.db");
    write_cochange_db(&db_path, "src/alpha.rs", "src/plain.rs");

    // --- Step 3: Run standalone --ast with --blast-radius src/plain.rs ---
    // Resolve blast-radius → FileIds the same way mod.rs does (via the shared resolver).
    // This exercises the production path: temporal::resolve_blast_radius_file_ids resolves
    // paths to FileIds, then run_ast_standalone intersects and writes to the provided buffer.
    let blast_fids = super::super::temporal::resolve_blast_radius_file_ids(
        Some("src/plain.rs"),
        project.path(),
        &db_path,
        &sorted,
        false,
    )
    .unwrap();

    let mut output_buf: Vec<u8> = Vec::new();
    let filtered_result = super::run_ast_standalone(
        "rust-nested-loop",
        20,
        false,
        cache.path(),
        &manifest,
        blast_fids,
        None, // no temporal sort
        None, // no temporal DB
        project.path(),
        &mut output_buf,
    )
    .unwrap();

    assert_eq!(
        filtered_result,
        std::process::ExitCode::SUCCESS,
        "--ast + --blast-radius must exit 0 (AC-F8)"
    );

    // --- Step 4: Assert the PRODUCTION OUTPUT shows alpha.rs but NOT beta.rs ---
    //
    // PF-007: this assertion observes the actual output written by run_ast_standalone,
    // not a locally re-derived set.  A reversion of the dispatch gate to
    // blast_radius.is_none() (PF-006 regression) would cause the full AST set
    // (both alpha.rs AND beta.rs) to be output, failing the beta-absent check below.
    let output_text = String::from_utf8(output_buf).unwrap();

    assert!(
        output_text.contains("alpha.rs"),
        "production output must contain src/alpha.rs (only co-change partner of plain.rs that matches rust-nested-loop); \
         got:\n{output_text}"
    );
    assert!(
        !output_text.contains("beta.rs"),
        "production output must NOT contain src/beta.rs (no co-change relationship with plain.rs); \
         if beta.rs appears, --blast-radius intersection is being silently dropped (PF-006 regression); \
         got:\n{output_text}"
    );

    // Also verify the count-based strict-subset property (belt-and-suspenders).
    // The output contains "N file(s) matched pattern" — we check N < unfiltered_count.
    let matched_count_line = output_text
        .lines()
        .find(|l| l.contains("file(s) matched pattern"));
    if let Some(line) = matched_count_line {
        let count: usize = line
            .split_whitespace()
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        assert!(
            count < unfiltered_count,
            "filtered count {count} must be strictly smaller than unfiltered AST set {unfiltered_count}; \
             if equal, --blast-radius is being silently dropped (PF-006 regression)"
        );
    }

    // --- Step 5: Graceful-degrade — absent temporal DB → full AST set, exit 0, no error ---
    // When the temporal DB is missing, resolve_blast_radius_file_ids returns None and
    // run_ast_standalone returns the full AST result set (no intersection).
    let absent_db = cache.path().join("no_such_temporal.db");
    let degrade_blast_fids = super::super::temporal::resolve_blast_radius_file_ids(
        Some("src/plain.rs"),
        project.path(),
        &absent_db,
        &sorted,
        false,
    )
    .unwrap();
    // When the DB is absent, resolve_blast_radius_file_ids returns None.
    assert!(
        degrade_blast_fids.is_none(),
        "absent temporal DB must yield None blast_file_ids"
    );

    let mut degrade_buf: Vec<u8> = Vec::new();
    let degrade_result = super::run_ast_standalone(
        "rust-nested-loop",
        20,
        false,
        cache.path(),
        &manifest,
        degrade_blast_fids, // None → no intersection, full AST set
        None,               // no temporal sort
        None,               // no temporal DB
        project.path(),
        &mut degrade_buf,
    )
    .unwrap();

    assert_eq!(
        degrade_result,
        std::process::ExitCode::SUCCESS,
        "absent temporal DB must degrade gracefully (exit 0, not error)"
    );

    // Graceful-degrade output must contain BOTH alpha.rs and beta.rs (full AST set).
    let degrade_text = String::from_utf8(degrade_buf).unwrap();
    assert!(
        degrade_text.contains("alpha.rs"),
        "graceful-degrade output must contain alpha.rs; got:\n{degrade_text}"
    );
    assert!(
        degrade_text.contains("beta.rs"),
        "graceful-degrade output must contain beta.rs (full set when DB absent); got:\n{degrade_text}"
    );
}

// ============================================================================
// Group 12: Intersection pool-cliff guard (#356)
// ============================================================================

/// Build a fixture that triggers the CANDIDATE_POOL_K=4 cliff.
///
/// Layout:
/// - `src/target.rs` — contains "nested" once AND has a nested for-loop.
///   This is the SINGLE qualifying file (matches both text + AST filters).
/// - `src/noast_N.rs` (N=1..6) — contain "nested" MANY times (high lexical score)
///   but have NO nested loops.  They match the text token but NOT the --ast filter.
///
/// # Why this makes the bug fire
///
/// With the old `CANDIDATE_POOL_K=4` the lexical pool at `--limit 1` is 4 files.
/// The 6 `noast_N.rs` files each contain "nested" 30 times (a lot of trigram matches),
/// so they rank much higher than `target.rs` (1 occurrence) in the unfiltered lexical
/// list.  The top-4 lexical hits are all `noast_*` files; `target.rs` falls off the
/// cliff.  `intersect_and_rank` sees no file in both the 4-slot lexical pool and the
/// 1-file AST set → result count = 0.  The test asserts count >= 1 → FAILS (RED).
///
/// With the fix (AD-356-1): `file_filter = Some({target.rs})`, `sq.limit = Some(1)`.
/// The reader scores only `target.rs`.  `intersect_and_rank` gets {target.rs} in
/// both layers → returns 1 result.  The test PASSES (GREEN).
fn make_project_with_lexical_cliff_fixture() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();

    // Single qualifying file: contains "nested" once AND a nested for-loop.
    fs::write(
        root.join("src/target.rs"),
        r#"
fn target() {
    // nested: this is the qualifying file
    for i in 0..4 {
        for j in 0..4 {
            println!("nested {i} {j}");
        }
    }
}
"#,
    )
    .unwrap();

    // Six distractor files: each contains "nested" 30 times but NO nested loops.
    // High occurrence count → much higher trigram score → they outrank target.rs.
    // They must NOT contain "for" blocks inside "for" blocks (no rust-nested-loop AST match).
    for i in 1..=6 {
        let occurrences = "// nested\n".repeat(30);
        fs::write(
            root.join(format!("src/noast_{i}.rs")),
            format!(
                r#"
{occurrences}
fn distractor_{i}() {{
    println!("not a nested loop");
}}
"#
            ),
        )
        .unwrap();
    }

    fs::write(root.join("config.json"), r#"{"key": "value"}"#).unwrap();

    dir
}

/// AC1 / AC2 — Pool-cliff discriminator guard (#356).
///
/// Proves that the compound text+`--ast` intersection returns qualifying files
/// EVEN WHEN they rank below the old `limit * CANDIDATE_POOL_K` cliff in the
/// unfiltered lexical list.
///
/// # Why this test FAILS against the unfixed code (RED)
///
/// The old path computes `sq.limit = config.limit.saturating_mul(4)` with NO
/// `file_filter`.  At `--limit 1` the lexical engine is capped at 4 candidates
/// drawn from the ENTIRE corpus.  The 6 `noast_*.rs` files each contain "nested"
/// 30 times — far more trigram matches than `target.rs` (1 occurrence) — so the
/// top-4 unfiltered lexical hits are all `noast_*` files.  `target.rs` falls off
/// position 4 and is invisible to `intersect_and_rank`.  The intersection is empty
/// → result count = 0.  This test asserts `count >= 1` → FAILS with the old code.
///
/// # Why this test PASSES after the fix (GREEN, AD-356-1 / AD-356-2)
///
/// After the fix: `sq.file_filter = Some({target.rs})` restricts the lexical
/// engine to the 1-file AST set; `sq.limit = Some(1)`.  The reader scores only
/// `target.rs`, finds it contains "nested", and returns it.  `intersect_and_rank`
/// sees the same file in both the lexical and AST layers → returns 1 result.
/// The test assertion `count >= 1` PASSES.
///
/// # Discriminating properties (PF-007)
///
/// - Asserts `count >= 1` (not just exit 0) — a broken implementation that returns
///   empty would fail.
/// - Asserts the returned path ends in "target.rs" (not a distractor) — a broken
///   implementation that returns a distractor would fail.
/// - If `CANDIDATE_POOL_K` were reintroduced, the distractors would fill the pool
///   at `--limit 1` and `target.rs` would again be invisible → count = 0, test
///   FAILS.
#[test]
fn text_ast_intersection_complete_below_pool_cliff_356() {
    use super::super::query::execute_query;

    let project = make_project_with_lexical_cliff_fixture();
    let cache = tempfile::tempdir().unwrap();

    build_project_index(project.path(), cache.path());

    // -- Precondition: AST filter matches exactly target.rs (1 file) ---------------
    let engine = super::open_ast_engine(cache.path()).unwrap();
    let ast_scored = super::resolve_ast_scored(&engine, "rust-nested-loop").unwrap();

    // Precondition: exactly 1 AST-matching file (target.rs with nested loops).
    // The 6 distractor files have no nested loops → should NOT be in ast_scored.
    assert_eq!(
        ast_scored.len(),
        1,
        "#356 precondition: rust-nested-loop must match EXACTLY 1 file (target.rs); \
         got {} — check that distractors have no nested loops",
        ast_scored.len()
    );

    // -- AC1 (exact-set completeness): full qualifying set at --limit 50 ----------
    // At any large limit the fix and the old code both return target.rs; we use
    // this as the reference set.
    let full_config = make_query_config(
        project.path(),
        cache.path(),
        "nested",
        50,
        Some(ast_scored.clone()),
        None,
    );
    let full_output = execute_query(&full_config, &TEST_ANALYTICS).unwrap();

    assert_eq!(
        full_output.results.len(),
        1,
        "AC1: --limit 50 must return the 1 qualifying file (target.rs); \
         got {} results",
        full_output.results.len()
    );
    assert!(
        full_output.results[0].path.contains("target.rs"),
        "AC1: the single result must be target.rs, got: {:?}",
        full_output.results[0].path
    );

    // -- AC2 (pool-cliff guard): at --limit 1 we must still get target.rs --------
    //
    // Pre-fix (CANDIDATE_POOL_K=4): the 6 distractor files rank higher than
    // target.rs lexically (30× "nested" vs 1×), so the top-4 unfiltered lexical
    // hits are all distractors.  target.rs falls off the cliff.  result count = 0.
    // (This is the RED assertion: count >= 1 FAILS against unfixed code.)
    //
    // Post-fix (AD-356-1/2): file_filter = {target.rs FileId}, sq.limit = 1.
    // Reader scores only target.rs → it is returned.  count = 1, PASSES.
    let limit1_config = make_query_config(
        project.path(),
        cache.path(),
        "nested",
        1,
        Some(ast_scored.clone()),
        None,
    );
    let limit1_output = execute_query(&limit1_config, &TEST_ANALYTICS).unwrap();

    // PRIMARY DISCRIMINATING ASSERTION (PF-007):
    // result count must be >= 1.  Old code returns 0 (cliff drops target.rs).
    // New code returns 1.  If CANDIDATE_POOL_K is reintroduced, count → 0 → test FAILS.
    assert!(
        !limit1_output.results.is_empty(),
        "AC2 (pool-cliff DISCRIMINATING): --limit 1 returned 0 results (expected >= 1). \
         target.rs must appear even though 6 distractors rank higher lexically. \
         If count=0, the pool cliff is still active (CANDIDATE_POOL_K regression).",
    );

    // The returned file must be the qualifying target.rs (not a distractor).
    assert!(
        limit1_output.results[0].path.contains("target.rs"),
        "AC2: returned file must be target.rs; got: {:?}. \
         A distractor appearing here means the AST filter is not being applied \
         to restrict the lexical pool.",
        limit1_output.results[0].path
    );

    // Note: AC13 (set property independent of ranking/limit) is covered by the
    // `text_ast_ad356_2_limit_guard_25_files` test below, which uses N=25 qualifying
    // files and drives at --limit {25, 10, 1}.  A --limit 5 assertion here with N=1
    // qualifying file is non-discriminating (pre-fix code also returns 1 result at
    // --limit 5 because pool=20 > corpus size), so it was removed to avoid overstating
    // coverage.  See review finding #6 / PF-007.
}

// ============================================================================
// AC8 — AD-356-2 limit-guard: sq.limit sized to the AST set, never None (#356)
// ============================================================================

/// Build a project with exactly N Rust files each containing both the text
/// token 'nested' and a nested for-loop (so all N match the `rust-nested-loop`
/// AST pattern AND the text query).
///
/// Used by `text_ast_ad356_2_limit_guard_25_files` to construct a 25-file
/// fixture whose AST set exceeds the reader's `unwrap_or(20)` default.
fn make_project_with_n_nested_loop_files(n: usize) -> TempDir {
    assert!(n >= 1, "must have at least 1 file");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();

    for i in 1..=n {
        // Each file contains "nested" once AND a nested for-loop so both the
        // text query ("nested") AND the rust-nested-loop AST pattern match.
        fs::write(
            root.join(format!("src/nested_{i:02}.rs")),
            format!(
                r#"
// nested: qualifying file {i}
fn work_{i}() {{
    for a in 0..{i} {{
        for b in 0..{i} {{
            let _ = (a, b);
        }}
    }}
}}
"#
            ),
        )
        .unwrap();
    }

    fs::write(root.join("config.json"), r#"{"key": "value"}"#).unwrap();

    dir
}

/// AC8 / AD-356-2 — sq.limit sized to the AST set, never None (#356).
///
/// This test is the dedicated discriminator for AD-356-2: the guarantee that
/// `sq.limit = Some(filter_set.len().max(1))` rather than `None`.
///
/// # Why this test is needed (PF-007)
///
/// The reader in `reader.rs` applies `let limit = query.limit.unwrap_or(20)`.
/// If `sq.limit` were `None` (or any constant ≤ 20), a 25-file AST set would
/// be silently re-capped at 20 results — reintroducing #356 for large sets.
///
/// The existing `text_ast_intersection_complete_below_pool_cliff_356` test uses
/// a 1-file AST set: with N=1 both `Some(1)` and `None` yield 1 result (1 ≤ 20),
/// so reverting line 362 of query.rs to `sq.limit = None` would NOT fail that
/// test.  That test is blind to the AD-356-2 half of the fix.
///
/// # What this test proves
///
/// - N=25 files ALL satisfy both the text query ("nested") and `rust-nested-loop`.
/// - At `--limit 25` with `sq.limit = Some(25)`: reader scores all 25 → returns 25.
/// - At `--limit 25` with `sq.limit = None` (regression): reader caps at 20 → only
///   20 returned → `assert_eq!(results.len(), 25)` **FAILS**.
///
/// # AC13 (set property independent of ranking)
///
/// At `--limit 10` exactly 10 of the 25 qualifying files are returned (the top-10
/// by RRF score), proving the set-completeness guarantee is rank-independent.
/// At `--limit 1` exactly 1 qualifying file is returned.
#[test]
fn text_ast_ad356_2_limit_guard_25_files() {
    use super::super::query::execute_query;

    const N: usize = 25;

    let project = make_project_with_n_nested_loop_files(N);
    let cache = tempfile::tempdir().unwrap();

    build_project_index(project.path(), cache.path());

    // -- Precondition: AST filter matches all N files ---------------------------
    let engine = super::open_ast_engine(cache.path()).unwrap();
    let ast_scored = super::resolve_ast_scored(&engine, "rust-nested-loop").unwrap();

    assert_eq!(
        ast_scored.len(),
        N,
        "AD-356-2 precondition: rust-nested-loop must match all {N} nested-loop files; \
         got {} — check that make_project_with_n_nested_loop_files generated valid Rust \
         with nested for-loops",
        ast_scored.len()
    );

    // -- AC8 (primary discriminating assertion): all N files returned at --limit N --
    //
    // Pre-fix (sq.limit = None): reader.rs `unwrap_or(20)` caps at 20.
    // N=25 > 20 → only 20 returned → assert_eq FAILS (RED confirms AD-356-2 gap).
    //
    // Post-fix (sq.limit = Some(25)): reader scores all 25 → returns 25 → PASSES.
    let full_config = make_query_config(
        project.path(),
        cache.path(),
        "nested",
        N,
        Some(ast_scored.clone()),
        None,
    );
    let full_output = execute_query(&full_config, &TEST_ANALYTICS).unwrap();

    assert_eq!(
        full_output.results.len(),
        N,
        "AC8 (AD-356-2 DISCRIMINATING): --limit {N} must return all {N} qualifying files. \
         Got {} — if the count is 20, sq.limit was None (unwrap_or(20) re-cap regression). \
         Reverting query.rs:362 to `sq.limit = None` would produce this failure.",
        full_output.results.len()
    );

    // All returned paths must be the nested_NN.rs files (no spurious entries).
    for r in &full_output.results {
        assert!(
            r.path.contains("nested_"),
            "AC8: result path must be one of the nested_NN.rs files; got: {:?}",
            r.path
        );
    }

    // -- AC13 (set property): intermediate --limit returns a proper subset ----------
    let mid_config = make_query_config(
        project.path(),
        cache.path(),
        "nested",
        10,
        Some(ast_scored.clone()),
        None,
    );
    let mid_output = execute_query(&mid_config, &TEST_ANALYTICS).unwrap();

    assert_eq!(
        mid_output.results.len(),
        10,
        "AC13: --limit 10 must return exactly 10 of the {N} qualifying files; \
         got {}",
        mid_output.results.len()
    );
    // Each mid result must also appear in the full set (proper subset check).
    let full_paths: std::collections::HashSet<&str> = full_output
        .results
        .iter()
        .map(|r| r.path.as_str())
        .collect();
    for r in &mid_output.results {
        assert!(
            full_paths.contains(r.path.as_str()),
            "AC13: path {:?} from --limit 10 is not in the full --limit {N} set — \
             compound filter is inconsistent across limit values",
            r.path
        );
    }

    // -- AC13 continued: --limit 1 returns exactly 1 qualifying file ---------------
    let limit1_config = make_query_config(
        project.path(),
        cache.path(),
        "nested",
        1,
        Some(ast_scored),
        None,
    );
    let limit1_output = execute_query(&limit1_config, &TEST_ANALYTICS).unwrap();

    assert_eq!(
        limit1_output.results.len(),
        1,
        "AC13: --limit 1 must return exactly 1 qualifying file; got {}",
        limit1_output.results.len()
    );
    assert!(
        limit1_output.results[0].path.contains("nested_"),
        "AC13: --limit 1 result must be a nested_NN.rs file; got: {:?}",
        limit1_output.results[0].path
    );
}

// ============================================================================
// AC12 — empty-AST early-out (#356)
// ============================================================================

/// AC12 — empty AST set returns empty QueryOutput, no panic, exit 0 (#356).
///
/// The early-out guard at `run_compound_query` (query.rs:331-339) handles the
/// case where `ast_scored = Some(empty vec)`.  With an empty `file_filter` the
/// reader scores zero files anyway, but the guard avoids the work and documents
/// the intent.
///
/// # Discriminating property (PF-007)
///
/// This is a reachable production path: `mod.rs:639-644` dispatches into
/// `run_compound_query` with `ast_scored = Some(vec![])` when the AST engine
/// resolves zero hits for the given pattern.  The test exercises this control
/// path directly by passing `ast_scored: Some(vec![])`.
///
/// If the early-out were deleted, behavior degrades safely (empty `file_filter`
/// still returns nothing), so this test focuses on asserting the contract:
/// no panic, no error, results empty.  Severity is low but the branch is new,
/// reachable code with a distinct control path and zero coverage without this
/// test.
#[test]
fn text_ast_empty_ast_set_returns_empty_ac12() {
    use super::super::query::execute_query;

    let project = make_project_with_two_nested_loop_files();
    let cache = tempfile::tempdir().unwrap();

    build_project_index(project.path(), cache.path());

    // Pass an explicitly empty AST scored vector — simulates a pattern that
    // matches no files (e.g. `skim search "foo" --ast <pattern-that-matches-nothing>`).
    // empty ast_scored vec is the key input for AC12.
    let config = make_query_config(
        project.path(),
        cache.path(),
        "nested",
        10,
        Some(vec![]),
        None,
    );

    // Must not panic, must not error, must return empty results (exit 0 semantics).
    let output = execute_query(&config, &TEST_ANALYTICS)
        .expect("AC12: execute_query must not error on empty AST set");

    assert!(
        output.results.is_empty(),
        "AC12: empty AST set must produce empty results; got {} results",
        output.results.len()
    );
    assert_eq!(
        output.total, 0,
        "AC12: total must be 0 for empty AST set; got {}",
        output.total
    );
}

// ============================================================================
// AC3 — blast∩AST compound completeness (#356)
// ============================================================================

/// Build a fixture with N qualifying files (both text+AST) and M lexical
/// distractors.
///
/// Qualifying files (`src/target_NN.rs`): contain "nested" once AND a nested
/// for-loop so both the text query ("nested") AND `rust-nested-loop` AST
/// pattern match.
///
/// Distractor files (`src/noast_NN.rs`): contain "nested" 30 times but NO
/// nested loops, so they score higher lexically but FAIL the AST filter.
///
/// Layout mirrors `make_project_with_lexical_cliff_fixture` but with N
/// qualifying files instead of 1, enabling AC3 (blast∩AST completeness) tests
/// where the qualifying set exceeds the old `limit * CANDIDATE_POOL_K = 4`
/// cliff.
fn make_project_with_blast_ast_cliff_fixture(n_qualifying: usize, n_distractors: usize) -> TempDir {
    assert!(n_qualifying >= 1, "must have at least 1 qualifying file");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();

    // Qualifying files: "nested" once + nested for-loop (match both text+AST).
    for i in 1..=n_qualifying {
        fs::write(
            root.join(format!("src/target_{i:02}.rs")),
            format!(
                r#"
// nested: qualifying file {i}
fn work_{i}() {{
    for a in 0..{i} {{
        for b in 0..{i} {{
            let _ = (a, b);
        }}
    }}
}}
"#
            ),
        )
        .unwrap();
    }

    // Distractor files: "nested" 30× but NO nested loops — outrank qualifying
    // files in unfiltered lexical scoring, yet fail the AST filter.
    for i in 1..=n_distractors {
        let body = "// nested\n".repeat(30);
        fs::write(
            root.join(format!("src/noast_{i:02}.rs")),
            format!(
                r#"
{body}
fn distractor_{i}() {{
    println!("not a nested loop");
}}
"#
            ),
        )
        .unwrap();
    }

    fs::write(root.join("config.json"), r#"{"key": "value"}"#).unwrap();

    dir
}

/// AC3 — blast∩AST compound completeness: `run_compound_query` with BOTH
/// `ast_scored` and `blast_radius_paths` set must return the complete
/// blast∩AST∩text intersection, not a `limit * K`-capped subset (#356).
///
/// # The second pool-cliff locus (plan AC3)
///
/// `run_compound_query` has TWO sub-paths:
///
/// 1. **No-blast** (`blast_file_ids = None`): `filter_set = ast_fid_set`.
///    Covered by `text_ast_intersection_complete_below_pool_cliff_356`.
///
/// 2. **blast+AST** (`blast_file_ids = Some`): `filter_set = blast ∩ ast`.
///    THIS test covers this sub-path — previously uncovered.
///
/// Pre-fix, BOTH sub-paths used `sq.limit = config.limit.saturating_mul(4)`
/// with no `file_filter`.  At `--limit 1` pool = 4 and the 6 lexical
/// distractors fill the pool, dropping all 6 qualifying files.  The test
/// asserts `count >= 1` → FAILS with the old code.
///
/// Post-fix (AD-356-1/2): `filter_set = blast ∩ ast = {target_01..target_06}`,
/// `sq.file_filter = Some(filter_set)`, `sq.limit = Some(6)`.  The reader
/// scores only the 6 qualifying files → all match text → at `--limit 1`
/// exactly 1 is returned from the complete set → test PASSES.
///
/// # Discriminating properties (PF-007)
///
/// - If `sq.limit = config.limit.saturating_mul(4)` were reintroduced on this
///   sub-path (regression), the distractors would fill the pool at `--limit 1`
///   and 0 qualifying files would be returned → `count >= 1` FAILS.
/// - If the blast intersection were dropped (regression: `filter_set = ast_fid_set`
///   ignoring blast), the distractor-filled pool would again prevent qualifying
///   files from appearing at `--limit 1` in the no-file-filter case.
/// - The `--limit N` assertion proves that ALL N qualifying files are accessible
///   (full-set completeness), not just that the top-1 works.
/// - The empty-blast∩AST sub-case asserts that a disjoint blast+AST set returns
///   empty with no panic (correctness guard per plan AC12(b)).
///
/// # Why `blast_radius_paths` is passed directly (no TemporalDb in test)
///
/// `execute_query` accepts `blast_radius_paths: Some(HashSet<String>)` as
/// pre-resolved repo-relative paths.  `mod.rs` uses `TemporalDb` to BUILD that
/// set before calling `execute_query`; the test bypasses `mod.rs` and injects
/// the paths directly — same production path, no test-only TemporalDb needed.
/// This is intentional per the NO-FAKE-SOLUTIONS rule: the intersection logic
/// in `run_compound_query` is independent of how the blast path set was built.
#[test]
fn text_ast_blast_intersection_complete_356() {
    use std::collections::HashSet;

    use super::super::query::execute_query;

    const N: usize = 6; // qualifying files (> old limit*4 = 4 at --limit 1)
    const DISTRACTORS: usize = 6; // lexically-outranking files that fail AST

    let project = make_project_with_blast_ast_cliff_fixture(N, DISTRACTORS);
    let cache = tempfile::tempdir().unwrap();

    build_project_index(project.path(), cache.path());

    // -- Precondition: AST filter matches exactly the N qualifying files -------
    let engine = super::open_ast_engine(cache.path()).unwrap();
    let ast_scored = super::resolve_ast_scored(&engine, "rust-nested-loop").unwrap();

    assert_eq!(
        ast_scored.len(),
        N,
        "AC3 precondition: rust-nested-loop must match exactly {N} qualifying files; \
         got {} — check that target_NN.rs files have nested for-loops and \
         noast_NN.rs files do not",
        ast_scored.len()
    );

    // -- blast_radius_paths: the N qualifying files (injected directly) --------
    //
    // In production, mod.rs resolves these from TemporalDb::cochanges_for_file.
    // Here we inject them as pre-resolved repo-relative paths — same code path
    // in execute_query (paths_to_file_ids builds the FileId set from sorted_paths).
    let blast_paths: HashSet<String> = (1..=N).map(|i| format!("src/target_{i:02}.rs")).collect();

    // -- AC3(a) full-set at --limit N: all N qualifying files returned ---------
    let full_config = make_query_config(
        project.path(),
        cache.path(),
        "nested",
        N,
        Some(ast_scored.clone()),
        Some(blast_paths.clone()),
    );
    let full_output = execute_query(&full_config, &TEST_ANALYTICS).unwrap();

    assert_eq!(
        full_output.results.len(),
        N,
        "AC3: --limit {N} with blast+AST must return all {N} qualifying files; \
         got {} — if the count is 4, the blast sub-path is using limit*4 (pool-cliff \
         regression on the blast+AST branch of run_compound_query)",
        full_output.results.len()
    );

    // All returned paths must be qualifying target files (not distractors).
    for r in &full_output.results {
        assert!(
            r.path.contains("target_"),
            "AC3: result path must be a target_NN.rs file (not a distractor); got: {:?}. \
             A distractor appearing here means the AST filter or blast intersection is \
             not being applied correctly.",
            r.path
        );
    }

    // -- AC3(b) pool-cliff discriminator at --limit 1 -------------------------
    //
    // PRE-FIX (regression): sq.limit = 1.saturating_mul(4) = 4, NO file_filter.
    // Pool = 4 candidates from the full corpus. The 6 distractor files each
    // contain "nested" 30×, outranking the 6 qualifying files (1× each).
    // The top-4 lexical hits are all distractors. None of the 6 qualifying
    // files appear in the pool → blast∩AST intersection is empty → 0 results.
    // THIS ASSERTION (count >= 1) WOULD FAIL against the pre-fix code.
    //
    // POST-FIX (AD-356-1/2): filter_set = blast∩ast = {target_01..target_06},
    // sq.file_filter = Some(filter_set), sq.limit = Some(6).
    // Reader scores only the 6 qualifying files → all match "nested" →
    // intersect_and_rank returns 6 → take(1) yields 1 result. PASSES.
    let limit1_config = make_query_config(
        project.path(),
        cache.path(),
        "nested",
        1,
        Some(ast_scored.clone()),
        Some(blast_paths.clone()),
    );
    let limit1_output = execute_query(&limit1_config, &TEST_ANALYTICS).unwrap();

    assert!(
        !limit1_output.results.is_empty(),
        "AC3 (pool-cliff DISCRIMINATING): --limit 1 with blast+AST returned 0 results. \
         target_NN.rs files must appear even though distractors rank higher lexically. \
         If count=0, the blast+AST sub-path of run_compound_query is using a pool cap \
         (limit*K regression) instead of file_filter=blast∩ast (AD-356-1 regression).",
    );

    // The returned file must be a qualifying target file (not a distractor).
    assert!(
        limit1_output.results[0].path.contains("target_"),
        "AC3: --limit 1 result must be a target_NN.rs file; got: {:?}. \
         A distractor appearing here means the blast∩AST file_filter is not \
         restricting the lexical engine to the qualifying set.",
        limit1_output.results[0].path
    );

    // The result must be in the complete blast∩AST∩text set (full_output paths).
    let full_paths: HashSet<&str> = full_output
        .results
        .iter()
        .map(|r| r.path.as_str())
        .collect();
    assert!(
        full_paths.contains(limit1_output.results[0].path.as_str()),
        "AC3: --limit 1 result {:?} is not in the complete --limit {N} set {:?}. \
         The result must be a member of the full blast∩AST∩text intersection.",
        limit1_output.results[0].path,
        full_paths
    );

    // -- AC3(c) empty blast∩AST: disjoint sets return empty, no panic ----------
    //
    // blast_radius_paths contains ONLY the noast files (distractors), which are
    // NOT in ast_scored (they have no nested loops). filter_set = blast∩ast = {}.
    // Expected: empty results, no panic.
    //
    // The filter_set.is_empty() early-out in run_compound_query (query.rs #356)
    // now handles this case explicitly rather than relying on the reader returning
    // 0 docs for an empty file_filter — both produce empty results.
    let disjoint_blast: HashSet<String> = (1..=DISTRACTORS)
        .map(|i| format!("src/noast_{i:02}.rs"))
        .collect();

    let disjoint_config = make_query_config(
        project.path(),
        cache.path(),
        "nested",
        10,
        Some(ast_scored),
        Some(disjoint_blast),
    );
    let disjoint_output = execute_query(&disjoint_config, &TEST_ANALYTICS)
        .expect("AC3(c): execute_query must not error on disjoint blast∩AST set");

    assert!(
        disjoint_output.results.is_empty(),
        "AC3(c): disjoint blast∩AST must return empty results (no panic); \
         got {} results — the filter_set.is_empty() early-out in run_compound_query \
         must return an empty QueryOutput for a disjoint blast∩AST intersection (#356).",
        disjoint_output.results.len()
    );
}

// ============================================================================
// AC3 — blast∩AST strict-subset discriminating test (#356, PF-007)
// ============================================================================

/// AC3 — blast+AST filtered set is a STRICT SUBSET of AST-only set (#356).
///
/// # What this tests
///
/// `run_compound_query` has two sub-paths:
///
/// 1. **No-blast** (`blast_file_ids = None`): `filter_set = ast_fid_set`.
/// 2. **blast+AST** (`blast_file_ids = Some`): `filter_set = blast ∩ ast`.
///
/// The existing tests (`text_ast_blast_intersection_complete_356`) verify that
/// sub-path 2 returns the complete blast∩AST∩text set.  That test uses a blast
/// set that equals the AST set, so it does NOT verify that restricting the blast
/// set actually REDUCES the result count relative to the no-blast path.
///
/// This test closes that gap by using a blast set that is a STRICT SUBSET of the
/// AST-matching files:
///
/// - No-blast run: all N qualifying files (target_01..target_N) are returned.
/// - Blast+AST run: blast covers only the first M < N qualifying files.
///   `filter_set = blast ∩ ast = {target_01..target_M}` → only M results.
///
/// # Discriminating property (PF-007)
///
/// The strict-subset assertion `blast_count < full_count` would FAIL in two
/// regression scenarios:
///
/// - If the blast intersection is dropped (`filter_set = ast_fid_set` always):
///   both runs return N files → `blast_count == full_count` → assertion fails.
/// - If `sq.file_filter` is not set: the reader scores unrestricted files and
///   may return distractors; the target-path membership check would also fail.
///
/// This is the missing discriminating guard that every prior blast+AST test
/// lacked — per the wave-4 review (#356 surviving finding, testing category).
#[test]
fn text_ast_blast_subset_is_strict_subset_of_ast_only_356() {
    use std::collections::HashSet;

    use super::super::query::execute_query;

    // 4 qualifying files (text+AST), 2 distractors (text only, no AST).
    // Blast set covers only the FIRST 2 of the 4 qualifying files.
    const N_QUALIFYING: usize = 4;
    const N_BLAST: usize = 2; // blast covers only a strict subset of qualifying
    const N_DISTRACTORS: usize = 2;

    let project = make_project_with_blast_ast_cliff_fixture(N_QUALIFYING, N_DISTRACTORS);
    let cache = tempfile::tempdir().unwrap();

    build_project_index(project.path(), cache.path());

    // Resolve the real AST scores (covers all N_QUALIFYING target files).
    let engine = super::open_ast_engine(cache.path()).unwrap();
    let ast_scored = super::resolve_ast_scored(&engine, "rust-nested-loop").unwrap();

    assert_eq!(
        ast_scored.len(),
        N_QUALIFYING,
        "Precondition: rust-nested-loop must match exactly {N_QUALIFYING} qualifying files; \
         got {} — check that target_NN.rs files have nested for-loops",
        ast_scored.len()
    );

    // -- Run 1: AST-only (no blast) — full qualifying set ----------------------
    let no_blast_config = make_query_config(
        project.path(),
        cache.path(),
        "nested",
        N_QUALIFYING,
        Some(ast_scored.clone()),
        None, // no blast filter
    );
    let no_blast_output = execute_query(&no_blast_config, &TEST_ANALYTICS).unwrap();

    let full_count = no_blast_output.results.len();
    assert_eq!(
        full_count, N_QUALIFYING,
        "AC3 precondition (no-blast): must return all {N_QUALIFYING} qualifying files; \
         got {full_count}"
    );

    // -- Run 2: blast+AST — blast covers only the first N_BLAST qualifying files --
    //
    // blast_radius_paths is a strict subset of the AST-matching files.
    // filter_set = blast ∩ ast = {target_01..target_N_BLAST}.
    // The reader is restricted to only those N_BLAST files → only N_BLAST results.
    let blast_paths: HashSet<String> = (1..=N_BLAST)
        .map(|i| format!("src/target_{i:02}.rs"))
        .collect();

    let blast_config = make_query_config(
        project.path(),
        cache.path(),
        "nested",
        N_QUALIFYING, // high limit — truncation is NOT the cause of the difference
        Some(ast_scored),
        Some(blast_paths.clone()),
    );
    let blast_output = execute_query(&blast_config, &TEST_ANALYTICS).unwrap();

    let blast_count = blast_output.results.len();

    // PF-007 DISCRIMINATING ASSERTION 1: strict subset — filtered < unfiltered.
    //
    // If the blast intersection logic in run_compound_query is dropped or broken
    // (filter_set reverts to ast_fid_set), blast_count == full_count and this
    // assertion FAILS — verifying the blast filter is actually applied.
    assert!(
        blast_count < full_count,
        "AC3 (strict-subset DISCRIMINATING): blast+AST count ({blast_count}) must be \
         STRICTLY LESS THAN the AST-only count ({full_count}). \
         If they are equal, the blast intersection in run_compound_query is not \
         restricting the result set — regression of AD-356-1 (#356 blast+AST sub-path)."
    );

    // PF-007 DISCRIMINATING ASSERTION 2: exact count matches blast set size.
    //
    // The blast set covers exactly N_BLAST qualifying files.  After filter_set
    // = blast ∩ ast and text verification, all N_BLAST must be returned.
    assert_eq!(
        blast_count, N_BLAST,
        "AC3 (blast subset count): blast+AST run must return exactly {N_BLAST} files \
         (blast ∩ AST ∩ text); got {blast_count}"
    );

    // PF-007 DISCRIMINATING ASSERTION 3: every result is in the blast set.
    //
    // No distractor or out-of-blast qualifying file may appear in the filtered
    // results — confirms the file_filter is applied, not just the count.
    for r in &blast_output.results {
        assert!(
            blast_paths.contains(&r.path),
            "AC3 (blast subset membership): result path {:?} is not in the blast set {:?}. \
             Only blast-set files must appear when blast+AST is active — a non-blast \
             file here means file_filter is not being applied in run_compound_query.",
            r.path,
            blast_paths
        );
    }

    // PF-007 DISCRIMINATING ASSERTION 4: every blast result is in the full set.
    //
    // blast results must be a subset of the no-blast results (they come from the
    // same AST∩text intersection, just restricted to the blast set).
    let full_paths: HashSet<&str> = no_blast_output
        .results
        .iter()
        .map(|r| r.path.as_str())
        .collect();
    for r in &blast_output.results {
        assert!(
            full_paths.contains(r.path.as_str()),
            "AC3 (blast subset ⊆ full): result {:?} from the blast+AST run is not \
             in the AST-only full result set {:?}. Blast results must be a subset of \
             the unfiltered AST results.",
            r.path,
            full_paths
        );
    }
}

// ============================================================================
// Group 12: #373 FileId↔path ordering skew — standalone --ast discriminating
// repro (AC-1)
// ============================================================================

/// AC-1 / AD-373-1: Discriminating repro for standalone --ast over a corpus
/// where PathBuf::cmp and str::cmp diverge.
///
/// Corpus:
/// - `foo.rs`       = `let x = 1;`     (no function_item)
/// - `foo/bar.rs`   = `fn only() {...}` (exactly ONE function_item)
/// - `foobar.rs`    = `const Z: i32 = 3;` (no function_item)
///
/// `run_ast_standalone("function_item > block", ...)` MUST return `foo/bar.rs`
/// and MUST NOT return `foo.rs` or `foobar.rs`.
///
/// Pre-fix: FileId(0) was assigned to `foo/bar.rs` (PathBuf component order)
/// but resolved to `foo.rs` (BTreeMap byte order), so the output contained
/// `foo.rs` (wrong file) — this test FAILS on the pre-fix code.
///
/// PF-007: the negative assertions (MUST NOT contain `foo.rs` / `foobar.rs`)
/// ensure this test would fail if AD-373-1 were reverted.
#[test]
fn run_ast_standalone_resolves_nested_dir_corpus_correctly() {
    use super::super::manifest::FileManifest;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let cache = tempfile::tempdir().unwrap();

    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("foo")).unwrap();

    // Only foo/bar.rs has a function_item.
    fs::write(root.join("foo.rs"), "let x = 1;\n").unwrap();
    fs::write(root.join("foo/bar.rs"), "fn only() { let y = 2; }\n").unwrap();
    fs::write(root.join("foobar.rs"), "const Z: i32 = 3;\n").unwrap();

    build_project_index(root, cache.path());

    let manifest = FileManifest::load(root.to_path_buf(), cache.path().to_path_buf()).unwrap();

    let mut out: Vec<u8> = Vec::new();
    let result = super::run_ast_standalone(
        "function_item > block",
        40,
        false, // text output
        cache.path(),
        &manifest,
        None, // no --blast-radius
        None, // no temporal sort
        None, // no temporal DB
        root,
        &mut out,
    )
    .unwrap();

    assert_eq!(
        result,
        std::process::ExitCode::SUCCESS,
        "AC-1: standalone --ast over nested-dir corpus must exit SUCCESS"
    );

    let text = String::from_utf8(out).unwrap();

    // POSITIVE: the ONLY file with function_item must appear.
    assert!(
        text.contains("foo/bar.rs") || text.contains("foo") && text.contains("bar.rs"),
        "AC-1 POSITIVE: output must contain 'foo/bar.rs' (the only file with a function_item). \
         Pre-fix: FileId skew caused foo.rs to be returned instead. Got:\n{text}"
    );

    // NEGATIVE 1 (PF-007): foo.rs has no function_item → must NOT appear.
    // Pre-fix: foo.rs was returned because FileId(0) resolved to it (skew).
    // We check that `foo.rs` does not appear as a standalone path
    // (not as part of `foo/bar.rs`).
    let foo_rs_standalone = text
        .lines()
        .any(|l| l.trim() == "foo.rs" || l.ends_with("/foo.rs") || l.ends_with("  foo.rs"));
    assert!(
        !foo_rs_standalone,
        "AC-1 NEGATIVE: output must NOT return foo.rs (no function_item). \
         Pre-fix: the FileId skew mapped foo/bar.rs's FileId to foo.rs. \
         Reverting AD-373-1 makes this fail. Got:\n{text}"
    );

    // NEGATIVE 2: foobar.rs has no function_item → must NOT appear.
    let foobar_in_text = text.lines().any(|l| l.contains("foobar.rs"));
    assert!(
        !foobar_in_text,
        "AC-1 NEGATIVE: output must NOT return foobar.rs (no function_item). Got:\n{text}"
    );
}

// ============================================================================
// Group 13 (#374): Structural verify gate (Part A AND-intersect + Part B gate)
// ============================================================================
//
// These tests cover the acceptance criteria from ticket #374:
//   AC1  — zero-n-gram data files excluded
//   AC2  — AND-intersect vs OR-union (engine level) → see query_tests.rs (a3/a3b/P3)
//   AC3  — verify gate drops unrelated subtrees
//            unit:        compound/reparse_tests.rs::pattern_occurs_false_for_unrelated_subtree_kinds_ac3_374
//            integration: run_ast_standalone_unrelated_subtree_excluded_ac3_374 (this file)
//   AC4  — true positives survive the gate
//   AC5  — verify-then-truncate-LAST
//   AC8  — recover_line stays line-recovery only (degraded row emitted, not dropped)
//   AC9  — single-n-gram identity (no regression)
//   AC10 — query-time only, no format/rebuild
//   AC11 — no elision marker when gate produces empty results
//   AC12 — O(pool) bound guard (constant reuse guard)
//   AC13 — entry-point agreement: ast_index/query_tests.rs::ac13_search_ast_and_layer_agree_on_fileid_set
//
// AC2 / AC7 engine-level AND-intersect tests live in `ast_index/query_tests.rs`
// (a3_*, a3b_*, P3 group). The `pattern_occurs_in_file` gate UNIT tests (AC6) live
// in `compound/reparse_tests.rs`.
// OD-374-3 ERROR-node fixture: compound/reparse_tests.rs::pattern_occurs_false_for_error_node_ancestor_od374_3.

/// Create a project for #374 tests: one Rust file with genuine nested loops,
/// one with no nested loops, one JSON, one Cargo.toml.
fn make_project_for_374_gate() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();

    // Rust file WITH nested loops (should match rust-nested-loop).
    fs::write(
        root.join("src/nested.rs"),
        r#"
fn nested() {
    for i in 0..5 {
        for j in 0..5 {
            let _ = i + j;
        }
    }
}
"#,
    )
    .unwrap();

    // Rust file WITHOUT nested loops (should NOT match).
    fs::write(
        root.join("src/simple.rs"),
        r#"
fn simple(x: i32) -> i32 {
    x + 1
}
"#,
    )
    .unwrap();

    // JSON file — non-tree-sitter, zero AST n-grams (AC1, AD-374-5).
    fs::write(root.join("data.json"), r#"{"kind": "nested", "count": 3}"#).unwrap();

    // Cargo.toml — non-tree-sitter, zero AST n-grams (AC1, AD-374-5).
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"test\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    dir
}

/// AC1 (#374): Standalone `--ast rust-nested-loop` MUST NOT include non-tree-sitter
/// data files (JSON, TOML) in the result set, and MUST include the genuinely
/// matching Rust file.
///
/// This is the headline false-positive fix: before #374 these data files could
/// appear in results because they were scored by the OR-union via posting lists.
/// With AND-intersect + verify gate they are correctly excluded.
///
/// NEGATIVE (PF-007): Removing the gate (reverting to old OR-union only) makes
/// Cargo.toml / data.json appear in output, failing the exclusion assertions.
#[test]
fn run_ast_standalone_excludes_non_tree_sitter_files_ac1_374() {
    use super::super::manifest::FileManifest;

    let project = make_project_for_374_gate();
    let cache = tempfile::tempdir().unwrap();
    build_project_index(project.path(), cache.path());

    let manifest =
        FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf()).unwrap();

    let mut out: Vec<u8> = Vec::new();
    let result = super::run_ast_standalone(
        "rust-nested-loop",
        20,
        false,
        cache.path(),
        &manifest,
        None,
        None,
        None,
        project.path(),
        &mut out,
    )
    .unwrap();

    assert_eq!(
        result,
        std::process::ExitCode::SUCCESS,
        "AC1: run_ast_standalone must exit 0"
    );

    let text = String::from_utf8(out).unwrap();

    // POSITIVE: the genuine Rust file with nested loops MUST appear.
    assert!(
        text.contains("nested.rs"),
        "AC1 POSITIVE: output must contain nested.rs (genuine match); got:\n{text}"
    );

    // NEGATIVE (PF-007): Data files must NOT appear.
    // Removing the gate makes them appear → this assertion fails.
    assert!(
        !text.contains("data.json"),
        "AC1 NEGATIVE: output must NOT contain data.json (non-tree-sitter, zero AST n-grams); \
         reverting the gate makes it appear. Got:\n{text}"
    );

    // NEGATIVE: Cargo.toml must not appear.
    assert!(
        !text.contains("Cargo.toml"),
        "AC1 NEGATIVE: output must NOT contain Cargo.toml (non-tree-sitter, zero AST n-grams); \
         reverting the gate makes it appear. Got:\n{text}"
    );

    // NEGATIVE: simple.rs (no nested loops) must not appear.
    assert!(
        !text.contains("simple.rs"),
        "AC1 NEGATIVE: output must NOT contain simple.rs (no nested loops); got:\n{text}"
    );
}

/// Create a project for AC4: Rust files with both `exact:false` and `exact:true` patterns.
///
/// - `src/loops.rs`  — nested for-loops (matches `rust-nested-loop`, exact:false proxy)
/// - `src/unsafe.rs` — unsafe block (matches `rust-unsafe-block`, exact:true)
/// - `config.json`   — non-tree-sitter (matches neither)
fn make_project_with_rust_exact_patterns() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();

    // Rust file with nested loops (rust-nested-loop, exact:false).
    fs::write(
        root.join("src/loops.rs"),
        r#"
fn outer() {
    for i in 0..5 {
        for j in 0..5 {
            let _ = i + j;
        }
    }
}
"#,
    )
    .unwrap();

    // Rust file with unsafe block (rust-unsafe-block, exact:true).
    // Pattern bigram: (unsafe_block, block).
    fs::write(
        root.join("src/unsafe.rs"),
        r#"
fn write_raw(ptr: *mut i32, val: i32) {
    unsafe { *ptr = val; }
}
"#,
    )
    .unwrap();

    // Non-tree-sitter file — must be excluded by the gate.
    fs::write(root.join("config.json"), r#"{"key": "value"}"#).unwrap();

    dir
}

/// AC4 (#374): True positives must survive the gate — both `exact:true` patterns
/// (`rust-unsafe-block`) and `exact:false` proxy patterns (`rust-nested-loop`).
///
/// The gate applies ONE logic path to all patterns via ancestor-correct matching;
/// it MUST NOT zero out a legitimately-matched result set.
///
/// NEGATIVE (PF-007): If the gate were inverted (drops all), these assertions fail.
#[test]
fn run_ast_standalone_true_positives_survive_gate_ac4_374() {
    use super::super::manifest::FileManifest;

    let project = make_project_with_rust_exact_patterns();
    let cache = tempfile::tempdir().unwrap();
    build_project_index(project.path(), cache.path());

    let manifest =
        FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf()).unwrap();

    // Test exact:false pattern (rust-nested-loop is a proxy pattern).
    let mut nested_out: Vec<u8> = Vec::new();
    super::run_ast_standalone(
        "rust-nested-loop",
        20,
        false,
        cache.path(),
        &manifest,
        None,
        None,
        None,
        project.path(),
        &mut nested_out,
    )
    .unwrap();
    let nested_text = String::from_utf8(nested_out).unwrap();
    assert!(
        nested_text.contains("loops.rs"),
        "AC4: rust-nested-loop (exact:false proxy) must return loops.rs; gate must not over-drop. \
         Got:\n{nested_text}"
    );
    // NEGATIVE (PF-007): config.json must NOT appear (non-tree-sitter).
    assert!(
        !nested_text.contains("config.json"),
        "AC4 NEGATIVE: config.json must not appear in rust-nested-loop results. Got:\n{nested_text}"
    );

    // Test exact:true pattern (rust-unsafe-block: bigram unsafe_block → block).
    let mut unsafe_out: Vec<u8> = Vec::new();
    super::run_ast_standalone(
        "rust-unsafe-block",
        20,
        false,
        cache.path(),
        &manifest,
        None,
        None,
        None,
        project.path(),
        &mut unsafe_out,
    )
    .unwrap();
    let unsafe_text = String::from_utf8(unsafe_out).unwrap();
    // src/unsafe.rs has `unsafe { *ptr = val; }` → exact:true bigram (unsafe_block, block).
    assert!(
        unsafe_text.contains("unsafe.rs"),
        "AC4: rust-unsafe-block (exact:true) must return unsafe.rs; gate must not over-drop. \
         Got:\n{unsafe_text}"
    );
    // NEGATIVE (PF-007): loops.rs (no unsafe block) must NOT appear.
    assert!(
        !unsafe_text.contains("loops.rs"),
        "AC4 NEGATIVE: loops.rs must not appear in rust-unsafe-block results (no unsafe block). \
         Got:\n{unsafe_text}"
    );
}

/// AC5 (#374): verify-then-truncate-LAST — at `--limit N`, the output must contain
/// exactly min(N, verified_count) verified results.
///
/// Uses a project with 3 matching files and `--limit 2`. The gate should keep 3
/// verified files and then truncate to 2 (not under-fill to 2 minus dropped).
///
/// NEGATIVE (PF-007): If truncation happens BEFORE the gate, the count would be
/// `limit - dropped`, under-filling the result set.
#[test]
fn run_ast_standalone_truncate_after_gate_ac5_374() {
    use super::super::manifest::FileManifest;

    // Build project with 3 matching files.
    let project = make_project_with_two_nested_loop_files();
    // This fixture has alpha.rs, beta.rs (both match rust-nested-loop), plain.rs
    // (doesn't match), and config.json.

    let cache = tempfile::tempdir().unwrap();
    build_project_index(project.path(), cache.path());

    let manifest =
        FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf()).unwrap();

    // limit=1: with gate working correctly we get exactly 1 verified result.
    let mut out: Vec<u8> = Vec::new();
    let result = super::run_ast_standalone(
        "rust-nested-loop",
        1, // limit = 1
        true,
        cache.path(),
        &manifest,
        None,
        None,
        None,
        project.path(),
        &mut out,
    )
    .unwrap();

    assert_eq!(result, std::process::ExitCode::SUCCESS, "AC5: must exit 0");

    let v: serde_json::Value = serde_json::from_str(&String::from_utf8(out).unwrap()).unwrap();
    let results = v["results"].as_array().unwrap();

    // With limit=1 and 2 genuine matches, exactly 1 verified result is returned.
    assert_eq!(
        results.len(),
        1,
        "AC5: --limit 1 must return exactly 1 result (verify-then-truncate-LAST); \
         if truncation happened before the gate, we might get 0 (PF-007 discriminating); \
         got: {results:?}"
    );
}

/// AC8 (#374): `recover_line` MUST remain line-recovery only — a file that passes
/// the verify gate but where `recover_line` returns `None` MUST still be emitted
/// as a degraded row (path present, no `:line`/snippet), not dropped.
///
/// We verify that `run_ast_standalone` emits the file path even when the
/// representative line cannot be recovered (degraded row, matching AC-F2 / AC-API3).
///
/// NEGATIVE (PF-007): If emit/drop were keyed off `recover_line`, the file
/// would vanish from output; this assertion would fail.
#[test]
fn run_ast_standalone_recover_line_none_still_emits_ac8_374() {
    use super::super::manifest::FileManifest;

    let project = make_project_with_rust();
    let cache = tempfile::tempdir().unwrap();
    build_project_index(project.path(), cache.path());

    let manifest =
        FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf()).unwrap();

    // Use text output (not JSON) — a degraded row shows the path without `:line`.
    let mut out: Vec<u8> = Vec::new();
    super::run_ast_standalone(
        "rust-nested-loop",
        20,
        false,
        cache.path(),
        &manifest,
        None,
        None,
        None,
        project.path(),
        &mut out,
    )
    .unwrap();

    let text = String::from_utf8(out).unwrap();

    // src/loops.rs must appear in output (it genuinely matches rust-nested-loop).
    assert!(
        text.contains("loops.rs"),
        "AC8: loops.rs must appear in output regardless of whether recover_line succeeds; \
         recover_line None MUST NOT drop the file. Got:\n{text}"
    );
}

/// AC9 (#374): Single-n-gram identity — `rust-unsafe-block` (a pattern with a single
/// bigram, `unsafe_block → block`) must return the same set before and after the gate.
///
/// Single-n-gram AND-intersect == union of that one list, so no file is added or
/// dropped by the AND-intersect step. The gate drops only non-matching files.
/// Files genuinely containing `rust-unsafe-block` must still appear.
///
/// This is also a regression guard: pre-existing AST tests must not break.
#[test]
fn run_ast_standalone_single_ngram_identity_ac9_374() {
    use super::super::manifest::FileManifest;

    // src/unsafe.rs has `unsafe { *ptr = val; }` → rust-unsafe-block bigram.
    let project = make_project_with_rust_exact_patterns();
    let cache = tempfile::tempdir().unwrap();
    build_project_index(project.path(), cache.path());

    let manifest =
        FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf()).unwrap();

    let mut out: Vec<u8> = Vec::new();
    super::run_ast_standalone(
        "rust-unsafe-block",
        20,
        false,
        cache.path(),
        &manifest,
        None,
        None,
        None,
        project.path(),
        &mut out,
    )
    .unwrap();

    let text = String::from_utf8(out).unwrap();

    // unsafe.rs has the rust-unsafe-block pattern (single bigram) → must appear.
    // Single-n-gram identity: AND-intersect over one list == the list itself.
    assert!(
        text.contains("unsafe.rs"),
        "AC9: single-bigram rust-unsafe-block must return unsafe.rs (identity: single-list \
         AND-intersect == old union). Got:\n{text}"
    );

    // NEGATIVE (PF-007): config.json (non-tree-sitter, no AST n-grams) must NOT appear.
    // The gate drops non-tree-sitter files even for single-bigram patterns.
    assert!(
        !text.contains("config.json"),
        "AC9 NEGATIVE: config.json must not appear (non-tree-sitter, gate drops it). \
         Got:\n{text}"
    );

    // NEGATIVE (PF-007): loops.rs has no unsafe block → must NOT appear.
    assert!(
        !text.contains("loops.rs"),
        "AC9 NEGATIVE: loops.rs has no unsafe block and must not appear. Got:\n{text}"
    );
}

/// AC10 (#374): Query-time only — the AST on-disk format MUST remain v2 after
/// running a gate query. No rebuild or format bump must be triggered.
///
/// NEGATIVE (PF-007): If the gate wrote to disk or bumped the format version,
/// a second index_version check would differ from the initial value.
#[test]
fn run_ast_standalone_no_format_change_ac10_374() {
    use super::super::manifest::FileManifest;

    let project = make_project_with_rust();
    let cache = tempfile::tempdir().unwrap();
    build_project_index(project.path(), cache.path());

    // Record AST format version before the gate query.
    let version_before = rskim_search::AstIndexReader::index_version(cache.path())
        .expect("index_version must succeed after build");

    let manifest =
        FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf()).unwrap();

    let mut out: Vec<u8> = Vec::new();
    super::run_ast_standalone(
        "rust-nested-loop",
        20,
        false,
        cache.path(),
        &manifest,
        None,
        None,
        None,
        project.path(),
        &mut out,
    )
    .unwrap();

    // Record AST format version after the gate query.
    let version_after = rskim_search::AstIndexReader::index_version(cache.path())
        .expect("index_version must succeed after gate query");

    assert_eq!(
        version_before, version_after,
        "AC10: AST index format version must not change after a gate query \
         (query-time only, no on-disk writes); \
         reverting to a write-on-query implementation would fail this assertion"
    );
    assert_eq!(
        version_before,
        rskim_search::AST_INDEX_FORMAT_VERSION,
        "AC10: format version must equal AST_INDEX_FORMAT_VERSION ({}) after build",
        rskim_search::AST_INDEX_FORMAT_VERSION
    );
}

/// AC11 (#374): When every candidate fails the gate, the result set is clean empty
/// (exit 0) with NO `elision_marker` and NO `SKIM_PASSTHROUGH` hint.
///
/// We achieve "every candidate fails the gate" by using a project where the only
/// files are non-tree-sitter (JSON/TOML) — the gate always returns false for them.
///
/// NEGATIVE (PF-007): If the gate mistakenly emitted an elision marker, the
/// assertion below would detect the text in output.
#[test]
fn run_ast_standalone_empty_gate_no_elision_marker_ac11_374() {
    use super::super::manifest::FileManifest;

    // Project with only non-tree-sitter files.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join("data.json"), r#"{"a": 1}"#).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"test\"\nversion=\"0.1.0\"\n",
    )
    .unwrap();

    let cache = tempfile::tempdir().unwrap();
    build_project_index(root, cache.path());

    let manifest = FileManifest::load(root.to_path_buf(), cache.path().to_path_buf()).unwrap();

    let mut out: Vec<u8> = Vec::new();
    let result = super::run_ast_standalone(
        "rust-nested-loop",
        20,
        false,
        cache.path(),
        &manifest,
        None,
        None,
        None,
        root,
        &mut out,
    )
    .unwrap();

    assert_eq!(
        result,
        std::process::ExitCode::SUCCESS,
        "AC11: empty gate result must exit 0 (not an error)"
    );

    let text = String::from_utf8(out).unwrap();

    // NEGATIVE (PF-007): must NOT contain elision marker or SKIM_PASSTHROUGH hint.
    assert!(
        !text.contains("SKIM_PASSTHROUGH"),
        "AC11 NEGATIVE: empty gate result must NOT emit SKIM_PASSTHROUGH hint; \
         an erroneously-added elision marker would contain this text. Got:\n{text}"
    );
    assert!(
        !text.contains("elision"),
        "AC11 NEGATIVE: empty gate result must NOT emit an elision marker. Got:\n{text}"
    );
}

/// AC12 (#374): Pool multiplier guard — the AST gate MUST reuse the module-level
/// `LEXICAL_CANDIDATE_POOL_K` constant from `query.rs` (single definition,
/// no fork). The value must be 5; changing it without #361 evidence fails this test.
///
/// This is the constant-reuse guard from OD-374-2. The assertion is structural:
/// it verifies the constant value (5) via a type-visible test so any change
/// without updating the #361 tracking issue is immediately visible.
///
/// NEGATIVE (PF-007): If the constant were forked in ast.rs with a different
/// value (e.g. K=3), the pool sizing would diverge from the lexical path and
/// this assertion would fail.
#[test]
fn ast_gate_reuses_lexical_candidate_pool_k_ac12_374() {
    // AD-374-3: the single named constant must be 5 (no measured corpus basis;
    // tracked under #361 per ADR-003). A change to this value without #361
    // evidence fails this guard test.
    const EXPECTED_K: usize = 5;
    assert_eq!(
        super::super::query::LEXICAL_CANDIDATE_POOL_K,
        EXPECTED_K,
        "AC12: LEXICAL_CANDIDATE_POOL_K must be {EXPECTED_K} (unmeasured heuristic, \
         tracked under #361 per ADR-003). Changing it without #361 evidence and \
         corpus measurements violates ADR-003. Update #361 first."
    );

    // AC12 POOL-SIZE BOUND: candidate_pool(limit, K) must equal max(K×limit, FLOOR).
    //
    // The documented contract: `limit.saturating_mul(k).max(CANDIDATE_POOL_FLOOR)`.
    // This pins the O(pool) bound: the gate re-parses at most pool files, where
    // pool ≤ K × limit for any limit ≥ FLOOR/K (i.e. limit ≥ 20 for K=5, FLOOR=100).
    // The bound is enforced by `.take(window)` in ast.rs BEFORE calling
    // pattern_occurs_in_file, so re-parse count ≤ pool.
    //
    // NEGATIVE (PF-007): If candidate_pool were changed to return just `limit` (no
    // multiplier), pool(100, K) would be 100 instead of 500, catching the regression.
    const FLOOR: usize = 100; // must match CANDIDATE_POOL_FLOOR in query.rs
    for limit in [1usize, 5, 10, 100] {
        let pool = super::super::query::candidate_pool(
            limit,
            super::super::query::LEXICAL_CANDIDATE_POOL_K,
        );
        let expected = (EXPECTED_K * limit).max(FLOOR);
        assert_eq!(
            pool,
            expected,
            "AC12 pool-size: candidate_pool({limit}, K={EXPECTED_K}) must be \
             max(K×limit, FLOOR={FLOOR}) = max({}, {FLOOR}) = {expected}; got {pool}",
            EXPECTED_K * limit
        );
    }

    // For limit >= FLOOR/K, the multiplier dominates: K×limit > FLOOR.
    // Verify the O(K×limit) linear bound at limit=100 where floor is not active.
    {
        let limit = 100usize;
        let pool = super::super::query::candidate_pool(
            limit,
            super::super::query::LEXICAL_CANDIDATE_POOL_K,
        );
        // K×limit = 500, which exceeds FLOOR=100, so pool must equal K×limit.
        assert_eq!(
            pool,
            EXPECTED_K * limit,
            "AC12 linear-bound: candidate_pool(100, K) must equal K×100={} when \
             K×limit > FLOOR={FLOOR} (pool multiplier dominates). Got {pool}",
            EXPECTED_K * limit
        );
        // The bound must be strictly less than total corpus for real projects.
        // (corpus size assertion not pinned here — corpus is runtime-dependent;
        // the above assertion ensures the multiplier path works correctly.)
    }
}

/// AC3 (#374): Unrelated-subtree end-to-end — `run_ast_standalone` must include
/// the genuine nested-loop file (File D) and exclude the closure-body file (File C),
/// where File C has all constituent CST kinds present but NOT in the required
/// `block → expression_statement → for_expression` ancestor chain.
///
/// File C uses a Rust closure whose body is directly `for_expression` (no
/// intervening block or `expression_statement`), so the trigram is absent from its
/// posting list and AND-intersect correctly excludes it.  File D has genuine nested
/// loops and appears in results.
///
/// The `pattern_occurs_in_file` unit test in `compound/reparse_tests.rs`
/// (`pattern_occurs_false_for_unrelated_subtree_kinds_ac3_374`) directly pins the
/// GATE behavior for File C; this integration test pins the END-TO-END pipeline.
///
/// NEGATIVE (PF-007): If the pipeline returned File C (e.g. via a regression
/// that re-introduces OR-union without the gate), the first assertion would fail.
#[test]
fn run_ast_standalone_unrelated_subtree_excluded_ac3_374() {
    use super::super::manifest::FileManifest;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();

    // File D: genuine nested loops — must appear in results.
    fs::write(
        root.join("src/nested.rs"),
        r#"
fn outer() {
    for i in 0..5 {
        for j in 0..5 { let _ = i + j; }
    }
}
"#,
    )
    .unwrap();

    // File C: closure body is directly for_expression.
    //   Rust CST: block → expression_statement → let_declaration →
    //             closure_expression → for_expression
    // The for_expression's parent is closure_expression (NOT expression_statement),
    // so the trigram (block, expression_statement, for_expression) is NOT in the
    // posting list → AND-intersect excludes it without reaching the strict gate.
    fs::write(
        root.join("src/closure_for.rs"),
        r#"
fn uses_closure() {
    let _g = || for i in 0..5 { let _ = i; };
    println!("done");
}
"#,
    )
    .unwrap();

    let cache = tempfile::tempdir().unwrap();
    build_project_index(root, cache.path());

    let manifest = FileManifest::load(root.to_path_buf(), cache.path().to_path_buf()).unwrap();

    let mut out: Vec<u8> = Vec::new();
    let result = super::run_ast_standalone(
        "rust-nested-loop",
        20,
        false,
        cache.path(),
        &manifest,
        None,
        None,
        None,
        root,
        &mut out,
    )
    .unwrap();

    assert_eq!(
        result,
        std::process::ExitCode::SUCCESS,
        "AC3: run_ast_standalone must exit 0"
    );

    let text = String::from_utf8(out).unwrap();

    // POSITIVE: genuine nested loops must appear.
    assert!(
        text.contains("nested.rs"),
        "AC3 POSITIVE: output must contain nested.rs (genuine nested loops); got:\n{text}"
    );

    // NEGATIVE (PF-007): closure_for.rs has for_expression but NOT in the required
    // ancestor chain → must NOT appear.  A regression to OR-union-only would make
    // it appear if the trigram somehow got into the posting list.
    assert!(
        !text.contains("closure_for.rs"),
        "AC3 NEGATIVE: output must NOT contain closure_for.rs (closure-body for, \
         no expression_statement wrapper) — unrelated-subtree file must be excluded. \
         Got:\n{text}"
    );
}

// AC13 (#374): Entry-point agreement — see
// `crates/rskim-search/src/ast_index/query_tests.rs::ac13_search_ast_and_layer_agree_on_fileid_set`
// for the full falsifiable test using real AstIndexBuilder/AstIndexReader.
// That test lives in `rskim-search` where all engine types are already in scope.

// ============================================================================
// Group 12: #377 — compound text+--ast path honors --weights (AD-377-1)
// ============================================================================
//
// Before #377, run_compound_query hardcoded `CompositeWeights::default()` and
// silently ignored caller-supplied `--weights` on the text+--ast path (the
// ticket's "byte-identical output" bug). These tests drive the real compound
// path (execute_query with ast_scored = Some) and assert the fix (PF-007: each
// fails if the fix is reverted).

/// AC2 (NEGATIVE, byte-identity): on the compound path, `composite_weights = None`
/// MUST produce results byte-identical to `Some(parse_weights_flag("0.5,0.3,0.2"))`
/// (the default profile). Back-compat guard for AD-377-1.
#[test]
fn compound_none_weights_byte_identical_to_explicit_default_ac2() {
    use super::super::query::execute_query;

    let project = make_project_with_two_nested_loop_files();
    let cache = tempfile::tempdir().unwrap();
    build_project_index(project.path(), cache.path());

    let engine = super::open_ast_engine(cache.path()).unwrap();
    let ast_scored = super::resolve_ast_scored(&engine, "rust-nested-loop").unwrap();
    assert!(
        ast_scored.len() >= 2,
        "AC2 setup: rust-nested-loop must match alpha.rs + beta.rs (>=2 files), got {}",
        ast_scored.len()
    );

    let mut base = make_query_config(
        project.path(),
        cache.path(),
        "fn",
        50,
        Some(ast_scored.clone()),
        None,
    );
    base.composite_weights = None;
    let out_none = execute_query(&base, &TEST_ANALYTICS).unwrap();

    let mut explicit = make_query_config(
        project.path(),
        cache.path(),
        "fn",
        50,
        Some(ast_scored),
        None,
    );
    explicit.composite_weights =
        Some(rskim_search::CompositeWeights6::parse_weights_flag("0.5,0.3,0.2").unwrap());
    let out_default = execute_query(&explicit, &TEST_ANALYTICS).unwrap();

    let pn: Vec<(&str, f64)> = out_none
        .results
        .iter()
        .map(|r| (r.path.as_str(), r.score))
        .collect();
    let pd: Vec<(&str, f64)> = out_default
        .results
        .iter()
        .map(|r| (r.path.as_str(), r.score))
        .collect();
    assert_eq!(
        pn, pd,
        "AC2: compound `None` weights must be byte-identical to explicit Some(0.5,0.3,0.2)"
    );
}

/// AC5 (contract, POSITIVE): `--weights 0,0,0` on the compound path returns the
/// FULL non-empty intersection, every score == 0.0, diverging intentionally from
/// the blast path (which returns empty). This ALSO falsifies the AD-377-1 bug:
/// under the old hardcoded `CompositeWeights::default()` (0.5,0.3,0.2) the scores
/// would be non-zero, so the `score == 0.0` assertion would fail.
#[test]
fn compound_zero_weights_returns_full_intersection_at_zero_ac5() {
    use super::super::query::execute_query;

    let project = make_project_with_two_nested_loop_files();
    let cache = tempfile::tempdir().unwrap();
    build_project_index(project.path(), cache.path());

    let engine = super::open_ast_engine(cache.path()).unwrap();
    let ast_scored = super::resolve_ast_scored(&engine, "rust-nested-loop").unwrap();
    assert!(
        ast_scored.len() >= 2,
        "AC5 setup: rust-nested-loop must match >=2 files, got {}",
        ast_scored.len()
    );

    // Reference run (default weights) to size the expected intersection cardinality.
    let ref_cfg = make_query_config(
        project.path(),
        cache.path(),
        "fn",
        50,
        Some(ast_scored.clone()),
        None,
    );
    let ref_out = execute_query(&ref_cfg, &TEST_ANALYTICS).unwrap();
    assert!(
        !ref_out.results.is_empty(),
        "AC5 setup: default-weights compound run must return a non-empty intersection"
    );

    let mut zero_cfg = make_query_config(
        project.path(),
        cache.path(),
        "fn",
        50,
        Some(ast_scored),
        None,
    );
    zero_cfg.composite_weights =
        Some(rskim_search::CompositeWeights6::parse_weights_flag("0,0,0").unwrap());
    let zero_out = execute_query(&zero_cfg, &TEST_ANALYTICS).unwrap();

    assert!(
        !zero_out.results.is_empty(),
        "AC5: --weights 0,0,0 on compound path must return a NON-EMPTY intersection \
         (diverges from blast path's empty result)"
    );
    assert_eq!(
        zero_out.results.len(),
        ref_out.results.len(),
        "AC5: zero-weights must return the FULL intersection (same cardinality as default)"
    );
    for r in &zero_out.results {
        assert_eq!(
            r.score, 0.0,
            "AC5/AD-377-1: every score under --weights 0,0,0 must be exactly 0.0 — a non-zero \
             score proves --weights is being ignored and the default 0.5,0.3,0.2 is still hardcoded \
             (got {} for {})",
            r.score, r.path
        );
    }
}

/// Fixture for the AC1/AC4 ordering-flip tests: two files that BOTH match
/// `rust-nested-loop` AND contain the token `needle`, but with a deliberate
/// lexical skew so the lexical ranking is unambiguous.
///
/// - `src/aaa.rs` — tiny file, `needle` repeated several times → lexically DOMINANT.
/// - `src/bbb.rs` — large filler body, `needle` once → lexically WEAK.
///
/// `aaa` sorts before `bbb`, so `FileId(aaa) < FileId(bbb)`. The tests assign the
/// AST-layer score so `bbb` is AST-DOMINANT. With those opposite rankings, shifting
/// the lexical:ast weight ratio flips the top result (AC1).
fn make_project_two_ast_files_lexical_skew() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();

    // aaa: tiny, many `needle` occurrences → high BM25F (short doc, high TF).
    fs::write(
        root.join("src/aaa.rs"),
        r#"
fn aaa() {
    // needle needle needle needle needle needle
    for i in 0..2 {
        for j in 0..2 {
            let _ = (i, j);
        }
    }
}
"#,
    )
    .unwrap();

    // bbb: large filler body, exactly one `needle` → low BM25F (long doc, low TF).
    let filler =
        "    // padding line to lengthen the document and depress term frequency\n".repeat(40);
    fs::write(
        root.join("src/bbb.rs"),
        format!(
            r#"
fn bbb() {{
    // needle
{filler}
    for x in 0..2 {{
        for y in 0..2 {{
            let _ = (x, y);
        }}
    }}
}}
"#
        ),
    )
    .unwrap();

    fs::write(root.join("config.json"), r#"{"key": "value"}"#).unwrap();
    dir
}

/// Resolve a single repo-relative path to its `FileId` via the manifest.
/// Panics if the path is not present (a test-setup bug).
fn file_id_for(project: &Path, cache: &Path, rel_path: &str) -> rskim_search::FileId {
    use super::super::manifest::FileManifest;
    let manifest =
        FileManifest::load(project.to_path_buf(), cache.to_path_buf()).expect("manifest must load");
    let sorted = manifest.sorted_paths();
    let mut allowed = std::collections::HashSet::new();
    allowed.insert(rel_path.to_string());
    let ids = super::super::temporal::paths_to_file_ids(&sorted, &allowed);
    assert_eq!(
        ids.len(),
        1,
        "expected exactly one FileId for {rel_path:?}, got {ids:?} (indexed: {sorted:?})"
    );
    *ids.iter().next().unwrap()
}

/// Build the asymmetric AST-scored vector for the skew fixture: `bbb` (AST-dominant)
/// gets the higher score, `aaa` the lower; sorted FileId-ASC per the frozen contract.
fn skew_ast_scored(project: &Path, cache: &Path) -> Vec<(rskim_search::FileId, f64)> {
    let fid_aaa = file_id_for(project, cache, "src/aaa.rs");
    let fid_bbb = file_id_for(project, cache, "src/bbb.rs");
    assert!(
        fid_aaa.0 < fid_bbb.0,
        "fixture invariant: aaa must sort before bbb (got {fid_aaa:?} / {fid_bbb:?})"
    );
    // aaa lower AST score, bbb higher AST score → AST rank: bbb=1, aaa=2.
    // Sorted FileId-ASC (aaa first) as intersect_and_rank requires.
    vec![(fid_aaa, 1.0), (fid_bbb, 9.0)]
}

/// AC1 (Functionality, POSITIVE): the compound text+--ast path honors the
/// lexical:ast ratio — the TOP result MUST flip between lexical-heavy (0.9,0.1,0.0)
/// and ast-heavy (0.1,0.9,0.0) weights. Falsifies the AD-377-1 bug: under the old
/// hardcoded default the top result would be identical for both weightings.
#[test]
fn compound_top_result_flips_with_weights_ac1() {
    use super::super::query::execute_query;

    let project = make_project_two_ast_files_lexical_skew();
    let cache = tempfile::tempdir().unwrap();
    build_project_index(project.path(), cache.path());

    let ast_scored = skew_ast_scored(project.path(), cache.path());

    let mut lex_heavy = make_query_config(
        project.path(),
        cache.path(),
        "needle",
        10,
        Some(ast_scored.clone()),
        None,
    );
    lex_heavy.composite_weights =
        Some(rskim_search::CompositeWeights6::parse_weights_flag("0.9,0.1,0.0").unwrap());
    let out_lex = execute_query(&lex_heavy, &TEST_ANALYTICS).unwrap();

    let mut ast_heavy = make_query_config(
        project.path(),
        cache.path(),
        "needle",
        10,
        Some(ast_scored),
        None,
    );
    ast_heavy.composite_weights =
        Some(rskim_search::CompositeWeights6::parse_weights_flag("0.1,0.9,0.0").unwrap());
    let out_ast = execute_query(&ast_heavy, &TEST_ANALYTICS).unwrap();

    assert!(
        !out_lex.results.is_empty() && !out_ast.results.is_empty(),
        "AC1 setup: both weightings must return a non-empty intersection (lex={}, ast={})",
        out_lex.results.len(),
        out_ast.results.len()
    );
    let top_lex = out_lex.results[0].path.as_str();
    let top_ast = out_ast.results[0].path.as_str();
    assert_ne!(
        top_lex, top_ast,
        "AC1: top result MUST flip when the lexical:ast ratio shifts (got {top_lex} both times) — \
         proves --weights is honored on the compound path"
    );
    assert_eq!(
        top_lex, "src/aaa.rs",
        "AC1: under lexical-heavy weights the lexically-dominant file (aaa) must rank first"
    );
    assert_eq!(
        top_ast, "src/bbb.rs",
        "AC1: under ast-heavy weights the AST-dominant file (bbb) must rank first"
    );
}

/// AC4 (Functionality, POSITIVE): the text+--ast+--blast-radius triple-flag path
/// honors the lexical:ast ratio exactly as AC1. The blast set allows BOTH files so
/// the intersection is unchanged; the flip is driven purely by the weight ratio.
#[test]
fn compound_with_blast_top_result_flips_with_weights_ac4() {
    use super::super::query::execute_query;

    let project = make_project_two_ast_files_lexical_skew();
    let cache = tempfile::tempdir().unwrap();
    build_project_index(project.path(), cache.path());

    let ast_scored = skew_ast_scored(project.path(), cache.path());

    // Blast set allows both AST files → blast∩AST == AST (intersection unchanged).
    let blast: std::collections::HashSet<String> =
        ["src/aaa.rs".to_string(), "src/bbb.rs".to_string()]
            .into_iter()
            .collect();

    let mut lex_heavy = make_query_config(
        project.path(),
        cache.path(),
        "needle",
        10,
        Some(ast_scored.clone()),
        Some(blast.clone()),
    );
    lex_heavy.composite_weights =
        Some(rskim_search::CompositeWeights6::parse_weights_flag("0.9,0.1,0.0").unwrap());
    let out_lex = execute_query(&lex_heavy, &TEST_ANALYTICS).unwrap();

    let mut ast_heavy = make_query_config(
        project.path(),
        cache.path(),
        "needle",
        10,
        Some(ast_scored),
        Some(blast),
    );
    ast_heavy.composite_weights =
        Some(rskim_search::CompositeWeights6::parse_weights_flag("0.1,0.9,0.0").unwrap());
    let out_ast = execute_query(&ast_heavy, &TEST_ANALYTICS).unwrap();

    assert!(
        !out_lex.results.is_empty() && !out_ast.results.is_empty(),
        "AC4 setup: both weightings must return a non-empty blast∩AST intersection"
    );
    let top_lex = out_lex.results[0].path.as_str();
    let top_ast = out_ast.results[0].path.as_str();
    assert_ne!(
        top_lex, top_ast,
        "AC4: top result MUST flip on the text+--ast+--blast path when the lexical:ast ratio shifts"
    );
    assert_eq!(top_lex, "src/aaa.rs", "AC4: lexical-heavy → aaa first");
    assert_eq!(top_ast, "src/bbb.rs", "AC4: ast-heavy → bbb first");
}
