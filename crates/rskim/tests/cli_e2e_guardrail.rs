//! E2E tests for guardrail behavior (#54).
//!
//! Tests the guardrail mechanism that falls back to raw content when
//! compressed output would be larger than the original.
//!
//! The guardrail fires when transformation inflates content (e.g., a file
//! with all type declarations and no compressible bodies).

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn skim_cmd() -> Command {
    Command::cargo_bin("skim").unwrap()
}

// ============================================================================
// Guardrail: types-only file (minimal compression possible)
// ============================================================================

#[test]
fn test_guardrail_types_only_file() {
    let dir = TempDir::new().unwrap();
    // A file with only type declarations — structure mode can't compress it
    // because there are no function bodies to strip. The guardrail should
    // detect that compressed output >= raw and either pass through raw or
    // produce output no larger than the original.
    let file = dir.path().join("types_only.ts");
    std::fs::write(
        &file,
        "type A = string;\n\
         type B = number;\n\
         type C = boolean;\n\
         type D = { a: A; b: B; c: C; };\n\
         type E = Array<D>;\n\
         type F = Map<string, E>;\n\
         type G = Promise<F>;\n\
         type H = Record<string, G>;\n\
         type I = Partial<H>;\n\
         type J = Required<I>;\n",
    )
    .unwrap();

    // Run skim on the types-only file — output should contain the type declarations
    // regardless of whether guardrail fires or not
    skim_cmd()
        .arg(file.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("type A"))
        .stdout(predicate::str::contains("type J"));
}

// ============================================================================
// Guardrail: file with compressible content (guardrail should NOT fire)
// ============================================================================

#[test]
fn test_guardrail_compressible_file_works_normally() {
    let dir = TempDir::new().unwrap();
    // A file with a function body that can be compressed
    let file = dir.path().join("compressible.ts");
    std::fs::write(
        &file,
        "function calculateTotal(items: number[]): number {\n\
         \tlet sum = 0;\n\
         \tfor (const item of items) {\n\
         \t\tsum += item;\n\
         \t}\n\
         \treturn sum;\n\
         }\n\
         \n\
         function processData(data: string[]): string[] {\n\
         \tconst results: string[] = [];\n\
         \tfor (const d of data) {\n\
         \t\tresults.push(d.trim().toUpperCase());\n\
         \t}\n\
         \treturn results;\n\
         }\n",
    )
    .unwrap();

    // Structure mode should compress the function bodies
    skim_cmd()
        .arg(file.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("calculateTotal"))
        .stdout(predicate::str::contains("processData"));
}

// ============================================================================
// Guardrail: stdin input
// ============================================================================

#[test]
fn test_guardrail_stdin_types_only() {
    // Pipe types-only content through stdin with `-` as the file argument
    // and --language to specify the language
    let input = "type X = string;\ntype Y = number;\ntype Z = boolean;\n";
    skim_cmd()
        .args(["-", "--language", "typescript"])
        .write_stdin(input)
        .assert()
        .success()
        .stdout(predicate::str::contains("type X"))
        .stdout(predicate::str::contains("type Z"));
}
