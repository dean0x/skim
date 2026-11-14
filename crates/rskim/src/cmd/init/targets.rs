//! Guidance-only agents for `skim init`.
//!
//! Some agents read rules from a folder in the home folder but have no hook
//! that skim can install: Kiro, Roo Code, Cline, Augment and Windsurf. For these
//! agents skim writes one guidance file and nothing else.
//!
//! - **Where.** [`TARGETS`] lists each agent: the folder that shows the agent is
//!   installed (`root`), the rules folder inside it (`dir`) and the file skim
//!   owns (`file`). Paths come from the home folder only. These agents have no
//!   variable that moves the folder.
//! - **When.** A file is written only when `root` exists. Skim creates the rules
//!   folder inside `root`, never `root` itself.
//! - **What.** The file is owned by skim and has no hook text. It goes through
//!   the one writer and the one remover of `guidance.rs`, so the currency check,
//!   the owned-file rules and the link rules are the same as for an agent.
//! - **Which runs.** Only auto-detect `skim init` and `skim init --uninstall`
//!   run the targets. They do not run under `--agent`, `--project` or
//!   `--no-guidance`.
//!
//! No row names a file that an agent reads before it reads other instruction
//! files (`AGENTS.md`, `global_rules.md`): such a file could hide the user's own.

use std::process::ExitCode;

use super::flags::InitFlags;
use super::guidance::{GuidanceTarget, block_is_current, holds_block, inject_into, remove_from};
use super::guidance_block::{GuidanceFormat, GuidanceHooks};
use super::guidance_path::{GuidanceScope, PathRefusal, resolve_guidance_path};
use super::helpers::confirm_proceed;
use crate::cmd::session::{AgentKind, InstructionEnv};

/// An agent that skim gives a guidance file and no hook.
#[derive(Debug, Clone, Copy)]
pub(super) struct GuidanceOnlyAgent {
    /// The name shown in messages.
    pub(super) name: &'static str,
    /// The folder in the home folder that shows the agent is installed.
    pub(super) root: &'static str,
    /// The rules folder, relative to the home folder. It lies inside `root`.
    pub(super) dir: &'static str,
    /// The name of the file skim owns in `dir`.
    pub(super) file: &'static str,
    /// The frontmatter the agent needs to load the file in every session.
    pub(super) frontmatter: Option<&'static str>,
}

/// The guidance-only agents. Every row is global scope and skim-owned.
pub(super) const TARGETS: &[GuidanceOnlyAgent] = &[
    GuidanceOnlyAgent {
        name: "Kiro",
        root: ".kiro",
        dir: ".kiro/steering",
        file: "skim.md",
        frontmatter: Some("---\ninclusion: always\n---\n\n"),
    },
    GuidanceOnlyAgent {
        name: "Roo Code",
        root: ".roo",
        dir: ".roo/rules",
        file: "skim.md",
        frontmatter: None,
    },
    GuidanceOnlyAgent {
        name: "Cline",
        root: ".cline",
        dir: ".cline/rules",
        file: "skim.md",
        frontmatter: None,
    },
    GuidanceOnlyAgent {
        name: "Augment",
        root: ".augment",
        dir: ".augment/rules",
        file: "skim.md",
        frontmatter: None,
    },
    GuidanceOnlyAgent {
        name: "Windsurf",
        root: ".devin",
        dir: ".devin/rules",
        file: "skim.md",
        frontmatter: Some("---\ntrigger: always_on\n---\n\n"),
    },
];

impl GuidanceOnlyAgent {
    /// The layout of the file: owned by skim, and no hook text.
    pub(super) const fn format(&self) -> GuidanceFormat {
        GuidanceFormat {
            frontmatter: self.frontmatter,
            owned: true,
            hooks: GuidanceHooks::NoHook,
        }
    }

    /// The guidance file of this agent after the path policy.
    ///
    /// Returns `None` when the agent is not installed: there is no home folder,
    /// or `root` is not a folder in it. Reads the file system and changes
    /// nothing.
    pub(super) fn resolve(
        &self,
        env: &InstructionEnv,
    ) -> Option<Result<GuidanceTarget, PathRefusal>> {
        let home = env.home_dir.as_deref()?;
        if !home.join(self.root).is_dir() {
            return None;
        }
        let file = home.join(self.dir).join(self.file);
        Some(
            resolve_guidance_path(&file, &GuidanceScope::Global).map(|resolved| GuidanceTarget {
                path: resolved.path,
                shown: resolved.shown,
                format: self.format(),
            }),
        )
    }
}

/// Whether the targets run for `flags`.
///
/// They run for an auto-detect invocation. A named agent, the project scope and
/// `--no-guidance` each leave them alone.
pub(super) fn targets_apply(flags: &InitFlags) -> bool {
    flags.agent.is_none() && !flags.project && !flags.no_guidance
}

/// The names of the agents that an invocation with `flags` looks for, for the
/// "no agents" messages: the agents with a hook, then the targets when they run.
pub(super) fn agent_names(flags: &InitFlags) -> String {
    let mut names: Vec<&str> = AgentKind::all_supported()
        .iter()
        .map(AgentKind::display_name)
        .collect();
    if targets_apply(flags) {
        names.extend(TARGETS.iter().map(|agent| agent.name));
    }
    names.join(", ")
}

/// What a pass over the targets found and did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct TargetsRun {
    /// Targets the pass had work for: installed agents on install, files that
    /// hold a skim block on uninstall.
    pub(super) handled: usize,
    /// Targets whose write or removal failed.
    pub(super) failed: usize,
}

impl TargetsRun {
    /// The exit code of the command: `code` of the agents, or a failure when a
    /// target failed.
    pub(super) fn exit_code_with(self, code: ExitCode) -> ExitCode {
        if self.failed > 0 {
            ExitCode::FAILURE
        } else {
            code
        }
    }
}

/// Write the guidance file of each installed target agent.
///
/// With `--dry-run` it prints what it would write and writes nothing. A target
/// that fails prints a line and is counted; the next target still runs. A path
/// the policy refuses prints a note and is not a failure.
pub(super) fn install_targets(flags: &InitFlags, env: &InstructionEnv) -> TargetsRun {
    install_with(flags, env, |target| inject_into(target, true))
}

/// [`install_targets`] with the writer passed in, so a test can make one write
/// fail.
fn install_with(
    flags: &InitFlags,
    env: &InstructionEnv,
    write: impl Fn(&GuidanceTarget) -> anyhow::Result<()>,
) -> TargetsRun {
    let mut run = TargetsRun::default();
    if !targets_apply(flags) {
        return run;
    }
    let version = env!("CARGO_PKG_VERSION");
    for agent in TARGETS {
        let Some(resolved) = agent.resolve(env) else {
            continue;
        };
        run.handled += 1;
        println!();
        println!("  skim init -- {} guidance setup", agent.name);
        println!();
        let target = match resolved {
            Ok(target) => target,
            Err(refusal) => {
                println!("  Note: {refusal}");
                continue;
            }
        };
        if flags.dry_run {
            let shown = target.shown.display();
            if block_is_current(&target, version) {
                println!("  [dry-run] Guidance in {shown} is already current");
            } else {
                println!("  [dry-run] Would inject guidance into {shown}");
            }
        } else if let Err(error) = write(&target) {
            eprintln!("  ✗ {}: guidance failed — {error}", agent.name);
            run.failed += 1;
        }
    }
    run
}

/// Remove the guidance file of each target agent that has one.
///
/// Only a file that holds a skim block is touched, and the remover decides
/// whether the whole file goes or only the block. Unless `--yes` or `--dry-run`
/// is given, it lists the files and asks first. With `--dry-run` it prints what
/// it would remove and removes nothing.
pub(super) fn remove_targets(flags: &InitFlags, env: &InstructionEnv) -> TargetsRun {
    remove_with(flags, env, |target| remove_from(target, &[]))
}

/// [`remove_targets`] with the remover passed in, so a test can make one removal
/// fail.
fn remove_with(
    flags: &InitFlags,
    env: &InstructionEnv,
    remove: impl Fn(&GuidanceTarget) -> anyhow::Result<()>,
) -> TargetsRun {
    let mut run = TargetsRun::default();
    if !targets_apply(flags) {
        return run;
    }
    let mut found = Vec::new();
    for agent in TARGETS {
        match agent.resolve(env) {
            Some(Ok(target)) if holds_block(&target) => found.push((agent, target)),
            Some(Err(refusal)) => println!("  Note: {refusal}"),
            Some(Ok(_)) | None => {}
        }
    }
    run.handled = found.len();
    if found.is_empty() {
        return run;
    }

    if !flags.yes && !flags.dry_run {
        println!();
        println!("  skim init --uninstall");
        println!();
        for (_, target) in &found {
            println!("    * Remove guidance from {}", target.shown.display());
        }
        println!();
        match confirm_proceed() {
            Ok(true) => {}
            Ok(false) => {
                println!("  Cancelled.");
                return run;
            }
            Err(error) => {
                eprintln!("  ✗ guidance-only agents: could not ask for confirmation — {error}");
                run.failed += 1;
                return run;
            }
        }
    }

    for (agent, target) in &found {
        if flags.dry_run {
            println!(
                "  [dry-run] Would remove guidance from {}",
                target.shown.display()
            );
        } else if let Err(error) = remove(target) {
            eprintln!("  ✗ {}: guidance removal failed — {error}", agent.name);
            run.failed += 1;
        }
    }
    run
}

#[cfg(test)]
#[path = "targets_tests.rs"]
mod tests;
