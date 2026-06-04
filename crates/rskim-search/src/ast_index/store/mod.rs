//! On-disk two-file mmap'd index for AST structural n-grams.
//!
//! # Format overview
//!
//! Two files are written to a directory: `ast_index.skidx` (fixed-size header,
//! sorted bigram/trigram lookup tables, per-file metadata) and
//! `ast_index.skpost` (concatenated posting lists).  Both are distinct from the
//! lexical index files (`index.skidx` / `index.skpost`) so the same `.skim/`
//! directory can hold both indexes simultaneously.
//!
//! ## Magic / version
//!
//! Magic `b"SKAX"`, version `1`.  Any layout change increments the version.
//!
//! # Public API
//!
//! [`AstIndexBuilder`] — call `add_file_ngrams` or `add_file` once per file,
//! then `build()`.  Use `build_from_files` for parallel bulk construction.
//!
//! [`AstIndexReader`] — returned by the builder.  Use `lookup_bigram` /
//! `lookup_trigram` for posting-list access and `file_meta` for per-file
//! metadata.
//!
//! [`AstPosting`] — individual posting element (`doc_id` + `count`).
//!
//! # Out of scope
//!
//! Structural query / scoring (Wave 3f) and CLI wiring (Wave 3g).

mod builder;
pub(crate) mod format;
mod reader;

pub use builder::AstIndexBuilder;
pub use format::AstFileMetaEntry;
pub use reader::{AstIndexReader, AstPosting};
