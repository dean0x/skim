//! [`CochangeMatrixBuilder`] — accumulates co-change pairs from git history
//! and serialises them to a single `cochange.skcc` file.
//!
//! # Atomicity contract
//!
//! The output file is written atomically via [`tempfile::NamedTempFile`] + `persist`
//! (rename), so readers never observe a partial write.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;

use super::format::{
    FILE_COMMIT_ENTRY_SIZE, FORMAT_VERSION, FileCommitEntry, HEADER_SIZE, PAIR_ENTRY_SIZE,
    PairEntry, SKCC_MAGIC, SkccHeader, compute_checksum, encode_file_commit, encode_header,
    encode_pair,
};
use crate::{CochangeStats, FileId, HistoryResult, Result, SearchError};

/// Maximum number of files in a commit before the commit is skipped.
///
/// Commits that touch more than this many files are bulk refactors / merges
/// that would pollute the coupling signal.
pub(crate) const COUPLING_MAX_FILES: usize = 50;

/// Maximum number of distinct co-change pairs the builder will accumulate.
///
/// Exceeding this limit returns `SearchError::IndexCorrupted` to protect
/// against unbounded memory growth.
pub(crate) const MAX_PAIRS: usize = 2_000_000;

// ============================================================================
// Public builder struct
// ============================================================================

/// Builds a co-change matrix from git history and writes it to a directory.
///
/// Does NOT implement [`crate::LayerBuilder`] — it takes a [`HistoryResult`]
/// rather than raw file content.
pub struct CochangeMatrixBuilder {
    output_dir: PathBuf,
}

impl CochangeMatrixBuilder {
    /// Create a new builder that will write `cochange.skcc` into `output_dir`.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Io`] if `output_dir` does not exist.
    #[must_use = "this returns a Result that should be checked"]
    pub fn new(output_dir: PathBuf) -> Result<Self> {
        if !output_dir.exists() {
            return Err(SearchError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("output_dir does not exist: {}", output_dir.display()),
            )));
        }
        Ok(Self { output_dir })
    }

    /// Accumulate co-change pairs from `history` and write `cochange.skcc`.
    ///
    /// # Arguments
    ///
    /// - `history` — parsed git history from [`crate::TemporalSource`].
    /// - `path_map` — caller-managed mapping from repo-root-relative paths to
    ///   [`FileId`]. Paths absent from the map are silently skipped and counted
    ///   in [`CochangeStats::unknown_paths_skipped`].
    ///
    /// # Errors
    ///
    /// - [`SearchError::IndexCorrupted`] if the number of accumulated pairs
    ///   exceeds [`MAX_PAIRS`].
    /// - [`SearchError::Io`] if writing fails.
    #[must_use = "this returns a Result that should be checked"]
    pub fn build(
        &self,
        history: &HistoryResult,
        path_map: &HashMap<PathBuf, FileId>,
    ) -> Result<CochangeStats> {
        self.build_with_limit(history, path_map, MAX_PAIRS)
    }

    /// Like [`build`] but with a caller-supplied `max_pairs` limit.
    ///
    /// Intended for unit tests that need to trigger the safety cap without
    /// generating 2 million distinct pairs.
    #[cfg(test)]
    pub(crate) fn build_with_max_pairs(
        &self,
        history: &HistoryResult,
        path_map: &HashMap<PathBuf, FileId>,
        max_pairs: usize,
    ) -> Result<CochangeStats> {
        self.build_with_limit(history, path_map, max_pairs)
    }

    fn build_with_limit(
        &self,
        history: &HistoryResult,
        path_map: &HashMap<PathBuf, FileId>,
        max_pairs: usize,
    ) -> Result<CochangeStats> {
        let (pairs, file_counts, mut stats) = accumulate_pairs(history, path_map, max_pairs)?;
        stats.pair_count = u32::try_from(pairs.len()).unwrap_or(u32::MAX);
        stats.file_count = u32::try_from(file_counts.len()).unwrap_or(u32::MAX);

        let data = serialize(&pairs, &file_counts)?;
        let out_path = self.output_dir.join("cochange.skcc");
        atomic_write(&self.output_dir, &out_path, &data)?;

        Ok(stats)
    }
}

// ============================================================================
// Private helpers
// ============================================================================

/// Intermediate accumulation result: `(pair_counts, file_commit_counts, stats)`.
type AccumulatedPairs = (HashMap<(u32, u32), u32>, HashMap<u32, u32>, CochangeStats);

/// Iterate all commits, resolve paths, generate canonical (min,max) pairs,
/// and track per-file commit counts.
///
/// `max_pairs` caps the number of distinct pairs; exceeding it returns
/// [`SearchError::IndexCorrupted`].  Production callers pass [`MAX_PAIRS`];
/// tests may pass a smaller value to exercise the error path cheaply.
///
/// Returns `(pair_counts, file_commit_counts, stats)`.
fn accumulate_pairs(
    history: &HistoryResult,
    path_map: &HashMap<PathBuf, FileId>,
    max_pairs: usize,
) -> Result<AccumulatedPairs> {
    let mut pair_counts: HashMap<(u32, u32), u32> =
        HashMap::with_capacity(history.commits.len().saturating_mul(4));
    let mut file_commit_counts: HashMap<u32, u32> = HashMap::with_capacity(path_map.len());

    let mut commits_processed: u32 = 0;
    let mut commits_skipped_too_large: u32 = 0;
    let mut unknown_paths_skipped: u32 = 0;

    for commit in &history.commits {
        commits_processed = commits_processed.saturating_add(1);

        // Skip commits touching too many files.
        if commit.changed_files.len() > COUPLING_MAX_FILES {
            commits_skipped_too_large = commits_skipped_too_large.saturating_add(1);
            continue;
        }

        // Resolve file IDs for this commit's changed files.
        // Deduplicate because the same path can appear more than once in a
        // commit (e.g. rename with modify). Without dedup, self-pairs (a==a)
        // would violate the canonical-ordering invariant.
        let mut ids: Vec<u32> = Vec::with_capacity(commit.changed_files.len());
        for fc in &commit.changed_files {
            match path_map.get(&fc.path) {
                Some(fid) => ids.push(fid.0),
                None => {
                    unknown_paths_skipped = unknown_paths_skipped.saturating_add(1);
                }
            }
        }
        ids.sort_unstable();
        ids.dedup();

        // Update per-file commit counts.
        for &id in &ids {
            let entry = file_commit_counts.entry(id).or_insert(0);
            *entry = entry.saturating_add(1);
        }

        // Generate canonical (min, max) pairs — skip self-pairs implicitly.
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                let a = ids[i].min(ids[j]);
                let b = ids[i].max(ids[j]);
                // a < b guaranteed by construction; self-pairs (a==b) impossible
                // when i != j and all IDs in a commit are distinct paths.
                debug_assert!(a < b, "canonical pair invariant: a({a}) < b({b})");

                // Check max_pairs limit before inserting a new entry.
                if !pair_counts.contains_key(&(a, b)) && pair_counts.len() >= max_pairs {
                    return Err(SearchError::IndexCorrupted(
                        "co-change pair count exceeds safety limit".into(),
                    ));
                }
                let entry = pair_counts.entry((a, b)).or_insert(0);
                *entry = entry.saturating_add(1);
            }
        }
    }

    let stats = CochangeStats {
        pair_count: 0, // filled in after accumulation
        file_count: 0, // filled in after accumulation
        commits_processed,
        commits_skipped_too_large,
        unknown_paths_skipped,
    };

    Ok((pair_counts, file_commit_counts, stats))
}

/// Serialise accumulated data into the `cochange.skcc` on-disk format.
fn serialize(
    pair_counts: &HashMap<(u32, u32), u32>,
    file_commit_counts: &HashMap<u32, u32>,
) -> Result<Vec<u8>> {
    // Sort file_commit entries by file_id ascending.
    let mut file_entries: Vec<FileCommitEntry> = file_commit_counts
        .iter()
        .map(|(&file_id, &commit_count)| FileCommitEntry {
            file_id,
            commit_count,
        })
        .collect();
    file_entries.sort_unstable_by_key(|e| e.file_id);

    // Sort pair entries by (file_a, file_b) ascending.
    let mut pair_entries: Vec<PairEntry> = pair_counts
        .iter()
        .map(|(&(file_a, file_b), &count)| PairEntry {
            file_a,
            file_b,
            count,
        })
        .collect();
    pair_entries.sort_unstable_by_key(|p| (p.file_a, p.file_b));

    // Serialise file_commit and pair arrays — use checked arithmetic to match
    // reader.rs and catch hypothetical overflow on 32-bit targets.
    let fc_bytes = file_entries
        .len()
        .checked_mul(FILE_COMMIT_ENTRY_SIZE)
        .ok_or_else(|| {
            SearchError::IndexCorrupted(
                "file_count * FILE_COMMIT_ENTRY_SIZE overflow".into(),
            )
        })?;
    let pair_bytes = pair_entries
        .len()
        .checked_mul(PAIR_ENTRY_SIZE)
        .ok_or_else(|| {
            SearchError::IndexCorrupted("pair_count * PAIR_ENTRY_SIZE overflow".into())
        })?;

    let mut fc_buf: Vec<u8> = Vec::with_capacity(fc_bytes);
    for e in &file_entries {
        fc_buf.extend_from_slice(&encode_file_commit(e));
    }
    let mut pair_buf: Vec<u8> = Vec::with_capacity(pair_bytes);
    for p in &pair_entries {
        pair_buf.extend_from_slice(&encode_pair(p));
    }

    // CRC32 over file_commit ++ pair bytes — delegate to format.rs so there
    // is a single source of truth for the checksum algorithm.
    let mut payload: Vec<u8> = Vec::with_capacity(fc_bytes + pair_bytes);
    payload.extend_from_slice(&fc_buf);
    payload.extend_from_slice(&pair_buf);
    let checksum = compute_checksum(&payload);

    // Build header.
    let pair_count = u32::try_from(pair_entries.len()).map_err(|_| {
        SearchError::IndexCorrupted(format!(
            "pair_count {} exceeds u32::MAX",
            pair_entries.len()
        ))
    })?;
    let file_count = u32::try_from(file_entries.len()).map_err(|_| {
        SearchError::IndexCorrupted(format!(
            "file_count {} exceeds u32::MAX",
            file_entries.len()
        ))
    })?;
    let header = SkccHeader {
        magic: *SKCC_MAGIC,
        version: FORMAT_VERSION,
        pair_count,
        file_count,
        checksum,
    };

    // Assemble: header + file_commit + pairs — use checked arithmetic to
    // match reader.rs and guard against overflow on 32-bit targets.
    let total = HEADER_SIZE
        .checked_add(fc_bytes)
        .and_then(|s| s.checked_add(pair_bytes))
        .ok_or_else(|| SearchError::IndexCorrupted("total buffer size overflow".into()))?;
    let mut buf = Vec::with_capacity(total);
    buf.extend_from_slice(&encode_header(&header));
    buf.extend_from_slice(&fc_buf);
    buf.extend_from_slice(&pair_buf);

    Ok(buf)
}

/// Atomically write `data` to `path` using a temp file in `dir`.
///
/// Sets explicit `0o644` permissions on Unix before persisting so that a
/// permissive process umask cannot leave the file world-writable.
fn atomic_write(dir: &Path, path: &Path, data: &[u8]) -> Result<()> {
    let mut tmp = NamedTempFile::new_in(dir)?;
    use std::io::Write as _;
    tmp.write_all(data)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o644))?;
    }

    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "builder_tests.rs"]
mod tests;
