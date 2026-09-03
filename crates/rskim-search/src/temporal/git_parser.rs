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
//! sort order (a committer-time priority queue). When `lookback_days > 0`, a
//! `ByCommitTimeCutoff` sort stops the walk as soon as the traversal queue contains
//! no commits newer than the cutoff, which is far more efficient than a full
//! traversal with post-hoc filtering. The cutoff operates on **committer date**,
//! matching `git log --since` semantics (AD-407-3).
//!
//! The walker visits the **full DAG** (not first-parent-only). Merge commits
//! (commits with more than one parent, including octopus merges) are skipped
//! **before** the tree diff so their subjects never enter the fix-risk classifier
//! (AD-407-1, AD-407-2). This matches `git log --no-merges`, which is exactly
//! what `skim heatmap` uses.
//!
//! For each non-merge commit we diff its tree against its first parent's tree
//! using `Tree::changes()` (requires the `blob-diff` gix feature). Root commits
//! (no parent) are diffed against the empty tree.
//!
//! Returned commits are sorted **stably** by `CommitInfo.timestamp` descending
//! (author time, newest first) so equal-timestamp commits preserve gix traversal
//! order. This is the documented ordering contract: consumers such as
//! `rskim-bench::temporal_split` rely on it (AD-407-4).
//!
//! # Walk bounds
//!
//! Two independent safety caps prevent OOM / runaway walks on large repositories
//! (e.g. linux kernel: 1 M+ commits) when `lookback_days = 0`:
//!
//! - `MAX_COMMITS` — maximum number of **retained** (non-merge) commits.
//! - `MAX_VISITED_COMMITS` — maximum number of loop **iterations** (visits),
//!   always ≥ `MAX_COMMITS`. A separate eprintln! fires for each cap.
//!
//! Both bounds are testable without constructing a large repository via
//! [`WalkBudget::charge_retain`] and [`WalkBudget::charge_visit`].
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
// Walk budget
// ============================================================================

/// Safety cap on **retained** (non-merge) commits.
///
/// Prevents OOM on very large repositories (e.g. linux kernel: 1M+ commits)
/// when `lookback_days = 0`. A distinct notice is printed when this cap fires.
const MAX_COMMITS: usize = 100_000;

/// Safety cap on loop **iterations** (visits), including merge-skipped commits.
///
/// After removing `.first_parent_only()`, merge commits are visited and discarded
/// without being pushed, so `MAX_COMMITS` no longer bounds loop iterations.
/// This second bound prevents infinite walks on merge-heavy repositories.
/// The 4× multiplier provides headroom over the observed visits-per-retained ratio
/// (1.08× on this repo, ~1.13× on linux.git) while remaining a safety valve that
/// should never fire in practice (AD-407-1).
///
/// `MAX_VISITED_COMMITS >= MAX_COMMITS` is enforced at compile time.
const MAX_VISITED_COMMITS: usize = 4 * MAX_COMMITS;

/// Compile-time assertion: the visit bound must be at least as large as the
/// retain bound.
const _: () = assert!(
    MAX_VISITED_COMMITS >= MAX_COMMITS,
    "MAX_VISITED_COMMITS must be >= MAX_COMMITS"
);

/// Walk budget tracker — two independent counters with two distinct safety caps.
///
/// Exposed as a public type so tests can drive both bounds directly without
/// constructing a large repository (AC-8).
#[derive(Default)]
pub struct WalkBudget {
    visited: usize,
    retained: usize,
}

impl WalkBudget {
    /// Create a new budget with both counters at zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Charge one loop iteration. Returns `true` when the visit cap has fired
    /// and the loop should break.
    ///
    /// Prints a distinct eprintln! notice on the first call that hits the cap
    /// (consistent with the retained-commit notice).
    pub fn charge_visit(&mut self) -> bool {
        self.visited += 1;
        if self.visited > MAX_VISITED_COMMITS {
            eprintln!(
                "skim: parse_history reached the {MAX_VISITED_COMMITS}-visit safety cap; \
                 truncating history. Pass lookback_days > 0 to scope the traversal."
            );
            true
        } else {
            false
        }
    }

    /// Charge one retained commit. Returns `true` when the retain cap has fired
    /// and the loop should break.
    ///
    /// Prints a distinct eprintln! notice on the first call that hits the cap.
    pub fn charge_retain(&mut self) -> bool {
        if self.retained >= MAX_COMMITS {
            eprintln!(
                "skim: parse_history reached the {MAX_COMMITS}-commit safety cap; \
                 truncating history. Pass lookback_days > 0 to scope the traversal."
            );
            true
        } else {
            self.retained += 1;
            false
        }
    }
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
    /// Walk the full commit DAG and return all non-merge commits.
    ///
    /// # Ordering contract
    ///
    /// Commits are yielded by gix in committer-time-descending order (newest
    /// first). After the walk, the result is **stably** re-sorted by
    /// `CommitInfo.timestamp` (author time) descending so equal-timestamp
    /// commits preserve gix traversal order (AD-407-4).
    ///
    /// Consumers that rely on newest-first ordering (e.g. `rskim-bench::temporal_split`)
    /// may depend on this contract.
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

    // Compute lookback cutoff (seconds since unix epoch).
    // Fail explicitly if the system clock predates the Unix epoch — silently
    // returning 0 would make cutoff_secs negative, causing all commits to pass
    // the filter and effectively ignoring the lookback window.
    let cutoff_secs: Option<i64> = if lookback_days > 0 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| SearchError::Git("system clock is before Unix epoch".into()))?
            .as_secs() as i64;
        Some(now - i64::from(lookback_days) * 86_400)
    } else {
        None
    };

    // Configure rev-walk sorting.
    //
    // AD-407-3: Use gix's ByCommitTimeCutoff (committer-date filter) instead of
    // a manual author-date break. The manual guard compared the wrong clock and,
    // now that the commit-date priority queue is live (full DAG walk), one
    // rebased or cherry-picked commit (old author date, recent committer date)
    // would terminate the entire walk. gix's cutoff matches `git log --since`
    // semantics exactly.
    let sorting = match cutoff_secs {
        Some(cutoff) => Sorting::ByCommitTimeCutoff {
            order: CommitTimeOrder::NewestFirst,
            seconds: cutoff,
        },
        None => Sorting::ByCommitTime(CommitTimeOrder::NewestFirst),
    };

    // AD-407-1: Walk the FULL DAG (remove .first_parent_only()) so every commit
    // on every merged branch is visible to the temporal layer. Merge commits are
    // skipped inside the loop before the tree diff (AD-407-2).
    let walk = repo
        .rev_walk([head_id])
        .sorting(sorting)
        .all()
        .map_err(gix_err)?;

    let mut commits: Vec<CommitInfo> = Vec::new();
    let mut budget = WalkBudget::new();

    for info_result in walk {
        // Visit-count guard — fires before any commit processing.
        if budget.charge_visit() {
            break;
        }

        let info = match info_result {
            Ok(info) => info,
            Err(_) if is_shallow => break,
            Err(e) => return Err(gix_err(e)),
        };

        // Decode the full commit object for parent count / author / message.
        let commit_obj = info.object().map_err(gix_err)?;
        let commit_ref = commit_obj.decode().map_err(gix_err)?;

        // AD-407-2: Skip merge commits (>1 parent, including octopus merges).
        // Merge subjects (e.g. "merge(#NNN): …") never match FIX_REGEX, so
        // letting them through would corrupt fix-risk classification. Skipping
        // here — after decode but before the tree diff — is the earliest safe
        // point; `debug_assert_eq!(commit_count, commits.len())` still holds
        // because merges are never pushed.
        if info.parent_ids.len() > 1 {
            continue;
        }

        // Author timestamp (i64 — can be negative for pre-epoch commits)
        let timestamp: i64 = match commit_ref.author().time().ok() {
            Some(t) => t.seconds,
            None => {
                // Malformed timestamp — skip this commit rather than failing
                continue;
            }
        };

        // Retain-count guard — fires before pushing to `commits`.
        if budget.charge_retain() {
            break;
        }

        let hash = info.id.to_string();
        let author = commit_ref.author().name.to_str_lossy().into_owned();
        // Use first line of commit message only
        let msg_bytes = commit_ref.message;
        let message = first_line_of(msg_bytes.to_str_lossy().as_ref()).to_owned();

        // Compute changed files (tree diff vs. first parent or empty tree).
        // Pass the already-decoded commit object to avoid a second object lookup.
        // In shallow clones, the parent object may be missing — treat as empty.
        let changed_files = match changed_files_for_commit(&repo, &commit_obj, &info.parent_ids) {
            Ok(files) => files,
            Err(_) if is_shallow => Vec::new(),
            Err(e) => return Err(e),
        };

        commits.push(CommitInfo {
            hash,
            timestamp,
            author,
            message,
            changed_files,
        });
    }

    // AD-407-4: Stable sort by author-time descending so the ordering contract
    // is a real guarantee, not a fixture accident (PF-007). Equal-timestamp
    // commits preserve gix traversal order (committer-time descending).
    commits.sort_by_key(|c| std::cmp::Reverse(c.timestamp));

    let commit_count = commits.len();
    let result = HistoryResult {
        commits,
        metadata: TemporalMetadata {
            is_shallow,
            commit_count,
        },
    };
    debug_assert_eq!(
        result.metadata.commit_count,
        result.commits.len(),
        "commit_count must equal commits.len()"
    );
    Ok(result)
}

/// Return the changed files in a commit by diffing its tree against its first
/// parent (or the empty tree for root commits).
///
/// Accepts the already-decoded `commit` object from the caller to avoid a
/// second object-store lookup per commit.
///
/// Uses `Tree::changes().for_each_to_obtain_tree()` which is the high-level
/// gix API. Requires the `blob-diff` feature.
fn changed_files_for_commit(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    parent_ids: &gix::traverse::commit::ParentIds,
) -> Result<Vec<FileChangeInfo>> {
    // Get the new (this commit's) tree
    let new_tree = commit.tree().map_err(gix_err)?;

    // Get old (parent's) tree, or empty tree for root commits
    let old_tree: gix::Tree<'_>;
    let empty_tree: gix::Tree<'_>;

    let lhs: &gix::Tree<'_> = if let Some(&parent_id) = parent_ids.first() {
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
        .for_each_to_obtain_tree(
            &new_tree,
            |change| -> std::result::Result<_, std::convert::Infallible> {
                use gix::object::tree::diff::Change;
                // All change kinds use the destination location.
                // Renames (Rewrite) also provide the new path via `location`.
                let (location, entry_mode) = match &change {
                    Change::Addition {
                        location,
                        entry_mode,
                        ..
                    }
                    | Change::Deletion {
                        location,
                        entry_mode,
                        ..
                    }
                    | Change::Modification {
                        location,
                        entry_mode,
                        ..
                    }
                    | Change::Rewrite {
                        location,
                        entry_mode,
                        ..
                    } => (location, entry_mode),
                };
                // Build PathBuf from the location bytes. `to_str_lossy()` returns a
                // `Cow<str>`; using `.as_ref()` avoids the intermediate owned String
                // allocation when the path is already valid UTF-8 (Cow::Borrowed).
                if entry_mode.is_no_tree() && !location.is_empty() {
                    changed_files.push(FileChangeInfo {
                        path: PathBuf::from(location.to_str_lossy().as_ref()),
                        additions: 0,
                        deletions: 0,
                    });
                }
                Ok(gix::object::tree::diff::Action::Continue)
            },
        )
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
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "git_parser_tests.rs"]
mod tests;
