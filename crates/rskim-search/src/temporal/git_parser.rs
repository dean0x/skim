//! Git history parser for the temporal search layer.
//!
//! Walks the first-parent ancestry of a repository's HEAD using `gix`,
//! extracting per-commit metadata needed by the co-change matrix and hotspot /
//! risk scoring modules.
//!
//! # First-parent only
//!
//! Merge commits are included (their timestamp and message are recorded) but
//! only the diff against their first parent is used. This matches standard
//! `git log --first-parent` semantics and avoids double-counting files that
//! landed via a feature branch.
//!
//! # Lookback window
//!
//! Commits whose committer timestamp predates `now - lookback_days * 86400`
//! are silently skipped. The caller controls how far back to look.

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{SystemTime, UNIX_EPOCH},
};

use gix::bstr::ByteSlice;
use gix::objs::TreeRefIter;
use regex::Regex;

use crate::types::{Result, SearchError};

use super::types::CommitInfo;

// ============================================================================
// Fix-commit regex
// ============================================================================

/// Returns a compiled regex that matches fix-related keywords at word boundaries.
///
/// Initialized once and reused for the lifetime of the process.
fn fix_pattern() -> &'static Regex {
    static FIX_PATTERN: OnceLock<Regex> = OnceLock::new();
    FIX_PATTERN.get_or_init(|| {
        Regex::new(r"(?i)\b(fix|fixes|fixed|bug|bugfix|revert|hotfix|patch)\b").unwrap_or_else(
            |_| {
                // The pattern is a compile-time literal and cannot fail;
                // `unreachable!` is used here to satisfy clippy's `unwrap_used`
                // lint while communicating that this branch is impossible.
                unreachable!("fix pattern is a valid literal and cannot fail to compile")
            },
        )
    })
}

/// Returns `true` when `message` contains a fix-related keyword at a word boundary.
fn is_fix_commit(message: &str) -> bool {
    fix_pattern().is_match(message)
}

// ============================================================================
// Public API
// ============================================================================

/// Parse a repository's git history and return per-commit metadata.
///
/// Walks the first-parent chain from `HEAD` and returns one [`CommitInfo`] per
/// commit whose committer timestamp falls within `lookback_days` of the current
/// UTC time. Results are in reverse-chronological order (most-recent first),
/// matching the natural traversal order of `git log --first-parent`.
///
/// # Errors
///
/// - [`SearchError::GitError`] if `repo_path` is not a git repository or any
///   git operation fails.
/// - Returns `Ok(vec![])` when the repository has no commits (unborn HEAD).
#[must_use = "parse_history returns the commit list; discarding it is likely a bug"]
pub fn parse_history(repo_path: &Path, lookback_days: u32) -> Result<Vec<CommitInfo>> {
    let cutoff = compute_cutoff_secs(lookback_days);

    let repo = gix::open(repo_path)
        .map_err(|e| SearchError::GitError(format!("open repo at {}: {e}", repo_path.display())))?;

    // An unborn HEAD (freshly initialized, no commits) is not an error.
    let head = repo
        .head()
        .map_err(|e| SearchError::GitError(format!("read HEAD: {e}")))?;

    if head.is_unborn() {
        return Ok(vec![]);
    }

    let head_commit = repo
        .head_commit()
        .map_err(|e| SearchError::GitError(format!("peel HEAD to commit: {e}")))?;

    let walk = repo
        .rev_walk(std::iter::once(head_commit.id))
        .first_parent_only()
        .all()
        .map_err(|e| SearchError::GitError(format!("initialize rev-walk: {e}")))?;

    let mut commits = Vec::new();
    let mut diff_state = gix::diff::tree::State::default();

    // gix's first-parent rev_walk yields commits roughly newest-first but
    // does not guarantee strict date order.  We use a consecutive-miss counter
    // to break out of the walk once we have seen many commits in a row that
    // all fall outside the lookback window — at that depth the chance of
    // finding any in-window commit is negligible and we avoid scanning the
    // full history of very old repositories.
    const MAX_CONSECUTIVE_MISSES: u32 = 500;
    let mut consecutive_misses: u32 = 0;

    for info in walk {
        let info = info.map_err(|e| SearchError::GitError(format!("rev-walk entry: {e}")))?;

        let commit = info
            .object()
            .map_err(|e| SearchError::GitError(format!("load commit object {}: {e}", info.id)))?;

        let time = commit
            .time()
            .map_err(|e| SearchError::GitError(format!("read commit time {}: {e}", info.id)))?;

        // `gix_date::Time::seconds` is i64 (UTC epoch seconds).
        // Negative timestamps predate 1970 — treat as 0 for filtering.
        let timestamp = u64::try_from(time.seconds).unwrap_or(0);

        if timestamp < cutoff {
            consecutive_misses += 1;
            if consecutive_misses >= MAX_CONSECUTIVE_MISSES {
                // We've seen 500 consecutive commits all older than the lookback
                // window.  Because the walk is roughly newest-first, the
                // probability of finding an in-window commit deeper in history
                // is negligible.  Stop iterating to bound memory and CPU cost.
                break;
            }
            continue;
        }
        consecutive_misses = 0;

        let message_raw = commit
            .message_raw()
            .map_err(|e| SearchError::GitError(format!("read message {}: {e}", info.id)))?;

        // Use only the subject line (first non-empty line) to stay within
        // memory bounds and match `git log --oneline` semantics.
        let message = first_line(message_raw.to_str_lossy().as_ref()).to_owned();

        let is_fix = is_fix_commit(&message);

        let hash = info.id.to_string();

        let changed_files = extract_changed_files(&repo, &commit, &mut diff_state)?;

        commits.push(CommitInfo {
            hash,
            timestamp,
            message,
            is_fix,
            changed_files,
        });
    }

    Ok(commits)
}

// ============================================================================
// Helpers
// ============================================================================

/// Compute the lookback cutoff as UTC epoch seconds.
fn compute_cutoff_secs(lookback_days: u32) -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let window = u64::from(lookback_days) * 86_400;
    now.saturating_sub(window)
}

/// Extract the first non-empty line from `message`, trimming trailing whitespace.
fn first_line(message: &str) -> &str {
    message
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(message)
        .trim_end()
}

/// Diff `commit` against its first parent and collect changed file paths.
///
/// For the initial commit (no parents), all files in the commit tree are
/// diffed against an empty tree. Only first-parent changes are included.
fn extract_changed_files(
    repo: &gix::Repository,
    commit: &gix::Commit<'_>,
    state: &mut gix::diff::tree::State,
) -> Result<Vec<PathBuf>> {
    let commit_tree = commit
        .tree()
        .map_err(|e| SearchError::GitError(format!("load commit tree: {e}")))?;

    // Collect parent tree data; use empty bytes for the root commit case.
    let parent_tree_data: Vec<u8> = match commit.parent_ids().next() {
        Some(parent_id) => {
            let parent_obj = repo
                .find_object(parent_id.detach())
                .map_err(|e| SearchError::GitError(format!("find parent object: {e}")))?;
            let parent_commit = parent_obj
                .try_into_commit()
                .map_err(|e| SearchError::GitError(format!("parent is not a commit: {e}")))?;
            parent_commit
                .tree()
                .map_err(|e| SearchError::GitError(format!("load parent tree: {e}")))?
                .data
                .clone()
        }
        None => {
            // Root commit — diff against empty tree.
            Vec::new()
        }
    };

    let mut recorder = gix::diff::tree::Recorder::default();

    // gix::diff::tree is the re-export of gix_diff::tree::function::diff.
    // It performs a pure tree-to-tree diff (no blob content needed).
    gix::diff::tree(
        TreeRefIter::from_bytes(&parent_tree_data),
        TreeRefIter::from_bytes(&commit_tree.data),
        state,
        &repo.objects,
        &mut recorder,
    )
    .map_err(|e| SearchError::GitError(format!("tree diff: {e}")))?;

    let changed: Vec<PathBuf> = recorder
        .records
        .iter()
        .map(|change| {
            let path = match change {
                gix::diff::tree::recorder::Change::Addition { path, .. } => path,
                gix::diff::tree::recorder::Change::Deletion { path, .. } => path,
                gix::diff::tree::recorder::Change::Modification { path, .. } => path,
            };
            // gix returns repo-relative forward-slash paths as BString.
            PathBuf::from(path.to_str_lossy().as_ref())
        })
        .collect();

    Ok(changed)
}
