//! Tests for the guidance report (guidance_report.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::{FileScope, GuidanceFact, GuidanceState, NotChecked, Reader, guidance_report};
use crate::cmd::init::guidance::guidance_target;
use crate::cmd::init::guidance_block::Marker;
use crate::cmd::init::guidance_path::GuidanceScope;
use crate::cmd::init::targets::TARGETS;
use crate::cmd::session::{AgentKind, InstructionEnv};

// ============================================================================
// Helpers
// ============================================================================

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A stale skim block: it names a version no binary has.
const STALE_BLOCK: &str = "<!-- skim-start v0.0.1 -->\nold text\n<!-- skim-end -->\n";

fn env_at(home: &Path) -> InstructionEnv {
    InstructionEnv::from_lookup(Some(home.to_path_buf()), |_| None)
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// The file text that `skim init` writes for `agent` in the global scope: the
/// render and the final newline that the writer adds.
fn current_global_text(agent: AgentKind, env: &InstructionEnv) -> String {
    let render = guidance_target(agent, &GuidanceScope::Global, env)
        .unwrap()
        .unwrap()
        .render(VERSION);
    format!("{render}\n")
}

/// The file text that `skim init --project` writes for `agent` under `root`.
fn current_project_text(agent: AgentKind, root: &Path, env: &InstructionEnv) -> String {
    let scope = GuidanceScope::Project {
        root: root.to_path_buf(),
    };
    let render = guidance_target(agent, &scope, env)
        .unwrap()
        .unwrap()
        .render(VERSION);
    format!("{render}\n")
}

fn global_path(agent: AgentKind, env: &InstructionEnv) -> PathBuf {
    agent.instruction_file(true, env).unwrap()
}

fn only(facts: Vec<GuidanceFact>) -> GuidanceFact {
    assert_eq!(facts.len(), 1, "expected one fact, got: {facts:#?}");
    facts.into_iter().next().unwrap()
}

/// Every folder and file under `dir`, with the bytes of each file.
fn snapshot(dir: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Option<Vec<u8>>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                out.insert(name, None);
                walk(root, &path, out);
            } else if meta.is_file() {
                out.insert(name, Some(std::fs::read(&path).unwrap()));
            } else {
                out.insert(name, None);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

// ============================================================================
// Global files
// ============================================================================

#[test]
fn a_home_folder_with_no_guidance_gives_no_fact() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    let facts = guidance_report(&env_at(home.path()), Some(project.path()));

    assert_eq!(facts, Vec::new());
}

#[test]
fn a_current_global_block_is_current() {
    let home = TempDir::new().unwrap();
    let env = env_at(home.path());
    let path = global_path(AgentKind::ClaudeCode, &env);
    write(&path, &current_global_text(AgentKind::ClaudeCode, &env));

    let fact = only(guidance_report(&env, None));

    assert_eq!(fact.readers, vec![Reader::Agent(AgentKind::ClaudeCode)]);
    assert_eq!(fact.scope, FileScope::Global);
    assert_eq!(fact.shown, path);
    assert_eq!(fact.state, GuidanceState::Current);
}

#[test]
fn a_block_of_another_version_is_stale_and_keeps_its_version() {
    let home = TempDir::new().unwrap();
    let env = env_at(home.path());
    write(&global_path(AgentKind::GeminiCli, &env), STALE_BLOCK);

    let fact = only(guidance_report(&env, None));

    assert_eq!(
        fact.state,
        GuidanceState::Stale {
            marker: Marker::Semver("0.0.1".to_string())
        }
    );
}

#[test]
fn a_block_of_this_version_with_other_text_is_changed() {
    let home = TempDir::new().unwrap();
    let env = env_at(home.path());
    let text = format!("<!-- skim-start v{VERSION} -->\nedited by hand\n<!-- skim-end -->\n");
    write(&global_path(AgentKind::GeminiCli, &env), &text);

    let fact = only(guidance_report(&env, None));

    assert_eq!(fact.state, GuidanceState::Changed);
}

#[test]
fn a_start_marker_with_no_end_marker_is_broken() {
    let home = TempDir::new().unwrap();
    let env = env_at(home.path());
    write(
        &global_path(AgentKind::GeminiCli, &env),
        "<!-- skim-start v0.0.1 -->\nno end marker\n",
    );

    let fact = only(guidance_report(&env, None));

    assert_eq!(fact.state, GuidanceState::Broken);
}

#[test]
fn a_marker_that_is_not_a_version_keeps_its_text_as_unrecognized() {
    let home = TempDir::new().unwrap();
    let env = env_at(home.path());
    let text = "<!-- skim-start v1.x\u{1b}[2J -->\nold\n<!-- skim-end -->\n";
    write(&global_path(AgentKind::GeminiCli, &env), text);

    let fact = only(guidance_report(&env, None));

    match fact.state {
        GuidanceState::Stale {
            marker: Marker::Unrecognized(shown),
        } => assert!(shown.contains("v1.x"), "{shown:?}"),
        other => panic!("expected an unrecognized marker, got {other:?}"),
    }
}

#[test]
fn a_file_with_no_skim_block_gives_no_fact() {
    let home = TempDir::new().unwrap();
    let env = env_at(home.path());
    write(
        &global_path(AgentKind::ClaudeCode, &env),
        "# My own notes\n",
    );

    assert_eq!(guidance_report(&env, None), Vec::new());
}

#[test]
fn a_codex_file_with_no_hook_is_current_when_it_holds_the_no_hook_text() {
    let home = TempDir::new().unwrap();
    let env = env_at(home.path());
    write(
        &global_path(AgentKind::CodexCli, &env),
        &current_global_text(AgentKind::CodexCli, &env),
    );

    let fact = only(guidance_report(&env, None));

    assert_eq!(fact.readers, vec![Reader::Agent(AgentKind::CodexCli)]);
    assert_eq!(fact.state, GuidanceState::Current);
}

#[test]
fn cursor_has_no_global_file_to_report() {
    let home = TempDir::new().unwrap();
    let env = env_at(home.path());
    write(
        &home.path().join(".cursor/rules/skim.mdc"),
        &current_global_text(AgentKind::ClaudeCode, &env),
    );

    assert_eq!(guidance_report(&env, None), Vec::new());
}

// ============================================================================
// Files that cannot be checked
// ============================================================================

#[test]
fn a_relative_config_variable_is_not_checked_and_names_the_variable() {
    let home = TempDir::new().unwrap();
    let env = InstructionEnv::from_lookup(Some(home.path().to_path_buf()), |name| {
        (name == "CLAUDE_CONFIG_DIR").then(|| OsString::from("relative-claude"))
    });

    let fact = only(guidance_report(&env, None));

    assert_eq!(fact.readers, vec![Reader::Agent(AgentKind::ClaudeCode)]);
    assert_eq!(
        fact.state,
        GuidanceState::NotChecked(NotChecked::RelativeVariable("CLAUDE_CONFIG_DIR"))
    );
}

#[test]
fn a_relative_home_folder_is_not_checked_for_any_agent() {
    let env = InstructionEnv::from_lookup(Some(PathBuf::from("relative-home")), |_| None);

    let facts = guidance_report(&env, None);

    let global_agents = AgentKind::all_supported().len() - 1; // Cursor has no global file
    assert_eq!(facts.len(), global_agents, "{facts:#?}");
    assert!(
        facts
            .iter()
            .all(|fact| fact.state == GuidanceState::NotChecked(NotChecked::RelativeHome)),
        "{facts:#?}"
    );
}

#[test]
fn a_global_file_over_the_size_limit_is_not_checked() {
    let home = TempDir::new().unwrap();
    let env = env_at(home.path());
    let big = format!("{STALE_BLOCK}{}", "x".repeat(1_048_577));
    write(&global_path(AgentKind::GeminiCli, &env), &big);

    let fact = only(guidance_report(&env, None));

    assert_eq!(fact.state, GuidanceState::NotChecked(NotChecked::TooLarge));
}

#[test]
fn a_global_file_that_is_not_utf8_is_not_checked_and_stays_as_it_was() {
    let home = TempDir::new().unwrap();
    let env = env_at(home.path());
    let path = global_path(AgentKind::GeminiCli, &env);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let bytes = b"<!-- skim-start v0.0.1 -->\n\xff\xfe\n<!-- skim-end -->\n";
    std::fs::write(&path, bytes).unwrap();

    let fact = only(guidance_report(&env, None));

    assert_eq!(fact.state, GuidanceState::NotChecked(NotChecked::NotUtf8));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[cfg(unix)]
#[test]
fn a_fifo_at_a_project_path_is_not_checked_and_does_not_block() {
    use std::os::unix::fs::FileTypeExt as _;

    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let fifo = project.path().join("CLAUDE.md");
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: the path is NUL-terminated and the mode is valid for mkfifo.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);

    let env = env_at(home.path());
    let root = project.path().to_path_buf();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(guidance_report(&env, Some(&root)));
    });
    let facts = receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the report must not block on a FIFO");

    let fact = only(facts);
    assert_eq!(fact.scope, FileScope::Project);
    assert_eq!(
        fact.state,
        GuidanceState::NotChecked(NotChecked::NotRegular)
    );
    assert!(
        std::fs::symlink_metadata(&fifo)
            .unwrap()
            .file_type()
            .is_fifo()
    );
}

#[cfg(unix)]
#[test]
fn a_project_link_that_leaves_the_project_is_not_checked_and_the_target_is_not_read() {
    let home = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let target = outside.path().join("notes.md");
    std::fs::write(&target, STALE_BLOCK).unwrap();
    std::os::unix::fs::symlink(&target, project.path().join("CLAUDE.md")).unwrap();

    let fact = only(guidance_report(&env_at(home.path()), Some(project.path())));

    assert_eq!(fact.readers, vec![Reader::Agent(AgentKind::ClaudeCode)]);
    assert_eq!(fact.shown, PathBuf::from("CLAUDE.md"));
    assert_eq!(
        fact.state,
        GuidanceState::NotChecked(NotChecked::LeavesProject)
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), STALE_BLOCK);
}

// ============================================================================
// Project files and de-duplication
// ============================================================================

#[test]
fn project_files_are_reported_with_the_project_scope_and_a_relative_path() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write(&project.path().join("GEMINI.md"), STALE_BLOCK);

    let fact = only(guidance_report(&env_at(home.path()), Some(project.path())));

    assert_eq!(fact.readers, vec![Reader::Agent(AgentKind::GeminiCli)]);
    assert_eq!(fact.scope, FileScope::Project);
    assert_eq!(fact.shown, PathBuf::from("GEMINI.md"));
}

#[test]
fn project_files_are_not_looked_at_without_a_project_folder() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write(&project.path().join("GEMINI.md"), STALE_BLOCK);

    assert_eq!(guidance_report(&env_at(home.path()), None), Vec::new());
}

#[test]
fn codex_and_crush_share_one_project_fact() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let env = env_at(home.path());
    write(
        &project.path().join("AGENTS.md"),
        &current_project_text(AgentKind::CodexCli, project.path(), &env),
    );

    let fact = only(guidance_report(&env, Some(project.path())));

    assert_eq!(
        fact.readers,
        vec![
            Reader::Agent(AgentKind::CodexCli),
            Reader::Agent(AgentKind::Crush)
        ]
    );
    assert_eq!(fact.shown, PathBuf::from("AGENTS.md"));
    assert_eq!(fact.state, GuidanceState::Current);
}

#[cfg(unix)]
#[test]
fn a_link_and_the_file_it_names_are_one_fact_under_the_name_of_the_file() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    write(&project.path().join("AGENTS.md"), STALE_BLOCK);
    std::os::unix::fs::symlink("AGENTS.md", project.path().join("CLAUDE.md")).unwrap();

    let fact = only(guidance_report(&env_at(home.path()), Some(project.path())));

    assert_eq!(fact.shown, PathBuf::from("AGENTS.md"));
    assert_eq!(
        fact.readers,
        vec![
            Reader::Agent(AgentKind::ClaudeCode),
            Reader::Agent(AgentKind::CodexCli),
            Reader::Agent(AgentKind::Crush)
        ]
    );
}

#[test]
fn a_file_that_both_scopes_reach_is_one_fact_in_the_global_scope() {
    let home = TempDir::new().unwrap();
    let env = env_at(home.path());
    let claude_folder = home.path().join(".claude");
    write(&claude_folder.join("CLAUDE.md"), STALE_BLOCK);

    // The project folder is the Claude folder, so `CLAUDE.md` is the same file.
    let fact = only(guidance_report(&env, Some(&claude_folder)));

    assert_eq!(fact.scope, FileScope::Global);
    assert_eq!(fact.readers, vec![Reader::Agent(AgentKind::ClaudeCode)]);
}

#[test]
fn global_facts_come_before_project_facts() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let env = env_at(home.path());
    write(&project.path().join("CLAUDE.md"), STALE_BLOCK);
    write(&global_path(AgentKind::GeminiCli, &env), STALE_BLOCK);

    let facts = guidance_report(&env, Some(project.path()));

    let scopes: Vec<FileScope> = facts.iter().map(|fact| fact.scope).collect();
    assert_eq!(scopes, vec![FileScope::Global, FileScope::Project]);
}

// ============================================================================
// Targets
// ============================================================================

#[test]
fn a_target_file_is_reported_under_the_name_of_the_target() {
    let home = TempDir::new().unwrap();
    let env = env_at(home.path());
    let kiro = TARGETS.iter().find(|agent| agent.name == "Kiro").unwrap();
    std::fs::create_dir_all(home.path().join(kiro.root)).unwrap();
    let target = kiro.resolve(&env).unwrap().unwrap();
    write(&target.path, &format!("{}\n", target.render(VERSION)));

    let fact = only(guidance_report(&env, None));

    assert_eq!(fact.readers, vec![Reader::Target("Kiro")]);
    assert_eq!(fact.scope, FileScope::Global);
    assert_eq!(fact.shown, target.shown);
    assert_eq!(fact.state, GuidanceState::Current);
}

#[test]
fn every_target_file_with_a_stale_block_is_reported() {
    let home = TempDir::new().unwrap();
    let env = env_at(home.path());
    for agent in TARGETS {
        write(&home.path().join(agent.dir).join(agent.file), STALE_BLOCK);
    }

    let facts = guidance_report(&env, None);

    let names: Vec<&str> = facts
        .iter()
        .flat_map(|fact| fact.readers.iter().map(|reader| reader.name()))
        .collect();
    let expected: Vec<&str> = TARGETS.iter().map(|agent| agent.name).collect();
    assert_eq!(names, expected);
    assert!(
        facts
            .iter()
            .all(|fact| matches!(fact.state, GuidanceState::Stale { .. }))
    );
}

#[test]
fn a_target_root_that_is_not_a_folder_gives_no_fact() {
    let home = TempDir::new().unwrap();
    let env = env_at(home.path());
    let kiro = TARGETS.iter().find(|agent| agent.name == "Kiro").unwrap();
    std::fs::write(home.path().join(kiro.root), STALE_BLOCK).unwrap();

    assert_eq!(guidance_report(&env, None), Vec::new());
}

// ============================================================================
// The report changes nothing
// ============================================================================

#[test]
fn the_report_leaves_every_file_and_folder_as_it_was() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let env = env_at(home.path());
    write(&global_path(AgentKind::GeminiCli, &env), STALE_BLOCK);
    write(
        &global_path(AgentKind::CodexCli, &env),
        "<!-- skim-start v0.0.1 -->\nno end marker\n",
    );
    write(&project.path().join("CLAUDE.md"), STALE_BLOCK);
    write(&project.path().join(".cursor/rules/skim.mdc"), STALE_BLOCK);
    std::fs::create_dir_all(home.path().join(".kiro")).unwrap();
    let before = (snapshot(home.path()), snapshot(project.path()));

    let facts = guidance_report(&env, Some(project.path()));

    assert!(!facts.is_empty());
    assert_eq!(before, (snapshot(home.path()), snapshot(project.path())));
}
