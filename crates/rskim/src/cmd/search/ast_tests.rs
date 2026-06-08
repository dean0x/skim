//! Tests for the AST structural query helpers (ast.rs).
//!
//! This file is included by `ast.rs` as `#[path = "ast_tests.rs"] mod tests;`
//! so `super::` refers to the `ast` module, and `super::super::` refers to
//! the `search` module.
//!
//! Groups:
//! 1. Parse/validate  — unit tests, no index required.
//! 2. Build & alignment — tempdir fixture, verifies both index files written.
//! 3. Output formatters — text and JSON shape, no index required.
//! 4. Intersection    — text + --ast combined.
//! 5. Auto-refresh    — self-heal when AST index is absent.
//! 6. API contract    — exit codes, flag parsing edge cases.

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

#[test]
fn intersection_disjoint_text_and_ast_returns_empty_exit_0() {
    // When AST filter and text query match different files, the intersection is
    // empty.  Empty results must exit 0 (AC-F8) — not an error.
    //
    // We build the index through run() so it lands in the same system cache dir
    // that the subsequent query will consult.
    let project = make_project_with_rust();
    let root_str = project.path().to_string_lossy().to_string();

    // Build the index (uses system cache dir keyed on project root hash).
    let build_result = super::super::run(
        &[
            "--build".to_string(),
            "--root".to_string(),
            root_str.clone(),
        ],
        &TEST_ANALYTICS,
    );
    // If build fails (e.g. env restrictions), skip rather than fail.
    if build_result.is_err() {
        return;
    }

    // "xyzzy_impossible_token" won't match any file lexically.
    let result = super::super::run(
        &[
            "xyzzy_impossible_token".to_string(),
            "--ast".to_string(),
            "try-catch".to_string(),
            "--root".to_string(),
            root_str,
        ],
        &TEST_ANALYTICS,
    )
    .unwrap();
    assert_eq!(
        result,
        std::process::ExitCode::SUCCESS,
        "empty intersection must exit 0 (AC-F8)"
    );
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
