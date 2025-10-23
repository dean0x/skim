//! Token counting using OpenAI's tiktoken tokenizer
//!
//! ARCHITECTURE: Uses cl100k_base encoding (GPT-3.5-turbo, GPT-4)
//! - Provides accurate token counts for LLM context window calculation
//! - Counts tokens before and after transformation
//! - Reports reduction statistics
//! - Lazy initialization to avoid recreating tokenizer on every call

use anyhow::Result;
use std::sync::OnceLock;
use tiktoken_rs::{cl100k_base, CoreBPE};

/// Global tokenizer instance (lazy-initialized on first use)
static TOKENIZER: OnceLock<CoreBPE> = OnceLock::new();

/// Get or initialize the global tokenizer instance
fn get_tokenizer() -> &'static CoreBPE {
    TOKENIZER.get_or_init(|| {
        cl100k_base().expect("Failed to initialize cl100k_base tokenizer")
    })
}

/// Count tokens in text using cl100k_base encoding (GPT-3.5-turbo, GPT-4)
pub fn count_tokens(text: &str) -> Result<usize> {
    let tokenizer = get_tokenizer();
    let tokens = tokenizer.encode_with_special_tokens(text);
    Ok(tokens.len())
}

/// Statistics for token reduction
#[derive(Debug, Clone)]
pub struct TokenStats {
    /// Original token count
    pub original: usize,
    /// Transformed token count
    pub transformed: usize,
}

impl TokenStats {
    /// Create new token stats
    pub fn new(original: usize, transformed: usize) -> Self {
        Self { original, transformed }
    }

    /// Calculate reduction percentage (negative if transformed is larger)
    pub fn reduction_percentage(&self) -> f32 {
        if self.original == 0 {
            return 0.0;
        }
        ((self.original as f32 - self.transformed as f32) / self.original as f32) * 100.0
    }

    /// Get tokens saved
    #[allow(dead_code)]
    pub fn tokens_saved(&self) -> usize {
        self.original.saturating_sub(self.transformed)
    }

    /// Format stats for display
    pub fn format(&self) -> String {
        format!(
            "{} tokens → {} tokens ({:.1}% reduction)",
            format_number(self.original),
            format_number(self.transformed),
            self.reduction_percentage()
        )
    }
}

/// Format number with thousands separators
fn format_number(n: usize) -> String {
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
        assert!(count < 10); // Should be around 3-4 tokens
    }

    #[test]
    fn test_token_stats() {
        let stats = TokenStats::new(1000, 200);
        assert_eq!(stats.reduction_percentage(), 80.0);
        assert_eq!(stats.tokens_saved(), 800);
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
