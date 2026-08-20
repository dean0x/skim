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

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

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

/// `skim doctor` must exit 1 and name "binary pin mismatch" when the hook was
/// installed by a binary at a different path but identical version and commit.
///
/// The trick: copy the test binary to a second path inside the TempDir, run
/// `init` from that copy (so the hook pins to the copy's path), then run
/// `doctor` from the original binary.  Same version and commit → integrity
/// stays `Verified` (no script bytes were changed); different canonical path →
/// `pin_is_current == false` → exit 1 with "binary pin mismatch".
///
/// This test verifies C-1: binary pin mismatch is advisory (⚠), NOT drift.
///
/// Two-clone scenario: init with a copied binary (so hook pins copy_path), then
/// run doctor with the original binary (same version + commit, different path).
/// After C-1, `pin_is_current == false` produces a `⚠` advisory line but does
/// NOT contribute to exit 1 — doctor exits 0.
///
/// The "binary pin mismatch" message must still appear so the user can see it,
/// just with `⚠` (advisory) instead of `✗` (drift) status.
///
/// Both invocations route through `skim_sandboxed_with_bin` (the single
/// authoritative sandbox env-var block) to satisfy PF-017.
#[cfg(unix)]
#[test]
fn test_doctor_exits_0_on_binary_pin_mismatch() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    std::fs::create_dir_all(home.join(".claude")).unwrap();

    // Copy the test binary to a second path inside the TempDir.
    let original_bin = common::skim_bin();
    let copy_path = home.join("skim-copy");
    std::fs::copy(&original_bin, &copy_path).expect("copying the test binary must succeed");

    // The copy is a regular file; mark it executable so init can invoke it.
    {
        let mut perms = std::fs::metadata(&copy_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&copy_path, perms).unwrap();
    }

    // Run `init` using the copy so the hook pins to copy_path (via
    // current_exe() inside the copy).  Routes through skim_sandboxed_with_bin
    // so the sandbox env-var block stays in one authoritative place (PF-017).
    common::skim_sandboxed_with_bin(home, &copy_path)
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

    // Run doctor from the original binary: same version/commit, different path
    // → Verified integrity, pin_is_current == false → exit 0 (advisory ⚠, not drift).
    common::skim_sandboxed(home)
        .arg("doctor")
        .current_dir(home)
        .env("PATH", hermetic_path())
        .assert()
        .success() // exit 0 — pin mismatch is advisory only (C-1 fix)
        .stdout(predicates::prelude::predicate::str::contains(
            "binary pin mismatch",
        ));
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

/// `skim doctor` must exit 0 when the compiled SHA is not found in the current
/// git repo — the end-user scenario (running doctor inside their own project,
/// which is not the skim source repo).
///
/// This is the C-2 regression test: before the fix, `print_staleness_section`
/// returned `true` (drift) when `git cat-file -e <sha>^{commit}` failed in the
/// cwd repo, causing exit 1 for every end user not running doctor from the skim
/// source directory.  After C-2, the "SHA not in this repo" case returns `false`
/// (neutral `–` line, no drift).
///
/// If `compiled_commit == "unknown"` (tarball build) or git is unavailable, the
/// staleness check is already skipped and the test trivially passes — that is
/// correct behaviour for those environments.
#[test]
fn test_doctor_does_not_exit_1_for_absent_sha() {
    let home = TempDir::new().unwrap();
    let home_path = home.path();

    std::fs::create_dir_all(home_path.join(".claude")).unwrap();
    do_sandboxed_init(home_path);

    // Create a throwaway git repo that does NOT contain the skim binary's SHA.
    let git_dir = TempDir::new().unwrap();
    let git_path = git_dir.path();

    let git_init_ok = std::process::Command::new("git")
        .arg("init")
        .current_dir(git_path)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !git_init_ok {
        // git not available in this environment — the staleness check is
        // skipped inside doctor (no drift possible), so exit 0 is guaranteed.
        return;
    }

    // Create one dummy commit so the repo has a HEAD (needed for `in_repo` check).
    let _ = std::process::Command::new("git")
        .args([
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "--allow-empty",
            "-m",
            "throwaway",
        ])
        .current_dir(git_path)
        .output();

    // Run doctor from inside the throwaway repo (which lacks the skim SHA).
    // After C-2: exits 0 (neutral `–` line for absent SHA, not drift).
    // Before C-2: would exit 1 ("SHA not found" → return true → exit 1).
    common::skim_sandboxed(home_path)
        .arg("doctor")
        .current_dir(git_path)
        .env("PATH", hermetic_path())
        .assert()
        .success(); // must exit 0 regardless of compiled_commit value
}

// ============================================================================
// Wrapper drift detection — Item 2 (#488)
// ============================================================================

/// When a wrapper symlink points at a DIFFERENT binary than the one running,
/// `skim doctor` must exit 1 and name the mismatch.
///
/// Setup:
///  1. Copy the test binary to `<tmp>/old-install/skim` — filename stays `skim`
///     so the stem check (`stem == "skim"`) passes, but the directory differs,
///     so the canonical path comparison detects a mismatch.
///  2. Install wrappers using the COPY (so the symlinks point to copy_path).
///  3. Run doctor from the ORIGINAL binary: same version/commit, different
///     canonical path → wrapper target mismatch → exit 1.
///
/// Both init and doctor invocations route through `skim_sandboxed_with_bin`
/// (the single authoritative sandbox env-var block) to satisfy PF-017.
#[cfg(unix)]
#[test]
fn test_doctor_exits_1_on_wrapper_target_mismatch() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    std::fs::create_dir_all(home.join(".claude")).unwrap();

    let original_bin = common::skim_bin();
    // Place the copy in a subdirectory named `old-install` so the filename stays
    // `skim` (stem == "skim" passes the doctor stem check) but the canonical path
    // differs from the running binary → triggers "wrapper target mismatch".
    let old_install_dir = home.join("old-install");
    std::fs::create_dir_all(&old_install_dir).unwrap();
    let copy_path = old_install_dir.join("skim");
    std::fs::copy(&original_bin, &copy_path).expect("copying the test binary must succeed");
    {
        let mut perms = std::fs::metadata(&copy_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&copy_path, perms).unwrap();
    }

    // Install hook AND wrappers using the copy — symlinks will point to copy_path.
    common::skim_sandboxed_with_bin(home, &copy_path)
        .args([
            "init",
            "--agent",
            "claude-code",
            "--no-guidance",
            "--wrappers",
        ])
        .env("PATH", hermetic_path())
        .assert()
        .success();

    // Verify at least one wrapper symlink was created.
    let wrappers_dir = home.join(".skim").join("bin");
    assert!(
        wrappers_dir.exists(),
        "wrapper directory must exist after init --wrappers"
    );

    // Run doctor from the ORIGINAL binary: wrapper targets point to copy_path,
    // but we are running original_bin → mismatch → exit 1.
    common::skim_sandboxed(home)
        .arg("doctor")
        .current_dir(home)
        .env("PATH", hermetic_path())
        .assert()
        .failure() // exit 1 — wrapper target mismatch is drift
        .stdout(predicates::prelude::predicate::str::contains(
            "wrapper target mismatch",
        ));
}

/// When all wrapper symlinks point at the SAME binary that is running, `skim
/// doctor` must exit 0 and not report any wrapper drift.
///
/// This is the "wrapper section clean" regression: the new read_link-based
/// check must not produce false positives on a correct install.
#[cfg(unix)]
#[test]
fn test_doctor_exits_0_with_correct_wrappers() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    std::fs::create_dir_all(home.join(".claude")).unwrap();

    // Install hook AND wrappers using the same binary that will run doctor.
    common::skim_sandboxed(home)
        .args([
            "init",
            "--agent",
            "claude-code",
            "--no-guidance",
            "--wrappers",
        ])
        .env("PATH", hermetic_path())
        .assert()
        .success();

    // Doctor from the same binary: wrappers point to this binary → no mismatch.
    common::skim_sandboxed(home)
        .arg("doctor")
        .current_dir(home)
        .env("PATH", hermetic_path())
        .assert()
        .success(); // exit 0 — correct wrappers do not produce drift
}

/// A foreign symlink in the wrappers directory (target stem != "skim"/"rskim")
/// must be reported as an advisory "foreign symlink — not installed by skim"
/// and must NOT cause `skim doctor` to exit 1.
///
/// Invariant being tested: wrapper install/uninstall only ever touches symlinks
/// whose target stem is "skim" or "rskim". A foreign symlink is reported but
/// never modified or removed. `skim doctor` reports it as advisory (warning),
/// not as drift (does not increment the mismatch counter → exit 0).
///
/// This behaviour was discovered when `test_doctor_exits_1_on_wrapper_target_mismatch`
/// used `skim-copy` as the target name: doctor correctly left it alone and exited 0.
#[cfg(unix)]
#[test]
fn test_doctor_foreign_symlink_is_advisory_not_exit_1() {
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    std::fs::create_dir_all(home.join(".claude")).unwrap();

    // Install hook AND wrappers so the wrappers directory is created.
    common::skim_sandboxed(home)
        .args([
            "init",
            "--agent",
            "claude-code",
            "--no-guidance",
            "--wrappers",
        ])
        .env("PATH", hermetic_path())
        .assert()
        .success();

    let wrappers_dir = home.join(".skim").join("bin");
    assert!(
        wrappers_dir.exists(),
        "wrapper directory must exist after init --wrappers"
    );

    // Place a foreign symlink in the wrappers dir whose target stem is NOT
    // "skim" or "rskim". Points at a well-known binary that is guaranteed to
    // exist on Unix (/bin/sh) so the target exists, but it is not a skim binary.
    let foreign_link = wrappers_dir.join("not-a-skim-tool");
    symlink("/bin/sh", &foreign_link).expect("creating foreign symlink must succeed");
    assert!(
        foreign_link.exists() || std::fs::symlink_metadata(&foreign_link).is_ok(),
        "foreign symlink must be present before doctor runs"
    );

    // Run doctor: the foreign symlink must be flagged as advisory but must NOT
    // produce a mismatch count → exit 0.
    let out = common::skim_sandboxed(home)
        .arg("doctor")
        .current_dir(home)
        .env("PATH", hermetic_path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        out.status.success(),
        "skim doctor must exit 0 when only foreign symlinks are present, got:\n{stdout}"
    );
    assert!(
        stdout.contains("foreign symlink — not installed by skim"),
        "doctor must report the foreign symlink as advisory, got:\n{stdout}"
    );

    // The foreign symlink must NOT have been removed or modified.
    assert!(
        std::fs::symlink_metadata(&foreign_link).is_ok(),
        "doctor must not remove the foreign symlink"
    );
    let target = std::fs::read_link(&foreign_link).expect("foreign symlink must still be readable");
    assert_eq!(
        target,
        std::path::Path::new("/bin/sh"),
        "doctor must not modify the foreign symlink target"
    );
}
