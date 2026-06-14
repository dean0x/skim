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

use crate::{ParsedBody, Result, provider::Provider};

/// Builder for chunked ingestion of a LLM request body.
///
/// Accumulates byte chunks and produces a [`ParsedBody`] when [`finish`](Self::finish)
/// is called.
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
///     builder.push(chunk);
/// }
/// let body = builder.finish()?;
/// # Ok::<(), rskim_llm::LlmError>(())
/// ```
#[derive(Debug, Default)]
pub struct ChunkIngestionBuilder {
    buf: Vec<u8>,
    provider_hint: Option<Provider>,
}

impl ChunkIngestionBuilder {
    /// Create a new empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a builder with a provider hint, skipping auto-detection.
    pub fn with_provider(provider: Provider) -> Self {
        Self {
            buf: Vec::new(),
            provider_hint: Some(provider),
        }
    }

    /// Append a chunk of bytes to the internal buffer.
    ///
    /// This operation is infallible — validation happens at [`finish`](Self::finish).
    pub fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// Finish ingestion and parse the accumulated bytes.
    ///
    /// Parses the accumulated buffer as a complete LLM request body. The result is
    /// byte-identical to parsing the same bytes in a single call to [`crate::parse`].
    ///
    /// # Errors
    ///
    /// Same error conditions as [`crate::parse`].
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
