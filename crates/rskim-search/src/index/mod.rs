//! Two-file mmap'd n-gram index for rskim-search.
//!
//! # File Format
//!
//! The index is split into two files in a single directory:
//!
//! - **`index.skidx`** — fixed-size header, sorted n-gram lookup table, and per-file
//!   metadata.  Memory-mapped for O(log n) binary-search lookups.
//! - **`index.skpost`** — variable-length posting lists (doc_id, field_id, position
//!   triplets).  Offsets and lengths stored in the `.skidx` lookup table.
//!
//! # Usage
//!
//! ```rust,ignore
//! use std::path::PathBuf;
//! use rskim_search::index::{NgramIndexBuilder, NgramIndexReader};
//! use rskim_search::{FileId, LayerBuilder, SearchQuery};
//!
//! // Build phase
//! let dir = PathBuf::from("/tmp/my_index");
//! let mut builder = NgramIndexBuilder::new(dir.clone()).unwrap();
//! builder.add_file(FileId(0), "fn main() { println!(\"hello\"); }", rskim_core::Language::Rust).unwrap();
//! let layer = builder.build().unwrap();
//!
//! // Query phase (or open from disk)
//! let reader = NgramIndexReader::open(&dir).unwrap();
//! let results = reader.search(&SearchQuery::new("main")).unwrap();
//! ```

mod builder;
mod format;
pub(crate) mod lang_map;
mod reader;

pub use builder::NgramIndexBuilder;
pub(crate) use format::FORMAT_VERSION as LEXICAL_FORMAT_VERSION;
pub use reader::NgramIndexReader;
