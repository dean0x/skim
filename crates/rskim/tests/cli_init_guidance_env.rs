//! Guidance path and read-only entry-point tests with a temporary home and cwd.

use std::fs;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;

mod common;

fn command(home: &Path) -> assert_cmd::Command {
    let mut cmd = common::skim_sandboxed_with_bin(home, &common::skim_bin());
    cmd.timeout(Duration::from_secs(15));
    cmd
}

#[test]
fn empty_claude_config_folder_writes_guidance_in_the_home_and_not_the_cwd() {
    let home = TempDir::new().unwrap();
    let cwd = home.path().join("project");
    fs::create_dir(&cwd).unwrap();
    fs::create_dir(home.path().join(".claude")).unwrap();
    command(home.path())
        .env("CLAUDE_CONFIG_DIR", "")
        .current_dir(&cwd)
        .args(["init", "--agent", "claude-code", "--yes", "--no-wrappers"])
        .assert()
        .success();

    let guidance = fs::read_to_string(home.path().join(".claude/CLAUDE.md")).unwrap();
    assert!(guidance.contains("<!-- skim-start v"));
    assert!(home.path().join(".claude/hooks/skim-rewrite.sh").is_file());
    assert_eq!(fs::read_dir(&cwd).unwrap().count(), 0);
}

#[test]
fn relative_guidance_folder_stops_install_before_any_file_is_written() {
    let home = TempDir::new().unwrap();
    let cwd = home.path().join("project");
    let config = home.path().join(".codex");
    fs::create_dir(&cwd).unwrap();
    fs::create_dir(&config).unwrap();
    command(home.path())
        .env("CODEX_HOME", "relative-guidance")
        .current_dir(&cwd)
        .args(["init", "--agent", "codex", "--yes", "--no-wrappers"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("CODEX_HOME"));

    assert_eq!(fs::read_dir(&cwd).unwrap().count(), 0);
    assert_eq!(fs::read_dir(&config).unwrap().count(), 0);
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 2);
}

#[test]
fn relative_guidance_folder_stops_uninstall_before_the_hook_is_removed() {
    let home = TempDir::new().unwrap();
    let cwd = home.path().join("project");
    fs::create_dir(&cwd).unwrap();
    fs::create_dir(home.path().join(".codex")).unwrap();
    command(home.path())
        .args(["init", "--agent", "codex", "--yes", "--no-wrappers"])
        .assert()
        .success();
    let paths = [
        ".codex/AGENTS.md",
        ".codex/settings.json",
        ".codex/hooks/skim-rewrite.sh",
    ];
    let before: Vec<_> = paths
        .iter()
        .map(|path| fs::read(home.path().join(path)).unwrap())
        .collect();
    command(home.path())
        .env("CODEX_HOME", "relative-guidance")
        .current_dir(&cwd)
        .args(["init", "--uninstall", "--agent", "codex", "--yes"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("CODEX_HOME"));

    for (path, bytes) in paths.iter().zip(before) {
        assert_eq!(fs::read(home.path().join(path)).unwrap(), bytes, "{path}");
    }
    assert_eq!(fs::read_dir(&cwd).unwrap().count(), 0);
}

#[test]
fn non_install_entry_points_leave_stale_instruction_files_byte_unchanged() {
    let home = TempDir::new().unwrap();
    let project = home.path().join("project");
    fs::create_dir(&project).unwrap();
    let paths = [
        ".claude/CLAUDE.md",
        ".codex/AGENTS.md",
        ".gemini/GEMINI.md",
        ".copilot/copilot-instructions.md",
        ".crush/AGENTS.md",
        ".kiro/steering/skim.md",
        ".roo/rules/skim.md",
        ".cline/rules/skim.md",
        ".augment/rules/skim.md",
        ".devin/rules/skim.md",
        "project/CLAUDE.md",
        "project/AGENTS.md",
        "project/GEMINI.md",
        "project/.github/copilot-instructions.md",
        "project/.cursor/rules/skim.mdc",
        "project/.cursorrules",
    ];
    let stale = b"user text\n<!-- skim-start v0.0.1 -->\nstale guidance\n<!-- skim-end -->\n";
    for path in paths {
        let path = home.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, stale).unwrap();
    }

    command(home.path())
        .current_dir(&project)
        .args(["rewrite", "--hook", "--agent", "claude-code"])
        .write_stdin(
            serde_json::json!({
                "hook_event_name": "PreToolUse",
                "tool_name": "Bash",
                "tool_input": {"command": "git status"}
            })
            .to_string(),
        )
        .assert()
        .success()
        .stdout(predicates::str::contains("updatedInput"));
    for path in paths {
        assert_eq!(
            fs::read(home.path().join(path)).unwrap(),
            stale,
            "rewrite: {path}"
        );
    }

    let wrapper = home.path().join("git");
    std::os::unix::fs::symlink(common::skim_bin(), &wrapper).unwrap();
    common::skim_sandboxed_with_bin(home.path(), &wrapper)
        .current_dir(&project)
        .timeout(Duration::from_secs(15))
        .arg("--version")
        .assert()
        .success()
        .stdout(predicates::str::contains("git version"));
    for path in paths {
        assert_eq!(
            fs::read(home.path().join(path)).unwrap(),
            stale,
            "wrapper: {path}"
        );
    }

    command(home.path())
        .current_dir(&project)
        .args(["rewrite", "--hook", "--agent", "claude-code"])
        .write_stdin(include_str!("fixtures/hooks/post_tool_use_plain.json"))
        .assert()
        .success()
        .stdout(predicates::str::contains("updatedToolOutput"));
    for path in paths {
        assert_eq!(
            fs::read(home.path().join(path)).unwrap(),
            stale,
            "post-hook: {path}"
        );
    }

    command(home.path())
        .current_dir(&project)
        .env("PATH", common::hermetic_path())
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicates::str::contains("stale"));
    for path in paths {
        assert_eq!(
            fs::read(home.path().join(path)).unwrap(),
            stale,
            "doctor: {path}"
        );
    }
}
