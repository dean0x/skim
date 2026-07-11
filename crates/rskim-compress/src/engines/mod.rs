//! Per-content-type compression engines (#304 Phase 2, P0.1 #427).
//!
//! Each submodule implements one arm of the content-type routing table:
//!
//! - `log` — thin adapter over `crate::log::compress_log`
//! - `json` — new valid-JSON structural compressor (D5)
//! - `mixed` — single-pass, CRLF-aware fence scanner + per-fence routing
//!
//! Note: the `code` engine (`engines/code.rs`) was deleted in P0.1 (#427).
//! Under ADR-007 (lossless-only egress), code blocks always pass through
//! byte-identical on the proxy path — no rskim-core AST transform is applied.

pub(crate) mod json;
pub(crate) mod log;
pub(crate) mod mixed;
