//! CLI subcommand disambiguation tests.
//!
//! Validates the pre-parse router correctly distinguishes file operations
//! from subcommands, maintaining 100% backward compatibility.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

const FIND_FIXTURE: &str = include_str!("fixtures/cmd/file/find_small.txt");
const GH_FIXTURE: &str = include_str!("fixtures/cmd/infra/gh_pr_list.json");
const GH_ISSUE_VIEW_FIXTURE: &str = include_str!("fixtures/cmd/infra/gh_issue_view.json");
const GH_PR_VIEW_FIXTURE: &str = include_str!("fixtures/cmd/infra/gh_pr_view.json");
// Symbol format used for piped stdin tests because strip_ansi (applied to
// stdin content before parsing) removes tab characters, breaking the
// tab-format fixture. Symbol format uses Unicode (✓/X/-) which survives
// strip_ansi. Tab format is covered by unit tests in pr_checks.rs.
const GH_PR_CHECKS_FIXTURE: &str = include_str!("fixtures/cmd/infra/gh_pr_checks_symbol.txt");
const GH_RUN_VIEW_FIXTURE: &str = include_str!("fixtures/cmd/infra/gh_run_view.json");
const LOG_FIXTURE: &str = include_str!("fixtures/cmd/log/plaintext_mixed.txt");

fn skim_cmd() -> Command {
    let mut cmd = Command::cargo_bin("skim").unwrap();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd
}

// ============================================================================
// File operation routing (backward compatibility)
// ============================================================================

#[test]
fn test_file_with_extension_routes_to_file_operation() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    fs::write(&file, "function add(a: number): number { return a; }").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file)
        .assert()
        .success()
        .stdout(predicate::str::contains("function add"));
}

#[test]
fn test_file_named_init_py_routes_to_file_operation() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("init.py");
    fs::write(&file, "def hello(): pass").unwrap();

    // "init" is a known subcommand, but "init.py" contains a dot
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file)
        .assert()
        .success()
        .stdout(predicate::str::contains("def hello"));
}

#[test]
fn test_path_with_separator_routes_to_file_operation() {
    let dir = TempDir::new().unwrap();
    let subdir = dir.path().join("src");
    fs::create_dir(&subdir).unwrap();
    let file = subdir.join("test.rs");
    fs::write(&file, "fn main() {}").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file)
        .arg("--mode=signatures")
        .assert()
        .success();
}

#[test]
fn test_stdin_dash_routes_to_file_operation() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg("-l")
        .arg("rust")
        .write_stdin("fn main() {}")
        .assert()
        .success()
        .stdout(predicate::str::contains("fn main"));
}

#[test]
fn test_glob_pattern_routes_to_file_operation() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("foo.ts");
    fs::write(&file, "const x = 1;").unwrap();

    // Glob chars → FileOperation (use relative pattern with current_dir)
    Command::cargo_bin("skim")
        .unwrap()
        .current_dir(dir.path())
        .arg("*.ts")
        .assert()
        .success();
}

#[test]
fn test_dot_routes_to_file_operation() {
    // "." is a directory — contains a dot → FileOperation
    Command::cargo_bin("skim")
        .unwrap()
        .arg(".")
        .assert()
        .success();
}

#[test]
fn test_no_positional_routes_to_file_operation() {
    // Flags only, no positional → FileOperation → clap handles --clear-cache
    Command::cargo_bin("skim")
        .unwrap()
        .arg("--clear-cache")
        .assert()
        .success();
}

#[test]
fn test_double_dash_before_subcommand_name_routes_to_file_operation() {
    // `skim -- test` should NOT route to subcommand
    // `--` means everything after is positional, so "test" is a file arg.
    // This will fail because no file named "test" exists, but the important
    // thing is it does NOT route to the subcommand stub.
    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg("--")
        .arg("test")
        .assert()
        .failure();

    // Should get a file error, not "not yet implemented"
    output.stderr(predicate::str::contains("not yet implemented").not());
}

// ============================================================================
// Subcommand routing
// ============================================================================

#[test]
fn test_known_subcommand_init_is_implemented() {
    // "init" is a known, implemented subcommand — help should work
    Command::cargo_bin("skim")
        .unwrap()
        .arg("init")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim init"));
}

#[test]
fn test_subcommand_init_with_unknown_flag_fails() {
    // "init" with an unknown flag should fail gracefully
    Command::cargo_bin("skim")
        .unwrap()
        .arg("init")
        .arg("--nonexistent-flag")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown flag"));
}

#[test]
fn test_subcommand_help_exits_zero() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("init")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim init"))
        .stdout(predicate::str::contains("Install skim"));
}

#[test]
fn test_subcommand_short_help_exits_zero() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("build")
        .arg("-h")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim build"));
}

// All known subcommands are now implemented — no stubs remaining.
// Previously tested init as a stub; now it's fully implemented (#44).

// ============================================================================
// File-named-as-subcommand precedence
// ============================================================================

#[test]
fn test_full_path_to_file_named_as_subcommand_uses_separator_heuristic() {
    let dir = TempDir::new().unwrap();
    // Create a file called "init" (no extension) in the temp dir
    let file = dir.path().join("init");
    fs::write(&file, "fn setup() {}").unwrap();

    // Full path contains "/" → routes via path-separator heuristic (never reaches path.exists())
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file)
        .arg("-l")
        .arg("rust")
        .assert()
        .success()
        .stdout(predicate::str::contains("fn setup"));
}

#[test]
fn test_bare_file_named_as_subcommand_routes_to_subcommand() {
    let dir = TempDir::new().unwrap();
    // Create a file called "init" (a known subcommand name) in the temp dir
    let file = dir.path().join("init");
    fs::write(&file, "fn setup() {}").unwrap();

    // After the router fix, bare "init" ALWAYS routes to the subcommand
    // regardless of whether a file with that name exists on disk.
    // To read such a file, users must use ./init or the full path.
    Command::cargo_bin("skim")
        .unwrap()
        .current_dir(dir.path())
        .arg("init")
        .arg("--help")
        .assert()
        .success()
        // Should show the subcommand help, not the file contents
        .stdout(predicate::str::contains("skim init"))
        .stdout(predicate::str::contains("fn setup").not());
}

#[test]
fn test_full_path_to_dir_named_as_subcommand_uses_separator_heuristic() {
    let dir = TempDir::new().unwrap();
    // Create a directory called "build" with a source file inside
    let build_dir = dir.path().join("build");
    fs::create_dir(&build_dir).unwrap();
    let file = build_dir.join("main.rs");
    fs::write(&file, "fn main() {}").unwrap();

    // Full path contains "/" → routes via path-separator heuristic (never reaches path.exists())
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&build_dir)
        .assert()
        .success();
}

#[test]
fn test_bare_dir_named_as_subcommand_routes_to_subcommand() {
    let dir = TempDir::new().unwrap();
    // Create a directory called "build" (a known subcommand name) with a source file inside
    let build_dir = dir.path().join("build");
    fs::create_dir(&build_dir).unwrap();
    let file = build_dir.join("main.rs");
    fs::write(&file, "fn main() {}").unwrap();

    // After the router fix, bare "build" ALWAYS routes to the subcommand
    // regardless of whether a directory with that name exists on disk.
    // To process such a directory, users must use ./build or the full path.
    Command::cargo_bin("skim")
        .unwrap()
        .current_dir(dir.path())
        .arg("build")
        .arg("--help")
        .assert()
        .success()
        // Should show the subcommand help, not process the directory
        .stdout(predicate::str::contains("skim build"));
}

// ============================================================================
// Flag-with-value parsing (ensure flags don't consume subcommand names)
// ============================================================================

#[test]
fn test_mode_flag_consumes_next_token() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    fs::write(&file, "function f(): void { return; }").unwrap();

    // `--mode signatures` — "signatures" is consumed by --mode, not treated
    // as a positional.
    Command::cargo_bin("skim")
        .unwrap()
        .arg("--mode")
        .arg("signatures")
        .arg(&file)
        .assert()
        .success();
}

#[test]
fn test_mode_equals_syntax_is_single_token() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    fs::write(&file, "function f(): void { return; }").unwrap();

    // `--mode=signatures` is one token — the router sees no positional
    // before the file path.
    Command::cargo_bin("skim")
        .unwrap()
        .arg("--mode=signatures")
        .arg(&file)
        .assert()
        .success();
}

// ============================================================================
// Help text includes subcommands
// ============================================================================

#[test]
fn test_help_lists_subcommands() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("SUBCOMMANDS"))
        .stdout(predicate::str::contains("init"))
        .stdout(predicate::str::contains("test"))
        .stdout(predicate::str::contains("build"))
        .stdout(predicate::str::contains("completions"));
}

// ============================================================================
// Unknown words fall through to FileOperation
// ============================================================================

#[test]
fn test_unknown_word_routes_to_file_operation() {
    // "foobar" is not a known subcommand — routes to FileOperation.
    // Clap/file-processing will produce an error since the file doesn't exist.
    Command::cargo_bin("skim")
        .unwrap()
        .arg("foobar")
        .assert()
        .failure()
        .stderr(predicate::str::contains("not yet implemented").not());
}

// ============================================================================
// Subcommand help
// ============================================================================

#[test]
fn test_subcommand_file_help() {
    skim_cmd()
        .args(["file", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("find"));
}

#[test]
fn test_subcommand_infra_help() {
    skim_cmd()
        .args(["infra", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("gh"));
}

#[test]
fn test_subcommand_log_help() {
    skim_cmd()
        .args(["log", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("dedup"));
}

// ============================================================================
// Error/edge paths
// ============================================================================

#[test]
fn test_subcommand_file_unknown_tool() {
    skim_cmd()
        .args(["file", "unknown-tool-xyz"])
        .assert()
        .failure();
}

// log uses its own stdin path (not run_parsed_command_with_mode), so empty
// stdin is intentionally treated as success — unlike file/test subcommands
// which fall through to spawn.
#[test]
fn test_subcommand_log_empty_stdin() {
    skim_cmd().arg("log").write_stdin("").assert().success();
}

#[test]
fn test_log_conflicting_flags() {
    skim_cmd()
        .args(["log", "--debug-only", "--keep-debug"])
        .write_stdin(LOG_FIXTURE)
        .assert()
        .success();
}

/// Empty piped stdin now falls through to spawning find (which exits 1 on macOS
/// with no path arg). The old test expected success because the old code
/// returned exit_code=Some(0) for an empty CommandOutput; the new empty-stdin
/// fallback correctly delegates to the real command and propagates its exit code.
#[test]
fn test_file_find_empty_stdin_falls_through_to_spawn() {
    // assert_cmd supplies a non-terminal pipe with no content — exactly the
    // scenario the empty-stdin fallback is designed for.  find(1) exits 1 on
    // macOS when invoked with no path argument, so we assert failure here.
    skim_cmd()
        .args(["file", "find"])
        .write_stdin("")
        .assert()
        .failure();
}

// ============================================================================
// JSON output
// ============================================================================

#[test]
fn test_subcommand_file_json() {
    let output = skim_cmd()
        .args(["file", "find", "--json"])
        .write_stdin(FIND_FIXTURE)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(json.get("tool").is_some(), "JSON should have 'tool' field");
}

#[test]
fn test_subcommand_infra_json() {
    let output = skim_cmd()
        .args(["infra", "gh", "--json"])
        .write_stdin(GH_FIXTURE)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(json.get("tool").is_some(), "JSON should have 'tool' field");
}

#[test]
fn test_subcommand_log_json() {
    let output = skim_cmd()
        .args(["log", "--json"])
        .write_stdin(LOG_FIXTURE)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    // total_lines is nested under the "result" key
    let has_total_lines = json
        .get("result")
        .and_then(|r| r.get("total_lines"))
        .is_some();
    assert!(
        has_total_lines,
        "JSON should have 'result.total_lines' field"
    );
}

// ============================================================================
// --show-stats token output
// ============================================================================

#[test]
fn test_subcommand_file_show_stats() {
    let output = skim_cmd()
        .args(["file", "find", "--show-stats"])
        .write_stdin(FIND_FIXTURE)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.to_lowercase().contains("token"),
        "stderr should contain token stats, got: {stderr}"
    );
}

#[test]
fn test_subcommand_infra_show_stats() {
    let output = skim_cmd()
        .args(["infra", "gh", "--show-stats"])
        .write_stdin(GH_FIXTURE)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.to_lowercase().contains("token"),
        "stderr should contain token stats, got: {stderr}"
    );
}

#[test]
fn test_subcommand_log_show_stats() {
    let output = skim_cmd()
        .args(["log", "--show-stats"])
        .write_stdin(LOG_FIXTURE)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.to_lowercase().contains("token"),
        "stderr should contain token stats, got: {stderr}"
    );
}

// ============================================================================
// gh view/checks integration tests (#131)
// ============================================================================

#[test]
fn test_subcommand_infra_gh_issue_view() {
    // Pipe issue JSON fixture via stdin — auto-detect should produce compressed output
    let output = skim_cmd()
        .args(["infra", "gh"])
        .write_stdin(GH_ISSUE_VIEW_FIXTURE)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("issue view"),
        "Expected 'issue view' in output, got: {stdout}"
    );
    assert!(
        stdout.contains("#42"),
        "Expected issue number in output, got: {stdout}"
    );
}

#[test]
fn test_subcommand_infra_gh_pr_view() {
    let output = skim_cmd()
        .args(["infra", "gh"])
        .write_stdin(GH_PR_VIEW_FIXTURE)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("pr view"),
        "Expected 'pr view' in output, got: {stdout}"
    );
    assert!(
        stdout.contains("#15"),
        "Expected PR number in output, got: {stdout}"
    );
}

#[test]
fn test_subcommand_infra_gh_pr_checks() {
    let output = skim_cmd()
        .args(["infra", "gh"])
        .write_stdin(GH_PR_CHECKS_FIXTURE)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("checks") || stdout.contains("check"),
        "Expected check summary in output, got: {stdout}"
    );
}

#[test]
fn test_subcommand_infra_gh_run_view() {
    let output = skim_cmd()
        .args(["infra", "gh"])
        .write_stdin(GH_RUN_VIEW_FIXTURE)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("run view"),
        "Expected 'run view' in output, got: {stdout}"
    );
    assert!(
        stdout.contains("#12345"),
        "Expected run ID in output, got: {stdout}"
    );
}

#[test]
fn test_subcommand_infra_gh_issue_view_json_output() {
    let output = skim_cmd()
        .args(["infra", "gh", "--json"])
        .write_stdin(GH_ISSUE_VIEW_FIXTURE)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(
        json.get("tool").is_some(),
        "JSON output should have 'tool' field"
    );
    assert_eq!(
        json.get("tool").and_then(|v| v.as_str()),
        Some("gh"),
        "tool field should be 'gh'"
    );
}

#[test]
fn test_subcommand_infra_gh_run_view_json_output() {
    let output = skim_cmd()
        .args(["infra", "gh", "--json"])
        .write_stdin(GH_RUN_VIEW_FIXTURE)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(
        json.get("tool").is_some(),
        "JSON output should have 'tool' field"
    );
}

#[test]
fn test_subcommand_infra_gh_existing_list_unchanged() {
    // Regression guard: existing list fixture must still work correctly
    let output = skim_cmd()
        .args(["infra", "gh"])
        .write_stdin(GH_FIXTURE)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("gh list"),
        "Regression: existing list fixture should produce 'gh list', got: {stdout}"
    );
}

// ============================================================================
// Gap fixes: piped stdin for `gh run watch` and `gh api` (v2.5.1)
// ============================================================================

#[test]
fn test_subcommand_infra_gh_run_watch_pipe_exits_clean() {
    // Mirrors the Tester's scenario:
    //   printf "workflow step 1\nworkflow step 2\ncompleted\n" | skim infra gh run watch
    //
    // None of the lines match job-status patterns, so no output is produced
    // but the process must exit 0 (clean finalize on empty state).
    let output = skim_cmd()
        .args(["infra", "gh", "run", "watch"])
        .write_stdin("workflow step 1\nworkflow step 2\ncompleted\n")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "gh run watch pipe mode must exit 0, got: {:?}",
        output.status
    );
}

#[test]
fn test_subcommand_infra_gh_run_watch_pipe_with_job_lines() {
    // Validates that actual job-status lines piped to `skim infra gh run watch`
    // produce compressed output (summaries) and exit 0.
    let input = "  * build In progress\n  ✓ build Completed\n  X test Failed\n";
    let output = skim_cmd()
        .args(["infra", "gh", "run", "watch"])
        .write_stdin(input)
        .output()
        .unwrap();
    assert!(output.status.success(), "exit status: {:?}", output.status);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains('✓') || stdout.contains("FAILED"),
        "expected job output, got: {stdout}"
    );
}

#[test]
fn test_subcommand_infra_gh_api_pipe_json_object() {
    // Mirrors the Tester's scenario:
    //   echo '{"login": "foo", "id": 42}' | skim infra gh api
    //
    // Must parse the JSON object and exit 0 (previously: exit 1 with error).
    let json = r#"{"login": "foo", "id": 42}"#;
    let output = skim_cmd()
        .args(["infra", "gh", "api"])
        .write_stdin(json)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "gh api pipe mode must exit 0, got: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("login") || stdout.contains("id"),
        "expected parsed output, got: {stdout}"
    );
}

#[test]
fn test_subcommand_infra_gh_api_pipe_json_array() {
    // Validates array input piped to `skim infra gh api` is parsed and
    // emits compressed output rather than an error.
    let json = r#"[{"id": 1, "name": "repo-a"}, {"id": 2, "name": "repo-b"}]"#;
    let output = skim_cmd()
        .args(["infra", "gh", "api"])
        .write_stdin(json)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "gh api array pipe mode must exit 0, got: {:?}",
        output.status
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("array") || stdout.contains("repo"),
        "expected parsed array output, got: {stdout}"
    );
}
