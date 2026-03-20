//! CLI subcommand disambiguation tests.
//!
//! Validates the pre-parse router correctly distinguishes file operations
//! from subcommands, maintaining 100% backward compatibility.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

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
fn test_known_subcommand_routes_to_stub() {
    // "init" is a known subcommand, no file named "init" exists
    Command::cargo_bin("skim")
        .unwrap()
        .arg("init")
        .assert()
        .failure()
        .stderr(predicate::str::contains("not yet implemented"));
}

#[test]
fn test_subcommand_with_args_routes_to_stub() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("test")
        .arg("cargo")
        .assert()
        .failure()
        .stderr(predicate::str::contains("not yet implemented"));
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
        .stdout(predicate::str::contains("not yet implemented"));
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

#[test]
fn test_unimplemented_subcommands_are_stubs() {
    // "completions" is intentionally excluded — it is implemented, not a stub.
    for subcmd in &["init", "test", "rewrite", "git", "build"] {
        Command::cargo_bin("skim")
            .unwrap()
            .arg(subcmd)
            .assert()
            .failure()
            .stderr(predicate::str::contains("not yet implemented"));
    }
}

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
fn test_bare_file_named_as_subcommand_routes_to_file_operation() {
    let dir = TempDir::new().unwrap();
    // Create a file called "init" (a known subcommand name) in the temp dir
    let file = dir.path().join("init");
    fs::write(&file, "fn setup() {}").unwrap();

    // Pass bare "init" with cwd set so path.exists() finds the file on disk.
    // This exercises the backward-compat precedence: on-disk file wins over subcommand.
    Command::cargo_bin("skim")
        .unwrap()
        .current_dir(dir.path())
        .arg("init")
        .arg("-l")
        .arg("rust")
        .assert()
        .success()
        .stdout(predicate::str::contains("fn setup"));
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
fn test_bare_dir_named_as_subcommand_routes_to_file_operation() {
    let dir = TempDir::new().unwrap();
    // Create a directory called "build" (a known subcommand name) with a source file inside
    let build_dir = dir.path().join("build");
    fs::create_dir(&build_dir).unwrap();
    let file = build_dir.join("main.rs");
    fs::write(&file, "fn main() {}").unwrap();

    // Pass bare "build" with cwd set so path.exists() finds the directory on disk.
    // This exercises the backward-compat precedence: on-disk directory wins over subcommand.
    Command::cargo_bin("skim")
        .unwrap()
        .current_dir(dir.path())
        .arg("build")
        .assert()
        .success();
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
