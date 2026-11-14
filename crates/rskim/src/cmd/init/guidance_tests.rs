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
