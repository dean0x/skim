//! Shared test helpers for co-change module tests.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::{CommitInfo, FileChangeInfo, FileId, HistoryResult, TemporalMetadata};

/// Build a [`HistoryResult`] from a list of commits, each described by the
/// file paths they changed.
pub(super) fn make_history(commits: Vec<Vec<&str>>) -> HistoryResult {
    let commit_list = commits
        .into_iter()
        .enumerate()
        .map(|(i, paths)| CommitInfo {
            hash: format!("{i:040x}"),
            timestamp: i as i64,
            author: "test".to_string(),
            message: "test commit".to_string(),
            changed_files: paths
                .into_iter()
                .map(|p| FileChangeInfo {
                    path: PathBuf::from(p),
                    additions: 1,
                    deletions: 0,
                })
                .collect(),
        })
        .collect();
    HistoryResult {
        commits: commit_list,
        metadata: TemporalMetadata {
            is_shallow: false,
            commit_count: 0,
        },
    }
}

/// Build a path-to-[`FileId`] map from a slice of path strings.
///
/// Paths are assigned sequential IDs starting at 0.
pub(super) fn make_path_map(paths: &[&str]) -> HashMap<PathBuf, FileId> {
    paths
        .iter()
        .enumerate()
        .map(|(i, p)| (PathBuf::from(p), FileId(i as u32)))
        .collect()
}
