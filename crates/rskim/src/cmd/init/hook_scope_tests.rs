//! Tests for project hook paths. Each case uses a temporary root.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;

use tempfile::TempDir;

use super::{
    CURSOR_LEGACY_SETTINGS, HookTarget, PathRefusal, ProjectHookScope, project_hook_targets,
};
use crate::cmd::init::DetectionEnv;
use crate::cmd::session::AgentKind::{
    self, ClaudeCode, CodexCli, CopilotCli, Crush, Cursor, GeminiCli,
};

fn assert_targets(agent: AgentKind, expected: &[HookTarget]) {
    assert_eq!(project_hook_targets(agent), expected);
}

#[test]
fn claude_targets_cover_hooks_and_permissions() {
    use HookTarget::{Folder, Owned, UserFile};
    assert_targets(
        ClaudeCode,
        &[
            Folder(".claude".into()),
            Folder(".claude/hooks".into()),
            Owned(".claude/hooks/skim-rewrite.sh".into()),
            Owned(".claude/hooks/skim-claude-code.sha256".into()),
            UserFile(".claude/settings.json".into()),
            Owned(".claude/settings.json.bak".into()),
            Owned(".claude/skim-permissions.json".into()),
        ],
    );
}

#[test]
fn codex_targets_cover_hooks_and_rules() {
    use HookTarget::{Folder, Owned, UserFile};
    assert_targets(
        CodexCli,
        &[
            Folder(".codex".into()),
            Folder(".codex/hooks".into()),
            Owned(".codex/hooks/skim-rewrite.sh".into()),
            Owned(".codex/hooks/skim-codex.sha256".into()),
            UserFile(".codex/settings.json".into()),
            Owned(".codex/settings.json.bak".into()),
            Owned(".codex/skim-permissions.json".into()),
            Folder(".codex/rules".into()),
            Owned(".codex/rules/skim.rules".into()),
        ],
    );
}

#[test]
fn copilot_targets_cover_dedicated_hooks_and_permissions() {
    use HookTarget::{Folder, Owned, UserFile};
    assert_targets(
        CopilotCli,
        &[
            Folder(".github".into()),
            Folder(".github/hooks".into()),
            Owned(".github/hooks/skim-rewrite.sh".into()),
            Owned(".github/hooks/skim-copilot.sha256".into()),
            Owned(".github/hooks/skim.json".into()),
            Owned(".github/skim-permissions.json".into()),
            UserFile(".github/permissions-config.json".into()),
            Owned(".github/permissions-config.json.bak".into()),
        ],
    );
}

#[test]
fn crush_targets_cover_hooks_and_config() {
    use HookTarget::{Folder, Owned, UserFile};
    assert_targets(
        Crush,
        &[
            Folder(".crush".into()),
            Folder(".crush/hooks".into()),
            Owned(".crush/hooks/skim-rewrite.sh".into()),
            Owned(".crush/hooks/skim-crush.sha256".into()),
            UserFile(".crush/crush.json".into()),
            Owned(".crush/crush.json.bak".into()),
        ],
    );
}

#[test]
fn cursor_targets_cover_hooks_and_legacy_config() {
    use HookTarget::{Folder, Owned, UserFile};
    assert_targets(
        Cursor,
        &[
            Folder(".cursor".into()),
            Folder(".cursor/hooks".into()),
            Owned(".cursor/hooks/skim-rewrite.sh".into()),
            Owned(".cursor/hooks/skim-cursor.sha256".into()),
            UserFile(".cursor/hooks.json".into()),
            Owned(".cursor/hooks.json.bak".into()),
            UserFile(".cursor/settings.json".into()),
        ],
    );
}

#[test]
fn gemini_targets_cover_hooks_and_policies() {
    use HookTarget::{Folder, Owned, UserFile};
    assert_targets(
        GeminiCli,
        &[
            Folder(".gemini".into()),
            Folder(".gemini/hooks".into()),
            Owned(".gemini/hooks/skim-rewrite.sh".into()),
            Owned(".gemini/hooks/skim-gemini.sha256".into()),
            UserFile(".gemini/settings.json".into()),
            Owned(".gemini/settings.json.bak".into()),
            Owned(".gemini/skim-permissions.json".into()),
            Folder(".gemini/policies".into()),
            Owned(".gemini/policies/skim.toml".into()),
        ],
    );
}

#[test]
fn an_empty_root_passes_without_any_creation() {
    let root = TempDir::new().unwrap();
    let canonical = root.path().canonicalize().unwrap();
    for &agent in AgentKind::all_supported() {
        let scope = ProjectHookScope::check(agent, root.path()).unwrap();
        let settings = match agent {
            CopilotCli => None,
            Cursor => Some(canonical.join(".cursor/hooks.json")),
            Crush => Some(canonical.join(".crush/crush.json")),
            _ => Some(canonical.join(agent.dot_dir_name()).join("settings.json")),
        };
        let legacy = (agent == Cursor).then(|| canonical.join(".cursor/settings.json"));
        assert_eq!(scope.settings_path(), settings.as_deref());
        assert_eq!(scope.cursor_legacy_path(), legacy.as_deref());
    }
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn a_folder_at_the_config_name_is_refused_before_install() {
    let root = TempDir::new().unwrap();
    let config = Path::new(".cursor/hooks.json");
    fs::create_dir_all(root.path().join(config)).unwrap();
    let refusal = ProjectHookScope::check(Cursor, root.path()).unwrap_err();
    assert!(matches!(refusal, PathRefusal::Unchecked { path, .. } if path == config));
}

#[test]
fn a_file_at_a_required_folder_is_refused() {
    let root = TempDir::new().unwrap();
    let folder = Path::new(".gemini/policies");
    fs::create_dir(root.path().join(".gemini")).unwrap();
    fs::write(root.path().join(folder), "user data").unwrap();
    let refusal = ProjectHookScope::check(GeminiCli, root.path()).unwrap_err();
    assert!(matches!(refusal, PathRefusal::Unchecked { path, .. } if path == folder));
}

#[test]
fn a_missing_root_is_refused() {
    let root = TempDir::new().unwrap();
    assert!(matches!(
        ProjectHookScope::check(Cursor, &root.path().join("missing")),
        Err(PathRefusal::Unchecked { path, .. }) if path == Path::new(".")
    ));
}

#[cfg(unix)]
mod links {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn an_outside_agent_folder_is_refused_by_relative_name_only() {
        let root = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        symlink(outside.path(), root.path().join(".cursor")).unwrap();
        let refusal = ProjectHookScope::check(Cursor, root.path()).unwrap_err();
        assert_eq!(
            refusal.to_string(),
            ".cursor leads outside this project folder. skim leaves it unchanged."
        );
    }

    #[test]
    fn nested_writer_folders_that_lead_outside_are_refused() {
        let outside = TempDir::new().unwrap();
        for (agent, folder) in [
            (CopilotCli, "hooks"),
            (GeminiCli, "policies"),
            (CodexCli, "rules"),
        ] {
            let root = TempDir::new().unwrap();
            let rel = Path::new(agent.dot_dir_name()).join(folder);
            fs::create_dir(root.path().join(agent.dot_dir_name())).unwrap();
            symlink(outside.path(), root.path().join(&rel)).unwrap();
            assert!(matches!(
                ProjectHookScope::check(agent, root.path()),
                Err(PathRefusal::LeavesProject { path }) if path == rel
            ));
        }
    }

    #[test]
    fn a_dangling_agent_folder_is_refused() {
        let root = TempDir::new().unwrap();
        symlink("not-there", root.path().join(".cursor")).unwrap();
        assert!(matches!(
            ProjectHookScope::check(Cursor, root.path()),
            Err(PathRefusal::Unchecked { path, .. }) if path == Path::new(".cursor")
        ));
        assert!(!root.path().join("not-there").exists());
    }

    #[test]
    fn inside_folder_and_config_links_pass_and_file_targets_are_canonical() {
        let root = TempDir::new().unwrap();
        let real = root.path().join("real");
        fs::create_dir(&real).unwrap();
        symlink("real", root.path().join(".cursor")).unwrap();
        for (config, target) in [
            ("hooks.json", "hook-target"),
            ("settings.json", "old-target"),
        ] {
            fs::write(real.join(target), "{}").unwrap();
            symlink(target, real.join(config)).unwrap();
        }
        let scope = ProjectHookScope::check(Cursor, root.path()).unwrap();
        let real = real.canonicalize().unwrap();
        assert_eq!(
            scope.settings_path(),
            Some(real.join("hook-target").as_path())
        );
        assert_eq!(
            scope.cursor_legacy_path(),
            Some(real.join("old-target").as_path())
        );
    }

    #[test]
    fn a_link_at_an_owned_name_is_not_followed() {
        let root = TempDir::new().unwrap();
        let hooks = root.path().join(".cursor/hooks");
        fs::create_dir_all(&hooks).unwrap();
        let outside = TempDir::new().unwrap();
        let target = outside.path().join("missing");
        let script = hooks.join("skim-rewrite.sh");
        symlink(&target, &script).unwrap();
        ProjectHookScope::check(Cursor, root.path()).unwrap();
        assert_eq!(fs::read_link(script).unwrap(), target);
        assert!(!target.exists());
    }

    #[test]
    fn a_user_config_link_that_ends_outside_is_refused() {
        let root = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let rel = Path::new(".github/permissions-config.json");
        fs::create_dir(root.path().join(".github")).unwrap();
        let target = outside.path().join("config");
        fs::write(&target, "{}").unwrap();
        symlink(&target, root.path().join(rel)).unwrap();
        assert!(matches!(
            ProjectHookScope::check(CopilotCli, root.path()),
            Err(PathRefusal::LeavesProject { path }) if path == rel
        ));
    }

    #[test]
    fn a_user_config_link_to_a_missing_file_or_folder_is_refused() {
        let root = TempDir::new().unwrap();
        let dir = root.path().join(".cursor");
        fs::create_dir(&dir).unwrap();
        let rel = Path::new(".cursor/hooks.json");
        for target in ["missing", "."] {
            symlink(target, root.path().join(rel)).unwrap();
            assert!(matches!(
                ProjectHookScope::check(Cursor, root.path()),
                Err(PathRefusal::NotRegular { path }) if path == rel
            ));
            fs::remove_file(root.path().join(rel)).unwrap();
        }
    }

    #[test]
    fn a_user_config_link_to_another_link_is_refused() {
        let root = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let dir = root.path().join(".claude");
        fs::create_dir(&dir).unwrap();
        let file = dir.join("real.json");
        fs::write(&file, "{}").unwrap();
        let rel = Path::new(".claude/settings.json");
        for first_hop in [dir.join("inside-link"), outside.path().join("outside-link")] {
            symlink(&file, &first_hop).unwrap();
            symlink(&first_hop, root.path().join(rel)).unwrap();
            let refusal = ProjectHookScope::check(ClaudeCode, root.path()).unwrap_err();
            assert!(matches!(refusal, PathRefusal::NotRegular { ref path } if path == rel));
            assert_eq!(
                refusal.to_string(),
                ".claude/settings.json is a link that does not end at a regular file. skim leaves it unchanged."
            );
            fs::remove_file(root.path().join(rel)).unwrap();
        }
    }

    #[test]
    fn recheck_refuses_a_folder_replaced_by_an_outside_link() {
        let root = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let hooks = root.path().join(".cursor/hooks");
        fs::create_dir_all(&hooks).unwrap();
        let scope = ProjectHookScope::check(Cursor, root.path()).unwrap();
        fs::remove_dir(&hooks).unwrap();
        symlink(outside.path(), &hooks).unwrap();
        assert!(matches!(
            scope.recheck(Cursor),
            Err(PathRefusal::LeavesProject { path }) if path == Path::new(".cursor/hooks")
        ));
    }
}

#[test]
fn real_writers_create_only_listed_targets() {
    const CHILD_ROOT: &str = "SKIM_TEST_WRITER_SCOPE_ROOT";
    let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
    // Only the isolated child changes cwd. Install and Copilot seed use it.
    if let Some(expected) = std::env::var_os(CHILD_ROOT) {
        assert_eq!(expected, cwd.as_os_str());
        for &agent in AgentKind::all_supported() {
            let root = tempfile::tempdir_in(&cwd).unwrap();
            std::env::set_current_dir(root.path()).unwrap();
            exercise_writers(agent, root.path());
            std::env::set_current_dir(&cwd).unwrap();
        }
        let scope = ProjectHookScope::for_current_folder(Cursor).unwrap();
        assert_eq!(
            scope.settings_path(),
            Some(cwd.join(".cursor/hooks.json").as_path())
        );
        return;
    }

    let project = TempDir::new().unwrap();
    fs::create_dir(project.path().join(".git")).unwrap();
    let home = project.path().join("home");
    fs::create_dir(&home).unwrap();
    let (_, module) = module_path!().split_once("::").unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .current_dir(project.path())
        .env_clear()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("CODEX_HOME", home.join("codex"))
        .envs(AgentKind::all_supported().iter().map(|&agent| {
            (
                DetectionEnv::override_var(agent),
                home.join(agent.cli_name()),
            )
        }))
        .env(CHILD_ROOT, project.path().canonicalize().unwrap())
        .args([
            "--exact",
            &format!("{module}::real_writers_create_only_listed_targets"),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_listed_paths(agent: AgentKind, root: &Path, targets: &[HookTarget]) {
    const MAX_PATHS: usize = 64;
    let entries = ignore::WalkBuilder::new(root)
        .standard_filters(false)
        .build()
        .skip(1)
        .take(MAX_PATHS + 1);
    for (index, entry) in entries.enumerate() {
        assert!(index < MAX_PATHS, "writer produced too many paths");
        let entry = entry.unwrap();
        let relative = entry.path().strip_prefix(root).unwrap();
        let kind = entry.file_type().unwrap();
        assert!(
            targets.iter().any(|target| target.path() == relative
                && match target {
                    HookTarget::Folder(_) => kind.is_dir(),
                    HookTarget::Owned(_) | HookTarget::UserFile(_) => kind.is_file(),
                }),
            "{} created an unlisted path: {}",
            agent.cli_name(),
            relative.display()
        );
    }
}

fn exercise_writers(agent: AgentKind, root: &Path) {
    use crate::cmd::hooks::{HookSupport, protocol_for_agent};
    use crate::cmd::init::helpers::{HOOK_SCRIPT_NAME, config_backup_name};
    use crate::cmd::init::install::execute_install;
    use crate::cmd::init::state::{detect_state, read_settings_json};
    use crate::cmd::permissions::{
        PermissionsTier, PruneMode, SeedOutcome, permissions_protocol_for_agent,
    };
    use crate::cmd::session::InstructionEnv;

    let dir = root.join(agent.dot_dir_name());
    let hook = protocol_for_agent(agent);
    if hook.hook_support() == HookSupport::RealHook {
        let uses_settings = !hook.uses_dedicated_hook_file();
        if uses_settings {
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join(hook.config_filename()), "{}").unwrap();
        }
        let legacy = dir.join(CURSOR_LEGACY_SETTINGS);
        if agent == Cursor {
            fs::write(
                &legacy,
                r#"{"theme":"fixture","hooks":{"PreToolUse":[{"hooks":[{"command":"skim-rewrite.sh"}]}]}}"#,
            )
            .unwrap();
        }

        let env = DetectionEnv::from_process();
        let state = detect_state(agent, true, &env).unwrap();
        assert_eq!(state.config_dir, dir);
        assert_eq!(state.hook_config_dir, dir);
        execute_install(
            &state,
            true,
            false,
            &InstructionEnv::from_process(),
            false,
            PermissionsTier::Seed,
            false,
        )
        .unwrap();

        assert!(dir.join("hooks").join(HOOK_SCRIPT_NAME).is_file());
        assert!(detect_state(agent, true, &env).unwrap().hook_installed);
        if uses_settings {
            assert!(dir.join(config_backup_name(agent)).is_file());
        }
        if agent == Cursor {
            assert_eq!(
                read_settings_json(&legacy),
                Some(serde_json::json!({ "theme": "fixture" }))
            );
        }
    }
    let targets = project_hook_targets(agent);
    if let Some(protocol) = permissions_protocol_for_agent(agent) {
        let entries = [protocol.render_entry("df"), protocol.render_entry("ls")];
        assert!(matches!(
            protocol
                .seed(&dir, PermissionsTier::Seed, &entries)
                .unwrap(),
            SeedOutcome::Added { .. }
        ));
        assert_listed_paths(agent, root, &targets);
        let scope = ProjectHookScope::check(agent, root).unwrap();
        assert_eq!(
            protocol
                .prune_retired_at(
                    &dir,
                    &entries[..1],
                    PruneMode::Apply,
                    scope.permission_config_path(agent).unwrap(),
                )
                .unwrap(),
            entries[1..]
        );
    }
    assert_listed_paths(agent, root, &targets);
}
