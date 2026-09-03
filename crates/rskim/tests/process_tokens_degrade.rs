//! Regression: extensionless shebang scripts behave the same with and without
//! a token budget.
//!
//! Guards the branch-asymmetry bug where `run_transform`'s `Some(token_budget)`
//! arm lacked the shebang detection + ADR-002 degrade that the `None` arm
//! already had. The result was that the *same* extensionless bash script gave
//! opposite outcomes: `skim ./deploy` → exit 0 (transformed), but
//! `skim ./deploy --tokens=500` → exit 3 (UnsupportedLanguage), because the
//! budget branch defaulted to TypeScript and `transform_auto_with_config`
//! errored on the unknown extension.
//!
//! Both invocations must now exit 0. A truly unknown file (no extension, no
//! shebang) must degrade to a lossless passthrough under a token budget rather
//! than erroring.

use tempfile::TempDir;

mod common;

fn skim_cmd() -> assert_cmd::Command {
    let mut cmd = common::skim();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd
}

/// An extensionless script whose language is only knowable from its `#!` line.
const BASH_SHEBANG_SCRIPT: &str = "#!/bin/bash\n\
     set -euo pipefail\n\
     deploy() {\n\
     \x20 echo \"deploying\"\n\
     }\n\
     deploy \"$@\"\n";

#[test]
fn extensionless_shebang_script_transforms_without_token_budget() {
    // Baseline: the `None` branch already handles this (documented working case).
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("deploy"); // no extension
    std::fs::write(&file, BASH_SHEBANG_SCRIPT).unwrap();

    skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--no-cache")
        .assert()
        .success();
}

#[test]
fn extensionless_shebang_script_transforms_with_token_budget() {
    // The regression: same file + --tokens=500 previously exited 3
    // (UnsupportedLanguage). It must now resolve Bash via the shebang and exit 0.
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("deploy"); // no extension
    std::fs::write(&file, BASH_SHEBANG_SCRIPT).unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--tokens")
        .arg("500")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "extensionless shebang script with --tokens must exit 0, stderr: {:?}",
        String::from_utf8_lossy(&output.stderr),
    );
    // No misleading "defaulting to TypeScript" warning should fire.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("defaulting to TypeScript"),
        "shebang file must not hit the TypeScript-default warning path: {stderr:?}",
    );
}

#[test]
fn extensionless_unknown_file_degrades_with_token_budget() {
    // No extension AND no shebang → no detectable language. The Some(budget)
    // branch must degrade to a lossless passthrough (ADR-002), exit 0, and emit
    // the full content — never error with UnsupportedLanguage.
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("notes"); // no extension, no shebang
    let content = "just some plain notes\nwith a couple of lines\nand no language\n";
    std::fs::write(&file, content).unwrap();

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--tokens")
        .arg("500")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "unknown-language file with --tokens must degrade (exit 0), stderr: {:?}",
        String::from_utf8_lossy(&output.stderr),
    );
    // Lossless passthrough: the original content survives.
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("just some plain notes"),
        "degraded passthrough must preserve raw content, got: {stdout:?}",
    );
}
