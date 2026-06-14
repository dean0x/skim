//! Conservative byte-length heuristic token counter.
//!
//! # Correctness guarantee
//!
//! For any BPE tokenizer operating over UTF-8 text, each token consumes **at
//! least one byte**. Therefore `token_count ≤ byte_count` holds by construction
//! for any BPE encoding, making `byte_len` a provably-safe ceiling regardless of
//! which specific tokenizer is in use (cl100k, o200k, or any future encoding).
//!
//! This guarantee is structural — it does not require calibration or a golden
//! corpus: it is a mathematical consequence of how BPE encodings work.
//!
//! # When to use
//!
//! Use this counter when the model provider is unknown or when a conservative
//! worst-case bound is needed without initialising a real tokenizer.

/// Count tokens using the conservative byte-length heuristic.
///
/// Returns the number of UTF-8 bytes in `text`. This is a proven-safe upper
/// bound on the actual token count for any BPE tokenizer over UTF-8.
///
/// This function is **infallible** — it never returns `Err` and never panics.
///
/// # Examples
///
/// ```
/// use rskim_tokens::heuristic::count_heuristic;
///
/// // For ASCII text, byte count equals character count
/// assert_eq!(count_heuristic("hello"), 5);
///
/// // For CJK text each character is 3 UTF-8 bytes — still a valid ceiling
/// let cjk = "日本語";
/// assert!(count_heuristic(cjk) >= cjk.chars().count());
///
/// // Empty input → 0
/// assert_eq!(count_heuristic(""), 0);
/// ```
#[must_use]
#[inline]
pub fn count_heuristic(text: &str) -> usize {
    text.len()
}
