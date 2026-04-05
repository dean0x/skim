//! Two-file mmap'd on-disk index format (`.skidx` + `.skpost`).
//!
//! Provides atomic write, memory-mapped read, and delta/tombstone support
//! for incremental updates.
