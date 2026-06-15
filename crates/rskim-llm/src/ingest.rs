//! Chunked ingestion API for streaming LLM request bodies.
//!
//! The [`ChunkIngestionBuilder`] accumulates byte chunks and parses at end-of-input.
//! The parsed result is byte-identical to whole-body parsing of the same bytes
//! (Invariant 7).
//!
//! # Design
//!
//! The chunked API is intentionally simple: it accumulates all chunks into an
//! internal buffer and parses once at the end. This mirrors the whole-body parse
//! exactly, guaranteeing output equivalence by construction.
//!
//! Streaming JSON parsers (partial-parse-per-chunk) were rejected because they would
//! complicate the model and introduce divergence risk. The use case for chunked
//! ingestion is HTTP streaming where the body arrives in segments — the final parse
//! happens once the last byte is received, not incrementally.
//!
//! # DoS / resource-exhaustion bound (OWASP A04)
//!
//! The streaming trust boundary requires an explicit byte cap. Without one, a caller
//! feeding chunks from an HTTP stream can drive unbounded heap growth before any
//! validation runs — the depth bound checked in [`crate::parse()`] does not bound
//! total byte size. The `max_bytes` field (default [`DEFAULT_MAX_BYTES`]) caps the
//! accumulated buffer in [`ChunkIngestionBuilder::push`], returning
//! [`crate::LlmError::BodyTooLarge`] eagerly rather than after full accumulation.
//!
//! Callers that need a larger limit can use [`ChunkIngestionBuilder::with_max_bytes`].
//! The limit must be set before the first [`push`](ChunkIngestionBuilder::push) call;
//! subsequent pushes enforce it.

use crate::{LlmError, ParsedBody, Result, provider::Provider};

/// Default maximum body size for chunked ingestion: 64 MiB.
///
/// Matches the spirit of [`crate::MAX_DEPTH`] as a generous upper bound for any
/// real Anthropic/OpenAI request body. The existing 10 MB linearity test body
/// is well within this limit.
pub const DEFAULT_MAX_BYTES: usize = 64 * 1024 * 1024;

/// Builder for chunked ingestion of a LLM request body.
///
/// Accumulates byte chunks and produces a [`ParsedBody`] when [`finish`](Self::finish)
/// is called.
///
/// # Body size limit
///
/// [`push`](Self::push) enforces a cumulative byte cap (default [`DEFAULT_MAX_BYTES`] =
/// 64 MiB, configurable via [`with_max_bytes`](Self::with_max_bytes)). Exceeding the
/// limit returns [`crate::LlmError::BodyTooLarge`] immediately so callers do not need
/// to cap the upstream stream independently.
///
/// # Examples
///
/// ```
/// use rskim_llm::ChunkIngestionBuilder;
///
/// let json = r#"{"model":"claude-3-5-sonnet-20241022","messages":[{"role":"user","content":"Hi"}],"max_tokens":100}"#;
///
/// let mut builder = ChunkIngestionBuilder::new();
/// for chunk in json.as_bytes().chunks(4) {
///     builder.push(chunk)?;
/// }
/// let body = builder.finish()?;
/// # Ok::<(), rskim_llm::LlmError>(())
/// ```
#[derive(Debug)]
pub struct ChunkIngestionBuilder {
    buf: Vec<u8>,
    provider_hint: Option<Provider>,
    max_bytes: usize,
}

impl Default for ChunkIngestionBuilder {
    fn default() -> Self {
        Self {
            buf: Vec::new(),
            provider_hint: None,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

impl ChunkIngestionBuilder {
    /// Create a new empty builder with the default byte limit ([`DEFAULT_MAX_BYTES`]).
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a builder with a provider hint, skipping auto-detection.
    pub fn with_provider(provider: Provider) -> Self {
        Self {
            provider_hint: Some(provider),
            ..Self::default()
        }
    }

    /// Set the maximum accumulated body size in bytes.
    ///
    /// [`push`](Self::push) returns [`crate::LlmError::BodyTooLarge`] as soon as the
    /// cumulative size would exceed this limit. Defaults to [`DEFAULT_MAX_BYTES`] (64 MiB).
    ///
    /// Call this before the first [`push`](Self::push); bytes already in the buffer
    /// are not re-checked when the limit is lowered.
    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Append a chunk of bytes to the internal buffer.
    ///
    /// # Errors
    ///
    /// Returns [`crate::LlmError::BodyTooLarge`] if the cumulative byte count would
    /// exceed the configured `max_bytes` limit (default [`DEFAULT_MAX_BYTES`] = 64 MiB).
    /// No bytes are appended when the error is returned.
    pub fn push(&mut self, chunk: &[u8]) -> Result<()> {
        let new_len = self.buf.len().saturating_add(chunk.len());
        if new_len > self.max_bytes {
            return Err(LlmError::BodyTooLarge(new_len));
        }
        self.buf.extend_from_slice(chunk);
        Ok(())
    }

    /// Finish ingestion and parse the accumulated bytes.
    ///
    /// Parses the accumulated buffer as a complete LLM request body. The result is
    /// byte-identical to parsing the same bytes in a single call to [`crate::parse()`].
    ///
    /// # Errors
    ///
    /// Same error conditions as [`crate::parse()`].
    pub fn finish(self) -> Result<ParsedBody> {
        match self.provider_hint {
            Some(p) => crate::parse::parse_with_provider(&self.buf, p),
            None => crate::parse::parse(&self.buf),
        }
    }

    /// Return the number of bytes accumulated so far.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Return true if no bytes have been pushed.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}
