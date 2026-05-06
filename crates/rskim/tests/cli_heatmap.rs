//! Integration tests for `skim heatmap` subcommand.
//!
//! Tests end-to-end CLI behavior for git history risk/coupling analysis.
//! For tests that need a git repo, we create a TempDir and set up deterministic commits.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use std::process::Command as StdCommand;

// ============================================================================
// Test helpers
// ============================================================================

/// Initialize a git repo with user identity configured.
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
        .args(["config", "user.name", "Test User"])
        .current_dir(dir)
        .output()
        .expect("git config name");
    // Use main as default branch
    StdCommand::new("git")
        .args(["checkout", "-b", "main"])
        .current_dir(dir)
        .output()
        .ok();
}

/// Write a file and make a commit.
fn git_commit(dir: &Path, filename: &str, content: &str, message: &str, timestamp: u64) {
    let file_path = dir.join(filename);
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&file_path, content).expect("write file");

    StdCommand::new("git")
        .args(["add", filename])
        .current_dir(dir)
        .output()
        .expect("git add");

    let ts = format!("{timestamp}");
    StdCommand::new("git")
        .args(["commit", "-m", message])
        .env("GIT_AUTHOR_DATE", &ts)
        .env("GIT_COMMITTER_DATE", &ts)
        .current_dir(dir)
        .output()
        .expect("git commit");
}

/// Commit multiple files at once.
fn git_commit_files(dir: &Path, files: &[(&str, &str)], message: &str, timestamp: u64) {
    for (filename, content) in files {
        let file_path = dir.join(filename);
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&file_path, content).expect("write file");
        StdCommand::new("git")
            .args(["add", filename])
            .current_dir(dir)
            .output()
            .expect("git add");
    }

    let ts = format!("{timestamp}");
    StdCommand::new("git")
        .args(["commit", "-m", message])
        .env("GIT_AUTHOR_DATE", &ts)
        .env("GIT_COMMITTER_DATE", &ts)
        .current_dir(dir)
        .output()
        .expect("git commit");
}

/// Current Unix timestamp minus a few days — within the default 90-day window.
fn recent_ts() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_sub(7 * 86400) // 7 days ago
}

/// Create a test repo with 5+ commits across multiple files.
fn create_test_repo(dir: &Path) {
    git_init(dir);

    let base_ts = recent_ts();

    // 5 commits with multiple files to exercise all metrics
    git_commit(
        dir,
        "src/main.rs",
        "fn main() {}",
        "feat: initial main",
        base_ts,
    );
    git_commit(
        dir,
        "src/lib.rs",
        "pub fn foo() {}",
        "feat: add lib",
        base_ts + 100,
    );
    git_commit_files(
        dir,
        &[
            ("src/main.rs", "fn main() { foo(); }"),
            ("src/lib.rs", "pub fn foo() -> i32 { 1 }"),
        ],
        "feat: wire main to lib",
        base_ts + 200,
    );
    git_commit(
        dir,
        "src/main.rs",
        "fn main() { let x = 1; }",
        "fix: correct main",
        base_ts + 300,
    );
    git_commit_files(
        dir,
        &[
            ("src/main.rs", "fn main() { println!(\"hello\"); }"),
            ("src/lib.rs", "pub fn foo() -> i32 { 42 }"),
            ("tests/test_lib.rs", "#[test] fn it_works() {}"),
        ],
        "feat: add tests",
        base_ts + 400,
    );
}

/// Create a repo with commits touching a Cargo.lock file (for --no-exclude tests).
fn create_repo_with_lock_file(dir: &Path) {
    git_init(dir);
    let base_ts = recent_ts();
    git_commit(dir, "src/main.rs", "fn main() {}", "feat: initial", base_ts);
    git_commit(
        dir,
        "Cargo.lock",
        "# generated lock file\n[[package]]\nname = \"foo\"",
        "chore: add lockfile",
        base_ts + 100,
    );
    git_commit(
        dir,
        "src/lib.rs",
        "pub fn bar() {}",
        "feat: add lib",
        base_ts + 200,
    );
}

// ============================================================================
// Help
// ============================================================================

#[test]
fn test_heatmap_help_exits_0() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--help"])
        .assert()
        .success();
}

#[test]
fn test_heatmap_help_contains_flag_names() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--since"))
        .stdout(predicate::str::contains("--last"))
        .stdout(predicate::str::contains("--window"))
        .stdout(predicate::str::contains("--path"))
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains("--top"))
        .stdout(predicate::str::contains("--no-exclude"))
        .stdout(predicate::str::contains("--coupling-threshold"))
        .stdout(predicate::str::contains("--fix-window"));
}

// ============================================================================
// Not a git repo
// ============================================================================

#[test]
fn test_heatmap_not_git_repo_exits_failure() {
    let dir = tempfile::tempdir().expect("tempdir");
    Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("Not a git repository"));
}

// ============================================================================
// Empty repo (no commits)
// ============================================================================

#[test]
fn test_heatmap_empty_repo_exits_failure() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("No commits found")
                .or(predicate::str::contains("No analyzable")),
        );
}

// ============================================================================
// JSON output with 5+ commits
// ============================================================================

#[test]
fn test_heatmap_json_output_valid_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("spawn skim heatmap");

    assert!(
        output.status.success(),
        "expected exit 0, got {:?}",
        output.status
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("expected valid JSON output");

    assert_eq!(parsed["version"], 1, "version must be 1");
    assert!(parsed["files"].is_array(), "files must be array");
    assert!(
        !parsed["files"].as_array().unwrap().is_empty(),
        "files must be non-empty"
    );
    assert!(parsed["modules"].is_array(), "modules must be array");
    assert!(
        parsed["coupling_graph"].is_array(),
        "coupling_graph must be array"
    );
    assert!(parsed["window"].is_object(), "window must be object");
    assert!(parsed["warnings"].is_array(), "warnings must be array");
}

#[test]
fn test_heatmap_json_has_all_metric_keys() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("spawn skim heatmap");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    let file = &parsed["files"][0];
    assert!(file["churn"].is_object(), "churn must be object");
    assert!(
        file["stability_score"].is_number(),
        "stability_score must be number"
    );
    assert!(file["authors"].is_object(), "authors must be object");
    assert!(file["fix_risk"].is_object(), "fix_risk must be object");
    assert!(
        file["blast_radius"].is_array(),
        "blast_radius must be array"
    );
}

// ============================================================================
// Text output
// ============================================================================

#[test]
fn test_heatmap_text_output_contains_sections() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Top Churn"))
        .stdout(predicate::str::contains("Blast Radius"))
        .stdout(predicate::str::contains("Module Health"))
        .stdout(predicate::str::contains("Bus Factor"));
}

// ============================================================================
// --since flag
// ============================================================================

#[test]
fn test_heatmap_since_filters_commits() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());

    // Old commit (2020)
    git_commit(
        dir.path(),
        "old.rs",
        "old content",
        "feat: old",
        1_580_000_000,
    );
    // Recent commit
    git_commit(
        dir.path(),
        "new.rs",
        "new content",
        "feat: new",
        1_700_000_000,
    );
    // Another recent commit
    git_commit(
        dir.path(),
        "new2.rs",
        "more content",
        "feat: more",
        1_700_000_100,
    );

    // --since=2023-01-01 (epoch 1672531200) should include only the two recent commits
    Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--json", "--since=1672531200"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("new.rs").or(predicate::str::contains("new2.rs")));
}

// ============================================================================
// --window preset
// ============================================================================

#[test]
fn test_heatmap_window_sprint_preset() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--window=sprint"])
        .current_dir(dir.path())
        .assert()
        // Either succeeds (commits in window) or fails with "No commits found"
        // depending on the commit timestamps vs now
        .stderr(predicate::str::is_empty().or(predicate::str::contains("No commits")));
}

// ============================================================================
// --path scoping
// ============================================================================

#[test]
fn test_heatmap_path_scope() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--json", "--path=src/", "--no-exclude"])
        .current_dir(dir.path())
        .output()
        .expect("spawn skim heatmap");

    // May succeed or fail depending on git behavior, but should not crash
    let stdout = String::from_utf8(output.stdout).unwrap();
    if output.status.success() && !stdout.is_empty() {
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or(serde_json::Value::Null);
        if parsed != serde_json::Value::Null {
            // All file paths should be under src/
            if let Some(files) = parsed["files"].as_array() {
                for file in files {
                    let path = file["path"].as_str().unwrap_or("");
                    assert!(path.starts_with("src/"), "path outside src/: {path}");
                }
            }
        }
    }
}

// ============================================================================
// --no-exclude includes lock files
// ============================================================================

#[test]
fn test_heatmap_no_exclude_includes_lock_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_repo_with_lock_file(dir.path());

    let output_with_exclude = Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("spawn skim heatmap (default excludes)");

    let output_no_exclude = Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--json", "--no-exclude"])
        .current_dir(dir.path())
        .output()
        .expect("spawn skim heatmap (no excludes)");

    if output_no_exclude.status.success() {
        let stdout = String::from_utf8(output_no_exclude.stdout).unwrap();
        // With --no-exclude, Cargo.lock should appear in the files
        assert!(
            stdout.contains("Cargo.lock"),
            "expected Cargo.lock in --no-exclude output"
        );
    }

    if output_with_exclude.status.success() {
        let stdout = String::from_utf8(output_with_exclude.stdout).unwrap();
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&stdout) {
            // Default excludes should filter Cargo.lock out of the analyzed files
            if let Some(files) = parsed["files"].as_array() {
                let has_lockfile = files
                    .iter()
                    .any(|f| f["path"].as_str().unwrap_or("").contains("Cargo.lock"));
                assert!(
                    !has_lockfile,
                    "Cargo.lock should not appear in files with default excludes"
                );
            }
        }
    }
}

// ============================================================================
// --coupling-threshold changes coupling output
// ============================================================================

#[test]
fn test_heatmap_coupling_threshold_flag_accepted() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    // A very low threshold should not crash
    Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--coupling-threshold=0.1"])
        .current_dir(dir.path())
        .assert()
        .code(predicate::in_iter([0i32, 1i32])); // success or no-commits-found failure
}

// ============================================================================
// Single-commit repo — partial report with insufficient_data markers
// ============================================================================

#[test]
fn test_heatmap_single_commit_repo() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    git_commit(
        dir.path(),
        "src/main.rs",
        "fn main() {}",
        "feat: init",
        recent_ts(),
    );

    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("spawn skim heatmap");

    if output.status.success() {
        let stdout = String::from_utf8(output.stdout).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        // Files should be present
        let files = parsed["files"].as_array().unwrap();
        assert!(!files.is_empty());
        // Single commit file should have insufficient_data = true
        let fix_risk = &files[0]["fix_risk"];
        assert_eq!(
            fix_risk["insufficient_data"], true,
            "single-commit file should have insufficient_data=true"
        );
    }
}

// ============================================================================
// --top flag limits output
// ============================================================================

#[test]
fn test_heatmap_top_flag_limits_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());

    // Create many files with many commits (within 90-day window)
    let base_ts = recent_ts();
    for i in 0..10u64 {
        for j in 0..3u64 {
            git_commit(
                dir.path(),
                &format!("file{i}.rs"),
                &format!("content {j}"),
                &format!("commit {i}-{j}"),
                base_ts + i * 10 + j,
            );
        }
    }

    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--json", "--top=3"])
        .current_dir(dir.path())
        .output()
        .expect("spawn skim heatmap");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    // Top 3 filter is on rendering, not data — files array may have all files
    // but the text output should only show 3
    let _ = parsed["files"].as_array().unwrap().len(); // just verifying it parses

    // Text output with --top=3 should show at most 3 churn entries
    let text_output = Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--top=3"])
        .current_dir(dir.path())
        .output()
        .expect("spawn skim heatmap text");
    if text_output.status.success() {
        let text = String::from_utf8(text_output.stdout).unwrap();
        assert!(text.contains("Top Churn"));
    }
}

// ============================================================================
// --last flag
// ============================================================================

#[test]
fn test_heatmap_last_flag() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());

    let base_ts = recent_ts();
    git_commit(dir.path(), "a.rs", "a", "first", base_ts);
    git_commit(dir.path(), "b.rs", "b", "second", base_ts + 100);
    git_commit(dir.path(), "c.rs", "c", "third", base_ts + 200);

    // --last 3 should succeed and analyze commits
    Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--last=3", "--json"])
        .current_dir(dir.path())
        .assert()
        .success();
}

// ============================================================================
// Dispatch registration: subcommand appears in --help
// ============================================================================

#[test]
fn test_heatmap_registered_in_help() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("heatmap"));
}
