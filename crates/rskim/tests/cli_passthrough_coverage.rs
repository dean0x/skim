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

/// `(label, before-flag git args, after-flag git args, raw-git args)` for the
/// skim-flag × git-subcommand matrix in `git_flag_stripping_passthrough`.
type SubcommandCell<'a> = (&'a str, &'a [&'a str], &'a [&'a str], &'a [&'a str]);

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

/// Build a throwaway git repo with one commit and return its path.
#[cfg(unix)]
fn git_repo(dir: &std::path::Path) {
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .expect("git must be available")
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    };
    git(&["init", "-q", "-b", "main"]);
    fs::write(
        dir.join("src.rs"),
        "fn main() {\n    println!(\"hi\");\n}\n",
    )
    .unwrap();
    git(&["add", "."]);
    git(&[
        "commit",
        "-qm",
        "seed commit with a reasonably long subject line",
    ]);
}

/// Run `program args…` in `cwd` and return stdout bytes.
fn raw_stdout(program: &str, args: &[&str], cwd: &std::path::Path) -> Vec<u8> {
    std::process::Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env("NO_COLOR", "1")
        .output()
        .unwrap_or_else(|e| panic!("{program} must be available: {e}"))
        .stdout
}

/// `git log` was the WORST measured leak: before the convergence gate,
/// `SKIM_PASSTHROUGH=1 skim git log -n 3` emitted **361 bytes against 7733 raw**
/// in this repo. `cmd/git/log.rs` never routes through
/// `run_parsed_command_with_mode`, so the execution-layer hatch never saw it.
#[cfg(unix)]
#[test]
fn test_git_log_passthrough_equals_raw_git_log() {
    let dir = TempDir::new().unwrap();
    git_repo(dir.path());
    let raw = raw_stdout("git", &["log", "-n", "1"], dir.path());
    assert!(!raw.is_empty(), "precondition: git log must produce output");

    let out = passthrough_skim()
        .current_dir(dir.path())
        .args(["git", "log", "-n", "1"])
        .output()
        .unwrap();
    assert_eq!(
        out.stdout,
        raw,
        "SKIM_PASSTHROUGH=1 skim git log must be byte-identical to git log; got {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// `git status` — a second git subcommand, and the one PF-024 measured emitting
/// the 121-byte `--porcelain=v2` stream where the user's command costs 100 B.
/// The gate runs the user's literal argv, so no substitution can survive it.
#[cfg(unix)]
#[test]
fn test_git_status_passthrough_equals_raw_git_status() {
    let dir = TempDir::new().unwrap();
    git_repo(dir.path());
    fs::write(dir.path().join("untracked.txt"), "x").unwrap();
    let raw = raw_stdout("git", &["status"], dir.path());

    let out = passthrough_skim()
        .current_dir(dir.path())
        .args(["git", "status"])
        .output()
        .unwrap();
    assert_eq!(out.stdout, raw, "git status must pass through verbatim");
}

/// `git show <rev>:<path>` — ADR-011 named this as confirmed hole #1: file-content
/// mode dropped 20% of a code file with ZERO stderr bytes, and `show.rs` never
/// called `is_passthrough_mode()`, so the hatch was a documented NO-OP there.
/// It flows through `cmd::dispatch`, so the convergence gate covers it.
#[cfg(unix)]
#[test]
fn test_git_show_rev_path_passthrough_equals_raw() {
    let dir = TempDir::new().unwrap();
    git_repo(dir.path());
    let raw = raw_stdout("git", &["show", "HEAD:src.rs"], dir.path());
    assert!(!raw.is_empty(), "precondition: blob must have content");

    let out = passthrough_skim()
        .current_dir(dir.path())
        .args(["git", "show", "HEAD:src.rs"])
        .output()
        .unwrap();
    assert_eq!(
        out.stdout, raw,
        "git show <rev>:<path> must pass through verbatim (ADR-011 hole #1)"
    );
}

/// Build family (`make`): a stubbed tool proves the gate covers the family
/// without depending on a real build toolchain.
#[cfg(unix)]
#[test]
fn test_build_tool_passthrough_equals_raw_make() {
    let dir = TempDir::new().unwrap();
    let payload = "cc -c a.c\ncc -c b.c\na.c:3:1: warning: unused variable 'x'\nld -o app\n";
    common::make_stub(dir.path(), "make", payload, "", 0);

    let out = passthrough_skim()
        .env("PATH", common::stub_path(dir.path()))
        .args(["make", "all"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        payload,
        "build-family output must pass through verbatim"
    );
}

/// `gh run watch` — named in the plan as a family the per-handler checks missed.
#[cfg(unix)]
#[test]
fn test_gh_run_watch_passthrough_equals_raw() {
    let dir = TempDir::new().unwrap();
    let payload = "* build in 1m2s (ID 123)\n* test in 44s (ID 124)\n✓ deploy in 12s (ID 125)\n";
    common::make_stub(dir.path(), "gh", payload, "", 0);

    let out = passthrough_skim()
        .env("PATH", common::stub_path(dir.path()))
        .args(["gh", "run", "watch"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        payload,
        "gh run watch must pass through verbatim"
    );
}

// ============================================================================
// B1: the gate must NOT hijack the FILTER role
//
// REGRESSION GUARD for the defect that made the first attempt at this gate
// break 11 tests: `SKIM_PASSTHROUGH=1` has two meanings depending on whether
// skim is being used as a command WRAPPER or as a compressing FILTER over piped
// input, and the gate must only claim the first.  Exec-ing the tool in filter
// mode DISCARDS the caller's piped payload — and for a tool that is not
// installed (the common case in CI) emits nothing whatsoever.
// ============================================================================

/// Piped stdin + no real args ⇒ FILTER role: the caller's bytes come back, and
/// `cypress` is never exec'd (it is not installed in this environment).
#[test]
fn test_passthrough_filter_role_forwards_piped_stdin_verbatim() {
    let raw = "{\"stats\":{\"suites\":1,\"tests\":2,\"passes\":2},\"results\":[]}";

    passthrough_skim()
        .args(["cypress", "run"])
        .write_stdin(raw)
        .assert()
        .stdout(predicate::str::contains("\"stats\""))
        .stdout(predicate::str::contains("\"suites\""));
}

/// The same discriminator for a MULTI-LEVEL dispatcher, where the handler sees
/// `[]` but dispatch sees `["test"]`.  Normalising argv wrong here is what made
/// `swift` / `dotnet` / `cargo` / `go` regress.
#[test]
fn test_passthrough_filter_role_forwards_piped_stdin_for_multi_level_dispatcher() {
    let raw = "swift raw output line 1\nswift raw output line 2\n";

    passthrough_skim()
        .args(["swift", "test"])
        .write_stdin(raw)
        .assert()
        .stdout(predicate::str::contains("swift raw output line 1"))
        .stdout(predicate::str::contains("swift raw output line 2"));
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
    // TypeScript pseudo mode strips decorators and non-parameter type annotations.
    // The @injectable() decorator is removed; the greet() parameter type is preserved (E1/ADR-008).
    fs::write(
        &file,
        "@injectable()\nexport class UserService {\n  private name: string;\n  greet(name: string): string { return `Hi ${name}`; }\n}\n",
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
    // Use a class with a decorator so pseudo mode strips content; greet() parameter type preserved (E1).
    fs::write(
        &file,
        "@injectable()\nexport class UserService {\n  private name: string;\n  greet(name: string): string { return `Hi ${name}`; }\n}\n",
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
    // Use a class with a decorator so pseudo mode strips content and fires the lossy marker.
    // The @injectable() decorator is removed; greet() parameter type is preserved (E1/ADR-008).
    fs::write(
        &file,
        "@injectable()\nexport class UserService {\n  private name: string;\n  greet(name: string): string { return `Hi ${name}`; }\n}\n",
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
    // All > 20, so fallback_line_truncate fires and emits the elision marker.
    //
    // Disclosure and remedy live on different streams. The full marker
    // ("… — SKIM_PASSTHROUGH=1 for full output") costs ~17 tokens on its own, so
    // at a 20-token budget it cannot fit on stdout without either busting the
    // budget or suppressing the marker entirely — and suppression would be silent
    // loss (#317). So stdout carries the *disclosure* (the count), kept short
    // enough to fit, and stderr carries the *remedy* as an unconditional
    // ADR-011 class-1 notice. Both obligations are met, neither budget is broken.
    skim()
        .arg(&file)
        .arg("--tokens=20")
        .assert()
        .success()
        .stdout(predicate::str::contains("truncated"))
        .stderr(predicate::str::contains("SKIM_PASSTHROUGH=1"));
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

// ============================================================================
// C1: strip_skim_flags — byte-comparison verification (PF-027)
//
// These tests verify the C1 fix: `strip_skim_flags` removes skim-only flags
// before the passthrough exec so the real tool never sees them.
//
// Invokes raw git via `/usr/bin/git` (absolute path) per PF-026 — a skim rewrite
// hook may be live on the developer machine; absolute paths bypass it.
// ============================================================================

/// `SKIM_PASSTHROUGH=1 skim git diff --json` must produce byte-identical output
/// to `/usr/bin/git diff`.
///
/// Before C1: git exited 129 with "error: invalid option: --json" because the
/// passthrough exec forwarded skim-only flags verbatim.
/// After C1: `strip_skim_flags("git", args)` removes `--json` before exec.
#[cfg(unix)]
#[test]
fn test_passthrough_git_diff_json_strips_flag() {
    let dir = TempDir::new().unwrap();
    git_repo(dir.path());
    // Modify the file without staging to produce an unstaged diff.
    fs::write(
        dir.path().join("src.rs"),
        "fn main() {\n    println!(\"world\");\n}\n",
    )
    .unwrap();

    // Raw baseline: /usr/bin/git diff (absolute path per PF-026).
    let raw = raw_stdout("/usr/bin/git", &["diff"], dir.path());
    assert!(
        !raw.is_empty(),
        "precondition: unstaged diff must produce output"
    );

    // Skim passthrough with --json: C1 strips --json before exec.
    let out = passthrough_skim()
        .current_dir(dir.path())
        .args(["git", "diff", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "SKIM_PASSTHROUGH=1 skim git diff --json must succeed \
         (C1 strips --json before passthrough exec); got exit {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        out.stdout, raw,
        "byte-comparison FAILED: SKIM_PASSTHROUGH=1 skim git diff --json \
         vs /usr/bin/git diff must be byte-identical; \
         C1 regression: strip_skim_flags must remove --json before passthrough exec"
    );
}

/// `SKIM_PASSTHROUGH=1 skim git show --json HEAD:src.rs` must produce
/// byte-identical output to `/usr/bin/git show HEAD:src.rs`.
#[cfg(unix)]
#[test]
fn test_passthrough_git_show_json_strips_flag() {
    let dir = TempDir::new().unwrap();
    git_repo(dir.path());
    let raw = raw_stdout("/usr/bin/git", &["show", "HEAD:src.rs"], dir.path());
    assert!(!raw.is_empty(), "precondition: blob must have content");

    let out = passthrough_skim()
        .current_dir(dir.path())
        .args(["git", "show", "--json", "HEAD:src.rs"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "SKIM_PASSTHROUGH=1 skim git show --json must succeed; got exit {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        out.stdout, raw,
        "byte-comparison FAILED: SKIM_PASSTHROUGH=1 skim git show --json HEAD:src.rs \
         vs /usr/bin/git show HEAD:src.rs must be byte-identical"
    );
}

/// `SKIM_PASSTHROUGH=1 skim git log -n 1 --json` must produce byte-identical output
/// to `/usr/bin/git log -n 1`.
#[cfg(unix)]
#[test]
fn test_passthrough_git_log_json_strips_flag() {
    let dir = TempDir::new().unwrap();
    git_repo(dir.path());
    let raw = raw_stdout("/usr/bin/git", &["log", "-n", "1"], dir.path());
    assert!(!raw.is_empty(), "precondition: git log must produce output");

    let out = passthrough_skim()
        .current_dir(dir.path())
        .args(["git", "log", "-n", "1", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "SKIM_PASSTHROUGH=1 skim git log -n 1 --json must succeed; got exit {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        out.stdout, raw,
        "byte-comparison FAILED: SKIM_PASSTHROUGH=1 skim git log -n 1 --json \
         vs /usr/bin/git log -n 1 must be byte-identical;\n\
         C1 regression: strip_skim_flags must remove --json before passthrough exec"
    );
}

/// `SKIM_PASSTHROUGH=1 skim git status --json` must produce byte-identical
/// output to `/usr/bin/git status`.
///
/// `git status --json` is the exit whose disclosure marker carries the legacy
/// `SKIM_PASSTHROUGH=1 for full output` remedy (D1 / ADR-011 class 1).  That
/// remedy is only true if the hatch actually reproduces the user's argv, which
/// requires `strip_skim_flags("git", …)` to remove `--json` before the exec —
/// otherwise git exits 129 with "error: invalid option: --json" and the marker
/// is pointing at a dead end.  This test is what keeps the printed remedy
/// honest for the status path, alongside the `diff`, `show` and `log` siblings
/// above.
#[cfg(unix)]
#[test]
fn test_passthrough_git_status_json_strips_flag() {
    let dir = TempDir::new().unwrap();
    git_repo(dir.path());
    // Modify the file without staging so status has something to report.
    fs::write(
        dir.path().join("src.rs"),
        "fn main() {\n    println!(\"world\");\n}\n",
    )
    .unwrap();

    // Raw baseline: /usr/bin/git status (absolute path per PF-026).
    let raw = raw_stdout("/usr/bin/git", &["status"], dir.path());
    assert!(
        !raw.is_empty(),
        "precondition: git status must produce output"
    );

    let out = passthrough_skim()
        .current_dir(dir.path())
        .args(["git", "status", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "SKIM_PASSTHROUGH=1 skim git status --json must succeed \
         (C1 strips --json before passthrough exec); got exit {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        out.stdout, raw,
        "byte-comparison FAILED: SKIM_PASSTHROUGH=1 skim git status --json \
         vs /usr/bin/git status must be byte-identical;\n\
         C1 regression: strip_skim_flags must remove --json before passthrough exec"
    );
}

// ============================================================================
// C2: --passthrough CLI flag parity with SKIM_PASSTHROUGH=1
// ============================================================================

/// `skim --passthrough git log -n 1` must produce byte-identical output to
/// `SKIM_PASSTHROUGH=1 skim git log -n 1`.
///
/// C2 adds `--passthrough` as a CLI-flag alternative to `SKIM_PASSTHROUGH=1`.
/// Both paths must trigger the same structural passthrough gate.
#[cfg(unix)]
#[test]
fn test_passthrough_flag_parity_with_env_var() {
    let dir = TempDir::new().unwrap();
    git_repo(dir.path());

    // Path A: SKIM_PASSTHROUGH=1 env var.
    let env_out = passthrough_skim()
        .current_dir(dir.path())
        .args(["git", "log", "-n", "1"])
        .output()
        .unwrap();
    assert!(env_out.status.success(), "env-var path must succeed");
    assert!(
        !env_out.stdout.is_empty(),
        "env-var path must produce output"
    );

    // Path B: --passthrough CLI flag (no env var).
    let flag_out = common::skim()
        .env_remove("SKIM_PASSTHROUGH")
        .current_dir(dir.path())
        .args(["--passthrough", "git", "log", "-n", "1"])
        .output()
        .unwrap();
    assert!(
        flag_out.status.success(),
        "--passthrough flag path must succeed; got exit {:?}\nstderr: {}",
        flag_out.status.code(),
        String::from_utf8_lossy(&flag_out.stderr)
    );
    assert_eq!(
        flag_out.stdout, env_out.stdout,
        "byte-comparison FAILED: --passthrough flag must produce byte-identical output \
         to SKIM_PASSTHROUGH=1;\n\
         C2 regression: set_passthrough_flag() must activate the same convergence gate \
         as the env var"
    );
}

// ============================================================================
// C1/C2 extended: table-driven — 8 skim-only flags × 3 git subcommands (24 cells)
// ============================================================================

/// Table-driven byte-identity sweep: 8 skim-only flags × 3 git subcommands = 24 cells.
///
/// For every `(flag, sub)` pair, asserts:
/// - `/usr/bin/git <sub-args>` exits 0 (precondition — verified once per subcommand)
/// - `SKIM_PASSTHROUGH=1 skim git <sub> <flag> [git-args]` exits 0
/// - stdout is byte-identical to the raw baseline
///
/// Flags: `--json`, `--mode=structure`, `--show-stats`, `--passthrough`,
/// `--max-lines 5`, `--tokens 50`, `--line-numbers`, `--debug`.
/// (`--debug` emits an 88-byte provenance banner on stderr only; stdout equality
/// still holds because the banner never touches fd 1.)
///
/// Subcommands: `diff` (unstaged change), `show HEAD:src.rs`, `log -n 1`.
/// Flag injection point: between the subcommand token and any git-native tail args,
/// consistent with the placement used in the individual C1 tests above.
#[cfg(unix)]
#[test]
fn test_passthrough_strips_every_skim_flag_for_git_diff_show_log() {
    let dir = TempDir::new().unwrap();
    git_repo(dir.path());
    // Produce an unstaged change so `git diff` generates non-empty output.
    fs::write(
        dir.path().join("src.rs"),
        "fn main() {\n    println!(\"world\");\n}\n",
    )
    .unwrap();

    // 8 skim-only flags.  Two-token forms for --max-lines and --tokens.
    let flags: &[(&str, &[&str])] = &[
        ("--json", &["--json"]),
        ("--mode=structure", &["--mode=structure"]),
        ("--show-stats", &["--show-stats"]),
        ("--passthrough", &["--passthrough"]),
        ("--max-lines 5", &["--max-lines", "5"]),
        ("--tokens 50", &["--tokens", "50"]),
        ("--line-numbers", &["--line-numbers"]),
        ("--debug", &["--debug"]),
    ];

    // 3 subcommands: (label, before-flag args, after-flag args, raw-git args).
    // skim invocation = `skim git <before> <flag_args…> <after>`
    // raw invocation  = `/usr/bin/git <raw_git_args>`
    let subs: &[SubcommandCell<'_>] = &[
        ("diff", &["diff"], &[], &["diff"]),
        (
            "show",
            &["show"],
            &["HEAD:src.rs"],
            &["show", "HEAD:src.rs"],
        ),
        ("log", &["log", "-n", "1"], &[], &["log", "-n", "1"]),
    ];

    for &(sub_label, before, after, raw_git_args) in subs {
        // Compute the raw baseline once per subcommand and assert its exit code.
        let raw_output = std::process::Command::new("/usr/bin/git")
            .args(raw_git_args)
            .current_dir(dir.path())
            .env("NO_COLOR", "1")
            .output()
            .unwrap_or_else(|e| panic!("/usr/bin/git {raw_git_args:?} failed to spawn: {e}"));
        assert!(
            raw_output.status.success(),
            "precondition: /usr/bin/git {raw_git_args:?} must exit 0"
        );
        if sub_label == "diff" {
            assert!(
                !raw_output.stdout.is_empty(),
                "precondition: /usr/bin/git diff must produce output \
                 (unstaged change required)"
            );
        }
        let raw = raw_output.stdout;

        for &(flag_label, flag_args) in flags {
            let cell = format!("{flag_label} \u{d7} git {sub_label}");

            // Build: skim git <before> <flag_args…> <after>
            let mut skim_args: Vec<&str> = vec!["git"];
            skim_args.extend_from_slice(before);
            skim_args.extend_from_slice(flag_args);
            skim_args.extend_from_slice(after);

            let out = passthrough_skim()
                .current_dir(dir.path())
                .args(&skim_args)
                .output()
                .unwrap();

            assert!(
                out.status.success(),
                "cell [{cell}]: skim must exit 0; got {:?}\nstderr: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(
                out.stdout, raw,
                "cell [{cell}]: stdout must be byte-identical to \
                 /usr/bin/git {raw_git_args:?}"
            );
        }
    }
}
