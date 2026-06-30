//! Per-content-type compression engines (#304 Phase 2).
//!
//! Each submodule implements one arm of the content-type routing table:
//!
//! - `code` — rskim-core AST transform for fenced code blocks
//! - `log` — thin adapter over `crate::log::compress_log`
//! - `json` — new valid-JSON structural compressor (D5)
//! - `mixed` — single-pass, CRLF-aware fence scanner + per-fence routing

pub(crate) mod code;
pub(crate) mod json;
pub(crate) mod log;
pub(crate) mod mixed;
