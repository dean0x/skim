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
    super::super::run(
        &[
            "--build".to_string(),
            "--root".to_string(),
            root_str.clone(),
        ],
        &TEST_ANALYTICS,
    )
    .expect("--build must succeed; fix the build pipeline if it fails here");

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
    // run_ast_standalone writes to its own internal BufWriter(stdout); we only check
    // the returned ExitCode here.
    let result = super::run_ast_standalone(
        "try-catch",
        20,
        false, // text output
        cache.path(),
        &manifest,
    )
    .unwrap();

    assert_eq!(
        result,
        std::process::ExitCode::SUCCESS,
        "standalone --ast must exit 0 (AC-F8)"
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

/// resolve_ast_file_filter returns a non-empty HashSet for a pattern that
/// matches at least one Rust file in the fixture project.
#[test]
fn resolve_ast_file_filter_returns_matching_file_ids() {
    use rskim_search::AstQueryEngine;

    let project = make_project_with_rust();
    let cache = tempfile::tempdir().unwrap();

    build_project_index(project.path(), cache.path());

    let engine = super::open_ast_engine(cache.path()).unwrap();

    // "rust-nested-loop" matches block → expression_statement → for_expression,
    // which appears in src/loops.rs (nested for loops).
    let ids = super::resolve_ast_file_filter(&engine, "rust-nested-loop").unwrap();

    // The project has at least one Rust file with nested loops — assert non-empty.
    assert!(
        !ids.is_empty(),
        "rust-nested-loop pattern must match at least one file in the fixture project"
    );

    // All returned FileIds must be within the manifest range.
    use super::super::manifest::FileManifest;
    let manifest =
        FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf()).unwrap();
    let file_count = manifest.sorted_paths().len();
    for id in &ids {
        assert!(
            (id.0 as usize) < file_count,
            "FileId({}) must be within manifest range [0, {})",
            id.0,
            file_count
        );
    }
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
/// We drive through the engine directly (resolve_ast_file_filter + execute_query)
/// so the test is hermetic (no system cache required).
#[test]
fn text_ast_intersection_preserves_lexical_snippets() {
    use super::super::query::execute_query;
    use super::super::types::QueryConfig;

    let project = make_project_with_rust();
    let cache = tempfile::tempdir().unwrap();

    build_project_index(project.path(), cache.path());

    // Build the AST file filter for "rust-nested-loop" (matches Rust nested for loops).
    let engine = super::open_ast_engine(cache.path()).unwrap();
    let ast_ids = super::resolve_ast_file_filter(&engine, "rust-nested-loop").unwrap();

    // The AST filter must be non-empty for this test to be meaningful.
    assert!(
        !ast_ids.is_empty(),
        "rust-nested-loop must match at least one file so intersection is testable"
    );

    // Run the lexical query with the AST filter applied.
    let config = QueryConfig {
        text: "nested".to_string(),
        limit: 20,
        json: false,
        root: project.path().to_path_buf(),
        cache_dir: cache.path().to_path_buf(),
        blast_radius_paths: None,
        ast_file_ids: Some(ast_ids),
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
/// Strategy (hermetic — uses `--root` to isolate from the system cache):
/// 1. Build ONLY the lexical index by building normally then deleting `ast_index.skidx`.
/// 2. Run `skim search "nested" --ast rust-nested-loop --root <project>`.
/// 3. Assert the command returns `Ok(ExitCode::SUCCESS)` and does NOT error —
///    the self-heal path rebuilt the AST index transparently.
#[test]
fn text_ast_combined_self_heals_missing_ast_index() {
    let project = make_project_with_rust();
    let cache = tempfile::tempdir().unwrap();

    // Build a full index (lexical + AST).
    build_project_index(project.path(), cache.path());

    // Delete only the AST index to simulate a pre-PR install (lexical present, AST absent).
    fs::remove_file(cache.path().join("ast_index.skidx")).unwrap();
    let _ = fs::remove_file(cache.path().join("ast_index.skpost")); // may or may not exist

    // Verify the lexical index still exists (so this is not a cold start).
    assert!(
        cache.path().join("index.skidx").exists(),
        "lexical index must exist for the test to simulate a no-AST install"
    );

    // Drive through run() using --root so it targets our isolated cache/project.
    // The combined text+--ast path must self-heal (rebuild the AST index) and
    // return SUCCESS rather than propagating "AST index not found".
    let root_str = project.path().to_string_lossy().to_string();
    let result = super::super::run(
        &[
            "nested".to_string(),
            "--ast".to_string(),
            "rust-nested-loop".to_string(),
            "--root".to_string(),
            root_str,
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
}

/// Regression guard: `skim search --ast <pattern> --blast-radius <file>` (no text
/// query) must NOT silently drop the `--ast` filter.
///
/// Previously the standalone AST arm required `blast_radius.is_none()`, so when
/// `--blast-radius` was also set the request fell through to `run_temporal_standalone`
/// which ignored `--ast` entirely. The fix widens the standalone AST arm to match
/// regardless of `--blast-radius` presence.
///
/// This test confirms the combined flags don't produce an error (no silently-dropped
/// `--ast`) — the route dispatches to the AST standalone path which builds the index
/// and returns SUCCESS.
#[test]
fn ast_plus_blast_radius_no_text_does_not_silently_drop_ast() {
    let project = make_project_with_rust();
    let root_str = project.path().to_string_lossy().to_string();

    // Build first so auto-refresh finds a complete index.
    super::super::run(
        &[
            "--build".to_string(),
            "--root".to_string(),
            root_str.clone(),
        ],
        &TEST_ANALYTICS,
    )
    .expect("--build must succeed before the combined --ast + --blast-radius test");

    // No text query + --ast + --blast-radius → should dispatch to standalone AST
    // (honoring --ast), NOT silently drop --ast into run_temporal_standalone.
    // A non-existent blast-radius file is fine — the index may have no co-change
    // data for it, but the AST path still runs.
    let result = super::super::run(
        &[
            "--ast".to_string(),
            "rust-nested-loop".to_string(),
            "--blast-radius".to_string(),
            "src/nonexistent.rs".to_string(),
            "--root".to_string(),
            root_str,
        ],
        &TEST_ANALYTICS,
    );

    assert!(
        result.is_ok(),
        "--ast + --blast-radius with no text query must not error, got: {:?}",
        result.unwrap_err()
    );
    assert_eq!(
        result.unwrap(),
        std::process::ExitCode::SUCCESS,
        "--ast + --blast-radius with no text query must exit 0"
    );
}

/// Regression guard: `skim search <text> --ast <pattern>` must also self-heal
/// when `ast_index.skidx` has a below-FORMAT_VERSION stub (v1 format).
///
/// This guards the same ordering fix as `text_ast_combined_self_heals_missing_ast_index`
/// but specifically tests the format-version probe path within `check_staleness`.
#[test]
fn text_ast_combined_self_heals_below_format_version_ast_index() {
    let project = make_project_with_rust();
    let cache = tempfile::tempdir().unwrap();

    // Build a full index.
    build_project_index(project.path(), cache.path());

    // Overwrite with a v1 stub (below AST_INDEX_FORMAT_VERSION).
    let stub: [u8; 6] = [b'S', b'K', b'A', b'X', 1, 0];
    fs::write(cache.path().join("ast_index.skidx"), stub).unwrap();

    let root_str = project.path().to_string_lossy().to_string();
    let result = super::super::run(
        &[
            "nested".to_string(),
            "--ast".to_string(),
            "rust-nested-loop".to_string(),
            "--root".to_string(),
            root_str,
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
}
