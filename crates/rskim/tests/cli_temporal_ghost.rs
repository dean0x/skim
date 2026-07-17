//! CLI integration tests for the temporal ghost filter (#408).
//!
//! Drives the real `skim` binary via `assert_cmd` against a fixture git
//! repository that contains committed-then-deleted files (ghosts).  Verifies
//! that `--hot`, `--cold`, `--risky`, and `--blast-radius` never surface a path
//! that an agent could not `Read` — i.e. every emitted path satisfies
//! `root.join(path).is_file()`.
//!
//! # Why a separate file (CROSS-PLAN)
//!
//! `cli_search_compose.rs` is owned by ticket #412's flag-compose E2E tests.
//! To avoid a merge conflict, the ghost-filter CLI E2E lives here.
//!
//! # Test plan coverage
//!
//! - AC3:  All four standalone arms (text + `--json`) exit 0 and emit no ghost.
//! - AC10: Ground-truth smoke — every path in JSON output satisfies `is_file()`.
//! - AC12: Heatmap arm does not emit a former-file path absent from disk.
//! - AC13: Heatmap and temporal arms agree: "gone.rs" absent from both.

use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;

use assert_cmd::Command;
use tempfile::TempDir;

// ============================================================================
// Helpers
// ============================================================================

/// Initialise a git repository with minimal identity config.
fn git_init(dir: &Path) {
    StdCommand::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .expect("git init");
    StdCommand::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(dir)
        .output()
        .expect("git config email");
    StdCommand::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir)
        .output()
        .expect("git config name");
}

/// Create a fixture git repo where `gone.rs` has several commits alongside
/// `keep.rs` (high co-change Jaccard) and is then deleted from disk.
///
/// Returns the owned `TempDir` (keep alive for test duration) and the HEAD SHA.
///
/// Git history layout:
/// - 3 joint commits (keep.rs + gone.rs) → joint=3, keep=4, gone=3
///   Jaccard = 3 / (4 + 3 - 3) = 3/4 = 0.75 ≥ MIN_COCHANGE_JACCARD (0.10)
/// - 1 extra keep.rs-only commit
///
/// After setup: `gone.rs` is deleted from disk via `fs::remove_file`, leaving
/// it in git history but absent on disk (the ghost scenario).
fn make_ghost_repo() -> (TempDir, String) {
    let dir = TempDir::new().expect("tempdir");
    git_init(dir.path());

    // Joint commit 1.
    fs::write(dir.path().join("keep.rs"), "fn keep1() {}").unwrap();
    fs::write(dir.path().join("gone.rs"), "fn gone1() {}").unwrap();
    StdCommand::new("git")
        .args(["add", "keep.rs", "gone.rs"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    StdCommand::new("git")
        .args(["commit", "-m", "feat: joint 1"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Joint commit 2.
    fs::write(dir.path().join("keep.rs"), "fn keep2() {}").unwrap();
    fs::write(dir.path().join("gone.rs"), "fn gone2() {}").unwrap();
    StdCommand::new("git")
        .args(["add", "keep.rs", "gone.rs"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    StdCommand::new("git")
        .args(["commit", "-m", "feat: joint 2"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Joint commit 3.
    fs::write(dir.path().join("keep.rs"), "fn keep3() {}").unwrap();
    fs::write(dir.path().join("gone.rs"), "fn gone3() {}").unwrap();
    StdCommand::new("git")
        .args(["add", "keep.rs", "gone.rs"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    StdCommand::new("git")
        .args(["commit", "-m", "feat: joint 3"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Extra keep.rs-only commit (keep: 4 total, gone: 3 total, joint: 3).
    fs::write(dir.path().join("keep.rs"), "fn keep4() {}").unwrap();
    StdCommand::new("git")
        .args(["add", "keep.rs"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    StdCommand::new("git")
        .args(["commit", "-m", "feat: keep only"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Read HEAD SHA.
    let out = StdCommand::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir.path())
        .output()
        .expect("git rev-parse HEAD");
    let head = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(head.len(), 40, "HEAD must be a 40-char SHA");

    // Delete gone.rs from disk — it remains in git history (ghost).
    fs::remove_file(dir.path().join("gone.rs")).expect("remove gone.rs");
    assert!(
        !dir.path().join("gone.rs").exists(),
        "gone.rs must be absent from disk after deletion"
    );
    assert!(
        dir.path().join("keep.rs").is_file(),
        "keep.rs must be present on disk"
    );

    (dir, head)
}

/// Build the temporal+lexical index for `proj` into `cache`.
fn build_index(proj: &Path, cache: &Path) {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "--build", "--root"])
        .arg(proj)
        .env("SKIM_CACHE_DIR", cache)
        .assert()
        .success();
}

/// Assert that "gone.rs" does not appear anywhere in `stdout`.
///
/// This is the primary discriminating assertion for AC3 / AC10:
/// if the ghost filter is removed, "gone.rs" would appear in temporal output.
fn assert_ghost_absent(stdout: &[u8], label: &str) {
    let text = String::from_utf8_lossy(stdout);
    assert!(
        !text.contains("gone.rs"),
        "AC3/AC10: '{label}' emitted 'gone.rs' (ghost) — ghost filter not applied"
    );
}

/// Parse JSON output and verify every `path` field satisfies `root.join(path).is_file()`.
///
/// AC10: every path emitted in `--json` output must be present on disk.
/// The JSON structure has `results: [{path: "..."}, ...]` for all temporal arms.
fn assert_json_paths_present(json_bytes: &[u8], root: &Path, label: &str) {
    let v: serde_json::Value = serde_json::from_slice(json_bytes)
        .unwrap_or_else(|e| panic!("'{label}' --json output is not valid JSON: {e}"));
    let results = v
        .get("results")
        .and_then(|r| r.as_array())
        .unwrap_or_else(|| panic!("'{label}' JSON missing 'results' array"));

    for result in results {
        let path = result
            .get("path")
            .and_then(|p| p.as_str())
            .unwrap_or_else(|| panic!("'{label}' JSON result missing 'path' field"));
        let abs = root.join(path);
        assert!(
            abs.is_file(),
            "AC10: '{label}' emitted path '{path}' that is NOT present on disk \
             (root.join(path).is_file() == false) — ghost filter not applied"
        );
    }
}

// ============================================================================
// AC3 / AC10 — All four standalone temporal arms, text + JSON
// ============================================================================

/// AC3 / AC10: `--risky` (text + JSON) exits 0, emits no ghost, all JSON paths present.
#[test]
fn test_ghost_filter_risky_text_and_json() {
    let (dir, _head) = make_ghost_repo();
    let cache = TempDir::new().unwrap();
    build_index(dir.path(), cache.path());

    // Text mode: exit 0 + no "gone.rs".
    let text_out = Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "--risky", "--root"])
        .arg(dir.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_ghost_absent(&text_out, "--risky text");

    // JSON mode: exit 0, no "gone.rs", every path is_file().
    let json_out = Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "--risky", "--json", "--root"])
        .arg(dir.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_ghost_absent(&json_out, "--risky --json");
    assert_json_paths_present(&json_out, dir.path(), "--risky --json");
}

/// AC3 / AC10: `--cold` (text + JSON) exits 0, emits no ghost, all JSON paths present.
#[test]
fn test_ghost_filter_cold_text_and_json() {
    let (dir, _head) = make_ghost_repo();
    let cache = TempDir::new().unwrap();
    build_index(dir.path(), cache.path());

    let text_out = Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "--cold", "--root"])
        .arg(dir.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_ghost_absent(&text_out, "--cold text");

    let json_out = Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "--cold", "--json", "--root"])
        .arg(dir.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_ghost_absent(&json_out, "--cold --json");
    assert_json_paths_present(&json_out, dir.path(), "--cold --json");
}

/// AC3 / AC10: `--hot` (text + JSON) exits 0, emits no ghost, all JSON paths present.
#[test]
fn test_ghost_filter_hot_text_and_json() {
    let (dir, _head) = make_ghost_repo();
    let cache = TempDir::new().unwrap();
    build_index(dir.path(), cache.path());

    let text_out = Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "--hot", "--root"])
        .arg(dir.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_ghost_absent(&text_out, "--hot text");

    let json_out = Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "--hot", "--json", "--root"])
        .arg(dir.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_ghost_absent(&json_out, "--hot --json");
    assert_json_paths_present(&json_out, dir.path(), "--hot --json");
}

/// AC3 / AC10: `--blast-radius keep.rs` (text + JSON) exits 0, emits no ghost,
/// all JSON paths present.
///
/// The (keep.rs, gone.rs) co-change pair was dropped by the ghost filter.
/// gone.rs must NOT appear as a blast-radius partner of keep.rs.
#[test]
fn test_ghost_filter_blast_radius_text_and_json() {
    let (dir, _head) = make_ghost_repo();
    let cache = TempDir::new().unwrap();
    build_index(dir.path(), cache.path());

    let text_out = Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "--blast-radius", "keep.rs", "--root"])
        .arg(dir.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_ghost_absent(&text_out, "--blast-radius text");

    let json_out = Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "--blast-radius", "keep.rs", "--json", "--root"])
        .arg(dir.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_ghost_absent(&json_out, "--blast-radius --json");
    assert_json_paths_present(&json_out, dir.path(), "--blast-radius --json");
}

// ============================================================================
// AC12 / AC13 — Heatmap alignment (OD2)
// ============================================================================

/// AC12 / AC13: `skim heatmap` and `skim search --hot` both exclude "gone.rs".
///
/// AC12: heatmap does not emit a path that `is_file()` rejects.
/// AC13: the two surfaces agree — "gone.rs" is absent from both.
///
/// Note: `skim heatmap` warns about files deleted on the *current branch*
/// (git diff output), not files deleted in previous commits.  Since "gone.rs"
/// was deleted in a past commit, it does not appear in `git diff` and will not
/// be present in heatmap output at all.  This test verifies that neither arm
/// exposes it.
#[test]
fn test_ghost_filter_heatmap_and_hot_agree() {
    let (dir, _head) = make_ghost_repo();
    let cache = TempDir::new().unwrap();
    build_index(dir.path(), cache.path());

    // Heatmap: exit 0, "gone.rs" not in output.
    let heatmap_out = Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--root"])
        .arg(dir.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // --hot: exit 0, "gone.rs" not in output.
    let hot_out = Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "--hot", "--root"])
        .arg(dir.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // AC13: neither surface emits "gone.rs".
    let heatmap_text = String::from_utf8_lossy(&heatmap_out);
    assert!(
        !heatmap_text.contains("gone.rs"),
        "AC12/AC13: heatmap must not emit 'gone.rs' (ghost)"
    );
    assert_ghost_absent(&hot_out, "AC13 --hot");
}
