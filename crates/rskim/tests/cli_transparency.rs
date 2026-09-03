//! E2E tests for the hook-rewrite transparency marker.
//!
//! The transparency marker is emitted to stderr when:
//! - `SKIM_REWRITTEN_FROM=<cat|head|tail>` is set in the environment, AND
//! - The served view differs from raw file bytes (mode != full, output != raw).
//!
//! It is NOT emitted when:
//! - The env var is absent (explicit `skim file.rs --mode=pseudo` invocation)
//! - The output is byte-identical to raw (e.g. `--mode=full`)
//! - The guardrail fired and raw bytes were served
//! - The file is a `.py` whose pseudo view happens to equal the raw content

use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;
mod common;

fn skim_cmd() -> assert_cmd::Command {
    let mut cmd = common::skim();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_REWRITTEN_FROM");
    cmd
}

// ============================================================================
// Marker fires when origin tag is present and view differs
// ============================================================================

/// Tagged pseudo read of a TypeScript file: pseudo mode strips parameter type
/// annotations, so the view always differs from raw bytes → marker must appear.
#[test]
fn test_transparency_marker_fires_on_tagged_pseudo_read() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.ts");
    // TS pseudo mode strips parameter types `: string`, `: number` etc.
    // The transformed view will not equal the raw bytes.
    fs::write(
        &file,
        "export function greet(name: string, age: number): string {\n  \
         const msg: string = `Hello ${name} age ${age}`;\n  \
         return msg;\n}\n",
    )
    .unwrap();

    skim_cmd()
        .env("SKIM_REWRITTEN_FROM", "cat")
        .arg(&file)
        .arg("--mode=pseudo")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim] transformed view"))
        .stderr(predicate::str::contains("cat"))
        .stderr(predicate::str::contains("pseudo"))
        .stderr(predicate::str::contains("SKIM_PASSTHROUGH=1"));
}

// ============================================================================
// Marker is silent without origin tag
// ============================================================================

/// Explicit `skim file.rs --mode=pseudo` without the env tag must NOT emit a marker.
/// This prevents false positives on direct skim invocations.
#[test]
fn test_no_marker_without_origin_tag() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.rs");
    fs::write(&file, "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n").unwrap();

    skim_cmd()
        .arg(&file)
        .arg("--mode=pseudo")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim] transformed view").not());
}

// ============================================================================
// Marker is silent when output equals raw (--mode=full)
// ============================================================================

/// Full mode is byte-identical to raw — no marker even with origin tag.
#[test]
fn test_no_marker_when_output_equals_raw_full_mode() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.rs");
    fs::write(&file, "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n").unwrap();

    skim_cmd()
        .env("SKIM_REWRITTEN_FROM", "cat")
        .arg(&file)
        .arg("--mode=full")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim] transformed view").not());
}

// ============================================================================
// Marker is silent when guardrail fires (raw bytes served, view_differs=false)
// ============================================================================

/// When the fidelity guardrail fires (transformed output > raw bytes), skim serves
/// raw bytes, making `final_output == contents` and therefore `view_differs = false`.
/// The transparency marker must stay silent even though `SKIM_REWRITTEN_FROM` is set.
///
/// Fixture shape mirrors `cli_guardrail.rs::test_guardrail_triggers_when_output_inflates`:
/// 20 empty-body JS functions where structure mode replaces each `{ }` (3 bytes)
/// with ` {...}` (6 bytes), inflating the output past the raw budget.
#[test]
fn test_no_marker_when_guardrail_fires() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("inflating.ts");

    // 20 functions × ~18 bytes = ~360 bytes raw (above MIN_RAW_SIZE_FOR_GUARDRAIL of 256).
    // Each empty body `{ }` (3 bytes) → ` {...}` (6 bytes) = +3 bytes per function,
    // so the compressed output exceeds the raw size and the guardrail fires.
    let mut source = String::new();
    for i in 0..20 {
        source.push_str(&format!("function f{i}() {{ }}\n"));
    }
    assert!(
        source.len() >= 256,
        "fixture must be >= 256 bytes for guardrail, got {}",
        source.len()
    );
    fs::write(&file, &source).unwrap();

    let output = skim_cmd()
        .env("SKIM_DEBUG", "1")
        .env("SKIM_REWRITTEN_FROM", "cat")
        .arg(&file)
        .arg("--mode=structure")
        .arg("--no-cache")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);

    // Guardrail must have fired — detectable via SKIM_DEBUG=1.
    assert!(
        stderr.contains("[skim:guardrail]"),
        "expected guardrail notice on stderr (with SKIM_DEBUG=1), got: {stderr}"
    );

    // Transparency marker must be absent — guardrail served raw bytes so view_differs=false.
    assert!(
        !stderr.contains("[skim] transformed view"),
        "transparency marker must be silent when guardrail serves raw bytes, got: {stderr}"
    );
}

// ============================================================================
// Marker names the mode (structure vs pseudo)
// ============================================================================

/// Structure-mode declaration file: when the structure-mode output differs from
/// raw bytes, the marker names "structure" not "pseudo".
#[test]
fn test_transparency_marker_names_structure_mode() {
    let dir = TempDir::new().unwrap();
    // Use a .ts file with a function body so structure mode strips the body.
    let file = dir.path().join("api.ts");
    fs::write(
        &file,
        "export function greet(name: string): string {\n  return `Hello ${name}`;\n}\n",
    )
    .unwrap();

    let stderr_bytes = skim_cmd()
        .env("SKIM_REWRITTEN_FROM", "cat")
        .arg(&file)
        .arg("--mode=structure")
        .output()
        .unwrap()
        .stderr;
    let stderr = String::from_utf8_lossy(&stderr_bytes);

    // Either the view differs (marker fires naming "structure") or it doesn't (no marker).
    // If it fires, the mode name must be "structure", not "pseudo".
    if stderr.contains("[skim] transformed view") {
        assert!(
            stderr.contains("structure"),
            "transparency marker must name 'structure' mode; got: {stderr}"
        );
        assert!(
            !stderr.contains("pseudo"),
            "transparency marker must not name 'pseudo' for structure mode; got: {stderr}"
        );
    }
}

// ============================================================================
// head tag with --max-lines
// ============================================================================

/// head tag with `--max-lines`: structure mode strips the function body (mirroring
/// what the head rewrite actually emits: `SKIM_REWRITTEN_FROM=head skim <file>
/// --mode=structure --max-lines N`).  Structure mode guarantees view differs from
/// raw, so the marker must appear and must name `head`.
#[test]
fn test_transparency_marker_with_head_tag() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("main.ts");
    // TypeScript with a function body: structure mode strips the body.
    fs::write(
        &file,
        "export function greet(name: string): string {\n  return `Hello ${name}`;\n}\n",
    )
    .unwrap();

    skim_cmd()
        .env("SKIM_REWRITTEN_FROM", "head")
        .arg(&file)
        .arg("--mode=structure")
        .arg("--max-lines=10")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim] transformed view"))
        .stderr(predicate::str::contains("head"))
        .stderr(predicate::str::contains("SKIM_PASSTHROUGH=1"));
}

// ============================================================================
// Multi-file aggregate — exactly one marker line
// ============================================================================

/// Multi-file: when multiple files differ, only ONE aggregate marker is emitted.
/// Uses TypeScript files so pseudo mode definitely changes the output.
#[test]
fn test_multi_file_aggregate_marker_emitted_once() {
    let dir = TempDir::new().unwrap();
    let f1 = dir.path().join("a.ts");
    let f2 = dir.path().join("b.ts");
    // TS pseudo mode strips `: number` type annotations → view differs from raw.
    fs::write(
        &f1,
        "export function foo(x: number): number {\n  return x * 2;\n}\n",
    )
    .unwrap();
    fs::write(
        &f2,
        "export function bar(x: number): number {\n  return x + 1;\n}\n",
    )
    .unwrap();

    let stderr_bytes = skim_cmd()
        .env("SKIM_REWRITTEN_FROM", "cat")
        .arg("--mode=pseudo")
        .arg(&f1)
        .arg(&f2)
        .output()
        .unwrap()
        .stderr;
    let stderr = String::from_utf8_lossy(&stderr_bytes);

    let marker_count = stderr.matches("[skim] transformed view").count();
    assert_eq!(
        marker_count, 1,
        "multi-file transparency marker must appear exactly once; got {marker_count} occurrences in stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("2/2 files not raw bytes"),
        "multi-file marker must show 2/2 count; got: {stderr}"
    );
}

// ============================================================================
// Cache hit: marker fires on second read (repeat invocation)
// ============================================================================

/// Cache hit path: the marker must also fire when the result comes from the
/// skim cache (not just on fresh reads). Uses a TypeScript file so pseudo
/// mode definitely produces a different view (strips parameter type annotations).
#[test]
fn test_transparency_marker_fires_on_cache_hit() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("cached.ts");
    // TS pseudo mode strips `: number` type annotations → view differs from raw.
    fs::write(
        &file,
        "export function square(x: number): number {\n  return x * x;\n}\n",
    )
    .unwrap();

    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    // First run (cache miss): populate the cache.
    skim_cmd()
        .env("SKIM_REWRITTEN_FROM", "cat")
        .env("SKIM_CACHE_DIR", &cache_dir)
        .arg(&file)
        .arg("--mode=pseudo")
        .assert()
        .success();

    // Second run (cache hit): marker must still fire.
    skim_cmd()
        .env("SKIM_REWRITTEN_FROM", "cat")
        .env("SKIM_CACHE_DIR", &cache_dir)
        .arg(&file)
        .arg("--mode=pseudo")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim] transformed view"));
}
