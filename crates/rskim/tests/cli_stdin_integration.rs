//! Integration tests for stdin and single-file code paths after
//! the `process_stdin()` / `write_result_and_stats()` refactor.
//!
//! These cover multi-flag combinations that weren't exercised by the
//! existing test suite, plus F6/F13-stdin analytics tests from Phase A1 (#359).

use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;
mod common;

// ============================================================================
// Analytics helpers (reused from cli_analytics_expansion)
// ============================================================================

/// Open the analytics DB and count rows in token_savings.
fn count_rows(db_path: &std::path::Path) -> i64 {
    let conn = rusqlite::Connection::open(db_path).expect("must open analytics DB");
    conn.query_row("SELECT COUNT(*) FROM token_savings", [], |r| r.get(0))
        .unwrap_or(0)
}

/// Query a single column from the first token_savings row.
fn row_value<T: rusqlite::types::FromSql>(db_path: &std::path::Path, col: &str) -> T {
    let conn = rusqlite::Connection::open(db_path).expect("must open DB");
    conn.query_row(
        &format!("SELECT {col} FROM token_savings LIMIT 1"),
        [],
        |r| r.get(0),
    )
    .unwrap_or_else(|e| panic!("query {col}: {e}"))
}

// ============================================================================
// Stdin combination tests (exercises process_stdin)
// ============================================================================

#[test]
fn test_stdin_mode_and_stats_combined() {
    let input = "fn add(a: i32, b: i32) -> i32 { a + b }\nfn sub(x: i32, y: i32) -> i32 { x - y }";

    common::skim()
        .args(["-", "--lang=rust", "--mode=signatures", "--show-stats"])
        .write_stdin(input)
        .assert()
        .success()
        .stdout(predicate::str::contains("fn add(a: i32, b: i32) -> i32"))
        .stdout(predicate::str::contains("fn sub(x: i32, y: i32) -> i32"))
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("tokens"));
}

#[test]
fn test_stdin_tokens_and_stats_combined() {
    let input = "fn add(a: i32, b: i32) -> i32 { a + b }";

    common::skim()
        .args(["-", "--lang=rust", "--tokens=50", "--show-stats"])
        .write_stdin(input)
        .assert()
        .success()
        .stdout(predicate::str::contains("fn add"))
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("tokens"));
}

#[test]
fn test_stdin_filename_tokens_mode() {
    let input = "def greet(name):\n    return f'Hello {name}'";

    common::skim()
        .args([
            "-",
            "--filename=app.py",
            "--tokens=100",
            "--mode=signatures",
        ])
        .write_stdin(input)
        .assert()
        .success()
        .stdout(predicate::str::contains("def greet"));
}

#[test]
fn test_stdin_empty_input() {
    common::skim()
        .args(["-", "--lang=typescript"])
        .write_stdin("")
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn test_stdin_incomplete_code() {
    // tree-sitter should handle incomplete code gracefully
    common::skim()
        .args(["-", "--lang=typescript"])
        .write_stdin("function incomplete() {")
        .assert()
        .success()
        .stdout(predicate::str::contains("function incomplete"));
}

#[test]
fn test_stdin_binary_input_fails() {
    common::skim()
        .args(["-", "--lang=typescript"])
        .write_stdin(b"\x80\x81\x82\x00\xff" as &[u8])
        .assert()
        .failure()
        .stderr(predicate::str::contains("UTF-8").or(predicate::str::contains("utf-8")));
}

#[test]
fn test_stdin_max_lines_and_stats() {
    // Use functions with substantial bodies so the B5 elision-hint overhead
    // (≈10 tokens appended to every truncation marker) is absorbed by genuine
    // body savings. Single-expression bodies like `{ 1 }` compress to roughly
    // the same token count as the marker, causing the guardrail to fire and emit
    // raw (bypassing --max-lines, so "truncated" never appears in stdout).
    let input = concat!(
        "fn a() { let x = long_function_name_one(1, 2, 3); let y = another_function(x); x + y }\n",
        "fn b() { let x = long_function_name_one(4, 5, 6); let y = another_function(x); x + y }\n",
        "fn c() { let x = long_function_name_one(7, 8, 9); let y = another_function(x); x + y }",
    );

    common::skim()
        .args(["-", "--lang=rust", "--max-lines=2", "--show-stats"])
        .write_stdin(input)
        .assert()
        .success()
        .stdout(predicate::str::contains("fn a"))
        .stdout(predicate::str::contains("truncated"))
        .stderr(predicate::str::contains("[skim]"));
}

// ============================================================================
// F4/AC-F4.4: stdin degrade tests (ADR-002 — shebang-less stdin passes through)
// ============================================================================

/// Batch: 3 extension-less files with `#!/bin/bash` shebangs → all render, exit 0.
///
/// AC-F4.4: unknown-ext files with a shebang must be detected as the shebang
/// language, not fail with "All N file(s) failed".
#[test]
fn test_f4_bash_shebang_batch_exit_0_all_render() {
    let dir = TempDir::new().unwrap();

    // Create 3 extension-less bash scripts.
    let script_content = "#!/bin/bash\nfunction setup() {\n  echo 'setup'\n}\nsetup";
    for name in ["deploy", "build", "test"] {
        let path = dir.path().join(name);
        fs::write(&path, script_content).unwrap();
    }

    let deploy = dir.path().join("deploy");
    let build = dir.path().join("build");
    let test = dir.path().join("test");

    let assert = common::skim()
        .arg(&deploy)
        .arg(&build)
        .arg(&test)
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    // All three must contribute content — none must fail silently.
    assert!(
        stdout.contains("setup"),
        "bash shebang scripts must render: got stdout={stdout}"
    );
    assert!(
        !stdout.contains("All 3 file(s) failed"),
        "must not report all-failed: {stdout}"
    );
}

/// Unknown-ext, no-shebang stdin degrades to byte-identical passthrough (exit 0).
///
/// ADR-002: shebang-less stdin without --language passes through verbatim.
/// SKIM_DEBUG=1 must emit a notice on stderr.
#[test]
fn test_f4_stdin_no_shebang_passthrough_exit_0() {
    let content = "some unknown content here\nline two\n";

    // Without SKIM_DEBUG — just exit 0 and byte-identical stdout.
    common::skim()
        .arg("-")
        .write_stdin(content)
        .assert()
        .success()
        .stdout(predicate::str::contains("some unknown content here"));
}

/// With SKIM_DEBUG=1, unknown-lang stdin must emit a stderr notice.
#[test]
fn test_f4_stdin_no_shebang_debug_notice() {
    let content = "unknown content without shebang\n";

    common::skim()
        .arg("-")
        .env("SKIM_DEBUG", "1")
        .write_stdin(content)
        .assert()
        .success()
        .stdout(predicate::str::contains("unknown content without shebang"))
        .stderr(predicate::str::contains("[skim] notice").and(
            predicate::str::contains("unknown language").or(predicate::str::contains("degraded")),
        ));
}

// ============================================================================
// Single-file combination tests (exercises write_result_and_stats)
// ============================================================================

#[test]
fn test_single_file_tokens_and_stats() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("example.rs");
    fs::write(
        &file,
        "fn add(a: i32, b: i32) -> i32 { a + b }\nfn sub(a: i32, b: i32) -> i32 { a - b }",
    )
    .unwrap();

    common::skim()
        .args(["--tokens=100", "--show-stats"])
        .arg(&file)
        .assert()
        .success()
        .stdout(predicate::str::contains("fn add").or(predicate::str::contains("fn sub")))
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("tokens"));
}

// ============================================================================
// F6 / F13-stdin: Phase A1 (#359) — stdin analytics
// ============================================================================

/// F6: stdin pipe → analytics records exactly 1 row.
///
/// `printf '...' | skim - --language=ts` must record 1 row in the analytics DB.
/// This validates that the retained stdin buffer travels to the background
/// tokenization thread and is not dropped.
#[test]
fn test_f6_stdin_records_one_row() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("analytics.db");

    let input = "function hello(name) { return `Hello ${name}`; }\nfunction bye() {}";

    std::process::Command::new(common::skim_bin())
        .args(["-", "--language=typescript"])
        .env("SKIM_ANALYTICS_DB", &db_path)
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DISABLE_ANALYTICS")
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(stdin) = child.stdin.take() {
                let mut w = stdin;
                let _ = w.write_all(input.as_bytes());
            }
            child.wait()
        })
        .expect("stdin run must succeed");

    let count = count_rows(&db_path);
    assert_eq!(
        count, 1,
        "F6: stdin pipe must record exactly 1 analytics row"
    );
}

/// F6b: stdin with --filename hint → language correctly detected from filename.
#[test]
fn test_f6_stdin_with_filename_hint_detects_language() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("analytics.db");

    let input = "def greet(name):\n    return f'Hello {name}'\n";

    std::process::Command::new(common::skim_bin())
        .args(["-", "--filename=app.py"])
        .env("SKIM_ANALYTICS_DB", &db_path)
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DISABLE_ANALYTICS")
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(stdin) = child.stdin.take() {
                let mut w = stdin;
                let _ = w.write_all(input.as_bytes());
            }
            child.wait()
        })
        .expect("stdin run must succeed");

    let count = count_rows(&db_path);
    assert_eq!(count, 1, "F6b: stdin with --filename must record 1 row");

    let lang: Option<String> = row_value(&db_path, "language");
    assert_eq!(
        lang.as_deref(),
        Some("python"),
        "F6b: language must be 'python' from --filename=app.py"
    );
}

/// F13-stdin: SKIM_DISABLE_ANALYTICS=1 → 0 rows for stdin.
#[test]
fn test_f13_disable_analytics_no_rows_for_stdin() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("analytics.db");

    let input = "function hello() {}";

    // common::skim() sets SKIM_DISABLE_ANALYTICS=1
    common::skim()
        .args(["-", "--language=typescript"])
        .env("SKIM_ANALYTICS_DB", &db_path)
        .env_remove("SKIM_PASSTHROUGH")
        .write_stdin(input)
        .assert()
        .success();

    let count = count_rows(&db_path);
    assert_eq!(
        count, 0,
        "F13-stdin: SKIM_DISABLE_ANALYTICS=1 must record 0 rows for stdin"
    );
}
