//! Co-change matrix: binary persistence of file co-change counts derived from
//! git history, with Jaccard similarity and top-K retrieval.
//!
//! # Usage
//!
//! ```rust,ignore
//! use std::collections::HashMap;
//! use std::path::PathBuf;
//! use rskim_search::cochange::{CochangeMatrixBuilder, CochangeMatrixReader};
//! use rskim_search::FileId;
//!
//! // Build phase (once per index refresh)
//! let dir = PathBuf::from("/tmp/my_index");
//! let builder = CochangeMatrixBuilder::new(dir.clone()).unwrap();
//! let path_map: HashMap<PathBuf, FileId> = HashMap::new(); // populate from manifest
//! let stats = builder.build(&history_result, &path_map).unwrap();
//!
//! // Query phase
//! let reader = CochangeMatrixReader::open(&dir).unwrap();
//! let j = reader.jaccard(FileId(0), FileId(1)).unwrap();
//! let partners = reader.pairs_for_file(FileId(0)).unwrap();
//! ```

pub(crate) mod builder;
pub(crate) mod format;
pub(crate) mod reader;
#[cfg(test)]
pub(crate) mod test_helpers;

pub use builder::CochangeMatrixBuilder;
pub use reader::CochangeMatrixReader;
