//! The guidance files that `skim doctor` reports.
//!
//! [`guidance_report`] looks at every file where skim may have written guidance:
//! the global file of each agent, the guidance-only targets and the project file
//! of each agent. It returns one [`GuidanceFact`] for each file that holds a skim
//! block and for each file it cannot check. A file with no skim block, and a
//! file that does not exist, give no fact.
//!
//! The report is read-only. It resolves each path with the path policy of the
//! installer and reads each file with the one bounded read of `guidance.rs`, so
//! doctor and `skim init` agree on which file is meant and on what its block is.
//! The state of a block comes from `block_status`, the one currency predicate.
//!
//! Two agents that read one file give one fact with two readers. The test is on
//! the resolved path, so a link and the file it names count as one file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::guidance::{
    GuidanceTarget, MAX_INSTRUCTION_FILE_SIZE, TargetRefusal, guidance_target,
    read_instruction_text,
};
use super::guidance_block::{BlockStatus, Marker, block_status};
use super::guidance_path::{GlobalPathRefusal, GuidanceScope, PathRefusal};
use super::targets::TARGETS;
use crate::cmd::safe_fs::ReadFailure;
use crate::cmd::session::{AgentKind, InstructionEnv};

/// Where a guidance file lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileScope {
    /// In the home folder (or the folder a config variable names).
    Global,
    /// In the project folder, the current folder of the command.
    Project,
}

/// Who reads a guidance file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reader {
    /// A supported agent, with or without a hook.
    Agent(AgentKind),
    /// A guidance-only agent: the name shown in messages.
    Target(&'static str),
}

impl Reader {
    /// The name to show. For an agent it is the value of `--agent`.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Agent(agent) => agent.cli_name(),
            Self::Target(name) => name,
        }
    }
}

/// Why the report cannot say what a file holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NotChecked {
    /// The file, or a folder above it, leads outside the project folder.
    LeavesProject,
    /// The file is not a regular file, or a link does not end at one.
    NotRegular,
    /// The file is larger than skim reads.
    TooLarge,
    /// The file is not valid UTF-8.
    NotUtf8,
    /// The path or the file could not be opened or checked.
    Unreadable,
    /// A config-folder variable holds a relative path. It names the variable.
    RelativeVariable(&'static str),
    /// The home folder is a relative path.
    RelativeHome,
}

impl std::fmt::Display for NotChecked {
    /// The reason, as a clause that follows "not checked".
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LeavesProject => f.write_str("it leads outside this folder"),
            Self::NotRegular => f.write_str("not a regular file"),
            Self::TooLarge => write!(
                f,
                "larger than {} MiB",
                MAX_INSTRUCTION_FILE_SIZE / (1024 * 1024)
            ),
            Self::NotUtf8 => f.write_str("not valid UTF-8"),
            Self::Unreadable => f.write_str("could not read it"),
            Self::RelativeVariable(var) => write!(f, "{var} is not an absolute path"),
            Self::RelativeHome => f.write_str("the home folder is not an absolute path"),
        }
    }
}

/// What the report found in a guidance file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GuidanceState {
    /// The block is what this binary writes.
    Current,
    /// The marker names another version, or no version skim knows.
    Stale { marker: Marker },
    /// The marker names this version, but the text differs.
    Changed,
    /// A start marker with no end marker.
    Broken,
    /// The file could not be checked.
    NotChecked(NotChecked),
}

/// One guidance file and what it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GuidanceFact {
    /// Who reads the file, in the order of the agent table. Never empty.
    pub(crate) readers: Vec<Reader>,
    pub(crate) scope: FileScope,
    /// The path to show. In project scope it is relative to the project folder.
    pub(crate) shown: PathBuf,
    pub(crate) state: GuidanceState,
}

/// Report the guidance files of the agents, the targets and the project.
///
/// `root` is the project folder. With `None` the project scope is not looked at.
/// Global facts come first, then the targets, then the project files.
pub(crate) fn guidance_report(env: &InstructionEnv, root: Option<&Path>) -> Vec<GuidanceFact> {
    let mut report = Report::default();
    report.add_agents(FileScope::Global, &GuidanceScope::Global, env);
    report.add_targets(env);
    if let Some(root) = root {
        let scope = GuidanceScope::Project {
            root: root.to_path_buf(),
        };
        report.add_agents(FileScope::Project, &scope, env);
    }
    report.facts
}

/// The facts found so far, and the files already looked at.
#[derive(Default)]
struct Report {
    facts: Vec<GuidanceFact>,
    /// Each file looked at, by resolved path, with the index of its fact. A file
    /// that gave no fact has `None`, so a second reader does not read it again.
    seen: BTreeMap<PathBuf, Option<usize>>,
}

/// A file the report looks at.
enum Candidate<'a> {
    /// A file the path policy accepted.
    Resolved(&'a GuidanceTarget),
    /// A file the path policy refused, or a global path that is wrong. `path` is
    /// the path the policy was given.
    Refused { path: &'a Path, why: NotChecked },
}

impl Report {
    /// Add the global or project file of every agent in `scope`.
    fn add_agents(&mut self, kind: FileScope, scope: &GuidanceScope, env: &InstructionEnv) {
        for &agent in AgentKind::all_supported() {
            let reader = Reader::Agent(agent);
            match guidance_target(agent, scope, env) {
                // The agent has no file in this scope (Cursor, global).
                Ok(None) => {}
                Ok(Some(target)) => self.add(kind, reader, Candidate::Resolved(&target)),
                Err(TargetRefusal::Skip(refusal)) => self.add(
                    kind,
                    reader,
                    Candidate::Refused {
                        path: refusal.path(),
                        why: refused(&refusal),
                    },
                ),
                Err(TargetRefusal::Stop(refusal)) => {
                    let path = agent.instruction_file(true, env).unwrap_or_default();
                    self.add(
                        kind,
                        reader,
                        Candidate::Refused {
                            path: &path,
                            why: stopped(&refusal),
                        },
                    );
                }
            }
        }
    }

    /// Add the file of every guidance-only target whose agent is installed.
    fn add_targets(&mut self, env: &InstructionEnv) {
        for agent in TARGETS {
            let reader = Reader::Target(agent.name);
            match agent.resolve(env) {
                // The agent is not installed.
                None => {}
                Some(Ok(target)) => {
                    self.add(FileScope::Global, reader, Candidate::Resolved(&target));
                }
                Some(Err(refusal)) => self.add(
                    FileScope::Global,
                    reader,
                    Candidate::Refused {
                        path: refusal.path(),
                        why: refused(&refusal),
                    },
                ),
            }
        }
    }

    /// Record that `reader` reads `candidate`. The file is read for its first
    /// reader only. It gives a fact when it holds a skim block or cannot be
    /// checked.
    fn add(&mut self, scope: FileScope, reader: Reader, candidate: Candidate<'_>) {
        // A resolved file is keyed by its canonical path, so a link to the folder
        // of another agent is one file. A refused path has no file to resolve.
        let (key, shown) = match &candidate {
            Candidate::Resolved(target) => (
                std::fs::canonicalize(&target.path).unwrap_or_else(|_| target.path.clone()),
                target.shown.clone(),
            ),
            Candidate::Refused { path, .. } => (path.to_path_buf(), path.to_path_buf()),
        };
        if let Some(seen) = self.seen.get(&key) {
            if let Some(index) = *seen
                && !self.facts[index].readers.contains(&reader)
            {
                self.facts[index].readers.push(reader);
            }
            return;
        }
        let state = match candidate {
            Candidate::Resolved(target) => state_of(target, env!("CARGO_PKG_VERSION")),
            Candidate::Refused { why, .. } => Some(GuidanceState::NotChecked(why)),
        };
        let index = state.map(|state| {
            self.facts.push(GuidanceFact {
                readers: vec![reader],
                scope,
                shown,
                state,
            });
            self.facts.len() - 1
        });
        self.seen.insert(key, index);
    }
}

/// What the file of `target` holds, or `None` when it gives no fact: the file is
/// missing or holds no skim block.
fn state_of(target: &GuidanceTarget, version: &str) -> Option<GuidanceState> {
    let text = match read_instruction_text(&target.path) {
        Ok(text) => text,
        Err(ReadFailure::Missing) => return None,
        Err(failure) => return Some(GuidanceState::NotChecked(unreadable(&failure))),
    };
    match block_status(&text, &target.block(version), &target.format) {
        BlockStatus::Absent => None,
        BlockStatus::Current => Some(GuidanceState::Current),
        BlockStatus::Stale { marker } => Some(GuidanceState::Stale { marker }),
        BlockStatus::Changed => Some(GuidanceState::Changed),
        BlockStatus::Broken => Some(GuidanceState::Broken),
    }
}

/// Why a file that failed to read is not checked.
fn unreadable(failure: &ReadFailure) -> NotChecked {
    match failure {
        ReadFailure::TooLarge => NotChecked::TooLarge,
        ReadFailure::NotUtf8 => NotChecked::NotUtf8,
        ReadFailure::Symlink | ReadFailure::Folder | ReadFailure::NotRegular => {
            NotChecked::NotRegular
        }
        ReadFailure::Missing
        | ReadFailure::Open(_)
        | ReadFailure::Inspect(_)
        | ReadFailure::Read(_) => NotChecked::Unreadable,
    }
}

/// Why the path policy leaves a file alone.
fn refused(refusal: &PathRefusal) -> NotChecked {
    match refusal {
        PathRefusal::LeavesProject { .. } => NotChecked::LeavesProject,
        PathRefusal::NotRegular { .. } => NotChecked::NotRegular,
        PathRefusal::Unchecked { .. } => NotChecked::Unreadable,
    }
}

/// Why a global path is wrong.
fn stopped(refusal: &GlobalPathRefusal) -> NotChecked {
    match refusal {
        GlobalPathRefusal::RelativeOverride(refused) => NotChecked::RelativeVariable(refused.var),
        GlobalPathRefusal::NotAbsolute(_) => NotChecked::RelativeHome,
    }
}

#[cfg(test)]
#[path = "guidance_report_tests.rs"]
mod tests;
