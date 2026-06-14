//! Token counting using OpenAI's tiktoken tokenizer
//!
//! ARCHITECTURE: Uses cl100k_base encoding (GPT-3.5-turbo, GPT-4) via rskim-tokens.
//! - Delegates to `rskim_tokens::Counter` for deterministic, panic-free counting.
//! - Preserves `encode_with_special_tokens` semantics (AC3 / constraint 13).
//! - Counter is constructed once and cached globally (constraint 11 latency).
//!
//! Public signature `pub(crate) fn count_tokens(text: &str) -> Result<usize>` is
//! FROZEN — zero call-site signature churn (AC15). Callers (cascade.rs, process.rs,
//! guardrail.rs, analytics/mod.rs, output/mod.rs, cmd/discover.rs) are unchanged.
//!
//! TokenStats and format_number remain binary-private (OQ7) — CLI display concerns
//! that do not belong in the library API.

use anyhow::Result;
use rskim_tokens::{Counter, Encoding};
use std::sync::OnceLock;

/// Global cl100k counter (lazy-initialised on first use via OnceLock).
///
/// Constructed once; subsequent calls to `count_tokens` reuse the same counter
/// for performance (constraint 11 — avoid recreating tokenizer on every call).
static COUNTER: OnceLock<Counter> = OnceLock::new();

/// Get or initialize the global cl100k counter.
fn get_counter() -> &'static Counter {
    COUNTER.get_or_init(|| {
        // Counter::new for cl100k is practically infallible (embedded vocab in tiktoken-rs).
        // On the dead Err path (embedded vocab corrupt), fall back to the byte-length
        // heuristic so the process continues rather than crashing (constraint 4).
        build_counter_with_fallback()
    })
}

/// Build the cl100k counter, falling back to the heuristic on (practically dead) init failure.
///
/// Separated from the OnceLock closure so the error-handling logic is readable.
///
/// Uses `Counter::heuristic()` (infallible by construction) as the fallback so
/// no panic macro is required on the dead error path (AC10 no-panic invariant).
fn build_counter_with_fallback() -> Counter {
    match Counter::new(Encoding::Cl100k) {
        Ok(counter) => counter,
        Err(e) => {
            // Practically dead: tiktoken embeds its vocab at compile time.
            // Counter::heuristic() is unconditionally infallible — no panic macro needed.
            eprintln!(
                "[skim] warning: cl100k tokenizer init failed ({e}); \
                 falling back to byte-length heuristic"
            );
            Counter::heuristic()
        }
    }
}

/// Count tokens in text using cl100k_base encoding (GPT-3.5-turbo, GPT-4).
///
/// Delegates to [`rskim_tokens::Counter`] with `Encoding::Cl100k`, preserving
/// `encode_with_special_tokens` semantics (constraint 13 / AC3).
///
/// # Frozen signature (AC15)
///
/// The signature `pub(crate) fn count_tokens(text: &str) -> Result<usize>` is
/// frozen. All existing call sites handle `Err` with their current patterns and
/// require no changes.
pub(crate) fn count_tokens(text: &str) -> Result<usize> {
    Ok(get_counter().count(text))
}

/// Statistics for token reduction
#[derive(Debug, Clone)]
pub(crate) struct TokenStats {
    /// Original token count
    pub(crate) original: usize,
    /// Transformed token count
    pub(crate) transformed: usize,
}

impl TokenStats {
    /// Create new token stats
    pub(crate) fn new(original: usize, transformed: usize) -> Self {
        Self {
            original,
            transformed,
        }
    }

    /// Calculate reduction percentage (negative if transformed is larger)
    pub(crate) fn reduction_percentage(&self) -> f32 {
        if self.original == 0 {
            return 0.0;
        }
        ((self.original as f32 - self.transformed as f32) / self.original as f32) * 100.0
    }

    /// Format stats for display
    pub(crate) fn format(&self) -> String {
        format!(
            "{} tokens → {} tokens ({:.1}% reduction)",
            format_number(self.original),
            format_number(self.transformed),
            self.reduction_percentage()
        )
    }
}

/// Format number with thousands separators
pub(crate) fn format_number(n: usize) -> String {
    let s = n.to_string();
    let mut result = String::new();

    for (count, ch) in s.chars().rev().enumerate() {
        if count > 0 && count % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }

    result.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_tokens() {
        let text = "Hello, world!";
        let count = count_tokens(text).unwrap();
        assert!(count > 0);
        assert!(count < 10);
    }

    #[test]
    fn test_token_stats() {
        let stats = TokenStats::new(1000, 200);
        assert_eq!(stats.reduction_percentage(), 80.0);
    }

    #[test]
    fn test_format_number() {
        assert_eq!(format_number(1000), "1,000");
        assert_eq!(format_number(1000000), "1,000,000");
        assert_eq!(format_number(123), "123");
    }

    #[test]
    fn test_stats_format() {
        let stats = TokenStats::new(1000, 200);
        let formatted = stats.format();
        assert!(formatted.contains("1,000"));
        assert!(formatted.contains("200"));
        assert!(formatted.contains("80.0%"));
    }
}
