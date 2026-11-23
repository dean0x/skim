//! Integration tests for `skim heatmap` subcommand.
//!
//! Tests end-to-end CLI behavior for git history risk/coupling analysis.
//! For tests that need a git repo, we create a TempDir and set up deterministic commits.

use predicates::prelude::*;
use std::ffi::OsStr;
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

fn git_config(dir: &Path, key: &str, value: impl AsRef<OsStr>) {
    let out = StdCommand::new("git")
        .args(["config", key])
        .arg(value)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
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

    git_commit_staged(dir, message, timestamp);
}

fn git_commit_staged(dir: &Path, message: &str, timestamp: u64) {
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

#[test]
fn test_heatmap_help_describes_utc_dates_and_window_presets() {
    common::skim()
        .args(["heatmap", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("YYYY-MM-DD (00:00 UTC)"))
        .stdout(predicate::str::contains(
            "sprint|month|quarter|half|year|all",
        ))
        .stdout(predicate::str::contains(
            "No date cutoff (same as --since 0)",
        ))
        .stdout(predicate::str::contains(
            "An unknown --window preset exits 1 and lists the valid names.",
        ));
}

#[test]
fn test_heatmap_help_describes_hotspot_rank_and_fallback() {
    common::skim()
        .args(["heatmap", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "The score is commits in the window x text lines at HEAD.",
        ))
        .stdout(predicate::str::contains(
            "If HEAD data is not available, skim warns and ranks by commits.",
        ));
}

#[test]
fn test_heatmap_help_describes_non_code_filter_for_fix_risk() {
    common::skim()
        .args(["heatmap", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "NON-CODE PATHS (--include-docs keeps them)",
        ))
        .stdout(predicate::str::contains(
            "out of the hotspots, fix risk and insights",
        ));
}

#[test]
fn test_heatmap_help_names_new_json_keys() {
    common::skim()
        .args(["heatmap", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("files[].lines"))
        .stdout(predicate::str::contains("files[].score"))
        .stdout(predicate::str::contains(
            "left_out.{hotspots,fix_risk,modules}",
        ))
        .stdout(predicate::str::contains("left_out.not_at_head"));
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

    let file = &parsed["files"][0];
    let mut keys: Vec<&str> = file
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "authors",
            "blast_radius",
            "churn",
            "fix_risk",
            "lines",
            "path",
            "score",
            "stability_score"
        ]
    );
    assert_eq!(file["path"], "src/main.rs");
    assert_eq!(file["churn"]["commits"], 4, "nested churn.commits");
    assert_eq!(file["lines"], 1);
    assert_eq!(file["score"], 4);
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
        .filter(|l| !l.starts_with(' ') && !l.starts_with("Before you edit"))
        .collect();
    assert_eq!(
        headings,
        [
            "Hotspots (commits x lines):",
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

#[test]
fn test_heatmap_since_date_starts_at_utc_midnight() {
    const MIDNIGHT: u64 = 1_793_491_200; // 2026-11-01 00:00:00 UTC
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    git_commit(
        dir.path(),
        "before.rs",
        "before",
        "feat: before",
        MIDNIGHT - 1,
    );
    git_commit(dir.path(), "at.rs", "at", "feat: at", MIDNIGHT);

    common::skim()
        .args(["heatmap", "--since", "2026-11-01"])
        .env("TZ", "UTC+8")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(" 2026-11-01.."))
        .stdout(predicate::str::contains(", 1 commit, time window"))
        .stdout(predicate::str::contains("at.rs"))
        .stdout(predicate::str::contains("before.rs").not());
}

#[test]
fn test_heatmap_since_date_errors_name_all_forms() {
    let dir = tempfile::tempdir().expect("tempdir");
    for date in [
        "2026-02-30",
        "2026-13-01",
        "2025-02-29",
        "1969-12-31",
        "10000-01-01",
    ] {
        common::skim()
            .args(["heatmap", "--since", date])
            .current_dir(dir.path())
            .assert()
            .code(1)
            .stdout(predicate::str::is_empty())
            .stderr(predicate::str::contains("--since"))
            .stderr(predicate::str::contains("epoch seconds"))
            .stderr(predicate::str::contains("YYYY-MM-DD"))
            .stderr(predicate::str::contains("duration"));
    }
}

#[test]
fn test_heatmap_since_duration_out_of_range_is_a_usage_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    common::skim()
        .args(["heatmap", "--since", "200000000000000d"])
        .current_dir(dir.path())
        .assert()
        .code(1)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(
            "--since expects epoch seconds, a UTC date (YYYY-MM-DD), or a duration (Nd, Nh, Nw)",
        ))
        .stderr(predicate::str::contains("panicked").not());
}

#[test]
fn test_heatmap_since_far_future_date_excludes_history() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    git_commit(dir.path(), "a.rs", "one\n", "feat: add file", 2_114_380_800);

    common::skim()
        .args(["heatmap", "--since", "9999-12-31"])
        .current_dir(dir.path())
        .assert()
        .code(1)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("No commits found in repository"));
}

// ============================================================================
// --window preset
// ============================================================================

#[test]
fn test_heatmap_window_all_and_since_zero_include_early_commits() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    git_commit(
        dir.path(),
        "old.rs",
        "old\n",
        "feat: old file",
        1_000_000_000,
    );
    git_commit(
        dir.path(),
        "new.rs",
        "new\n",
        "feat: new file",
        1_100_000_000,
    );
    git_commit(
        dir.path(),
        "Cargo.lock",
        "lock\n",
        "chore: lock",
        1_200_000_000,
    );
    let output = StdCommand::new("git")
        .args(["rev-list", "--count", "--no-merges", "HEAD"])
        .current_dir(dir.path())
        .output()
        .expect("git rev-list");
    assert!(output.status.success());
    let total: u64 = std::str::from_utf8(&output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(total, 3);

    for args in [
        ["--window", "all"],
        ["--since", "0"],
        ["--since", "1970-01-01"],
    ] {
        let json = heatmap_json_with(dir.path(), &args);
        assert_eq!(json["window"]["commits_analyzed"], total - 1);
        assert_eq!(json["window"]["since"], "1970-01-01");
    }
}

#[test]
fn test_heatmap_window_unknown_preset_names_valid_presets() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    git_commit(dir.path(), "a.rs", "one\n", "feat: add file", 1_700_000_000);

    for args in [
        vec!["--window=biweekly"],
        vec!["--since=0", "--window=biweekly"],
    ] {
        common::skim()
            .arg("heatmap")
            .args(args)
            .current_dir(dir.path())
            .assert()
            .code(1)
            .stdout(predicate::str::is_empty())
            .stderr(predicate::str::contains(
                "--window must be one of sprint, month, quarter, half, year, all; got: biweekly",
            ));
    }
}

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

#[test]
fn test_heatmap_counts_signed_commits_with_signature_display_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git_init(root);
    let key = root.join(".git").join("fixture-signing-key");
    let keygen = StdCommand::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .output();
    let keygen = match keygen {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skip signed heatmap test: ssh-keygen is not available");
            return;
        }
        result => result.expect("spawn ssh-keygen"),
    };
    assert!(keygen.status.success(), "ssh-keygen failed: {keygen:?}");
    let signers = root.join(".git").join("fixture-allowed-signers");
    let public_key = std::fs::read_to_string(key.with_extension("pub")).unwrap();
    std::fs::write(&signers, format!("test@example.com {public_key}")).unwrap();
    git_config(root, "gpg.format", "ssh");
    git_config(root, "gpg.ssh.allowedSignersFile", &signers);
    git_config(root, "user.signingkey", &key);
    git_config(root, "commit.gpgsign", "true");
    git_config(root, "log.showSignature", "true");
    let ts = 1_700_000_000;
    for (i, text) in ["one", "two", "three"].into_iter().enumerate() {
        git_commit(root, "a.rs", text, text, ts + i as u64);
    }

    let all = heatmap_json_with(root, &["--window", "all"]);
    assert_eq!(all["window"]["commits_analyzed"], 3, "{all}");
    let last = heatmap_json_with(root, &["--last", "2"]);
    assert_eq!(last["window"]["commits_analyzed"], 2, "{last}");
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
        header.starts_with("Heatmap: ") && header.ends_with(", scoped to 1 file"),
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
    assert_diff_dash_value_refused(&["--diff", "-x"]);
}

#[test]
fn test_heatmap_diff_dash_value_equals_form_refused() {
    assert_diff_dash_value_refused(&["--diff=-x"]);
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
    // Named files keep every path: the non-code rule does not drop the
    // file under the top tests/ folder from files[].
    let files: Vec<&str> = json["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["path"].as_str())
        .collect();
    assert!(files.contains(&"tests/test_lib.rs"), "{json}");
    assert!(json.get("left_out").is_none(), "{json}");
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
        header.starts_with("Heatmap: ") && header.ends_with(", scoped to 1 file"),
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
    assert!(lines[0].contains(", 1 commit, "), "{stdout}");
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
fn test_heatmap_large_file_with_fewer_commits_ranks_first() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git_init(root);
    git_commit_files(
        root,
        &[
            ("large.rs", &"// large\n".repeat(20)),
            ("small.rs", "// one\n"),
        ],
        "feat: add files",
        1_700_000_000,
    );
    git_commit(root, "small.rs", "// two\n", "fix: small", 1_700_000_001);
    std::fs::write(root.join("large.rs"), "// uncommitted\n").unwrap();
    std::fs::write(root.join("HEAD"), "not the revision\n").unwrap();

    let json = heatmap_json_with(root, &["--window", "all"]);
    assert_eq!(json["files"][0]["path"], "large.rs");
    assert_eq!(json["files"][0]["churn"]["commits"], 1);
    assert_eq!(json["files"][0]["lines"], 20);
    assert_eq!(json["files"][0]["score"], 20);
    assert_eq!(json["files"][1]["score"], 2);
}

#[cfg(unix)]
fn head_rank_fixture(root: &Path) {
    git_init(root);
    git_commit_files(
        root,
        &[("a.rs", "one\ntwo\nthree\n"), ("z.rs", "one\n")],
        "feat: add files",
        1_700_000_000,
    );
    git_commit(root, "z.rs", "two\n", "feat: update", 1_700_000_001);
}

#[cfg(unix)]
fn heatmap_with_old_git(root: &Path) -> serde_json::Value {
    let real_git = StdCommand::new("/bin/sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    assert!(real_git.status.success(), "{real_git:?}");
    let real_git =
        std::fs::canonicalize(String::from_utf8(real_git.stdout).unwrap().trim()).unwrap();
    let tools = tempfile::tempdir().unwrap();
    common::write_stub_script(
        tools.path(),
        "git",
        r#"#!/bin/sh
for arg do
    if [ "$arg" = "--no-lazy-fetch" ]; then
        printf '%s\n' 'unknown option: --no-lazy-fetch' >&2
        exit 129
    fi
done
exec "$HEATMAP_TEST_REAL_GIT" "$@"
"#,
    );
    let output = common::skim()
        .env("PATH", common::stub_path(tools.path()))
        .env("HEATMAP_TEST_REAL_GIT", real_git)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(["heatmap", "--window", "all", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[cfg(unix)]
#[test]
fn test_heatmap_old_git_keeps_line_rank_for_a_full_clone() {
    let repo = tempfile::tempdir().unwrap();
    head_rank_fixture(repo.path());

    let json = heatmap_with_old_git(repo.path());
    assert_eq!(json["files"][0]["path"], "a.rs", "{json}");
    assert_eq!(json["files"][0]["lines"], 3);
    assert_eq!(json["files"][0]["score"], 3);
    assert_eq!(json["files"][1]["lines"], 1);
    assert_eq!(json["files"][1]["score"], 2);
    assert_eq!(json["warnings"], serde_json::json!([]));
}

#[cfg(unix)]
#[test]
fn test_heatmap_old_git_warns_and_ranks_a_partial_clone_by_commits() {
    let repo = tempfile::tempdir().unwrap();
    let source = repo.path().join("source");
    std::fs::create_dir(&source).unwrap();
    head_rank_fixture(&source);
    git_config(&source, "uploadpack.allowFilter", "true");

    // All fixture blobs fit the filter. Only the rejected option causes fallback.
    let clone = repo.path().join("clone");
    let output = StdCommand::new("git")
        .env("GIT_ALLOW_PROTOCOL", "file")
        .args([
            "clone",
            "--no-local",
            "--no-checkout",
            "--filter=blob:limit=1m",
        ])
        .arg(&source)
        .arg(&clone)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    let json = heatmap_with_old_git(&clone);
    assert_eq!(json["files"][0]["path"], "z.rs", "{json}");
    assert_eq!(json["files"][0]["churn"]["commits"], 2);
    assert_eq!(json["files"][0]["score"], 0);
    assert_eq!(
        json["warnings"],
        serde_json::json!(["HEAD data is not available. Hotspots rank by commits only."])
    );
}

#[cfg(unix)]
#[test]
fn test_heatmap_head_failure_keeps_history_in_commit_order() {
    let repo = tempfile::tempdir().unwrap();
    head_rank_fixture(repo.path());
    let tools = tempfile::tempdir().unwrap();
    common::write_stub_script(
        tools.path(),
        "git",
        r#"#!/bin/sh
for arg do
    if [ "$arg" = "$HEATMAP_TEST_FAIL" ]; then
        printf '%s\n' "$arg" >> "$HEATMAP_TEST_CALLS"
        exit 128
    fi
done
PATH=$HEATMAP_TEST_PATH exec git "$@"
"#,
    );
    let calls = tools.path().join("calls");
    for failing in ["ls-tree", "grep"] {
        std::fs::write(&calls, "").unwrap();
        let output = common::skim()
            .env("PATH", common::stub_path(tools.path()))
            .env("HEATMAP_TEST_PATH", std::env::var_os("PATH").unwrap())
            .env("HEATMAP_TEST_FAIL", failing)
            .env("HEATMAP_TEST_CALLS", &calls)
            .args(["heatmap", "--window", "all", "--detail", "hotspots"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{failing}: {output:?}");
        assert_eq!(
            std::fs::read_to_string(&calls).unwrap(),
            format!("{failing}\n")
        );
        let text = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            section_rows(&text, "Hotspots"),
            ["    2 x 0 = 0  z.rs", "    1 x 0 = 0  a.rs"],
            "{failing}: {text}"
        );
        let warnings: Vec<_> = text
            .lines()
            .filter(|line| line.starts_with("warning:"))
            .collect();
        assert_eq!(
            warnings,
            ["warning: HEAD data is not available. Hotspots rank by commits only."]
        );
    }
}

#[cfg(unix)]
#[test]
fn test_heatmap_head_reads_leave_partial_clone_objects_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    git_init(&source);
    git_config(&source, "uploadpack.allowFilter", "true");
    git_commit(&source, "old.rs", "old\n", "feat: add old", 1_700_000_000);
    git_commit(
        &source,
        "recent.rs",
        "new\n",
        "feat: add new",
        1_700_001_000,
    );

    let clone = dir.path().join("clone");
    let local_git = |cwd: &Path, args: &[&str]| {
        let output = StdCommand::new("git")
            .env("GIT_ALLOW_PROTOCOL", "file")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    local_git(
        dir.path(),
        &[
            "clone",
            "--filter=blob:none",
            "--no-checkout",
            &format!("file://{}", source.display()),
            "clone",
        ],
    );
    // Supply the blob that the history window needs. Leave the old blob absent.
    local_git(
        &clone,
        &[
            "hash-object",
            "-w",
            source.join("recent.rs").to_str().unwrap(),
        ],
    );
    let before = local_git(&clone, &["count-objects", "-v"]);

    let output = common::skim()
        .env("GIT_ALLOW_PROTOCOL", "file")
        .env_remove("GIT_NO_LAZY_FETCH")
        .args(["heatmap", "--since", "1700000500", "--json"])
        .current_dir(&clone)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        local_git(&clone, &["count-objects", "-v"]),
        before,
        "object store grew"
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(sorted_paths(&json, "files"), ["recent.rs"]);
    assert_eq!(json["files"][0]["churn"]["commits"], 1);
    assert_eq!(json["files"][0]["score"], 0);
    assert_eq!(
        json["warnings"],
        serde_json::json!(["HEAD data is not available. Hotspots rank by commits only."])
    );
}

#[test]
fn test_heatmap_counts_quoted_head_paths_from_a_subfolder() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git_init(root);
    git_config(root, "core.quotePath", "true");
    git_commit_files(
        root,
        &[("top.rs", "a\n"), ("src/deep/café.rs", "a\nb\n")],
        "feat: add files",
        1_700_000_000,
    );

    let cwd = root.join("src");
    let quoted = r#""src/deep/caf\303\251.rs""#;
    let all = heatmap_json_with(&cwd, &["--window", "all"]);
    assert_eq!(file_entry(&all, quoted)["lines"], 2, "{all}");
    assert_eq!(file_entry(&all, "top.rs")["lines"], 1, "{all}");

    let scoped = heatmap_json_with(&cwd, &["--window", "all", "--path", "deep"]);
    assert_eq!(sorted_paths(&scoped, "files"), [quoted]);
    assert_eq!(file_entry(&scoped, quoted)["lines"], 2, "{scoped}");
}

#[test]
fn test_heatmap_uses_root_relative_git_paths_with_diff_relative_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let ts = 1_700_000_000;
    git_init(root);
    git_config(root, "diff.relative", "true");
    git_commit(root, "base.rs", "base\n", "feat: add base", ts);
    git_commit(root, "top.rs", "top\n", "feat: add top", ts + 1);
    git_commit(root, "src/a.rs", "one\ntwo\n", "feat: add source", ts + 2);

    let cwd = root.join("src");
    let all = heatmap_json_with(&cwd, &["--window", "all"]);
    assert_eq!(
        sorted_paths(&all, "files"),
        ["base.rs", "src/a.rs", "top.rs"],
        "{all}"
    );
    assert_eq!(all["files"][0]["path"], "src/a.rs", "{all}");
    assert_eq!(all["files"][0]["score"], 2, "{all}");
    assert_eq!(all["left_out"]["not_at_head"].as_u64().unwrap_or(0), 0);

    let scoped = heatmap_json_with(&cwd, &["--window", "all", "--path", "a.rs"]);
    assert_eq!(sorted_paths(&scoped, "files"), ["src/a.rs"], "{scoped}");
    let diff = heatmap_json_with(&cwd, &["--window", "all", "--diff", "HEAD~2"]);
    assert_eq!(
        sorted_paths(&diff, "files"),
        ["src/a.rs", "top.rs"],
        "{diff}"
    );
}

#[test]
fn test_heatmap_binary_at_head_is_not_a_hotspot_but_an_empty_file_is() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git_init(root);
    git_commit_files(
        root,
        &[("changed.dat", "text\n"), ("empty.rs", "")],
        "feat: add text",
        1_700_000_000,
    );
    git_commit(
        root,
        "changed.dat",
        "a\0b",
        "feat: change data",
        1_700_000_001,
    );

    let json = heatmap_json_with(root, &["--window", "all"]);
    assert_eq!(sorted_paths(&json, "files"), ["empty.rs"]);
    assert_eq!(file_entry(&json, "empty.rs")["lines"], 0);
    assert!(json.get("left_out").is_none());
}

#[test]
fn test_heatmap_excludes_nonregular_head_paths_but_keeps_deleted_card_history() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git_init(root);
    let base = 1_700_000_000;
    for i in 0..5 {
        let content = format!("// live {i}\n");
        git_commit_files(
            root,
            &[
                ("live.rs", &content),
                ("gone.rs", &content),
                ("link.rs", &content),
                ("module.rs", &content),
            ],
            "fix: change files",
            base + i,
        );
    }
    let git = |args: &[&str]| {
        let out = StdCommand::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    git(&["config", "user.name", "Other Author"]);
    for i in 5..11 {
        git_commit(
            root,
            "side.rs",
            &format!("// side {i}\n"),
            "feat: side",
            base + i,
        );
    }
    let before = heatmap_json_with(root, &["--window", "all", "--detail", "bus-factor"]);
    assert!(sorted_paths(&before, "fix_risk").contains(&"gone.rs".into()));
    assert!(sorted_paths(&before, "bus_factor").contains(&"gone.rs".into()));

    let blob = git(&["rev-parse", "HEAD:live.rs"]);
    let commit = git(&["rev-parse", "HEAD"]);
    git(&["branch", "before-removal"]);
    git(&["rm", "--", "gone.rs"]);
    git(&[
        "update-index",
        "--cacheinfo",
        &format!("120000,{blob},link.rs"),
    ]);
    git(&[
        "update-index",
        "--cacheinfo",
        &format!("160000,{commit},module.rs"),
    ]);
    git_commit_staged(root, "feat: remove regular files", base + 11);
    git_commit(
        root,
        "binary.dat",
        "binary\0data",
        "feat: add binary",
        base + 12,
    );

    let json = heatmap_json_with(root, &["--window", "all", "--detail", "bus-factor"]);
    assert_eq!(sorted_paths(&json, "files"), ["live.rs", "side.rs"]);
    assert_eq!(json["left_out"]["not_at_head"], 3);
    assert_eq!(json["coupling_graph"], serde_json::json!([]));
    assert_eq!(
        file_entry(&json, "live.rs")["blast_radius"],
        serde_json::json!([])
    );
    assert_eq!(sorted_paths(&json, "fix_risk"), ["live.rs"]);
    assert_eq!(sorted_paths(&json, "bus_factor"), ["live.rs", "side.rs"]);
    let text = heatmap_stdout(root, &["--window", "all"]);
    assert!(text.contains("3 paths not at HEAD"), "{text}");

    let card = heatmap_json_with(root, &["--window", "all", "gone.rs"]);
    assert_eq!(card["cards"][0]["commits"], 6);
    assert_eq!(card["cards"][0]["partners"][0]["path"], "live.rs");
    assert_eq!(card["cards"][0]["partners"].as_array().unwrap().len(), 1);
    assert_eq!(card["files"], serde_json::json!([]));

    let diff = heatmap_json_with(root, &["--window", "all", "--diff", "before-removal"]);
    assert!(
        diff["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|warning| { !warning.as_str().unwrap().contains("gone.rs") })
    );
}

fn git_move(dir: &Path, old: &str, new: &str) {
    std::fs::create_dir_all(dir.join(new).parent().unwrap()).unwrap();
    let out = StdCommand::new("git")
        .args(["mv", "--", old, new])
        .current_dir(dir)
        .output()
        .expect("git mv");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_commit_move(dir: &Path, old: &str, new: &str, message: &str, timestamp: u64) {
    git_move(dir, old, new);
    git_commit_staged(dir, message, timestamp);
}

#[test]
fn test_heatmap_plain_rename_moves_history_to_new_path() {
    let dir = tempfile::tempdir().unwrap();
    git_init(dir.path());
    let content = "fn work() {}\n".repeat(20);
    git_commit(dir.path(), "old.rs", &content, "feat: add", 1_700_000_000);
    git_move(dir.path(), "old.rs", "new.rs");
    git_commit(
        dir.path(),
        "new.rs",
        &(content + "// edit\n"),
        "fix: move",
        1_700_000_001,
    );

    let json = heatmap_json_with(dir.path(), &["--window", "all"]);
    assert_eq!(json["files"].as_array().unwrap().len(), 1, "{json}");
    assert_eq!(file_entry(&json, "new.rs")["churn"]["commits"], 2);
}

#[test]
fn test_heatmap_rename_uses_the_new_path_in_all_metric_sections() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git_init(root);
    let ts = 1_700_000_000;
    for i in 0..5 {
        git_commit_paths_as(
            root,
            &["a.rs", "partner.rs"],
            &format!("fix: before {i}"),
            ts + i,
            "Alice",
        );
    }
    git_commit_move(root, "a.rs", "b.rs", "refactor: rename", ts + 5);
    for i in 0..5 {
        git_commit_paths_as(
            root,
            &["b.rs", "partner.rs"],
            &format!("fix: after {i}"),
            ts + 6 + i,
            "Alice",
        );
    }
    // A second author and a lower base fix rate let both path names meet the gates.
    for i in 0..12 {
        git_commit_as(
            root,
            "other.rs",
            &format!("feat: other {i}"),
            ts + 11 + i,
            "Bob",
        );
    }

    let json = heatmap_json_with(root, &["--window", "all", "--detail", "bus-factor"]);
    let mut old_path_sections = Vec::new();
    for key in ["files", "fix_risk", "bus_factor"] {
        let rows = json[key].as_array().unwrap();
        assert!(
            rows.iter().any(|row| row["path"] == "b.rs"),
            "b.rs missing from {key}: {rows:?}"
        );
        if rows.iter().any(|row| row["path"] == "a.rs") {
            old_path_sections.push(key);
        }
    }
    let edges = json["coupling_graph"].as_array().unwrap();
    assert!(
        edges
            .iter()
            .any(|edge| edge["a"] == "b.rs" && edge["b"] == "partner.rs"),
        "b.rs missing from coupling_graph: {edges:?}"
    );
    if edges
        .iter()
        .any(|edge| edge["a"] == "a.rs" || edge["b"] == "a.rs")
    {
        old_path_sections.push("coupling_graph");
    }
    assert!(
        old_path_sections.is_empty(),
        "a.rs remains in {old_path_sections:?}"
    );
    assert_eq!(
        json["left_out"]["not_at_head"].as_u64().unwrap_or(0),
        0,
        "the HEAD filter must not hide the old path"
    );
    assert_eq!(file_entry(&json, "b.rs")["churn"]["commits"], 10);
}

#[test]
fn test_heatmap_brace_rename_paths_use_the_new_path() {
    let dir = tempfile::tempdir().unwrap();
    git_init(dir.path());
    let moves = [
        ("src/old/a.rs", "src/new/a.rs"),
        ("old/b.rs", "new/b.rs"),
        ("src/c.rs", "src/added/c.rs"),
        ("src/gone/d.rs", "src/d.rs"),
    ];
    git_commit_files(
        dir.path(),
        &moves.map(|(old, _)| (old, old)),
        "feat: add",
        1_700_000_000,
    );
    for (old, new) in moves {
        git_move(dir.path(), old, new);
    }
    git_commit_staged(dir.path(), "refactor: move", 1_700_000_001);

    let json = heatmap_json_with(dir.path(), &["--window", "all"]);
    assert_eq!(
        json["files"].as_array().unwrap().len(),
        moves.len(),
        "{json}"
    );
    for (_, new) in moves {
        assert_eq!(file_entry(&json, new)["churn"]["commits"], 1);
    }
}

#[test]
fn test_heatmap_rename_chain_leaves_reused_old_path_separate() {
    let dir = tempfile::tempdir().unwrap();
    git_init(dir.path());
    let ts = 1_700_000_000;
    git_commit(dir.path(), "old.rs", "first\n", "feat: add original", ts);
    git_commit_move(
        dir.path(),
        "old.rs",
        "middle.rs",
        "refactor: move once",
        ts + 1,
    );
    git_commit(
        dir.path(),
        "middle.rs",
        "second\n",
        "feat: edit original",
        ts + 2,
    );
    git_commit_move(
        dir.path(),
        "middle.rs",
        "final.rs",
        "refactor: move twice",
        ts + 3,
    );
    git_commit(
        dir.path(),
        "old.rs",
        "another file\n",
        "feat: add another file",
        ts + 4,
    );

    let json = heatmap_json_with(dir.path(), &["--window", "all"]);
    assert_eq!(json["files"].as_array().unwrap().len(), 2, "{json}");
    assert_eq!(file_entry(&json, "final.rs")["churn"]["commits"], 2);
    assert_eq!(file_entry(&json, "old.rs")["churn"]["commits"], 1);
}

#[test]
fn test_heatmap_rename_alias_applies_before_exclusion_and_cards() {
    let dir = tempfile::tempdir().unwrap();
    git_init(dir.path());
    git_commit(
        dir.path(),
        "Cargo.lock",
        "first\n",
        "feat: add",
        1_700_000_000,
    );
    git_commit_move(
        dir.path(),
        "Cargo.lock",
        "lib.rs",
        "refactor: move",
        1_700_000_001,
    );
    git_commit(dir.path(), "lib.rs", "second\n", "fix: edit", 1_700_000_002);

    let json = heatmap_json_with(dir.path(), &["--window", "all"]);
    assert_eq!(file_entry(&json, "lib.rs")["churn"]["commits"], 2);
    let card = heatmap_stdout(dir.path(), &["--window", "all", "lib.rs"]);
    assert_eq!(card_lines(&card, "lib.rs")[0], "lib.rs: 2 commits");
}

#[test]
fn test_heatmap_rename_arrow_in_file_name_stays_literal() {
    let dir = tempfile::tempdir().unwrap();
    git_init(dir.path());
    let path = "src/{old => new}.rs";
    git_commit(dir.path(), path, "first\n", "feat: add", 1_700_000_000);

    let json = heatmap_json_with(dir.path(), &["--window", "all"]);
    assert_eq!(file_entry(&json, path)["churn"]["commits"], 1);
}

#[test]
fn test_heatmap_rename_only_commit_does_not_add_a_touch() {
    let dir = tempfile::tempdir().unwrap();
    git_init(dir.path());
    git_commit(
        dir.path(),
        "src/old.rs",
        "first\n",
        "feat: add",
        1_700_000_000,
    );
    git_commit_move(
        dir.path(),
        "src/old.rs",
        "src/new.rs",
        "refactor: move",
        1_700_000_001,
    );

    let json = heatmap_json_with(dir.path(), &["--window", "all"]);
    assert_eq!(json["window"]["commits_analyzed"], 1);
    assert_eq!(file_entry(&json, "src/new.rs")["churn"]["commits"], 1);
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

/// The one run warning of [`create_repo_with_60_file_commit`].
const LARGE_COMMIT_WARNING: &str = "coupling: 1 commit with more than 50 files not paired";

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
    let expected = LARGE_COMMIT_WARNING;

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
fn test_insights_text_and_json_carry_the_run_warnings() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_repo_with_60_file_commit(dir.path());
    let expected = LARGE_COMMIT_WARNING;

    let json = heatmap_json_with(dir.path(), &["--insights"]);
    assert_eq!(json["warnings"], serde_json::json!([expected]), "{json}");

    let text = heatmap_stdout(dir.path(), &["--insights"]);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("Insights: "), "{text}");
    assert_eq!(lines[1], format!("warning: {expected}"), "{text}");
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("warning: ")).count(),
        1,
        "{text}"
    );
}

fn coupling_warnings(text: &str) -> Vec<&str> {
    text.lines()
        .filter_map(|line| line.strip_prefix("warning: "))
        .filter(|warning| warning.starts_with("coupling: "))
        .collect()
}

#[test]
fn test_heatmap_card_names_the_file_in_one_large_commit() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_repo_with_60_file_commit(dir.path());
    let expected = format!("{LARGE_COMMIT_WARNING} (1 touches bulk/f00.rs)");

    for args in [&["bulk/f00.rs"][..], &["--insights", "bulk/f00.rs"][..]] {
        let text = heatmap_stdout(dir.path(), args);
        assert_eq!(coupling_warnings(&text), [expected.as_str()], "{text}");
        let json = heatmap_json_with(dir.path(), args);
        assert_eq!(json["warnings"], serde_json::json!([expected]), "{json}");
    }
}

#[test]
fn test_heatmap_card_uses_the_run_count_and_counts_both_touches() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    let target = "tests/flags-cli.test.ts";
    let names: Vec<_> = (0..50).map(|i| format!("bulk/f{i:02}.rs")).collect();
    let ts = recent_ts();
    for (offset, body, changed) in [
        (0, "// one", target),
        (60, "// two", target),
        (120, "// three", "src/other.rs"),
    ] {
        let files: Vec<_> = names
            .iter()
            .map(|name| (name.as_str(), body))
            .chain([(changed, body)])
            .collect();
        git_commit_files(dir.path(), &files, "chore: bulk change", ts + offset);
    }

    let global = "coupling: 3 commits with more than 50 files not paired";
    let text = heatmap_stdout(dir.path(), &[target]);
    assert_eq!(
        coupling_warnings(&text),
        [format!("{global} (2 touch {target})")],
        "{text}"
    );
    let plain = heatmap_stdout(dir.path(), &[]);
    assert_eq!(coupling_warnings(&plain), [global], "{plain}");
}

#[test]
fn test_heatmap_card_omits_the_warning_without_a_large_commit_touch() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_repo_with_60_file_commit(dir.path());
    let args = ["src/main.rs"];

    let text = heatmap_stdout(dir.path(), &args);
    assert!(coupling_warnings(&text).is_empty(), "{text}");
    let json = heatmap_json_with(dir.path(), &args);
    assert_eq!(json["warnings"], serde_json::json!([]), "{json}");
    let plain = heatmap_stdout(dir.path(), &[]);
    assert_eq!(coupling_warnings(&plain), [LARGE_COMMIT_WARNING], "{plain}");
}

#[test]
fn test_heatmap_card_warning_names_targets_in_argument_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_repo_with_60_file_commit(dir.path());

    let text = heatmap_stdout(dir.path(), &["bulk/f12.rs", "bulk/f00.rs"]);
    assert_eq!(
        coupling_warnings(&text),
        [format!(
            "{LARGE_COMMIT_WARNING} (1 touches bulk/f12.rs, 1 touches bulk/f00.rs)"
        )],
        "{text}"
    );
}

#[test]
fn test_heatmap_card_warning_uses_the_safe_path_in_text_and_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    assert!(
        StdCommand::new("git")
            .args(["config", "core.quotePath", "false"])
            .current_dir(dir.path())
            .status()
            .expect("git config")
            .success()
    );
    let target = "bulk/b\u{202E}c.rs";
    let names: Vec<_> = (0..50).map(|i| format!("bulk/f{i:02}.rs")).collect();
    let files: Vec<_> = names
        .iter()
        .map(|name| (name.as_str(), "// bulk"))
        .chain([(target, "// bulk")])
        .collect();
    git_commit_files(dir.path(), &files, "chore: bulk reformat", recent_ts());

    let text = heatmap_stdout(dir.path(), &[target]);
    let warnings = coupling_warnings(&text);
    assert_eq!(
        warnings,
        [format!(
            "{LARGE_COMMIT_WARNING} (1 touches bulk/b\u{FFFD}c.rs)"
        )],
        "{text:?}"
    );
    assert!(!text.contains('\u{202E}'), "{text:?}");
    let json = heatmap_json_with(dir.path(), &[target]);
    assert_eq!(json["warnings"], serde_json::json!(warnings), "{json:?}");
    assert_eq!(json["cards"][0]["path"], "bulk/b\u{FFFD}c.rs", "{json:?}");
}

fn diff_warning_repo(root: &Path) {
    git_init(root);
    git_config(root, "core.quotePath", "false");
    let git = |args: &[&str]| {
        let out = StdCommand::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
    };
    let gone = "gone\u{202e}.rs";
    let ts = recent_ts();
    git_commit(root, gone, "// gone\n", "feat: add file", ts);
    git(&["branch", "base"]);
    git(&["rm", "--", gone]);
    git_commit(root, "live.rs", "// live\n", "feat: replace file", ts + 1);
}

#[cfg(unix)]
#[test]
fn test_heatmap_diff_keeps_spaces_in_repository_and_file_names() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo ");
    std::fs::create_dir(&root).unwrap();
    git_init(&root);
    git_commit(&root, "first.rs", "one\n", "feat: first", 1_700_000_000);
    let path = " spaced.rs ";
    git_commit(&root, path, "one\ntwo\n", "feat: second", 1_700_000_001);

    let json = heatmap_json_with(&root, &["--window", "all", "--diff", "HEAD~1"]);
    assert_eq!(json["repository"], "repo ");
    assert_eq!(sorted_paths(&json, "files"), [path]);
    assert_eq!(file_entry(&json, path)["lines"], 2);
    assert_eq!(json["warnings"], serde_json::json!([]));
}

#[cfg(unix)]
#[test]
fn test_heatmap_diff_drops_invalid_paths_without_widening_scope() {
    let repo = tempfile::tempdir().unwrap();
    diff_warning_repo(repo.path());
    let tools = tempfile::tempdir().unwrap();
    common::write_stub_script(
        tools.path(),
        "git",
        r#"#!/bin/sh
if [ "$3" = diff ]; then
    printf '%s\n' '"\056\056/outside.rs"'
    if [ "$HEATMAP_TEST_INCLUDE_VALID" = yes ]; then
        printf '%s\n' live.rs
    fi
    exit 0
fi
PATH=$HEATMAP_TEST_PATH exec git "$@"
"#,
    );
    let run = |include_valid: &str| {
        common::skim()
            .env("PATH", common::stub_path(tools.path()))
            .env("HEATMAP_TEST_PATH", std::env::var_os("PATH").unwrap())
            .env("HEATMAP_TEST_INCLUDE_VALID", include_valid)
            .args(["heatmap", "--window", "all", "--diff", "base", "--json"])
            .current_dir(repo.path())
            .output()
            .unwrap()
    };
    let mixed = run("yes");
    let rejected = run("no");
    assert_eq!(
        (mixed.status.code(), rejected.status.code()),
        (Some(0), Some(1))
    );
    assert!(rejected.stdout.is_empty(), "{rejected:?}");
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("no valid changed paths"));
    let json: serde_json::Value = serde_json::from_slice(&mixed.stdout).unwrap();
    assert_eq!(json["file_targets"], serde_json::json!(["live.rs"]));
    assert_eq!(sorted_paths(&json, "files"), ["live.rs"]);
    assert_eq!(json["warnings"], serde_json::json!([]));
    let stderr = String::from_utf8(mixed.stderr).unwrap();
    assert!(stderr.contains("1 invalid changed path"), "{stderr}");
    assert!(!stderr.contains("outside.rs"), "{stderr}");
}

#[cfg(unix)]
#[test]
fn test_heatmap_debug_shows_repository_root_in_display_form() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo\u{0007}root");
    std::fs::create_dir(&root).unwrap();
    git_init(&root);
    git_commit(&root, "live.rs", "one\n", "feat: add file", 1_700_000_000);

    let output = common::skim()
        .env("SKIM_DEBUG", "1")
        .args(["heatmap", "--window", "all", "--debug", "--json"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    let trace = stderr
        .lines()
        .find(|line| line.starts_with("[skim:heatmap] repo root: "))
        .unwrap();
    assert!(trace.ends_with("/repo�root"), "{trace:?}");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(file_entry(&json, "live.rs")["lines"], 1);
}

#[test]
fn test_heatmap_diff_warning_paths_match_in_stderr_and_json() {
    let dir = tempfile::tempdir().unwrap();
    diff_warning_repo(dir.path());
    let output = common::skim()
        .env_remove("SKIM_DEBUG")
        .args([
            "heatmap",
            "--window",
            "all",
            "--diff",
            "base",
            "--exclude",
            "gone*.rs",
            "--json",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        stderr,
        "skim heatmap: warning: 'gone�.rs' is not a regular file on current branch\n"
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json["warnings"],
        serde_json::json!(["targeted file 'gone�.rs' not found in git history"])
    );
}

#[cfg(unix)]
#[test]
fn test_heatmap_diff_warning_with_closed_stderr_keeps_stdout() {
    let dir = tempfile::tempdir().unwrap();
    diff_warning_repo(dir.path());
    let args = ["heatmap", "--window", "all", "--diff", "base", "--json"];
    let baseline = common::skim()
        .args(args)
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(baseline.status.success(), "{baseline:?}");
    assert!(String::from_utf8_lossy(&baseline.stderr).contains("is not a regular file"));

    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let output = StdCommand::new(assert_cmd::cargo::cargo_bin!("skim"))
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env_remove("SKIM_DEBUG")
        .args(args)
        .current_dir(dir.path())
        .stderr(std::process::Stdio::from(writer))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(!output.stdout.is_empty());
    assert_eq!(output.stdout, baseline.stdout);
}

#[test]
fn test_heatmap_diff_keeps_the_global_large_commit_warning() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_repo_with_60_file_commit(dir.path());
    assert!(
        StdCommand::new("git")
            .args(["checkout", "-b", "feature-card"])
            .current_dir(dir.path())
            .output()
            .expect("git checkout")
            .status
            .success()
    );
    git_commit(
        dir.path(),
        "src/main.rs",
        "// branch",
        "feat: branch",
        recent_ts() + 200,
    );

    let args = ["--diff", "main"];
    let text = heatmap_stdout(dir.path(), &args);
    assert_eq!(coupling_warnings(&text), [LARGE_COMMIT_WARNING], "{text}");
    let json = heatmap_json_with(dir.path(), &args);
    assert_eq!(json["file_targets"], serde_json::json!(["src/main.rs"]));
    assert_eq!(json["warnings"], serde_json::json!([LARGE_COMMIT_WARNING]));
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

    // --no-exclude alone keeps the excluded files, but the non-code rule
    // still leaves out the doc file and the two files under tests/.
    let code_only = common::skim()
        .args(["heatmap", "--json", "--no-exclude"])
        .current_dir(dir.path())
        .output()
        .expect("spawn skim heatmap --no-exclude");
    assert!(code_only.status.success());
    let code_only: serde_json::Value =
        serde_json::from_slice(&code_only.stdout).expect("valid JSON");
    assert_eq!(code_only["left_out"]["hotspots"], 3, "{code_only}");

    let all = common::skim()
        .args(["heatmap", "--json", "--no-exclude", "--include-docs"])
        .current_dir(dir.path())
        .output()
        .expect("spawn skim heatmap --no-exclude --include-docs");
    assert!(all.status.success());
    let all: serde_json::Value = serde_json::from_slice(&all.stdout).expect("valid JSON");
    assert!(all.get("left_out").is_none(), "{all}");
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

/// Issue #597: a card lists only commits whose subject says they fix
/// something, not commits that mention a fix keyword in a description.
#[test]
fn test_heatmap_card_fix_commits_skip_subjects_that_only_mention_a_keyword() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    let mut ts = recent_ts();
    let file = ["src/a.rs".to_string()];
    for subject in [
        "feat: add a",
        "docs: explain later-fix proximity",
        "feat: add bug report template",
        "fix(a): handle empty input",
        "Revert \"feat: add a\"",
    ] {
        ts += 60;
        git_commit_paths(dir.path(), &file, subject, ts);
    }
    let text = heatmap_stdout(dir.path(), &["src/a.rs", "--top", "5"]);
    assert!(text.contains("\"fix(a): handle empty input\""), "{text}");
    assert!(text.contains("Revert"), "{text}");
    assert!(!text.contains("later-fix"), "{text}");
    assert!(!text.contains("bug report"), "{text}");
}

#[test]
fn test_heatmap_default_text_fits_in_thirty_lines_with_more_lines() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_ranked_repo(dir.path());

    let text = heatmap_stdout(dir.path(), &[]);
    let lines: Vec<&str> = text.lines().collect();
    // header 1; hotspots 1 + 10 + 1; coupling 1 + 5 + 1; fix risk 1 + 5 + 1;
    // the card hint 1.
    assert_eq!(lines.len(), 28, "{text}");
    assert_eq!(
        lines.last(),
        Some(&"Before you edit a file: skim heatmap <file>"),
        "{text}"
    );
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
    let mut expected: Vec<String> = (0..8)
        .map(|i| format!("    6 x 1 = 6  src/hub/h{i}.rs"))
        .collect();
    expected.push("    5 x 1 = 5  src/pair/p0a.rs".to_string());
    expected.push("    5 x 1 = 5  src/pair/p0b.rs".to_string());
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
fn test_heatmap_hotspots_cap_the_one_line_files_in_score_order() {
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
            let (stats, path) = row.trim_start().split_once("  ").expect("stats and path");
            let (commits, score) = stats.split_once(" x 1 = ").expect("one line at HEAD");
            assert_eq!(commits, score, "{row}");
            (score.parse().expect("hotspot score"), path)
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
fn test_heatmap_fix_risk_counts_non_code_rows_and_include_docs_restores_them() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    create_five_commit_repo(root, "feat");
    let ts = recent_ts() + 600;
    for i in 0..5 {
        git_commit(
            root,
            "docs/guide.md",
            &format!("Guide {i}\n"),
            "feat: update guide",
            ts + i * 2,
        );
        let body = format!("// revision {i}\n");
        git_commit_files(
            root,
            &[
                ("src/live.rs", &body),
                ("README.md", &body),
                ("docs/example.rs", &body),
            ],
            "fix: update files",
            ts + i * 2 + 1,
        );
    }

    let args = ["--detail", "fix-risk"];
    let json = heatmap_json_with(root, &args);
    let paths = sorted_paths(&json, "fix_risk");
    assert_eq!(paths, ["src/live.rs"]);
    let all = heatmap_json_with(root, &["--detail", "fix-risk", "--include-docs"]);
    let all_paths = sorted_paths(&all, "fix_risk");
    assert_eq!(all_paths, ["README.md", "docs/example.rs", "src/live.rs"]);
    let restored = all_paths.len() - paths.len();
    assert_eq!(json["left_out"]["fix_risk"], restored);

    let text = heatmap_stdout(root, &args);
    let expected = format!("  {restored} non-code files left out (--include-docs)");
    assert!(
        section_lines(&text, "Fix risk").contains(&expected.as_str()),
        "{text}"
    );
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
fn test_heatmap_detail_coupling_caps_at_25_rows_and_top_shows_the_json_total() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_ranked_repo(dir.path());

    let text = heatmap_stdout(dir.path(), &["--detail", "coupling"]);
    let json = heatmap_json_with(dir.path(), &["--detail", "coupling"]);
    let rows = section_rows(&text, "Coupling");
    assert_eq!(rows.len(), 25, "{text}");
    assert_eq!(
        text.lines().last(),
        Some("  11 more (raise --top)"),
        "{text}"
    );
    assert_eq!(json["counts"]["coupling"]["total"], 36);
    assert_eq!(json["counts"]["coupling"]["shown"], 25);
    assert_eq!(json["coupling_graph"].as_array().unwrap().len(), 25);
    assert!(
        !text.contains("skim heatmap <file>"),
        "no card hint: {text}"
    );

    let all = heatmap_stdout(dir.path(), &["--detail", "coupling", "--top", "36"]);
    assert_eq!(section_rows(&all, "Coupling").len(), 36, "{all}");
    assert!(
        !all.contains(" more ("),
        "no row is hidden at --top 36:\n{all}"
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
fn test_heatmap_two_runs_with_a_run_warning_give_identical_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_repo_with_60_file_commit(dir.path());

    let warning_line = format!("warning: {LARGE_COMMIT_WARNING}");
    for args in [&[][..], &["--insights"][..]] {
        let first = heatmap_stdout(dir.path(), args);
        let second = heatmap_stdout(dir.path(), args);
        assert_eq!(first, second, "two runs of {args:?} differ");
        let hits = first.lines().filter(|l| *l == warning_line).count();
        assert_eq!(hits, 1, "{args:?}:\n{first}");
    }
    for args in [&["--json"][..], &["--insights", "--json"][..]] {
        let first = heatmap_stdout(dir.path(), args);
        let second = heatmap_stdout(dir.path(), args);
        assert_eq!(first, second, "two runs of {args:?} differ");
        let json: serde_json::Value = serde_json::from_str(&first).expect("valid JSON");
        assert_eq!(
            json["warnings"],
            serde_json::json!([LARGE_COMMIT_WARNING]),
            "{args:?}"
        );
    }
}

/// Kinds of run warning: `WARNING_KINDS` in `cmd/heatmap/mod.rs`. The test
/// can not import it from the binary crate; the debug binary under test
/// asserts the same bound on every run.
const WARNING_KINDS: usize = 8;

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
    let ranked_patterns = json["excluded_patterns"].clone();

    // The run warnings: at most one line for each kind, none twice, and the
    // same list in both JSON outputs.
    let dir = tempfile::tempdir().expect("tempdir");
    create_repo_with_60_file_commit(dir.path());
    let default = heatmap_json(dir.path());
    let insights = heatmap_json_with(dir.path(), &["--insights"]);
    for json in [&default, &insights] {
        let warnings: Vec<&str> = json["warnings"]
            .as_array()
            .expect("warnings array")
            .iter()
            .map(|w| w.as_str().expect("warning text"))
            .collect();
        let distinct: std::collections::HashSet<&str> = warnings.iter().copied().collect();
        assert!(warnings.len() <= WARNING_KINDS, "{warnings:?}");
        assert_eq!(
            distinct.len(),
            warnings.len(),
            "a warning twice: {warnings:?}"
        );
    }
    assert_eq!(insights["warnings"], default["warnings"]);
    assert_eq!(
        default["warnings"],
        serde_json::json!([LARGE_COMMIT_WARNING])
    );

    // excluded_patterns has no cap: it is the fixed list of the default
    // excludes and the --exclude values, so the history does not change it.
    // The cap of file_targets (20) has its exact test in
    // test_heatmap_large_diff_caps_file_targets_and_gives_one_missing_warning.
    let patterns = default["excluded_patterns"].as_array().expect("patterns");
    assert!(!patterns.is_empty());
    assert_eq!(default["excluded_patterns"], ranked_patterns);
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
            let escaped = subject.replace('\\', "\\\\").replace('"', "\\\"");
            subject
                .starts_with("fix")
                .then(|| format!("    {} {date} \"{escaped}\"", &hash[..7]))
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
        r#"  owners: "Test User" 89%, "Ada|Lovelace" 11%"#,
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
    assert_eq!(owners, [r#"  owners: "Test User" 89%, "Ada|Lovelace" 11%"#]);
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

    assert!(
        text.lines().any(|l| l == "Hotspots (commits x lines):"),
        "{text}"
    );
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

fn create_lockfile_repo(dir: &Path) {
    git_init(dir);
    git_commit(dir, "package-lock.json", "{}", "chore: lock", recent_ts());
}

#[test]
fn test_heatmap_card_names_the_default_filter_when_it_drops_all_commits() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_lockfile_repo(dir.path());
    let args = ["package-lock.json"];

    let text = heatmap_stdout(dir.path(), &args);
    assert_eq!(
        card_lines(&text, "package-lock.json"),
        ["package-lock.json: left out by the default excludes; --no-exclude keeps it"],
        "{text}"
    );
    let json = heatmap_json_with(dir.path(), &args);
    let card = &json["cards"][0];
    assert_eq!(card["path"], "package-lock.json");
    assert_eq!(card["in_window"], false);
    assert_eq!(card["excluded"], "default");
    assert_eq!(json["window"]["commits_analyzed"], 0);
    assert!(
        !json.to_string().contains("not in history window"),
        "{json}"
    );
}

#[test]
fn test_heatmap_card_keeps_the_lockfile_with_no_exclude() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_lockfile_repo(dir.path());
    let args = ["--no-exclude", "package-lock.json"];

    let text = heatmap_stdout(dir.path(), &args);
    assert_eq!(
        top_level_lines(&text),
        ["package-lock.json: 1 commit"],
        "{text}"
    );
    let json = heatmap_json_with(dir.path(), &args);
    let card = &json["cards"][0];
    assert_eq!(card["in_window"], true);
    assert_eq!(card["commits"], 1);
    assert!(card.get("excluded").is_none(), "{card}");
}

#[test]
fn test_heatmap_refuses_an_invalid_exclude_pattern() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    git_commit(dir.path(), "gen/a.rs", "// a", "feat: a", recent_ts());

    common::skim()
        .args(["heatmap", "--exclude", "gen/{a,b", "gen/a.rs"])
        .current_dir(dir.path())
        .assert()
        .code(1)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("gen/{a,b"))
        .stderr(predicate::str::contains("unclosed alternate group"));
}

#[test]
fn test_heatmap_card_names_only_the_user_filter() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_repo_with_60_file_commit(dir.path());
    let args = ["--exclude", "bulk/f00.rs", "bulk/f00.rs"];

    let text = heatmap_stdout(dir.path(), &args);
    assert_eq!(
        card_lines(&text, "bulk/f00.rs"),
        ["bulk/f00.rs: left out by --exclude; remove the matching patterns to keep it"],
        "{text}"
    );
    assert!(coupling_warnings(&text).is_empty(), "{text}");
    let json = heatmap_json_with(dir.path(), &args);
    let card = &json["cards"][0];
    assert_eq!(card["in_window"], false);
    assert_eq!(card["excluded"], "user");
    assert_eq!(json["warnings"], serde_json::json!([]));
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
        r#"  owners: "Test User" 89%"#,
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
fn test_heatmap_two_forms_of_one_file_give_one_card() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_card_repo(dir.path());

    let json = heatmap_json_with(dir.path(), &[CARD_FILE, "./src/core.rs", "src/util.rs"]);

    assert_eq!(
        json["counts"]["cards"],
        serde_json::json!({"total": 2, "shown": 2})
    );
    let paths: Vec<&str> = json["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, [CARD_FILE, "src/util.rs"]);
}

/// Three folders below one temp folder: two repos, `a` (the card repo) and
/// `b` (one commit), and `plain`, a folder in no repo. Git looks for a
/// repo no higher than the temp folder, so a repo of the host machine
/// cannot hold `plain`. The temp folder path has no symlink in it.
struct RepoPair {
    _tmp: tempfile::TempDir,
    root: std::path::PathBuf,
}

impl RepoPair {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonical temp folder");
        for name in ["a", "b", "plain"] {
            std::fs::create_dir(root.join(name)).expect("create folder");
        }
        create_card_repo(&root.join("a"));
        git_init(&root.join("b"));
        git_commit(
            &root.join("b"),
            "main.rs",
            "fn main() {}",
            "feat: b",
            recent_ts(),
        );
        std::fs::write(root.join("plain/loose.rs"), "fn loose() {}").expect("write file");
        Self { _tmp: tmp, root }
    }

    fn path(&self, rel: &str) -> std::path::PathBuf {
        self.root.join(rel)
    }

    fn heatmap(&self, cwd: &str, args: &[&str]) -> std::process::Output {
        common::skim()
            .arg("heatmap")
            .args(args)
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .current_dir(self.path(cwd))
            .output()
            .expect("spawn skim heatmap")
    }
}

#[test]
fn test_heatmap_card_of_a_file_in_another_repo_comes_from_that_repo() {
    let repos = RepoPair::new();
    let own = repos.heatmap("a", &[CARD_FILE]);
    assert!(own.status.success(), "{own:?}");
    let expected = String::from_utf8(own.stdout).unwrap();
    assert_eq!(
        card_lines(&expected, CARD_FILE).first().copied(),
        Some("src/core.rs: 9 commits")
    );

    let absolute = repos.path("a/src/core.rs");
    let absolute = absolute.to_str().unwrap();
    for (cwd, arg) in [
        ("b", absolute),
        ("b", "../a/src/core.rs"),
        ("b", "../a/src/../src/./core.rs"),
        ("plain", absolute),
        ("plain", "../a/src/core.rs"),
    ] {
        let output = repos.heatmap(cwd, &[arg]);
        assert!(output.status.success(), "{cwd} {arg}: {output:?}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            expected,
            "{cwd} {arg}"
        );
    }
}

#[test]
fn test_heatmap_path_scope_with_a_card_in_another_repo_is_relative_to_that_root() {
    let repos = RepoPair::new();
    let own = repos.heatmap("a", &["--path", "src", CARD_FILE]);
    assert!(own.status.success(), "{own:?}");
    let other = repos.heatmap("b", &["--path", "src", "../a/src/core.rs"]);
    assert!(other.status.success(), "{other:?}");
    assert_eq!(other.stdout, own.stdout);
    // The scope narrows the history: the card differs from the full one.
    let full = repos.heatmap("a", &[CARD_FILE]);
    assert_ne!(own.stdout, full.stdout);
}

#[test]
fn test_heatmap_json_card_of_a_file_in_another_repo_names_the_repo_path() {
    let repos = RepoPair::new();
    let output = repos.heatmap("b", &["--json", "../a/src/core.rs", "../a/src/util.rs"]);
    assert!(output.status.success(), "{output:?}");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json["file_targets"],
        serde_json::json!(["src/core.rs", "src/util.rs"])
    );
}

#[test]
fn test_heatmap_card_path_in_no_repo_exits_1_and_names_the_path() {
    let repos = RepoPair::new();
    let loose = repos.path("plain/loose.rs");
    let loose = loose.to_str().unwrap().to_string();
    let missing = repos.path("plain/missing/gone.rs");
    let missing = missing.to_str().unwrap().to_string();

    for (cwd, arg) in [
        ("a", "../plain/loose.rs"),
        ("a", "src/../../plain/loose.rs"),
        ("a", loose.as_str()),
        ("a", missing.as_str()),
        ("plain", "loose.rs"),
        ("b", "../outside.rs"),
    ] {
        let output = repos.heatmap(cwd, &[arg]);
        assert_eq!(output.status.code(), Some(1), "{cwd} {arg}");
        assert!(output.stdout.is_empty(), "{cwd} {arg}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!("'{arg}' is in no git repository")),
            "{cwd} {arg}: {stderr}"
        );
    }
}

#[test]
fn test_heatmap_cards_from_two_repos_exit_1_with_no_card() {
    let repos = RepoPair::new();
    for (cwd, args) in [
        ("a", [CARD_FILE, "../b/main.rs"]),
        ("b", ["main.rs", "../a/src/core.rs"]),
        ("plain", ["../a/src/core.rs", "../b/main.rs"]),
    ] {
        let output = repos.heatmap(cwd, &args);
        assert_eq!(output.status.code(), Some(1), "{cwd} {args:?}");
        assert!(output.stdout.is_empty(), "{cwd} {args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!(
                "'{}' and '{}' are in different repositories",
                args[0], args[1]
            )),
            "{cwd} {args:?}: {stderr}"
        );
    }
}

fn stdout_of(output: &std::process::Output) -> String {
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout.clone()).expect("utf-8 stdout")
}

fn assert_refused(output: &std::process::Output, message: &str) {
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(message), "want {message:?} in {stderr}");
}

#[test]
fn test_heatmap_card_of_a_repo_root_names_the_root() {
    let repos = RepoPair::new();
    for (cwd, arg) in [
        ("a", "."),
        ("a", "./"),
        ("a/src", ".."),
        ("b", "../a"),
        ("plain", "../a"),
    ] {
        let output = repos.heatmap(cwd, &[arg]);
        assert_refused(
            &output,
            &format!("'{arg}' names the repository root, not a file"),
        );
    }
}

#[test]
fn test_heatmap_card_with_git_dir_set_keeps_the_cwd_repo() {
    let repos = RepoPair::new();
    let expected = stdout_of(&repos.heatmap("a", &[CARD_FILE]));
    let absolute = repos.path("a/.git");
    for git_dir in [std::ffi::OsStr::new(".git"), absolute.as_os_str()] {
        let output = common::skim()
            .args(["heatmap", CARD_FILE])
            .env("GIT_CEILING_DIRECTORIES", &repos.root)
            .env("GIT_DIR", git_dir)
            .current_dir(repos.path("a"))
            .output()
            .unwrap();
        assert_eq!(stdout_of(&output), expected, "GIT_DIR={git_dir:?}");
    }
}

#[cfg(unix)]
#[test]
fn test_heatmap_card_through_a_symlinked_folder_in_the_cwd_repo_keeps_the_cwd_repo() {
    let repos = RepoPair::new();
    // The link is in repo `a`, and it points into repo `b`.
    std::os::unix::fs::symlink(repos.path("b"), repos.path("a/linkdir")).expect("symlink");
    let text = stdout_of(&repos.heatmap("a", &["linkdir/main.rs"]));
    let card = card_lines(&text, "linkdir/main.rs");
    assert_eq!(card.len(), 1, "{text}");
    assert!(
        card[0].starts_with("linkdir/main.rs: not in history window ("),
        "{text}"
    );
}

#[test]
fn test_heatmap_card_path_that_leaves_and_reenters_the_cwd_repo_gives_the_card() {
    let repos = RepoPair::new();
    let expected = stdout_of(&repos.heatmap("a", &[CARD_FILE]));
    assert_eq!(
        stdout_of(&repos.heatmap("a", &["../a/src/core.rs"])),
        expected
    );
    assert_eq!(
        stdout_of(&repos.heatmap("a/src", &["../../a/src/core.rs"])),
        expected
    );
}

#[test]
fn test_heatmap_card_with_path_scope_from_a_subfolder_keeps_the_cwd_scope() {
    let repos = RepoPair::new();
    let expected = stdout_of(&repos.heatmap("a", &["--path", "src", CARD_FILE]));
    assert_eq!(
        stdout_of(&repos.heatmap("a/src", &["--path", ".", "core.rs"])),
        expected
    );
}

#[test]
fn test_heatmap_card_git_failure_in_another_repo_names_the_path() {
    let repos = RepoPair::new();
    let output = repos.heatmap("b", &["../a/.git/config"]);
    assert_refused(
        &output,
        "'../a/.git/config': git rev-parse --show-toplevel failed",
    );
}

#[test]
fn test_heatmap_cards_in_one_folder_of_another_repo_ask_git_once_for_the_root() {
    let repos = RepoPair::new();
    let trace = repos.path("trace.log");
    let output = common::skim()
        .args([
            "heatmap",
            "../a/src/core.rs",
            "../a/src/util.rs",
            "../a/src/gone.rs",
        ])
        .env("GIT_CEILING_DIRECTORIES", &repos.root)
        .env("GIT_TRACE", &trace)
        .current_dir(repos.path("b"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let log = std::fs::read_to_string(&trace).expect("git trace");
    // One call for the cwd root, one for the folder `a/src`, and one in the
    // run itself. Without the cache of each folder, each file adds a call.
    let calls = log
        .lines()
        .filter(|l| l.contains("rev-parse --show-toplevel"))
        .count();
    assert_eq!(calls, 3, "{log}");
}

#[cfg(unix)]
#[test]
fn test_heatmap_card_path_through_a_symlinked_folder_stays_refused() {
    let repos = RepoPair::new();
    let link = repos.path("link");
    std::os::unix::fs::symlink(repos.path("a"), &link).expect("symlink");
    let arg = link.join("src/core.rs");
    let arg = arg.to_str().unwrap();

    let output = repos.heatmap("b", &[arg]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty());
    // This error cuts a path longer than 72 chars to 69 chars and "...".
    let shown = if arg.chars().count() <= 72 {
        arg.to_string()
    } else {
        format!("{}...", arg.chars().take(69).collect::<String>())
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("'{shown}' is outside the repository root")),
        "{stderr}"
    );
}

#[cfg(unix)]
#[test]
fn test_heatmap_card_of_a_fifo_path_returns_without_blocking() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_card_repo(dir.path());
    let made = StdCommand::new("mkfifo")
        .arg(dir.path().join("pipe"))
        .status()
        .expect("mkfifo");
    assert!(made.success());

    // The environment of `common::skim()`. `run_bounded` kills the run and
    // fails the test at its time bound: a run that opens the FIFO would block.
    let mut cmd = StdCommand::new(common::skim_bin());
    cmd.args(["heatmap", "pipe"])
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env("NO_COLOR", "1")
        .env_remove("SKIM_REWRITTEN_FROM")
        .current_dir(dir.path());
    let output = common::run_bounded(cmd);

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
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
            "  owners: \"Test User\" 67%, \"Eve\u{FFFD}x\" 33%".to_string(),
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
fn test_heatmap_default_detail_and_insights_use_the_card_path_form() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git_init(root);
    git_config(root, "core.quotePath", "false");
    let path = "src/a\u{202e}.rs";
    let shown = "src/a�.rs";
    for i in 0..3 {
        let body = format!("// revision {i}\n");
        git_commit_files(
            root,
            &[(path, &body), ("src/b.rs", &body)],
            "feat: change both",
            recent_ts() + i,
        );
    }

    for args in [&[][..], &["--detail", "hotspots"][..], &["--insights"][..]] {
        let text = heatmap_stdout(root, args);
        assert!(text.contains(shown), "{text:?}");
        assert!(!text.contains(path), "{text:?}");
    }
    let json = heatmap_json(root);
    assert_eq!(file_entry(&json, shown)["lines"], 1, "{json}");
    let insights = heatmap_json_with(root, &["--insights"]);
    assert!(
        sorted_paths(&insights, "top_files")
            .iter()
            .any(|path| path == shown),
        "{insights}"
    );
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

#[test]
fn test_heatmap_json_repository_is_the_repo_folder_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_test_repo(dir.path());
    let name = dir
        .path()
        .file_name()
        .and_then(|n| n.to_str())
        .expect("utf-8 temp folder name");

    // The default JSON, the insights JSON and the card JSON all give the
    // name that the text header shows, never the absolute path.
    for args in [&[][..], &["--insights"][..], &["src/main.rs"][..]] {
        let json = heatmap_json_with(dir.path(), args);
        let repository = json["repository"].as_str().expect("repository string");
        assert_eq!(repository, name, "--json {args:?}");
        assert!(!repository.contains('/'), "--json {args:?}: {repository}");
    }
    let text = heatmap_stdout(dir.path(), &[]);
    let header = text.lines().next().unwrap_or_default();
    assert!(header.starts_with(&format!("Heatmap: {name} ")), "{header}");
}

// 2023-11-14 22:13:20 UTC. Use explicit windows with these fixtures.
const MAP_COMMIT_TIME: u64 = 1_700_000_000;
const MAP_ARGS: &[&str] = &["--map", "--window", "all"];

fn create_map_repo(root: &Path) {
    git_init(root);
    for i in 0..3 {
        git_commit_files(
            root,
            &[
                (
                    "src/z_long.rs",
                    &format!("struct Item;\n\nfn long() {{\n    let _x = {i};\n}}\n"),
                ),
                (
                    "src/a_short.rs",
                    &format!("fn short() {{ let _x = {i}; }}\n"),
                ),
            ],
            "feat: update paired files",
            MAP_COMMIT_TIME + i,
        );
    }
}

#[test]
fn test_heatmap_map_shows_ranked_file_blocks_with_symbols_and_partners() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("map-fixture");
    std::fs::create_dir(&root).unwrap();
    create_map_repo(&root);

    let text = heatmap_stdout(&root, MAP_ARGS);
    let header = text.lines().next().unwrap();
    assert!(
        header.starts_with("Heatmap map: map-fixture 1970-01-01.."),
        "{text}"
    );
    assert!(header.ends_with(", 3 commits, all window"), "{text}");
    assert_eq!(
        text.lines().skip(1).collect::<Vec<_>>(),
        [
            "src/z_long.rs: 3 commits, 5 lines, score 15",
            "  changes with: src/a_short.rs",
            "  struct_item Item:1",
            "  function_item long:3",
            "src/a_short.rs: 3 commits, 1 line, score 3",
            "  changes with: src/z_long.rs",
            "  function_item short:1",
        ],
        "{text}"
    );
}

const MAP_CODE_FILES: usize = 32;

fn create_large_map_repo(root: &Path) {
    git_init(root);
    let code: String = (0..12)
        .map(|i| format!("fn count_unique_matches_in_bucket_{i}() {{}}\n"))
        .collect();
    let paths: Vec<_> = (0..MAP_CODE_FILES)
        .map(|i| format!("src/file_{i:02}.rs"))
        .collect();
    let mut files: Vec<_> = paths
        .iter()
        .map(|path| (path.as_str(), code.as_str()))
        .collect();
    // The larger JSON file ranks first. Its language has no symbol rows.
    let metadata = format!("[\n{}0\n]\n", "0,\n".repeat(20));
    files.push(("src/metadata.json", &metadata));
    git_commit_files(root, &files, "feat: add map fixture", MAP_COMMIT_TIME);
}

fn map_token_count(text: &str) -> usize {
    static COUNTER: std::sync::LazyLock<rskim_tokens::Counter> = std::sync::LazyLock::new(|| {
        rskim_tokens::Counter::new(rskim_tokens::Encoding::Cl100k).unwrap()
    });
    COUNTER.count(text)
}

#[test]
fn test_heatmap_map_bounds_all_stdout_at_default_and_explicit_budgets() {
    let dir = tempfile::tempdir().unwrap();
    create_large_map_repo(dir.path());

    let default = heatmap_stdout(dir.path(), MAP_ARGS);
    let small = heatmap_stdout(dir.path(), &["--map", "--window", "all", "--tokens", "300"]);
    let (default_tokens, small_tokens) = (map_token_count(&default), map_token_count(&small));
    assert!(
        default_tokens <= 1000 && small_tokens <= 300,
        "stdout tokens: {default_tokens} (limit 1000), {small_tokens} (limit 300)"
    );
    for text in [&default, &small] {
        assert!(text.starts_with("Heatmap map: "), "{text}");
        assert!(
            text.contains("  function_item count_unique_matches_in_bucket_0:1\n"),
            "{text}"
        );
    }
    assert!(default.lines().count() > small.lines().count());
}

#[test]
fn test_heatmap_map_trailer_counts_shown_files_and_symbol_losses() {
    let dir = tempfile::tempdir().unwrap();
    create_large_map_repo(dir.path());

    let text = heatmap_stdout(dir.path(), &["--map", "--window", "all", "--tokens", "300"]);
    let shown = text.lines().filter(|line| line.starts_with("src/")).count();
    let total = MAP_CODE_FILES + 1;
    assert!((2..total).contains(&shown), "{text}");
    assert!(text.contains("src/metadata.json: "), "{text}");
    assert_eq!(
        text.lines().last().unwrap(),
        format!(
            "[skim] {shown} of {total} files; language: 1 — adjust the map budget with --tokens"
        ),
        "{text}"
    );
    assert!(!text.contains("SKIM_PASSTHROUGH"), "{text}");
}

#[test]
fn test_heatmap_map_two_runs_give_identical_bytes() {
    let dir = tempfile::tempdir().unwrap();
    create_map_repo(dir.path());

    let first = heatmap_stdout(dir.path(), MAP_ARGS);
    let second = heatmap_stdout(dir.path(), MAP_ARGS);
    assert!(first.contains("  function_item long:3\n"), "{first}");
    assert_eq!(first.as_bytes(), second.as_bytes());
}

#[test]
fn test_heatmap_map_empty_repository_exits_one_with_no_commits_message() {
    let dir = tempfile::tempdir().unwrap();
    git_init(dir.path());

    let output = common::skim()
        .arg("heatmap")
        .args(MAP_ARGS)
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_refused(&output, "No commits found in repository");
}

#[test]
fn test_heatmap_map_shallow_clone_ranks_by_size_within_budget() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    create_map_repo(&source);
    let output = StdCommand::new("git")
        .env("GIT_ALLOW_PROTOCOL", "file")
        .current_dir(dir.path())
        .args([
            "clone",
            "--depth",
            "1",
            &format!("file://{}", source.display()),
            "shallow",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    let text = heatmap_stdout(
        &dir.path().join("shallow"),
        &["--map", "--window", "all", "--tokens", "300"],
    );
    assert!(text.starts_with("Heatmap map: shallow "), "{text}");
    assert!(
        text.lines()
            .next()
            .unwrap()
            .ends_with(", 1 commit, all window"),
        "{text}"
    );
    let lines = top_level_lines(&text);
    assert!(
        lines[0].starts_with("warning: Shallow clone detected"),
        "{text}"
    );
    assert_eq!(
        lines[1..],
        [
            "src/z_long.rs: 1 commit, 5 lines, score 5",
            "src/a_short.rs: 1 commit, 1 line, score 1"
        ],
        "{text}"
    );
    assert!(map_token_count(&text) <= 300, "{text}");
}

#[test]
fn test_heatmap_map_refuses_conflicts_and_invalid_token_budgets() {
    let dir = tempfile::tempdir().unwrap();
    for (args, message) in [
        (&["--map", "--json"][..], "cannot combine --map with --json"),
        (
            &["--map", "--format", "json"],
            "cannot combine --map with --json",
        ),
        (
            &["--map", "--detail", "hotspots"],
            "cannot combine --map with --detail",
        ),
        (
            &["--map", "--insights"],
            "cannot combine --map with --insights",
        ),
        (
            &["--map", "--diff", "main"],
            "cannot combine --map with --diff",
        ),
        (
            &["--map", "src/a.rs"],
            "cannot combine --map with explicit file arguments",
        ),
        (&["--map", "--top", "20"], "cannot combine --map with --top"),
        (&["--map", "--tokens", "0"], "--tokens must be at least 1"),
        (
            &["--map", "--tokens", "10000001"],
            "--tokens must be at most 10000000",
        ),
        (&["--tokens", "300"], "--tokens requires --map"),
    ] {
        let output = common::skim()
            .arg("heatmap")
            .args(args)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert_refused(&output, message);
        assert_refused(&output, "Run `skim heatmap --help` for usage.");
    }
}

#[test]
fn test_heatmap_map_path_window_and_excludes_scope_rows() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git_init(root);
    git_commit(
        root,
        "src/old.rs",
        "fn old() {}\n",
        "feat: add old",
        MAP_COMMIT_TIME,
    );
    git_commit_files(
        root,
        &[
            ("src/keep.rs", "fn keep() {}\n"),
            ("src/skip.rs", "fn skip() {}\n"),
            ("src/Cargo.lock", "version = 4\n"),
            ("lib/other.rs", "fn other() {}\n"),
        ],
        "feat: add current files",
        MAP_COMMIT_TIME + 86_400,
    );

    let rows = |args: &[&str]| -> Vec<String> {
        top_level_lines(&heatmap_stdout(root, args))
            .iter()
            .filter_map(|line| line.split_once(": "))
            .filter(|(_, stats)| stats.starts_with("1 commit, "))
            .map(|(path, _)| path.to_owned())
            .collect()
    };
    assert_eq!(
        rows(MAP_ARGS),
        ["lib/other.rs", "src/keep.rs", "src/old.rs", "src/skip.rs"]
    );
    assert_eq!(
        rows(&["--map", "--window", "all", "--path", "src"]),
        ["src/keep.rs", "src/old.rs", "src/skip.rs"]
    );
    assert_eq!(
        rows(&["--map", "--since", "2023-11-15", "--path", "src"]),
        ["src/keep.rs", "src/skip.rs"]
    );
    assert_eq!(
        rows(&[
            "--map",
            "--since",
            "2023-11-15",
            "--path",
            "src",
            "--exclude",
            "**/skip.rs"
        ]),
        ["src/keep.rs"]
    );
    assert_eq!(
        rows(&[
            "--map",
            "--since",
            "2023-11-15",
            "--path",
            "src",
            "--no-exclude"
        ]),
        ["src/Cargo.lock", "src/keep.rs", "src/skip.rs"]
    );
}

#[test]
fn test_heatmap_map_keeps_head_symbols_after_an_uncommitted_edit() {
    let dir = tempfile::tempdir().unwrap();
    create_map_repo(dir.path());
    let before = heatmap_stdout(dir.path(), MAP_ARGS);
    std::fs::write(dir.path().join("src/z_long.rs"), "fn not_in_head() {}\n").unwrap();

    let after = heatmap_stdout(dir.path(), MAP_ARGS);
    assert!(after.contains("  function_item long:3\n"), "{after}");
    assert_eq!(after.as_bytes(), before.as_bytes());
}

#[test]
fn test_heatmap_map_shows_method_names_but_not_key_expressions() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let source = "class Report {\n[keyFor('title')]() {}\ntitle() {}\n}\n";
    git_init(root);
    git_commit_files(
        root,
        &[("report.js", source), ("report.ts", source)],
        "feat: add reports",
        MAP_COMMIT_TIME,
    );
    let text = heatmap_stdout(root, MAP_ARGS);
    for path in ["report.js", "report.ts"] {
        assert!(
            text.contains(&format!(
                "{path}: 1 commit, 4 lines, score 4\n  class_declaration Report:1\n  method_definition Report::title:3\n"
            )),
            "{text}"
        );
    }
    assert!(!text.contains("keyFor"), "{text}");
}

#[test]
fn test_heatmap_map_takes_stats_and_symbols_from_the_same_head_blob() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git_init(root);
    git_commit_files(
        root,
        &[
            ("a.rs", "fn original() {}\n"),
            ("b.rs", "\nfn other() {}\n"),
        ],
        "feat: add files",
        MAP_COMMIT_TIME,
    );
    let out = StdCommand::new("git")
        .args(["replace", "HEAD:a.rs", "HEAD:b.rs"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let text = heatmap_stdout(root, MAP_ARGS);
    assert!(
        text.contains("a.rs: 1 commit, 1 line, score 1\n  function_item original:1\n"),
        "{text}"
    );
}

#[cfg(unix)]
#[test]
fn test_heatmap_map_closed_stderr_keeps_full_stdout() {
    let dir = tempfile::tempdir().unwrap();
    create_map_repo(dir.path());
    let args = ["heatmap", "--map", "--window", "all", "--debug"];
    let baseline = common::skim()
        .args(args)
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        stdout_of(&baseline).contains("  function_item long:3\n"),
        "{baseline:?}"
    );
    assert!(String::from_utf8_lossy(&baseline.stderr).contains("[skim:heatmap] HEAD data:"));

    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let output = StdCommand::new(assert_cmd::cargo::cargo_bin!("skim"))
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .args(args)
        .current_dir(dir.path())
        .stderr(std::process::Stdio::from(writer))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, baseline.stdout);
}

#[test]
fn test_heatmap_help_describes_the_map_budget_and_symbol_rows() {
    common::skim()
        .args(["heatmap", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim heatmap --map [OPTIONS]"))
        .stdout(predicate::str::contains("--tokens <N>"))
        .stdout(predicate::str::contains(
            "default: 1000; range: 1 to 10000000",
        ))
        .stdout(predicate::str::contains("row caps below (not for --map)"))
        .stdout(predicate::str::contains("12 symbol rows per file"))
        .stdout(predicate::str::contains("kind name:line"))
        .stdout(predicate::str::contains("+N more"))
        .stdout(predicate::str::contains("top-level const names"))
        .stdout(predicate::str::contains(
            "JSON, YAML and TOML have no symbol rows",
        ));
}
