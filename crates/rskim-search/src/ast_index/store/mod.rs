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
//! Magic `b"SKAX"`, version `2`.  Any layout change increments the version.
//!
//! ## AST INDEX FORMAT NOTE (v2)
//!
//! v2 extends `AstFileMetaEntry` from 5 to 15 bytes, adds `avg_max_depth` in
//! previously-reserved header bytes [38..42], and stores synthetic n-grams from
//! the AST Pattern Library alongside real n-grams. All v1 indexes must be
//! rebuilt (`skim search index --rebuild`). Auto-rebuild is wired in Wave 3f/3g.
//!
//! # Public API
//!
//! [`AstIndexBuilder`] — call `add_file_ngrams` or `add_file` once per file,
//! then `build()`.  Use `build_from_files` for parallel bulk construction.
//!
//! [`AstIndexReader`] — returned by the builder.  Use `lookup_bigram` /
//! `lookup_trigram` for posting-list access, `file_meta` for per-file
//! metadata, `file_metrics` for structural metrics, and `index_version` for
//! cheap version probing.
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
