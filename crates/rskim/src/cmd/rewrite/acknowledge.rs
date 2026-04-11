//! Already-compact command acknowledgement (AD-2).
//!
//! Some commands produce inherently small, near-optimal output — rewriting
//! them would add overhead without savings. This module maintains the
//! canonical list of such commands so that `classify_command` can return
//! `CommandClassification::AlreadyCompact` instead of `Unhandled`.
//!
//! # Design note
//! Acknowledged commands do NOT have a skim handler and do NOT appear in
//! the rewrite rule table. They exist only in this module so that:
//! 1. `skim rewrite` can emit the *original* command on stdout (exit 0).
//! 2. `skim discover` can stop flagging them as non-rewritable gaps.
//!
//! # Adding new entries
//! Add a `&[&str]` slice to `ACK_PREFIX_PATTERNS`. Prefix matching is used:
//! a pattern `&["git", "worktree", "list"]` matches any command that *starts
//! with* those three tokens, including `git worktree list --porcelain`.

/// Prefix patterns for commands whose output is already near-optimal.
///
/// Each inner slice is a prefix of shell tokens. A command segment matches
/// if its token slice starts with the pattern tokens.
pub(super) const ACK_PREFIX_PATTERNS: &[&[&str]] = &[
    // `git worktree list` is a small table; no meaningful compression is possible.
    &["git", "worktree", "list"],
];

/// Return `true` if `tokens` starts with any acknowledged-compact prefix.
///
/// # Examples
/// ```ignore
/// assert!(is_segment_ack(&["git", "worktree", "list"]));
/// assert!(is_segment_ack(&["git", "worktree", "list", "--porcelain"]));
/// assert!(!is_segment_ack(&["git", "worktree", "add", "path"]));
/// assert!(!is_segment_ack(&[]));
/// ```
pub(super) fn is_segment_ack(tokens: &[&str]) -> bool {
    ACK_PREFIX_PATTERNS
        .iter()
        .any(|pattern| tokens.len() >= pattern.len() && &tokens[..pattern.len()] == *pattern)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_segment_ack_worktree_list() {
        assert!(
            is_segment_ack(&["git", "worktree", "list"]),
            "exact match must be acknowledged"
        );
    }

    #[test]
    fn test_is_segment_ack_worktree_list_with_trailing_args() {
        assert!(
            is_segment_ack(&["git", "worktree", "list", "--porcelain"]),
            "prefix match with trailing args must be acknowledged"
        );
    }

    #[test]
    fn test_is_segment_ack_worktree_add_not_matched() {
        assert!(
            !is_segment_ack(&["git", "worktree", "add", "path/to/worktree"]),
            "git worktree add must NOT be acknowledged"
        );
    }

    #[test]
    fn test_is_segment_ack_empty_tokens() {
        assert!(!is_segment_ack(&[]), "empty token list must not match");
    }

    #[test]
    fn test_is_segment_ack_shorter_than_pattern() {
        assert!(
            !is_segment_ack(&["git", "worktree"]),
            "prefix shorter than pattern must not match"
        );
    }

    #[test]
    fn test_is_segment_ack_single_token() {
        assert!(!is_segment_ack(&["git"]), "single token must not match");
    }

    #[test]
    fn test_is_segment_ack_unrelated_command() {
        assert!(
            !is_segment_ack(&["echo", "hello"]),
            "unrelated command must not match"
        );
    }
}
