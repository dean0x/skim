//! Already-compact command acknowledgement (AD-RW-2).
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
//!
//! # AD-RW-11 (2026-04-11) — prettier and rustfmt --check acknowledgement
//!
//! `prettier --check` and `rustfmt --check` invoked without explicit file/glob
//! arguments emit near-zero output on a clean codebase: exit 0, empty stdout.
//! Rewriting to `skim lint prettier/rustfmt` in this case produces a `LINT OK`
//! header that is longer than the original empty output. The ACK list causes
//! the original command to be echoed as-is, respecting the compress-or-skip rule.
//!
//! When file arguments ARE present and formatting issues exist, output is large
//! enough that skim compression is worthwhile — handled by the rewrite rules.
//!
//! NOTE: ACK prefix matching uses prefix order, so these bare-prefix entries
//! (`["prettier", "--check"]`) only match when the command ends there or has
//! additional flag-only args. Commands with positional file args fall through
//! to the rewrite rule (`prettier --check src/`) because the ACK check happens
//! on the same token sequence. Both behaviours coexist correctly because:
//! - `prettier --check` → matches ACK (bare check, no files specified)
//! - `prettier --check src/` → `src/` is a positional arg beyond `--check`,
//!   BUT the ACK prefix `["prettier", "--check"]` is a prefix of any prettier
//!   --check invocation. This means ALL `prettier --check ...` commands are ACKed.
//!
//! DESIGN DECISION: We acknowledge the entire `prettier --check` prefix family.
//! The rationale: even with file args, the output list is brief enough that
//! `skim lint prettier` provides marginal compression value. The overhead of the
//! skim header outweighs the gain for small to medium projects. This aligns with
//! the compress-or-skip feedback rule.

/// Prefix patterns for commands whose output is already near-optimal.
///
/// Each inner slice is a prefix of shell tokens. A command segment matches
/// if its token slice starts with the pattern tokens.
pub(super) const ACK_PREFIX_PATTERNS: &[&[&str]] = &[
    // `git worktree list` is a small table; no meaningful compression is possible.
    &["git", "worktree", "list"],
    // AD-RW-11: `prettier --check [files]` output is near-optimal (empty on clean,
    // brief file list on failure). Skim header overhead exceeds compression gain.
    &["prettier", "--check"],
    // AD-RW-11: `npx prettier --check [files]` — same rationale as above.
    &["npx", "prettier", "--check"],
    // AD-RW-11: `rustfmt --check [files]` — empty on clean, short diff headers on
    // failure. The skim LINT OK wrapper adds tokens without reducing agent load.
    &["rustfmt", "--check"],
    // AD-RW-11: `cargo fmt --check` is a `rustfmt --check`-equivalent wrapper;
    // same empty-on-clean contract. Both the short form and the `--` pass-through
    // form are ACKed so `skim rewrite` echoes them unchanged.
    &["cargo", "fmt", "--check"],
    // AD-RW-11: `cargo fmt -- --check` — pass-through variant. The `--` token is
    // treated literally by `is_segment_ack`; there is no compound-operator
    // splitting on bare `--` (CompoundOp only recognizes `&&|;`).
    &["cargo", "fmt", "--", "--check"],
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

    // AD-RW-11: prettier --check acknowledgement

    #[test]
    fn test_is_segment_ack_prettier_check() {
        assert!(
            is_segment_ack(&["prettier", "--check"]),
            "prettier --check must be acknowledged"
        );
    }

    #[test]
    fn test_is_segment_ack_prettier_check_with_files() {
        assert!(
            is_segment_ack(&["prettier", "--check", "src/"]),
            "prettier --check src/ must be acknowledged (entire family)"
        );
    }

    #[test]
    fn test_is_segment_ack_npx_prettier_check() {
        assert!(
            is_segment_ack(&["npx", "prettier", "--check"]),
            "npx prettier --check must be acknowledged"
        );
    }

    #[test]
    fn test_is_segment_ack_rustfmt_check() {
        assert!(
            is_segment_ack(&["rustfmt", "--check"]),
            "rustfmt --check must be acknowledged"
        );
    }

    #[test]
    fn test_is_segment_ack_rustfmt_check_with_file() {
        assert!(
            is_segment_ack(&["rustfmt", "--check", "src/main.rs"]),
            "rustfmt --check src/main.rs must be acknowledged"
        );
    }

    #[test]
    fn test_is_segment_ack_prettier_format_not_matched() {
        // `prettier src/` without --check is a format operation, not acknowledged.
        assert!(
            !is_segment_ack(&["prettier", "src/"]),
            "prettier without --check must NOT be acknowledged"
        );
    }

    #[test]
    fn test_is_segment_ack_rustfmt_without_check_not_matched() {
        // `rustfmt src/main.rs` without --check formats in-place, not acknowledged.
        assert!(
            !is_segment_ack(&["rustfmt", "src/main.rs"]),
            "rustfmt without --check must NOT be acknowledged"
        );
    }

    // AD-RW-11: cargo fmt --check acknowledgement (added in evaluator follow-up)

    #[test]
    fn test_is_segment_ack_cargo_fmt_check() {
        assert!(
            is_segment_ack(&["cargo", "fmt", "--check"]),
            "cargo fmt --check must be acknowledged"
        );
    }

    #[test]
    fn test_is_segment_ack_cargo_fmt_dashdash_check() {
        assert!(
            is_segment_ack(&["cargo", "fmt", "--", "--check"]),
            "cargo fmt -- --check must be acknowledged (pass-through variant)"
        );
    }

    #[test]
    fn test_is_segment_ack_cargo_fmt_without_check_not_matched() {
        // `cargo fmt` without --check reformats in-place; not acknowledged.
        assert!(
            !is_segment_ack(&["cargo", "fmt"]),
            "cargo fmt without --check must NOT be acknowledged"
        );
    }

    #[test]
    fn test_is_segment_ack_cargo_fmt_check_with_trailing_args() {
        assert!(
            is_segment_ack(&["cargo", "fmt", "--check", "--manifest-path", "Cargo.toml"]),
            "cargo fmt --check with trailing args must be acknowledged"
        );
    }
}
