//! Tests for the guidance path policy (guidance_path.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::{
    GlobalPathRefusal, GuidanceScope, PathRefusal, check_global_target, require_absolute_global,
    resolve_guidance_path,
};
use crate::cmd::session::{AgentKind, InstructionEnv, RelativeOverride};

// ============================================================================
// Helpers
// ============================================================================

/// A relative folder that no test may create.
const RELATIVE_DIR: &str = "relative-config-folder-for-guidance-tests";

fn abs_home() -> PathBuf {
    std::env::temp_dir().join("guidance-tests-home")
}

/// An env whose every config-folder variable holds `value`.
fn env_with_all(value: &str) -> InstructionEnv {
    InstructionEnv::from_lookup(Some(abs_home()), |_| Some(OsString::from(value)))
}

fn env_without_overrides() -> InstructionEnv {
    InstructionEnv::from_lookup(Some(abs_home()), |_| None)
}

/// A project folder (the root) and a second folder outside it.
struct Fixture {
    root: TempDir,
    outside: TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            root: TempDir::new().unwrap(),
            outside: TempDir::new().unwrap(),
        }
    }

    fn scope(&self) -> GuidanceScope {
        GuidanceScope::Project {
            root: self.root.path().to_path_buf(),
        }
    }

    /// The canonical root: the base of every resolved project path.
    fn canonical_root(&self) -> PathBuf {
        std::fs::canonicalize(self.root.path()).unwrap()
    }

    fn in_root(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn in_outside(&self, name: &str) -> PathBuf {
        self.outside.path().join(name)
    }
}

#[cfg(unix)]
fn link(target: &Path, at: &Path) {
    std::os::unix::fs::symlink(target, at).unwrap();
}

// ============================================================================
// Global paths are absolute
// ============================================================================

#[test]
fn a_relative_override_is_refused_and_the_message_names_the_variable() {
    let env = env_with_all(RELATIVE_DIR);
    let path = AgentKind::CodexCli.instruction_file(true, &env).unwrap();

    let refused = require_absolute_global(AgentKind::CodexCli, &path, &env).unwrap_err();

    assert_eq!(
        refused,
        GlobalPathRefusal::RelativeOverride(RelativeOverride {
            var: "CODEX_HOME",
            value: PathBuf::from(RELATIVE_DIR),
        })
    );
    assert!(refused.to_string().contains("CODEX_HOME"), "{refused}");
}

#[test]
fn an_absolute_global_path_passes() {
    let env = env_without_overrides();
    let path = AgentKind::ClaudeCode.instruction_file(true, &env).unwrap();
    assert_eq!(
        require_absolute_global(AgentKind::ClaudeCode, &path, &env),
        Ok(())
    );
}

#[test]
fn a_relative_global_path_without_an_override_is_refused() {
    // A relative home folder gives a relative global path with no variable at fault.
    let env = InstructionEnv::from_lookup(Some(PathBuf::from("relative-home")), |_| None);
    let path = AgentKind::ClaudeCode.instruction_file(true, &env).unwrap();

    let refused = require_absolute_global(AgentKind::ClaudeCode, &path, &env).unwrap_err();

    assert_eq!(refused, GlobalPathRefusal::NotAbsolute(path));
}

#[test]
fn an_agent_with_no_global_file_passes_the_target_check() {
    // Cursor has no global instruction file, so a bad variable does not matter.
    assert_eq!(
        check_global_target(AgentKind::Cursor, &env_with_all(RELATIVE_DIR)),
        Ok(())
    );
}

#[test]
fn the_target_check_refuses_a_relative_override_of_every_agent_with_a_global_file() {
    let env = env_with_all(RELATIVE_DIR);
    for &agent in AgentKind::all_supported() {
        let Some(var) = InstructionEnv::override_var(agent) else {
            continue;
        };
        let refused = check_global_target(agent, &env).unwrap_err().to_string();
        assert!(refused.contains(var), "{agent}: {refused}");
    }
}

// ============================================================================
// Project scope: plain files and missing folders
// ============================================================================

#[test]
fn a_plain_project_file_resolves_to_itself_and_is_shown_relative_to_the_root() {
    let fx = Fixture::new();
    std::fs::write(fx.in_root("CLAUDE.md"), "text\n").unwrap();

    let resolved = resolve_guidance_path(Path::new("CLAUDE.md"), &fx.scope()).unwrap();

    assert_eq!(resolved.path, fx.canonical_root().join("CLAUDE.md"));
    assert_eq!(resolved.shown, PathBuf::from("CLAUDE.md"));
}

#[test]
fn missing_project_folders_are_accepted_and_none_is_created() {
    let fx = Fixture::new();

    let resolved = resolve_guidance_path(Path::new(".cursor/rules/skim.mdc"), &fx.scope()).unwrap();

    assert_eq!(
        resolved.path,
        fx.canonical_root().join(".cursor/rules/skim.mdc")
    );
    assert_eq!(resolved.shown, PathBuf::from(".cursor/rules/skim.mdc"));
    assert!(
        !fx.in_root(".cursor").exists(),
        "the policy must create no folder"
    );
}

#[test]
fn a_parent_folder_component_is_refused() {
    let fx = Fixture::new();

    // The second path climbs out of the root through a folder that does not exist.
    for path in ["../CLAUDE.md", "missing/../../CLAUDE.md"] {
        let refused = resolve_guidance_path(Path::new(path), &fx.scope()).unwrap_err();

        assert!(
            matches!(refused, PathRefusal::LeavesProject { .. }),
            "{path}: {refused}"
        );
    }
}

#[test]
fn a_project_root_that_is_missing_is_refused() {
    let fx = Fixture::new();
    let scope = GuidanceScope::Project {
        root: fx.in_root("not-here"),
    };

    let refused = resolve_guidance_path(Path::new("CLAUDE.md"), &scope).unwrap_err();

    assert!(
        matches!(refused, PathRefusal::Unchecked { .. }),
        "{refused}"
    );
}

// ============================================================================
// Project scope: links
// ============================================================================

#[cfg(unix)]
#[test]
fn a_project_link_to_a_file_inside_the_project_is_followed() {
    let fx = Fixture::new();
    std::fs::write(fx.in_root("AGENTS.md"), "shared text\n").unwrap();
    link(Path::new("AGENTS.md"), &fx.in_root("CLAUDE.md"));

    let resolved = resolve_guidance_path(Path::new("CLAUDE.md"), &fx.scope()).unwrap();

    assert_eq!(resolved.path, fx.canonical_root().join("AGENTS.md"));
    assert_eq!(resolved.shown, PathBuf::from("AGENTS.md"));
}

#[cfg(unix)]
#[test]
fn a_project_link_to_a_file_outside_the_project_is_refused_and_names_the_file() {
    let fx = Fixture::new();
    std::fs::write(fx.in_outside("secret.md"), "outside text\n").unwrap();
    link(&fx.in_outside("secret.md"), &fx.in_root("CLAUDE.md"));

    let refused = resolve_guidance_path(Path::new("CLAUDE.md"), &fx.scope()).unwrap_err();

    assert!(
        matches!(&refused, PathRefusal::LeavesProject { path } if path == Path::new("CLAUDE.md")),
        "{refused}"
    );
    assert!(refused.to_string().contains("CLAUDE.md"), "{refused}");
    assert_eq!(
        std::fs::read_to_string(fx.in_outside("secret.md")).unwrap(),
        "outside text\n"
    );
}

#[cfg(unix)]
#[test]
fn a_project_link_chain_that_ends_outside_the_project_is_refused() {
    let fx = Fixture::new();
    std::fs::write(fx.in_outside("secret.md"), "outside text\n").unwrap();
    // CLAUDE.md -> AGENTS.md (inside) -> the outside file.
    link(&fx.in_outside("secret.md"), &fx.in_root("AGENTS.md"));
    link(Path::new("AGENTS.md"), &fx.in_root("CLAUDE.md"));

    let refused = resolve_guidance_path(Path::new("CLAUDE.md"), &fx.scope()).unwrap_err();

    assert!(
        matches!(refused, PathRefusal::LeavesProject { .. }),
        "{refused}"
    );
}

#[cfg(unix)]
#[test]
fn a_linked_project_folder_that_leads_outside_the_project_is_refused() {
    let fx = Fixture::new();
    link(fx.outside.path(), &fx.in_root(".cursor"));

    let refused =
        resolve_guidance_path(Path::new(".cursor/rules/skim.mdc"), &fx.scope()).unwrap_err();

    assert!(
        matches!(&refused, PathRefusal::LeavesProject { path } if path == Path::new(".cursor/rules/skim.mdc")),
        "{refused}"
    );
    assert!(
        std::fs::read_dir(fx.outside.path())
            .unwrap()
            .next()
            .is_none(),
        "the policy must write nothing outside the project"
    );
}

#[cfg(unix)]
#[test]
fn a_project_link_to_a_folder_is_refused_as_not_a_regular_file() {
    let fx = Fixture::new();
    std::fs::create_dir(fx.in_root("docs")).unwrap();
    link(Path::new("docs"), &fx.in_root("CLAUDE.md"));

    let refused = resolve_guidance_path(Path::new("CLAUDE.md"), &fx.scope()).unwrap_err();

    assert!(
        matches!(refused, PathRefusal::NotRegular { .. }),
        "{refused}"
    );
}

#[cfg(unix)]
#[test]
fn a_project_link_to_nothing_is_refused_as_not_a_regular_file() {
    let fx = Fixture::new();
    link(Path::new("gone.md"), &fx.in_root("CLAUDE.md"));

    let refused = resolve_guidance_path(Path::new("CLAUDE.md"), &fx.scope()).unwrap_err();

    assert!(
        matches!(refused, PathRefusal::NotRegular { .. }),
        "{refused}"
    );
}

#[test]
fn a_project_folder_name_that_is_a_file_is_reported_as_unchecked() {
    let fx = Fixture::new();
    std::fs::write(fx.in_root(".cursor"), "not a folder\n").unwrap();

    let refused =
        resolve_guidance_path(Path::new(".cursor/rules/skim.mdc"), &fx.scope()).unwrap_err();

    assert!(
        matches!(refused, PathRefusal::Unchecked { .. }),
        "{refused}"
    );
    assert!(
        refused.to_string().contains(".cursor/rules/skim.mdc"),
        "{refused}"
    );
}

// ============================================================================
// Global scope
// ============================================================================

#[test]
fn a_plain_global_file_resolves_to_itself() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("CLAUDE.md");
    std::fs::write(&file, "text\n").unwrap();

    let resolved = resolve_guidance_path(&file, &GuidanceScope::Global).unwrap();

    assert_eq!(resolved.path, file);
    assert_eq!(resolved.shown, file);
}

#[cfg(unix)]
#[test]
fn a_global_link_to_a_regular_file_is_followed() {
    let dir = TempDir::new().unwrap();
    let real = dir.path().join("dotfiles").join("claude.md");
    std::fs::create_dir_all(real.parent().unwrap()).unwrap();
    std::fs::write(&real, "text\n").unwrap();
    let at = dir.path().join("CLAUDE.md");
    link(&real, &at);

    let resolved = resolve_guidance_path(&at, &GuidanceScope::Global).unwrap();

    assert_eq!(resolved.path, std::fs::canonicalize(&real).unwrap());
}

#[cfg(unix)]
#[test]
fn a_global_link_to_a_folder_is_refused() {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("docs")).unwrap();
    let at = dir.path().join("CLAUDE.md");
    link(Path::new("docs"), &at);

    let refused = resolve_guidance_path(&at, &GuidanceScope::Global).unwrap_err();

    assert!(
        matches!(refused, PathRefusal::NotRegular { .. }),
        "{refused}"
    );
}

#[cfg(unix)]
#[test]
fn a_global_link_to_a_device_is_refused() {
    let dir = TempDir::new().unwrap();
    let at = dir.path().join("CLAUDE.md");
    link(Path::new("/dev/null"), &at);

    let refused = resolve_guidance_path(&at, &GuidanceScope::Global).unwrap_err();

    assert!(
        matches!(refused, PathRefusal::NotRegular { .. }),
        "{refused}"
    );
}

#[test]
fn a_relative_global_path_is_refused_by_the_policy() {
    let refused =
        resolve_guidance_path(Path::new("relative/CLAUDE.md"), &GuidanceScope::Global).unwrap_err();

    assert!(
        matches!(refused, PathRefusal::Unchecked { .. }),
        "{refused}"
    );
}

// ============================================================================
// Scope from the flag
// ============================================================================

#[test]
fn the_project_scope_is_the_current_folder_and_the_global_scope_has_no_root() {
    assert_eq!(
        GuidanceScope::for_flag(true).unwrap(),
        GuidanceScope::Global
    );
    assert_eq!(
        GuidanceScope::for_flag(false).unwrap(),
        GuidanceScope::Project {
            root: std::env::current_dir().unwrap()
        }
    );
}
