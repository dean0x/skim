//! Two-file mmap'd on-disk index format (`.skidx` + `.skpost`).
//!
//! Provides atomic write, memory-mapped read, and delta/tombstone support
//! for incremental updates.
//!
//! # File Layout
//!
//! ## `.skidx` (lookup table)
//! - Bytes 0..32: [`IndexHeader`] (magic, version, ngram_count, file_count, timestamp)
//! - Bytes 32+: Sorted array of [`IndexEntry`] (20 bytes each, sorted by `ngram_hash`)
//!
//! ## `.skpost` (postings)
//! - Flat array of [`PostingEntry`] (12 bytes each), referenced by `(offset, length)` in
//!   [`IndexEntry`].
//!
//! ## `lexical.delta` (incremental updates)
//! - Flat array of 20-byte records: `ngram_hash` (8 bytes LE) + [`PostingEntry`] (12 bytes).
//!
//! ## `lexical.tombstones` (deleted doc_ids)
//! - Sorted array of `u32` LE values (4 bytes each).
//!
//! # Atomicity
//!
//! Writes go to `.tmp` files first, then [`std::fs::rename`] swaps them in.
//! Any stale `.tmp` files from a previous crash are deleted before starting.
//!
//! # Module Layout
//!
//! - [`entry`] — [`IndexEntry`] on-disk record type
//! - [`writer`] — [`write_index`] atomic two-file write
//! - [`reader`] — [`IndexReader`] mmap'd reader with binary-search lookup
//! - [`delta`] — [`DeltaWriter`] and [`DeltaReader`] for incremental updates
//! - [`tombstones`] — [`Tombstones`] invalidated doc_id set

mod delta;
mod entry;
mod reader;
mod tombstones;
mod writer;

// Re-export the public surface so callers (query.rs, builder.rs) use the same
// paths they did before the split, with no import changes required.
pub use delta::{DeltaReader, DeltaWriter};
pub use reader::IndexReader;
pub use tombstones::Tombstones;
pub use writer::write_index;
