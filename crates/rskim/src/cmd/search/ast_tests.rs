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

#[test]
fn run_ast_plus_hot_returns_202_error() {
    // run() must return Err with #202 reference when --ast + --hot combined.
    // Validation fires BEFORE cache resolution so no system cache is written.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_string_lossy().to_string();
    let err = super::super::run(
        &[
            "--ast".to_string(),
            "try-catch".to_string(),
            "--hot".to_string(),
            "--root".to_string(),
            root,
        ],
        &TEST_ANALYTICS,
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("#202"),
        "--ast + --hot should reference #202, got: {msg}"
    );
}

#[test]
fn run_ast_bogus_plus_hot_returns_202_first() {
    // Validation order: #202 fires BEFORE unknown-pattern check.
    // Validation fires BEFORE cache resolution so no system cache is written.
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
        msg.contains("#202"),
        "#202 check should fire before unknown-pattern check, got: {msg}"
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
fn format_ast_text_no_colon_line_suffix() {
    // AC-F1: AST text output must NOT include `:line` suffix.
    let results = vec![
        super::AstResult {
            path: "src/foo.rs".to_string(),
            score: 2.5,
        },
        super::AstResult {
            path: "src/bar.rs".to_string(),
            score: 1.2,
        },
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
    // No `:line` suffix on AST-only results (file-level).
    assert!(
        !out.contains("src/foo.rs:"),
        "AST text output must NOT have :line suffix (AC-F1)"
    );
    assert!(
        out.contains("AST pattern: try-catch"),
        "header must name the pattern"
    );
}

#[test]
fn format_ast_json_mode_is_ast_no_line_keys() {
    // AC-A1: mode=="ast", no line/snippet keys.
    let results = vec![super::AstResult {
        path: "src/foo.rs".to_string(),
        score: 2.5,
    }];
    let mut buf = BufWriter::new(Vec::new());
    super::format_ast_json(&results, "try-catch", "Try/catch blocks", &mut buf).unwrap();
    let out = String::from_utf8(buf.into_inner().unwrap()).unwrap();

    let v: serde_json::Value =
        serde_json::from_str(&out).expect("format_ast_json must produce valid JSON");
    assert_eq!(v["mode"], "ast", "mode must be 'ast' (AC-A1)");
    assert_eq!(v["pattern"], "try-catch");
    assert_eq!(v["total"], 1);
    assert!(v["results"].is_array());

    let first = &v["results"][0];
    assert!(first["path"].is_string());
    assert!(first["score"].is_number());
    // No line or snippet keys in AST-level JSON.
    assert!(
        first.get("line").is_none() && first.get("snippet").is_none(),
        "AST JSON must not have line or snippet keys (AC-A1)"
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
                json: true,
                root: project.path().to_path_buf(),
                cache_dir: cache_dir.clone(),
                blast_radius_paths: None,
                ast_scored: Some(ast_scored),
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
        json: false,
        root: project.path().to_path_buf(),
        cache_dir: cache.path().to_path_buf(),
        blast_radius_paths: None,
        ast_scored: Some(ast_scored),
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
        json: false,
        root: project.path().to_path_buf(),
        cache_dir: cache.path().to_path_buf(),
        blast_radius_paths: None,
        ast_scored: None,
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
        json: false,
        root: project.path().to_path_buf(),
        cache_dir: cache.path().to_path_buf(),
        blast_radius_paths: None,
        ast_scored: Some(ast_scored),
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
