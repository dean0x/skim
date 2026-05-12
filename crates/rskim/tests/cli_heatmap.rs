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

    let add_out = StdCommand::new("git")
        .args(["add", filename])
        .current_dir(dir)
        .output()
        .expect("git add");
    assert!(
        add_out.status.success(),
        "git add failed for {filename}: {}",
        String::from_utf8_lossy(&add_out.stderr)
    );

    let ts = format!("{timestamp}");
    let commit_out = StdCommand::new("git")
        .args(["commit", "-m", message])
        .env("GIT_AUTHOR_DATE", &ts)
        .env("GIT_COMMITTER_DATE", &ts)
        .current_dir(dir)
        .output()
        .expect("git commit");
    assert!(
        commit_out.status.success(),
        "git commit failed for {message}: {}",
        String::from_utf8_lossy(&commit_out.stderr)
    );
}

/// Commit multiple files at once.
fn git_commit_files(dir: &Path, files: &[(&str, &str)], message: &str, timestamp: u64) {
    for (filename, content) in files {
        let file_path = dir.join(filename);
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&file_path, content).expect("write file");
        let add_out = StdCommand::new("git")
            .args(["add", filename])
            .current_dir(dir)
            .output()
            .expect("git add");
        assert!(
            add_out.status.success(),
            "git add failed for {filename}: {}",
            String::from_utf8_lossy(&add_out.stderr)
        );
    }

    let ts = format!("{timestamp}");
    let commit_out = StdCommand::new("git")
        .args(["commit", "-m", message])
        .env("GIT_AUTHOR_DATE", &ts)
        .env("GIT_COMMITTER_DATE", &ts)
        .current_dir(dir)
        .output()
        .expect("git commit");
    assert!(
        commit_out.status.success(),
        "git commit failed for {message}: {}",
        String::from_utf8_lossy(&commit_out.stderr)
    );
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
        .stdout(predicate::str::contains("--fix-window"))
        .stdout(predicate::str::contains("--diff"));
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
        .stdout(predicate::str::contains("new.rs").or(predicate::str::contains("new2.rs")))
        .stdout(predicate::str::contains("old.rs").not());
}

// ============================================================================
// --window preset
// ============================================================================

#[test]
fn test_heatmap_window_sprint_preset() {
    let dir = tempfile::tempdir().expect("tempdir");
    // create_test_repo uses recent_ts() (7 days ago), well within sprint (14 days)
    create_test_repo(dir.path());

    Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--window=sprint"])
        .current_dir(dir.path())
        .assert()
        .success();
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

    assert!(
        output.status.success(),
        "expected exit 0 for --path=src/, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("expected valid JSON output");
    // All file paths should be under src/
    if let Some(files) = parsed["files"].as_array() {
        for file in files {
            let path = file["path"].as_str().unwrap_or("");
            assert!(path.starts_with("src/"), "path outside src/: {path}");
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

    assert!(
        output_no_exclude.status.success(),
        "expected exit 0 with --no-exclude, stderr: {}",
        String::from_utf8_lossy(&output_no_exclude.stderr)
    );
    let stdout_no_exclude = String::from_utf8(output_no_exclude.stdout).unwrap();
    // With --no-exclude, Cargo.lock should appear in the files
    assert!(
        stdout_no_exclude.contains("Cargo.lock"),
        "expected Cargo.lock in --no-exclude output"
    );

    assert!(
        output_with_exclude.status.success(),
        "expected exit 0 with default excludes, stderr: {}",
        String::from_utf8_lossy(&output_with_exclude.stderr)
    );
    let stdout_with_exclude = String::from_utf8(output_with_exclude.stdout).unwrap();
    let parsed = serde_json::from_str::<serde_json::Value>(&stdout_with_exclude)
        .expect("expected valid JSON with default excludes");
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
        .success();
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

    assert!(
        output.status.success(),
        "expected exit 0 for single-commit repo, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("expected valid JSON output");
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

    assert!(
        output.status.success(),
        "expected exit 0 for --json --top=3, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("expected valid JSON output");
    // In JSON mode, --top=3 truncates the files array to at most 3 entries
    let files = parsed["files"].as_array().unwrap();
    assert!(
        files.len() <= 3,
        "expected at most 3 files in JSON output with --top=3, got {}",
        files.len()
    );

    // Text output with --top=3 should succeed and show Top Churn section
    let text_output = Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--top=3"])
        .current_dir(dir.path())
        .output()
        .expect("spawn skim heatmap text");
    assert!(
        text_output.status.success(),
        "expected exit 0 for text --top=3, stderr: {}",
        String::from_utf8_lossy(&text_output.stderr)
    );
    let text = String::from_utf8(text_output.stdout).unwrap();
    assert!(text.contains("Top Churn"));
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

// ============================================================================
// File targeting
// ============================================================================

#[test]
fn test_heatmap_explicit_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg("heatmap")
        .arg("--json")
        .arg("src/main.rs")
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    // Only targeted file in results
    let files = json["files"].as_array().unwrap();
    assert!(
        files
            .iter()
            .all(|f| f["path"].as_str().unwrap() == "src/main.rs"),
        "unexpected files: {files:?}"
    );

    // file_targets present
    let targets = json["file_targets"].as_array().unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].as_str().unwrap(), "src/main.rs");
}

#[test]
fn test_heatmap_explicit_files_text_header() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg("heatmap")
        .arg("src/main.rs")
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("scoped to 1 files"),
        "header missing scope: {stdout}"
    );
}

#[test]
fn test_heatmap_diff_and_files_mutual_exclusion() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    Command::cargo_bin("skim")
        .unwrap()
        .arg("heatmap")
        .arg("--diff")
        .arg("main")
        .arg("src/file.rs")
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot combine --diff"));
}

#[test]
fn test_heatmap_diff_bad_ref() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    Command::cargo_bin("skim")
        .unwrap()
        .arg("heatmap")
        .arg("--diff")
        .arg("nonexistent-branch")
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "base branch 'nonexistent-branch' not found",
        ));
}

#[test]
fn test_heatmap_file_not_in_history() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg("heatmap")
        .arg("--json")
        .arg("nonexistent.rs")
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let warnings = json["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|w| {
            let s = w.as_str().unwrap();
            s.contains("nonexistent.rs") && s.contains("not found in git history")
        }),
        "expected warning not found in: {warnings:?}"
    );
}

#[test]
fn test_heatmap_files_no_top_truncation() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg("heatmap")
        .arg("--json")
        .arg("src/main.rs")
        .arg("src/lib.rs")
        .arg("tests/test_lib.rs")
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let targets = json["file_targets"].as_array().unwrap();
    assert_eq!(targets.len(), 3);
}

#[test]
fn test_heatmap_no_targets_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg("heatmap")
        .arg("--json")
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        json.get("file_targets").is_none(),
        "file_targets should be absent when no targets given: {json}"
    );
}

#[test]
fn test_heatmap_diff_flag() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    // Create a branch from the current state
    StdCommand::new("git")
        .args(["checkout", "-b", "feature-test"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    // Add a new file on the branch
    git_commit(
        dir.path(),
        "src/new_file.rs",
        "fn new() {}",
        "add new file",
        recent_ts(),
    );

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg("heatmap")
        .arg("--diff")
        .arg("main")
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The diff output should include the scope annotation
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("scoped to"),
        "should show scoped header: {stdout}"
    );
}

#[test]
fn test_heatmap_diff_no_changes() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    Command::cargo_bin("skim")
        .unwrap()
        .arg("heatmap")
        .arg("--diff")
        .arg("HEAD")
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("no files changed vs 'HEAD'"));
}

#[test]
fn test_heatmap_coupling_preserved_with_targets() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ts = recent_ts();

    git_init(dir.path());
    // Create coupled files — always committed together
    for i in 0..6u64 {
        git_commit_files(
            dir.path(),
            &[
                ("src/config.rs", &format!("// v{i}\nfn config() {{}}")),
                ("src/main.rs", &format!("// v{i}\nfn main() {{}}")),
            ],
            &format!("update pair {i}"),
            ts + i * 86400,
        );
    }
    // Additional solo commits to make coupling < 1.0
    git_commit(
        dir.path(),
        "src/main.rs",
        "// solo",
        "solo main",
        ts + 7 * 86400,
    );

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg("heatmap")
        .arg("--json")
        .arg("src/main.rs")
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    // Coupling graph should include edge between main.rs and config.rs
    let coupling = json["coupling_graph"].as_array().unwrap();
    let has_config_edge = coupling.iter().any(|e| {
        let a = e["a"].as_str().unwrap();
        let b = e["b"].as_str().unwrap();
        (a == "src/main.rs" && b == "src/config.rs") || (a == "src/config.rs" && b == "src/main.rs")
    });
    assert!(
        has_config_edge,
        "coupling graph should include config.rs edge: {coupling:?}"
    );
}

// ============================================================================
// --insights flag
// ============================================================================

#[test]
fn test_insights_text_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--insights"])
        .env("NO_COLOR", "1")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Insights"));
}

#[test]
fn test_insights_text_no_metric_sections() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    // --insights should produce only the Insights section, not the full heatmap sections
    Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--insights"])
        .env("NO_COLOR", "1")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Top Churn").not())
        .stdout(predicate::str::contains("Blast Radius").not())
        .stdout(predicate::str::contains("Module Health").not())
        .stdout(predicate::str::contains("Bus Factor").not());
}

#[test]
fn test_insights_json_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--insights", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("spawn skim heatmap --insights --json");

    assert!(
        output.status.success(),
        "expected exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("expected valid JSON from --insights --json");

    assert!(parsed["insights"].is_array(), "insights must be array");
    assert_eq!(parsed["version"], 1, "version must be 1");
}

#[test]
fn test_insights_json_has_top_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--insights", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("spawn skim heatmap --insights --json");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    assert!(
        parsed["top_files"].is_array(),
        "top_files must be array in insights JSON"
    );
    assert!(
        parsed["flagged_modules"].is_array(),
        "flagged_modules must be array in insights JSON"
    );
}

#[test]
fn test_json_without_insights_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("spawn skim heatmap --json");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    // Regular --json output should NOT have insights-specific top-level keys
    assert!(
        parsed.get("insights").is_none(),
        "regular --json should not have 'insights' key"
    );
    assert!(
        parsed.get("top_files").is_none(),
        "regular --json should not have 'top_files' key"
    );
    // But it should have the standard heatmap keys
    assert!(parsed["files"].is_array(), "files must be present");
    assert!(parsed["modules"].is_array(), "modules must be present");
}

#[test]
fn test_insights_empty_repo() {
    // A repo with a single commit yields very few metrics — insights should gracefully handle it
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    let base_ts = recent_ts();
    git_commit(
        dir.path(),
        "src/main.rs",
        "fn main() {}",
        "feat: initial",
        base_ts,
    );

    // With only 1 commit, the tool might succeed or fail depending on min threshold.
    // Both outcomes are valid; assert specific behavior on each path so a panic or
    // unexpected output is caught rather than silently passing.
    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--insights"])
        .env("NO_COLOR", "1")
        .current_dir(dir.path())
        .output()
        .expect("spawn skim heatmap --insights");

    if output.status.success() {
        let stdout = String::from_utf8(output.stdout).unwrap();
        // Either findings or empty-state message — any other output is a regression.
        assert!(
            stdout.contains("Insights") || stdout.contains("no notable findings"),
            "expected Insights header or empty-state message on success, got: {stdout}"
        );
    } else {
        let stderr = String::from_utf8(output.stderr).unwrap();
        // On failure the tool must emit a known diagnostic — not a panic or silent crash.
        assert!(
            stderr.contains("No commits") || stderr.contains("No analyzable"),
            "expected known diagnostic on non-zero exit, got stderr: {stderr}"
        );
    }
}

#[test]
fn test_insights_with_file_targeting() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    // --insights with a positional file arg should succeed and show insights output
    Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--insights", "src/main.rs"])
        .env("NO_COLOR", "1")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Insights"));
}

#[test]
fn test_insights_help_text() {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["heatmap", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--insights"));
}
