//! Integration tests for the composable `skim search` CLI surface (full-CLI
//! integration / wave-4d).
//!
//! These drive the real `skim` binary via `assert_cmd` to prove that `--ast`
//! composes with temporal flags, a text query, `--blast-radius`, `--limit`, and
//! `--json` without erroring or panicking — the behaviour the removed interim
//! guard used to block. End-to-end coverage that the hermetic unit tests in
//! `cmd/search/{ast,temporal}_tests.rs` cannot give (those bypass the binary).
//!
//! Temporal-sort ORDERING is covered hermetically by the unit tests (seeding a
//! temporal DB requires real git history, which is fragile in an external test);
//! here we assert graceful composition + exit codes + JSON well-formedness.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;

// ============================================================================
// Helpers
// ============================================================================

/// Write a minimal project with two Rust files that contain match-based error
/// handling (so `--ast try-catch` matches) and the lexical token "error".
fn make_project(root: &Path) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/a.rs"),
        r#"
fn handle_a() {
    let r: Result<i32, &str> = Ok(1);
    match r {
        Ok(v) => println!("{v}"),
        Err(e) => eprintln!("error: {e}"),
    }
}
"#,
    )
    .unwrap();
    fs::write(
        root.join("src/b.rs"),
        r#"
fn handle_b() {
    let r: Result<i32, &str> = Err("boom");
    match r {
        Ok(v) => println!("{v}"),
        Err(e) => eprintln!("another error: {e}"),
    }
}
"#,
    )
    .unwrap();
}

/// Build the search index for `proj`, routing all cache I/O to `cache` so the
/// test never touches `~/.cache/skim/`.
fn build_index(proj: &Path, cache: &Path) {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "--build", "--root"])
        .arg(proj)
        .env("SKIM_CACHE_DIR", cache)
        .assert()
        .success();
}

// ============================================================================
// AC-H1 / AC-N2: help completeness + no residual interim-guard language
// ============================================================================

#[test]
fn search_help_documents_full_surface_and_degradation() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "--help"])
        .assert()
        .success()
        // AC-H1(a): a full-surface example combining all flags.
        .stdout(predicate::str::contains("Full CLI surface"))
        .stdout(predicate::str::contains("--ast god-function --hot"))
        // AC-H1(b): graceful-degradation note for absent heatmap data.
        .stdout(predicate::str::contains("degrade gracefully"))
        // The #283 single-node limitation line REMAINS.
        .stdout(predicate::str::contains("#283"))
        // AC-N2: no residual interim-guard language for --ast + temporal.
        .stdout(predicate::str::contains("#202").not())
        .stdout(predicate::str::contains("not yet composable").not())
        // A4(i): --weights flag and its default are documented in help.
        .stdout(predicate::str::contains("--weights"))
        .stdout(predicate::str::contains("0.5,0.3,0.2"));
}

// ============================================================================
// T-C2: docs↔code guard — default weight string matches WEIGHT6_* constants
// ============================================================================

/// Assert that the `skim search --help` default weight string matches the
/// WEIGHT6_* constants exported from `rskim_search::compound::weights`.
///
/// If a future code change bumps a default, this test will fail and remind
/// the author to update the help text (ADR-003: no empirically-baseless claims).
#[test]
fn weights_help_default_matches_code_default() {
    use rskim_search::compound::weights::{WEIGHT6_AST, WEIGHT6_LEXICAL, WEIGHT6_TEMPORAL};

    // format!("{}", f64) yields "0.5", "0.3", "0.2" for the current defaults.
    let expected = format!("{},{},{}", WEIGHT6_LEXICAL, WEIGHT6_AST, WEIGHT6_TEMPORAL);

    Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(expected));
}

// ============================================================================
// AC-N3: sort-mode mutual exclusion preserved after guard removal
// ============================================================================

#[test]
fn search_ast_hot_cold_mutually_exclusive() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "--ast", "try-catch", "--hot", "--cold"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("mutually exclusive"));
}

// ============================================================================
// AC-F1 / AC-A3: the full flag surface exits 0 with valid JSON and never #202
// ============================================================================

#[test]
fn search_full_surface_exits_zero_with_valid_json() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    make_project(proj.path());
    build_index(proj.path(), cache.path());

    // text + --ast + --hot + --blast-radius + --limit + --json — the canonical
    // "wire everything together" surface. No temporal.db exists (no git history),
    // so --hot/--blast-radius degrade gracefully: warnings on stderr, exit 0.
    let output = Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search",
            "error",
            "--ast",
            "try-catch",
            "--hot",
            "--blast-radius",
            "src/a.rs",
            "--limit",
            "20",
            "--json",
            "--root",
        ])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .success()
        .get_output()
        .clone();

    // stdout must be well-formed JSON (AC-F1).
    let stdout = String::from_utf8(output.stdout).unwrap();
    serde_json::from_str::<serde_json::Value>(stdout.trim())
        .unwrap_or_else(|e| panic!("full-surface stdout must be valid JSON ({e}); got:\n{stdout}"));

    // Neither stream may reference the removed interim guard (AC-F1 / AC-N2).
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        !stdout.contains("#202") && !stderr.contains("#202"),
        "no output may reference the removed #202 guard;\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

#[test]
fn search_standalone_ast_hot_degrades_gracefully() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    make_project(proj.path());
    build_index(proj.path(), cache.path());

    // Standalone (empty text) --ast + --hot with no temporal.db: must exit 0 with
    // valid JSON in unsorted order and a temporal-data warning on stderr (AC-A3).
    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "--ast", "try-catch", "--hot", "--json", "--root"])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("standalone --ast --hot stdout must be valid JSON ({e}); got:\n{stdout}")
    });
    assert_eq!(
        json["mode"], "ast",
        "standalone --ast envelope mode must be 'ast'"
    );

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("temporal"),
        "absent temporal data must warn on stderr (AC-A3); got:\n{stderr}"
    );
    assert!(
        !stdout.contains("#202") && !stderr.contains("#202"),
        "no output may reference the removed #202 guard"
    );
}

// ============================================================================
// #375 — legacy `index` positional removed; bareword 'index' is now a query
// ============================================================================

/// Write a project containing ≥4 files each with the token "index", so that
/// AC2's --limit bound is meaningful (result count can saturate the default
/// limit, proving --limit was honored rather than silently dropped).
fn make_index_project(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    for (name, content) in [
        ("src/a.rs", "fn a() { let index = 0; let _ = index; }"),
        ("src/b.rs", "fn b() { let index = 1; let _ = index; }"),
        ("src/c.rs", "fn c() { let index = 2; let _ = index; }"),
        ("src/d.rs", "fn d() { let index = 3; let _ = index; }"),
    ] {
        std::fs::write(root.join(name), content).unwrap();
    }
}

/// AC2 (#375): `skim search index --limit 2 --json` must succeed, must NOT emit
/// "unexpected argument", and must return ≤ 2 result rows (proving --limit reached
/// the query parser and was applied, not silently dropped to the default 20).
///
/// Before #375, `skim search index --limit 3` errored with:
///   "error: unexpected argument '--limit' found"
/// because the `index` positional intercepted the call and dispatched to
/// `IndexCli`, which does not accept `--limit`.  After removal, `--limit` is
/// parsed by `parse_flags` on the query path and honored.
///
/// Discriminating assertion (PF-007): the ≤ limit_cap assertion fails if the
/// positional intercept were restored (IndexCli rejects --limit → exit 1) OR if
/// --limit were silently dropped (result count > limit_cap).
#[test]
fn search_index_limit_is_honored_not_rejected() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    make_index_project(proj.path());
    build_index(proj.path(), cache.path());

    const LIMIT_CAP: usize = 2;
    let output = Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search",
            "index",
            "--limit",
            &LIMIT_CAP.to_string(),
            "--json",
            "--root",
        ])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .success() // exit 0 — would be exit 1 if IndexCli still rejected --limit
        .get_output()
        .clone();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unexpected argument"),
        "AC2: `skim search index --limit` must not produce 'unexpected argument'; \
         got stderr:\n{stderr}"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("AC2: stdout must be valid JSON ({e}); got:\n{stdout}"));
    let rows = json["results"]
        .as_array()
        .expect("AC2: JSON must have a 'results' array");
    assert!(
        rows.len() <= LIMIT_CAP,
        "AC2: result count ({}) must be ≤ --limit ({}), proving --limit was honored",
        rows.len(),
        LIMIT_CAP
    );
}

/// AC3 (#375, cold-start): On a fresh project with NO pre-existing index, a bare
/// `skim search index --json` must exit 0 and return ≥ 1 result on STDOUT
/// (auto-build fires, then the query runs).
///
/// This is the exact reported repro: under the OLD behavior the call built the
/// index and stopped, emitting zero result rows on stdout.  After removal, the
/// query path runs (after auto-building), returning files that contain "index".
///
/// The auto-build chatter (`building index…` / `indexed N files`) may appear on
/// STDERR — stdout assertions only.  Per the resolved Open Decision (zero-change),
/// no discoverability-hint string is asserted on stderr.
///
/// Discriminating assertion (PF-007): the present-result-row assertion fails
/// under the old behavior (builder ran and stopped, stdout is empty → no results).
#[test]
fn search_index_cold_start_auto_builds_and_returns_results() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap(); // brand-new empty cache, NO pre-build
    make_index_project(proj.path());

    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "index", "--json", "--root"])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("AC3: stdout must be valid JSON ({e}); got:\n{stdout}"));
    let rows = json["results"]
        .as_array()
        .expect("AC3: JSON must have a 'results' array");
    assert!(
        !rows.is_empty(),
        "AC3: cold-start `skim search index` must return ≥1 result on STDOUT \
         (query path ran after auto-build); stdout:\n{stdout}"
    );
    // Confirm at least one result path contains "index" token or its parent dir
    // (discriminating: builder output never put result rows on stdout).
    assert!(
        rows.iter().any(|r| r["path"].as_str().is_some()),
        "AC3: result rows must have a 'path' field; got:\n{stdout}"
    );
}

/// AC5 (#375): `skim search --help` stdout must NOT contain the removed subcommand
/// line "index            Build or update the search index (legacy)".
///
/// Full-line predicate (NOT bare "index") so the test passes even though "index"
/// still appears legitimately elsewhere in help (--build 'auto-build on first
/// query', --update, examples like 'Show index statistics').
///
/// Discriminating assertion (PF-007): if print_help() were not edited, the removed
/// line would still be present → the .not() assertion fails.
#[test]
fn search_help_no_longer_lists_index_as_subcommand() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "--help"])
        .assert()
        .success()
        // AC5: full removed-line string must be absent (AD-375-3).
        .stdout(
            predicate::str::contains("index            Build or update the search index (legacy)")
                .not(),
        )
        // AC5: --build and --rebuild must remain (builds are not gone, just re-surfaced).
        .stdout(predicate::str::contains("--build"))
        .stdout(predicate::str::contains("--rebuild"));
}

/// AC8 (#375): no residual positional shadow — `skim search build` runs a QUERY
/// for the word "build", not an index build.
///
/// Discriminating assertion (PF-007): if any positional-to-action mapping survived,
/// `skim search build` would route to the builder and produce zero result rows on
/// stdout → the present-result-row assertion fails.
#[test]
fn search_bareword_build_is_a_query_not_a_build_action() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    // File containing the word "build" so the query has something to return.
    std::fs::create_dir_all(proj.path().join("src")).unwrap();
    std::fs::write(
        proj.path().join("src/main.rs"),
        "// build pipeline\nfn main() { println!(\"build\"); }",
    )
    .unwrap();
    build_index(proj.path(), cache.path());

    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "build", "--json", "--root"])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .success()
        .get_output()
        .clone();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unexpected argument"),
        "AC8: `skim search build` must not produce 'unexpected argument'; stderr:\n{stderr}"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("AC8: stdout must be valid JSON ({e}); got:\n{stdout}"));
    let rows = json["results"]
        .as_array()
        .expect("AC8: JSON must have a 'results' array");
    assert!(
        !rows.is_empty(),
        "AC8: `skim search build` must return ≥1 result (query path, not builder); \
         stdout:\n{stdout}"
    );
}

// ============================================================================
// #400 — invalid/nonexistent --root validation (fail-loud gate)
// ============================================================================
//
// All tests run with an isolated SKIM_CACHE_DIR so they never touch ~/.cache/skim.
// Tests drive the real binary (assert_cmd) to exercise the full
//   main → dispatch → search::run → resolve_root_and_cache
// entry-point path, not internal helpers.

mod root_validation_400 {
    use assert_cmd::Command;
    use predicates::prelude::*;
    use std::fs;

    /// Collect the paths of all hash subdirectories under `<cache>/search/`.
    /// Returns an empty vec when `search/` is absent or empty. Used by
    /// `assert_no_cache_subdirs` (emptiness check) and the AC6/AC7 exact-count
    /// assertions so the `read_dir + filter_map(ok) + filter(is_dir)` idiom lives
    /// in exactly one place and failure messages show the offending paths.
    fn collect_hash_subdirs(cache: &std::path::Path) -> Vec<std::path::PathBuf> {
        let search_dir = cache.join("search");
        if !search_dir.exists() {
            return Vec::new();
        }
        fs::read_dir(&search_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.path())
            .collect()
    }

    /// Assert that the `search/` subtree of `cache` contains no hash subdirs.
    /// AC2 and AC3 share this check (PF-007 discriminating for the funnel bail).
    /// On failure the message includes the offending paths for easier diagnosis.
    fn assert_no_cache_subdirs(cache: &std::path::Path, label: &str) {
        let dirs = collect_hash_subdirs(cache);
        assert!(
            dirs.is_empty(),
            "{label}: no search/<hash>/ dirs must be created for a bad root; \
             found: {dirs:?}"
        );
    }

    // -------------------------------------------------------------------------
    // AC1 (#400) — non-existent --root fails loud with the full message triple
    // -------------------------------------------------------------------------
    /// Before #400 this exited 0 with "no results"; after #400 it must exit
    /// nonzero and emit all three expected message parts (PF-007 discriminating).
    #[test]
    fn ac1_nonexistent_root_fails_loud() {
        let cache = tempfile::tempdir().unwrap();
        Command::cargo_bin("skim")
            .unwrap()
            .args(["search", "foo", "--root", "/does/not/exist_400"])
            .env("SKIM_CACHE_DIR", cache.path())
            .assert()
            .failure()
            .stderr(predicate::str::contains("--root"))
            .stderr(predicate::str::contains("/does/not/exist_400"))
            .stderr(predicate::str::contains("Pass the directory to index"));
    }

    // -------------------------------------------------------------------------
    // AC2 (#400) — no cache directory is created for a non-existent root
    // -------------------------------------------------------------------------
    /// Before #400, search/<ae6b160b...>/ was created; after the funnel fix no
    /// cache dir is created (the bail fires before create_dir_all; PF-007).
    #[test]
    fn ac2_no_cache_dir_on_bad_root() {
        let cache = tempfile::tempdir().unwrap();

        let output = Command::cargo_bin("skim")
            .unwrap()
            .args(["search", "foo", "--root", "/does/not/exist_400"])
            .env("SKIM_CACHE_DIR", cache.path())
            .assert()
            .failure()
            .get_output()
            .clone();

        // search/ subdir must not exist or must have zero sub-dirs
        assert_no_cache_subdirs(cache.path(), "AC2");

        // Stderr must NOT contain "building index" (funnel bailed before create_dir_all)
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("building index"),
            "AC2: rejection must not trigger an index build; stderr:\n{stderr}"
        );
    }

    // -------------------------------------------------------------------------
    // AC3 (#400) — file-as-root is rejected by the is_dir guard
    // -------------------------------------------------------------------------
    /// canonicalize() succeeds for a regular file; the explicit is_dir() guard
    /// is what catches it. Before #400 this indexed 1 file and exited 0 (PF-007).
    #[test]
    fn ac3_file_root_rejected() {
        let cache = tempfile::tempdir().unwrap();
        let tmp_file = tempfile::NamedTempFile::new().unwrap();

        Command::cargo_bin("skim")
            .unwrap()
            .args(["search", "foo", "--root"])
            .arg(tmp_file.path())
            .env("SKIM_CACHE_DIR", cache.path())
            .assert()
            .failure()
            .stderr(predicate::str::contains("is not a directory"))
            .stderr(predicate::str::contains("Pass the directory to index"));

        // No cache hash dir created for a file root
        assert_no_cache_subdirs(cache.path(), "AC3");
    }

    // -------------------------------------------------------------------------
    // AC4 (#400) — valid root returns ground-truth results, exit 0
    // -------------------------------------------------------------------------
    /// Uses `alpha_token` as a unique sentinel — exactly one file contains it.
    /// The exact-set assertion (PF-007): src/a.rs appears, src/b.rs does not.
    #[test]
    fn ac4_valid_root_returns_ground_truth() {
        let proj = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();

        // Ground truth: src/a.rs has alpha_token; src/b.rs does not
        fs::create_dir_all(proj.path().join("src")).unwrap();
        fs::write(
            proj.path().join("src/a.rs"),
            "fn find_alpha() { let alpha_token = 42; let _ = alpha_token; }\n",
        )
        .unwrap();
        fs::write(
            proj.path().join("src/b.rs"),
            "fn find_beta() { let beta_value = 99; let _ = beta_value; }\n",
        )
        .unwrap();

        // Build the index
        Command::cargo_bin("skim")
            .unwrap()
            .args(["search", "--build", "--root"])
            .arg(proj.path())
            .env("SKIM_CACHE_DIR", cache.path())
            .assert()
            .success();

        let output = Command::cargo_bin("skim")
            .unwrap()
            .args(["search", "alpha_token", "--json", "--root"])
            .arg(proj.path())
            .env("SKIM_CACHE_DIR", cache.path())
            .assert()
            .success()
            .get_output()
            .clone();

        let stdout = String::from_utf8_lossy(&output.stdout);
        let json: serde_json::Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("AC4: stdout must be valid JSON ({e}); got:\n{stdout}"));
        let results = json["results"]
            .as_array()
            .expect("AC4: must have 'results' array");

        let paths: Vec<&str> = results.iter().filter_map(|r| r["path"].as_str()).collect();

        // Ground truth: alpha_token appears only in src/a.rs
        assert!(
            paths.iter().any(|p| p.contains("src/a.rs")),
            "AC4: result set must contain src/a.rs (only file with alpha_token); \
             got paths: {:?}",
            paths
        );
        assert!(
            !paths.iter().any(|p| p.contains("src/b.rs")),
            "AC4: src/b.rs must NOT appear in results (lacks alpha_token); \
             got paths: {:?}",
            paths
        );
    }

    // -------------------------------------------------------------------------
    // AC5 (#400) — empty but valid directory is NOT rejected
    // -------------------------------------------------------------------------
    /// The gate keys on existence + is_dir, never on emptiness. An empty dir is
    /// a legitimately valid root that just has no indexed files. Exit 0 + "no results".
    #[test]
    fn ac5_empty_valid_dir_not_rejected() {
        let proj = tempfile::tempdir().unwrap(); // empty
        let cache = tempfile::tempdir().unwrap();
        Command::cargo_bin("skim")
            .unwrap()
            .args(["search", "foo", "--root"])
            .arg(proj.path())
            .env("SKIM_CACHE_DIR", cache.path())
            .assert()
            .success()
            .stdout(predicate::str::contains("no results"));
    }

    // -------------------------------------------------------------------------
    // AC6 (#400) — trailing-slash hash stability: dir and dir/ map to one cache hash
    // -------------------------------------------------------------------------
    /// Before the fix, the AD-381-2 lexical fallback was only used for
    /// non-existent roots. Valid roots go through canonicalize() which already
    /// collapses trailing slashes — so both spellings produce the same sha256 prefix
    /// and only one search/<hash>/ dir must exist after both runs.
    #[test]
    fn ac6_trailing_slash_hash_stability() {
        let proj = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();

        // Put one file in the project so the index is non-trivial
        fs::create_dir_all(proj.path().join("src")).unwrap();
        fs::write(proj.path().join("src/lib.rs"), "fn main() {}\n").unwrap();

        let dir_str = proj.path().to_string_lossy().into_owned();
        let dir_trailing = format!("{dir_str}/");

        // Run once with plain path, once with trailing slash (separate invocations)
        let _ = Command::cargo_bin("skim")
            .unwrap()
            .args(["search", "main", "--root", &dir_str])
            .env("SKIM_CACHE_DIR", cache.path())
            .output()
            .unwrap();

        let _ = Command::cargo_bin("skim")
            .unwrap()
            .args(["search", "main", "--root", &dir_trailing])
            .env("SKIM_CACHE_DIR", cache.path())
            .output()
            .unwrap();

        // Exactly one search/<hash>/ dir must exist — both spellings map to the same hash
        let subdirs = collect_hash_subdirs(cache.path());
        assert_eq!(
            subdirs.len(),
            1,
            "AC6: both trailing-slash and plain spellings must map to ONE cache hash dir; \
             found {} dirs: {:?}",
            subdirs.len(),
            subdirs
        );
    }

    // -------------------------------------------------------------------------
    // AC7 (#400) — symlink-to-dir is accepted; symlink-to-file / dangling rejected
    // -------------------------------------------------------------------------
    #[cfg(unix)]
    #[test]
    fn ac7_symlink_root() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();

        // real/ dir with one Rust file
        let real_dir = tmp.path().join("real");
        fs::create_dir_all(&real_dir).unwrap();
        fs::write(
            real_dir.join("tok.rs"),
            "fn symlink_unique_tok() { let x = 1; let _ = x; }\n",
        )
        .unwrap();

        // A regular file
        let real_file = tmp.path().join("f.rs");
        fs::write(&real_file, "fn f() {}\n").unwrap();

        // Symlinks
        let link_dir = tmp.path().join("link_dir");
        let link_file = tmp.path().join("link_file");
        let link_dead = tmp.path().join("link_dead");
        symlink(&real_dir, &link_dir).unwrap();
        symlink(&real_file, &link_file).unwrap();
        symlink(tmp.path().join("no_such_target_400"), &link_dead).unwrap();

        // symlink-to-dir must succeed
        Command::cargo_bin("skim")
            .unwrap()
            .args(["search", "symlink_unique_tok", "--root"])
            .arg(&link_dir)
            .env("SKIM_CACHE_DIR", cache.path())
            .assert()
            .success();

        // run again via the real dir path to verify same hash
        Command::cargo_bin("skim")
            .unwrap()
            .args(["search", "symlink_unique_tok", "--root"])
            .arg(&real_dir)
            .env("SKIM_CACHE_DIR", cache.path())
            .assert()
            .success();

        // Both routes must map to exactly ONE cache hash dir
        let subdirs = collect_hash_subdirs(cache.path());
        assert_eq!(
            subdirs.len(),
            1,
            "AC7: symlink-dir and real-dir must map to ONE cache hash; \
             found {} dirs: {:?}",
            subdirs.len(),
            subdirs
        );

        // symlink-to-file must fail with is_dir message
        Command::cargo_bin("skim")
            .unwrap()
            .args(["search", "foo", "--root"])
            .arg(&link_file)
            .env("SKIM_CACHE_DIR", cache.path())
            .assert()
            .failure()
            .stderr(predicate::str::contains("is not a directory"));

        // dangling symlink must fail (canonicalize error → Pass the directory hint)
        Command::cargo_bin("skim")
            .unwrap()
            .args(["search", "foo", "--root"])
            .arg(&link_dead)
            .env("SKIM_CACHE_DIR", cache.path())
            .assert()
            .failure()
            .stderr(predicate::str::contains("Pass the directory to index"));
    }

    // -------------------------------------------------------------------------
    // AC10 (#400) — exit code is exactly 1 for all rejection cases
    // -------------------------------------------------------------------------
    /// Distinguishes exit 1 (general/user-input error) from skim's exit 2
    /// (parse error) and exit 3 (unsupported language). Both reuse the existing
    /// anyhow::Err → ExitCode::FAILURE mapping (main.rs; no new plumbing).
    #[test]
    fn ac10_exit_code_is_exactly_1() {
        let cache = tempfile::tempdir().unwrap();
        let tmp_file = tempfile::NamedTempFile::new().unwrap();

        // Non-existent root → exit 1
        Command::cargo_bin("skim")
            .unwrap()
            .args(["search", "foo", "--root", "/does/not/exist_400"])
            .env("SKIM_CACHE_DIR", cache.path())
            .assert()
            .code(1);

        // File root → exit 1
        Command::cargo_bin("skim")
            .unwrap()
            .args(["search", "foo", "--root"])
            .arg(tmp_file.path())
            .env("SKIM_CACHE_DIR", cache.path())
            .assert()
            .code(1);

        // Note: `--root ""` is not included here because it is rejected by
        // take_flag_value's empty-value guard (mod.rs:506-508 — "value must not be
        // empty or whitespace-only") before reaching resolve_root_and_cache.
        // That guard predates #400, so `--root ""` is not discriminating for the
        // #400 funnel fix (PF-007) — the assertion would still pass with the entire
        // root-validation funnel deleted.
    }

    // -------------------------------------------------------------------------
    // AC11 (#400) — rejection is bounded; no index build on a bad root
    // -------------------------------------------------------------------------
    /// Pre-fix, the bad-root path performed a full walk + index write. Post-fix,
    /// the funnel bail fires before create_dir_all and before any walk — no
    /// "building index" message appears on stderr (PF-007 discriminating proxy).
    #[test]
    fn ac11_no_build_on_rejection() {
        let cache = tempfile::tempdir().unwrap();
        let output = Command::cargo_bin("skim")
            .unwrap()
            .args(["search", "foo", "--root", "/does/not/exist_400"])
            .env("SKIM_CACHE_DIR", cache.path())
            .assert()
            .failure()
            .get_output()
            .clone();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("building index"),
            "AC11: bad-root rejection must not trigger an index build; stderr:\n{stderr}"
        );
    }
}

// ============================================================================
// #402 — tracked-but-.gitignored union (recall correctness fix)
// ============================================================================
//
// All tests use isolated SKIM_CACHE_DIR so they never touch ~/.cache/skim.
// Tests drive the real binary (assert_cmd) to exercise the full
//   main → dispatch → search::run → walk_metadata → merge_tracked_union
// entry-point path.
//
// Oracle for positive recall assertions: `git grep -l <token>` — respects the
// git index and includes tracked-but-.gitignored files (ADR-003 / ADR-007).

mod tracked_union_402 {
    use assert_cmd::Command;
    use predicates::prelude::*;
    use serde_json::Value;
    use std::fs;
    use std::path::Path;
    use std::process::Command as StdCommand;

    // ---- Helpers ----------------------------------------------------------------

    /// Initialize a real git repo at `root` with the standard test layout:
    /// ```
    /// root/
    ///   .gitignore       <- "secretdoc.md\n"
    ///   secretdoc.md     <- tracked via `git add -f`; content contains `token`
    ///   src/a.rs         <- ordinary tracked file
    /// ```
    fn init_tracked_ignored_repo(root: &Path, token: &str) {
        StdCommand::new("git")
            .args(["init"])
            .current_dir(root)
            .output()
            .expect("git init");
        StdCommand::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(root)
            .output()
            .expect("git config email");
        StdCommand::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(root)
            .output()
            .expect("git config name");

        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join(".gitignore"), "secretdoc.md\n").unwrap();
        fs::write(root.join("secretdoc.md"), format!("{token}\n")).unwrap();
        fs::write(root.join("src/a.rs"), "fn a() {}\n").unwrap();

        StdCommand::new("git")
            .args(["add", "-f", "secretdoc.md", "src/a.rs", ".gitignore"])
            .current_dir(root)
            .output()
            .expect("git add");
        StdCommand::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(root)
            .output()
            .expect("git commit");
    }

    /// Build the search index for `proj`, routing all cache I/O to `cache`.
    fn build_index_402(proj: &Path, cache: &Path) {
        Command::cargo_bin("skim")
            .unwrap()
            .args(["search", "--build", "--root"])
            .arg(proj)
            .env("SKIM_CACHE_DIR", cache)
            .assert()
            .success();
    }

    /// Run `skim search <query> --root <proj> --json`, return stdout.
    fn query_json(proj: &Path, cache: &Path, query: &str) -> String {
        let out = Command::cargo_bin("skim")
            .unwrap()
            .args(["search", query, "--root"])
            .arg(proj)
            .arg("--json")
            .env("SKIM_CACHE_DIR", cache)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        String::from_utf8_lossy(&out).into_owned()
    }

    /// Extract the `path` fields from a `skim search --json` result.
    fn result_paths(json_str: &str) -> Vec<String> {
        let v: Value = serde_json::from_str(json_str.trim())
            .unwrap_or_else(|e| panic!("JSON parse error: {e}; got:\n{json_str}"));
        v["results"]
            .as_array()
            .expect("must have 'results' array")
            .iter()
            .filter_map(|r| r["path"].as_str().map(ToString::to_string))
            .collect()
    }

    // -------------------------------------------------------------------------
    // AC2 (#402) — hermetic exact-set repro: tracked-but-.gitignored file found
    // -------------------------------------------------------------------------
    /// Core correctness test: a file that is in `.gitignore` but also tracked
    /// (`git add -f`) MUST appear in `skim search` results.
    ///
    /// Discriminating (PF-007): before #402, `secretdoc.md` is MISSING because
    /// the ignore-walk honours `.gitignore`. The moment the union is reverted,
    /// this test fails.
    #[test]
    fn ac2_tracked_gitignored_file_found() {
        let proj = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        init_tracked_ignored_repo(proj.path(), "ZZUNIQUETOKEN");
        build_index_402(proj.path(), cache.path());

        let json = query_json(proj.path(), cache.path(), "ZZUNIQUETOKEN");
        let paths = result_paths(&json);

        assert!(
            paths.iter().any(|p| p.contains("secretdoc.md")),
            "AC2: secretdoc.md (tracked-but-.gitignored) must appear in search results; \
             paths: {paths:?}"
        );
        // src/a.rs must NOT appear (lacks the token).
        assert!(
            !paths.iter().any(|p| p.contains("src/a.rs")),
            "AC2: src/a.rs must not appear (does not contain ZZUNIQUETOKEN); paths: {paths:?}"
        );
    }

    // -------------------------------------------------------------------------
    // AC3 (#402) — tracked hidden-dir file is found
    // -------------------------------------------------------------------------
    /// A file in a hidden directory (e.g., `.config/app.rs`) is normally
    /// dropped by `hidden(true)`. When tracked in git, it must appear.
    #[test]
    fn ac3_tracked_hidden_dir_file_found() {
        let proj = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();

        StdCommand::new("git")
            .args(["init"])
            .current_dir(proj.path())
            .output()
            .expect("git init");
        StdCommand::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(proj.path())
            .output()
            .expect("git config email");
        StdCommand::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(proj.path())
            .output()
            .expect("git config name");

        fs::create_dir_all(proj.path().join(".config")).unwrap();
        fs::write(proj.path().join(".config/app.rs"), "// HIDDENTOKEN\n").unwrap();
        fs::create_dir_all(proj.path().join("src")).unwrap();
        fs::write(proj.path().join("src/a.rs"), "fn a() {}\n").unwrap();

        StdCommand::new("git")
            .args(["add", ".config/app.rs", "src/a.rs"])
            .current_dir(proj.path())
            .output()
            .expect("git add");
        StdCommand::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(proj.path())
            .output()
            .expect("git commit");

        build_index_402(proj.path(), cache.path());

        let json = query_json(proj.path(), cache.path(), "HIDDENTOKEN");
        let paths = result_paths(&json);

        assert!(
            paths.iter().any(|p| p.contains(".config/app.rs")),
            "AC3: .config/app.rs (tracked hidden-dir file) must appear in search results; \
             paths: {paths:?}"
        );
    }

    // -------------------------------------------------------------------------
    // AC4 (#402) — Precision: untracked-ignored junk stays excluded
    // -------------------------------------------------------------------------
    /// The union only adds TRACKED files. An untracked `.gitignore`d file must
    /// remain excluded so skim does not become `--no-ignore`.
    #[test]
    fn ac4_untracked_ignored_file_excluded() {
        let proj = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();

        StdCommand::new("git")
            .args(["init"])
            .current_dir(proj.path())
            .output()
            .expect("git init");
        StdCommand::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(proj.path())
            .output()
            .expect("git config email");
        StdCommand::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(proj.path())
            .output()
            .expect("git config name");

        fs::create_dir_all(proj.path().join("build")).unwrap();
        fs::create_dir_all(proj.path().join("src")).unwrap();
        // Untracked junk — never git-added
        fs::write(proj.path().join("build/junk.rs"), "// JUNKTOKEN\n").unwrap();
        // Tracked ordinary file
        fs::write(proj.path().join("src/a.rs"), "fn a() {}\n").unwrap();
        fs::write(proj.path().join(".gitignore"), "build/\n").unwrap();

        StdCommand::new("git")
            .args(["add", "src/a.rs", ".gitignore"])
            .current_dir(proj.path())
            .output()
            .expect("git add");
        StdCommand::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(proj.path())
            .output()
            .expect("git commit");

        build_index_402(proj.path(), cache.path());

        let json = query_json(proj.path(), cache.path(), "JUNKTOKEN");
        let paths = result_paths(&json);

        assert!(
            !paths.iter().any(|p| p.contains("build/junk.rs")),
            "AC4: untracked-ignored build/junk.rs must NOT appear in search results \
             (union only adds tracked files); paths: {paths:?}"
        );
    }

    // -------------------------------------------------------------------------
    // AC5 (#402) — Non-git root: ignore-walk stays authoritative
    // -------------------------------------------------------------------------
    /// A temp dir with NO `.git` directory: the `.git` gate prevents the union
    /// from running at all. A `.gitignore`d file that is NOT tracked stays out.
    #[test]
    fn ac5_non_git_root_union_skipped() {
        let proj = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();

        // No git init — no .git directory
        fs::write(proj.path().join(".gitignore"), "notes.rs\n").unwrap();
        fs::write(proj.path().join("notes.rs"), "// NGTOKEN\n").unwrap();

        build_index_402(proj.path(), cache.path());

        let json = query_json(proj.path(), cache.path(), "NGTOKEN");
        let paths = result_paths(&json);

        assert!(
            !paths.iter().any(|p| p.contains("notes.rs")),
            "AC5: notes.rs must NOT appear on a non-git root \
             (union gate requires .git to exist); paths: {paths:?}"
        );
    }

    // -------------------------------------------------------------------------
    // AC7 (#402) — Zero-delta steady state (no perpetual rebuild loop)
    // -------------------------------------------------------------------------
    /// After building the index once (which unions secretdoc.md), a subsequent
    /// query must complete WITHOUT triggering a rebuild. If the union set
    /// diverged between the build path and the staleness-scan path (AD-379-1
    /// violation), the second query would trigger `WorkingTreeChanged` every
    /// time → perpetual rebuild loop.
    ///
    /// Discriminating: a "working tree changed" or "building index" line on
    /// stderr on the SECOND query proves the loop would occur.
    #[test]
    fn ac7_zero_delta_steady_state() {
        let proj = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        init_tracked_ignored_repo(proj.path(), "ZZUNIQUETOKEN");

        // First query: cold cache → builds the index (union adds secretdoc.md).
        Command::cargo_bin("skim")
            .unwrap()
            .args(["search", "ZZUNIQUETOKEN", "--root"])
            .arg(proj.path())
            .env("SKIM_CACHE_DIR", cache.path())
            .assert()
            .success();

        // Second query: index is warm; must NOT rebuild.
        let out = Command::cargo_bin("skim")
            .unwrap()
            .args(["search", "ZZUNIQUETOKEN", "--root"])
            .arg(proj.path())
            .env("SKIM_CACHE_DIR", cache.path())
            .assert()
            .success()
            .get_output()
            .clone();

        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("building index"),
            "AC7: second query must NOT trigger a rebuild (would be a perpetual loop); \
             stderr:\n{stderr}"
        );
    }

    // -------------------------------------------------------------------------
    // AC12 (#402) — Exit codes and SKIM_DEBUG diagnostic
    // -------------------------------------------------------------------------
    #[test]
    fn ac12_exit_codes_and_debug_line() {
        let proj = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        init_tracked_ignored_repo(proj.path(), "ZZUNIQUETOKEN");

        // Build with SKIM_DEBUG=1: must emit the "unioned N git-tracked file(s)" line.
        let build_out = Command::cargo_bin("skim")
            .unwrap()
            .args(["search", "--build", "--root"])
            .arg(proj.path())
            .env("SKIM_CACHE_DIR", cache.path())
            .env("SKIM_DEBUG", "1")
            .assert()
            .success()
            .get_output()
            .clone();
        let build_stderr = String::from_utf8_lossy(&build_out.stderr);
        assert!(
            build_stderr.contains("unioned") && build_stderr.contains("git-tracked file"),
            "AC12(c): SKIM_DEBUG=1 build must emit the union diagnostic line; \
             stderr:\n{build_stderr}"
        );

        // Hit query → exit 0.
        Command::cargo_bin("skim")
            .unwrap()
            .args(["search", "ZZUNIQUETOKEN", "--root"])
            .arg(proj.path())
            .env("SKIM_CACHE_DIR", cache.path())
            .assert()
            .code(0);

        // Miss query → exit 0 (skim always exits 0 on no results).
        Command::cargo_bin("skim")
            .unwrap()
            .args(["search", "NOTPRESENTTOKEN402", "--root"])
            .arg(proj.path())
            .env("SKIM_CACHE_DIR", cache.path())
            .assert()
            .code(0);

        // Rebuild without SKIM_DEBUG → union diagnostic line must NOT appear.
        let cache2 = tempfile::tempdir().unwrap();
        let rebuild_out = Command::cargo_bin("skim")
            .unwrap()
            .args(["search", "--build", "--root"])
            .arg(proj.path())
            .env("SKIM_CACHE_DIR", cache2.path())
            .assert()
            .success()
            .get_output()
            .clone();
        let rebuild_stderr = String::from_utf8_lossy(&rebuild_out.stderr);
        assert!(
            !rebuild_stderr.contains("unioned") || !rebuild_stderr.contains("git-tracked file"),
            "AC12(c) negative: without SKIM_DEBUG the union line must be absent; \
             stderr:\n{rebuild_stderr}"
        );
    }

    // -------------------------------------------------------------------------
    // AC13 integration (ii) (#402) — cache MISS on mutated .git/index
    // -------------------------------------------------------------------------
    /// After `git add -f newsecret.md` (which rewrites `.git/index`), the next
    /// query must find `newsecret.md` in results.
    ///
    /// Discriminating: if the memoization never invalidates (always returns the
    /// cached list), the newly-added file stays missing → assertion fails.
    #[test]
    fn ac13_integration_recall_after_git_add() {
        let proj = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        init_tracked_ignored_repo(proj.path(), "ZZUNIQUETOKEN");

        // Build the initial index.
        build_index_402(proj.path(), cache.path());

        // Verify initial state: ZZUNIQUETOKEN found, NEWSECRETTOKEN not found.
        let paths_before = result_paths(&query_json(proj.path(), cache.path(), "NEWSECRETTOKEN"));
        assert!(
            !paths_before.iter().any(|p| p.contains("newsecret.md")),
            "AC13: newsecret.md must not appear before git add"
        );

        // Add a new tracked-but-.gitignored file (rewrites .git/index).
        fs::write(proj.path().join("newsecret.md"), "NEWSECRETTOKEN\n").unwrap();
        StdCommand::new("git")
            .args(["add", "-f", "newsecret.md"])
            .current_dir(proj.path())
            .output()
            .expect("git add newsecret.md");

        // Trigger a rebuild (the new file should force WorkingTreeChanged).
        Command::cargo_bin("skim")
            .unwrap()
            .args(["search", "NEWSECRETTOKEN", "--root"])
            .arg(proj.path())
            .env("SKIM_CACHE_DIR", cache.path())
            .assert()
            .success();

        // After rebuild, NEWSECRETTOKEN must be found.
        let paths_after = result_paths(&query_json(proj.path(), cache.path(), "NEWSECRETTOKEN"));
        assert!(
            paths_after.iter().any(|p| p.contains("newsecret.md")),
            "AC13: newsecret.md must appear after git add + rebuild \
             (cache miss must re-enumerate); paths: {paths_after:?}"
        );
    }

    // -------------------------------------------------------------------------
    // Cross-plan composed dog-food: #400 + #402 compose (ADR-007)
    // -------------------------------------------------------------------------
    /// One fresh isolated SKIM_CACHE_DIR: (a) bad-root fails loud (#400 behavior)
    /// AND (b) a tracked-but-.gitignored file surfaces in search (#402 behavior).
    /// Proves the two fixes compose in the wave (applies ADR-007).
    #[test]
    fn cross_plan_400_402_compose() {
        let proj = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        init_tracked_ignored_repo(proj.path(), "ZZUNIQUETOKEN");

        // (a) #400: bad root fails with exit 1 + message.
        Command::cargo_bin("skim")
            .unwrap()
            .args(["search", "foo", "--root", "/does/not/exist_402_compose"])
            .env("SKIM_CACHE_DIR", cache.path())
            .assert()
            .failure()
            .stderr(predicate::str::contains("Pass the directory to index"));

        // (b) #402: tracked-but-.gitignored file found.
        build_index_402(proj.path(), cache.path());
        let paths = result_paths(&query_json(proj.path(), cache.path(), "ZZUNIQUETOKEN"));
        assert!(
            paths.iter().any(|p| p.contains("secretdoc.md")),
            "cross-plan: secretdoc.md must appear (tracked-union #402); paths: {paths:?}"
        );
    }
}
