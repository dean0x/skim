//! [`GixSource`] — gix-based implementation of [`TemporalSource`].
//!
//! # Architecture
//!
//! - Stateless: each `parse_history` call opens a fresh repository handle.
//!   This keeps `GixSource` trivially `Send + Sync` without any locking.
//! - All gix types are converted to [`CommitInfo`]/[`FileChangeInfo`] at the
//!   parser boundary. No gix types appear in the public API.
//! - Error conversion via `gix_err()`: every gix failure maps to
//!   `SearchError::Git(String)`.
//!
//! # Traversal strategy
//!
//! Commits are visited from newest to oldest using gix's `ByCommitTime(NewestFirst)`
//! sort order. When `lookback_days > 0`, a `ByCommitTimeCutoff` sort stops the walk
//! as soon as the traversal queue contains no commits newer than the cutoff, which
//! is far more efficient than a full traversal with post-hoc filtering.
//!
//! For each commit we diff its tree against its first parent's tree using
//! `Tree::changes()` (requires the `blob-diff` gix feature). Root commits
//! (no parent) are diffed against the empty tree.
//!
//! # Limitations
//!
//! - Line counts (`additions`/`deletions`) are set to `0` — tracking which files
//!   changed is sufficient for temporal scoring; blob-level line counts require a full
//!   diff per file per commit, which is prohibitively slow for large repositories.
//! - Binary files are included in `changed_files` (with 0 add/del counts).
//! - Tree entries (directories) are skipped; only file-mode entries are returned.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use gix::bstr::ByteSlice;
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;

use crate::types::{
    CommitInfo, FileChangeInfo, HistoryResult, Result, SearchError, TemporalMetadata,
    TemporalSource,
};

// ============================================================================
// Error helper
// ============================================================================

/// Convert any displayable error into `SearchError::Git(String)`.
#[inline]
fn gix_err(e: impl std::fmt::Display) -> SearchError {
    SearchError::Git(e.to_string())
}

// ============================================================================
// GixSource
// ============================================================================

/// Stateless gix-based git history parser.
///
/// Implements [`TemporalSource`]; thread-safe (`Send + Sync`) and cheap to copy.
#[derive(Debug, Clone, Copy)]
pub struct GixSource;

impl TemporalSource for GixSource {
    fn parse_history(&self, repo_path: &Path, lookback_days: u32) -> Result<HistoryResult> {
        parse_history_impl(repo_path, lookback_days)
    }
}

// ============================================================================
// Implementation
// ============================================================================

fn parse_history_impl(repo_path: &Path, lookback_days: u32) -> Result<HistoryResult> {
    // Open repository, discovering .git in parent directories
    let mut repo = gix::discover(repo_path).map_err(gix_err)?;

    // Enable object cache — recommended for ByCommitTime traversals that look
    // up each commit at least twice
    repo.object_cache_size_if_unset(4 * 1024 * 1024);

    // Check shallow clone status
    let is_shallow = repo.is_shallow();

    // Resolve HEAD — gracefully handle unborn/empty repos
    let head_id = match repo.head_id() {
        Ok(id) => id.detach(),
        Err(e) => {
            let msg = e.to_string().to_ascii_lowercase();
            if is_unborn_error(&msg) {
                return Ok(empty_result(is_shallow));
            }
            return Err(gix_err(e));
        }
    };

    // Compute lookback cutoff (seconds since unix epoch)
    let cutoff_secs: Option<i64> = if lookback_days > 0 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        Some(now - i64::from(lookback_days) * 86_400)
    } else {
        None
    };

    // Configure rev-walk sorting
    let sorting = match cutoff_secs {
        Some(cutoff) => Sorting::ByCommitTimeCutoff {
            order: CommitTimeOrder::NewestFirst,
            seconds: cutoff,
        },
        None => Sorting::ByCommitTime(CommitTimeOrder::NewestFirst),
    };

    let walk = repo
        .rev_walk([head_id])
        .first_parent_only()
        .sorting(sorting)
        .all()
        .map_err(gix_err)?;

    let mut commits: Vec<CommitInfo> = Vec::new();

    for info_result in walk {
        let info = info_result.map_err(gix_err)?;

        // Decode the full commit object for author/message fields
        let commit_obj = info.object().map_err(gix_err)?;
        let commit_ref = commit_obj.decode().map_err(gix_err)?;

        // Author timestamp (i64 — can be negative for pre-epoch commits)
        let timestamp: i64 = match commit_ref.author().time().ok() {
            Some(t) => t.seconds,
            None => {
                // Malformed timestamp — skip this commit rather than failing
                continue;
            }
        };

        // Safety check: if the commit predates the cutoff, stop walking
        if cutoff_secs.is_some_and(|cutoff| timestamp < cutoff) {
            break;
        }

        let hash = info.id.to_string();
        let author = commit_ref.author().name.to_str_lossy().into_owned();
        // Use first line of commit message only
        let msg_bytes = commit_ref.message;
        let message = first_line_of(msg_bytes.to_str_lossy().as_ref()).to_owned();

        // Compute changed files (tree diff vs. first parent or empty tree)
        let changed_files = changed_files_for_commit(&repo, &info)?;

        commits.push(CommitInfo {
            hash,
            timestamp,
            author,
            message,
            changed_files,
        });
    }

    let commit_count = commits.len();
    Ok(HistoryResult {
        commits,
        metadata: TemporalMetadata {
            is_shallow,
            commit_count,
        },
    })
}

/// Return the changed files in a commit by diffing its tree against its first
/// parent (or the empty tree for root commits).
///
/// Uses `Tree::changes().for_each_to_obtain_tree()` which is the high-level
/// gix API. Requires the `blob-diff` feature.
fn changed_files_for_commit(
    repo: &gix::Repository,
    info: &gix::revision::walk::Info<'_>,
) -> Result<Vec<FileChangeInfo>> {
    let commit = info.object().map_err(gix_err)?;

    // Get the new (this commit's) tree
    let new_tree = commit.tree().map_err(gix_err)?;

    // Get old (parent's) tree, or empty tree for root commits
    let old_tree: gix::Tree<'_>;
    let empty_tree: gix::Tree<'_>;

    let lhs: &gix::Tree<'_> = if let Some(&parent_id) = info.parent_ids.first() {
        let parent_obj = repo.find_object(parent_id).map_err(gix_err)?;
        let parent_commit = parent_obj
            .try_into_commit()
            .map_err(|e| gix_err(format!("parent is not a commit: {e}")))?;
        old_tree = parent_commit.tree().map_err(gix_err)?;
        &old_tree
    } else {
        empty_tree = repo.empty_tree();
        &empty_tree
    };

    // Collect changed file paths via gix's tree-diff platform
    let mut changed_files: Vec<FileChangeInfo> = Vec::new();

    lhs.changes()
        .map_err(gix_err)?
        .for_each_to_obtain_tree(&new_tree, |change| -> std::result::Result<_, std::convert::Infallible> {
            use gix::object::tree::diff::Change;
            let path_opt = match &change {
                Change::Addition { location, entry_mode, .. } => {
                    if entry_mode.is_no_tree() && !location.is_empty() {
                        Some(location.to_str_lossy().into_owned())
                    } else {
                        None
                    }
                }
                Change::Deletion { location, entry_mode, .. } => {
                    if entry_mode.is_no_tree() && !location.is_empty() {
                        Some(location.to_str_lossy().into_owned())
                    } else {
                        None
                    }
                }
                Change::Modification { location, entry_mode, .. } => {
                    if entry_mode.is_no_tree() && !location.is_empty() {
                        Some(location.to_str_lossy().into_owned())
                    } else {
                        None
                    }
                }
                Change::Rewrite { location, entry_mode, .. } => {
                    // Renames: use destination path
                    if entry_mode.is_no_tree() && !location.is_empty() {
                        Some(location.to_str_lossy().into_owned())
                    } else {
                        None
                    }
                }
            };
            if let Some(path) = path_opt {
                changed_files.push(FileChangeInfo {
                    path: PathBuf::from(path),
                    additions: 0,
                    deletions: 0,
                });
            }
            Ok(gix::object::tree::diff::Action::Continue)
        })
        .map_err(gix_err)?;

    Ok(changed_files)
}

// ============================================================================
// Helpers
// ============================================================================

/// Return `true` when an error message signals an unborn (empty) repository.
fn is_unborn_error(msg: &str) -> bool {
    msg.contains("unborn")
        || msg.contains("cannot resolve head")
        || msg.contains("does not exist")
        || msg.contains("not found")
        || msg.contains("no reference was found")
        || msg.contains("does not have any commits")
}

/// Build an empty `HistoryResult` for a repo with no commits.
fn empty_result(is_shallow: bool) -> HistoryResult {
    HistoryResult {
        commits: Vec::new(),
        metadata: TemporalMetadata {
            is_shallow,
            commit_count: 0,
        },
    }
}

/// Return the first non-empty line of `s`, trimmed.
fn first_line_of(s: &str) -> &str {
    s.lines().next().unwrap_or(s).trim()
}

// ============================================================================
// Co-located tests
// ============================================================================

#[cfg(test)]
#[path = "git_parser_tests.rs"]
mod tests;
