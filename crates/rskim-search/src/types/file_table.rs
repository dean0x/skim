//! File identity types and the bidirectional path-to-id mapping.
//!
//! ARCHITECTURE: FileTable is I/O-free. It never touches the filesystem.
//! All normalization is done lexically via path component analysis.

// FileTable uses usize↔u64 conversions that are infallible only on 64-bit targets.
// Reject compilation on 32-bit to prevent silent truncation of FileId values.
#[cfg(not(target_pointer_width = "64"))]
compile_error!("rskim-search requires a 64-bit target (usize must be at least 64 bits)");

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Opaque identifier for a file in the search index.
///
/// All search layers reference files by `FileId`, resolved to paths via [`FileTable`].
/// This indirection allows layers to store compact integer keys instead of heap-allocated paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileId(u64);

impl FileId {
    /// Create a new `FileId` from a raw integer.
    pub fn new(id: u64) -> Self {
        Self(id)
    }

    /// Return the raw integer value.
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

/// Bidirectional mapping between file paths and compact [`FileId`] values.
///
/// All search layers reference files by `FileId`. Callers resolve IDs back to
/// paths via [`FileTable::lookup`]. The table is I/O-free — it does not touch
/// the filesystem.
#[derive(Debug, Clone)]
pub struct FileTable {
    /// Ordered list of registered paths; index into this vec is the raw FileId.
    paths: Vec<PathBuf>,
    /// Reverse map: normalized path -> FileId.
    ids: HashMap<PathBuf, FileId>,
}

// Custom Serialize: only emit the paths vec (ids are derived)
impl Serialize for FileTable {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        self.paths.serialize(serializer)
    }
}

// Custom Deserialize: reconstruct ids from paths, normalizing each path so that
// deserialized state is consistent with state built via register(). Without
// normalization, a serialized "./src/main.rs" would be treated as a new path
// by subsequent register("src/main.rs") calls, breaking idempotency.
impl<'de> Deserialize<'de> for FileTable {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let raw_paths = Vec::<PathBuf>::deserialize(deserializer)?;
        let mut paths = Vec::with_capacity(raw_paths.len());
        let mut ids = HashMap::with_capacity(raw_paths.len());
        for (i, p) in raw_paths.iter().enumerate() {
            let normalized = Self::normalize(p);
            // u64::try_from(i) is infallible on 64-bit (guarded by compile_error above)
            #[allow(clippy::cast_possible_truncation)]
            let id = FileId::new(i as u64);
            ids.insert(normalized.clone(), id);
            paths.push(normalized);
        }
        Ok(Self { paths, ids })
    }
}

impl FileTable {
    /// Create an empty `FileTable`.
    pub fn new() -> Self {
        Self {
            paths: Vec::new(),
            ids: HashMap::new(),
        }
    }

    /// Register `path` and return its `FileId`.
    ///
    /// Idempotent: re-registering an already-known path returns the same `FileId`.
    /// The path is normalized (leading `./` stripped, `..` components collapsed) before
    /// lookup; two paths that normalize to the same value get the same `FileId`.
    pub fn register(&mut self, path: &Path) -> FileId {
        let normalized = Self::normalize(path);
        if let Some(&id) = self.ids.get(&normalized) {
            return id;
        }
        // paths.len() → u64 is infallible on 64-bit (guarded by compile_error above);
        // usize is always ≤ u64 on 64-bit targets.
        #[allow(clippy::cast_possible_truncation)]
        let id = FileId::new(self.paths.len() as u64);
        // Clone into ids first, then move the original into paths — avoids a second clone.
        self.ids.insert(normalized.clone(), id);
        self.paths.push(normalized);
        id
    }

    /// Resolve a `FileId` back to a path, if it was registered.
    ///
    /// Returns `None` for IDs that were never registered with this table.
    pub fn lookup(&self, id: FileId) -> Option<&Path> {
        // usize::try_from is infallible on 64-bit targets (guarded by compile_error above),
        // but using try_from makes the conversion explicit and safe.
        let idx = usize::try_from(id.as_u64()).ok()?;
        self.paths.get(idx).map(PathBuf::as_path)
    }

    /// Return the number of registered files.
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    /// Return `true` if no files have been registered.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Normalize `path` for consistent lookup.
    ///
    /// Rules (I/O-free — no `fs::canonicalize`):
    /// - Leading `./` is stripped (CurDir component removed).
    /// - `..` components are collapsed by removing the preceding component.
    /// - Absolute paths are kept as-is.
    fn normalize(path: &Path) -> PathBuf {
        // Fast path: if the path contains no `.` or `..` components, it is already
        // normalized — return a cheap clone without allocating the components Vec.
        let needs_normalization = path
            .components()
            .any(|c| matches!(c, Component::CurDir | Component::ParentDir));
        if !needs_normalization {
            return path.to_path_buf();
        }

        let mut components: Vec<Component<'_>> = Vec::new();
        for component in path.components() {
            match component {
                Component::CurDir => {
                    // Strip `.` components (handles leading `./`)
                }
                Component::ParentDir => {
                    // Pop the last normal component to handle `..`
                    if matches!(components.last(), Some(Component::Normal(_))) {
                        components.pop();
                    } else {
                        components.push(component);
                    }
                }
                other => {
                    components.push(other);
                }
            }
        }
        components.iter().collect()
    }
}

impl Default for FileTable {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_id_accessors() {
        let id = FileId::new(42);
        assert_eq!(id.as_u64(), 42);
    }

    #[test]
    fn test_file_table_register_and_lookup() {
        let mut table = FileTable::new();
        assert!(table.is_empty());

        let id = table.register(Path::new("src/main.rs"));
        assert_eq!(table.len(), 1);
        assert!(!table.is_empty());

        let path = table.lookup(id);
        assert_eq!(path, Some(Path::new("src/main.rs")));

        // Idempotent: re-registering returns the same FileId
        let id2 = table.register(Path::new("src/main.rs"));
        assert_eq!(id, id2);
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn test_file_table_normalizes_paths() {
        let mut table = FileTable::new();

        let id1 = table.register(Path::new("./src/main.rs"));
        let id2 = table.register(Path::new("src/main.rs"));

        // Both paths normalize to "src/main.rs" — same FileId, single entry
        assert_eq!(id1, id2);
        assert_eq!(table.len(), 1);
    }
}
