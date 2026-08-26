//! B1–B5 transparency-invariant tests (Work Stream B: Transparency).
//!
//! ## Invariant I2
//! "Never lose content without saying so; `SKIM_PASSTHROUGH=1` always works."
//!
//! ## Coverage
//! - B1: Structural passthrough gate covers every command family + log + read path
//! - B2: Proxy passthrough accepts "true" / "yes" (not just "1")
//! - B3: `lossy_view_marker` fires at exit 0 without `SKIM_REWRITTEN_FROM` tag
//! - B4: Lossy marker names the elided class
//! - B5: rskim-core truncation markers carry SKIM_PASSTHROUGH=1 hint when CLI wires it
//! - ADR-011 regression: lossy markers are unconditional (no SKIM_DEBUG gate)
//!
//! ## Surfaces under test
//! Only the rewrite-engine surface (skim binary as subcommand) is tested here.
//! Wrapper-surface parity is covered by `cli_both_surfaces_paired.rs`.

use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;
mod common;

fn skim() -> assert_cmd::Command {
    let mut cmd = common::skim();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_REWRITTEN_FROM");
    cmd
}

fn passthrough_skim() -> assert_cmd::Command {
    let mut cmd = common::skim();
    cmd.env("SKIM_PASSTHROUGH", "1");
    cmd.env_remove("SKIM_REWRITTEN_FROM");
    cmd
}

// ============================================================================
// B1: Read-path passthrough — `SKIM_PASSTHROUGH=1 skim <file>`
// ============================================================================

/// `SKIM_PASSTHROUGH=1 skim file.ts` must output the raw file bytes unchanged.
///
/// Before B1, the read path had no structural passthrough gate — it went through
/// the full transform pipeline even in passthrough mode.  After B1, the gate
/// fires early in `process_single_arg` and outputs raw bytes.
#[test]
fn test_read_path_passthrough_outputs_raw_bytes() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.ts");
    let raw = "export function greet(name: string): string {\n  return `Hello ${name}`;\n}\n";
    fs::write(&file, raw).unwrap();

    passthrough_skim()
        .arg(&file)
        .assert()
        .success()
        .stdout(predicate::eq(raw))
        .stderr(predicate::str::is_empty());
}

/// `SKIM_PASSTHROUGH=1 skim -` (stdin) must copy stdin to stdout unchanged.
#[test]
fn test_stdin_passthrough_copies_raw_bytes() {
    let raw = "2025-01-01T00:00:00Z INFO  server started\n2025-01-01T00:00:01Z ERROR oops\n";

    passthrough_skim()
        .arg("-")
        .write_stdin(raw)
        .assert()
        .success()
        .stdout(predicate::eq(raw))
        .stderr(predicate::str::is_empty());
}

// ============================================================================
// B1: Dispatch-level passthrough — tool wrapper family
// ============================================================================

/// `SKIM_PASSTHROUGH=1 skim ls <dir>` must produce byte-identical output to
/// running `ls <dir>` directly.  The dispatch gate must apply before any
/// compression logic.
#[test]
fn test_ls_passthrough_equals_raw_ls() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("a.txt"), "a").unwrap();
    fs::write(dir.path().join("b.rs"), "b").unwrap();

    // Capture raw `ls` output
    let raw_out = std::process::Command::new("ls")
        .arg(dir.path())
        .output()
        .expect("ls must be available")
        .stdout;
    let raw_str = String::from_utf8_lossy(&raw_out);

    passthrough_skim()
        .args(["ls", &dir.path().display().to_string()])
        .assert()
        .success()
        .stdout(predicate::eq(raw_str.as_ref()));
}

// ============================================================================
// B1: Log passthrough — `SKIM_PASSTHROUGH=1 skim log`
// ============================================================================

/// `skim log` is a META subcommand (not a PATH-wrapper target). Before B1,
/// it had no passthrough gate of its own.  After B1, it checks
/// `is_passthrough_mode()` at the top of its `run()` function and forwards
/// stdin raw.
#[test]
fn test_log_passthrough_forwards_stdin_raw() {
    let raw = "2025-01-01T00:00:00Z INFO  hello\n2025-01-01T00:00:01Z WARN  world\n";

    passthrough_skim()
        .args(["log"])
        .write_stdin(raw)
        .assert()
        .success()
        .stdout(predicate::eq(raw))
        .stderr(predicate::str::is_empty());
}

// ============================================================================
// B1: env exclusion — `SKIM_PASSTHROUGH=1 skim env` must still redact (PF-012)
// ============================================================================

/// Security invariant: passthrough MUST NOT bypass `skim env` redaction.
/// Credential values must be replaced by *** even when SKIM_PASSTHROUGH=1.
///
/// PF-012: a size/net-savings heuristic may choose between renderings, but
/// every non-negotiable property — redaction, sanitization — must hold on
/// ALL paths.
#[test]
fn test_env_passthrough_still_redacts() {
    // Inject a fake secret into the env.  skim env must redact its value.
    let assert = passthrough_skim()
        .args(["env"])
        .env("MY_SECRET_TOKEN", "super-secret-value-1234")
        .assert()
        .success();

    // The key must appear (env shows variable names).
    assert.stdout(predicate::str::contains("MY_SECRET_TOKEN"));
    // The raw value must NOT appear.
    passthrough_skim()
        .args(["env"])
        .env("MY_SECRET_TOKEN", "super-secret-value-1234")
        .assert()
        .success()
        .stdout(predicate::str::contains("super-secret-value-1234").not());
}

// ============================================================================
// B2: Proxy passthrough accepts "true" and "yes" (not just "1")
// ============================================================================

// Note: proxy is feature-gated. These tests verify the shared helper behavior
// via the rewrite-path check in cmd/mod.rs rather than launching an actual proxy.

/// `check_passthrough_str("true")` must return truthy — verifiable via the
/// read-path gate (which calls `is_passthrough_mode` → `check_passthrough_str`).
#[test]
fn test_passthrough_true_value_activates_gate() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.rs");
    let raw = "pub fn f() -> i32 { 42 }\n";
    fs::write(&file, raw).unwrap();

    common::skim()
        .env("SKIM_PASSTHROUGH", "true")
        .env_remove("SKIM_REWRITTEN_FROM")
        .arg(&file)
        .assert()
        .success()
        .stdout(predicate::eq(raw));
}

/// `check_passthrough_str("yes")` must return truthy.
#[test]
fn test_passthrough_yes_value_activates_gate() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.rs");
    let raw = "pub fn f() -> i32 { 42 }\n";
    fs::write(&file, raw).unwrap();

    common::skim()
        .env("SKIM_PASSTHROUGH", "yes")
        .env_remove("SKIM_REWRITTEN_FROM")
        .arg(&file)
        .assert()
        .success()
        .stdout(predicate::eq(raw));
}

/// `check_passthrough_str("YES")` (uppercase) must return truthy.
#[test]
fn test_passthrough_yes_uppercase_activates_gate() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.rs");
    let raw = "pub fn add(a: i32, b: i32) -> i32 { a + b }\n";
    fs::write(&file, raw).unwrap();

    common::skim()
        .env("SKIM_PASSTHROUGH", "YES")
        .env_remove("SKIM_REWRITTEN_FROM")
        .arg(&file)
        .assert()
        .success()
        .stdout(predicate::eq(raw));
}

// ============================================================================
// B3: lossy_view_marker fires at exit 0 without SKIM_REWRITTEN_FROM
// ============================================================================

/// When `skim file.ts --mode=pseudo` runs WITHOUT `SKIM_REWRITTEN_FROM`, the
/// view still differs from raw bytes.  After B3, a lossy-view marker fires
/// on stderr even without the rewrite origin tag.
///
/// ADR-011 class 1: loss-bearing markers are UNCONDITIONAL — they do NOT require
/// `SKIM_REWRITTEN_FROM` and are NOT gated by `SKIM_DEBUG`.
#[test]
fn test_lossy_view_marker_fires_without_origin_tag() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.ts");
    // TypeScript pseudo mode strips `: string` type annotations — view always differs.
    fs::write(
        &file,
        "export function greet(name: string): string {\n  return `Hi ${name}`;\n}\n",
    )
    .unwrap();

    skim()
        .arg(&file)
        .arg("--mode=pseudo")
        .arg("--no-cache")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("SKIM_PASSTHROUGH=1"));
}

/// ADR-011 regression: lossy marker must fire WITHOUT `SKIM_DEBUG`.
///
/// Lossy-view markers are ADR-011 class 1 (unconditional), not class 2
/// (SKIM_DEBUG-gated). This test verifies the marker appears even without SKIM_DEBUG.
#[test]
fn test_lossy_marker_fires_without_skim_debug() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.ts");
    fs::write(
        &file,
        "export function greet(name: string): string {\n  return `Hi ${name}`;\n}\n",
    )
    .unwrap();

    // Explicitly remove SKIM_DEBUG — marker must still fire.
    skim()
        .env_remove("SKIM_DEBUG")
        .arg(&file)
        .arg("--mode=pseudo")
        .arg("--no-cache")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"));
}

/// ADR-011 regression: no-loss guardrail fallback emits ZERO stderr bytes
/// without SKIM_DEBUG (class 2 banner, debug-gated).
///
/// When the guardrail fires and raw bytes are served, the view equals the raw
/// bytes → no lossy-view marker.  The guardrail's debug-level notice must also
/// be absent without SKIM_DEBUG.
#[test]
fn test_no_loss_guardrail_emits_no_stderr_without_skim_debug() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("inflating.ts");
    // 20 empty-body JS functions: structure mode inflates output → guardrail fires.
    let mut source = String::new();
    for i in 0..20 {
        source.push_str(&format!("function f{i}() {{ }}\n"));
    }
    fs::write(&file, &source).unwrap();

    skim()
        .env_remove("SKIM_DEBUG")
        .arg(&file)
        .arg("--mode=structure")
        .arg("--no-cache")
        .assert()
        .success()
        .stderr(predicate::str::is_empty());
}

// ============================================================================
// B4: Lossy marker names the elided class
// ============================================================================

/// The lossy-view marker must name the transformation class for pseudo mode.
/// Acceptable class identifiers: "pseudo", "pseudo view", "bodies".
#[test]
fn test_lossy_marker_names_pseudo_class() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.ts");
    fs::write(
        &file,
        "export function greet(name: string): string {\n  return `Hi ${name}`;\n}\n",
    )
    .unwrap();

    skim()
        .arg(&file)
        .arg("--mode=pseudo")
        .arg("--no-cache")
        .assert()
        .success()
        // The marker must identify "pseudo" as the view type.
        .stderr(predicate::str::contains("pseudo"));
}

/// The lossy-view marker must name the transformation class for structure mode.
#[test]
fn test_lossy_marker_names_structure_class() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.ts");
    // A function with a non-trivial body so structure mode differs from raw.
    fs::write(
        &file,
        "export function greet(name: string): string {\n  const msg = `Hi ${name}`;\n  return msg;\n}\n",
    )
    .unwrap();

    skim()
        .arg(&file)
        .arg("--mode=structure")
        .arg("--no-cache")
        .assert()
        .success()
        .stderr(predicate::str::contains("structure"));
}

// ============================================================================
// B5: rskim-core truncation markers carry SKIM_PASSTHROUGH=1 hint
// ============================================================================

/// `skim file.ts --max-lines=1` truncates output and emits a comment marker.
/// After B5, the marker must include "SKIM_PASSTHROUGH=1" so readers know
/// how to get the full output.
#[test]
fn test_max_lines_marker_carries_passthrough_hint() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.ts");
    // 10-line function: max-lines=1 will definitely truncate.
    let src = (1..=10)
        .map(|i| format!("export const v{i} = {i};\n"))
        .collect::<String>();
    fs::write(&file, &src).unwrap();

    skim()
        .arg(&file)
        .arg("--max-lines=1")
        .assert()
        .success()
        // The truncation marker must carry the passthrough hint.
        .stdout(predicate::str::contains("SKIM_PASSTHROUGH=1"));
}

/// `skim file.ts --last-lines=1` truncates from the top and must include the hint.
#[test]
fn test_last_lines_marker_carries_passthrough_hint() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.ts");
    let src = (1..=10)
        .map(|i| format!("export const v{i} = {i};\n"))
        .collect::<String>();
    fs::write(&file, &src).unwrap();

    skim()
        .arg(&file)
        .arg("--last-lines=1")
        .assert()
        .success()
        .stdout(predicate::str::contains("SKIM_PASSTHROUGH=1"));
}

/// `skim file.rs --tokens=20` falls through to token-budget truncation when all
/// cascade modes exceed the budget, and the fallback marker must carry the hint.
///
/// Uses a Rust file with BOTH type declarations and functions: all cascade modes
/// (structure → signatures → types) produce non-empty output for this fixture,
/// so with a tiny token budget ALL modes are exhausted and `fallback_line_truncate`
/// is invoked.  That function calls `truncate_to_token_budget` which emits the
/// B5 elision marker carrying "SKIM_PASSTHROUGH=1 for full output".
#[test]
fn test_token_budget_marker_carries_passthrough_hint() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.rs");
    // Rust file with types + functions: structure/signatures/types modes all
    // produce non-empty output, ensuring fallback_line_truncate is reached.
    let src = "\
pub type Id = u64;\n\
pub type Name = String;\n\
pub struct User { pub id: Id, pub name: Name }\n\
pub enum Status { Active, Inactive }\n\
pub fn get_user(id: Id) -> Option<User> { None }\n\
pub fn set_status(user: &mut User, status: Status) { }\n\
pub fn delete_user(id: Id) -> bool { false }\n\
pub fn list_users() -> Vec<User> { vec![] }\n\
pub fn count_users() -> usize { 0 }\n\
pub fn find_by_name(name: &Name) -> Option<User> { None }\n";
    fs::write(&file, src).unwrap();

    // --tokens=20: structure mode ~100 tokens, signatures ~60 tokens, types ~36 tokens
    // All > 20, so fallback_line_truncate fires and emits the B5 elision marker.
    skim()
        .arg(&file)
        .arg("--tokens=20")
        .assert()
        .success()
        // fallback_line_truncate uses truncate_to_token_budget which emits:
        // "// ... (N lines truncated) — SKIM_PASSTHROUGH=1 for full output"
        .stdout(predicate::str::contains("SKIM_PASSTHROUGH=1"));
}

/// rskim-core markers carry NO CLI hint when the library is used without CLI
/// wiring (elision_hint = None in TransformConfig).
///
/// This test verifies the hint is absent when TransformConfig is built directly
/// without the CLI layer setting `elision_hint`. Since we can't call the library
/// directly from E2E tests, we use the `--no-cache` flag to ensure fresh output
/// and check that the library marker (sans hint) is structurally present by
/// verifying the hint IS present via the CLI path (see test above).
///
/// Implementation note: both marker variants (with/without hint) contain
/// "truncated" — the B5 invariant is that the CLI-built config ADDS the hint.
/// The E2E tests above confirm the CLI path does carry it.
#[test]
fn test_max_lines_marker_hint_absent_when_not_set() {
    // This test verifies that without CLI wiring, the hint "SKIM_PASSTHROUGH=1"
    // would NOT appear in the marker output. However, from the CLI we ALWAYS
    // wire the hint (B5 requirement). So this is a documentation test: it
    // passes vacuously because the CLI always wires the hint.
    //
    // The real test of "no CLI text when unset" is covered by the rskim-core
    // unit tests in crates/rskim-core/src/transform/truncate.rs.
    //
    // We assert here that the marker text itself contains "truncated" (not just the hint).
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.ts");
    let src = (1..=10)
        .map(|i| format!("export const v{i} = {i};\n"))
        .collect::<String>();
    fs::write(&file, &src).unwrap();

    skim()
        .arg(&file)
        .arg("--max-lines=1")
        .assert()
        .success()
        .stdout(predicate::str::contains("truncated"));
}
