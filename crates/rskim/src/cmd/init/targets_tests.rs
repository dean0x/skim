//! Tests for the guidance-only agents (targets.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use tempfile::TempDir;

use super::{
    GuidanceOnlyAgent, TARGETS, TargetsRun, agent_names, install_targets, install_with,
    remove_targets, remove_with, targets_apply,
};
use crate::cmd::init::flags::{InitFlags, PermissionsTier};
use crate::cmd::init::guidance::{inject_into, remove_from};
use crate::cmd::init::guidance_block::GuidanceHooks;
use crate::cmd::init::helpers::guidance_content;
use crate::cmd::session::{AgentKind, InstructionEnv};

// ============================================================================
// Helpers
// ============================================================================

/// The flags of a plain auto-detect install: no named agent, global scope.
fn flags() -> InitFlags {
    InitFlags {
        project: false,
        yes: true,
        dry_run: false,
        uninstall: false,
        force: false,
        no_guidance: false,
        dev: false,
        agent: None,
        wrappers: None,
        permissions: None,
        permissions_tier: PermissionsTier::Seed,
    }
}

fn env_at(home: &Path) -> InstructionEnv {
    InstructionEnv::from_lookup(Some(home.to_path_buf()), |_| None)
}

/// A home folder that holds the root folder of each agent in `roots`.
fn home_with(roots: &[&str]) -> TempDir {
    let home = TempDir::new().unwrap();
    for root in roots {
        std::fs::create_dir_all(home.path().join(root)).unwrap();
    }
    home
}

/// A home folder that holds the root folder of every target agent.
fn home_with_every_root() -> TempDir {
    let roots: Vec<&str> = TARGETS.iter().map(|agent| agent.root).collect();
    home_with(&roots)
}

fn file_of(home: &Path, agent: &GuidanceOnlyAgent) -> std::path::PathBuf {
    home.join(agent.dir).join(agent.file)
}

fn agent(name: &str) -> &'static GuidanceOnlyAgent {
    TARGETS.iter().find(|a| a.name == name).unwrap()
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
            if path.is_dir() {
                out.insert(name, None);
                walk(root, &path, out);
            } else {
                out.insert(name, Some(std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// Install guidance as this binary writes it, then return the written text.
fn installed_text(home: &TempDir, agent: &GuidanceOnlyAgent) -> String {
    let run = install_targets(&flags(), &env_at(home.path()));
    assert_eq!(run.failed, 0, "the install must not fail");
    std::fs::read_to_string(file_of(home.path(), agent)).unwrap()
}

const USER_TEXT: &str = "# My own steering\n\nKeep this line.\n";

/// A skim block of an old version.
const STALE_BLOCK: &str = "<!-- skim-start v0.0.1 -->\nold text\n<!-- skim-end -->\n";

// ============================================================================
// The table
// ============================================================================

#[test]
fn the_table_names_the_five_agents_with_their_folders_and_frontmatter() {
    let rows: Vec<_> = TARGETS
        .iter()
        .map(|a| (a.name, a.root, a.dir, a.file, a.frontmatter))
        .collect();

    assert_eq!(
        rows,
        vec![
            (
                "Kiro",
                ".kiro",
                ".kiro/steering",
                "skim.md",
                Some("---\ninclusion: always\n---\n\n")
            ),
            ("Roo Code", ".roo", ".roo/rules", "skim.md", None),
            ("Cline", ".cline", ".cline/rules", "skim.md", None),
            ("Augment", ".augment", ".augment/rules", "skim.md", None),
            (
                "Windsurf",
                ".devin",
                ".devin/rules",
                "skim.md",
                Some("---\ntrigger: always_on\n---\n\n")
            ),
        ]
    );
}

/// Files that an agent reads before it reads other instruction files, or that
/// several agents share. A row that names one could hide the user's own files.
const FIRST_MATCH_FILES: &[&str] = &[
    "AGENTS.md",
    "AGENT.md",
    "CLAUDE.md",
    "GEMINI.md",
    "WARP.md",
    "global_rules.md",
    "copilot-instructions.md",
    ".rules",
    ".cursorrules",
    ".windsurfrules",
    ".clinerules",
    ".roorules",
    ".augment-guidelines",
];

#[test]
fn no_row_names_a_file_that_an_agent_reads_before_other_instruction_files() {
    for agent in TARGETS {
        assert!(
            !FIRST_MATCH_FILES.contains(&agent.file),
            "{}: {} is a shared or first-match file",
            agent.name,
            agent.file
        );
        assert_eq!(agent.file, "skim.md", "{}: skim owns skim.md", agent.name);
    }
}

#[test]
fn every_row_stays_inside_its_root_in_the_home_folder() {
    let mut files = std::collections::BTreeSet::new();
    for agent in TARGETS {
        for part in [agent.root, agent.dir, agent.file] {
            let path = Path::new(part);
            assert!(path.is_relative(), "{}: {part} is absolute", agent.name);
            assert!(
                path.components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
                "{}: {part} has a special component",
                agent.name
            );
        }
        let dir = Path::new(agent.dir);
        assert!(
            dir.starts_with(agent.root) && dir != Path::new(agent.root),
            "{}: the rules folder {} is not inside the root {}",
            agent.name,
            agent.dir,
            agent.root
        );
        assert!(
            files.insert(dir.join(agent.file)),
            "{}: two rows write one file",
            agent.name
        );
    }
}

#[test]
fn the_agent_names_in_a_message_list_every_agent_and_every_target() {
    let names = agent_names(&flags());

    for agent in AgentKind::all_supported() {
        assert!(names.contains(agent.display_name()), "{names}");
    }
    for agent in TARGETS {
        assert!(names.contains(agent.name), "{names}");
    }
}

#[test]
fn the_agent_names_leave_out_the_targets_when_they_do_not_run() {
    let names = agent_names(&InitFlags {
        project: true,
        ..flags()
    });

    assert!(names.contains("Claude Code"), "{names}");
    for agent in TARGETS {
        assert!(!names.contains(agent.name), "{names}");
    }
}

// ============================================================================
// Which invocations run the targets
// ============================================================================

#[test]
fn only_an_auto_detect_global_invocation_with_guidance_runs_the_targets() {
    assert!(targets_apply(&flags()));
    assert!(!targets_apply(&InitFlags {
        agent: Some(AgentKind::ClaudeCode),
        ..flags()
    }));
    assert!(!targets_apply(&InitFlags {
        project: true,
        ..flags()
    }));
    assert!(!targets_apply(&InitFlags {
        no_guidance: true,
        ..flags()
    }));
}

#[test]
fn install_writes_nothing_when_the_flags_skip_the_targets() {
    for skipping in [
        InitFlags {
            agent: Some(AgentKind::ClaudeCode),
            ..flags()
        },
        InitFlags {
            project: true,
            ..flags()
        },
        InitFlags {
            no_guidance: true,
            ..flags()
        },
    ] {
        let home = home_with_every_root();
        let before = snapshot(home.path());

        let run = install_targets(&skipping, &env_at(home.path()));

        assert_eq!(run, TargetsRun::default());
        assert_eq!(snapshot(home.path()), before, "{skipping:?}");
    }
}

#[test]
fn uninstall_removes_nothing_when_the_flags_skip_the_targets() {
    for skipping in [
        InitFlags {
            agent: Some(AgentKind::ClaudeCode),
            ..flags()
        },
        InitFlags {
            project: true,
            ..flags()
        },
        InitFlags {
            no_guidance: true,
            ..flags()
        },
    ] {
        let home = home_with_every_root();
        install_targets(&flags(), &env_at(home.path()));
        let before = snapshot(home.path());

        let run = remove_targets(&skipping, &env_at(home.path()));

        assert_eq!(run, TargetsRun::default());
        assert_eq!(snapshot(home.path()), before, "{skipping:?}");
    }
}

// ============================================================================
// Install
// ============================================================================

#[test]
fn install_writes_each_installed_agent_a_file_with_its_frontmatter() {
    let home = home_with_every_root();

    let run = install_targets(&flags(), &env_at(home.path()));

    assert_eq!(
        run,
        TargetsRun {
            handled: TARGETS.len(),
            failed: 0
        }
    );
    for agent in TARGETS {
        let text = std::fs::read_to_string(file_of(home.path(), agent)).unwrap();
        let version = env!("CARGO_PKG_VERSION");
        let expected = format!(
            "{}{}\n",
            agent.frontmatter.unwrap_or(""),
            guidance_content(version, GuidanceHooks::NoHook)
        );
        assert_eq!(text, expected, "{}", agent.name);
        assert!(
            !text.contains("### Command wrapping"),
            "{}: a hook claim in a file of an agent with no hook",
            agent.name
        );
    }
}

#[test]
fn install_gives_kiro_and_windsurf_their_frontmatter_and_the_others_none() {
    let home = home_with_every_root();
    install_targets(&flags(), &env_at(home.path()));

    let text = |name: &str| std::fs::read_to_string(file_of(home.path(), agent(name))).unwrap();

    assert!(text("Kiro").starts_with("---\ninclusion: always\n---\n\n<!-- skim-start"));
    assert!(text("Windsurf").starts_with("---\ntrigger: always_on\n---\n\n<!-- skim-start"));
    for name in ["Roo Code", "Cline", "Augment"] {
        assert!(text(name).starts_with("<!-- skim-start"), "{name}");
    }
}

#[test]
fn install_never_creates_a_root_folder_that_is_missing() {
    let home = home_with(&[".roo"]);

    let run = install_targets(&flags(), &env_at(home.path()));

    assert_eq!(run.handled, 1);
    assert!(file_of(home.path(), agent("Roo Code")).is_file());
    for missing in TARGETS.iter().filter(|a| a.name != "Roo Code") {
        assert!(
            !home.path().join(missing.root).exists(),
            "{}: the root folder was created",
            missing.name
        );
    }
}

#[test]
fn install_with_no_root_folder_changes_nothing() {
    let home = home_with(&[]);

    let run = install_targets(&flags(), &env_at(home.path()));

    assert_eq!(run, TargetsRun::default());
    assert!(snapshot(home.path()).is_empty());
}

#[test]
fn install_ignores_a_root_that_is_a_file() {
    let home = home_with(&[]);
    std::fs::write(home.path().join(".kiro"), "not a folder").unwrap();

    let run = install_targets(&flags(), &env_at(home.path()));

    assert_eq!(run, TargetsRun::default());
    assert_eq!(
        std::fs::read_to_string(home.path().join(".kiro")).unwrap(),
        "not a folder"
    );
}

#[test]
fn install_creates_only_the_rules_folder_inside_the_root() {
    let home = home_with(&[".kiro"]);

    install_targets(&flags(), &env_at(home.path()));

    let created: Vec<_> = snapshot(home.path()).into_keys().collect();
    assert_eq!(
        created,
        vec![".kiro", ".kiro/steering", ".kiro/steering/skim.md"]
    );
}

#[test]
fn a_dry_run_install_writes_nothing_and_still_counts_the_agent() {
    let home = home_with_every_root();
    let before = snapshot(home.path());

    let run = install_targets(
        &InitFlags {
            dry_run: true,
            ..flags()
        },
        &env_at(home.path()),
    );

    assert_eq!(run.handled, TARGETS.len());
    assert_eq!(run.failed, 0);
    assert_eq!(snapshot(home.path()), before);
}

#[test]
fn a_second_install_leaves_the_files_byte_unchanged() {
    let home = home_with_every_root();
    install_targets(&flags(), &env_at(home.path()));
    let before = snapshot(home.path());

    let run = install_targets(&flags(), &env_at(home.path()));

    assert_eq!(run.failed, 0);
    assert_eq!(snapshot(home.path()), before);
}

#[test]
fn install_replaces_a_stale_block_and_keeps_one_frontmatter() {
    let home = home_with(&[".kiro"]);
    let kiro = agent("Kiro");
    let file = file_of(home.path(), kiro);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, format!("{}{STALE_BLOCK}", kiro.frontmatter.unwrap())).unwrap();

    install_targets(&flags(), &env_at(home.path()));

    let text = std::fs::read_to_string(&file).unwrap();
    assert_eq!(text.matches("inclusion: always").count(), 1);
    assert!(!text.contains("old text"));
    assert!(text.contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn install_leaves_a_file_of_the_user_that_holds_no_skim_markers() {
    let home = home_with(&[".kiro"]);
    let file = file_of(home.path(), agent("Kiro"));
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, USER_TEXT).unwrap();

    let run = install_targets(&flags(), &env_at(home.path()));

    assert_eq!(run.failed, 0, "a refused file is a note, not a failure");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), USER_TEXT);
}

#[test]
fn a_failed_target_is_counted_and_the_other_targets_still_run() {
    let home = home_with(&[".kiro", ".roo", ".cline"]);
    let kiro_folder = home.path().join(".kiro");

    // The write for Kiro fails; the writer is the real one for the others.
    let run = install_with(&flags(), &env_at(home.path()), |target| {
        if target.path.starts_with(&kiro_folder) {
            anyhow::bail!("no space left on the device")
        }
        inject_into(target, true)
    });

    assert_eq!(
        run,
        TargetsRun {
            handled: 3,
            failed: 1
        }
    );
    assert!(!file_of(home.path(), agent("Kiro")).exists());
    assert!(file_of(home.path(), agent("Roo Code")).is_file());
    assert!(file_of(home.path(), agent("Cline")).is_file());
}

#[test]
fn a_failed_removal_is_counted_and_the_other_targets_are_still_removed() {
    let home = home_with(&[".kiro", ".roo", ".cline"]);
    install_targets(&flags(), &env_at(home.path()));
    let kiro_folder = home.path().join(".kiro");

    let run = remove_with(&flags(), &env_at(home.path()), |target| {
        if target.path.starts_with(&kiro_folder) {
            anyhow::bail!("the file is busy")
        }
        remove_from(target, &[])
    });

    assert_eq!(
        run,
        TargetsRun {
            handled: 3,
            failed: 1
        }
    );
    assert!(file_of(home.path(), agent("Kiro")).is_file());
    assert!(!file_of(home.path(), agent("Roo Code")).exists());
    assert!(!file_of(home.path(), agent("Cline")).exists());
}

#[test]
fn a_path_the_policy_refuses_is_a_note_and_not_a_failure() {
    // A file where the Kiro rules folder must go: the file cannot be checked.
    let home = home_with(&[".kiro", ".roo"]);
    std::fs::write(home.path().join(".kiro/steering"), "in the way").unwrap();

    let run = install_targets(&flags(), &env_at(home.path()));

    assert_eq!(
        run,
        TargetsRun {
            handled: 2,
            failed: 0
        }
    );
    assert!(file_of(home.path(), agent("Roo Code")).is_file());
    assert_eq!(
        std::fs::read_to_string(home.path().join(".kiro/steering")).unwrap(),
        "in the way"
    );
}

#[cfg(unix)]
#[test]
fn install_leaves_a_file_that_is_a_link_to_a_folder_alone() {
    let home = home_with(&[".kiro"]);
    let outside = TempDir::new().unwrap();
    let file = file_of(home.path(), agent("Kiro"));
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(outside.path(), &file).unwrap();

    let run = install_targets(&flags(), &env_at(home.path()));

    assert_eq!(run.failed, 0, "a refused link is a note, not a failure");
    assert!(snapshot(outside.path()).is_empty());
    assert!(
        std::fs::symlink_metadata(&file)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[cfg(unix)]
#[test]
fn install_writes_through_a_link_to_a_regular_file_and_keeps_the_link() {
    let home = home_with(&[".roo"]);
    let elsewhere = TempDir::new().unwrap();
    let real = elsewhere.path().join("real-skim.md");
    std::fs::write(&real, STALE_BLOCK).unwrap();
    let file = file_of(home.path(), agent("Roo Code"));
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real, &file).unwrap();

    install_targets(&flags(), &env_at(home.path()));

    let text = std::fs::read_to_string(&real).unwrap();
    assert!(text.contains(env!("CARGO_PKG_VERSION")), "{text}");
    assert!(!text.contains("old text"), "{text}");
    assert!(
        std::fs::symlink_metadata(&file)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn no_home_folder_means_no_target() {
    let env = InstructionEnv::from_lookup(None, |_| None);

    assert_eq!(install_targets(&flags(), &env), TargetsRun::default());
    assert_eq!(remove_targets(&flags(), &env), TargetsRun::default());
}

// ============================================================================
// Uninstall
// ============================================================================

#[test]
fn uninstall_deletes_each_file_that_holds_only_the_skim_block() {
    let home = home_with_every_root();
    install_targets(&flags(), &env_at(home.path()));

    let run = remove_targets(&flags(), &env_at(home.path()));

    assert_eq!(
        run,
        TargetsRun {
            handled: TARGETS.len(),
            failed: 0
        }
    );
    for agent in TARGETS {
        assert!(!file_of(home.path(), agent).exists(), "{}", agent.name);
        assert!(
            home.path().join(agent.dir).is_dir() && home.path().join(agent.root).is_dir(),
            "{}: the folders stay",
            agent.name
        );
    }
}

#[test]
fn uninstall_keeps_the_text_of_the_user_and_takes_out_only_the_block() {
    let home = home_with(&[".kiro"]);
    let kiro = agent("Kiro");
    let text = installed_text(&home, kiro);
    let file = file_of(home.path(), kiro);
    std::fs::write(&file, format!("{text}\n{USER_TEXT}")).unwrap();

    let run = remove_targets(&flags(), &env_at(home.path()));

    assert_eq!(run.failed, 0);
    let after = std::fs::read_to_string(&file).unwrap();
    assert!(after.contains(USER_TEXT.trim_end()), "{after}");
    assert!(!after.contains("skim-start"), "{after}");
}

#[test]
fn uninstall_leaves_a_file_without_skim_markers_alone() {
    let home = home_with(&[".kiro", ".roo"]);
    let file = file_of(home.path(), agent("Kiro"));
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, USER_TEXT).unwrap();

    let run = remove_targets(&flags(), &env_at(home.path()));

    assert_eq!(run, TargetsRun::default());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), USER_TEXT);
}

#[test]
fn uninstall_with_no_files_changes_nothing() {
    let home = home_with_every_root();
    let before = snapshot(home.path());

    let run = remove_targets(&flags(), &env_at(home.path()));

    assert_eq!(run, TargetsRun::default());
    assert_eq!(snapshot(home.path()), before);
}

#[test]
fn a_dry_run_uninstall_removes_nothing_and_still_counts_the_file() {
    let home = home_with_every_root();
    install_targets(&flags(), &env_at(home.path()));
    let before = snapshot(home.path());

    let run = remove_targets(
        &InitFlags {
            dry_run: true,
            ..flags()
        },
        &env_at(home.path()),
    );

    assert_eq!(run.handled, TARGETS.len());
    assert_eq!(snapshot(home.path()), before);
}

#[cfg(unix)]
#[test]
fn uninstall_leaves_a_file_that_is_a_link_to_a_folder_alone() {
    let home = home_with(&[".kiro"]);
    let outside = TempDir::new().unwrap();
    std::fs::write(outside.path().join("keep.md"), "keep").unwrap();
    let file = file_of(home.path(), agent("Kiro"));
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(outside.path(), &file).unwrap();

    let run = remove_targets(&flags(), &env_at(home.path()));

    assert_eq!(run, TargetsRun::default());
    assert_eq!(
        std::fs::read_to_string(outside.path().join("keep.md")).unwrap(),
        "keep"
    );
}

// ============================================================================
// The exit code
// ============================================================================

#[test]
fn a_failed_target_turns_a_success_into_a_failure_and_nothing_else_changes() {
    let failed = TargetsRun {
        handled: 1,
        failed: 1,
    };
    let fine = TargetsRun {
        handled: 1,
        failed: 0,
    };

    assert_eq!(failed.exit_code_with(ExitCode::SUCCESS), ExitCode::FAILURE);
    assert_eq!(fine.exit_code_with(ExitCode::SUCCESS), ExitCode::SUCCESS);
    assert_eq!(fine.exit_code_with(ExitCode::FAILURE), ExitCode::FAILURE);
}
