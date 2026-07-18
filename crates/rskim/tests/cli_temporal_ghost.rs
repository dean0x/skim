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
//! - AC3:  All four standalone arms (text + `--json`) exit 0, emit no ghost,
//!         and emit at least one present file (non-vacuous PF-007 anchor).
//! - AC10: Ground-truth smoke — every path in JSON output satisfies `is_file()`;
//!         results non-empty where expected.
//! - AC12: `skim heatmap --diff` warns when a diffed path is replaced by a
//!         directory (`is_file()==false`, `exists()==true`) — tested in
//!         `test_heatmap_diff_path_replaced_by_directory_warns`.
//! - AC13: `skim search --hot` excludes "gone.rs" (ghost filter applied at build
//!         time); `skim heatmap` (no `--diff`) shows it from git history, which
//!         is correct and expected.

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
///
/// `require_nonempty`: when `true`, asserts the results array is non-empty so
/// that the ghost-absence check cannot pass vacuously on an empty result set
/// (PF-007: a test asserting only exit-0 is worthless — assert a DISCRIMINATING
/// observable).  Pass `false` for the `--blast-radius` arm where all co-change
/// partners are ghosts and an empty result set is the correct post-filter state.
fn assert_json_paths_present(json_bytes: &[u8], root: &Path, label: &str, require_nonempty: bool) {
    let v: serde_json::Value = serde_json::from_slice(json_bytes)
        .unwrap_or_else(|e| panic!("'{label}' --json output is not valid JSON: {e}"));
    let results = v
        .get("results")
        .and_then(|r| r.as_array())
        .unwrap_or_else(|| panic!("'{label}' JSON missing 'results' array"));

    if require_nonempty {
        assert!(
            !results.is_empty(),
            "AC10: '{label}' JSON 'results' array is empty — temporal query \
             returned nothing; ghost-absence assertion cannot be verified \
             (possible index-wiring regression)"
        );
    }

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

    // Text mode: exit 0 + no "gone.rs" + keep.rs present (non-vacuous anchor).
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
    let text = String::from_utf8_lossy(&text_out);
    assert!(
        text.contains("keep.rs"),
        "AC3: '--risky text' must emit 'keep.rs' (present file); \
         if empty the ghost-absence check is vacuous (PF-007)"
    );

    // JSON mode: exit 0, no "gone.rs", every path is_file(), results non-empty.
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
    assert_json_paths_present(&json_out, dir.path(), "--risky --json", true);
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
    let text = String::from_utf8_lossy(&text_out);
    assert!(
        text.contains("keep.rs"),
        "AC3: '--cold text' must emit 'keep.rs' (present file); \
         if empty the ghost-absence check is vacuous (PF-007)"
    );

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
    assert_json_paths_present(&json_out, dir.path(), "--cold --json", true);
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
    let text = String::from_utf8_lossy(&text_out);
    assert!(
        text.contains("keep.rs"),
        "AC3: '--hot text' must emit 'keep.rs' (present file); \
         if empty the ghost-absence check is vacuous (PF-007)"
    );

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
    assert_json_paths_present(&json_out, dir.path(), "--hot --json", true);
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
    // After ghost-filtering, gone.rs (the only partner) is removed and the
    // output says `No co-change data for "keep.rs".`  That message still
    // contains "keep.rs", so this assertion is non-vacuous: if the binary
    // crashed silently and emitted nothing it would fail here (PF-007).
    let text = String::from_utf8_lossy(&text_out);
    assert!(
        text.contains("keep.rs"),
        "AC3: '--blast-radius text' must reference 'keep.rs' \
         (target appears in the no-data message when all partners are filtered)"
    );

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
    // require_nonempty=false: all co-change partners (only gone.rs) are ghosts
    // and are correctly filtered, yielding an empty results array.  An empty
    // result is the correct post-filter state here, not a regression.
    assert_json_paths_present(&json_out, dir.path(), "--blast-radius --json", false);
}

// ============================================================================
// AC12 / AC13 — Heatmap alignment (OD2)
// ============================================================================

/// AC13: `skim search --hot` excludes "gone.rs" (ghost filter applied at build time).
///
/// AC13: the temporal `--hot` arm does not surface a deleted-from-disk path.
///
/// Note: `skim heatmap` without `--diff` shows ALL files from git history and
/// intentionally includes gone.rs (it has 3 commits).  The heatmap ghost-filter
/// (`is_file()` in `resolve_diff_files`) is only triggered with `--diff`; that
/// path is covered by `test_heatmap_diff_path_replaced_by_directory_warns`.
/// This test uses `current_dir` as the idiom from cli_heatmap.rs:226-228 — the
/// heatmap has no `--root` flag (its parser rejects unknown flags via `bail!`).
#[test]
fn test_ghost_filter_heatmap_and_hot_agree() {
    let (dir, _head) = make_ghost_repo();
    let cache = TempDir::new().unwrap();
    build_index(dir.path(), cache.path());

    // Heatmap: exit 0, keep.rs appears (positive non-vacuous anchor).
    // gone.rs also appears (3 commits in history) — that is expected because the
    // heatmap without --diff reads raw git log and has no ghost filter.
    let heatmap_out = Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap"])
        .current_dir(dir.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let heatmap_text = String::from_utf8_lossy(&heatmap_out);
    assert!(
        heatmap_text.contains("keep.rs"),
        "AC13: heatmap must emit 'keep.rs' (present file with 4 commits)"
    );

    // --hot: exit 0, "gone.rs" not in output, keep.rs present.
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
    assert_ghost_absent(&hot_out, "AC13 --hot");
    let hot_text = String::from_utf8_lossy(&hot_out);
    assert!(
        hot_text.contains("keep.rs"),
        "AC13: '--hot' must emit 'keep.rs'; \
         if empty the ghost-absence check is vacuous (PF-007)"
    );
}

// ============================================================================
// AD-408-2 (OD2) — heatmap --diff is_file() vs exists() discrimination
// ============================================================================

/// AD-408-2: `skim heatmap --diff` warns when a diffed path is replaced by a
/// directory on disk (`is_file() == false` while `exists() == true`).
///
/// This is the only test that exercises the `is_file()` guard introduced in
/// `resolve_diff_files` (heatmap/mod.rs).  The discriminating case is a
/// former-file path now occupied by a directory: `exists()` would return `true`
/// and silently skip the warning; `is_file()` returns `false` and emits it.
///
/// Setup:
/// - commit A: `changed.rs` is a regular file
/// - commit B: `changed.rs` is modified (HEAD)
/// - disk: `changed.rs` is replaced by a same-named directory (not committed)
///
/// `git diff A...HEAD --name-only` lists `changed.rs` as modified.
/// `is_file()` on the absolute path returns false (it is a directory).
/// Warning must appear in stderr.
#[test]
fn test_heatmap_diff_path_replaced_by_directory_warns() {
    let dir = TempDir::new().expect("tempdir");
    git_init(dir.path());

    // Commit A: create changed.rs as a regular file.
    fs::write(dir.path().join("changed.rs"), "fn a() {}").unwrap();
    StdCommand::new("git")
        .args(["add", "changed.rs"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    StdCommand::new("git")
        .args(["commit", "-m", "base: add changed.rs"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Capture base SHA (the diff base for --diff).
    let base_out = StdCommand::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir.path())
        .output()
        .expect("git rev-parse HEAD");
    let base_sha = String::from_utf8_lossy(&base_out.stdout).trim().to_string();
    assert_eq!(base_sha.len(), 40, "base SHA must be 40 chars");

    // Commit B: modify changed.rs so it appears in `git diff <base>...HEAD`.
    fs::write(dir.path().join("changed.rs"), "fn a_v2() {}").unwrap();
    StdCommand::new("git")
        .args(["add", "changed.rs"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    StdCommand::new("git")
        .args(["commit", "-m", "modify changed.rs"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Replace changed.rs with a same-named directory (not committed).
    // After this: exists() == true, is_file() == false — the case that
    // exists() would have missed (AD-408-2).
    fs::remove_file(dir.path().join("changed.rs")).expect("remove changed.rs");
    fs::create_dir(dir.path().join("changed.rs")).expect("create changed.rs dir");
    assert!(
        dir.path().join("changed.rs").exists(),
        "directory must exist at changed.rs path"
    );
    assert!(
        !dir.path().join("changed.rs").is_file(),
        "changed.rs must NOT be a regular file (it is a directory)"
    );

    // Run `skim heatmap --diff <base_sha>`.
    // git diff base...HEAD lists changed.rs as modified.
    // is_file() on the absolute path returns false (it is a directory)
    // → "is not a regular file on current branch" warning emitted to stderr.
    let out = Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--diff"])
        .arg(&base_sha)
        .current_dir(dir.path())
        .output()
        .expect("skim heatmap --diff");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("is not a regular file on current branch"),
        "AD-408-2: expected 'is not a regular file on current branch' warning when a \
         diffed path is occupied by a directory (is_file()==false, exists()==true); \
         stderr was: {stderr:?}"
    );
}
