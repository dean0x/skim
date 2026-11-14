//! Tests for the guidance path checks (guidance.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::path::PathBuf;

use super::{
    GlobalPathRefusal, check_global_target, inject_guidance, remove_guidance,
    require_absolute_global, resolve_instruction_path,
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

// ============================================================================
// require_absolute_global
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
// The callers refuse before they read or write
// ============================================================================

#[test]
fn inject_refuses_a_relative_override_and_writes_nothing() {
    let env = env_with_all(RELATIVE_DIR);

    let error = inject_guidance(AgentKind::CodexCli, true, &env).unwrap_err();

    assert!(error.to_string().contains("CODEX_HOME"), "{error}");
    assert!(
        !std::path::Path::new(RELATIVE_DIR).exists(),
        "a refused install must not create the relative folder"
    );
}

#[test]
fn remove_refuses_a_relative_override() {
    let env = env_with_all(RELATIVE_DIR);

    let error = remove_guidance(AgentKind::GeminiCli, true, &env).unwrap_err();

    assert!(error.to_string().contains("GEMINI_CONFIG_DIR"), "{error}");
}

#[test]
fn project_scope_ignores_the_overrides() {
    // Project paths are relative by design; only the global scope is checked.
    let env = env_with_all(RELATIVE_DIR);
    let path = resolve_instruction_path(AgentKind::ClaudeCode, false, &env).unwrap();
    assert_eq!(path, PathBuf::from("CLAUDE.md"));
}

#[test]
fn the_cursor_project_fallback_is_not_refused() {
    // Cursor has no global file; the fallback path is project-relative on purpose.
    let env = env_with_all(RELATIVE_DIR);
    let path = resolve_instruction_path(AgentKind::Cursor, true, &env).unwrap();
    assert_eq!(path, PathBuf::from(".cursor/rules/skim.mdc"));
}
