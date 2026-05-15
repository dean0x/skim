//! Temporal git history parsing for downstream ranking signals.
//!
//! # Architecture
//!
//! - [`GixSource`]: pure gix-based implementation of [`TemporalSource`].
//! - [`is_fix_commit`]: standalone fix-classification predicate.
//!
//! No I/O outside of [`GixSource::parse_history`]. All gix types are converted
//! to shared [`CommitInfo`]/[`FileChangeInfo`] types at the parser boundary.

mod git_parser;

pub use git_parser::GixSource;

use regex::Regex;

/// Regex that identifies "fix" commits by subject keywords.
///
/// Compiled once and reused via `std::sync::LazyLock`.
#[allow(clippy::expect_used)] // hardcoded pattern is always valid
static FIX_REGEX: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"(?i)\b(fix|bug|hotfix|patch|revert)\b").expect("valid regex")
});

/// Returns `true` when the commit message matches a fix-related keyword.
///
/// Recognised keywords (case-insensitive, word-boundary anchored):
/// `fix`, `bug`, `hotfix`, `patch`, `revert`.
///
/// # Examples
///
/// ```rust
/// use rskim_search::is_fix_commit;
///
/// assert!(is_fix_commit("fix: null pointer dereference"));
/// assert!(is_fix_commit("Revert \"bad change\""));
/// assert!(!is_fix_commit("add feature X"));
/// assert!(!is_fix_commit("prefix_word"));  // word-boundary, not substring
/// ```
#[must_use]
pub fn is_fix_commit(message: &str) -> bool {
    FIX_REGEX.is_match(message)
}
