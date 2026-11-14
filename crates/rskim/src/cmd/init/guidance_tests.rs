//! Tests for the guidance path checks (guidance.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::path::PathBuf;

use super::{
    GuidanceFormat, GuidanceHooks, GuidanceScope, PathRefusal, TargetRefusal, agents_sharing_file,
    guidance_target, inject_guidance, kept_shared_note, no_global_file_note, remove_guidance,
    writable_target,
};
use crate::cmd::init::guidance_block::CURSOR_RULE_FRONTMATTER;
use crate::cmd::init::helpers::guidance_content;
use crate::cmd::session::{AgentKind, InstructionEnv};

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

    let error = remove_guidance(AgentKind::GeminiCli, true, &env, &[]).unwrap_err();

    assert!(error.to_string().contains("GEMINI_CONFIG_DIR"), "{error}");
}

// ============================================================================
// guidance_target
// ============================================================================

fn project_scope(root: &tempfile::TempDir) -> GuidanceScope {
    GuidanceScope::Project {
        root: root.path().to_path_buf(),
    }
}

#[test]
fn project_scope_ignores_the_overrides() {
    // Project paths are relative by design; only the global scope is checked.
    let root = tempfile::TempDir::new().unwrap();
    let env = env_with_all(RELATIVE_DIR);

    let target = guidance_target(AgentKind::ClaudeCode, &project_scope(&root), &env)
        .unwrap()
        .unwrap();

    assert_eq!(target.shown, PathBuf::from("CLAUDE.md"));
    assert_eq!(target.format, GuidanceFormat::markdown(GuidanceHooks::Hook));
}

#[test]
fn the_cursor_project_file_is_a_cursor_rule_file() {
    // Cursor has no global file; its project file is a `.mdc` rule file.
    let root = tempfile::TempDir::new().unwrap();
    let env = env_with_all(RELATIVE_DIR);

    let target = guidance_target(AgentKind::Cursor, &project_scope(&root), &env)
        .unwrap()
        .unwrap();

    assert_eq!(target.shown, PathBuf::from(".cursor/rules/skim.mdc"));
    assert_eq!(
        target.format,
        GuidanceFormat::cursor_rule(GuidanceHooks::Hook)
    );
}

#[test]
fn cursor_has_no_global_target() {
    let env = env_with_all(RELATIVE_DIR);

    let target = guidance_target(AgentKind::Cursor, &GuidanceScope::Global, &env).unwrap();

    assert_eq!(target, None);
}

#[test]
fn a_global_install_for_cursor_has_no_target_and_does_not_fall_back_to_the_project_file() {
    let env = env_with_all(RELATIVE_DIR);

    let target = writable_target(AgentKind::Cursor, true, &env).unwrap();

    assert_eq!(
        target, None,
        "a global Cursor install must not write a file in the current folder"
    );
}

#[test]
fn the_cursor_note_names_the_project_command() {
    assert_eq!(
        no_global_file_note(AgentKind::Cursor),
        "  Note: Cursor has no global guidance file. \
         In a repository, run `skim init --agent cursor --project` to add it."
    );
}

#[test]
fn a_relative_override_stops_the_command_and_names_the_variable() {
    let env = env_with_all(RELATIVE_DIR);

    let refusal = guidance_target(AgentKind::CodexCli, &GuidanceScope::Global, &env).unwrap_err();

    assert!(matches!(refusal, TargetRefusal::Stop(_)), "{refusal:?}");
    let error = refusal.report().unwrap_err();
    assert!(error.to_string().contains("CODEX_HOME"), "{error}");
}

#[cfg(unix)]
#[test]
fn a_project_link_that_leaves_the_project_skips_the_file_and_the_command_goes_on() {
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();
    std::fs::write(outside.path().join("other.md"), "outside text\n").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("other.md"),
        root.path().join("CLAUDE.md"),
    )
    .unwrap();

    let refusal = guidance_target(
        AgentKind::ClaudeCode,
        &project_scope(&root),
        &env_with_all(RELATIVE_DIR),
    )
    .unwrap_err();

    assert!(
        matches!(
            refusal,
            TargetRefusal::Skip(PathRefusal::LeavesProject { .. })
        ),
        "{refusal:?}"
    );
    refusal.report().unwrap();
}

#[test]
fn the_text_of_a_target_follows_its_format() {
    let root = tempfile::TempDir::new().unwrap();
    let env = env_with_all(RELATIVE_DIR);
    let markdown = guidance_target(AgentKind::ClaudeCode, &project_scope(&root), &env)
        .unwrap()
        .unwrap();
    let rule_file = guidance_target(AgentKind::Cursor, &project_scope(&root), &env)
        .unwrap()
        .unwrap();

    assert_eq!(
        markdown.render("9.9.9"),
        guidance_content("9.9.9", GuidanceHooks::Hook)
    );
    assert_eq!(
        rule_file.render("9.9.9"),
        format!(
            "{CURSOR_RULE_FRONTMATTER}{}",
            guidance_content("9.9.9", GuidanceHooks::Hook)
        )
    );
    assert_eq!(
        rule_file.block("9.9.9"),
        guidance_content("9.9.9", GuidanceHooks::Hook)
    );
}

// ============================================================================
// Global links through install and uninstall
// ============================================================================

#[cfg(unix)]
#[test]
fn install_and_uninstall_follow_a_global_link_to_a_regular_file() {
    let home = tempfile::TempDir::new().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let real = home.path().join("dotfiles-claude.md");
    std::fs::write(&real, "# My rules\n").unwrap();
    let at = claude.join("CLAUDE.md");
    std::os::unix::fs::symlink(&real, &at).unwrap();
    let env = InstructionEnv::from_lookup(Some(home.path().to_path_buf()), |_| None);

    inject_guidance(AgentKind::ClaudeCode, true, &env).unwrap();

    assert!(
        std::fs::symlink_metadata(&at)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let installed = std::fs::read_to_string(&real).unwrap();
    assert!(installed.starts_with("# My rules\n"), "{installed}");
    assert!(installed.contains("skim-start"), "{installed}");

    remove_guidance(AgentKind::ClaudeCode, true, &env, &[]).unwrap();

    assert!(
        std::fs::symlink_metadata(&at)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "# My rules\n");
}

#[cfg(unix)]
#[test]
fn install_and_uninstall_leave_a_global_link_to_a_folder_alone() {
    let home = tempfile::TempDir::new().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let folder = home.path().join("some-folder");
    std::fs::create_dir(&folder).unwrap();
    let at = claude.join("CLAUDE.md");
    std::os::unix::fs::symlink(&folder, &at).unwrap();
    let env = InstructionEnv::from_lookup(Some(home.path().to_path_buf()), |_| None);

    inject_guidance(AgentKind::ClaudeCode, true, &env).unwrap();
    remove_guidance(AgentKind::ClaudeCode, true, &env, &[]).unwrap();

    assert!(
        std::fs::symlink_metadata(&at)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        std::fs::read_dir(&folder).unwrap().next().is_none(),
        "nothing may be written into the folder"
    );
}

// ============================================================================
// Reads: no link, no block on a FIFO, strict UTF-8, bounded size
// ============================================================================

use std::path::Path;

use super::{MAX_INSTRUCTION_FILE_SIZE, atomic_write_stripped, read_existing_safely};

/// Run `work` on its own thread and fail the test when it does not return.
/// A read that blocks on a FIFO would otherwise hang the test run.
fn within_timeout<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(work());
    });
    receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the call must not block")
}

/// An env whose global instruction files all sit under `home`.
fn env_at(home: &Path) -> InstructionEnv {
    InstructionEnv::from_lookup(Some(home.to_path_buf()), |_| None)
}

/// A stale skim block that `inject_guidance` replaces.
const STALE_BLOCK: &str = "<!-- skim-start v0.0.1 -->\nold text\n<!-- skim-end -->\n";

#[cfg(unix)]
fn make_fifo(path: &Path) {
    let made = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .unwrap();
    assert!(made.success(), "mkfifo failed");
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(unix)]
#[test]
fn a_fifo_is_skipped_by_the_reader_and_stays_a_fifo() {
    use std::os::unix::fs::FileTypeExt as _;

    let dir = tempfile::TempDir::new().unwrap();
    let fifo = dir.path().join("CLAUDE.md");
    make_fifo(&fifo);

    let path = fifo.clone();
    let read = within_timeout(move || read_existing_safely(&path).unwrap());

    assert_eq!(read, None);
    assert!(
        std::fs::symlink_metadata(&fifo)
            .unwrap()
            .file_type()
            .is_fifo()
    );
}

#[cfg(unix)]
#[test]
fn inject_skips_a_fifo_at_the_instruction_path_without_blocking() {
    use std::os::unix::fs::FileTypeExt as _;

    let home = tempfile::TempDir::new().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let fifo = claude.join("CLAUDE.md");
    make_fifo(&fifo);

    let env = env_at(home.path());
    within_timeout(move || inject_guidance(AgentKind::ClaudeCode, true, &env)).unwrap();

    assert!(
        std::fs::symlink_metadata(&fifo)
            .unwrap()
            .file_type()
            .is_fifo()
    );
}

#[test]
fn a_file_that_is_not_utf8_is_skipped_and_stays_byte_unchanged() {
    let home = tempfile::TempDir::new().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let file = claude.join("CLAUDE.md");
    let bytes: &[u8] = b"# notes \xff\xfe kept as they are\n";
    std::fs::write(&file, bytes).unwrap();

    assert_eq!(read_existing_safely(&file).unwrap(), None);
    inject_guidance(AgentKind::ClaudeCode, true, &env_at(home.path())).unwrap();
    remove_guidance(AgentKind::ClaudeCode, true, &env_at(home.path()), &[]).unwrap();

    assert_eq!(std::fs::read(&file).unwrap(), bytes);
}

#[cfg(unix)]
#[test]
fn the_reader_does_not_follow_a_link_at_the_file_name() {
    let dir = tempfile::TempDir::new().unwrap();
    let real = dir.path().join("real.md");
    std::fs::write(&real, "outside text\n").unwrap();
    let link = dir.path().join("CLAUDE.md");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    assert_eq!(read_existing_safely(&link).unwrap(), None);
}

#[test]
fn a_missing_file_reads_as_none() {
    let dir = tempfile::TempDir::new().unwrap();
    assert_eq!(
        read_existing_safely(&dir.path().join("absent.md")).unwrap(),
        None
    );
}

#[test]
fn a_file_over_the_bound_is_skipped_and_a_file_at_the_bound_is_read() {
    let dir = tempfile::TempDir::new().unwrap();
    let at_bound = dir.path().join("at.md");
    let over_bound = dir.path().join("over.md");
    let bound = usize::try_from(MAX_INSTRUCTION_FILE_SIZE).unwrap();
    std::fs::write(&at_bound, vec![b'a'; bound]).unwrap();
    std::fs::write(&over_bound, vec![b'a'; bound + 1]).unwrap();

    assert_eq!(
        read_existing_safely(&at_bound).unwrap().map(|s| s.len()),
        Some(bound)
    );
    assert_eq!(read_existing_safely(&over_bound).unwrap(), None);
}

// ============================================================================
// Writes: random temporary name, mode kept, 0600 for a new file
// ============================================================================

#[cfg(unix)]
#[test]
fn the_writer_leaves_a_link_and_a_file_at_the_old_temporary_name_unchanged() {
    let dir = tempfile::TempDir::new().unwrap();
    let outside = dir.path().join("outside.txt");
    std::fs::write(&outside, "outside text\n").unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir(&work).unwrap();

    // The old writer used `<file name>.tmp`: here as a link, and as a file.
    let md_tmp = work.join("CLAUDE.md.tmp");
    std::os::unix::fs::symlink(&outside, &md_tmp).unwrap();
    let mdc_tmp = work.join("skim.mdc.tmp");
    std::fs::write(&mdc_tmp, "user data\n").unwrap();

    atomic_write_stripped(&work.join("CLAUDE.md"), "new text\n").unwrap();
    atomic_write_stripped(&work.join("skim.mdc"), "new text\n").unwrap();

    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "outside text\n");
    assert!(
        std::fs::symlink_metadata(&md_tmp)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_to_string(&mdc_tmp).unwrap(), "user data\n");
    assert_eq!(
        std::fs::read_to_string(work.join("CLAUDE.md")).unwrap(),
        "new text\n"
    );
    assert_eq!(
        std::fs::read_to_string(work.join("skim.mdc")).unwrap(),
        "new text\n"
    );
}

#[cfg(unix)]
#[test]
fn install_and_uninstall_leave_the_old_temporary_name_unchanged() {
    let home = tempfile::TempDir::new().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let outside = home.path().join("outside.txt");
    std::fs::write(&outside, "outside text\n").unwrap();
    let tmp_link = claude.join("CLAUDE.md.tmp");
    std::os::unix::fs::symlink(&outside, &tmp_link).unwrap();
    std::fs::write(
        claude.join("CLAUDE.md"),
        format!("# Header\n\n{STALE_BLOCK}\n# Footer\n"),
    )
    .unwrap();
    let env = env_at(home.path());

    inject_guidance(AgentKind::ClaudeCode, true, &env).unwrap();
    remove_guidance(AgentKind::ClaudeCode, true, &env, &[]).unwrap();

    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "outside text\n");
    assert!(
        std::fs::symlink_metadata(&tmp_link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_to_string(claude.join("CLAUDE.md")).unwrap(),
        "# Header\n\n# Footer\n"
    );
}

#[cfg(unix)]
#[test]
fn an_upgrade_rewrite_keeps_the_mode_of_the_instruction_file() {
    for mode in [0o600, 0o640, 0o644] {
        let home = tempfile::TempDir::new().unwrap();
        let claude = home.path().join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        let file = claude.join("CLAUDE.md");
        std::fs::write(&file, format!("# Header\n\n{STALE_BLOCK}")).unwrap();
        set_mode(&file, mode);

        inject_guidance(AgentKind::ClaudeCode, true, &env_at(home.path())).unwrap();

        let rewritten = std::fs::read_to_string(&file).unwrap();
        assert!(!rewritten.contains("v0.0.1"), "the block was not replaced");
        assert_eq!(mode_of(&file), mode, "mode {mode:o} was not kept");
    }
}

#[cfg(unix)]
#[test]
fn a_new_instruction_file_is_created_with_mode_0600() {
    let home = tempfile::TempDir::new().unwrap();

    inject_guidance(AgentKind::ClaudeCode, true, &env_at(home.path())).unwrap();

    let file = home.path().join(".claude").join("CLAUDE.md");
    assert!(file.exists());
    assert_eq!(mode_of(&file), 0o600);
}

// ============================================================================
// The render that the writer selects
// ============================================================================

use super::{GuidanceTarget, LeftReason, UpdateOutcome, strip_target, update_existing_guidance};
use crate::cmd::init::helpers::{
    MAX_SEARCH_SECTION_LINES, SEARCH_SECTION_NEEDLES, SEARCH_WHEN, search_section,
};
use crate::cmd::init::targets::TARGETS;

const RENDER_VERSION: &str = "3.0.1";

/// `true` when the text for the file of `agent` in this scope may speak of a
/// hook. Codex has no hook, and Crush shares the project `AGENTS.md` with it.
fn expects_hook(agent: AgentKind, global: bool) -> bool {
    !matches!(
        (agent, global),
        (AgentKind::CodexCli, _) | (AgentKind::Crush, false)
    )
}

#[test]
fn every_render_the_selector_returns_holds_the_search_lines_and_fits_its_variant() {
    let home = tempfile::TempDir::new().unwrap();
    let root = tempfile::TempDir::new().unwrap();
    let env = env_at(home.path());
    let mut seen = 0;

    let check_search_section = |label: &str, render: &str| {
        let section = search_section(render);
        assert!(!section.is_empty(), "{label}: no Search and read section");
        assert!(
            section.len() <= MAX_SEARCH_SECTION_LINES,
            "{label}: the section has {} lines",
            section.len()
        );
        let text = section.join("\n");
        assert!(text.contains(SEARCH_WHEN), "{label}: no when lines");
        for needle in SEARCH_SECTION_NEEDLES {
            assert!(text.contains(needle), "{label}: no {needle:?}");
        }
    };

    // The guidance-only agents have no hook, so their text has no hook claims.
    for agent in TARGETS {
        std::fs::create_dir_all(home.path().join(agent.root)).unwrap();
        let target = agent
            .resolve(&env)
            .expect("the root folder exists")
            .unwrap();
        seen += 1;
        let render = target.render(RENDER_VERSION);

        check_search_section(agent.name, &render);
        assert!(
            !render.contains("### Command wrapping"),
            "{}: hook claims in the text of an agent with no hook",
            agent.name
        );
        assert_eq!(
            render.starts_with("---\n"),
            agent.frontmatter.is_some(),
            "{}: the frontmatter does not match the row",
            agent.name
        );
    }

    for &agent in AgentKind::all_supported() {
        for global in [true, false] {
            let scope = if global {
                GuidanceScope::Global
            } else {
                project_scope(&root)
            };
            let Some(target) = guidance_target(agent, &scope, &env).unwrap() else {
                continue;
            };
            seen += 1;
            let label = format!("{agent} (global: {global})");
            let render = target.render(RENDER_VERSION);

            check_search_section(&label, &render);
            assert_eq!(
                render.contains("### Command wrapping"),
                expects_hook(agent, global),
                "{label}: the hook claims do not match the agents that read the file"
            );
            assert_eq!(
                render.starts_with("---\n"),
                target.path.extension().is_some_and(|e| e == "mdc"),
                "{label}: the frontmatter does not match the file kind"
            );
        }
    }

    // Six agents in two scopes, less the Cursor global file that does not exist,
    // and one file for each guidance-only agent.
    assert_eq!(seen, 11 + TARGETS.len());
}

#[test]
fn codex_and_crush_get_one_render_for_their_shared_project_agents_file() {
    let root = tempfile::TempDir::new().unwrap();
    let env = env_at(root.path());

    let codex = guidance_target(AgentKind::CodexCli, &project_scope(&root), &env)
        .unwrap()
        .unwrap();
    let crush = guidance_target(AgentKind::Crush, &project_scope(&root), &env)
        .unwrap()
        .unwrap();

    assert_eq!(codex.path, crush.path);
    assert_eq!(codex.format.hooks, GuidanceHooks::NoHook);
    assert_eq!(
        codex.render(RENDER_VERSION),
        crush.render(RENDER_VERSION),
        "the two agents must not rewrite each other's block"
    );
}

#[cfg(unix)]
#[test]
fn a_project_claude_file_that_links_to_agents_md_gets_the_text_of_agents_md() {
    let root = tempfile::TempDir::new().unwrap();
    std::fs::write(root.path().join("AGENTS.md"), "# Shared rules\n").unwrap();
    std::os::unix::fs::symlink("AGENTS.md", root.path().join("CLAUDE.md")).unwrap();
    let env = env_at(root.path());

    let claude = guidance_target(AgentKind::ClaudeCode, &project_scope(&root), &env)
        .unwrap()
        .unwrap();
    let codex = guidance_target(AgentKind::CodexCli, &project_scope(&root), &env)
        .unwrap()
        .unwrap();

    assert_eq!(claude.path, codex.path);
    assert_eq!(
        claude.render(RENDER_VERSION),
        codex.render(RENDER_VERSION),
        "a link and its target must get one text"
    );
    assert_eq!(claude.format.hooks, GuidanceHooks::NoHook);
}

#[test]
fn two_agents_whose_variables_name_one_folder_share_one_global_render() {
    let home = tempfile::TempDir::new().unwrap();
    let shared = home.path().join("shared-agent-folder");
    let crush_alone = InstructionEnv::from_lookup(Some(home.path().to_path_buf()), |_| None);
    let crush_with_codex = InstructionEnv::from_lookup(Some(home.path().to_path_buf()), |name| {
        matches!(name, "CODEX_HOME" | "CRUSH_CONFIG_DIR").then(|| shared.clone().into_os_string())
    });

    let alone = guidance_target(AgentKind::Crush, &GuidanceScope::Global, &crush_alone)
        .unwrap()
        .unwrap();
    let with_codex = guidance_target(AgentKind::Crush, &GuidanceScope::Global, &crush_with_codex)
        .unwrap()
        .unwrap();

    assert_eq!(alone.format.hooks, GuidanceHooks::Hook);
    assert_eq!(with_codex.path, shared.join("AGENTS.md"));
    assert_eq!(with_codex.format.hooks, GuidanceHooks::NoHook);
}

// ============================================================================
// Updating a file that exists
// ============================================================================

/// The Cursor rule file of the project `root`, as the selector returns it.
fn cursor_target(root: &tempfile::TempDir) -> GuidanceTarget {
    let target = guidance_target(
        AgentKind::Cursor,
        &project_scope(root),
        &env_at(root.path()),
    )
    .unwrap()
    .unwrap();
    std::fs::create_dir_all(target.path.parent().unwrap()).unwrap();
    target
}

fn claude_target(root: &tempfile::TempDir) -> GuidanceTarget {
    guidance_target(
        AgentKind::ClaudeCode,
        &project_scope(root),
        &env_at(root.path()),
    )
    .unwrap()
    .unwrap()
}

fn old_block() -> String {
    guidance_content("3.0.0", GuidanceHooks::Hook)
}

fn new_block() -> String {
    guidance_content(RENDER_VERSION, GuidanceHooks::Hook)
}

/// Write `text` to the file of `target`, update it, and return what happened
/// and what the file holds.
fn update(target: &GuidanceTarget, text: &str) -> (UpdateOutcome, String) {
    std::fs::write(&target.path, text).unwrap();
    let outcome = update_existing_guidance(target, text, RENDER_VERSION).unwrap();
    (outcome, std::fs::read_to_string(&target.path).unwrap())
}

#[test]
fn an_upgrade_leaves_a_skim_shaped_rule_file_with_exactly_one_frontmatter() {
    let root = tempfile::TempDir::new().unwrap();
    let target = cursor_target(&root);
    let doubled = format!(
        "{CURSOR_RULE_FRONTMATTER}{CURSOR_RULE_FRONTMATTER}{}\n",
        old_block()
    );

    let (outcome, after) = update(&target, &doubled);

    assert_eq!(outcome, UpdateOutcome::Replaced { changed: false });
    assert_eq!(after, format!("{CURSOR_RULE_FRONTMATTER}{}\n", new_block()));
    assert_eq!(after.matches("alwaysApply: true").count(), 1);
}

#[test]
fn an_upgrade_keeps_the_user_text_of_a_rule_file_byte_for_byte() {
    let root = tempfile::TempDir::new().unwrap();
    let target = cursor_target(&root);
    let user_before = "# My cursor rules\n\n  Keep   this\t spacing.\n\n";
    let user_after = "\n\nAfter the block.\n\n\n";
    let text = format!(
        "{CURSOR_RULE_FRONTMATTER}{user_before}{CURSOR_RULE_FRONTMATTER}{}{user_after}",
        old_block()
    );

    let (outcome, after) = update(&target, &text);

    assert_eq!(outcome, UpdateOutcome::Replaced { changed: false });
    assert_eq!(
        after,
        format!(
            "{CURSOR_RULE_FRONTMATTER}{user_before}{}{user_after}",
            new_block()
        )
    );
}

#[test]
fn a_rule_file_with_user_text_and_a_current_block_is_left_byte_unchanged() {
    let root = tempfile::TempDir::new().unwrap();
    let target = cursor_target(&root);
    let text = format!(
        "{CURSOR_RULE_FRONTMATTER}My notes.\n\n{}\n\nMore notes.\n",
        new_block()
    );

    let (outcome, after) = update(&target, &text);

    assert_eq!(outcome, UpdateOutcome::AlreadyCurrent);
    assert_eq!(after, text);
}

#[test]
fn a_rule_file_with_no_markers_is_left_unchanged_and_the_note_names_the_file() {
    let root = tempfile::TempDir::new().unwrap();
    let target = cursor_target(&root);
    let text = "---\nalwaysApply: true\n---\n\nMy own rules, no skim block.\n";

    let (outcome, after) = update(&target, text);

    assert_eq!(outcome, UpdateOutcome::Left(LeftReason::NoMarkers));
    assert_eq!(after, text);
    let note = outcome.message(&target.shown, RENDER_VERSION).unwrap();
    assert!(note.contains(".cursor/rules/skim.mdc"), "{note}");
}

#[test]
fn a_rule_file_with_a_broken_block_is_left_unchanged() {
    let root = tempfile::TempDir::new().unwrap();
    let target = cursor_target(&root);
    let text = format!("{CURSOR_RULE_FRONTMATTER}<!-- skim-start v3.0.0 -->\nno end marker\n");

    let (outcome, after) = update(&target, &text);

    assert_eq!(outcome, UpdateOutcome::Left(LeftReason::BrokenMarkers));
    assert_eq!(after, text);
}

#[test]
fn a_block_of_another_version_is_replaced_even_when_one_version_starts_with_the_other() {
    let root = tempfile::TempDir::new().unwrap();
    let target = claude_target(&root);
    // The binary is v3.0.1. A v3.0.10 block starts with the same characters.
    let newer = guidance_content("3.0.10", GuidanceHooks::Hook);

    let (outcome, after) = update(&target, &format!("# Mine\n\n{newer}\n"));

    assert_eq!(outcome, UpdateOutcome::Replaced { changed: false });
    assert_eq!(after, format!("# Mine\n\n{}\n", new_block()));
}

#[test]
fn a_changed_block_is_replaced_and_the_note_names_the_file() {
    let root = tempfile::TempDir::new().unwrap();
    let target = claude_target(&root);
    let edited = new_block().replace("Skim earns its cost", "Skim earns its keep");

    let (outcome, after) = update(&target, &format!("# Mine\n\n{edited}\n"));

    assert_eq!(outcome, UpdateOutcome::Replaced { changed: true });
    assert_eq!(after, format!("# Mine\n\n{}\n", new_block()));
    let note = outcome.message(&target.shown, RENDER_VERSION).unwrap();
    assert_eq!(
        note,
        "  Note: Replaced changed guidance in CLAUDE.md; review the diff before a commit"
    );
}

#[test]
fn a_broken_block_in_a_markdown_file_is_left_unchanged() {
    let root = tempfile::TempDir::new().unwrap();
    let target = claude_target(&root);
    let text = "<!-- skim-end -->\nsome content\n<!-- skim-start v1.0.0 -->\n";

    let (outcome, after) = update(&target, text);

    assert_eq!(outcome, UpdateOutcome::Left(LeftReason::BrokenMarkers));
    assert_eq!(after, text);
}

#[test]
fn a_current_block_in_a_markdown_file_is_left_byte_unchanged() {
    let root = tempfile::TempDir::new().unwrap();
    let target = claude_target(&root);
    let text = format!("# Mine\n\n{}\n\n# More\n", new_block());

    let (outcome, after) = update(&target, &text);

    assert_eq!(outcome, UpdateOutcome::AlreadyCurrent);
    assert_eq!(after, text);
}

#[test]
fn a_markdown_file_with_no_block_gets_one_appended() {
    let root = tempfile::TempDir::new().unwrap();
    let target = claude_target(&root);

    let (outcome, after) = update(&target, "# My Rules\n");

    assert_eq!(outcome, UpdateOutcome::Appended);
    assert_eq!(after, format!("# My Rules\n\n{}\n", new_block()));
}

// ============================================================================
// Removing the block
// ============================================================================

#[test]
fn uninstall_deletes_a_rule_file_that_is_skim_shaped() {
    let root = tempfile::TempDir::new().unwrap();
    let target = cursor_target(&root);
    std::fs::write(
        &target.path,
        format!(
            "{CURSOR_RULE_FRONTMATTER}{CURSOR_RULE_FRONTMATTER}{}\n",
            new_block()
        ),
    )
    .unwrap();

    strip_target(&target, &[]).unwrap();

    assert!(!target.path.exists());
}

#[test]
fn uninstall_strips_only_the_block_from_a_rule_file_with_user_text() {
    let root = tempfile::TempDir::new().unwrap();
    let target = cursor_target(&root);
    std::fs::write(
        &target.path,
        format!("{CURSOR_RULE_FRONTMATTER}{}\n\nMy notes.\n", new_block()),
    )
    .unwrap();

    strip_target(&target, &[]).unwrap();

    assert_eq!(
        std::fs::read_to_string(&target.path).unwrap(),
        format!("{}\n\nMy notes.\n", CURSOR_RULE_FRONTMATTER.trim_end())
    );
}

#[test]
fn uninstall_leaves_a_rule_file_with_no_markers_alone() {
    let root = tempfile::TempDir::new().unwrap();
    let target = cursor_target(&root);
    let text = "---\nalwaysApply: true\n---\n\nMy own rules.\n";
    std::fs::write(&target.path, text).unwrap();

    strip_target(&target, &[]).unwrap();

    assert_eq!(std::fs::read_to_string(&target.path).unwrap(), text);
}

#[test]
fn uninstall_deletes_a_markdown_file_that_holds_only_the_block() {
    let root = tempfile::TempDir::new().unwrap();
    let target = claude_target(&root);
    std::fs::write(&target.path, format!("{}\n", new_block())).unwrap();

    strip_target(&target, &[]).unwrap();

    assert!(!target.path.exists());
}

// ============================================================================
// A file that other agents share
// ============================================================================

/// The agents that share the file of `agent` in the project `root`.
fn sharing_in_project(agent: AgentKind, root: &tempfile::TempDir) -> Vec<AgentKind> {
    agents_sharing_file(agent, &project_scope(root), &env_at(root.path()))
}

#[test]
fn codex_and_crush_share_the_project_agents_file_and_claude_shares_nothing() {
    let root = tempfile::TempDir::new().unwrap();

    assert_eq!(
        sharing_in_project(AgentKind::CodexCli, &root),
        [AgentKind::Crush]
    );
    assert_eq!(
        sharing_in_project(AgentKind::Crush, &root),
        [AgentKind::CodexCli]
    );
    assert_eq!(sharing_in_project(AgentKind::ClaudeCode, &root), []);
}

#[cfg(unix)]
#[test]
fn a_claude_file_that_links_to_agents_md_is_shared_with_codex_and_crush() {
    let root = tempfile::TempDir::new().unwrap();
    std::fs::write(root.path().join("AGENTS.md"), "shared text\n").unwrap();
    std::os::unix::fs::symlink("AGENTS.md", root.path().join("CLAUDE.md")).unwrap();

    assert_eq!(
        sharing_in_project(AgentKind::ClaudeCode, &root),
        [AgentKind::CodexCli, AgentKind::Crush]
    );
}

#[test]
fn agents_share_a_global_file_only_when_their_variables_name_one_folder() {
    let home = tempfile::TempDir::new().unwrap();
    let shared = home.path().join("shared-agent-folder");
    let apart = InstructionEnv::from_lookup(Some(home.path().to_path_buf()), |_| None);
    let together = InstructionEnv::from_lookup(Some(home.path().to_path_buf()), |name| {
        matches!(name, "CODEX_HOME" | "CRUSH_CONFIG_DIR").then(|| shared.clone().into_os_string())
    });

    assert_eq!(
        agents_sharing_file(AgentKind::CodexCli, &GuidanceScope::Global, &apart),
        []
    );
    assert_eq!(
        agents_sharing_file(AgentKind::CodexCli, &GuidanceScope::Global, &together),
        [AgentKind::Crush]
    );
}

/// A project `AGENTS.md` that holds the no-hook block and the text of the user.
fn shared_agents_target(root: &tempfile::TempDir) -> GuidanceTarget {
    guidance_target(AgentKind::Crush, &project_scope(root), &env_at(root.path()))
        .unwrap()
        .unwrap()
}

#[test]
fn uninstall_keeps_a_block_while_another_agent_still_uses_the_file() {
    let root = tempfile::TempDir::new().unwrap();
    let target = shared_agents_target(&root);
    for text in [
        format!("{}\n", target.block(RENDER_VERSION)),
        format!("# My notes\n\n{}\n", target.block(RENDER_VERSION)),
    ] {
        std::fs::write(&target.path, &text).unwrap();

        strip_target(&target, &[AgentKind::CodexCli]).unwrap();

        assert_eq!(std::fs::read_to_string(&target.path).unwrap(), text);
    }
}

#[test]
fn uninstall_removes_the_block_when_no_other_agent_uses_the_file() {
    let root = tempfile::TempDir::new().unwrap();
    let target = shared_agents_target(&root);
    std::fs::write(
        &target.path,
        format!("# My notes\n\n{}\n", target.block(RENDER_VERSION)),
    )
    .unwrap();

    strip_target(&target, &[]).unwrap();

    let after = std::fs::read_to_string(&target.path).unwrap();
    assert!(!after.contains("skim-start"), "{after}");
    assert!(after.contains("# My notes"), "{after}");
}

#[test]
fn the_kept_note_names_the_file_and_the_agents_that_use_it() {
    let shown = std::path::Path::new("AGENTS.md");

    assert_eq!(
        kept_shared_note(shown, &[AgentKind::CodexCli]),
        "  Note: kept guidance in AGENTS.md; Codex CLI still uses it"
    );
    assert_eq!(
        kept_shared_note(shown, &[AgentKind::CodexCli, AgentKind::Crush]),
        "  Note: kept guidance in AGENTS.md; Codex CLI, Crush still use it"
    );
}
