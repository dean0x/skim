//! Tests for the guidance path checks (guidance.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::path::PathBuf;

use super::{
    GuidanceFormat, GuidanceScope, PathRefusal, TargetRefusal, guidance_target, inject_guidance,
    remove_guidance,
};
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

    let error = remove_guidance(AgentKind::GeminiCli, true, &env).unwrap_err();

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
    assert_eq!(target.format, GuidanceFormat::Markdown);
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
    assert_eq!(target.format, GuidanceFormat::Mdc);
}

#[test]
fn cursor_has_no_global_target() {
    let env = env_with_all(RELATIVE_DIR);

    let target = guidance_target(AgentKind::Cursor, &GuidanceScope::Global, &env).unwrap();

    assert_eq!(target, None);
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
        super::super::helpers::guidance_content("9.9.9")
    );
    assert_eq!(
        rule_file.render("9.9.9"),
        super::super::helpers::guidance_content_mdc("9.9.9")
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

    remove_guidance(AgentKind::ClaudeCode, true, &env).unwrap();

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
    remove_guidance(AgentKind::ClaudeCode, true, &env).unwrap();

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
    remove_guidance(AgentKind::ClaudeCode, true, &env_at(home.path())).unwrap();

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
    remove_guidance(AgentKind::ClaudeCode, true, &env).unwrap();

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
