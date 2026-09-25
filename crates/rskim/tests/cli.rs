//! CLI integration tests using assert_cmd
//!
//! Tests the full CLI binary with real command-line arguments.

use predicates::prelude::*;
use std::fs;
use std::time::Instant;
use tempfile::TempDir;
mod common;

// ============================================================================
// Basic CLI Tests
// ============================================================================

#[test]
fn test_cli_version() {
    common::skim()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim"))
        .stdout(predicate::str::is_match(r"\d+\.\d+\.\d+").unwrap());
}

#[test]
fn test_cli_help() {
    common::skim()
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
    // Bodies large enough that collapsing them to `{...}` outweighs the stderr
    // disclosure the ADR-001 guard now charges (structure: 76 B / 22 t). The
    // single-line original saved 11 B / 4 t, so raw was served and both the
    // `{...}` and the `return a + b` negation failed.
    // Measured: raw 501 B / 142 t → 89 B / 14 t, margin +336 B / +106 t.
    fs::write(
        &file_path,
        r#"function add(a: number, b: number): number {
  const sum = a + b;
  const doubled = sum * 2;
  const clamped = Math.min(doubled, 1000);
  const rounded = Math.round(clamped * 100) / 100;
  return a + b;
}

function subtract(a: number, b: number): number {
  const difference = a - b;
  const scaled = difference * 3;
  const clamped = Math.max(scaled, -1000);
  return clamped - difference;
}

function multiply(a: number, b: number): number {
  const product = a * b;
  const scaled = product * 7;
  const clamped = Math.min(scaled, 1000000);
  return clamped + product;
}
"#,
    )
    .unwrap();

    common::skim()
        .arg(&file_path)
        .arg("--mode")
        .arg("structure")
        .assert()
        .success()
        .stdout(predicate::str::contains("function add"))
        .stdout(predicate::str::contains("{...}"))
        .stdout(predicate::str::contains("return a + b").not());
}

#[test]
fn test_cli_signatures_mode() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    // Sized so signature extraction is actually SERVED: the single-line original
    // saved 18 B / 7 t against an 89 B / 24 t marker, so raw was served and
    // `contains("return").not()` saw the body.
    // Measured: raw 501 B / 142 t → 66 B / 7 t, margin +346 B / +111 t.
    fs::write(
        &file_path,
        r#"function add(a: number, b: number): number {
  const sum = a + b;
  const doubled = sum * 2;
  const clamped = Math.min(doubled, 1000);
  const rounded = Math.round(clamped * 100) / 100;
  return a + b;
}

function subtract(a: number, b: number): number {
  const difference = a - b;
  const scaled = difference * 3;
  const clamped = Math.max(scaled, -1000);
  return clamped - difference;
}

function multiply(a: number, b: number): number {
  const product = a * b;
  const scaled = product * 7;
  const clamped = Math.min(scaled, 1000000);
  return clamped + product;
}
"#,
    )
    .unwrap();

    common::skim()
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
    // Types mode keeps type declarations and drops functions, so the functions
    // are what pays for the disclosure (types: 79 B / 24 t). The 51-byte original
    // saved 30 B / 9 t and was served raw, leaving `function foo` in the output.
    // Measured: raw 492 B / 165 t → 42 B / 12 t, margin +371 B / +129 t.
    fs::write(
        &file_path,
        r#"type UserId = string;
type OrderId = string;

function foo() {
  const total = 42;
  const scaled = total * 2;
  const clamped = Math.min(scaled, 1000);
  const rounded = Math.round(clamped * 100) / 100;
  return rounded;
}

function bar() {
  const x = 1;
  const y = 2;
  const z = x + y;
  const w = z * 3;
  return w - z;
}

function baz() {
  const values = [1, 2, 3, 4, 5];
  const total = values.reduce((acc, v) => acc + v, 0);
  const average = total / values.length;
  return average;
}
"#,
    )
    .unwrap();

    common::skim()
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

    common::skim()
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

    common::skim().arg(&file_path).assert().success();
}

#[test]
fn test_cli_auto_detect_python() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.py");
    fs::write(&file_path, "def test(): pass").unwrap();

    common::skim().arg(&file_path).assert().success();
}

#[test]
fn test_cli_auto_detect_rust() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.rs");
    fs::write(&file_path, "fn test() {}").unwrap();

    common::skim().arg(&file_path).assert().success();
}

// ============================================================================
// Stdin Tests
// ============================================================================

#[test]
fn test_cli_stdin_with_language() {
    common::skim()
        .arg("-")
        .arg("--language")
        .arg("typescript")
        .write_stdin("function test() { return 42; }")
        .assert()
        .success()
        .stdout(predicate::str::contains("function test"));
}

#[test]
fn test_cli_stdin_without_language_passes_through() {
    // ADR-002: shebang-less stdin without --language degrades to lossless passthrough
    // (exit 0), consistent with the file path behaviour for unknown extensions.
    // The input content is emitted verbatim (non-UTF-8 stdin still fails).
    common::skim()
        .arg("-")
        .write_stdin("function test() {}")
        .assert()
        .success()
        .stdout(predicate::str::contains("function test() {}"));
}

// ============================================================================
// Error Handling Tests
// ============================================================================

#[test]
fn test_cli_nonexistent_file() {
    common::skim()
        .arg("nonexistent.ts")
        .assert()
        .failure()
        .stderr(predicate::str::contains("No such file"));
}

#[test]
fn test_cli_unsupported_extension() {
    // ADR-002: unknown extensions degrade to lossless passthrough (exit 0).
    // The original error-on-unknown behavior was replaced by graceful degradation.
    // SKIM_DEBUG=1 emits a notice; without it, the output is the file contents.
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.xyz");
    fs::write(&file_path, "some code").unwrap();

    common::skim()
        .arg(&file_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("some code"));
}

#[test]
fn test_cli_invalid_mode() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function test() {}").unwrap();

    common::skim()
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

    // Every fixture here carries TWO multi-statement functions. One short function
    // is not enough: the ADR-001 guard charges the 76 B / 22 t structure marker
    // against the saving, and the previous one-liners saved only 3-49 B / 1-16 t,
    // so all five were served raw and no `{...}` appeared anywhere. The per-file
    // margins below are each >=2x the marker in both bytes and tokens.
    // (The pre-existing Rust note about single-expression bodies tying on token
    // count still holds — it is the same gate, now with the disclosure priced in.)

    // TypeScript — margin +252 B / +80 t
    let ts_file = temp_dir.path().join("test.ts");
    fs::write(
        &ts_file,
        r#"function test() {
  const total = 42;
  const scaled = total * 2;
  const clamped = Math.min(scaled, 1000);
  const rounded = Math.round(clamped * 100) / 100;
  return rounded + total;
}

function verify() {
  const base = 17;
  const widened = base * 3;
  const bounded = Math.max(widened, -1000);
  const settled = Math.round(bounded * 100) / 100;
  return settled - base;
}
"#,
    )
    .unwrap();
    common::skim()
        .arg(&ts_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("{...}"));

    // Python — margin +178 B / +64 t
    let py_file = temp_dir.path().join("test.py");
    fs::write(
        &py_file,
        r#"def test():
    total = 42
    scaled = total * 2
    clamped = min(scaled, 1000)
    rounded = round(clamped * 100) / 100
    return rounded + total


def verify():
    base = 17
    widened = base * 3
    bounded = max(widened, -1000)
    settled = round(bounded * 100) / 100
    return settled - base
"#,
    )
    .unwrap();
    common::skim()
        .arg(&py_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("{...}"));

    // Rust — margin +218 B / +65 t
    let rs_file = temp_dir.path().join("test.rs");
    fs::write(
        &rs_file,
        r#"fn compute_result(x: i64, y: i64) -> i64 {
    let sum = x + y;
    let product = x * y;
    let clamped = product.min(1_000_000);
    let adjusted = clamped - sum;
    sum + product + adjusted
}

fn verify_result(x: i64, y: i64) -> i64 {
    let base = x - y;
    let widened = base * 3;
    let bounded = widened.max(-1_000_000);
    let settled = bounded + base;
    settled - widened
}
"#,
    )
    .unwrap();
    common::skim()
        .arg(&rs_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("{...}"));

    // Go — margin +203 B / +64 t
    let go_file = temp_dir.path().join("test.go");
    fs::write(
        &go_file,
        r#"func test() int {
    total := 42
    scaled := total * 2
    clamped := scaled
    if clamped > 1000 {
        clamped = 1000
    }
    return clamped + total
}

func verify() int {
    base := 17
    widened := base * 3
    bounded := widened
    if bounded < -1000 {
        bounded = -1000
    }
    return bounded - base
}
"#,
    )
    .unwrap();
    common::skim()
        .arg(&go_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("{...}"));

    // Java — margin +290 B / +74 t
    let java_file = temp_dir.path().join("Test.java");
    fs::write(
        &java_file,
        r#"class Test {
    int test() {
        int total = 42;
        int scaled = total * 2;
        int clamped = Math.min(scaled, 1000);
        int rounded = Math.round(clamped * 100) / 100;
        return rounded + total;
    }

    int verify() {
        int base = 17;
        int widened = base * 3;
        int bounded = Math.max(widened, -1000);
        int settled = bounded + base;
        return settled - widened;
    }
}
"#,
    )
    .unwrap();
    common::skim()
        .arg(&java_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("{...}"));
}

// ============================================================================
// Edge Case Tests
// ============================================================================

#[test]
fn test_cli_empty_file() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("empty.ts");
    fs::write(&file_path, "").unwrap();

    common::skim()
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

    common::skim()
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
    common::skim().arg(&file_path).assert().success();
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
    common::skim()
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
    // `// regular comment` sits BELOW the function on purpose: a comment run
    // contiguous from byte 0 is the module header and is preserved in every
    // language under #476. Keeping it at the top would test header preservation
    // rather than the stripping this test exists for — see
    // test_cli_minimal_mode_preserves_module_header.
    // The stripped comment run is the ONLY saving minimal mode produces here, so
    // one trailing comment (19 B / 4 t) could never cover the 108 B / 28 t marker
    // the ADR-001 guard charges — raw was served and `// regular comment`
    // survived. Comments are token-cheap relative to bytes, so the run is long
    // enough for the TOKEN margin to clear 2x as well.
    // Measured: raw 693 B / 158 t → 109 B / 45 t, margin +520 B / +85 t.
    fs::write(
        &file_path,
        r#"/**
 * JSDoc
 */
function add(a: number, b: number): number {
    // body comment
    return a + b;
}

// regular comment
// a second standalone comment that minimal mode removes from the output
// a third standalone comment, also removed, adding to the measured saving
// a fourth standalone comment so the saving comfortably exceeds the marker
// a fifth standalone comment keeping the margin above twice the disclosure
// a sixth standalone comment, removed like every other trailing comment run
// a seventh standalone comment widening the token margin past the threshold
// an eighth standalone comment so a tokeniser bump cannot flip this verdict
// a ninth standalone comment completing the trailing run under the function
"#,
    )
    .unwrap();

    common::skim()
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
fn test_cli_minimal_mode_preserves_module_header() {
    // #476: the leading contiguous comment run is the module header and is
    // preserved in every language, not just the four that used to be allowlisted.
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("header.ts");
    // The run below the blank line is the only saving, so it has to be long
    // enough to cover the 108 B / 28 t minimal marker; the original 123-byte
    // fixture saved 33 B / 7 t and was served raw, so the "stripped" line was
    // still present. Margin +439 B / +74 t.
    fs::write(
        &file_path,
        r#"// Copyright header line
// Part of the skim integration suite, still inside the module header run

// stripped after the blank line
// a second standalone comment below the header break, also stripped
// a third standalone comment below the break, adding to the saving
// a fourth standalone comment so the margin clears the disclosure twice over
// a fifth standalone comment widening the token margin past the threshold
// a sixth standalone comment so a tokeniser bump cannot flip this verdict
// a seventh standalone comment below the header break, likewise removed
// an eighth standalone comment completing the stripped run before the code
function add(a: number, b: number): number {
    const sum = a + b;
    return sum;
}
"#,
    )
    .unwrap();

    common::skim()
        .arg(&file_path)
        .arg("--mode")
        .arg("minimal")
        .assert()
        .success()
        // Top-of-file header preserved
        .stdout(predicate::str::contains("// Copyright header line"))
        // A run after the blank-line header break is still stripped
        .stdout(predicate::str::contains("// stripped after the blank line").not())
        .stdout(predicate::str::contains("function add"));
}

#[test]
fn test_cli_minimal_mode_stdin() {
    // The comment sits BELOW the function on purpose — see the note in
    // test_cli_minimal_mode.
    common::skim()
        .arg("-")
        .arg("--language")
        .arg("typescript")
        .arg("--mode")
        .arg("minimal")
        // stdin is charged the marker exactly as a file read is, and the 45-byte
        // original saved 15 B / 4 t against 108 B / 28 t — served raw, so
        // `// strip this` survived. Margin +354 B / +58 t.
        // (None of the added lines may contain the literal `// strip this`, or
        // the negative predicate would match one of them instead.)
        .write_stdin(
            "function test() { return 42; }\n\
             \n\
             // strip this\n\
             // a second standalone comment removed by minimal mode from the output\n\
             // a third standalone comment, also removed, adding to the measured saving\n\
             // a fourth standalone comment so the saving comfortably exceeds the marker\n\
             // a fifth standalone comment widening the token margin past the threshold\n\
             // a sixth standalone comment so a tokeniser bump cannot flip this verdict\n\
             // a seventh standalone comment completing the trailing run under the code\n",
        )
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

    common::skim()
        .arg(&file_path)
        .arg("--mode")
        .arg("minimal")
        .assert()
        .success()
        // Shebang preserved
        .stdout(predicate::str::contains("#!/usr/bin/env python3"))
        // Code preserved
        .stdout(predicate::str::contains("def hello()"))
        // Module-header comment immediately after shebang (no blank line) preserved (#476)
        .stdout(predicate::str::contains("# regular comment"));
}

#[test]
fn test_cli_minimal_mode_help_text() {
    common::skim()
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
    common::skim()
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

    let lang_output = common::skim()
        .arg("-")
        .arg("--lang=typescript")
        .write_stdin(input)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let language_output = common::skim()
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
    // `{...}` is the only evidence that `--lang=typescript` actually selected the
    // TS parser for a `.txt` file, so it must not be dropped — which means the
    // compressed view has to be served. The one-liner saved 11 B / 4 t against a
    // 76 B / 22 t marker. Margin +336 B / +106 t.
    fs::write(
        &file_path,
        r#"function add(a: number, b: number): number {
  const sum = a + b;
  const doubled = sum * 2;
  const clamped = Math.min(doubled, 1000);
  const rounded = Math.round(clamped * 100) / 100;
  return a + b;
}

function subtract(a: number, b: number): number {
  const difference = a - b;
  const scaled = difference * 3;
  const clamped = Math.max(scaled, -1000);
  return clamped - difference;
}

function multiply(a: number, b: number): number {
  const product = a * b;
  const scaled = product * 7;
  const clamped = Math.min(scaled, 1000000);
  return clamped + product;
}
"#,
    )
    .unwrap();

    // --lang alias should work with file arguments, not just stdin
    common::skim()
        .arg(&file_path)
        .arg("--lang=typescript")
        .assert()
        .success()
        .stdout(predicate::str::contains("function add"))
        .stdout(predicate::str::contains("{...}"));
}

// ============================================================================
// --filename Tests
// ============================================================================

#[test]
fn test_cli_filename_detects_rust() {
    common::skim()
        .arg("-")
        .arg("--filename=main.rs")
        // `{...}` is the only assertion that can fail if detection picks the
        // wrong parser — `contains("fn hello()")` is satisfied by the raw input
        // itself — so it stays, and the fixture is sized to make it reachable.
        // (The sibling `_typescript`/`_python`/`_json` tests assert only the
        // positive symbol and are correspondingly unfalsifiable; that is not a
        // pattern worth copying.) Margin +235 B / +50 t against 76 B / 22 t.
        .write_stdin(
            r#"fn hello() {
    let greeting = "hi";
    let repeated = greeting.repeat(3);
    let trimmed = repeated.trim().to_string();
    let shouted = trimmed.to_uppercase();
    println!("{shouted}");
}

fn farewell() {
    let parting = "bye";
    let repeated = parting.repeat(2);
    let trimmed = repeated.trim().to_string();
    println!("{trimmed}");
}
"#,
        )
        .assert()
        .success()
        .stdout(predicate::str::contains("fn hello()"))
        .stdout(predicate::str::contains("{...}"));
}

#[test]
fn test_cli_filename_detects_typescript() {
    common::skim()
        .arg("-")
        .arg("--filename=app.ts")
        .write_stdin("function greet(name: string): string { return name; }")
        .assert()
        .success()
        .stdout(predicate::str::contains("function greet"));
}

#[test]
fn test_cli_filename_detects_python() {
    common::skim()
        .arg("-")
        .arg("--filename=script.py")
        .write_stdin("def hello():\n    return 42")
        .assert()
        .success()
        .stdout(predicate::str::contains("def hello()"));
}

#[test]
fn test_cli_filename_detects_go() {
    common::skim()
        .arg("-")
        .arg("--filename=main.go")
        // As with the Rust case: `{...}` is the falsifiable half, so the fixture
        // must be large enough for the structure view to be served.
        // Margin +203 B / +64 t against 76 B / 22 t.
        .write_stdin(
            r#"func hello() int {
    total := 42
    scaled := total * 2
    clamped := scaled
    if clamped > 1000 {
        clamped = 1000
    }
    return clamped + total
}

func farewell() int {
    base := 17
    widened := base * 3
    bounded := widened
    if bounded < -1000 {
        bounded = -1000
    }
    return bounded - base
}
"#,
        )
        .assert()
        .success()
        .stdout(predicate::str::contains("func hello()"))
        .stdout(predicate::str::contains("{...}"));
}

#[test]
fn test_cli_filename_detects_java() {
    common::skim()
        .arg("-")
        .arg("--filename=Main.java")
        // As with the Rust and Go cases: `{...}` is the falsifiable half.
        // Margin +290 B / +74 t against 76 B / 22 t.
        .write_stdin(
            r#"class Main {
    int hello() {
        int total = 42;
        int scaled = total * 2;
        int clamped = Math.min(scaled, 1000);
        int rounded = Math.round(clamped * 100) / 100;
        return rounded + total;
    }

    int farewell() {
        int base = 17;
        int widened = base * 3;
        int bounded = Math.max(widened, -1000);
        int settled = bounded + base;
        return settled - widened;
    }
}
"#,
        )
        .assert()
        .success()
        .stdout(predicate::str::contains("class Main"))
        .stdout(predicate::str::contains("{...}"));
}

#[test]
fn test_cli_filename_detects_json() {
    common::skim()
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
    common::skim()
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
    common::skim()
        .arg("-")
        .arg("--filename=Makefile")
        .write_stdin("all: build")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Unsupported language for file: Makefile",
        ));
}

#[test]
fn test_cli_filename_unknown_ext_fails() {
    common::skim()
        .arg("-")
        .arg("--filename=foo.xyz")
        .write_stdin("some content")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Unsupported language for file: foo.xyz",
        ));
}

#[test]
fn test_cli_filename_not_stdin_fails() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function test() { }").unwrap();

    common::skim()
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
    common::skim()
        .arg("-")
        .arg("--filename=src/lib/main.rs")
        .write_stdin("fn hello() { 42 }")
        .assert()
        .success()
        .stdout(predicate::str::contains("fn hello()"));
}

#[test]
fn test_cli_filename_with_mode() {
    common::skim()
        .arg("-")
        .arg("--filename=app.ts")
        .arg("--mode=signatures")
        // `contains("return name").not()` is a transform assertion, so signature
        // extraction has to be served: the 75-byte original saved 39 B / 10 t
        // against an 89 B / 24 t marker. Margin +363 B / +71 t.
        .write_stdin(
            r#"type UserId = string;

function greet(name: string): string {
  const rendered = `Hello ${name}`;
  const trimmed = rendered.trim();
  const shouted = trimmed.toUpperCase();
  return name;
}

function announce(subject: string, channel: string): string {
  const rendered = `${subject} on ${channel}`;
  const trimmed = rendered.trim();
  const shouted = trimmed.toUpperCase();
  return shouted;
}

function dispatch(payload: string, endpoint: string): string {
  const rendered = `${payload} sent to ${endpoint}`;
  const trimmed = rendered.trim();
  const shouted = trimmed.toUpperCase();
  return shouted;
}
"#,
        )
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

    common::skim()
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

    let output = common::skim()
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
        output_str.contains("{...}"),
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

    common::skim()
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

// ============================================================================
// Pseudo Mode CLI Tests
// ============================================================================

#[test]
fn test_cli_pseudo_mode() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(
        &file_path,
        "export function add(a: number, b: number): number { return a + b; }",
    )
    .unwrap();

    common::skim()
        .arg(&file_path)
        .arg("--mode")
        .arg("pseudo")
        .assert()
        .success()
        .stdout(predicate::str::contains("function add"))
        .stdout(predicate::str::contains("return a + b"))
        // ADR-008/E1: param type annotations are API surface — preserved in pseudo mode.
        // Both parameter and return type annotations survive.
        .stdout(predicate::str::contains(
            "function add(a: number, b: number)",
        ))
        .stdout(predicate::str::contains("): number"))
        // `export` is preserved as API surface (A4 contract)
        .stdout(predicate::str::contains("export"));
}

#[test]
fn test_cli_pseudo_mode_python() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.py");
    // Python pseudo strips parameter annotations and keeps bodies, so the saving
    // is annotation mass alone — the 57-byte original saved 5 B / 2 t against a
    // 128 B / 32 t marker and was served raw, leaving `: str` in the output.
    // `greet` deliberately keeps its single parameter so the assertion
    // `contains("def greet(name)")` still matches after stripping; the other
    // functions supply the mass. Margin +250 B / +56 t.
    fs::write(
        &file_path,
        r#"def greet(name: str) -> str:
    return f"Hello, {name}!"


def announce(subject: Mapping[str, Sequence[int]], predicate: Optional[Callable[[int], bool]], locale: Union[str, bytes, None], channel: Sequence[Mapping[str, float]]) -> str:
    rendered = f"{subject} {predicate}"
    return rendered


def dispatch(payload: Mapping[str, Sequence[bytes]], endpoint: Optional[Callable[[str], None]], retries: Union[int, float, None], timeout: Sequence[Mapping[str, float]]) -> str:
    rendered = f"{payload} sent to {endpoint}"
    return rendered


def reconcile(ledger: Mapping[str, Sequence[Decimal]], adjustments: Optional[Callable[[Decimal], Decimal]], window: Union[int, timedelta, None], sink: Sequence[Mapping[str, Decimal]]) -> str:
    rendered = f"{ledger} reconciled over {window}"
    return rendered
"#,
    )
    .unwrap();

    common::skim()
        .arg(&file_path)
        .arg("--mode")
        .arg("pseudo")
        .assert()
        .success()
        // Param type annotation stripped; return type preserved (A4 contract)
        .stdout(predicate::str::contains("def greet(name)"))
        .stdout(predicate::str::contains(": str").not())
        .stdout(predicate::str::contains("-> str"));
}

#[test]
fn test_cli_pseudo_mode_rust() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.rs");
    fs::write(
        &file_path,
        "pub fn hello() -> String {\n    \"world\".to_string()\n}\n",
    )
    .unwrap();

    common::skim()
        .arg(&file_path)
        .arg("--mode")
        .arg("pseudo")
        .assert()
        .success()
        .stdout(predicate::str::contains("fn hello"))
        // `pub` is preserved as API surface (A4 contract)
        .stdout(predicate::str::contains("pub fn"));
}

#[test]
fn test_cli_pseudo_mode_stdin() {
    common::skim()
        .arg("-")
        .arg("--lang=typescript")
        .arg("--mode=pseudo")
        // `x = 42` is the pseudo rewrite of `const x: number = 42;` — a string
        // that does not occur in the raw input, so the compressed view must be
        // served. A single declaration saved 9 B / 3 t against a 128 B / 32 t
        // marker; the extra declarations supply the annotation mass.
        // Margin +230 B / +51 t.
        .write_stdin(
            r#"export const x: number = 42;
export const alpha: ReadonlyArray<Record<string, number>> = [];
export const beta: Map<string, Array<OrderEntity>> = new Map();
export const gamma: Record<string, ReadonlyArray<Error>> = {};
export const delta: CacheStore<string, OrderEntity> = createStore();
export const epsilon: TraceProvider<SpanContext, LogRecord> = createTracer();
export const zeta: MetricsSink<Counter, Gauge, Histogram> = createSink();
export const eta: ConfigResolver<ServiceOptions, Defaults> = createResolver();
export const theta: Repository<OrderEntity, OrderId> = createRepository();
export const iota: StructuredLogger<LogRecord, SpanContext> = createLogger();
"#,
        )
        .assert()
        .success()
        .stdout(predicate::str::contains("x = 42"))
        // `export` is preserved as API surface (A4 contract)
        .stdout(predicate::str::contains("export"));
}
