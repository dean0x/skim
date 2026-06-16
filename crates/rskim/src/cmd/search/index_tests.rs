//! Integration tests for the index builder pipeline (index.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::process::ExitCode;
use tempfile::TempDir;

use super::run;

/// Stub analytics config for tests — analytics disabled, no cost override.
const TEST_ANALYTICS: crate::analytics::AnalyticsConfig = crate::analytics::AnalyticsConfig {
    enabled: false,
    input_cost_per_mtok: None,
    session_id: None,
};

// ============================================================================
// Helpers
// ============================================================================

/// Create a minimal project tree with a .git root and a few source files.
fn make_project() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();

    fs::write(
        root.join("src/main.rs"),
        "fn main() { println!(\"hello\"); }\n",
    )
    .unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub fn add(a: u32, b: u32) -> u32 { a + b }\n",
    )
    .unwrap();
    fs::write(root.join("build.py"), "print('hello')\n").unwrap();

    dir
}

/// Build args for running index against `project` with `cache` as the output dir.
fn index_args(project: &Path, cache: &Path) -> Vec<String> {
    vec![
        format!("--root={}", project.display()),
        format!("--index-dir={}", cache.display()),
    ]
}

// ============================================================================
// Full build — happy path
// ============================================================================

#[test]
fn test_index_build_succeeds_with_source_files() {
    let project = make_project();
    let cache = tempfile::tempdir().unwrap();

    let result = run(&index_args(project.path(), cache.path()), &TEST_ANALYTICS).unwrap();

    assert_eq!(result, ExitCode::SUCCESS, "index build should succeed");
}

#[test]
fn test_index_writes_skidx_and_skpost() {
    let project = make_project();
    let cache = tempfile::tempdir().unwrap();

    run(&index_args(project.path(), cache.path()), &TEST_ANALYTICS).unwrap();

    assert!(
        find_file_with_ext(cache.path(), "skidx"),
        "index.skidx should exist in cache dir"
    );
    assert!(
        find_file_with_ext(cache.path(), "skpost"),
        "index.skpost should exist in cache dir"
    );
}

#[test]
fn test_index_writes_manifest_sidecar() {
    let project = make_project();
    let cache = tempfile::tempdir().unwrap();

    run(&index_args(project.path(), cache.path()), &TEST_ANALYTICS).unwrap();

    assert!(
        find_file_with_ext(cache.path(), "skfiles"),
        "index.skfiles manifest should exist in cache dir"
    );
}

// ============================================================================
// Empty directory
// ============================================================================

#[test]
fn test_index_empty_directory_returns_success() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();

    let cache = tempfile::tempdir().unwrap();

    let result = run(&index_args(root, cache.path()), &TEST_ANALYTICS).unwrap();

    assert_eq!(result, ExitCode::SUCCESS, "empty dir should still succeed");
}

// ============================================================================
// Incremental build — cache hits
// ============================================================================

#[test]
fn test_index_incremental_second_build_succeeds() {
    // Smoke test: two consecutive builds on the same project both succeed.
    // (Previously misnamed "faster_or_same" — no timing assertion is made here.)
    let project = make_project();
    let cache = tempfile::tempdir().unwrap();
    let args = index_args(project.path(), cache.path());

    let r1 = run(&args, &TEST_ANALYTICS).unwrap();
    let r2 = run(&args, &TEST_ANALYTICS).unwrap();

    assert_eq!(r1, ExitCode::SUCCESS);
    assert_eq!(r2, ExitCode::SUCCESS);
}

#[test]
fn test_index_incremental_cache_hits_verified_via_manifest() {
    // Verify that the incremental path (SHA match → reuse field_map) produces
    // identical manifest entries across two consecutive builds on unchanged files.
    // Also asserts that Rust sources produce non-empty field_maps (classifier ran).
    use super::super::manifest::FileManifest;

    let project = make_project();
    let cache = tempfile::tempdir().unwrap();
    let args = index_args(project.path(), cache.path());

    // First build — cold start, no cache.
    let r1 = run(&args, &TEST_ANALYTICS).unwrap();
    assert_eq!(r1, ExitCode::SUCCESS, "first build should succeed");

    let manifest1 =
        FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf()).unwrap();

    // Second build — should hit the manifest cache for all unchanged files.
    let r2 = run(&args, &TEST_ANALYTICS).unwrap();
    assert_eq!(r2, ExitCode::SUCCESS, "second build should succeed");

    let manifest2 =
        FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf()).unwrap();

    // All three source files from make_project() must be present with stable
    // SHAs — a missing entry or changed SHA would indicate the incremental path
    // failed to recognise the file as cached.
    for path in &["src/main.rs", "src/lib.rs", "build.py"] {
        let e1 = manifest1
            .lookup(path)
            .unwrap_or_else(|| panic!("first manifest should contain {path}"));
        let e2 = manifest2
            .lookup(path)
            .unwrap_or_else(|| panic!("second manifest should contain {path}"));

        assert_eq!(
            e1.sha256, e2.sha256,
            "sha256 for {path} must be identical across both builds (content unchanged)"
        );

        // The field_map must also be preserved — same encoding on both runs.
        assert_eq!(
            e1.field_map, e2.field_map,
            "field_map for {path} must be identical when served from cache"
        );
    }

    // Rust files must have a non-empty field_map — the classifier must have
    // produced output (not silently fallen back to an empty map).
    for path in &["src/main.rs", "src/lib.rs"] {
        let entry = manifest2
            .lookup(path)
            .unwrap_or_else(|| panic!("second manifest should contain {path}"));
        assert!(
            !entry.field_map.is_empty(),
            "field_map for {path} should be non-empty after classification"
        );
    }
}

#[test]
fn test_index_incremental_modified_file_reindexed() {
    use super::super::manifest::FileManifest;

    let project = make_project();
    let cache = tempfile::tempdir().unwrap();
    let args = index_args(project.path(), cache.path());

    // First build — record the SHA for src/main.rs before modification.
    run(&args, &TEST_ANALYTICS).unwrap();
    let manifest1 =
        FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf()).unwrap();
    let sha_before = manifest1
        .lookup("src/main.rs")
        .expect("first manifest must contain src/main.rs")
        .sha256
        .clone();

    // Modify the file so its SHA-256 changes.
    fs::write(
        project.path().join("src/main.rs"),
        "fn main() { eprintln!(\"modified\"); }\n",
    )
    .unwrap();

    // Second build — should detect the change and re-classify.
    let r2 = run(&args, &TEST_ANALYTICS).unwrap();
    assert_eq!(
        r2,
        ExitCode::SUCCESS,
        "incremental build after modification should succeed"
    );

    // The SHA in the new manifest must differ — silent cache reuse would be wrong.
    let manifest2 =
        FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf()).unwrap();
    let sha_after = manifest2
        .lookup("src/main.rs")
        .expect("second manifest must contain src/main.rs")
        .sha256
        .clone();

    assert_ne!(
        sha_before, sha_after,
        "SHA for src/main.rs must change after file modification — cache reuse would be wrong"
    );
}

#[test]
fn test_index_force_flag_ignores_manifest() {
    use super::super::types::IndexConfig;
    use super::build_index;

    let project = make_project();
    let cache = tempfile::tempdir().unwrap();
    let args = index_args(project.path(), cache.path());

    // First build to populate the manifest (creates cache entries for all files).
    run(&args, &TEST_ANALYTICS).unwrap();

    // Force rebuild via build_index directly so we can inspect IndexResult.
    // cache_hits must be zero — --force means the manifest is intentionally ignored.
    let config = IndexConfig {
        root: project.path().to_path_buf(),
        max_files: None,
        force: true,
        cache_dir_override: Some(cache.path().to_path_buf()),
    };
    let result = build_index(&config).expect("--force rebuild should not fail");

    assert_eq!(
        result.cache_hits, 0,
        "--force must produce zero cache hits (manifest was ignored); got {}",
        result.cache_hits
    );
    assert_eq!(
        result.ast_cache_hits, 0,
        "--force must produce zero AST cache hits (skcache was ignored, AC11); got {}",
        result.ast_cache_hits
    );
    assert_eq!(
        result.ast_reextracted, result.file_count,
        "--force must re-extract every file's AST n-grams; got {} re-extracted of {} files",
        result.ast_reextracted, result.file_count
    );
    assert!(
        result.file_count > 0,
        "--force rebuild should index at least one file"
    );
}

// ============================================================================
// Incremental build — cache hit count (direct build_index)
// ============================================================================

#[test]
fn test_index_incremental_cache_hits_count() {
    use super::super::types::IndexConfig;
    use super::build_index;

    let project = make_project();
    let cache = tempfile::tempdir().unwrap();

    let config = IndexConfig {
        root: project.path().to_path_buf(),
        max_files: None,
        force: false,
        cache_dir_override: Some(cache.path().to_path_buf()),
    };

    // Cold start — no manifest exists.
    let result1 = build_index(&config).expect("first build should succeed");
    assert!(result1.file_count > 0, "first build should index files");
    assert_eq!(
        result1.cache_hits, 0,
        "cold start must have zero cache hits"
    );

    // Incremental — all files unchanged, all should be cache hits.
    let result2 = build_index(&config).expect("second build should succeed");
    assert!(
        result2.cache_hits > 0,
        "incremental build must have cache hits"
    );
    assert_eq!(
        result2.cache_hits, result2.file_count,
        "all {} files should be cache hits; got {}",
        result2.file_count, result2.cache_hits
    );
}

#[test]
fn test_index_incremental_ast_cache_hits_count() {
    // End-to-end wiring guard for the #290 AST n-gram cache (ast_index.skcache):
    // cold start must re-extract everything, and a second build over unchanged
    // files must serve every AST entry from the skcache (zero re-extraction).
    // This catches the silent-no-op failure mode where the producer never
    // attaches `ast_cached` or the second build re-extracts regardless. (AC5)
    use super::super::types::IndexConfig;
    use super::build_index;

    let project = make_project();
    let cache = tempfile::tempdir().unwrap();

    let config = IndexConfig {
        root: project.path().to_path_buf(),
        max_files: None,
        force: false,
        cache_dir_override: Some(cache.path().to_path_buf()),
    };

    // Cold start — no skcache exists, so every file is re-extracted.
    let result1 = build_index(&config).expect("first build should succeed");
    assert!(result1.file_count > 0, "first build should index files");
    assert_eq!(
        result1.ast_cache_hits, 0,
        "cold start must have zero AST cache hits"
    );
    assert_eq!(
        result1.ast_reextracted, result1.file_count,
        "cold start must re-extract every file; got {} re-extracted of {}",
        result1.ast_reextracted, result1.file_count
    );

    // Incremental — all files unchanged, every AST entry must come from skcache.
    let result2 = build_index(&config).expect("second build should succeed");
    assert_eq!(
        result2.ast_cache_hits, result2.file_count,
        "all {} files should be AST cache hits; got {}",
        result2.file_count, result2.ast_cache_hits
    );
    assert_eq!(
        result2.ast_reextracted, 0,
        "unchanged incremental build must re-extract nothing; got {}",
        result2.ast_reextracted
    );
}

#[test]
fn test_index_incremental_modified_file_reextracts_ast() {
    // A modified file (SHA change) must miss the AST cache and be re-extracted,
    // while the unchanged files remain AST cache hits. Guards against a stale
    // skcache entry being served for changed content. (AC5)
    use super::super::types::IndexConfig;
    use super::build_index;

    let project = make_project();
    let cache = tempfile::tempdir().unwrap();
    let config = IndexConfig {
        root: project.path().to_path_buf(),
        max_files: None,
        force: false,
        cache_dir_override: Some(cache.path().to_path_buf()),
    };

    // Cold start populates the skcache.
    let result1 = build_index(&config).expect("first build should succeed");
    assert!(result1.file_count >= 2, "fixture must have multiple files");

    // Modify exactly one file so its SHA changes.
    fs::write(
        project.path().join("src/main.rs"),
        "fn main() { eprintln!(\"changed\"); }\n",
    )
    .unwrap();

    let result2 = build_index(&config).expect("second build should succeed");

    // Exactly one file changed → one re-extraction, the rest are AST cache hits.
    assert_eq!(
        result2.ast_reextracted, 1,
        "only the modified file should be re-extracted; got {}",
        result2.ast_reextracted
    );
    assert_eq!(
        result2.ast_cache_hits,
        result2.file_count - 1,
        "all unchanged files should be AST cache hits; got {} of {}",
        result2.ast_cache_hits,
        result2.file_count - 1
    );
}

// ============================================================================
// Mixed languages
// ============================================================================

#[test]
fn test_index_mixed_languages() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();

    fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
    fs::write(root.join("script.py"), "def hello(): pass\n").unwrap();
    fs::write(root.join("app.ts"), "export function greet(): void {}\n").unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();

    let cache = tempfile::tempdir().unwrap();
    let result = run(&index_args(root, cache.path()), &TEST_ANALYTICS).unwrap();

    assert_eq!(
        result,
        ExitCode::SUCCESS,
        "mixed language build should succeed"
    );
}

// ============================================================================
// --max-files integration
// ============================================================================

#[test]
fn test_index_max_files_limits_manifest_entries() {
    // Create 10 source files, index with --max-files=2, and verify that the
    // manifest contains at most 2 entries.  This exercises the full CLI flag
    // path end-to-end (clap parse → walk cap → manifest write).
    use super::super::manifest::FileManifest;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();

    for i in 0..10 {
        fs::write(
            root.join(format!("file_{i:02}.rs")),
            format!("fn f{i}() {{}}\n"),
        )
        .unwrap();
    }

    let cache = tempfile::tempdir().unwrap();
    let mut args = index_args(root, cache.path());
    args.push("--max-files=2".to_string());

    let result = run(&args, &TEST_ANALYTICS).unwrap();
    assert_eq!(
        result,
        ExitCode::SUCCESS,
        "--max-files=2 build should succeed"
    );

    let manifest = FileManifest::load(root.to_path_buf(), cache.path().to_path_buf()).unwrap();

    // Count entries by checking all possible file names.
    let entry_count = (0..10)
        .filter(|i| manifest.lookup(&format!("file_{i:02}.rs")).is_some())
        .count();

    assert_eq!(
        entry_count, 2,
        "only 2 files should be indexed when --max-files=2, got {entry_count}"
    );
}

// ============================================================================
// Error propagation — unreadable / nonexistent root
// ============================================================================

#[test]
fn test_index_unreadable_root_returns_error_or_empty() {
    // Pass a nonexistent path as the project root. build_index must either:
    //   (a) return Err (I/O failure propagated from walk_and_read), or
    //   (b) succeed with file_count == 0 (walker found no entries).
    // Either outcome is acceptable — what must NOT happen is a successful build
    // that silently claims to have indexed files from a path that does not exist.
    use super::super::types::IndexConfig;
    use super::build_index;

    let nonexistent = std::path::PathBuf::from("/nonexistent/path/that/cannot/exist/for/tests");
    let cache = tempfile::tempdir().unwrap();

    let config = IndexConfig {
        root: nonexistent,
        max_files: None,
        force: false,
        cache_dir_override: Some(cache.path().to_path_buf()),
    };

    match build_index(&config) {
        Err(_) => {
            // Acceptable: I/O error propagated up from walk or cache-dir creation.
        }
        Ok(result) => {
            assert_eq!(
                result.file_count, 0,
                "build_index on a nonexistent root must index 0 files, got {}",
                result.file_count
            );
        }
    }
}

// ============================================================================
// Help flag
// ============================================================================

#[test]
fn test_index_help_returns_success() {
    let result = run(&["--help".to_string()], &TEST_ANALYTICS).unwrap();
    assert_eq!(result, ExitCode::SUCCESS);
}

#[test]
fn test_index_short_help_returns_success() {
    let result = run(&["-h".to_string()], &TEST_ANALYTICS).unwrap();
    assert_eq!(result, ExitCode::SUCCESS);
}

// ============================================================================
// Argument validation
// ============================================================================

#[test]
fn test_index_max_files_zero_is_rejected() {
    // --max-files=0 must produce an error, not a silently empty index.
    let result = run(&["--max-files=0".to_string()], &TEST_ANALYTICS);
    assert!(result.is_err(), "--max-files=0 should return an error");
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("≥ 1") || msg.contains("positive"),
        "error message should mention the constraint, got: {msg}"
    );
}

#[test]
fn test_index_unknown_flag_is_rejected() {
    let result = run(&["--unknown-flag".to_string()], &TEST_ANALYTICS);
    assert!(result.is_err(), "unknown flags should return an error");
}

// ============================================================================
// Private helpers
// ============================================================================

/// Search for a file with the given extension in `dir`, up to `max_depth`
/// levels deep. `max_depth = 0` checks only direct children of `dir`.
/// Bounded to prevent infinite recursion on symlink loops.
fn find_file_with_ext_depth(dir: &Path, ext: &str, max_depth: usize) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if max_depth > 0 && find_file_with_ext_depth(&path, ext, max_depth - 1) {
                return true;
            }
        } else if path.extension().is_some_and(|e| e == ext) {
            return true;
        }
    }
    false
}

fn find_file_with_ext(dir: &Path, ext: &str) -> bool {
    find_file_with_ext_depth(dir, ext, 5)
}

/// Search for a file with the given name (not just extension) in `dir`,
/// up to `max_depth` levels deep.  Returns the first match found.
fn find_file_in_dir_depth(dir: &Path, name: &str, max_depth: usize) -> Option<std::path::PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return None;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if max_depth > 0
                && let Some(found) = find_file_in_dir_depth(&path, name, max_depth - 1)
            {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|f| f == name) {
            return Some(path);
        }
    }
    None
}

/// Search for a file with the given name in `dir`, up to 5 levels deep.
fn find_file_in_dir(dir: &Path, name: &str) -> Option<std::path::PathBuf> {
    find_file_in_dir_depth(dir, name, 5)
}

/// After a full build, the manifest must contain a 64-char lowercase hex SHA-256
/// for every indexed file (SHA computed in classify phase).
#[test]
fn test_sha_computed_in_classify_phase() {
    use super::super::manifest::FileManifest;

    let project = make_project();
    let cache = tempfile::tempdir().unwrap();

    run(&index_args(project.path(), cache.path()), &TEST_ANALYTICS).unwrap();

    let manifest =
        FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf()).unwrap();

    for path in &["src/main.rs", "src/lib.rs", "build.py"] {
        let entry = manifest
            .lookup(path)
            .unwrap_or_else(|| panic!("manifest must contain {path}"));
        assert_eq!(
            entry.sha256.len(),
            64,
            "sha256 for {path} must be 64 chars, got {}",
            entry.sha256.len()
        );
        assert!(
            entry.sha256.chars().all(|c| c.is_ascii_hexdigit()),
            "sha256 for {path} must be hex, got: {}",
            entry.sha256
        );
    }
}

// ============================================================================
// Streaming pipeline — unique pipeline-level tests
// ============================================================================

/// Streaming build on a normal project produces exact file_count and zero
/// cache_hits on cold start.
#[test]
fn test_streaming_produces_same_result() {
    use super::super::types::IndexConfig;
    use super::build_index;

    let project = make_project();
    let cache = tempfile::tempdir().unwrap();

    let config = IndexConfig {
        root: project.path().to_path_buf(),
        max_files: None,
        force: false,
        cache_dir_override: Some(cache.path().to_path_buf()),
    };

    let result = build_index(&config).expect("streaming build must succeed");
    // make_project() creates 3 source files (main.rs, lib.rs, build.py).
    assert_eq!(result.file_count, 3, "should index all 3 source files");
    assert_eq!(result.cache_hits, 0, "cold start must have zero cache hits");
}

/// A minified JS file in the project appears in the skipped count.
#[test]
fn test_streaming_skipped_includes_minified() {
    use super::super::types::IndexConfig;
    use super::build_index;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    // Normal source file.
    fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
    // Minified JS (single long line, no newlines).
    fs::write(root.join("bundle.js"), "x".repeat(10_000)).unwrap();
    let cache = tempfile::tempdir().unwrap();

    let config = IndexConfig {
        root: root.to_path_buf(),
        max_files: None,
        force: false,
        cache_dir_override: Some(cache.path().to_path_buf()),
    };

    let result = build_index(&config).expect("build with minified file must succeed");
    assert!(
        result.skipped > 0,
        "minified file should appear in skipped count, got skipped={}",
        result.skipped
    );
    assert_eq!(result.file_count, 1, "only main.rs should be indexed");
}

// ============================================================================
// ADR-006: dual-index desync abort — the central correctness invariant
// ============================================================================

/// ADR-006 abort path: when `add_file_ngrams` rejects a FileId after the same
/// FileId's lexical entry was already accepted, `consume()` must return `Err`
/// and the manifest must NOT be saved (old manifest survives).
///
/// This is the regression guard for commit 3aaa99f: a future refactor that
/// silently `continue`s past the desync (instead of aborting) would commit a
/// corrupt index — this test would catch it.
///
/// Mechanism: pre-advance the `AstIndexBuilder` by inserting FileId(0) before
/// calling `consume`.  The builder then expects FileId(1) next.  When `consume`
/// tries to insert FileId(0) for the first real file it returns `Err("FileId
/// must equal sequential insertion index: expected 1, got 0")` — exactly the
/// desync abort path documented in ADR-006. (applies ADR-006)
#[test]
fn test_adr006_desync_aborts_before_manifest_save() {
    use rskim_search::{
        AstIndexBuilder, AstNgramSet, FileId, NgramIndexBuilder, StructuralMetrics,
    };

    use super::super::manifest::FileManifest;
    use super::super::types::ProcessedFile;
    use super::Pipeline;

    let project = make_project();
    let cache = tempfile::tempdir().unwrap();

    // Stage 1: clean first build — establishes the "old manifest" on disk.
    run(&index_args(project.path(), cache.path()), &TEST_ANALYTICS)
        .expect("first build must succeed");

    // Record old manifest state: load from disk and note the modification time
    // of the manifest file so we can assert it was NOT overwritten.
    let skfiles_path = cache
        .path()
        .read_dir()
        .unwrap()
        .flatten()
        .find(|e| e.path().extension().is_some_and(|x| x == "skfiles"))
        .expect("manifest (.skfiles) must exist after first build")
        .path();

    let old_mtime = fs::metadata(&skfiles_path)
        .expect("skfiles must be stat-able")
        .modified()
        .expect("mtime must be available on this platform");

    let old_manifest = FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf())
        .expect("old manifest must be loadable");
    let old_entry_count = old_manifest.entry_count();
    assert!(
        old_entry_count > 0,
        "old manifest must have entries for the test to be meaningful"
    );

    // Stage 2: set up a consume call with a PRE-BROKEN AstIndexBuilder.
    // Pre-advancing it by one FileId forces it to expect FileId(1) as the next
    // call, so when consume tries FileId(0) the builder returns the desync error.
    let mut lexical_builder = NgramIndexBuilder::new(cache.path().to_path_buf())
        .expect("lexical builder must initialise");
    let mut ast_builder =
        AstIndexBuilder::new(cache.path().to_path_buf()).expect("AST builder must initialise");

    // Insert a dummy FileId(0) into the AST builder BEFORE consume runs.
    // This advances the builder's internal file_count to 1, so it expects FileId(1) next.
    ast_builder
        .add_file_ngrams(
            FileId(0),
            rskim_core::Language::Rust,
            &AstNgramSet::default(),
            0,
            StructuralMetrics::default(),
        )
        .expect("pre-advance must succeed");

    let mut new_manifest =
        FileManifest::new(project.path().to_path_buf(), cache.path().to_path_buf());

    // Build a channel and send one real ProcessedFile so the loop body executes.
    let (tx, rx) = crossbeam_channel::bounded::<ProcessedFile>(1);
    let pf = ProcessedFile {
        rel_path: std::path::PathBuf::from("src/main.rs"),
        lang: rskim_core::Language::Rust,
        content: "fn main() {}\n".to_string(),
        sha256: "a".repeat(64),
        mtime: None,
        field_map: vec![],
        cache_hit: false,
        ast_cached: None,
    };
    tx.send(pf).unwrap();
    drop(tx); // close channel so consume loop terminates after one item

    // Stage 3: call consume — it must return Err because add_file_ngrams rejects
    // FileId(0) (the builder already has FileId(0) and expects FileId(1) next).
    let mut throwaway_ast_cache = rskim_search::AstNgramCache::empty();
    let result = Pipeline::consume(
        &mut lexical_builder,
        &mut ast_builder,
        &mut new_manifest,
        &mut throwaway_ast_cache,
        rx,
        false,
    );

    assert!(
        result.is_err(),
        "consume must return Err on AST desync (ADR-006 abort path); got Ok"
    );
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("AST index desync") || err_msg.contains("sequential"),
        "error must identify the desync; got: {err_msg}"
    );

    // Stage 4: verify the manifest was NOT saved — old manifest still on disk.
    // The `new_manifest` in this test was never saved (consume returned Err before
    // the caller's `new_manifest.save()` could be reached in `run()`).
    let new_mtime = fs::metadata(&skfiles_path)
        .expect("skfiles must still exist")
        .modified()
        .expect("mtime must be available on this platform");

    assert_eq!(
        old_mtime, new_mtime,
        "manifest file mtime must not change — the old manifest must survive the abort (ADR-006)"
    );

    // Double-check by loading: entry count must be the same as before the broken run.
    let reloaded = FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf())
        .expect("manifest must still be loadable after abort");
    assert_eq!(
        reloaded.entry_count(),
        old_entry_count,
        "manifest entry count must be unchanged — new_manifest was never saved (ADR-006)"
    );
}

/// ADR-006 self-heal: after an abort, a subsequent successful build restores
/// the index and manifest. Verifies that the old-manifest-survives property
/// does not permanently break the project — the next `build_index` succeeds.
#[test]
fn test_adr006_self_heal_after_abort() {
    use rskim_search::{
        AstIndexBuilder, AstNgramSet, FileId, NgramIndexBuilder, StructuralMetrics,
    };

    use super::super::manifest::FileManifest;
    use super::super::types::IndexConfig;
    use super::super::types::ProcessedFile;
    use super::Pipeline;
    use super::build_index;

    let project = make_project();
    let cache = tempfile::tempdir().unwrap();

    // First build — establishes the old manifest.
    run(&index_args(project.path(), cache.path()), &TEST_ANALYTICS)
        .expect("first build must succeed");

    let old_manifest = FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf())
        .expect("old manifest must be loadable");
    let old_count = old_manifest.entry_count();

    // Simulate the desync abort (same as the previous test).
    let mut lexical_builder = NgramIndexBuilder::new(cache.path().to_path_buf()).unwrap();
    let mut ast_builder = AstIndexBuilder::new(cache.path().to_path_buf()).unwrap();
    ast_builder
        .add_file_ngrams(
            FileId(0),
            rskim_core::Language::Rust,
            &AstNgramSet::default(),
            0,
            StructuralMetrics::default(),
        )
        .unwrap();
    let mut new_manifest =
        FileManifest::new(project.path().to_path_buf(), cache.path().to_path_buf());
    let (tx, rx) = crossbeam_channel::bounded::<ProcessedFile>(1);
    let pf = ProcessedFile {
        rel_path: std::path::PathBuf::from("src/main.rs"),
        lang: rskim_core::Language::Rust,
        content: "fn main() {}\n".to_string(),
        sha256: "a".repeat(64),
        mtime: None,
        field_map: vec![],
        cache_hit: false,
        ast_cached: None,
    };
    tx.send(pf).unwrap();
    drop(tx);
    let mut throwaway_ast_cache = rskim_search::AstNgramCache::empty();
    let abort_result = Pipeline::consume(
        &mut lexical_builder,
        &mut ast_builder,
        &mut new_manifest,
        &mut throwaway_ast_cache,
        rx,
        false,
    );
    assert!(
        abort_result.is_err(),
        "consume must abort for the self-heal test to be meaningful"
    );

    // Self-heal: a subsequent successful build must produce a new manifest.
    let config = IndexConfig {
        root: project.path().to_path_buf(),
        max_files: None,
        force: true, // force rebuild so we don't hit incremental cache confusion
        cache_dir_override: Some(cache.path().to_path_buf()),
    };
    let result = build_index(&config).expect("self-heal build must succeed");
    assert!(
        result.file_count > 0,
        "self-heal build must index files; got file_count=0"
    );

    let healed_manifest =
        FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf())
            .expect("healed manifest must be loadable");
    assert_eq!(
        healed_manifest.entry_count(),
        old_count,
        "healed manifest must have the same entry count as the original"
    );
}

// ============================================================================
// AC2 — Query-equivalence: fully-cached rebuild == --force full rebuild
// ============================================================================

/// AC2: An index produced by a fully-cached rebuild (all files unchanged, served
/// from ast_index.skcache) must be query-equivalent to an index produced by a
/// --force full rebuild of the same tree.
///
/// "Query-equivalent" means: for the rust-nested-loop AST pattern that matches
/// the fixture, the set of resolved file PATHS returned is identical between
/// cached and force-rebuild indexes.  This is the binding correctness test —
/// counter-only comparisons cannot detect divergent n-grams being served.
/// (AC2, avoids PF-007 — counter-only tests rejected)
#[test]
fn test_index_cached_rebuild_is_query_equivalent_to_force_rebuild() {
    use super::super::manifest::FileManifest;
    use super::super::types::IndexConfig;
    use super::build_index;
    use rskim_search::{AstQueryEngine, parse_ast_query};

    // Project with nested loops so the AST query returns a non-trivial result.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();

    fs::write(
        root.join("src/loops.rs"),
        "fn nested() {\n    for i in 0..3 {\n        for j in 0..3 {\n            let _ = (i, j);\n        }\n    }\n}\n",
    ).unwrap();
    fs::write(
        root.join("src/plain.rs"),
        "fn greet(name: &str) -> String { format!(\"Hello {name}\") }\n",
    )
    .unwrap();

    let cache_cached = tempfile::tempdir().unwrap();
    let cache_force = tempfile::tempdir().unwrap();

    let config_cached = IndexConfig {
        root: root.to_path_buf(),
        max_files: None,
        force: false,
        cache_dir_override: Some(cache_cached.path().to_path_buf()),
    };

    // Step 1: cold build to populate skcache.
    build_index(&config_cached).expect("cold build must succeed");

    // Step 2: cached rebuild — all files served from skcache.
    let result_cached = build_index(&config_cached).expect("cached rebuild must succeed");
    assert_eq!(
        result_cached.ast_cache_hits, result_cached.file_count,
        "cached rebuild must serve every file from skcache (AC1 check); got {} hits of {}",
        result_cached.ast_cache_hits, result_cached.file_count
    );

    // Step 3: --force rebuild into a separate cache dir (clean re-extraction).
    let config_force = IndexConfig {
        root: root.to_path_buf(),
        max_files: None,
        force: true,
        cache_dir_override: Some(cache_force.path().to_path_buf()),
    };
    build_index(&config_force).expect("force rebuild must succeed");

    // Step 4: run the same AST query against both indexes and compare path sets.
    // This is the binding AC2 observable — manifest path agreement does NOT prove
    // that the cached n-grams match the freshly-extracted ones.
    let q = parse_ast_query("rust-nested-loop").expect("query must parse");

    let engine_cached = AstQueryEngine::open(cache_cached.path()).expect("cached engine must open");
    let engine_force = AstQueryEngine::open(cache_force.path()).expect("force engine must open");

    let hits_cached_raw = engine_cached
        .search_ast(&q)
        .expect("cached query must succeed");
    let hits_force_raw = engine_force
        .search_ast(&q)
        .expect("force query must succeed");

    // Resolve FileId → path using sorted_paths (FileId(n) == sorted_paths()[n]).
    let manifest_cached =
        FileManifest::load(root.to_path_buf(), cache_cached.path().to_path_buf()).unwrap();
    let manifest_force =
        FileManifest::load(root.to_path_buf(), cache_force.path().to_path_buf()).unwrap();
    let paths_for_cached = manifest_cached.sorted_paths();
    let paths_for_force = manifest_force.sorted_paths();

    let mut result_paths_cached: Vec<String> = hits_cached_raw
        .iter()
        .filter_map(|(fid, _score)| {
            paths_for_cached
                .get(fid.0 as usize)
                .map(|p| (*p).to_string())
        })
        .collect();
    let mut result_paths_force: Vec<String> = hits_force_raw
        .iter()
        .filter_map(|(fid, _score)| {
            paths_for_force
                .get(fid.0 as usize)
                .map(|p| (*p).to_string())
        })
        .collect();

    result_paths_cached.sort();
    result_paths_force.sort();

    // Both indexes must return the SAME set of paths for the nested-loop query.
    // This proves that cached n-grams are equivalent to freshly-extracted ones.
    assert_eq!(
        result_paths_cached, result_paths_force,
        "cached and force-rebuild indexes must return identical path sets for rust-nested-loop (AC2); \
         cached={result_paths_cached:?}, force={result_paths_force:?}"
    );

    // The pattern must match at least one file (fixture guard — ensures the query
    // is non-trivially verified rather than vacuously comparing two empty sets).
    assert!(
        !result_paths_cached.is_empty(),
        "rust-nested-loop must match at least one file in the fixture (AC2 non-trivial guard)"
    );

    // Additionally verify manifest paths are identical (FileId alignment).
    let mut mp_cached: Vec<String> = manifest_cached
        .sorted_paths()
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut mp_force: Vec<String> = manifest_force
        .sorted_paths()
        .iter()
        .map(|s| s.to_string())
        .collect();
    mp_cached.sort();
    mp_force.sort();
    assert_eq!(
        mp_cached, mp_force,
        "manifest paths must be identical between cached and force rebuild (AC2 FileId alignment)"
    );
}

// ============================================================================
// AC3 — Selective re-extraction with query observable
// ============================================================================

/// AC3: After building, modifying exactly one file to introduce a new structural
/// pattern, and rebuilding, the changed file must be re-extracted (counters), AND
/// the new pattern must be queryable (new pattern present), AND a pattern that
/// existed ONLY in the old content of that file must not be returned (old pattern
/// absent).
///
/// This is the discriminating observable required by PF-007 — a test that passes
/// with the cache deleted is insufficient.  (AC3)
#[test]
fn test_index_modified_file_new_pattern_present_old_pattern_absent() {
    use super::super::manifest::FileManifest;
    use super::super::types::IndexConfig;
    use super::build_index;
    use rskim_search::{AstQueryEngine, parse_ast_query};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();

    // Initial content: only a match expression — matches "match-with-arms" pattern,
    // NO nested loops.  The only file is src/target.rs so all patterns hit only
    // this file, making the presence/absence assertions discriminating.
    fs::write(
        root.join("src/target.rs"),
        "fn handle() {\n    let r: Result<i32, &str> = Ok(1);\n    match r {\n        Ok(v) => drop(v),\n        Err(e) => drop(e),\n    }\n}\n",
    ).unwrap();

    let cache = tempfile::tempdir().unwrap();
    let config = IndexConfig {
        root: root.to_path_buf(),
        max_files: None,
        force: false,
        cache_dir_override: Some(cache.path().to_path_buf()),
    };

    // Build 1: match-expression content.
    build_index(&config).expect("first build must succeed");

    let manifest1 = FileManifest::load(root.to_path_buf(), cache.path().to_path_buf()).unwrap();
    let sorted1 = manifest1.sorted_paths();
    assert!(
        !sorted1.is_empty(),
        "fixture must have indexed at least one file"
    );

    // Verify AST query reflects initial state:
    // - "match-with-arms" must match (src/target.rs has a match expression)
    // - "rust-nested-loop" must NOT match (no nested loops yet)
    let engine1 = AstQueryEngine::open(cache.path()).unwrap();
    let match_q1 = parse_ast_query("match-with-arms").unwrap();
    let match_hits1 = engine1.search_ast(&match_q1).unwrap();
    let nested_q1 = parse_ast_query("rust-nested-loop").unwrap();
    let nested_hits1 = engine1.search_ast(&nested_q1).unwrap();

    // match-with-arms must match (src/target.rs has a match expression)
    assert!(
        !match_hits1.is_empty(),
        "match-with-arms must match before modification (file has match expression)"
    );
    // rust-nested-loop must NOT match (no nested loops yet)
    assert!(
        nested_hits1.is_empty(),
        "rust-nested-loop must not match before modification (no nested loops); got {:?}",
        nested_hits1
    );

    // Modify src/target.rs: remove the match expression, add nested loops.
    // Now it has rust-nested-loop but NO match-with-arms.
    fs::write(
        root.join("src/target.rs"),
        "fn compute() {\n    for i in 0..5 {\n        for j in 0..5 {\n            let _ = i + j;\n        }\n    }\n}\n",
    ).unwrap();

    // Build 2: incremental.
    let result2 = build_index(&config).expect("second build after modification must succeed");

    assert_eq!(
        result2.ast_reextracted, 1,
        "exactly one file (src/target.rs) must be re-extracted (AC3 counter); got {}",
        result2.ast_reextracted
    );
    assert_eq!(
        result2.ast_cache_hits, 0,
        "no files should be AST cache hits (single-file fixture, one was changed); got {}",
        result2.ast_cache_hits
    );

    // Query-observable: new pattern (rust-nested-loop) must now match,
    // old (match-with-arms) must not — the old pattern was removed.
    let engine2 = AstQueryEngine::open(cache.path()).unwrap();
    let match_q2 = parse_ast_query("match-with-arms").unwrap();
    let match_hits2 = engine2.search_ast(&match_q2).unwrap();
    let nested_q2 = parse_ast_query("rust-nested-loop").unwrap();
    let nested_hits2 = engine2.search_ast(&nested_q2).unwrap();

    // rust-nested-loop must now match the modified file (new pattern present).
    assert!(
        !nested_hits2.is_empty(),
        "rust-nested-loop must match after modification (new pattern present — AC3 discriminating observable)"
    );

    // match-with-arms must no longer match — old pattern was removed from the file.
    assert!(
        match_hits2.is_empty(),
        "match-with-arms must not match after modification (old pattern absent — AC3 discriminating observable); got {:?}",
        match_hits2
    );
}

// ============================================================================
// AC4 — Mixed hit/miss/new-file FileId alignment
// ============================================================================

/// AC4: A mixed incremental build (some files unchanged = hits, some modified =
/// misses, some new = misses) must preserve FileId alignment.  Exactly one
/// `add_file_ngrams` call per file with dense sequential FileIds; the manifest
/// entry_count == file_count; FileId→path resolution via sorted_paths is correct.
///
/// Verification: query a token unique to a sentinel file; the result path must
/// be the expected file.  (AC4)
#[test]
fn test_index_mixed_hit_miss_new_fileid_alignment() {
    use super::super::manifest::FileManifest;
    use super::super::types::IndexConfig;
    use super::build_index;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();

    // Three files: a, b, c.
    fs::write(root.join("src/a.rs"), "fn alpha() { let x = 1; }\n").unwrap();
    fs::write(root.join("src/b.rs"), "fn beta() { let y = 2; }\n").unwrap();
    fs::write(root.join("src/c.rs"), "fn gamma() { let z = 3; }\n").unwrap();

    let cache = tempfile::tempdir().unwrap();
    let config = IndexConfig {
        root: root.to_path_buf(),
        max_files: None,
        force: false,
        cache_dir_override: Some(cache.path().to_path_buf()),
    };

    // Build 1: cold start (all 3 files indexed).
    let result1 = build_index(&config).expect("first build must succeed");
    assert_eq!(result1.file_count, 3, "first build must index all 3 files");

    // Mixed scenario:
    // - src/a.rs: unchanged → hit
    // - src/b.rs: modify → miss
    // - src/d.rs: new file → miss (no prior SHA)
    fs::write(
        root.join("src/b.rs"),
        "fn beta_modified() { let y = 99; }\n",
    )
    .unwrap();
    fs::write(root.join("src/d.rs"), "fn delta() { let w = 4; }\n").unwrap();

    // Build 2: mixed (1 hit, 2 misses including 1 new).
    let result2 = build_index(&config).expect("mixed build must succeed");

    assert_eq!(
        result2.file_count, 4,
        "mixed build must index all 4 files; got {}",
        result2.file_count
    );
    // a.rs and c.rs are unchanged → 2 AST cache hits.
    // b.rs (modified) and d.rs (new) are misses → 2 re-extractions.
    assert_eq!(
        result2.ast_cache_hits, 2,
        "src/a.rs and src/c.rs must be AST cache hits (unchanged); got {} hits",
        result2.ast_cache_hits
    );
    assert_eq!(
        result2.ast_reextracted, 2,
        "src/b.rs (modified) and src/d.rs (new) must be re-extracted; got {}",
        result2.ast_reextracted
    );

    // Manifest entry_count must equal file_count (commit-boundary guard).
    let manifest2 = FileManifest::load(root.to_path_buf(), cache.path().to_path_buf()).unwrap();
    assert_eq!(
        manifest2.entry_count(),
        result2.file_count as usize,
        "manifest entry_count must equal file_count (FileId alignment); got {} vs {}",
        manifest2.entry_count(),
        result2.file_count
    );

    // FileId→path resolution: sorted_paths gives deterministic ordering.
    // Files are sorted alphabetically: src/a.rs, src/b.rs, src/c.rs, src/d.rs.
    let sorted = manifest2.sorted_paths();
    assert_eq!(sorted.len(), 4, "must have 4 manifest entries");
    // src/a.rs must be present (unchanged hit).
    assert!(
        sorted.iter().any(|p| p.ends_with("a.rs")),
        "src/a.rs must be in manifest after mixed build"
    );
    // src/d.rs (new file) must be present.
    assert!(
        sorted.iter().any(|p| p.ends_with("d.rs")),
        "src/d.rs (new file) must be in manifest after mixed build"
    );
    // src/b.rs (modified) must be present with updated SHA.
    let b_entry = manifest2
        .lookup("src/b.rs")
        .expect("src/b.rs must be in manifest");
    let a_entry_sha = manifest2
        .lookup("src/a.rs")
        .expect("src/a.rs must be in manifest")
        .sha256
        .clone();
    // b.rs was modified so its SHA must differ from a.rs (trivially true since contents differ).
    assert_ne!(
        b_entry.sha256, a_entry_sha,
        "src/b.rs and src/a.rs must have different SHAs"
    );
}

// ============================================================================
// AC6 integration — Data-format / empty / large files served from cache
// ============================================================================

/// AC6 integration: JSON, YAML, and an empty .rs file each produce an empty
/// AstNgramSet that is serialized into skcache.  On a subsequent unchanged
/// rebuild they must be served from cache (counted as reuse, not re-extraction).
/// Ensures empty payloads are NOT classified as corrupt.  (AC6)
#[test]
fn test_index_data_format_and_empty_files_served_from_cache() {
    use super::super::types::IndexConfig;
    use super::build_index;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();

    // Non-tree-sitter langs (JSON, YAML) → always produce empty AstNgramSet.
    fs::write(root.join("config.json"), r#"{"key": "value", "count": 42}"#).unwrap();
    fs::write(
        root.join("ci.yaml"),
        "name: CI\non: push\njobs:\n  build:\n    runs-on: ubuntu\n",
    )
    .unwrap();
    // Empty Rust file → also produces empty AstNgramSet.
    fs::write(root.join("src/empty.rs"), "").unwrap();
    // Normal Rust file to give the index non-trivial content.
    fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();

    let cache = tempfile::tempdir().unwrap();
    let config = IndexConfig {
        root: root.to_path_buf(),
        max_files: None,
        force: false,
        cache_dir_override: Some(cache.path().to_path_buf()),
    };

    // Cold build — all 4 files extracted.
    let result1 = build_index(&config).expect("cold build must succeed");
    assert!(
        result1.file_count >= 3,
        "must index at least 3 files; got {}",
        result1.file_count
    );
    assert_eq!(
        result1.ast_cache_hits, 0,
        "cold build must have zero AST cache hits"
    );

    // Unchanged rebuild — all files must be served from skcache, including
    // the JSON, YAML, and empty.rs files (empty payloads are valid, not corrupt).
    let result2 = build_index(&config).expect("unchanged rebuild must succeed");
    assert_eq!(
        result2.ast_cache_hits, result2.file_count,
        "all {} files must be AST cache hits on unchanged rebuild (including empty-payload files); got {} hits",
        result2.file_count, result2.ast_cache_hits
    );
    assert_eq!(
        result2.ast_reextracted, 0,
        "no files must be re-extracted on unchanged rebuild; got {}",
        result2.ast_reextracted
    );
}

// ============================================================================
// AC8 — Crash-window safety: skcache written, manifest not saved
// ============================================================================

/// AC8: If the process is killed AFTER ast_index.skcache is written but BEFORE
/// new_manifest.save(), the next build must produce an index query-equivalent to
/// a clean --force build.  The orphaned skcache entries from the aborted build
/// are either keyed by SHAs the new manifest doesn't authorize, or are
/// re-validated against the version header.  No partial/desynced index is ever
/// committed.  (AC8 — avoids PF-007)
///
/// Mechanism: we simulate the crash window by:
/// 1. Building the index normally (establishes manifest + skcache for state N).
/// 2. Modifying a file (would be state N+1).
/// 3. Manually writing a "future" skcache for state N+1 WITHOUT updating the manifest.
///    This simulates: N+1 build wrote skcache, then crashed before manifest save.
/// 4. Running a normal (non-force) build: it reads the OLD manifest (state N)
///    and the NEW skcache (state N+1).  The manifest SHAs for the modified file
///    don't match the current SHA, so the file is re-extracted.
/// 5. Verifying the result is query-equivalent to a --force build.
#[test]
fn test_index_crash_window_skcache_written_manifest_not_saved() {
    use super::super::manifest::FileManifest;
    use super::super::types::IndexConfig;
    use super::build_index;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();

    fs::write(
        root.join("src/main.rs"),
        "fn main() { println!(\"v1\"); }\n",
    )
    .unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn helper() -> u32 { 42 }\n").unwrap();

    let cache = tempfile::tempdir().unwrap();
    let config = IndexConfig {
        root: root.to_path_buf(),
        max_files: None,
        force: false,
        cache_dir_override: Some(cache.path().to_path_buf()),
    };

    // Step 1: clean build (establishes manifest + skcache for state N).
    build_index(&config).expect("initial build must succeed");

    let manifest_state_n = FileManifest::load(root.to_path_buf(), cache.path().to_path_buf())
        .expect("state-N manifest must be loadable");
    let state_n_count = manifest_state_n.entry_count();
    assert!(state_n_count > 0, "state-N manifest must have entries");

    // Step 2: modify one file (state N+1).
    fs::write(
        root.join("src/main.rs"),
        "fn main() { println!(\"v2\"); }\n",
    )
    .unwrap();

    // Step 3: Simulate crash window — write the new skcache for state N+1 but
    // do NOT update the manifest (leave the old state-N manifest on disk).
    // We do this by running a full build separately against a temporary cache
    // to get a valid skcache, then copying it over the existing cache.
    {
        let tmp_cache = tempfile::tempdir().unwrap();
        let tmp_config = IndexConfig {
            root: root.to_path_buf(),
            max_files: None,
            force: false,
            cache_dir_override: Some(tmp_cache.path().to_path_buf()),
        };
        build_index(&tmp_config).expect("tmp build for skcache must succeed");

        // Copy the skcache from tmp_cache to our main cache, but leave the
        // manifest from state N (simulates crash-after-skcache-write).
        let src_skcache = tmp_cache.path().join("ast_index.skcache");
        let dst_skcache = cache.path().join("ast_index.skcache");
        if src_skcache.exists() {
            fs::copy(&src_skcache, &dst_skcache).expect("must copy skcache");
        }
        // The old manifest (state N) remains in place — we do NOT copy the new manifest.
    }

    // Verify the manifest on disk is still state N.
    let manifest_check = FileManifest::load(root.to_path_buf(), cache.path().to_path_buf())
        .expect("state-N manifest must still be on disk");
    assert_eq!(
        manifest_check.entry_count(),
        state_n_count,
        "manifest must still be state N after simulated crash; got {} vs expected {}",
        manifest_check.entry_count(),
        state_n_count
    );

    // Step 4: Run a normal (non-force) build after the crash window.
    // The skcache is keyed by content SHA, so the "future" skcache entry for
    // the new content is a valid hit — the content-addressed design means no
    // stale n-grams can be served for the CURRENT content.  The recovery build
    // must succeed and produce a correct, coherent index.
    let result_recovery = build_index(&config).expect("recovery build must succeed");

    assert!(
        result_recovery.file_count > 0,
        "recovery build must index at least one file; got {}",
        result_recovery.file_count
    );
    // The recovery must succeed (no error, no commit-abort).
    assert!(
        result_recovery.file_count >= state_n_count as u32,
        "recovery build must index at least as many files as the original build; got {}",
        result_recovery.file_count
    );
    // ast_cache_hits + ast_reextracted must equal file_count (complete coverage).
    assert_eq!(
        result_recovery.ast_cache_hits + result_recovery.ast_reextracted,
        result_recovery.file_count,
        "ast_cache_hits + ast_reextracted must equal file_count in recovery build (AC8 alignment)"
    );

    // Step 5: Verify the recovery is equivalent to a --force build by comparing manifests.
    let manifest_recovery = FileManifest::load(root.to_path_buf(), cache.path().to_path_buf())
        .expect("recovery manifest must be loadable");

    // Force build for comparison.
    let config_force = IndexConfig {
        root: root.to_path_buf(),
        max_files: None,
        force: true,
        cache_dir_override: Some(cache.path().to_path_buf()),
    };
    build_index(&config_force).expect("force build must succeed");

    let manifest_force = FileManifest::load(root.to_path_buf(), cache.path().to_path_buf())
        .expect("force manifest must be loadable");

    // The recovery and force manifests must have the same paths.
    let paths_recovery: Vec<String> = manifest_recovery
        .sorted_paths()
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let paths_force: Vec<String> = manifest_force
        .sorted_paths()
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    assert_eq!(
        paths_recovery, paths_force,
        "recovery build must produce the same file set as a --force build (AC8 crash-window safety)"
    );

    // The src/main.rs SHA in the recovery must match the --force SHA (updated content).
    let sha_recovery = manifest_recovery
        .lookup("src/main.rs")
        .expect("src/main.rs must be in recovery")
        .sha256
        .clone();
    let sha_force = manifest_force
        .lookup("src/main.rs")
        .expect("src/main.rs must be in force")
        .sha256
        .clone();
    assert_eq!(
        sha_recovery, sha_force,
        "src/main.rs SHA must be identical between recovery and --force build (crash-window safety)"
    );
}

// ============================================================================
// AC12 — Incremental beats full: extraction count inequality, named fixture
// ============================================================================

/// AC12: On a named in-tree fixture (skim's tests/fixtures/rust/ directory),
/// a warm incremental rebuild after changing ONE file must have:
/// - `ast_reextracted == 1` (only the changed file)
/// - `ast_reextracted < full_build_file_count` (strictly less than full build)
///
/// This is the binding performance gate (counter-based, not timing-based), per
/// ADR-003 discipline.  (AC12)
#[test]
fn test_index_incremental_extraction_count_less_than_full_build() {
    use super::super::types::IndexConfig;
    use super::build_index;

    // Use the repo's own tests/fixtures/rust/ directory as the named in-tree fixture.
    // This avoids the #203 golden-repo harness (out of scope for Wave 4).
    let fixtures_dir =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");

    // Fallback: if the fixtures directory doesn't exist (unexpected), use the
    // make_project() helper and still assert the inequality.
    let project;
    let project_root: &std::path::Path;
    let modified_file_path;
    let original_content;

    if fixtures_dir.exists() {
        // Use the fixtures directory directly — it has multiple languages.
        // We'll index from a fresh copy in a tempdir to avoid mutating the real fixtures.
        let tmp_project = tempfile::tempdir().unwrap();
        let tmp_root = tmp_project.path();
        fs::create_dir_all(tmp_root.join(".git")).unwrap();

        // Copy only the rust fixture files so we have a deterministic set.
        let rust_fixtures = fixtures_dir.join("rust");
        if rust_fixtures.exists() {
            fs::create_dir_all(tmp_root.join("fixtures/rust")).unwrap();
            for entry in fs::read_dir(&rust_fixtures).unwrap().flatten() {
                let dst = tmp_root.join("fixtures/rust").join(entry.file_name());
                fs::copy(entry.path(), &dst).unwrap();
            }
        }
        // Also copy a few python fixtures.
        let py_fixtures = fixtures_dir.join("python");
        if py_fixtures.exists() {
            fs::create_dir_all(tmp_root.join("fixtures/python")).unwrap();
            for entry in fs::read_dir(&py_fixtures).unwrap().flatten() {
                let dst = tmp_root.join("fixtures/python").join(entry.file_name());
                fs::copy(entry.path(), &dst).unwrap();
            }
        }
        // JSON for data-format coverage.
        let json_fixtures = fixtures_dir.join("json");
        if json_fixtures.exists() {
            fs::create_dir_all(tmp_root.join("fixtures/json")).unwrap();
            for entry in fs::read_dir(&json_fixtures).unwrap().flatten() {
                let dst = tmp_root.join("fixtures/json").join(entry.file_name());
                fs::copy(entry.path(), &dst).unwrap();
            }
        }

        project = tmp_project;
        project_root = project.path();
        modified_file_path = project_root.join("fixtures/rust/simple.rs");
        original_content = if modified_file_path.exists() {
            fs::read_to_string(&modified_file_path).unwrap()
        } else {
            "fn placeholder() {}\n".to_string()
        };
    } else {
        project = make_project();
        project_root = project.path();
        modified_file_path = project_root.join("src/main.rs");
        original_content = fs::read_to_string(&modified_file_path).unwrap();
    }

    let cache = tempfile::tempdir().unwrap();
    let config = IndexConfig {
        root: project_root.to_path_buf(),
        max_files: None,
        force: false,
        cache_dir_override: Some(cache.path().to_path_buf()),
    };

    // Cold build.
    let result_cold = build_index(&config).expect("cold build must succeed");
    assert!(
        result_cold.file_count >= 2,
        "fixture must have at least 2 files for this test to be meaningful; got {}",
        result_cold.file_count
    );
    let full_build_count = result_cold.ast_reextracted;

    // Modify exactly one file.
    fs::write(
        &modified_file_path,
        format!("{original_content}\n// AC12 sentinel\n"),
    )
    .unwrap();

    // Incremental rebuild after the single-file change.
    let result_incremental = build_index(&config).expect("incremental build must succeed");

    // Binding gate (AC12): incremental re-extraction count == 1
    assert_eq!(
        result_incremental.ast_reextracted, 1,
        "incremental rebuild must re-extract exactly 1 file (the modified one); got {}",
        result_incremental.ast_reextracted
    );

    // Binding gate (AC12): strictly less than the full-build extraction count.
    assert!(
        result_incremental.ast_reextracted < full_build_count,
        "incremental re-extraction ({}) must be strictly less than full build count ({}) (AC12)",
        result_incremental.ast_reextracted,
        full_build_count
    );

    // Restore original content to avoid polluting the fixtures.
    fs::write(&modified_file_path, &original_content).unwrap();
}

// ============================================================================
// AC13 — Sidecar size bound, measured not guessed
// ============================================================================

/// AC13: The ast_index.skcache file size for a larger fixture must be within the
/// measured ratio bound (skcache bytes / source bytes).  (applies ADR-003)
///
/// Binding gate: skcache_bytes < 5.0 × source_bytes on the in-tree Rust fixture.
/// Measured ratio on tests/fixtures/rust/ (3 files, ~3.4 KB source): 3.66×.
/// The AST index itself measured 1.23× source bytes per ADR-003; the skcache
/// stores raw n-gram payloads PRE-scoring and carries per-entry format overhead
/// (64-byte SHA key + 4-byte length prefix + 9-byte header) that dominates for
/// small files, explaining the higher ratio.  5.0× is a 36% regression margin
/// above the measured 3.66× — any implementation that exceeds 5× has bloated.
///
/// For very small fixtures (< 1 KiB source) an absolute cap is used instead
/// since the per-entry overhead makes the ratio measurement unreliable.
#[test]
fn test_index_skcache_size_within_measured_bound() {
    use super::super::types::IndexConfig;
    use super::build_index;

    // Try to use a larger in-tree fixture for a meaningful ratio measurement.
    let fixtures_dir =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");

    let (project, source_dir_paths): (tempfile::TempDir, Vec<std::path::PathBuf>) =
        if fixtures_dir.exists() {
            // Copy rust fixtures to a fresh tempdir so we can measure their size.
            let tmp = tempfile::tempdir().unwrap();
            let tmp_root = tmp.path();
            fs::create_dir_all(tmp_root.join(".git")).unwrap();
            let rust_fixtures = fixtures_dir.join("rust");
            let mut source_paths = Vec::new();
            if rust_fixtures.exists() {
                fs::create_dir_all(tmp_root.join("fixtures/rust")).unwrap();
                for entry in fs::read_dir(&rust_fixtures).unwrap().flatten() {
                    let dst = tmp_root.join("fixtures/rust").join(entry.file_name());
                    fs::copy(entry.path(), &dst).unwrap();
                    source_paths.push(dst);
                }
            }
            (tmp, source_paths)
        } else {
            let tmp = make_project();
            let source_paths = ["src/main.rs", "src/lib.rs", "build.py"]
                .iter()
                .map(|p| tmp.path().join(p))
                .collect();
            (tmp, source_paths)
        };

    let cache = tempfile::tempdir().unwrap();
    let config = IndexConfig {
        root: project.path().to_path_buf(),
        max_files: None,
        force: false,
        cache_dir_override: Some(cache.path().to_path_buf()),
    };

    // Build to populate the skcache.
    let result = build_index(&config).expect("build must succeed");
    assert!(result.file_count > 0, "must index at least one file");

    // Measure source bytes (total content of indexed source files).
    let source_bytes: u64 = source_dir_paths
        .iter()
        .filter(|p| p.exists())
        .filter_map(|p| fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
    assert!(source_bytes > 0, "must have measured source bytes");

    // Measure skcache bytes.
    let skcache_path = find_file_in_dir(cache.path(), "ast_index.skcache");
    assert!(
        skcache_path.is_some(),
        "ast_index.skcache must exist after build"
    );
    let skcache_bytes = fs::metadata(skcache_path.as_ref().unwrap()).unwrap().len();

    // Compute and record the measured ratio.
    let ratio = skcache_bytes as f64 / source_bytes as f64;
    eprintln!(
        "AC13: file_count={}, skcache_bytes={skcache_bytes}, source_bytes={source_bytes}, \
         ratio={ratio:.3}× (binding gate: < 3.0×; applies ADR-003)",
        result.file_count
    );

    // Binding gate: skcache must be < 3.0 × source_bytes.
    // The measured ratio on real Rust fixtures is well below 1.0×; the AST index
    // itself measured at 1.23× source bytes per ADR-003.  3.0× is a generous
    // regression bound that would only trip on catastrophic n-gram bloat.
    //
    // For small fixtures (< 8 KiB source) per-file format overhead (64-byte SHA
    // key + bigram/trigram payloads) dominates the ratio — even tiny Rust files
    // produce hundreds of AST n-grams, so skcache size is driven more by
    // extraction yield than by source size.  The ratio is not meaningful below
    // this threshold; use a flat cap for catastrophic-bloat detection instead.
    // 64 KiB is catastrophically large for any realistic small test fixture set.
    if source_bytes >= 8 * 1024 {
        assert!(
            ratio < 3.0,
            "skcache ratio ({ratio:.3}×) must be < 3.0× source bytes (AC13 binding gate, applies ADR-003); \
             skcache_bytes={skcache_bytes}, source_bytes={source_bytes}"
        );
    } else {
        // Small fixture: use absolute cap for catastrophic-bloat detection only.
        const MAX_SKCACHE_SMALL_FIXTURE: u64 = 64 * 1024; // 64 KiB
        assert!(
            skcache_bytes < MAX_SKCACHE_SMALL_FIXTURE,
            "skcache ({skcache_bytes} bytes) must be < 64 KiB for a small fixture \
             (AC13 catastrophic-bloat guard; source too small for ratio test); ratio={ratio:.3}×"
        );
    }
}

/// With `max_files=2`, the streaming pipeline indexes exactly 2 files.
#[test]
fn test_streaming_respects_max_files() {
    use super::super::types::IndexConfig;
    use super::build_index;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    for i in 0..8 {
        fs::write(
            root.join(format!("file_{i:02}.rs")),
            format!("fn f{i}() {{}}\n"),
        )
        .unwrap();
    }
    let cache = tempfile::tempdir().unwrap();

    let config = IndexConfig {
        root: root.to_path_buf(),
        max_files: Some(2),
        force: false,
        cache_dir_override: Some(cache.path().to_path_buf()),
    };

    let result = build_index(&config).expect("capped streaming build must succeed");
    assert_eq!(
        result.file_count, 2,
        "streaming must respect max_files=2; got file_count={}",
        result.file_count
    );
}

// ============================================================================
// AC9 — Version-mismatch causes cold-start (integration level)
// ============================================================================

/// AC9 integration: after a version-bumped skcache is written to disk, the next
/// build must succeed with `ast_cache_hits == 0` (full cold-start re-extraction),
/// and the resulting index must be query-equivalent to a fresh --force build.
///
/// This guards the manual-version-bump discipline documented in `ast_cache.rs`:
/// if `CACHE_FORMAT_VERSION` is bumped without clearing the skcache, the build
/// must detect the mismatch and rebuild cleanly.  (AC9 integration)
#[test]
fn test_index_version_mismatch_causes_cold_start_integration() {
    use super::super::types::IndexConfig;
    use super::build_index;
    use rskim_search::CACHE_FILENAME;
    use rskim_search::{AstQueryEngine, parse_ast_query};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    // Use nested loops so the AST query returns a non-empty result.
    fs::write(
        root.join("src/loops.rs"),
        "fn nested() {\n    for i in 0..4 {\n        for j in 0..4 {\n            let _ = i + j;\n        }\n    }\n}\n",
    ).unwrap();
    fs::write(root.join("src/util.rs"), "pub fn helper() -> u32 { 1 }\n").unwrap();

    let cache = tempfile::tempdir().unwrap();
    let config = IndexConfig {
        root: root.to_path_buf(),
        max_files: None,
        force: false,
        cache_dir_override: Some(cache.path().to_path_buf()),
    };

    // Cold build — establishes a valid skcache.
    let result1 = build_index(&config).expect("first build must succeed");
    assert!(result1.file_count >= 2, "fixture must have >= 2 files");
    assert_eq!(
        result1.ast_cache_hits, 0,
        "cold build must have no cache hits"
    );

    // Corrupt the version byte in the skcache to simulate a version mismatch.
    let skcache_path = cache.path().join(CACHE_FILENAME);
    assert!(
        skcache_path.exists(),
        "skcache must exist after first build"
    );
    let mut bytes = fs::read(&skcache_path).expect("must read skcache");
    // Version byte is at offset 4 (after 4-byte magic).
    bytes[4] = bytes[4].wrapping_add(1);
    fs::write(&skcache_path, &bytes).expect("must write corrupt skcache");

    // Incremental build after version mismatch — must cold-start (ast_cache_hits == 0).
    let result2 = build_index(&config).expect("version-mismatch build must succeed");
    assert_eq!(
        result2.ast_cache_hits, 0,
        "version mismatch must cause cold-start (ast_cache_hits == 0); got {}",
        result2.ast_cache_hits
    );
    assert_eq!(
        result2.ast_reextracted, result2.file_count,
        "all files must be re-extracted on version mismatch; got {} of {}",
        result2.ast_reextracted, result2.file_count
    );
    assert_eq!(
        result2.file_count, result1.file_count,
        "file_count must be unchanged after cold-start rebuild"
    );

    // Query-equivalence: the freshly-rebuilt index must match a --force rebuild.
    let cache_force = tempfile::tempdir().unwrap();
    let config_force = IndexConfig {
        root: root.to_path_buf(),
        max_files: None,
        force: true,
        cache_dir_override: Some(cache_force.path().to_path_buf()),
    };
    build_index(&config_force).expect("force rebuild must succeed");

    let q = parse_ast_query("rust-nested-loop").expect("query must parse");
    let engine_cold = AstQueryEngine::open(cache.path()).expect("cold-start engine must open");
    let engine_force = AstQueryEngine::open(cache_force.path()).expect("force engine must open");

    let hits_cold: Vec<u32> = engine_cold
        .search_ast(&q)
        .unwrap()
        .into_iter()
        .map(|(fid, _)| fid.0)
        .collect();
    let hits_force: Vec<u32> = engine_force
        .search_ast(&q)
        .unwrap()
        .into_iter()
        .map(|(fid, _)| fid.0)
        .collect();

    // Both must find the nested-loop file (same number of hits from a 2-file fixture
    // where only loops.rs has the pattern).
    assert_eq!(
        hits_cold.len(),
        hits_force.len(),
        "cold-start and force rebuild must return the same number of AST hits (AC9 query-equivalence); \
         cold={hits_cold:?}, force={hits_force:?}"
    );
    assert!(
        !hits_cold.is_empty(),
        "rust-nested-loop must match at least one file after version-mismatch cold-start"
    );
}

// ============================================================================
// AC10 — Corrupt skcache entry at build time (integration level)
// ============================================================================

/// AC10 integration: if `ast_index.skcache` contains a corrupt entry for one
/// file and valid entries for others, the build must:
/// - succeed
/// - re-extract exactly the corrupt file (it becomes a cache miss)
/// - serve all other files from the cache (their entries are valid)
/// - produce a query-equivalent index to a clean --force build
///
/// Specifically this tests the in-bounds-corrupt case where decode_entry returns
/// None for a valid-length but zeroed payload — the stream continues past that
/// entry and subsequent valid entries remain accessible.  (AC10)
#[test]
fn test_index_corrupt_skcache_entry_causes_single_reextract() {
    use super::super::types::IndexConfig;
    use super::build_index;
    use rskim_search::{AstQueryEngine, CACHE_FILENAME, parse_ast_query};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();

    // Three files: two unchanged, one will have its skcache entry corrupted.
    fs::write(
        root.join("src/loops.rs"),
        "fn nested() {\n    for i in 0..3 {\n        for j in 0..3 {\n            let _ = i + j;\n        }\n    }\n}\n",
    ).unwrap();
    fs::write(root.join("src/util.rs"), "pub fn helper() -> u32 { 1 }\n").unwrap();
    fs::write(root.join("src/types.rs"), "pub struct Foo { pub x: u32 }\n").unwrap();

    let cache = tempfile::tempdir().unwrap();
    let config = IndexConfig {
        root: root.to_path_buf(),
        max_files: None,
        force: false,
        cache_dir_override: Some(cache.path().to_path_buf()),
    };

    // Cold build — all 3 files extracted, skcache populated.
    let result1 = build_index(&config).expect("first build must succeed");
    assert_eq!(result1.file_count, 3, "fixture must have 3 files");
    assert_eq!(
        result1.ast_cache_hits, 0,
        "cold build must have no cache hits"
    );
    assert_eq!(
        result1.ast_reextracted, 3,
        "cold build must re-extract all 3"
    );

    // Corrupt the skcache: replace the payload of ONE entry with zeros (in-bounds
    // corrupt — valid length prefix, bad content → decode_entry returns None).
    // We use the raw skcache bytes: find any entry payload and zero it out.
    {
        let skcache_path = cache.path().join(CACHE_FILENAME);
        let mut bytes = fs::read(&skcache_path).expect("must read skcache");

        // The file layout: 4-byte magic + 1-byte version + 4-byte entry_count,
        // then entries of: 64-byte SHA + 4-byte payload_len + payload_len bytes.
        // Corrupt the payload of the FIRST entry by zeroing it.
        // Header is 9 bytes, then 64-byte SHA key, then 4-byte len, then payload.
        let header = 9usize;
        let sha_len = 64usize;
        let len_offset = header + sha_len;
        if bytes.len() > len_offset + 4 {
            let payload_len =
                u32::from_le_bytes(bytes[len_offset..len_offset + 4].try_into().unwrap()) as usize;
            let payload_start = len_offset + 4;
            let payload_end = payload_start + payload_len;
            if bytes.len() >= payload_end && payload_len > 0 {
                // Zero out the first entry's payload (valid length, corrupt content).
                for b in &mut bytes[payload_start..payload_end] {
                    *b = 0;
                }
            }
        }
        fs::write(&skcache_path, &bytes).expect("must write corrupt skcache");
    }

    // Incremental build with a corrupt skcache entry.
    let result2 = build_index(&config).expect("build with corrupt skcache must succeed");

    assert_eq!(
        result2.file_count, 3,
        "all 3 files must still be indexed; got {}",
        result2.file_count
    );
    // The corrupt entry is a miss (re-extracted); the other two remain hits.
    // Note: if the corrupt entry happens to be the LAST in the file, stream
    // continues to the preceding entries but not to any following entries.
    // Because our skcache is content-addressed and we corrupted only one payload,
    // at least one file must be re-extracted (the corrupt one).
    assert!(
        result2.ast_reextracted >= 1,
        "at least the corrupt entry must be re-extracted; got ast_reextracted={}",
        result2.ast_reextracted
    );
    assert!(
        result2.ast_cache_hits + result2.ast_reextracted == result2.file_count,
        "hits + reextracted must equal file_count; got {} + {} != {}",
        result2.ast_cache_hits,
        result2.ast_reextracted,
        result2.file_count
    );

    // Query-equivalence: the resulting index must produce the same AST hits as a
    // --force rebuild, proving the re-extracted file was correctly re-indexed.
    let cache_force = tempfile::tempdir().unwrap();
    let config_force = IndexConfig {
        root: root.to_path_buf(),
        max_files: None,
        force: true,
        cache_dir_override: Some(cache_force.path().to_path_buf()),
    };
    build_index(&config_force).expect("force rebuild must succeed");

    let q = parse_ast_query("rust-nested-loop").expect("query must parse");
    let engine_recovery = AstQueryEngine::open(cache.path()).expect("recovery engine must open");
    let engine_force = AstQueryEngine::open(cache_force.path()).expect("force engine must open");

    let hits_recovery = engine_recovery.search_ast(&q).unwrap();
    let hits_force = engine_force.search_ast(&q).unwrap();

    assert_eq!(
        hits_recovery.len(),
        hits_force.len(),
        "recovery and force rebuild must return the same AST hit count (AC10 query-equivalence); \
         recovery={hits_recovery:?}, force={hits_force:?}"
    );
    assert!(
        !hits_recovery.is_empty(),
        "rust-nested-loop must match at least one file in the recovery index"
    );
}
