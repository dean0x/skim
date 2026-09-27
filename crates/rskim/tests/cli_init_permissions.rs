//! Integration tests for `skim init --permissions`.
//!
//! ## TTY constraint
//!
//! The test harness stdin is NEVER a TTY (pipe/subprocess stdin). This IS the
//! test: `confirm_grant` must refuse silently on non-TTY, so ALL integration
//! paths exercise the refusal branch. The consent-YES path is covered by unit
//! tests in `cmd/init/helpers.rs` and `cmd/permissions/` only.
//!
//! ## Isolation
//!
//! Every invocation here runs a REAL install, so every invocation goes through
//! [`common::skim_sandboxed`] — the single authoritative sandbox env block
//! (PF-017).  A per-agent `*_CONFIG_DIR` override alone does not isolate an
//! install: it names one directory and leaves `HOME` — and therefore
//! `~/.skim/bin`, `~/.cache/skim`, every *other* agent's config directory, and
//! the guidance files under it — resolving to the developer's own.  The working
//! directory is part of the sandbox for the same reason: a successful install
//! ends in `install_search_integration`, which walks up from the process cwd and
//! writes `post-commit`/`post-merge`/`post-checkout` into the first repository it
//! finds.  Left at the cwd cargo supplies, that is this clone.
//!
//! The sandbox already points every `*_CONFIG_DIR` inside that home, so these
//! tests name the directory an assertion is about with [`agent_config_dir`]
//! rather than re-setting the variable by hand.  A hand-rolled override of a
//! variable the sandbox owns is the shape `cli_init.rs`'s own guard rejects —
//! it is how PF-017 was re-opened the second time — and it is unnecessary here:
//! resolving the path through `common::SANDBOX_REDIRECTED_VARS` keeps the table
//! the single place the mapping is written down.

use predicates::prelude::*;
use tempfile::TempDir;
mod common;

/// The directory the sandbox points `var` at, created so it exists.
///
/// Creation is not incidental.  `detect_installed_agents` counts an agent only
/// when its override "points to an existing directory", so an uncreated path
/// reads as "that agent is not installed" — the difference between exercising
/// the fan-out and exercising nothing.  Resolving through the sandbox table
/// (rather than joining a literal) means the install writes where the sandbox
/// says it writes, with no second copy of the mapping to drift.
/// A directory inside the sandbox home that reads as a git project root.
///
/// `find_git_root_from_cwd` walks ancestors for a path with a `.git` entry and
/// returns the first hit, so an empty `.git` directory satisfies it — no git
/// invocation, and nothing outside the sandbox is reachable.
///
/// Copilot needs this: the I-25 pre-check returns before `confirm_grant` when
/// the cwd is not in a repository, so a Copilot consent test run from the bare
/// sandbox home would assert "nothing was written" about the git-root skip
/// instead of about the TTY gate it names.
fn git_project_dir(home: &TempDir) -> std::path::PathBuf {
    let project = home.path().join("project");
    std::fs::create_dir_all(project.join(".git")).expect("project .git must be creatable");
    project
}

fn agent_config_dir(home: &TempDir, var: &str) -> std::path::PathBuf {
    let relative = common::SANDBOX_REDIRECTED_VARS
        .iter()
        .find(|(name, _)| *name == var)
        .map(|(_, relative)| *relative)
        .unwrap_or_else(|| panic!("{var} is not a sandbox-redirected variable"));
    let dir = common::sandbox_var_path(home.path(), relative);
    std::fs::create_dir_all(&dir).expect("agent config dir must be creatable");
    dir
}

// ============================================================================
// Non-TTY refusal: --permissions without a TTY must not write anything
// ============================================================================

/// Core non-TTY contract: `skim init --agent claude --permissions` on a
/// non-interactive stdin must exit 0 and write NO sidecar.
///
/// The TTY gate in `confirm_grant` is the primary defense against prompt-injected
/// self-grant. This test verifies it fires on the integration path.
#[test]
fn test_permissions_non_tty_writes_nothing_claude() {
    let home = TempDir::new().unwrap();
    let cfg = agent_config_dir(&home, "CLAUDE_CONFIG_DIR");

    common::skim_sandboxed(home.path())
        .args(["init", "--agent", "claude", "--permissions"])
        .assert()
        .success();

    // No sidecar must be written.
    assert!(
        !cfg.join("skim-permissions.json").exists(),
        "skim-permissions.json must NOT be written when stdin is not a TTY"
    );
    // settings.json must not gain a permissions.allow array.
    let settings_path = cfg.join("settings.json");
    if settings_path.exists() {
        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        let allow = content
            .get("permissions")
            .and_then(|p| p.get("allow"))
            .and_then(|a| a.as_array());
        let has_skim_entries = allow.is_some_and(|arr| {
            arr.iter()
                .any(|e| e.as_str().is_some_and(|s| s.starts_with("Bash(skim ")))
        });
        assert!(
            !has_skim_entries,
            "settings.json must not contain Bash(skim …) entries on non-TTY install"
        );
    }
}

/// Non-TTY refusal applies for Gemini CLI too.
#[test]
fn test_permissions_non_tty_writes_nothing_gemini() {
    let home = TempDir::new().unwrap();
    let cfg = agent_config_dir(&home, "GEMINI_CONFIG_DIR");

    common::skim_sandboxed(home.path())
        .args([
            "init",
            "--agent",
            "gemini",
            "--permissions",
            "--no-guidance",
        ])
        .assert()
        .success();

    assert!(
        !cfg.join("skim-permissions.json").exists(),
        "skim-permissions.json must NOT be written for Gemini on non-TTY"
    );
}

/// Non-TTY refusal applies for Copilot CLI too.
#[test]
fn test_permissions_non_tty_writes_nothing_copilot() {
    let home = TempDir::new().unwrap();
    let cfg = agent_config_dir(&home, "COPILOT_CONFIG_DIR");
    // Run from a git project (inside the sandbox) so the I-25 git-root
    // pre-check passes and the invocation actually reaches the consent gate
    // this test is about. From the bare sandbox home it would return earlier,
    // and the assertion below would hold for the wrong reason.
    let project = git_project_dir(&home);

    common::skim_sandboxed(home.path())
        .args([
            "init",
            "--agent",
            "copilot",
            "--permissions",
            "--no-guidance",
        ])
        .current_dir(&project)
        .assert()
        .success();

    assert!(
        !cfg.join("skim-permissions.json").exists(),
        "skim-permissions.json must NOT be written for Copilot on non-TTY"
    );
    assert!(
        !cfg.join("permissions-config.json").exists(),
        "permissions-config.json must NOT be written for Copilot on non-TTY"
    );
}

// ============================================================================
// --dry-run: must enumerate 8 entries for Claude, not write anything
// ============================================================================

/// `skim init --agent claude --permissions --dry-run` must:
/// - Exit 0.
/// - Print all 8 `Bash(skim <tool>:*)` entries (F1: dry-run bypasses consent).
/// - Write NO sidecar or any other file.
#[test]
fn test_permissions_dry_run_enumerates_8_claude_entries() {
    let home = TempDir::new().unwrap();
    let cfg = agent_config_dir(&home, "CLAUDE_CONFIG_DIR");

    let out = common::skim_sandboxed(home.path())
        .args(["init", "--agent", "claude", "--permissions", "--dry-run"])
        .output()
        .expect("skim must run");

    assert!(out.status.success(), "exit 0 on dry-run");

    // No sidecar written on dry-run.
    assert!(
        !cfg.join("skim-permissions.json").exists(),
        "dry-run must not write a sidecar"
    );
    // No settings.json written on dry-run (only the hook dry-run output appears).
    // (settings.json may be absent — the dry-run never writes it.)

    let stdout = String::from_utf8_lossy(&out.stdout);

    // Dry-run must mention all 8 Bash(skim <tool>:*) entries regardless of TTY.
    // Consent is not required to DISPLAY what would happen; the line annotates
    // that real-install consent would still be required.
    for tool in &["df", "diff", "du", "grep", "ls", "rg", "tree", "wc"] {
        let entry = format!("Bash(skim {tool}:*)");
        assert!(
            stdout.contains(&entry),
            "dry-run must enumerate '{entry}' in stdout, got:\n{stdout}"
        );
    }

    // Sanity: the consent-annotation line must be present.
    assert!(
        stdout.contains("consent required at install"),
        "dry-run must annotate that consent is required at install, got:\n{stdout}"
    );

    // Sanity: [dry-run] prefix present and no files written beyond temp dir.
    assert!(
        stdout.contains("[dry-run]"),
        "dry-run must produce [dry-run] output, got:\n{stdout}"
    );
}

// ============================================================================
// F2/F3 — explicit --permissions refused on non-TTY must print a loud notice
// ============================================================================

/// `echo "" | skim init --permissions --agent claude` (non-interactive stdin):
/// - Must exit 0 (no error).
/// - Must write NO sidecar.
/// - Must print a loud notice that --permissions was requested but not granted.
///
/// The notice must state (a) requested-but-not-granted, (b) non-interactive
/// cause, and (c) interactive re-run remedy.
#[test]
fn test_permissions_non_tty_explicit_request_prints_notice() {
    let home = TempDir::new().unwrap();
    let cfg = agent_config_dir(&home, "CLAUDE_CONFIG_DIR");

    let out = common::skim_sandboxed(home.path())
        .args(["init", "--agent", "claude", "--permissions"])
        .output()
        .expect("skim must run");

    // Must succeed.
    assert!(
        out.status.success(),
        "must exit 0 even when consent is refused"
    );

    // Must not write sidecar.
    assert!(
        !cfg.join("skim-permissions.json").exists(),
        "skim-permissions.json must NOT be written when non-TTY refuses consent"
    );

    let stdout = String::from_utf8_lossy(&out.stdout);

    // The notice must be on stdout so it appears in agent context.
    assert!(
        stdout.contains("--permissions was requested but not granted"),
        "stdout must mention '--permissions was requested but not granted', got:\n{stdout}"
    );
    assert!(
        stdout.contains("non-interactive"),
        "notice must mention non-interactive cause, got:\n{stdout}"
    );
    assert!(
        stdout.contains("interactively"),
        "notice must mention interactive re-run remedy, got:\n{stdout}"
    );
}

/// `echo "" | skim init --permissions --agent claude --yes` (non-interactive + --yes):
/// Same as above — --yes cannot bypass consent.
#[test]
fn test_permissions_non_tty_yes_flag_prints_notice() {
    let home = TempDir::new().unwrap();
    let cfg = agent_config_dir(&home, "CLAUDE_CONFIG_DIR");

    let out = common::skim_sandboxed(home.path())
        .args(["init", "--agent", "claude", "--permissions", "--yes"])
        .output()
        .expect("skim must run");

    // Must succeed.
    assert!(
        out.status.success(),
        "must exit 0 even when consent is refused"
    );

    // Must not write sidecar.
    assert!(
        !cfg.join("skim-permissions.json").exists(),
        "skim-permissions.json must NOT be written when --yes refuses consent non-interactively"
    );

    let stdout = String::from_utf8_lossy(&out.stdout);

    // Same notice must appear — --yes is never sufficient to grant permissions.
    assert!(
        stdout.contains("--permissions was requested but not granted"),
        "stdout must mention '--permissions was requested but not granted', got:\n{stdout}"
    );
    assert!(
        stdout.contains("non-interactive"),
        "notice must mention non-interactive cause, got:\n{stdout}"
    );
    assert!(
        stdout.contains("interactively"),
        "notice must mention interactive re-run remedy, got:\n{stdout}"
    );
}

// ============================================================================
// --no-permissions: explicit opt-out
// ============================================================================

#[test]
fn test_no_permissions_flag_skips_seeding() {
    let home = TempDir::new().unwrap();
    let cfg = agent_config_dir(&home, "CLAUDE_CONFIG_DIR");

    common::skim_sandboxed(home.path())
        .args(["init", "--agent", "claude", "--no-permissions"])
        .assert()
        .success();

    assert!(
        !cfg.join("skim-permissions.json").exists(),
        "--no-permissions must not write a sidecar"
    );
}

// ============================================================================
// --permissions + --project: must conflict
// ============================================================================

#[test]
fn test_permissions_and_project_flags_conflict() {
    // No agent config directory: the conflict is rejected while parsing flags,
    // before anything is written, so there is nothing to assert about a
    // directory.  The sandbox stays because it is what contains the damage if
    // that rejection ever regresses — the install would otherwise proceed.
    let home = TempDir::new().unwrap();

    common::skim_sandboxed(home.path())
        .args(["init", "--agent", "claude", "--permissions", "--project"])
        .assert()
        .failure()
        .stderr(
            predicates::str::contains("--permissions").and(predicates::str::contains("--project")),
        );
}

// ============================================================================
// Boundary contract: READ_ONLY_SUBCOMMANDS must not appear in rewrite/dispatch
// ============================================================================

/// `READ_ONLY_SUBCOMMANDS` is an install-time-only registry.
///
/// It must NEVER be imported or referenced from the rewrite or dispatch
/// paths — doing so would make runtime behavior depend on the install-time
/// tool list, which must remain independent.
///
/// If this test fails: a new import of `READ_ONLY_SUBCOMMANDS` was added to
/// a rewrite or dispatch source file. Move the logic to `cmd/permissions/` or
/// `cmd/init/` instead.
#[test]
fn contract_read_only_subcommands_absent_from_rewrite_dispatch() {
    let rewrite_sources = [
        include_str!("../src/cmd/rewrite/mod.rs"),
        include_str!("../src/cmd/rewrite/rules.rs"),
        include_str!("../src/cmd/rewrite/hook.rs"),
        include_str!("../src/cmd/rewrite/handlers.rs"),
        include_str!("../src/cmd/rewrite/engine.rs"),
        include_str!("../src/cmd/rewrite/compound.rs"),
        include_str!("../src/cmd/rewrite/types.rs"),
        include_str!("../src/cmd/dispatch.rs"),
    ];
    let file_names = [
        "cmd/rewrite/mod.rs",
        "cmd/rewrite/rules.rs",
        "cmd/rewrite/hook.rs",
        "cmd/rewrite/handlers.rs",
        // engine.rs holds `try_rewrite` (the PF-004 rewrite-engine surface) and
        // compound.rs holds `try_rewrite_compound` — the two central text-transform
        // entry points the install-time-only registry must never leak into.
        "cmd/rewrite/engine.rs",
        "cmd/rewrite/compound.rs",
        "cmd/rewrite/types.rs",
        "cmd/dispatch.rs",
    ];
    for (src, name) in rewrite_sources.iter().zip(file_names.iter()) {
        assert!(
            !src.contains("READ_ONLY_SUBCOMMANDS"),
            "`READ_ONLY_SUBCOMMANDS` must not appear in `{name}` \
             (install-time registry; forbidden in rewrite/dispatch paths). \
             Move the logic to cmd/permissions/ or cmd/init/ instead."
        );
    }
}

// ============================================================================
// Codex exclusion from auto-detect permissions fan-out
// ============================================================================

// ============================================================================
// Mirror tier: empty mirror proposals must not prompt and must print a notice
// ============================================================================

/// `skim init --agent claude --permissions --permissions-tier mirror` with no
/// existing allow-list entries: must exit 0, print the "nothing to seed" notice,
/// and write nothing.
///
/// Pins I-24: the empty-mirror path (a) does not prompt `confirm_grant`,
/// (b) writes no sidecar and no config change, and (c) prints a notice instead
/// of silently returning `Ok(true)` (fabricated consent).
#[test]
fn test_permissions_mirror_empty_proposals_no_prompt_no_write() {
    let home = TempDir::new().unwrap();
    // Empty dir — no allow-list entries to mirror.
    let cfg = agent_config_dir(&home, "CLAUDE_CONFIG_DIR");

    let out = common::skim_sandboxed(home.path())
        .args([
            "init",
            "--agent",
            "claude",
            "--permissions",
            "--permissions-tier",
            "mirror",
        ])
        .output()
        .expect("skim must run");

    assert!(
        out.status.success(),
        "must exit 0 when no entries to mirror"
    );

    // (b) No sidecar must be written.
    assert!(
        !cfg.join("skim-permissions.json").exists(),
        "skim-permissions.json must NOT be written when mirror proposals are empty"
    );

    // (b) No permissions entries in settings.json.
    let settings_path = cfg.join("settings.json");
    if settings_path.exists() {
        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        let has_skim = content
            .get("permissions")
            .and_then(|p| p.get("allow"))
            .and_then(|a| a.as_array())
            .is_some_and(|arr| {
                arr.iter()
                    .any(|e| e.as_str().is_some_and(|s| s.starts_with("Bash(skim ")))
            });
        assert!(
            !has_skim,
            "settings.json must not contain Bash(skim \u{2026}) entries on empty mirror"
        );
    }

    let stdout = String::from_utf8_lossy(&out.stdout);

    // (c) The "nothing to seed" notice must appear.
    assert!(
        stdout.contains("No existing allow-list entries eligible to mirror"),
        "must print 'nothing to seed' notice, got:\n{stdout}"
    );
}

// ============================================================================
// Copilot non-git directory: permissions skip with loud notice (I-25)
// ============================================================================

/// `skim init --agent copilot --permissions --no-guidance` from a non-git CWD:
/// must exit 0 (install completes fully), print a skip notice, and write no sidecar.
///
/// Pins I-25: the git-root pre-check fires before `confirm_grant` so consent
/// is never taken for an impossible seed and no partial install state is left
/// (the hook + settings registration still complete).
#[test]
fn test_permissions_copilot_non_git_skip() {
    let home = TempDir::new().unwrap();
    let copilot_cfg = agent_config_dir(&home, "COPILOT_CONFIG_DIR");
    let non_git_dir = TempDir::new().unwrap(); // not inside any git repository

    let out = common::skim_sandboxed(home.path())
        .args([
            "init",
            "--agent",
            "copilot",
            "--permissions",
            "--no-guidance",
        ])
        // Chained AFTER the sandbox, so it wins.  Kept explicit because the
        // non-git cwd is this test's subject, not merely its isolation — the
        // sandbox home would satisfy the precondition silently.
        .current_dir(non_git_dir.path())
        .output()
        .expect("skim must run");

    assert!(
        out.status.success(),
        "install must complete successfully outside a git repo"
    );

    // No sidecar written.
    assert!(
        !copilot_cfg.join("skim-permissions.json").exists(),
        "skim-permissions.json must NOT be written outside a git repo"
    );

    let stdout = String::from_utf8_lossy(&out.stdout);

    // The skip notice must appear.
    assert!(
        stdout.contains("Copilot permissions seeding skipped"),
        "skip notice must appear when not in a git repo, got:\n{stdout}"
    );
    assert!(
        stdout.contains("git repository"),
        "skip notice must mention git repository, got:\n{stdout}"
    );
}

/// In auto-detect mode (`skim init --permissions` without `--agent`), Codex CLI
/// must be excluded from the permissions fan-out even when Codex is detected.
///
/// This is verified at the unit-test level in `install.rs::test_codex_excluded_from_auto_detect_permissions_fan_out`.
/// This integration test verifies that auto-detect with a Codex override exits
/// successfully (it silently excludes Codex, not an error).
#[test]
fn test_permissions_auto_detect_with_codex_override_succeeds() {
    let home = TempDir::new().unwrap();
    let cfg = agent_config_dir(&home, "CODEX_CONFIG_DIR");

    // Run with --permissions and only the Codex config directory created, so
    // Codex is the one agent that is "detected".  It must then be silently
    // excluded from the permissions fan-out.
    //
    // This is the site with the largest blast radius in the file and the reason
    // the sandbox is not optional here: with no `--agent`, the install fans out
    // over every agent auto-detect returns.  `detect_installed_agents` restricts
    // that fan-out to overridden directories that EXIST, and the sandbox points
    // every one of those overrides inside this TempDir — so the detected set is
    // {Codex} by construction, rather than whatever the machine running the
    // suite happens to have installed.
    common::skim_sandboxed(home.path())
        .args(["init", "--permissions", "--no-guidance"])
        .assert()
        .success();

    // No permissions sidecar must be written (Codex excluded + non-TTY consent).
    assert!(
        !cfg.join("skim-permissions.json").exists(),
        "skim-permissions.json must not be written for Codex in auto-detect mode"
    );
}
