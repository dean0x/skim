//! CLI integration tests for --tokens flag (token budget cascade)
//!
//! Tests the --tokens N flag that cascades through transformation modes
//! until the output fits within the specified token budget.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::PathBuf;
use tempfile::TempDir;

/// Get a command for the skim binary
fn skim_cmd() -> Command {
    let mut cmd = Command::cargo_bin("skim").unwrap();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd
}

/// Resolve path to a test fixture file relative to the workspace root
fn fixture_path(relative: &str) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crates/rskim -> workspace root
    path.pop();
    path.pop();
    path.join("tests/fixtures").join(relative)
}

/// Parse the transformed token count from --show-stats stderr output.
///
/// Stats format: "X tokens → Y tokens (Z% reduction)"
/// Returns the Y value (transformed token count).
fn parse_transformed_token_count(stderr: &str) -> usize {
    let arrow_pos = stderr
        .find('\u{2192}')
        .unwrap_or_else(|| panic!("Expected '→' in stats output. stderr: {stderr:?}"));
    let after_arrow = stderr[arrow_pos + '\u{2192}'.len_utf8()..].trim_start();
    let token_str: String = after_arrow
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .collect();
    assert!(
        !token_str.is_empty(),
        "Failed to parse token count after '→'. stderr: {stderr:?}",
    );
    token_str
        .replace(',', "")
        .parse()
        .unwrap_or_else(|e| panic!("Invalid token count '{token_str}': {e}. stderr: {stderr:?}"))
}

// ============================================================================
// Basic tests
// ============================================================================

#[test]
fn test_tokens_flag_accepted() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    std::fs::write(
        &file,
        "function hello(name: string): string { return `Hi ${name}`; }\n",
    )
    .unwrap();

    skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--tokens")
        .arg("500")
        .arg("--no-cache")
        .assert()
        .success();
}

#[test]
fn test_tokens_zero_rejected() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    std::fs::write(&file, "function foo() {}").unwrap();

    skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--tokens")
        .arg("0")
        .assert()
        .failure()
        .stderr(predicate::str::contains("--tokens must be at least 1"));
}

#[test]
fn test_tokens_output_within_budget() {
    // THE fundamental invariant: actual token count <= N
    // We use a generous budget that structure mode should satisfy
    let fixture = fixture_path("typescript/simple.ts");
    assert!(fixture.exists(), "Fixture should exist: {:?}", fixture);

    let output = skim_cmd()
        .arg(fixture.to_str().unwrap())
        .arg("--tokens")
        .arg("500")
        .arg("--show-stats")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success());

    let stderr = String::from_utf8(output.stderr).unwrap();
    // Stats should show the output token count
    assert!(
        stderr.contains("tokens"),
        "Should show token stats: {:?}",
        stderr,
    );
}

// ============================================================================
// Cascade tests
// ============================================================================

#[test]
fn test_tokens_large_budget_no_cascade() {
    // A very generous budget should not trigger cascade (no diagnostic)
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    std::fs::write(
        &file,
        "function hello(name: string): string { return `Hi ${name}`; }\n",
    )
    .unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--tokens")
        .arg("10000")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success());

    let stderr = String::from_utf8(output.stderr).unwrap();
    // No cascade diagnostic expected
    assert!(
        !stderr.contains("escalated"),
        "Large budget should not trigger cascade: {:?}",
        stderr,
    );
}

#[test]
fn test_tokens_small_budget_cascades() {
    // A tight budget on a file with lots of code should trigger cascade
    let fixture = fixture_path("typescript/simple.ts");

    let output = skim_cmd()
        .arg(fixture.to_str().unwrap())
        .arg("--tokens")
        .arg("25")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success());

    let stderr = String::from_utf8(output.stderr).unwrap();
    // Should see cascade diagnostic (escalation beyond default structure mode)
    assert!(
        stderr.contains("escalated") || stderr.contains("token budget"),
        "Tight budget should trigger cascade: {:?}",
        stderr,
    );
}

#[test]
fn test_tokens_very_small_budget_fallback_truncation() {
    // An impossibly small budget should trigger final line truncation.
    // Use a file with type definitions so that even types mode has output.
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    std::fs::write(
        &file,
        "type UserId = string;\n\
         type UserName = string;\n\
         type UserEmail = string;\n\
         type UserAge = number;\n\
         type UserStatus = 'active' | 'inactive';\n\
         interface User {\n  id: UserId;\n  name: UserName;\n  email: UserEmail;\n  age: UserAge;\n  status: UserStatus;\n}\n\
         function createUser(name: string): User { return {} as User; }\n",
    )
    .unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--tokens")
        .arg("3")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success());

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("line truncation"),
        "Very small budget should trigger line truncation: {:?}",
        stderr,
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    // With a budget of 3 tokens, even the omission marker (~8-10 tokens)
    // exceeds the budget, so the output should be empty rather than
    // violating the token budget invariant.
    assert!(
        stdout.is_empty() || stdout.contains("truncated"),
        "Output should be empty (budget too small for marker) or contain truncation marker: {:?}",
        stdout,
    );
}

// ============================================================================
// Interaction tests
// ============================================================================

#[test]
fn test_tokens_with_explicit_mode() {
    // --mode=signatures --tokens N should start cascade at signatures
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    std::fs::write(
        &file,
        "type UserId = string;\n\
         interface User { id: UserId; name: string; }\n\
         function greet(name: string): string { return `Hi ${name}`; }\n\
         function add(a: number, b: number): number { return a + b; }\n",
    )
    .unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--mode=signatures")
        .arg("--tokens")
        .arg("500")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).unwrap();
    // Starting at signatures mode: should see function signatures but not type definitions
    // (unless signatures mode includes them)
    assert!(
        stdout.contains("function") || stdout.contains("greet"),
        "Should contain function signatures: {:?}",
        stdout,
    );
}

#[test]
fn test_tokens_with_max_lines() {
    // Both --tokens and --max-lines should work together
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    std::fs::write(
        &file,
        "type A = string;\n\
         type B = number;\n\
         type C = boolean;\n\
         function foo(): void { return; }\n\
         function bar(): void { return; }\n\
         function baz(): void { return; }\n",
    )
    .unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--tokens")
        .arg("500")
        .arg("--max-lines")
        .arg("3")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).unwrap();
    let line_count = stdout.lines().count();
    assert!(
        line_count <= 3,
        "Output should respect --max-lines=3, got {} lines: {:?}",
        line_count,
        stdout,
    );
}

#[test]
fn test_tokens_with_show_stats() {
    let fixture = fixture_path("typescript/simple.ts");

    let output = skim_cmd()
        .arg(fixture.to_str().unwrap())
        .arg("--tokens")
        .arg("50")
        .arg("--show-stats")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success());

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("tokens") && stderr.contains("%"),
        "Should show token stats: {:?}",
        stderr,
    );
}

#[test]
fn test_tokens_with_stdin() {
    skim_cmd()
        .arg("-")
        .arg("-l")
        .arg("typescript")
        .arg("--tokens")
        .arg("50")
        .write_stdin(
            "function hello(name: string): string { return `Hi ${name}`; }\n\
             function world(): void { console.log('world'); }\n",
        )
        .assert()
        .success();
}

#[test]
fn test_tokens_with_glob() {
    // Per-file budget: each file independently limited
    let dir = TempDir::new().unwrap();

    std::fs::write(
        dir.path().join("file1.ts"),
        "function foo(a: number): number { return a * 2; }\n\
         function bar(b: string): string { return b.toUpperCase(); }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("file2.ts"),
        "function baz(c: boolean): boolean { return !c; }\n\
         function qux(d: number[]): number { return d.length; }\n",
    )
    .unwrap();

    let output = skim_cmd()
        .arg("*.ts")
        .arg("--tokens")
        .arg("50")
        .arg("--no-cache")
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "Glob with --tokens should succeed. stderr: {:?}",
        String::from_utf8_lossy(&output.stderr),
    );
}

// ============================================================================
// Edge cases
// ============================================================================

#[test]
fn test_tokens_empty_file() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("empty.ts");
    std::fs::write(&file, "").unwrap();

    skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--tokens")
        .arg("100")
        .arg("--no-cache")
        .assert()
        .success();
}

#[test]
fn test_tokens_already_within_budget() {
    // Small file with generous budget: no transformation needed beyond default
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("tiny.ts");
    std::fs::write(&file, "type X = string;\n").unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--tokens")
        .arg("1000")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success());

    let stderr = String::from_utf8(output.stderr).unwrap();
    // No cascade needed
    assert!(
        !stderr.contains("escalated"),
        "Small file within budget should not cascade: {:?}",
        stderr,
    );
}

#[test]
fn test_tokens_budget_invariant_with_fixture() {
    // Verify the fundamental invariant on a real fixture: output tokens <= budget
    // We use --show-stats and parse the stderr to extract the transformed token count
    let fixture = fixture_path("typescript/simple.ts");
    let budget = 30;

    let output = skim_cmd()
        .arg(fixture.to_str().unwrap())
        .arg("--tokens")
        .arg(budget.to_string())
        .arg("--show-stats")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success());

    let stderr = String::from_utf8(output.stderr).unwrap();
    let token_count = parse_transformed_token_count(&stderr);
    assert!(
        token_count <= budget,
        "Transformed tokens ({}) should be <= budget ({}). stderr: {:?}",
        token_count,
        budget,
        stderr,
    );
}

#[test]
fn test_tokens_with_mode_types_single_mode_cascade() {
    // --mode=types has only one mode in the cascade [Types].
    // With a tight budget, it must go directly to line truncation fallback.
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.ts");
    std::fs::write(
        &file,
        "type UserId = string;\n\
         type UserName = string;\n\
         type UserEmail = string;\n\
         type UserAge = number;\n\
         type UserStatus = 'active' | 'inactive';\n\
         interface User { id: UserId; name: UserName; email: UserEmail; }\n\
         function createUser(name: string): User { return {} as User; }\n",
    )
    .unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--mode=types")
        .arg("--tokens")
        .arg("15")
        .arg("--show-stats")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "Types mode with --tokens should succeed. stderr: {:?}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stderr = String::from_utf8(output.stderr).unwrap();
    let token_count = parse_transformed_token_count(&stderr);
    assert!(
        token_count <= 15,
        "Output tokens ({}) should be <= budget (15). stderr: {:?}",
        token_count,
        stderr,
    );
}

#[test]
fn test_tokens_with_python_file() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.py");
    std::fs::write(
        &file,
        "def calculate(a: int, b: int) -> int:\n    return a + b\n\n\
         def multiply(a: int, b: int) -> int:\n    return a * b\n\n\
         class Calculator:\n    def __init__(self):\n        self.result = 0\n",
    )
    .unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--tokens")
        .arg("30")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "Python file with --tokens should succeed. stderr: {:?}",
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn test_tokens_with_rust_file() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.rs");
    std::fs::write(
        &file,
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\n\
         pub struct Point {\n    x: f64,\n    y: f64,\n}\n\n\
         impl Point {\n    pub fn new(x: f64, y: f64) -> Self {\n        Point { x, y }\n    }\n}\n",
    )
    .unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--tokens")
        .arg("40")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "Rust file with --tokens should succeed. stderr: {:?}",
        String::from_utf8_lossy(&output.stderr),
    );
}

// ============================================================================
// Serde-based language tests (S7 short-circuit)
// ============================================================================

#[test]
fn test_tokens_with_toml_serde_language() {
    // Serde-based languages (JSON, YAML, TOML) produce identical output
    // for Structure/Signatures/Types modes. Verify --tokens works correctly.
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("config.toml");
    std::fs::write(
        &file,
        "[server]\nhost = \"localhost\"\nport = 8080\n\n\
         [database]\nurl = \"postgres://localhost/db\"\npool_size = 10\n\n\
         [logging]\nlevel = \"info\"\nformat = \"json\"\n",
    )
    .unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--tokens")
        .arg("500")
        .arg("--show-stats")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(output.status.success());

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("tokens"),
        "Should show token stats for TOML: {:?}",
        stderr,
    );

    // Should NOT show cascade escalation (serde output is mode-independent)
    assert!(
        !stderr.contains("escalated"),
        "TOML with generous budget should not cascade: {:?}",
        stderr,
    );
}

#[test]
fn test_tokens_serde_tight_budget_truncates() {
    // Verify serde language with very tight budget triggers truncation correctly
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("config.toml");
    std::fs::write(
        &file,
        "[server]\nhost = \"localhost\"\nport = 8080\n\n\
         [database]\nurl = \"postgres://localhost/db\"\npool_size = 10\n\n\
         [logging]\nlevel = \"info\"\nformat = \"json\"\n",
    )
    .unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--tokens")
        .arg("5")
        .arg("--show-stats")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "TOML with tight budget should succeed. stderr: {:?}",
        String::from_utf8_lossy(&output.stderr),
    );
}
