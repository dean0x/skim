//! CLI integration tests using assert_cmd
//!
//! Tests the full CLI binary with real command-line arguments.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::time::Instant;
use tempfile::TempDir;

// ============================================================================
// Basic CLI Tests
// ============================================================================

#[test]
fn test_cli_version() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim"))
        .stdout(predicate::str::is_match(r"\d+\.\d+\.\d+").unwrap());
}

#[test]
fn test_cli_help() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim"))
        .stdout(predicate::str::contains("--mode"))
        .stdout(predicate::str::contains("--language"));
}

// ============================================================================
// File Processing Tests
// ============================================================================

#[test]
fn test_cli_structure_mode() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(
        &file_path,
        "function add(a: number, b: number): number { return a + b; }",
    )
    .unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--mode")
        .arg("structure")
        .assert()
        .success()
        .stdout(predicate::str::contains("function add"))
        .stdout(predicate::str::contains("{ /* ... */ }"))
        .stdout(predicate::str::contains("return a + b").not());
}

#[test]
fn test_cli_signatures_mode() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(
        &file_path,
        "function add(a: number, b: number): number { return a + b; }",
    )
    .unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--mode")
        .arg("signatures")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "function add(a: number, b: number): number",
        ))
        .stdout(predicate::str::contains("return").not());
}

#[test]
fn test_cli_types_mode() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(
        &file_path,
        "type UserId = string;\nfunction foo() { return 42; }",
    )
    .unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--mode")
        .arg("types")
        .assert()
        .success()
        .stdout(predicate::str::contains("type UserId"))
        .stdout(predicate::str::contains("function foo").not());
}

#[test]
fn test_cli_full_mode() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    let content = "function add(a: number, b: number): number { return a + b; }";
    fs::write(&file_path, content).unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--mode")
        .arg("full")
        .assert()
        .success()
        .stdout(predicate::str::contains(content));
}

// ============================================================================
// Language Detection Tests
// ============================================================================

#[test]
fn test_cli_auto_detect_typescript() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function test() { }").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success();
}

#[test]
fn test_cli_auto_detect_python() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.py");
    fs::write(&file_path, "def test(): pass").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success();
}

#[test]
fn test_cli_auto_detect_rust() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.rs");
    fs::write(&file_path, "fn test() {}").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success();
}

// ============================================================================
// Stdin Tests
// ============================================================================

#[test]
fn test_cli_stdin_with_language() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg("--language")
        .arg("typescript")
        .write_stdin("function test() { return 42; }")
        .assert()
        .success()
        .stdout(predicate::str::contains("function test"));
}

#[test]
fn test_cli_stdin_without_language_fails() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .write_stdin("function test() {}")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "requires --language or --filename",
        ));
}

// ============================================================================
// Error Handling Tests
// ============================================================================

#[test]
fn test_cli_nonexistent_file() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("nonexistent.ts")
        .assert()
        .failure()
        .stderr(predicate::str::contains("No such file"));
}

#[test]
fn test_cli_unsupported_extension() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.xyz");
    fs::write(&file_path, "some code").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("Unsupported language"));
}

#[test]
fn test_cli_invalid_mode() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function test() {}").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--mode")
        .arg("invalid")
        .assert()
        .failure();
}

// ============================================================================
// Multi-Language Tests
// ============================================================================

#[test]
fn test_cli_all_languages_structure() {
    let temp_dir = TempDir::new().unwrap();

    // TypeScript
    let ts_file = temp_dir.path().join("test.ts");
    fs::write(&ts_file, "function test() { return 42; }").unwrap();
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&ts_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("{ /* ... */ }"));

    // Python
    let py_file = temp_dir.path().join("test.py");
    fs::write(&py_file, "def test():\n    return 42").unwrap();
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&py_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("{ /* ... */ }"));

    // Rust
    let rs_file = temp_dir.path().join("test.rs");
    fs::write(&rs_file, "fn test() { 42 }").unwrap();
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&rs_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("{ /* ... */ }"));

    // Go
    let go_file = temp_dir.path().join("test.go");
    fs::write(&go_file, "func test() int { return 42 }").unwrap();
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&go_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("{ /* ... */ }"));

    // Java
    let java_file = temp_dir.path().join("Test.java");
    fs::write(&java_file, "class Test { int test() { return 42; } }").unwrap();
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&java_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("{ /* ... */ }"));
}

// ============================================================================
// Edge Case Tests
// ============================================================================

#[test]
fn test_cli_empty_file() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("empty.ts");
    fs::write(&file_path, "").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn test_cli_unicode_content() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function greet() { return \"你好 🎉\"; }").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("function greet"));
}

#[test]
fn test_cli_malformed_syntax() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("broken.ts");
    fs::write(&file_path, "function broken(() { { { {").unwrap();

    // tree-sitter is error-tolerant, should not crash
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .assert()
        .success();
}

// ============================================================================
// Language Flag Tests
// ============================================================================

#[test]
fn test_cli_explicit_language_override() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(&file_path, "function test() { return 42; }").unwrap();

    // Force TypeScript parsing despite .txt extension
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--language")
        .arg("typescript")
        .assert()
        .success()
        .stdout(predicate::str::contains("function test"));
}

// ============================================================================
// Minimal Mode Tests
// ============================================================================

#[test]
fn test_cli_minimal_mode() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(
        &file_path,
        "// regular comment\n/**\n * JSDoc\n */\nfunction add(a: number, b: number): number {\n    // body comment\n    return a + b;\n}\n",
    )
    .unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--mode")
        .arg("minimal")
        .assert()
        .success()
        // JSDoc preserved
        .stdout(predicate::str::contains("JSDoc"))
        // Body comment preserved
        .stdout(predicate::str::contains("// body comment"))
        // All code preserved
        .stdout(predicate::str::contains("function add"))
        .stdout(predicate::str::contains("return a + b"))
        // Regular comment stripped
        .stdout(predicate::str::contains("// regular comment").not());
}

#[test]
fn test_cli_minimal_mode_stdin() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg("--language")
        .arg("typescript")
        .arg("--mode")
        .arg("minimal")
        .write_stdin("// strip this\nfunction test() { return 42; }")
        .assert()
        .success()
        .stdout(predicate::str::contains("function test"))
        .stdout(predicate::str::contains("return 42"))
        .stdout(predicate::str::contains("// strip this").not());
}

#[test]
fn test_cli_minimal_mode_python_shebang() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.py");
    fs::write(
        &file_path,
        "#!/usr/bin/env python3\n# regular comment\ndef hello():\n    pass\n",
    )
    .unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--mode")
        .arg("minimal")
        .assert()
        .success()
        // Shebang preserved
        .stdout(predicate::str::contains("#!/usr/bin/env python3"))
        // Code preserved
        .stdout(predicate::str::contains("def hello()"))
        // Regular comment stripped
        .stdout(predicate::str::contains("# regular comment").not());
}

#[test]
fn test_cli_minimal_mode_help_text() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("minimal"));
}

// ============================================================================
// --lang Alias Tests
// ============================================================================

#[test]
fn test_cli_lang_alias_stdin() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg("--lang=typescript")
        .write_stdin("function add(a: number, b: number): number { return a + b; }")
        .assert()
        .success()
        .stdout(predicate::str::contains("function add"));
}

#[test]
fn test_cli_lang_and_language_equivalent() {
    let input = "function greet(name: string): string { return `Hello ${name}`; }";

    let lang_output = Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg("--lang=typescript")
        .write_stdin(input)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let language_output = Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg("--language=typescript")
        .write_stdin(input)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_eq!(
        lang_output, language_output,
        "--lang and --language should produce identical output"
    );
}

// ============================================================================
// --lang Alias with File Argument Tests
// ============================================================================

#[test]
fn test_cli_lang_alias_with_file() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    fs::write(
        &file_path,
        "function add(a: number, b: number): number { return a + b; }",
    )
    .unwrap();

    // --lang alias should work with file arguments, not just stdin
    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--lang=typescript")
        .assert()
        .success()
        .stdout(predicate::str::contains("function add"))
        .stdout(predicate::str::contains("{ /* ... */ }"));
}

// ============================================================================
// --filename Tests
// ============================================================================

#[test]
fn test_cli_filename_detects_rust() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg("--filename=main.rs")
        .write_stdin("fn hello() { println!(\"hi\"); }")
        .assert()
        .success()
        .stdout(predicate::str::contains("fn hello()"))
        .stdout(predicate::str::contains("{ /* ... */ }"));
}

#[test]
fn test_cli_filename_detects_typescript() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg("--filename=app.ts")
        .write_stdin("function greet(name: string): string { return name; }")
        .assert()
        .success()
        .stdout(predicate::str::contains("function greet"));
}

#[test]
fn test_cli_filename_detects_python() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg("--filename=script.py")
        .write_stdin("def hello():\n    return 42")
        .assert()
        .success()
        .stdout(predicate::str::contains("def hello()"));
}

#[test]
fn test_cli_filename_detects_go() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg("--filename=main.go")
        .write_stdin("func hello() int { return 42 }")
        .assert()
        .success()
        .stdout(predicate::str::contains("func hello()"))
        .stdout(predicate::str::contains("{ /* ... */ }"));
}

#[test]
fn test_cli_filename_detects_java() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg("--filename=Main.java")
        .write_stdin("class Main { int hello() { return 42; } }")
        .assert()
        .success()
        .stdout(predicate::str::contains("class Main"))
        .stdout(predicate::str::contains("{ /* ... */ }"));
}

#[test]
fn test_cli_filename_detects_json() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg("--filename=config.json")
        .write_stdin(r#"{"name": "skim", "version": "1.0.0", "nested": {"key": "value"}}"#)
        .assert()
        .success()
        .stdout(predicate::str::contains("name"))
        .stdout(predicate::str::contains("version"));
}

#[test]
fn test_cli_filename_language_override() {
    // --language takes priority over --filename
    Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg("--language=python")
        .arg("--filename=main.rs")
        .write_stdin("def hello():\n    return 42")
        .assert()
        .success()
        .stdout(predicate::str::contains("def hello()"));
}

#[test]
fn test_cli_filename_no_extension_fails() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg("--filename=Makefile")
        .write_stdin("all: build")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "unrecognized filename 'Makefile'",
        ));
}

#[test]
fn test_cli_filename_unknown_ext_fails() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg("--filename=foo.xyz")
        .write_stdin("some content")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "requires --language or --filename",
        ));
}

#[test]
fn test_cli_filename_not_stdin_fails() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function test() { }").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg(&file_path)
        .arg("--filename=main.rs")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--filename is only valid when reading from stdin",
        ));
}

#[test]
fn test_cli_filename_with_path_prefix() {
    // --filename with directory components should still detect language from extension
    Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg("--filename=src/lib/main.rs")
        .write_stdin("fn hello() { 42 }")
        .assert()
        .success()
        .stdout(predicate::str::contains("fn hello()"));
}

#[test]
fn test_cli_filename_with_mode() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg("--filename=app.ts")
        .arg("--mode=signatures")
        .write_stdin("type UserId = string;\nfunction greet(name: string): string { return name; }")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "function greet(name: string): string",
        ))
        .stdout(predicate::str::contains("return name").not());
}

// ============================================================================
// --filename + --mode Combined Tests
// ============================================================================

#[test]
fn test_cli_filename_rust_signatures() {
    // Key scenario: `git show HEAD:file.rs | skim --mode=signatures`
    // Verifies --filename works with Rust code and --mode=signatures
    let rust_code = r#"
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

pub struct Calculator {
    value: i32,
}

impl Calculator {
    pub fn new(value: i32) -> Self {
        Self { value }
    }

    pub fn compute(&self, x: i32) -> i32 {
        self.value + x
    }
}
"#;

    Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg("--filename=lib.rs")
        .arg("--mode=signatures")
        .write_stdin(rust_code)
        .assert()
        .success()
        // Function signatures should appear
        .stdout(predicate::str::contains(
            "pub fn add(a: i32, b: i32) -> i32",
        ))
        .stdout(predicate::str::contains("pub fn new(value: i32) -> Self"))
        .stdout(predicate::str::contains(
            "pub fn compute(&self, x: i32) -> i32",
        ))
        // Implementation details should NOT appear
        .stdout(predicate::str::contains("a + b").not())
        .stdout(predicate::str::contains("Self { value }").not())
        .stdout(predicate::str::contains("self.value + x").not());
}

// ============================================================================
// Large Stdin Streaming Tests
// ============================================================================

#[test]
fn test_cli_stdin_large_input_streaming() {
    // Generate 1000 TypeScript functions to verify streaming works with large input
    let mut input = String::new();
    for i in 0..1000 {
        input.push_str(&format!(
            "function func{}(x: number): number {{ return x + {}; }}\n",
            i, i
        ));
    }

    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg("--language=typescript")
        .write_stdin(input)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let output_str = String::from_utf8(output).unwrap();

    // Verify first and last functions appear in output
    assert!(
        output_str.contains("function func0"),
        "First function should appear in output"
    );
    assert!(
        output_str.contains("function func999"),
        "Last function should appear in output"
    );

    // Verify bodies are stripped (structure mode is default)
    assert!(
        output_str.contains("{ /* ... */ }"),
        "Function bodies should be replaced with placeholder"
    );
    assert!(
        !output_str.contains("return x + 0"),
        "Implementation details should be stripped"
    );
}

// ============================================================================
// Performance Acceptance Tests
// ============================================================================

#[test]
fn test_cli_stdin_large_input_completes_within_time_bound() {
    // Performance acceptance criterion: large input (1000 functions) must complete
    // within 5 seconds. This is extremely generous given the 50ms target for 1000-line
    // files, but guards against gross regressions in the stdin/pipe path.
    let mut input = String::new();
    for i in 0..1000 {
        input.push_str(&format!(
            "function func{}(x: number): number {{ return x + {}; }}\n",
            i, i
        ));
    }

    let start = Instant::now();

    Command::cargo_bin("skim")
        .unwrap()
        .arg("-")
        .arg("--language=typescript")
        .write_stdin(input)
        .assert()
        .success();

    let elapsed = start.elapsed();
    assert!(
        elapsed.as_secs() < 5,
        "Processing 1000 functions via stdin took {:?}, which exceeds the 5s acceptance bound",
        elapsed
    );
}
