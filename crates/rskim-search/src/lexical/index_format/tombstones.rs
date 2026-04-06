//! Tombstone set for invalidated doc_ids.

use std::{
    fs::File,
    io::{self, BufWriter, Read, Write},
    path::Path,
};

use crate::SearchError;

const TOMBSTONES_FILE: &str = "lexical.tombstones";

// ============================================================================
// Tombstones — deleted doc_id set
// ============================================================================

/// Set of invalidated doc_ids excluded from main index results.
///
/// Maintains a sorted `Vec<u32>` for O(log n) `contains` queries and
/// O(n log n) `add` + `save` operations.
#[derive(Debug, Default)]
pub struct Tombstones {
    doc_ids: Vec<u32>,
}

impl Tombstones {
    /// Load tombstones from `lexical.tombstones` in the given directory.
    ///
    /// Returns an empty `Tombstones` if the file does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Io`] if the file exists but cannot be read.
    #[must_use = "the loaded Tombstones must be used for filtering; dropping it immediately is a bug"]
    pub fn load(dir: &Path) -> crate::Result<Self> {
        let path = dir.join(TOMBSTONES_FILE);
        let mut file = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(e) => return Err(SearchError::Io(e)),
        };

        // Maximum tombstone file size: 10 million doc_ids × 4 bytes = 40 MB.
        // A legitimate tombstone file beyond this size indicates corruption.
        const MAX_TOMBSTONE_BYTES: u64 = 40_000_000;

        let meta = file.metadata().map_err(SearchError::Io)?;
        if meta.len() > MAX_TOMBSTONE_BYTES {
            return Err(SearchError::CorruptedIndex {
                path: path.display().to_string(),
                reason: format!(
                    "tombstone file is {} bytes, exceeds maximum of {} bytes",
                    meta.len(),
                    MAX_TOMBSTONE_BYTES
                ),
            });
        }

        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(SearchError::Io)?;

        if bytes.len() % 4 != 0 {
            return Err(SearchError::CorruptedIndex {
                path: path.display().to_string(),
                reason: format!("tombstone file size {} is not a multiple of 4", bytes.len()),
            });
        }

        let mut doc_ids: Vec<u32> = bytes
            .chunks_exact(4)
            .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect();

        // The file should already be sorted, but sort defensively.
        doc_ids.sort_unstable();
        doc_ids.dedup();

        Ok(Self { doc_ids })
    }

    /// Save tombstones to `lexical.tombstones` in the given directory.
    ///
    /// Writes sorted `u32` LE values. Overwrites any existing file.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Io`] on write failure.
    pub fn save(&self, dir: &Path) -> crate::Result<()> {
        let path = dir.join(TOMBSTONES_FILE);
        let file = File::create(&path).map_err(SearchError::Io)?;
        let mut writer = BufWriter::new(file);
        for &id in &self.doc_ids {
            writer
                .write_all(&id.to_le_bytes())
                .map_err(SearchError::Io)?;
        }
        writer.flush().map_err(SearchError::Io)?;
        Ok(())
    }

    /// Add a doc_id to the tombstone set.
    ///
    /// Maintains sorted order. Duplicates are silently ignored.
    pub fn add(&mut self, doc_id: u32) {
        match self.doc_ids.binary_search(&doc_id) {
            Ok(_) => {} // already present
            Err(pos) => self.doc_ids.insert(pos, doc_id),
        }
    }

    /// Return whether the given `doc_id` is tombstoned.
    ///
    /// O(log n) binary search.
    pub fn contains(&self, doc_id: u32) -> bool {
        self.doc_ids.binary_search(&doc_id).is_ok()
    }

    /// Return `true` if the tombstone set is empty.
    pub fn is_empty(&self) -> bool {
        self.doc_ids.is_empty()
    }

    /// Return the number of tombstoned doc_ids.
    pub fn len(&self) -> usize {
        self.doc_ids.len()
    }
}
