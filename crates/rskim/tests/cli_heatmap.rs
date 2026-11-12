//! Integration tests for `skim heatmap` subcommand.
//!
//! Tests end-to-end CLI behavior for git history risk/coupling analysis.
//! For tests that need a git repo, we create a TempDir and set up deterministic commits.

use predicates::prelude::*;
use std::path::Path;
use std::process::Command as StdCommand;
mod common;

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
    common::skim()
        .args(["heatmap", "--help"])
        .assert()
        .success();
}

#[test]
fn test_heatmap_help_contains_flag_names() {
    common::skim()
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

#[test]
fn test_heatmap_help_describes_the_short_view_detail_and_cards() {
    let out = common::skim()
        .args(["heatmap", "--help"])
        .output()
        .expect("spawn skim heatmap --help");
    assert!(out.status.success());
    let help = String::from_utf8(out.stdout).unwrap();

    let detail = help
        .lines()
        .find(|l| l.trim_start().starts_with("--detail"))
        .unwrap_or_else(|| panic!("no --detail line:\n{help}"));
    assert!(
        detail.contains("hotspots|coupling|fix-risk|modules|bus-factor"),
        "{detail}"
    );
    for needle in ["CHANGELOG.md", "--no-exclude", "version 2", "5 commits"] {
        assert!(help.contains(needle), "help has no {needle:?}:\n{help}");
    }
    for stale in ["default: 20", "agent-friendly", "Top Churn", "%%"] {
        assert!(!help.contains(stale), "help still says {stale:?}:\n{help}");
    }
}

// ============================================================================
// Not a git repo
// ============================================================================

#[test]
fn test_heatmap_not_git_repo_exits_failure() {
    let dir = tempfile::tempdir().expect("tempdir");
    common::skim()
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
    common::skim()
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

    let output = common::skim()
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
    assert_eq!(stdout.lines().count(), 1, "compact JSON is one line");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("expected valid JSON output");

    assert_eq!(parsed["version"], 2, "version must be 2");
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

    let output = common::skim()
        .args(["heatmap", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("spawn skim heatmap");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    // The hotspot order puts src/main.rs (4 commits) first.
    let file = &parsed["files"][0];
    assert_eq!(file["path"], "src/main.rs");
    assert_eq!(file["churn"]["commits"], 4, "nested churn.commits");
    assert!(
        file["stability_score"].is_number(),
        "stability_score must be number"
    );
    assert!(file["authors"].is_object(), "authors must be object");
    assert_eq!(
        file["fix_risk"]["keyword_pct"], 25.0,
        "nested fix_risk.keyword_pct: 1 fix commit of 4"
    );
    assert_eq!(
        file["fix_risk"]["insufficient_data"], true,
        "nested fix_risk.insufficient_data: 4 commits are below support"
    );
    assert!(
        file["blast_radius"].is_array(),
        "blast_radius must be array"
    );
    assert!(
        parsed.get("file_targets").is_none(),
        "file_targets only for a scoped run"
    );
}

// ============================================================================
// Text output
// ============================================================================

#[test]
fn test_heatmap_text_output_contains_sections() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    let text = heatmap_stdout(dir.path(), &[]);
    let headings: Vec<&str> = text
        .lines()
        .skip(1)
        .filter(|l| !l.starts_with(' '))
        .collect();
    assert_eq!(
        headings,
        [
            "Hotspots (commits):",
            "Coupling (confidence, shared commits):",
            "Fix risk (fix commits/commits, lift, base rate):",
        ],
        "module health and bus factor are not in the default view:\n{text}"
    );
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
    common::skim()
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

    common::skim()
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

    let output = common::skim()
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

    let output_with_exclude = common::skim()
        .args(["heatmap", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("spawn skim heatmap (default excludes)");

    let output_no_exclude = common::skim()
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
    common::skim()
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

    let output = common::skim()
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

    let output = common::skim()
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
    // In JSON mode, --top=3 gives exactly 3 of the 10 files.
    let files = parsed["files"].as_array().unwrap();
    assert_eq!(files.len(), 3, "files with --top=3");
    assert_eq!(
        parsed["counts"]["hotspots"],
        serde_json::json!({"total": 10, "shown": 3})
    );

    // Text output with --top=3 shows 3 hotspot rows and counts the other 7.
    let text = heatmap_stdout(dir.path(), &["--top=3"]);
    assert_eq!(section_rows(&text, "Hotspots").len(), 3, "{text}");
    assert!(
        section_lines(&text, "Hotspots").contains(&"  7 more (--detail hotspots)"),
        "{text}"
    );
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
    common::skim()
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
    common::skim()
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

    let output = common::skim()
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

    let output = common::skim()
        .arg("heatmap")
        .arg("src/main.rs")
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let header = stdout.lines().next().unwrap_or_default();
    assert!(
        header.starts_with("Heatmap: ") && header.ends_with(", scoped to 1 files"),
        "header missing scope: {stdout}"
    );
}

#[test]
fn test_heatmap_diff_and_files_mutual_exclusion() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    common::skim()
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

    common::skim()
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

/// Sorted names of the top-level entries in `dir`.
fn dir_entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("read_dir")
        .map(|e| {
            e.expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// Heatmap flag values reach git as values only: a `--diff` value that
/// starts with `-` exits 1 with a message, and no file appears in the repo.
fn assert_diff_dash_value_refused(diff_args: &[&str]) {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());
    let before = dir_entries(dir.path());

    let output = common::skim()
        .arg("heatmap")
        .args(diff_args)
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert_eq!(
        dir_entries(dir.path()),
        before,
        "no file may appear in the repo for args {diff_args:?}"
    );
    assert_eq!(output.status.code(), Some(1), "args {diff_args:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--diff value must not start with '-'"),
        "stderr: {stderr}"
    );
}

#[test]
fn test_heatmap_diff_dash_value_space_form_refused() {
    assert_diff_dash_value_refused(&["--diff", "--output=heatmap-out.txt"]);
}

#[test]
fn test_heatmap_diff_dash_value_equals_form_refused() {
    assert_diff_dash_value_refused(&["--diff=--output=heatmap-out.txt"]);
}

#[test]
fn test_heatmap_file_not_in_history() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    // The card of a file with no commit in the window says so; the
    // warnings do not repeat it.
    let json = heatmap_json_with(dir.path(), &["nonexistent.rs"]);
    assert_eq!(
        json["cards"],
        serde_json::json!([{
            "path": "nonexistent.rs",
            "in_window": false,
            "commits": 0,
            "counts": {
                "owners": {"total": 0, "shown": 0},
                "partners": {"total": 0, "shown": 0},
                "fix_commits": {"total": 0, "shown": 0}
            },
            "owners": [],
            "partners": [],
            "fix_commits": []
        }])
    );
    assert_eq!(json["warnings"], serde_json::json!([]));

    let since = json["window"]["since"].as_str().unwrap();
    let text = heatmap_stdout(dir.path(), &["nonexistent.rs"]);
    assert_eq!(
        card_lines(&text, "nonexistent.rs"),
        [format!(
            "nonexistent.rs: not in history window (5 commits since {since})"
        )]
    );
}

#[test]
fn test_heatmap_files_no_top_truncation() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    let output = common::skim()
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

    let output = common::skim()
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

    let output = common::skim()
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
    // The diff output should include the scope annotation: one changed file.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let header = stdout.lines().next().unwrap_or_default();
    assert!(
        header.starts_with("Heatmap: ") && header.ends_with(", scoped to 1 files"),
        "should show scoped header: {stdout}"
    );
}

#[test]
fn test_heatmap_diff_no_changes() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    common::skim()
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

    let output = common::skim()
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

    common::skim()
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
    common::skim()
        .args(["heatmap", "--insights"])
        .env("NO_COLOR", "1")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Hotspots (").not())
        .stdout(predicate::str::contains("Coupling (").not())
        .stdout(predicate::str::contains("Fix risk (").not())
        .stdout(predicate::str::contains("Modules (").not())
        .stdout(predicate::str::contains("Bus factor (").not());
}

#[test]
fn test_insights_json_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    let output = common::skim()
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
    assert_eq!(stdout.lines().count(), 1, "compact JSON is one line");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("expected valid JSON from --insights --json");

    assert!(parsed["insights"].is_array(), "insights must be array");
    assert_eq!(parsed["version"], 2, "version must be 2");
}

#[test]
fn test_insights_json_has_top_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    let output = common::skim()
        .args(["heatmap", "--insights", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("spawn skim heatmap --insights --json");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    // No file reaches 5 commits and no pair 3 shared commits, and `src`
    // keeps 4 of its 5 commits inside: no finding, so the lists are empty.
    assert_eq!(parsed["insights"], serde_json::json!([]));
    assert_eq!(parsed["top_files"], serde_json::json!([]));
    assert_eq!(parsed["flagged_modules"], serde_json::json!([]));
    assert_eq!(
        parsed["counts"]["insights"],
        serde_json::json!({"total": 0, "shown": 0})
    );
}

#[test]
fn test_json_without_insights_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    let output = common::skim()
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
    // But it should have the standard heatmap keys. Module health is not in
    // the default view: the array is empty and the count gives the total.
    assert!(parsed["files"].is_array(), "files must be present");
    assert_eq!(parsed["modules"], serde_json::json!([]));
    assert_eq!(
        parsed["counts"]["modules"],
        serde_json::json!({"total": 1, "shown": 0})
    );
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

    // One commit is below every support minimum: exit 0, the header line,
    // and the empty-state line only.
    let stdout = heatmap_stdout(dir.path(), &["--insights"]);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{stdout}");
    assert!(lines[0].starts_with("Insights: "), "{stdout}");
    assert!(lines[0].contains(", 1 commits, "), "{stdout}");
    assert_eq!(lines[1], "  (no notable findings)");
}

#[test]
fn test_insights_with_file_targeting() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    // With a file argument, --insights shows the same card as the plain view.
    let insights = heatmap_stdout(dir.path(), &["--insights", "src/main.rs"]);
    assert_eq!(insights, heatmap_stdout(dir.path(), &["src/main.rs"]));
    assert_eq!(
        card_lines(&insights, "src/main.rs").first().copied(),
        Some("src/main.rs: 4 commits"),
        "{insights}"
    );
}

#[test]
fn test_insights_help_text() {
    common::skim()
        .args(["heatmap", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--insights"));
}

// ============================================================================
// Noise and metrics: pair support, the coupling file cap, fix proximity
// ============================================================================

/// Run `skim heatmap --json` in `dir` and parse stdout.
fn heatmap_json(dir: &Path) -> serde_json::Value {
    heatmap_json_with(dir, &[])
}

/// Run `skim heatmap --json <args>` in `dir` and parse stdout.
fn heatmap_json_with(dir: &Path, args: &[&str]) -> serde_json::Value {
    let output = common::skim()
        .args(["heatmap", "--json"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn skim heatmap --json");
    assert!(
        output.status.success(),
        "skim heatmap --json {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("valid heatmap JSON")
}

/// The `files[]` entry of `path` in heatmap JSON.
fn file_entry<'a>(json: &'a serde_json::Value, path: &str) -> &'a serde_json::Value {
    json["files"]
        .as_array()
        .expect("files array")
        .iter()
        .find(|f| f["path"] == path)
        .unwrap_or_else(|| panic!("{path} missing from files[]"))
}

#[test]
fn test_heatmap_coupling_needs_three_shared_commits() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ts = recent_ts();
    git_init(dir.path());
    // src/a.rs and src/b.rs share 2 commits; src/c.rs and src/d.rs share 3.
    for i in 0..2u64 {
        git_commit_files(
            dir.path(),
            &[
                ("src/a.rs", &format!("// a{i}")),
                ("src/b.rs", &format!("// b{i}")),
            ],
            &format!("feat: ab {i}"),
            ts + i * 100,
        );
    }
    for i in 0..3u64 {
        git_commit_files(
            dir.path(),
            &[
                ("src/c.rs", &format!("// c{i}")),
                ("src/d.rs", &format!("// d{i}")),
            ],
            &format!("feat: cd {i}"),
            ts + 1000 + i * 100,
        );
    }

    let json = heatmap_json(dir.path());

    let edges: Vec<(String, String, u64)> = json["coupling_graph"]
        .as_array()
        .expect("coupling_graph array")
        .iter()
        .map(|e| {
            (
                e["a"].as_str().unwrap().to_string(),
                e["b"].as_str().unwrap().to_string(),
                e["support"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        edges,
        vec![("src/c.rs".to_string(), "src/d.rs".to_string(), 3)],
        "only the pair with 3 shared commits is coupled"
    );
    assert_eq!(
        file_entry(&json, "src/a.rs")["blast_radius"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "a pair with 2 shared commits has no blast radius"
    );
}

/// A repo with one commit of 60 files and one later single-file commit.
fn create_repo_with_60_file_commit(dir: &Path) {
    git_init(dir);
    let ts = recent_ts();
    let names: Vec<String> = (0..60).map(|i| format!("bulk/f{i:02}.rs")).collect();
    let files: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "// bulk")).collect();
    git_commit_files(dir, &files, "chore: bulk reformat", ts);
    git_commit(dir, "src/main.rs", "fn main() {}", "feat: main", ts + 100);
}

#[test]
fn test_heatmap_large_commit_warning_in_text_and_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_repo_with_60_file_commit(dir.path());
    let expected = "coupling: 1 commit with more than 50 files not paired";

    let json = heatmap_json(dir.path());
    let json_hits: Vec<&str> = json["warnings"]
        .as_array()
        .expect("warnings array")
        .iter()
        .filter_map(|w| w.as_str())
        .filter(|w| w.contains("files not paired"))
        .collect();
    assert_eq!(json_hits, vec![expected]);
    assert_eq!(
        json["coupling_graph"].as_array().unwrap().len(),
        0,
        "the 60-file commit makes no pairs"
    );

    let output = common::skim()
        .args(["heatmap"])
        .current_dir(dir.path())
        .output()
        .expect("spawn skim heatmap");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let text_hits: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("files not paired"))
        .collect();
    assert_eq!(text_hits.len(), 1, "one warning line, got: {stdout}");
    assert!(
        text_hits[0].ends_with(expected),
        "warning line: {}",
        text_hits[0]
    );
}

#[test]
fn test_heatmap_proximity_counts_feat_then_later_fix() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ts = recent_ts();
    git_init(dir.path());
    git_commit(dir.path(), "src/a.rs", "// v1", "feat: add a", ts);
    git_commit(dir.path(), "src/a.rs", "// v2", "fix: repair a", ts + 100);

    let json = heatmap_json(dir.path());
    let fix_risk = &file_entry(&json, "src/a.rs")["fix_risk"];
    // 2 commits; the feat is followed by a later fix: 1 / 2 = 50%.
    assert_eq!(fix_risk["proximity_pct"].as_f64(), Some(50.0));
    assert_eq!(fix_risk["fix_commits"].as_u64(), Some(1));
}

#[test]
fn test_heatmap_proximity_ignores_fix_then_later_feat() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ts = recent_ts();
    git_init(dir.path());
    git_commit(dir.path(), "src/a.rs", "// v1", "fix: repair a", ts);
    git_commit(dir.path(), "src/a.rs", "// v2", "feat: extend a", ts + 100);

    let json = heatmap_json(dir.path());
    let fix_risk = &file_entry(&json, "src/a.rs")["fix_risk"];
    assert_eq!(fix_risk["proximity_pct"].as_f64(), Some(0.0));
    assert_eq!(fix_risk["fix_commits"].as_u64(), Some(1));
}

// ============================================================================
// Package modules and default excludes
// ============================================================================

/// Sorted `path` values of the JSON array `key`.
fn sorted_paths(json: &serde_json::Value, key: &str) -> Vec<String> {
    let mut paths: Vec<String> = json[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} array"))
        .iter()
        .map(|e| e["path"].as_str().expect("path string").to_string())
        .collect();
    paths.sort();
    paths
}

/// Changelog, snapshot and generated-code paths, plus two normal source files.
/// `src/schema.rs` has a "generated" marker only in its header.
const EXCLUDE_FIXTURE: &[(&str, &str)] = &[
    ("crates/x/CHANGELOG.md", "# Changelog"),
    ("tests/view.snap", "snapshot"),
    ("tests/snapshots/case.txt", "snapshot"),
    ("api/foo.pb.go", "package api"),
    ("py/foo_pb2.py", "# proto"),
    ("src/generated/types.rs", "// types"),
    ("src/schema.rs", "// @generated by a tool"),
    ("src/main.rs", "fn main() {}"),
];

#[test]
fn test_heatmap_default_excludes_changelog_snapshot_and_generated_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    let ts = recent_ts();
    // Five commits of every file, so the pairs reach the coupling support
    // and `src` reaches the module support.
    for i in 0..5u64 {
        let files: Vec<(&str, String)> = EXCLUDE_FIXTURE
            .iter()
            .map(|(p, c)| (*p, format!("{c} {i}")))
            .collect();
        let files: Vec<(&str, &str)> = files.iter().map(|(p, c)| (*p, c.as_str())).collect();
        git_commit_files(dir.path(), &files, &format!("feat: step {i}"), ts + i * 100);
    }
    let kept = vec!["src/main.rs".to_string(), "src/schema.rs".to_string()];

    // Module health is not in the default view; `--detail modules` gives it.
    let json = heatmap_json_with(dir.path(), &["--detail", "modules"]);
    assert_eq!(sorted_paths(&json, "files"), kept);
    assert_eq!(sorted_paths(&json, "modules"), vec!["src".to_string()]);
    let endpoints: Vec<String> = json["coupling_graph"]
        .as_array()
        .expect("coupling_graph array")
        .iter()
        .flat_map(|e| [e["a"].as_str().unwrap(), e["b"].as_str().unwrap()])
        .map(str::to_string)
        .collect();
    assert_eq!(endpoints.len(), 2, "one pair is left: {endpoints:?}");
    assert!(endpoints.iter().all(|p| kept.contains(p)), "{endpoints:?}");

    let text = common::skim()
        .args(["heatmap"])
        .current_dir(dir.path())
        .output()
        .expect("spawn skim heatmap");
    assert!(text.status.success());
    let stdout = String::from_utf8(text.stdout).unwrap();
    for (path, _) in &EXCLUDE_FIXTURE[..6] {
        assert!(!stdout.contains(path), "{path} in text output:\n{stdout}");
    }

    let all = common::skim()
        .args(["heatmap", "--json", "--no-exclude"])
        .current_dir(dir.path())
        .output()
        .expect("spawn skim heatmap --no-exclude");
    assert!(all.status.success());
    let all: serde_json::Value = serde_json::from_slice(&all.stdout).expect("valid JSON");
    let mut every: Vec<String> = EXCLUDE_FIXTURE.iter().map(|(p, _)| p.to_string()).collect();
    every.sort();
    assert_eq!(sorted_paths(&all, "files"), every);
}

/// (path, total_commits) of each JSON module, sorted by path.
fn module_commits(json: &serde_json::Value) -> Vec<(String, u64)> {
    let mut modules: Vec<(String, u64)> = json["modules"]
        .as_array()
        .expect("modules array")
        .iter()
        .map(|m| {
            (
                m["path"].as_str().unwrap().to_string(),
                m["total_commits"].as_u64().unwrap(),
            )
        })
        .collect();
    modules.sort();
    modules
}

/// A Cargo workspace and an npm package, every manifest with invalid content.
fn create_package_repo(dir: &Path) {
    git_init(dir);
    let ts = recent_ts();
    git_commit_files(
        dir,
        &[
            ("Cargo.toml", "[workspace\nmembers = ["),
            ("crates/a/Cargo.toml", "[[[ not toml"),
            ("crates/b/Cargo.toml", "name = "),
            ("packages/x/package.json", "{ not json"),
        ],
        "chore: manifests",
        ts,
    );
    // Five commits of each source file, so every module reaches the module
    // support minimum of 5 commits.
    for i in 0..5u64 {
        for (n, path) in [
            "crates/a/src/lib.rs",
            "crates/b/src/lib.rs",
            "packages/x/index.js",
            "src/main.rs",
        ]
        .iter()
        .enumerate()
        {
            let at = ts + 100 + i * 1000 + n as u64 * 10;
            git_commit(
                dir,
                path,
                &format!("// {i}"),
                &format!("feat: {path} {i}"),
                at,
            );
        }
    }
}

#[test]
fn test_heatmap_modules_follow_package_manifests() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_package_repo(dir.path());

    // The manifest commit touches all three packages; the root manifest has
    // no module, and `src` keeps the top-folder rule.
    let json = heatmap_json_with(dir.path(), &["--detail", "modules"]);
    assert_eq!(
        module_commits(&json),
        vec![
            ("crates/a".to_string(), 6),
            ("crates/b".to_string(), 6),
            ("packages/x".to_string(), 6),
            ("src".to_string(), 5),
        ]
    );
}

#[test]
fn test_heatmap_file_scope_keeps_the_package_module() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_package_repo(dir.path());
    // `--detail` refuses file arguments, so the scope comes from `--diff`:
    // one changed file on a branch, the seventh commit of `crates/a`.
    StdCommand::new("git")
        .args(["checkout", "-b", "feature-scope"])
        .current_dir(dir.path())
        .output()
        .expect("git checkout");
    git_commit(
        dir.path(),
        "crates/a/src/lib.rs",
        "// branch",
        "feat: change a on a branch",
        recent_ts() + 5000,
    );

    let json = heatmap_json_with(dir.path(), &["--diff", "main", "--detail", "modules"]);
    assert_eq!(
        json["file_targets"],
        serde_json::json!(["crates/a/src/lib.rs"])
    );
    assert_eq!(module_commits(&json), vec![("crates/a".to_string(), 7)]);
}

// ============================================================================
// --detail
// ============================================================================

#[test]
fn test_heatmap_detail_unknown_name_lists_valid_names() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = common::skim()
        .args(["heatmap", "--detail", "churn"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty(), "no stdout on a bad --detail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "--detail must be one of hotspots, coupling, fix-risk, modules, bus-factor; got: churn"
        ),
        "stderr: {stderr}"
    );
}

// ============================================================================
// Text view: header, sections, caps
// ============================================================================

/// Run `skim heatmap <args>` in `dir`, require exit 0, and return stdout.
fn heatmap_stdout(dir: &Path, args: &[&str]) -> String {
    let output = common::skim()
        .arg("heatmap")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn skim heatmap");
    assert!(
        output.status.success(),
        "skim heatmap {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf-8 stdout")
}

/// The indented lines under the text heading that starts with `heading`.
fn section_lines<'a>(text: &'a str, heading: &str) -> Vec<&'a str> {
    text.lines()
        .skip_while(|l| !l.starts_with(heading))
        .skip(1)
        .take_while(|l| l.starts_with("  "))
        .collect()
}

/// The rows of a text section: its lines without the "more" line and
/// without a "(none" line.
fn section_rows<'a>(text: &'a str, heading: &str) -> Vec<&'a str> {
    section_lines(text, heading)
        .into_iter()
        .filter(|l| !l.contains(" more (") && !l.trim_start().starts_with('('))
        .collect()
}

/// Commit new content for every path of `paths` in one commit.
fn git_commit_paths(dir: &Path, paths: &[String], message: &str, timestamp: u64) {
    for path in paths {
        let file_path = dir.join(path);
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).expect("create dirs");
        }
        std::fs::write(&file_path, format!("// {message}\n")).expect("write file");
    }
    let add_out = StdCommand::new("git")
        .arg("add")
        .arg("--")
        .args(paths)
        .current_dir(dir)
        .output()
        .expect("git add");
    assert!(
        add_out.status.success(),
        "git add failed: {}",
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

/// 40 files with many ties, one author, 62 commits:
/// - `src/hub/h0.rs`..`h7.rs`: 6 commits that touch all 8 (28 pairs with 6
///   shared commits each; each hub file has 7 partners);
/// - `src/pair/pNa.rs` with `src/pair/pNb.rs`, N = 0..8: 5 shared commits
///   per pair (8 pairs). Pairs 0-4 have only fix commits, so their 10 files
///   are above the fix base rate (25 of 62 commits);
/// - `src/solo/s00.rs`..`s15.rs`: 1 commit each.
fn create_ranked_repo(dir: &Path) {
    git_init(dir);
    let mut ts = recent_ts();
    let mut next_ts = move || {
        ts += 60;
        ts
    };
    let hub: Vec<String> = (0..8).map(|i| format!("src/hub/h{i}.rs")).collect();
    for c in 0..6 {
        git_commit_paths(dir, &hub, &format!("feat: hub {c}"), next_ts());
    }
    for p in 0..8 {
        let pair = [format!("src/pair/p{p}a.rs"), format!("src/pair/p{p}b.rs")];
        let kind = if p < 5 { "fix" } else { "feat" };
        for c in 0..5 {
            git_commit_paths(dir, &pair, &format!("{kind}: pair {p} {c}"), next_ts());
        }
    }
    for s in 0..16 {
        let solo = [format!("src/solo/s{s:02}.rs")];
        git_commit_paths(dir, &solo, &format!("feat: solo {s}"), next_ts());
    }
}

#[test]
fn test_heatmap_default_text_fits_in_thirty_lines_with_more_lines() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_ranked_repo(dir.path());

    let text = heatmap_stdout(dir.path(), &[]);
    let lines: Vec<&str> = text.lines().collect();
    // header 1; hotspots 1 + 10 + 1; coupling 1 + 5 + 1; fix risk 1 + 5 + 1.
    assert_eq!(lines.len(), 27, "{text}");
    assert!(lines.len() <= 30);
    for more in [
        "  30 more (--detail hotspots)",
        "  31 more (--detail coupling)",
        "  5 more (--detail fix-risk)",
    ] {
        assert!(lines.contains(&more), "missing {more:?}:\n{text}");
    }
}

#[test]
fn test_heatmap_text_ranks_ties_by_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_ranked_repo(dir.path());

    let text = heatmap_stdout(dir.path(), &[]);
    let mut expected: Vec<String> = (0..8).map(|i| format!("    6  src/hub/h{i}.rs")).collect();
    expected.push("    5  src/pair/p0a.rs".to_string());
    expected.push("    5  src/pair/p0b.rs".to_string());
    assert_eq!(section_rows(&text, "Hotspots"), expected);
    assert_eq!(
        section_rows(&text, "Coupling"),
        [
            "  100%   6  src/hub/h0.rs <-> src/hub/h1.rs",
            "  100%   6  src/hub/h0.rs <-> src/hub/h2.rs",
            "  100%   6  src/hub/h0.rs <-> src/hub/h3.rs",
            "  100%   6  src/hub/h0.rs <-> src/hub/h4.rs",
            "  100%   6  src/hub/h0.rs <-> src/hub/h5.rs",
        ]
    );
    // 5 fix commits of 5; base rate 25 / 62 = 40.3%; lift 1 / 0.403 = 2.5.
    assert_eq!(
        section_rows(&text, "Fix risk"),
        [
            "  5/5  lift 2.5  base 40%  src/pair/p0a.rs",
            "  5/5  lift 2.5  base 40%  src/pair/p0b.rs",
            "  5/5  lift 2.5  base 40%  src/pair/p1a.rs",
            "  5/5  lift 2.5  base 40%  src/pair/p1b.rs",
            "  5/5  lift 2.5  base 40%  src/pair/p2a.rs",
        ]
    );
}

#[test]
fn test_heatmap_hotspots_are_the_ten_highest_churn_files_in_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    let ts = recent_ts();
    // Commit k touches every file with k or more commits, so `src/fNN.rs`
    // gets NN commits. `src/e10.rs` ties with `src/f10.rs` on 10 commits.
    for k in 1..=12usize {
        let mut paths: Vec<String> = (k..=12).map(|i| format!("src/f{i:02}.rs")).collect();
        if k <= 10 {
            paths.push("src/e10.rs".to_string());
        }
        git_commit_paths(
            dir.path(),
            &paths,
            &format!("feat: step {k}"),
            ts + k as u64 * 60,
        );
    }

    let text = heatmap_stdout(dir.path(), &[]);
    let rows: Vec<(u64, &str)> = section_rows(&text, "Hotspots")
        .iter()
        .map(|row| {
            let (count, path) = row.trim_start().split_once("  ").expect("count and path");
            (count.parse().expect("commit count"), path)
        })
        .collect();
    assert_eq!(
        rows,
        [
            (12, "src/f12.rs"),
            (11, "src/f11.rs"),
            (10, "src/e10.rs"),
            (10, "src/f10.rs"),
            (9, "src/f09.rs"),
            (8, "src/f08.rs"),
            (7, "src/f07.rs"),
            (6, "src/f06.rs"),
            (5, "src/f05.rs"),
            (4, "src/f04.rs"),
        ]
    );
    assert!(
        section_lines(&text, "Hotspots").contains(&"  3 more (--detail hotspots)"),
        "{text}"
    );
}

#[test]
fn test_heatmap_text_header_states_the_window() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());

    let text = heatmap_stdout(dir.path(), &[]);
    let header = text.lines().next().expect("header line");
    let pattern = regex::Regex::new(
        r"^Heatmap: \S+ \d{4}-\d{2}-\d{2}\.\.\d{4}-\d{2}-\d{2}, 5 commits, dual window \((time|count)\)$",
    )
    .unwrap();
    assert!(pattern.is_match(header), "header: {header}");
    let name = dir.path().file_name().unwrap().to_str().unwrap();
    assert!(header.contains(name), "header has the repo name: {header}");
    assert!(!header.contains('/'), "header has no path: {header}");
}

/// A repo with 5 commits of `src/a.rs`, each with the message `prefix: N`.
fn create_five_commit_repo(dir: &Path, prefix: &str) {
    git_init(dir);
    let ts = recent_ts();
    for i in 0..5u64 {
        git_commit(
            dir,
            "src/a.rs",
            &format!("// {i}"),
            &format!("{prefix}: change {i}"),
            ts + i * 60,
        );
    }
}

#[test]
fn test_heatmap_fix_risk_gives_one_reason_line_at_zero_and_full_base_rate() {
    for (prefix, rate) in [("feat", "0%"), ("fix", "100%")] {
        let dir = tempfile::tempdir().expect("tempdir");
        create_five_commit_repo(dir.path(), prefix);

        let text = heatmap_stdout(dir.path(), &[]);
        assert_eq!(
            section_lines(&text, "Fix risk"),
            [format!("  (none: the fix base rate is {rate})")],
            "{text}"
        );
    }
}

#[test]
fn test_heatmap_detail_modules_text_lists_package_modules() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_package_repo(dir.path());

    let text = heatmap_stdout(dir.path(), &["--detail", "modules"]);
    let headings: Vec<&str> = text
        .lines()
        .skip(1)
        .filter(|l| !l.starts_with(' '))
        .collect();
    assert_eq!(headings, ["Modules (encapsulation, commits):"], "{text}");
    let mut rows: Vec<(String, u64)> = section_rows(&text, "Modules")
        .iter()
        .map(|row| {
            let mut parts = row.split_whitespace();
            let _encapsulation = parts.next().expect("encapsulation");
            let commits = parts.next().expect("commits").parse().expect("number");
            (parts.next().expect("path").to_string(), commits)
        })
        .collect();
    rows.sort();
    assert_eq!(
        rows,
        vec![
            ("crates/a".to_string(), 6),
            ("crates/b".to_string(), 6),
            ("packages/x".to_string(), 6),
            ("src".to_string(), 5),
        ]
    );
}

// ============================================================================
// JSON version 2: counts and caps
// ============================================================================

#[test]
fn test_heatmap_json_counts_give_total_and_shown_per_section() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_ranked_repo(dir.path());

    let json = heatmap_json(dir.path());
    // (counts key, array key, total, shown)
    let sections = [
        ("hotspots", "files", 40, 10),
        ("coupling", "coupling_graph", 36, 5),
        ("fix_risk", "fix_risk", 10, 5),
        ("modules", "modules", 1, 0),
        ("bus_factor", "bus_factor", 0, 0),
    ];
    for (count_key, array_key, total, shown) in sections {
        assert_eq!(
            json["counts"][count_key],
            serde_json::json!({"total": total, "shown": shown}),
            "{count_key}"
        );
        assert_eq!(
            json[array_key].as_array().expect(array_key).len(),
            shown,
            "{array_key} length equals counts.{count_key}.shown"
        );
    }
}

#[test]
fn test_heatmap_detail_coupling_text_rows_equal_the_json_total() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_ranked_repo(dir.path());

    let text = heatmap_stdout(dir.path(), &["--detail", "coupling"]);
    let json = heatmap_json_with(dir.path(), &["--detail", "coupling"]);
    let rows = section_rows(&text, "Coupling");
    assert_eq!(rows.len(), 36, "{text}");
    assert_eq!(json["counts"]["coupling"]["total"], 36);
    assert_eq!(json["counts"]["coupling"]["shown"], 36);
    assert_eq!(json["coupling_graph"].as_array().unwrap().len(), 36);
    assert!(
        !text.contains(" more ("),
        "no row is hidden in a --detail view without --top:\n{text}"
    );
}

// ============================================================================
// --top on every view, insights cap, bus factor, stable bytes
// ============================================================================

/// The finding lines of `--insights` text: every line after the header,
/// without the "more" line.
fn insight_rows(text: &str) -> Vec<&str> {
    text.lines()
        .skip(1)
        .filter(|l| !l.contains(" more ("))
        .collect()
}

#[test]
fn test_heatmap_top_gives_exact_rows_in_every_view() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_ranked_repo(dir.path());

    // Default text: 3 rows per section and the exact hidden count.
    let text = heatmap_stdout(dir.path(), &["--top", "3"]);
    for (heading, more) in [
        ("Hotspots", "  37 more (--detail hotspots)"),
        ("Coupling", "  33 more (--detail coupling)"),
        ("Fix risk", "  7 more (--detail fix-risk)"),
    ] {
        assert_eq!(section_rows(&text, heading).len(), 3, "{heading}:\n{text}");
        assert!(
            section_lines(&text, heading).contains(&more),
            "{heading}:\n{text}"
        );
    }

    // --json: 3 entries per array, and counts give total and shown.
    let json = heatmap_json_with(dir.path(), &["--top", "3"]);
    for (count_key, array_key, total) in [
        ("hotspots", "files", 40),
        ("coupling", "coupling_graph", 36),
        ("fix_risk", "fix_risk", 10),
    ] {
        assert_eq!(json[array_key].as_array().unwrap().len(), 3, "{array_key}");
        assert_eq!(
            json["counts"][count_key],
            serde_json::json!({"total": total, "shown": 3}),
            "{count_key}"
        );
    }

    // --insights --json and --insights text: 3 findings and the exact rest.
    let insights = heatmap_json_with(dir.path(), &["--insights", "--top", "3"]);
    let total = insights["counts"]["insights"]["total"].as_u64().unwrap();
    assert!(total > 3, "the fixture has more than 3 findings: {total}");
    assert_eq!(insights["insights"].as_array().unwrap().len(), 3);
    assert_eq!(insights["counts"]["insights"]["shown"], 3);
    let text = heatmap_stdout(dir.path(), &["--insights", "--top", "3"]);
    assert_eq!(insight_rows(&text).len(), 3, "{text}");
    assert_eq!(
        text.lines().last(),
        Some(format!("  {} more (raise --top)", total - 3).as_str())
    );
}

#[test]
fn test_heatmap_insights_show_twenty_findings_and_one_more_line() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_ranked_repo(dir.path());

    let json = heatmap_json_with(dir.path(), &["--insights"]);
    let total = json["counts"]["insights"]["total"].as_u64().unwrap();
    assert!(total > 20, "the fixture has more than 20 findings: {total}");
    let shown = json["insights"].as_array().unwrap();
    assert_eq!(shown.len(), 20);

    let text = heatmap_stdout(dir.path(), &["--insights"]);
    assert_eq!(insight_rows(&text).len(), 20, "{text}");
    let more: Vec<&str> = text.lines().filter(|l| l.contains(" more (")).collect();
    assert_eq!(more, [format!("  {} more (raise --top)", total - 20)]);

    // top_files holds the files of the shown findings only, once each, in
    // finding order (module findings name no file).
    let mut expected: Vec<&str> = Vec::new();
    for insight in shown {
        let file = insight["file"].as_str().unwrap();
        if insight["category"] != "encapsulation" && !expected.contains(&file) {
            expected.push(file);
        }
    }
    let top_files: Vec<&str> = json["top_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(top_files, expected);
}

/// Commit `filename` as author `name` at `timestamp`.
fn git_commit_as(dir: &Path, filename: &str, message: &str, timestamp: u64, name: &str) {
    std::fs::write(dir.join(filename), format!("// {message}\n")).expect("write file");
    let add_out = StdCommand::new("git")
        .args(["add", "--", filename])
        .current_dir(dir)
        .output()
        .expect("git add");
    assert!(add_out.status.success(), "git add failed for {filename}");
    let ts = format!("{timestamp}");
    let commit_out = StdCommand::new("git")
        .args(["commit", "-m", message])
        .env("GIT_AUTHOR_NAME", name)
        .env("GIT_AUTHOR_EMAIL", "other@example.com")
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

#[test]
fn test_heatmap_one_author_window_has_no_bus_factor_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    // One author; src/a.rs has 5 commits, all by that author.
    create_five_commit_repo(dir.path(), "feat");

    let text = heatmap_stdout(dir.path(), &["--detail", "bus-factor"]);
    assert_eq!(section_lines(&text, "Bus factor"), ["  (none)"], "{text}");
    let json = heatmap_json_with(dir.path(), &["--detail", "bus-factor"]);
    assert_eq!(json["bus_factor"], serde_json::json!([]));
    assert_eq!(json["counts"]["bus_factor"]["total"], 0);

    let text = heatmap_stdout(dir.path(), &["--insights"]);
    assert!(!text.contains("bus-factor"), "{text}");
    let insights = heatmap_json_with(dir.path(), &["--insights"]);
    let categories: Vec<&str> = insights["insights"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["category"].as_str().unwrap())
        .collect();
    assert!(!categories.contains(&"bus-factor"), "{categories:?}");
    let risky: Vec<&serde_json::Value> = insights["top_files"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["bus_factor_risk"] == true)
        .collect();
    assert!(risky.is_empty(), "{risky:?}");
}

#[test]
fn test_heatmap_two_author_window_shows_a_bus_factor_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    // src/a.rs: 5 commits, all by Test User; src/b.rs: 1 commit by Other Dev.
    create_five_commit_repo(dir.path(), "feat");
    git_commit_as(
        dir.path(),
        "src/b.rs",
        "feat: add b",
        recent_ts() + 600,
        "Other Dev",
    );

    let text = heatmap_stdout(dir.path(), &["--detail", "bus-factor"]);
    assert_eq!(
        section_lines(&text, "Bus factor"),
        ["  100%   5  src/a.rs"],
        "{text}"
    );
    let json = heatmap_json_with(dir.path(), &["--detail", "bus-factor"]);
    assert_eq!(
        json["bus_factor"],
        serde_json::json!([
            {"path": "src/a.rs", "top_author_pct": 100.0, "support": 5, "authors": 1}
        ])
    );

    let insights = heatmap_json_with(dir.path(), &["--insights"]);
    let bus: Vec<&str> = insights["insights"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["category"] == "bus-factor")
        .map(|i| i["file"].as_str().unwrap())
        .collect();
    assert_eq!(bus, ["src/a.rs"]);
}

#[test]
fn test_heatmap_two_runs_give_identical_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Many ties on commits, confidence, support and fix rate.
    create_ranked_repo(dir.path());

    let views: [&[&str]; 6] = [
        &[],
        &["--json"],
        &["--insights"],
        &["--insights", "--json"],
        &["src/hub/h0.rs"],
        &["--json", "src/hub/h0.rs"],
    ];
    for args in views {
        let first = heatmap_stdout(dir.path(), args);
        let second = heatmap_stdout(dir.path(), args);
        assert!(!first.is_empty(), "{args:?}");
        assert_eq!(first, second, "two runs of {args:?} differ");
    }
}

#[test]
fn test_heatmap_both_json_outputs_are_version_two_compact_and_capped() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_ranked_repo(dir.path());

    for (args, caps) in [
        (
            &["--json"][..],
            &[
                ("files", 10),
                ("coupling_graph", 5),
                ("fix_risk", 5),
                ("modules", 0),
                ("bus_factor", 0),
            ][..],
        ),
        (
            &["--insights", "--json"][..],
            &[("insights", 20), ("top_files", 20), ("flagged_modules", 20)][..],
        ),
    ] {
        let stdout = heatmap_stdout(dir.path(), args);
        assert_eq!(stdout.lines().count(), 1, "{args:?} is one line");
        let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
        assert_eq!(json["version"], 2, "{args:?}");
        for (key, cap) in caps {
            let len = json[*key].as_array().expect(key).len();
            assert!(len <= *cap, "{args:?} {key}: {len} > {cap}");
        }
    }

    // Every hub file has 7 partners; files[] keeps the 5 strongest.
    let json = heatmap_json(dir.path());
    for file in json["files"].as_array().unwrap() {
        let partners = file["blast_radius"].as_array().unwrap().len();
        let path = file["path"].as_str().unwrap();
        let expected = if path.starts_with("src/hub/") { 5 } else { 1 };
        assert_eq!(partners, expected, "{path}");
    }
}

#[test]
fn test_heatmap_large_diff_caps_file_targets_and_gives_one_missing_warning() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_ranked_repo(dir.path());
    StdCommand::new("git")
        .args(["checkout", "-b", "feature-large"])
        .current_dir(dir.path())
        .output()
        .expect("git checkout");
    // 31 changed files: the 24 hub and pair files of the window, and 7
    // files that the default excludes keep out of the history.
    let mut changed: Vec<String> = (0..8).map(|i| format!("src/hub/h{i}.rs")).collect();
    for p in 0..8 {
        changed.push(format!("src/pair/p{p}a.rs"));
        changed.push(format!("src/pair/p{p}b.rs"));
    }
    changed.extend((0..6).map(|v| format!("vendor/v{v:02}.rs")));
    changed.push("Cargo.lock".to_string());
    git_commit_paths(
        dir.path(),
        &changed,
        "feat: large change",
        recent_ts() + 7200,
    );
    // git lists the changed files in path order.
    changed.sort();

    let json = heatmap_json_with(dir.path(), &["--diff", "main"]);
    assert_eq!(json["file_targets"], serde_json::json!(changed[..20]));
    assert_eq!(
        json["counts"]["file_targets"],
        serde_json::json!({"total": 31, "shown": 20})
    );
    let warning = "7 targeted files not found in git history: \
                   'Cargo.lock', 'vendor/v00.rs', 'vendor/v01.rs' and 4 more";
    assert_eq!(json["warnings"], serde_json::json!([warning]));

    let text = heatmap_stdout(dir.path(), &["--diff", "main"]);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines.len() <= 30, "{} lines:\n{text}", lines.len());
    assert!(lines[0].ends_with(", scoped to 31 files"), "{text}");
    assert_eq!(lines[1], format!("warning: {warning}"), "{text}");
}

// ============================================================================
// Per-file card
// ============================================================================

/// The file of the card fixture with partners, fix commits and two owners.
const CARD_FILE: &str = "src/core.rs";

/// Commit new content for every path of `paths` in one commit, as author
/// `name`.
fn git_commit_paths_as(dir: &Path, paths: &[&str], message: &str, timestamp: u64, name: &str) {
    for path in paths {
        let file_path = dir.join(path);
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).expect("create dirs");
        }
        std::fs::write(&file_path, format!("// {message}\n")).expect("write file");
    }
    let add_out = StdCommand::new("git")
        .arg("add")
        .arg("--")
        .args(paths)
        .current_dir(dir)
        .output()
        .expect("git add");
    assert!(
        add_out.status.success(),
        "git add failed: {}",
        String::from_utf8_lossy(&add_out.stderr)
    );
    let ts = format!("{timestamp}");
    let commit_out = StdCommand::new("git")
        .args(["commit", "-m", message])
        .env("GIT_AUTHOR_NAME", name)
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
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

/// 9 commits that all touch `src/core.rs`, newest last:
/// - 1: core and `src/util.rs`, feat;
/// - 2-3: core, util, `tests/core_test.rs` and `src/rare.rs`, fix;
/// - 4-7: core, util and the test, fix;
/// - 8: core and util, feat;
/// - 9: core alone, fix, by the author `Ada|Lovelace`.
///
/// Core has two partners: util (8 shared commits, confidence 83%) and the
/// test (6 shared, 58%). `src/rare.rs` shares only 2 commits. Core has 7 fix
/// commits and two authors; the test file has one author.
fn create_card_repo(dir: &Path) {
    git_init(dir);
    let mut ts = recent_ts();
    let mut next_ts = move || {
        ts += 60;
        ts
    };
    let me = "Test User";
    let pair = ["src/core.rs", "src/util.rs"];
    let trio = ["src/core.rs", "src/util.rs", "tests/core_test.rs"];
    let quad = [
        "src/core.rs",
        "src/util.rs",
        "tests/core_test.rs",
        "src/rare.rs",
    ];
    git_commit_paths_as(dir, &pair, "feat: core and util", next_ts(), me);
    for n in 1..=2 {
        git_commit_paths_as(dir, &quad, &format!("fix: core bug {n}"), next_ts(), me);
    }
    for n in 3..=6 {
        git_commit_paths_as(dir, &trio, &format!("fix: core bug {n}"), next_ts(), me);
    }
    git_commit_paths_as(dir, &pair, "feat: more core", next_ts(), me);
    git_commit_paths_as(
        dir,
        &["src/core.rs"],
        "fix: core edge case",
        next_ts(),
        "Ada|Lovelace",
    );
}

/// The card line of each fix commit of `path`, newest first, from git
/// itself: the 7-char SHA, the UTC author date and the quoted subject.
fn expected_fix_lines(dir: &Path, path: &str) -> Vec<String> {
    let out = StdCommand::new("git")
        .args([
            "log",
            "--format=%H%x00%ad%x00%s",
            "--date=format-local:%Y-%m-%d",
            "--",
            path,
        ])
        .env("TZ", "UTC")
        .current_dir(dir)
        .output()
        .expect("git log");
    assert!(out.status.success(), "git log failed");
    String::from_utf8(out.stdout)
        .expect("utf-8 git log")
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\0');
            let (hash, date, subject) = (fields.next()?, fields.next()?, fields.next()?);
            subject
                .starts_with("fix")
                .then(|| format!("    {} {date} \"{subject}\"", &hash[..7]))
        })
        .collect()
}

/// The lines of the card of `path` in `text`: its title line and the
/// indented lines below it. Empty when there is no such card.
fn card_lines<'a>(text: &'a str, path: &str) -> Vec<&'a str> {
    let title = format!("{path}: ");
    let mut lines = text.lines().skip_while(|l| !l.starts_with(&title));
    let Some(first) = lines.next() else {
        return Vec::new();
    };
    std::iter::once(first)
        .chain(lines.take_while(|l| l.starts_with("  ")))
        .collect()
}

/// The lines of `text` that are not indented, after the header line: card
/// titles and the "more" line of the cards.
fn top_level_lines(text: &str) -> Vec<&str> {
    text.lines()
        .skip(1)
        .filter(|l| !l.starts_with("  "))
        .collect()
}

fn owned_strings(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|l| (*l).to_string()).collect()
}

#[test]
fn test_heatmap_card_lists_a_test_partner_first_and_three_fix_commits_from_history() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_card_repo(dir.path());
    let fix = expected_fix_lines(dir.path(), CARD_FILE);
    assert_eq!(fix.len(), 7);

    let text = heatmap_stdout(dir.path(), &[CARD_FILE]);

    // The author name with `|` keeps its date: the newest fix line is the
    // one of git itself.
    let mut expected = owned_strings(&[
        "src/core.rs: 9 commits",
        "  owners: Test User 89%, Ada|Lovelace 11%",
        "  partners (confidence, shared commits):",
        "     58%   6  tests/core_test.rs",
        "     83%   8  src/util.rs",
        "  fix commits from history (newest first):",
    ]);
    expected.extend(fix[..3].iter().cloned());
    expected.push("  4 more fix commits (raise --top)".to_string());
    assert_eq!(card_lines(&text, CARD_FILE), expected, "{text}");
    assert!(expected.len() <= 12);
    // The header line and the card, nothing else.
    assert!(text.starts_with("Heatmap: "), "{text}");
    assert_eq!(text.lines().count(), 1 + expected.len(), "{text}");
}

#[test]
fn test_heatmap_card_owners_line_needs_two_authors() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_card_repo(dir.path());
    let fix = expected_fix_lines(dir.path(), "tests/core_test.rs");

    let text = heatmap_stdout(dir.path(), &["tests/core_test.rs", CARD_FILE]);

    // One author: no owners line. The partners tie on confidence and
    // support, so the path decides.
    let mut expected = owned_strings(&[
        "tests/core_test.rs: 6 commits",
        "  partners (confidence, shared commits):",
        "    100%   6  src/core.rs",
        "    100%   6  src/util.rs",
        "  fix commits from history (newest first):",
    ]);
    expected.extend(fix[..3].iter().cloned());
    expected.push("  3 more fix commits (raise --top)".to_string());
    assert_eq!(card_lines(&text, "tests/core_test.rs"), expected, "{text}");

    let owners: Vec<&str> = card_lines(&text, CARD_FILE)
        .into_iter()
        .filter(|l| l.starts_with("  owners: "))
        .collect();
    assert_eq!(owners, ["  owners: Test User 89%, Ada|Lovelace 11%"]);
}

#[test]
fn test_heatmap_card_partners_need_three_shared_commits() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_card_repo(dir.path());
    let fix = expected_fix_lines(dir.path(), "src/rare.rs");

    // src/rare.rs shares 2 commits with each of its 3 co-changed files.
    let text = heatmap_stdout(dir.path(), &["src/rare.rs"]);
    let mut expected = owned_strings(&[
        "src/rare.rs: 2 commits",
        "  partners (confidence, shared commits):",
        "    (none)",
        "  fix commits from history (newest first):",
    ]);
    expected.extend(fix.iter().cloned());
    assert_eq!(card_lines(&text, "src/rare.rs"), expected, "{text}");
}

#[test]
fn test_heatmap_six_file_arguments_give_five_cards_and_one_more_line() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_card_repo(dir.path());
    let args = [
        CARD_FILE,
        "src/util.rs",
        "tests/core_test.rs",
        "src/rare.rs",
        "src/none1.rs",
        "src/none2.rs",
    ];
    let since = heatmap_json_with(dir.path(), &args)["window"]["since"]
        .as_str()
        .unwrap()
        .to_string();

    let text = heatmap_stdout(dir.path(), &args);

    assert_eq!(
        top_level_lines(&text),
        [
            "src/core.rs: 9 commits".to_string(),
            "src/util.rs: 8 commits".to_string(),
            "tests/core_test.rs: 6 commits".to_string(),
            "src/rare.rs: 2 commits".to_string(),
            format!("src/none1.rs: not in history window (9 commits since {since})"),
            "1 more file (raise --top)".to_string(),
        ],
        "{text}"
    );
}

#[test]
fn test_heatmap_diff_gives_the_default_view_not_cards() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_card_repo(dir.path());
    StdCommand::new("git")
        .args(["checkout", "-b", "feature-card"])
        .current_dir(dir.path())
        .output()
        .expect("git checkout");
    git_commit_paths_as(
        dir.path(),
        &[CARD_FILE],
        "feat: branch change",
        recent_ts() + 3600,
        "Test User",
    );

    let text = heatmap_stdout(dir.path(), &["--diff", "main"]);

    assert!(text.lines().any(|l| l == "Hotspots (commits):"), "{text}");
    assert!(card_lines(&text, CARD_FILE).is_empty(), "{text}");
}

#[test]
fn test_heatmap_card_of_a_file_outside_the_window_names_the_window() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    let now = recent_ts();
    git_commit(
        dir.path(),
        "src/old.rs",
        "// old",
        "feat: old",
        now - 30 * 86400,
    );
    git_commit(dir.path(), "src/new.rs", "// new 1", "feat: new 1", now);
    git_commit(
        dir.path(),
        "src/new.rs",
        "// new 2",
        "feat: new 2",
        now + 60,
    );
    let since_epoch = (now - 86400).to_string();
    let args = ["--since", since_epoch.as_str(), "src/old.rs"];
    let window = heatmap_json_with(dir.path(), &args)["window"].clone();
    assert_eq!(window["commits_analyzed"], 2);

    let text = heatmap_stdout(dir.path(), &args);

    assert_eq!(
        card_lines(&text, "src/old.rs"),
        [format!(
            "src/old.rs: not in history window (2 commits since {})",
            window["since"].as_str().unwrap()
        )],
        "{text}"
    );
}

#[test]
fn test_heatmap_card_is_the_same_with_insights_and_top_keeps_five_fix_commits() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_card_repo(dir.path());
    let fix = expected_fix_lines(dir.path(), CARD_FILE);

    let plain = heatmap_stdout(dir.path(), &[CARD_FILE]);
    assert_eq!(
        heatmap_stdout(dir.path(), &["--insights", CARD_FILE]),
        plain
    );

    // --top 9: every owner and partner, but at most 5 fix commits; --top
    // cannot show the other fix commits.
    let text = heatmap_stdout(dir.path(), &["--top", "9", CARD_FILE]);
    let card = card_lines(&text, CARD_FILE);
    let fix_rows: Vec<&str> = card
        .iter()
        .copied()
        .filter(|l| l.starts_with("    ") && l.ends_with('"'))
        .collect();
    assert_eq!(fix_rows, fix[..5]);
    assert_eq!(card.last().copied(), Some("  2 more fix commits (git log)"));

    // --top 1: one row in each list, and one line with every hidden count.
    let text = heatmap_stdout(dir.path(), &["--top", "1", CARD_FILE]);
    let mut expected = owned_strings(&[
        "src/core.rs: 9 commits",
        "  owners: Test User 89%",
        "  partners (confidence, shared commits):",
        "     58%   6  tests/core_test.rs",
        "  fix commits from history (newest first):",
    ]);
    expected.push(fix[0].clone());
    expected.push("  1 more owner, 1 more partner, 6 more fix commits (raise --top)".to_string());
    assert_eq!(card_lines(&text, CARD_FILE), expected, "{text}");
}

#[test]
fn test_heatmap_four_path_forms_give_byte_identical_cards() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_card_repo(dir.path());
    let toplevel = StdCommand::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir.path())
        .output()
        .expect("git rev-parse");
    let toplevel = String::from_utf8(toplevel.stdout).unwrap();
    let absolute = format!("{}/src/core.rs", toplevel.trim_end());

    let plain = heatmap_stdout(dir.path(), &[CARD_FILE]);
    assert_eq!(
        card_lines(&plain, CARD_FILE).first().copied(),
        Some("src/core.rs: 9 commits")
    );
    assert_eq!(heatmap_stdout(dir.path(), &["./src/core.rs"]), plain);
    assert_eq!(heatmap_stdout(dir.path(), &[absolute.as_str()]), plain);
    assert_eq!(heatmap_stdout(&dir.path().join("src"), &["core.rs"]), plain);
    assert_eq!(
        heatmap_stdout(&dir.path().join("tests"), &["../src/./core.rs"]),
        plain
    );
}

#[test]
fn test_heatmap_card_path_outside_the_root_exits_1_with_no_card() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_card_repo(dir.path());

    for arg in ["../outside.rs", "src/../../outside.rs", "/etc/hosts"] {
        let output = common::skim()
            .args(["heatmap", arg])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{arg}");
        assert!(output.stdout.is_empty(), "{arg}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!("'{arg}' is outside the repository")),
            "{arg}: {stderr}"
        );
    }
}

#[test]
fn test_heatmap_card_of_a_fifo_path_returns_without_blocking() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_card_repo(dir.path());
    let made = StdCommand::new("mkfifo")
        .arg(dir.path().join("pipe"))
        .status()
        .expect("mkfifo");
    assert!(made.success());

    let mut child = StdCommand::new(env!("CARGO_BIN_EXE_skim"))
        .args(["heatmap", "pipe"])
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .current_dir(dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn skim");
    // At most 300 polls of 100 ms: a run that opens the FIFO would block.
    let mut status = None;
    for _ in 0..300 {
        status = child.try_wait().expect("try_wait");
        if status.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let Some(status) = status else {
        child.kill().ok();
        panic!("skim heatmap blocked on a FIFO path argument");
    };
    assert!(status.success());
    let mut stdout = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut stdout).unwrap();
    let card = card_lines(&stdout, "pipe");
    assert_eq!(card.len(), 1, "{stdout}");
    assert!(
        card[0].starts_with("pipe: not in history window ("),
        "{stdout}"
    );
}

#[test]
fn test_heatmap_card_ties_rank_by_path_and_two_runs_agree() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Each hub file has 7 partners with the same confidence and support.
    create_ranked_repo(dir.path());

    let text = heatmap_stdout(dir.path(), &["src/hub/h0.rs"]);

    assert_eq!(
        card_lines(&text, "src/hub/h0.rs"),
        [
            "src/hub/h0.rs: 6 commits",
            "  partners (confidence, shared commits):",
            "    100%   6  src/hub/h1.rs",
            "    100%   6  src/hub/h2.rs",
            "    100%   6  src/hub/h3.rs",
            "    100%   6  src/hub/h4.rs",
            "  fix commits from history (newest first):",
            "    (none)",
            "  3 more partners (raise --top)",
        ],
        "{text}"
    );
    assert_eq!(heatmap_stdout(dir.path(), &["src/hub/h0.rs"]), text);
}

#[test]
fn test_heatmap_card_json_is_version_two_compact_and_capped() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_card_repo(dir.path());
    let args = [
        "--json",
        CARD_FILE,
        "src/util.rs",
        "tests/core_test.rs",
        "src/rare.rs",
        "src/none1.rs",
        "src/none2.rs",
    ];

    let stdout = heatmap_stdout(dir.path(), &args);

    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(json["version"], 2);
    assert_eq!(
        json["counts"]["cards"],
        serde_json::json!({"total": 6, "shown": 5})
    );
    // No malformed git log record: the author name with `|` parses.
    assert_eq!(json["warnings"], serde_json::json!([]));
    let cards = json["cards"].as_array().unwrap();
    assert_eq!(cards.len(), 5);
    for card in cards {
        for (key, cap) in [("owners", 3), ("partners", 4), ("fix_commits", 3)] {
            let len = card[key].as_array().unwrap().len();
            assert!(len <= cap, "{key}: {card}");
            assert_eq!(card["counts"][key]["shown"], len, "{key}: {card}");
        }
    }

    let core = &cards[0];
    assert_eq!(core["path"], CARD_FILE);
    assert_eq!(core["in_window"], true);
    assert_eq!(core["commits"], 9);
    let owners: Vec<(&str, u64)> = core["owners"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| (o["name"].as_str().unwrap(), o["commits"].as_u64().unwrap()))
        .collect();
    assert_eq!(owners, [("Test User", 8), ("Ada|Lovelace", 1)]);
    let partners: Vec<(&str, u64, bool)> = core["partners"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["path"].as_str().unwrap(),
                p["support"].as_u64().unwrap(),
                p["test"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        partners,
        [("tests/core_test.rs", 6, true), ("src/util.rs", 8, false)]
    );
    let subjects: Vec<&str> = core["fix_commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["subject"].as_str().unwrap())
        .collect();
    assert_eq!(
        subjects,
        ["fix: core edge case", "fix: core bug 6", "fix: core bug 5"]
    );
    assert_eq!(
        core["counts"]["fix_commits"],
        serde_json::json!({"total": 7, "shown": 3})
    );
    let fix = expected_fix_lines(dir.path(), CARD_FILE);
    let first = &core["fix_commits"][0];
    assert_eq!(
        format!(
            "    {} {} \"{}\"",
            first["sha"].as_str().unwrap(),
            first["date"].as_str().unwrap(),
            first["subject"].as_str().unwrap()
        ),
        fix[0]
    );
}

#[test]
fn test_heatmap_card_replaces_control_and_bidi_characters() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    // Git path quoting off: the bidi mark of the partner file name reaches
    // the numstat output as it is.
    StdCommand::new("git")
        .args(["config", "core.quotePath", "false"])
        .current_dir(dir.path())
        .output()
        .expect("git config");
    let partner = "src/b\u{202E}c.rs";
    let subject = "fix: \u{1b}]0;title \u{202E}gnp.exe\rmore \u{2066}end";
    let ts = recent_ts();
    let files = ["src/a.rs", partner];
    git_commit_paths_as(dir.path(), &files, "feat: one", ts, "Test User");
    git_commit_paths_as(dir.path(), &files, "feat: two", ts + 60, "Test User");
    git_commit_paths_as(dir.path(), &files, subject, ts + 120, "Eve\u{202E}x");
    let fix = expected_fix_lines(dir.path(), "src/a.rs");
    let sha = &fix[0][4..11];

    let text = heatmap_stdout(dir.path(), &["src/a.rs"]);

    let safe_subject = "fix: \u{FFFD}]0;title \u{FFFD}gnp.exe\u{FFFD}more \u{FFFD}end";
    let card = card_lines(&text, "src/a.rs");
    assert_eq!(card.len(), 6, "{text}");
    assert_eq!(
        card[1..],
        [
            "  owners: Test User 67%, Eve\u{FFFD}x 33%".to_string(),
            "  partners (confidence, shared commits):".to_string(),
            "    100%   3  src/b\u{FFFD}c.rs".to_string(),
            "  fix commits from history (newest first):".to_string(),
            format!("    {sha} {} \"{safe_subject}\"", &fix[0][12..22]),
        ],
        "{text}"
    );
    for unsafe_char in ['\u{1b}', '\r', '\u{202E}', '\u{2066}'] {
        assert!(!text.contains(unsafe_char), "{text:?}");
    }

    let json = heatmap_json_with(dir.path(), &["src/a.rs"]);
    let card = &json["cards"][0];
    assert_eq!(card["fix_commits"][0]["subject"], safe_subject);
    assert_eq!(card["owners"][1]["name"], "Eve\u{FFFD}x");
    assert_eq!(card["partners"][0]["path"], "src/b\u{FFFD}c.rs");
}

#[test]
fn test_heatmap_card_finds_a_non_ascii_file_name_in_history() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    let ts = recent_ts();
    git_commit(dir.path(), "src/café.rs", "// 1", "feat: one", ts);
    git_commit(dir.path(), "src/café.rs", "// 2", "feat: two", ts + 60);

    // Git quotes the name in its numstat output; the card target uses the
    // same form.
    let text = heatmap_stdout(dir.path(), &["src/café.rs"]);

    assert_eq!(
        top_level_lines(&text),
        ["\"src/caf\\303\\251.rs\": 2 commits"],
        "{text}"
    );
}
