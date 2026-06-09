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
    use rskim_search::{AstIndexBuilder, AstNgramSet, FileId, NgramIndexBuilder, StructuralMetrics};

    use super::Pipeline;
    use super::super::manifest::FileManifest;
    use super::super::types::ProcessedFile;

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

    let old_manifest =
        FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf())
            .expect("old manifest must be loadable");
    let old_entry_count = old_manifest.entry_count();
    assert!(old_entry_count > 0, "old manifest must have entries for the test to be meaningful");

    // Stage 2: set up a consume call with a PRE-BROKEN AstIndexBuilder.
    // Pre-advancing it by one FileId forces it to expect FileId(1) as the next
    // call, so when consume tries FileId(0) the builder returns the desync error.
    let mut lexical_builder = NgramIndexBuilder::new(cache.path().to_path_buf())
        .expect("lexical builder must initialise");
    let mut ast_builder = AstIndexBuilder::new(cache.path().to_path_buf())
        .expect("AST builder must initialise");

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
    };
    tx.send(pf).unwrap();
    drop(tx); // close channel so consume loop terminates after one item

    // Stage 3: call consume — it must return Err because add_file_ngrams rejects
    // FileId(0) (the builder already has FileId(0) and expects FileId(1) next).
    let result = Pipeline::consume(
        &mut lexical_builder,
        &mut ast_builder,
        &mut new_manifest,
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
    let reloaded =
        FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf())
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
    use rskim_search::{AstIndexBuilder, AstNgramSet, FileId, NgramIndexBuilder, StructuralMetrics};

    use super::Pipeline;
    use super::super::manifest::FileManifest;
    use super::super::types::ProcessedFile;
    use super::super::types::IndexConfig;
    use super::build_index;

    let project = make_project();
    let cache = tempfile::tempdir().unwrap();

    // First build — establishes the old manifest.
    run(&index_args(project.path(), cache.path()), &TEST_ANALYTICS)
        .expect("first build must succeed");

    let old_manifest =
        FileManifest::load(project.path().to_path_buf(), cache.path().to_path_buf())
            .expect("old manifest must be loadable");
    let old_count = old_manifest.entry_count();

    // Simulate the desync abort (same as the previous test).
    let mut lexical_builder = NgramIndexBuilder::new(cache.path().to_path_buf()).unwrap();
    let mut ast_builder = AstIndexBuilder::new(cache.path().to_path_buf()).unwrap();
    ast_builder
        .add_file_ngrams(FileId(0), rskim_core::Language::Rust, &AstNgramSet::default(), 0, StructuralMetrics::default())
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
    };
    tx.send(pf).unwrap();
    drop(tx);
    let abort_result = Pipeline::consume(
        &mut lexical_builder,
        &mut ast_builder,
        &mut new_manifest,
        rx,
        false,
    );
    assert!(abort_result.is_err(), "consume must abort for the self-heal test to be meaningful");

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
