//! E2E integration tests for `skim doctor` — hook integrity reporting (#471).
//!
//! Each test uses `skim_sandboxed` with a `TempDir`-scoped home directory so
//! that `skim init` and `skim doctor` cannot touch the developer's real
//! `~/.gemini/GEMINI.md`, `~/.skim/bin/`, or any other real home-dir state
//! (PF-017 avoids PF-017).
//!
//! The cwd for all `skim doctor` invocations is set to the sandbox home
//! directory (which is NOT a git repository) so the staleness-vs-HEAD check
//! inside doctor skips deterministically and cannot cause spurious exit-1s.
//!
//! ## PATH isolation
//!
//! `skim doctor`'s $PATH scan reports drift when the binary that WINS on PATH
//! differs from the binary being tested (e.g. `target/release/skim` on PATH
//! vs `target/debug/skim` running the test). To prevent this spurious exit-1,
//! tests that assert exit-0 MUST pass a controlled PATH that puts the test
//! binary's directory first via `hermetic_path()`.
//!
//! Tests asserting exit-1 (`test_doctor_exits_1_and_names_tamper_...`) also
//! use `hermetic_path()` for consistency and to ensure the asserted drift comes
//! only from the tampered hook, not PATH state.

use std::io::Write;
use tempfile::TempDir;
mod common;

// ============================================================================
// Helpers
// ============================================================================

/// Return a PATH string with the test binary's parent directory prepended.
///
/// This ensures `skim doctor`'s $PATH scan finds only the test binary as the
/// winning `skim` entry, preventing spurious PATH-drift exit-1s on machines
/// where a release build (`target/release/skim`) also appears on PATH.
fn hermetic_path() -> String {
    let bin = common::skim_bin();
    let bin_dir = bin.parent().expect("skim binary has a parent directory");
    let system_path = std::env::var("PATH").unwrap_or_default();
    format!("{}:{}", bin_dir.display(), system_path)
}

/// Install the skim hook into a sandboxed home directory.
///
/// Uses `--agent claude-code --no-guidance --no-wrappers` to avoid interactive
/// prompts and to confine mutations to the known `.claude/hooks/` path.
fn do_sandboxed_init(home: &std::path::Path) {
    common::skim_sandboxed(home)
        .args([
            "init",
            "--agent",
            "claude-code",
            "--no-guidance",
            "--no-wrappers",
        ])
        .env("PATH", hermetic_path())
        .assert()
        .success();
}

/// Path to the installed hook script inside the sandbox.
fn hook_script_path(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".claude/hooks/skim-rewrite.sh")
}

/// Path to the SHA-256 manifest inside the sandbox.
fn manifest_path(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".claude/hooks/skim-claude-code.sha256")
}

// ============================================================================
// E2E tests
// ============================================================================

/// After a fresh `skim init`, `skim doctor` must exit 0 (HEALTHY).
///
/// This is the failing test before #471: appending even one byte to the hook
/// script previously left doctor reporting ✓ healthy on exit 0 because
/// `print_hook_section` derived its verdict from `SKIM_HOOK_*` markers parsed
/// out of the script text rather than from the SHA-256 manifest.
#[test]
fn test_doctor_exits_0_after_clean_init() {
    let home = TempDir::new().unwrap();
    let home = home.path();

    // detect_installed_agents() in override-mode checks if the config dir
    // is an existing directory — create it before running init.
    std::fs::create_dir_all(home.join(".claude")).unwrap();

    do_sandboxed_init(home);

    // current_dir(home): the sandbox dir is not a git repo, so the
    // staleness-vs-HEAD check inside doctor skips and cannot cause exit 1.
    // hermetic_path(): ensures the test binary wins on $PATH so that the PATH
    // scan section does not report drift from an unrelated release build.
    common::skim_sandboxed(home)
        .arg("doctor")
        .current_dir(home)
        .env("PATH", hermetic_path())
        .assert()
        .success();
}

/// After tampering with the hook script (appending one byte), `skim doctor`
/// must exit 1 AND name the tamper in stdout.
///
/// This is the core regression case for #471: the old code exited 0 even
/// after tampering because it read its verdict from the tampered bytes.
#[test]
fn test_doctor_exits_1_and_names_tamper_after_hook_modification() {
    let home = TempDir::new().unwrap();
    let home = home.path();

    std::fs::create_dir_all(home.join(".claude")).unwrap();
    do_sandboxed_init(home);

    // Verify the manifest exists (confirming init wrote it).
    assert!(
        manifest_path(home).exists(),
        "SHA-256 manifest must be written by skim init"
    );

    // Tamper: append exactly one byte to the hook script.
    let script = hook_script_path(home);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&script)
        .expect("hook script must exist after init");
    file.write_all(b"X").unwrap();
    drop(file);

    // Doctor must exit 1 AND say "tampered" in stdout.
    // hermetic_path() ensures drift comes only from the tamper, not PATH state.
    common::skim_sandboxed(home)
        .arg("doctor")
        .current_dir(home)
        .env("PATH", hermetic_path())
        .assert()
        .failure() // exit 1
        .stdout(predicates::prelude::predicate::str::contains("tampered"));
}

/// When the SHA-256 manifest is deleted (simulating a pre-manifest install),
/// `skim doctor` must exit 0 — `NoManifest` is advisory, not drift.
///
/// Users who installed skim before manifest support existed have done nothing
/// wrong and must not have their `skim doctor` exit-0 broken.
#[test]
fn test_doctor_exits_0_when_no_manifest() {
    let home = TempDir::new().unwrap();
    let home = home.path();

    std::fs::create_dir_all(home.join(".claude")).unwrap();
    do_sandboxed_init(home);

    // Delete the sidecar to simulate a pre-manifest install.
    let manifest = manifest_path(home);
    assert!(manifest.exists(), "manifest must exist before deletion");
    std::fs::remove_file(&manifest).unwrap();

    // NoManifest → advisory, not drift → exit 0.
    // hermetic_path() prevents PATH drift from an unrelated release build.
    common::skim_sandboxed(home)
        .arg("doctor")
        .current_dir(home)
        .env("PATH", hermetic_path())
        .assert()
        .success();
}
