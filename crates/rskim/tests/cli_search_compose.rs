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
        .stdout(predicate::str::contains("not yet composable").not());
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
