//! Token counting using OpenAI's tiktoken tokenizer
//!
//! ARCHITECTURE: Uses cl100k_base encoding (GPT-3.5-turbo, GPT-4)
//! - Provides accurate token counts for LLM context window calculation
//! - Counts tokens before and after transformation
//! - Reports reduction statistics

use anyhow::Result;
use tiktoken_rs::cl100k_base;

/// Count tokens in text using cl100k_base encoding (GPT-3.5-turbo, GPT-4)
pub fn count_tokens(text: &str) -> Result<usize> {
    let bpe = cl100k_base()?;
    let tokens = bpe.encode_with_special_tokens(text);
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

    /// Calculate reduction percentage
    pub fn reduction_percentage(&self) -> f32 {
        if self.original == 0 {
            return 0.0;
        }
        ((self.original - self.transformed) as f32 / self.original as f32) * 100.0
    }

    /// Get tokens saved
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
    let mut count = 0;

    for ch in s.chars().rev() {
        if count > 0 && count % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
        count += 1;
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
