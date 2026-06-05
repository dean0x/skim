//! [`CochangeMatrixBuilder`] — accumulates co-change pairs from git history
//! and serialises them to a single `cochange.skcc` file.
//!
//! # Atomicity contract
//!
//! The output file is written atomically via [`crate::io_util::atomic_write`]
//! (`NamedTempFile::new_in` + `write_all` + `sync_all` + `persist`), so
//! readers never observe a partial write.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::PathBuf;

use super::format::{
    FILE_COMMIT_ENTRY_SIZE, FORMAT_VERSION, FileCommitEntry, HEADER_SIZE, PAIR_ENTRY_SIZE,
    PairEntry, SKCC_MAGIC, SkccHeader, compute_checksum, encode_file_commit, encode_header,
    encode_pair,
};
use crate::{CochangeStats, FileId, HistoryResult, Result, SearchError, io_util::atomic_write};

/// Maximum number of files in a commit before the commit is skipped.
///
/// Commits that touch more than this many files are bulk refactors / merges
/// that would pollute the coupling signal.
pub(crate) const COUPLING_MAX_FILES: usize = 50;

/// Maximum number of distinct co-change pairs the builder will accumulate.
///
/// Exceeding this limit returns [`SearchError::CapacityExceeded`] to protect
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
    /// - [`SearchError::CapacityExceeded`] if the number of accumulated pairs
    ///   exceeds [`MAX_PAIRS`].
    /// - [`SearchError::Io`] if writing fails.
    pub fn build(
        &self,
        history: &HistoryResult,
        path_map: &HashMap<PathBuf, FileId>,
    ) -> Result<CochangeStats> {
        self.build_with_limit(history, path_map, MAX_PAIRS)
    }

    /// Like [`build`] but with a caller-supplied `max_pairs` limit.
    ///
    /// `pub(crate)` so tests can trigger the safety cap with a small limit
    /// without generating 2 million distinct pairs.
    pub(crate) fn build_with_limit(
        &self,
        history: &HistoryResult,
        path_map: &HashMap<PathBuf, FileId>,
        max_pairs: usize,
    ) -> Result<CochangeStats> {
        let (pairs, file_counts, mut stats) = accumulate_pairs(history, path_map, max_pairs)?;
        stats.pair_count = u32::try_from(pairs.len()).map_err(|_| {
            SearchError::IndexCorrupted(format!("pair_count {} exceeds u32::MAX", pairs.len()))
        })?;
        stats.file_count = u32::try_from(file_counts.len()).map_err(|_| {
            SearchError::IndexCorrupted(format!(
                "file_count {} exceeds u32::MAX",
                file_counts.len()
            ))
        })?;

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
/// [`SearchError::CapacityExceeded`].  Production callers pass [`MAX_PAIRS`];
/// tests may pass a smaller value to exercise the error path cheaply.
///
/// Returns `(pair_counts, file_commit_counts, stats)`.
fn accumulate_pairs(
    history: &HistoryResult,
    path_map: &HashMap<PathBuf, FileId>,
    max_pairs: usize,
) -> Result<AccumulatedPairs> {
    // Bound initial capacity by max_pairs / 4 so high-overlap repos don't
    // over-allocate when the effective pair count is small relative to commits.
    let initial_capacity = history.commits.len().min(max_pairs / 4);
    let mut pair_counts: HashMap<(u32, u32), u32> = HashMap::with_capacity(initial_capacity);
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
        generate_pairs(&ids, &mut pair_counts, max_pairs)?;
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

/// Accumulate all canonical `(a, b)` pairs for the given sorted, deduped ID
/// slice into `pair_counts`.
///
/// Since `ids` is sorted and deduplicated, `ids[i] < ids[j]` whenever `i < j`,
/// so `a = ids[i]` and `b = ids[j]` is already in canonical order — no
/// `.min()` / `.max()` required.
///
/// Returns [`SearchError::CapacityExceeded`] if inserting a new pair would
/// exceed `max_pairs`.
fn generate_pairs(
    ids: &[u32],
    pair_counts: &mut HashMap<(u32, u32), u32>,
    max_pairs: usize,
) -> Result<()> {
    for (idx, &a) in ids.iter().enumerate() {
        for &b in &ids[idx + 1..] {
            // a < b guaranteed by construction; self-pairs (a==b) impossible
            // because ids is sorted and deduplicated.
            debug_assert!(a < b, "canonical pair invariant: a({a}) < b({b})");

            // Use Entry API for a single hash probe in the common (under-
            // capacity) case.  When the map is already full we must guard
            // against inserting new keys while still allowing increments to
            // existing ones — the Occupied/Vacant match handles both in a
            // single lookup.
            if pair_counts.len() < max_pairs {
                // Under capacity: entry() is safe to insert; single probe.
                let count = pair_counts.entry((a, b)).or_insert(0);
                *count = count.saturating_add(1);
            } else {
                // At capacity: existing keys may still be incremented.
                match pair_counts.entry((a, b)) {
                    Entry::Occupied(mut occ) => {
                        *occ.get_mut() = occ.get().saturating_add(1);
                    }
                    Entry::Vacant(_) => {
                        return Err(SearchError::CapacityExceeded(
                            "co-change pair count exceeds safety limit".into(),
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Collect and sort file-commit entries by `file_id` ascending.
fn collect_sorted_file_entries(counts: &HashMap<u32, u32>) -> Vec<FileCommitEntry> {
    let mut entries: Vec<FileCommitEntry> = counts
        .iter()
        .map(|(&file_id, &commit_count)| FileCommitEntry {
            file_id,
            commit_count,
        })
        .collect();
    entries.sort_unstable_by_key(|e| e.file_id);
    entries
}

/// Collect and sort pair entries by `(file_a, file_b)` ascending.
fn collect_sorted_pair_entries(counts: &HashMap<(u32, u32), u32>) -> Vec<PairEntry> {
    let mut entries: Vec<PairEntry> = counts
        .iter()
        .map(|(&(file_a, file_b), &count)| PairEntry {
            file_a,
            file_b,
            count,
        })
        .collect();
    entries.sort_unstable_by_key(|p| (p.file_a, p.file_b));
    entries
}

/// Serialise accumulated data into the `cochange.skcc` on-disk format.
fn serialize(
    pair_counts: &HashMap<(u32, u32), u32>,
    file_commit_counts: &HashMap<u32, u32>,
) -> Result<Vec<u8>> {
    let file_entries = collect_sorted_file_entries(file_commit_counts);
    let pair_entries = collect_sorted_pair_entries(pair_counts);

    // Compute byte counts with checked arithmetic to catch overflow on 32-bit targets.
    let fc_bytes = file_entries
        .len()
        .checked_mul(FILE_COMMIT_ENTRY_SIZE)
        .ok_or_else(|| {
            SearchError::IndexCorrupted("file_count * FILE_COMMIT_ENTRY_SIZE overflow".into())
        })?;
    let pair_bytes = pair_entries
        .len()
        .checked_mul(PAIR_ENTRY_SIZE)
        .ok_or_else(|| {
            SearchError::IndexCorrupted("pair_count * PAIR_ENTRY_SIZE overflow".into())
        })?;

    // Assemble: header placeholder + file_commit + pairs.
    let total = HEADER_SIZE
        .checked_add(fc_bytes)
        .and_then(|s| s.checked_add(pair_bytes))
        .ok_or_else(|| SearchError::IndexCorrupted("total buffer size overflow".into()))?;
    let mut buf = Vec::with_capacity(total);
    buf.extend([0u8; HEADER_SIZE]); // placeholder, overwritten below
    for e in &file_entries {
        buf.extend_from_slice(&encode_file_commit(e));
    }
    for p in &pair_entries {
        buf.extend_from_slice(&encode_pair(p));
    }

    // CRC32 over file_commit ++ pair bytes — delegate to format.rs so there
    // is a single source of truth for the checksum algorithm.
    let checksum = compute_checksum(&buf[HEADER_SIZE..]);

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
    buf[..HEADER_SIZE].copy_from_slice(&encode_header(&header));

    Ok(buf)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "builder_tests.rs"]
mod tests;
