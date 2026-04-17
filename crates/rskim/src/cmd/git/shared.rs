//! Shared helpers for git subcommand parsers.
//!
//! # Why this module exists vs `git/mod.rs`
//!
//! `git/mod.rs` owns the run-entry-point, dispatch logic, and analytics
//! infrastructure (`build_analytics_label`, `finalize_git_output_owned`, etc.).
//! This module provides **stateless pure helpers** that individual sub-parsers
//! need but that have no business being in the dispatch layer:
//!
//! - [`scrub_git_url`] — credential URL scrubbing for push/fetch output.
//!
//! Keeping these helpers here avoids import cycles: `push.rs` and `commit.rs`
//! can `use super::shared::scrub_git_url` without pulling in the full mod.rs
//! dispatch machinery.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;

// ============================================================================
// Credential URL scrubbing
// ============================================================================

/// Regex matching the `<user:pass@>` or `<token@>` authority prefix in a URL.
///
/// Matches any sequence of non-`@` characters immediately before `@hostname`.
/// This covers:
/// - `https://token@github.com/org/repo`
/// - `https://user:password@gitlab.com/org/repo`
/// - `git://user@bitbucket.org/org/repo`
///
/// The substitution replaces only the `<auth>@` part, preserving the rest of
/// the URL so the user still sees where the push/fetch targeted.
static CREDENTIAL_URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(https?://|git://)[^@\s]+@").expect("credential URL regex is valid")
});

/// Scrub credential tokens from a git remote URL.
///
/// Replaces `https://<token>@host/...` with `https://host/...`, preserving the
/// host and path so that push/fetch output remains informative without leaking
/// credentials.
///
/// # DESIGN NOTE (AD-GP-1)
///
/// Git push (and clone/fetch) can embed auth tokens in the remote URL when
/// callers use `https://<token>@github.com/org/repo` for scripted pushes.
/// These tokens appear verbatim in push output on stderr.  We scrub them so
/// that skim-compressed push output never contains credentials.
///
/// We keep the URL rather than stripping it entirely because the host + path
/// tells the caller exactly which remote was targeted — useful information.
///
/// Returns a [`Cow<str>`]:
/// - `Borrowed` when no credentials were found (zero allocation).
/// - `Owned` when the regex matched and replacement occurred.
pub(super) fn scrub_git_url(s: &str) -> Cow<'_, str> {
    CREDENTIAL_URL_RE.replace_all(s, "${1}")
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scrub_git_url_token_auth() {
        let input = "remote: https://ghp_supersecrettoken@github.com/org/repo.git";
        let result = scrub_git_url(input);
        assert!(!result.contains("ghp_supersecrettoken"), "token should be scrubbed");
        assert!(result.contains("github.com/org/repo.git"), "URL remainder preserved");
    }

    #[test]
    fn test_scrub_git_url_user_password() {
        let input = "To https://user:hunter2@gitlab.com/org/repo.git";
        let result = scrub_git_url(input);
        assert!(!result.contains("hunter2"), "password should be scrubbed");
        assert!(!result.contains("user:"), "username should be scrubbed");
        assert!(result.contains("gitlab.com/org/repo.git"));
    }

    #[test]
    fn test_scrub_git_url_no_credentials_borrowed() {
        let input = "To https://github.com/org/repo.git";
        let result = scrub_git_url(input);
        // When no credentials are present, the original string is returned as Cow::Borrowed.
        assert_eq!(result.as_ref(), input);
    }

    #[test]
    fn test_scrub_git_url_git_protocol() {
        let input = "git://user@bitbucket.org/org/repo.git";
        let result = scrub_git_url(input);
        assert!(!result.contains("user@"), "user auth should be scrubbed");
        assert!(result.contains("bitbucket.org"));
    }

    #[test]
    fn test_scrub_git_url_plain_text_unchanged() {
        let input = "Everything up-to-date";
        let result = scrub_git_url(input);
        assert_eq!(result.as_ref(), input);
    }
}
