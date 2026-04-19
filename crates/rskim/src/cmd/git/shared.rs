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
/// Matches the userinfo component of a URL authority (RFC 3986 §3.2.1):
/// any non-`@`, non-whitespace, non-`/`, non-`?`, non-`#` characters
/// immediately before `@hostname`.  The character class `[^@\s/?#]` restricts
/// the match to the authority component, preventing over-greedy matching when
/// a URL appears inside a query parameter of another URL.
///
/// This covers:
/// - `https://token@github.com/org/repo`
/// - `https://user:password@gitlab.com/org/repo`
/// - `git://user@bitbucket.org/org/repo`
/// - `ssh://user:token@github.com/org/repo` (AD-GP-1)
///
/// The substitution replaces only the `<auth>@` part, preserving the rest of
/// the URL so the user still sees where the push/fetch targeted.
///
/// # DESIGN NOTE (AD-GP-3) — RFC 3986 authority restriction
///
/// The original pattern `[^@\s]+@` was over-greedy: given a callback URL in a
/// query string like `https://host/?cb=https://user:pass@bad.com`, the regex
/// would skip the real host and scrub from `https://` all the way through
/// `bad.com`.  Restricting to `[^@\s/?#]+@` confines the match to the URL
/// authority segment, preserving the primary host and only stripping credentials
/// that appear before the first `/`, `?`, or `#`.
///
/// # DESIGN NOTE (AD-GP-1) — ssh:// coverage
///
/// `ssh://user:token@host/path` embeds credentials in the same authority segment
/// as https/git.  Added `ssh://` to the scheme alternation so SSH-cloned repos
/// with embedded credentials are scrubbed on the same code path.
static CREDENTIAL_URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(https?://|git://|ssh://)[^@\s/?#]+@").expect("credential URL regex is valid")
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

/// Scrub credential tokens from every line of a multi-line string.
///
/// Applies [`scrub_git_url`] line-by-line and joins with `\n` (normalising
/// `\r\n` to `\n` — intentional for Unix-first CLI output).
///
/// # Allocation behaviour (PF-024)
///
/// [`scrub_git_url`] returns [`Cow::Borrowed`] when a line contains no
/// credentials (the common case), producing zero per-line heap allocations
/// for clean lines.  By collecting `Cow` items and calling `.join("\n")` once,
/// this function avoids the N `into_owned()` calls that the previous inline
/// pattern used — one `String` is allocated for the joined result regardless
/// of whether any line was modified.
pub(super) fn scrub_lines(input: &str) -> String {
    input
        .lines()
        .map(scrub_git_url)
        .collect::<Vec<_>>()
        .join("\n")
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
        assert!(
            !result.contains("ghp_supersecrettoken"),
            "token should be scrubbed"
        );
        assert!(
            result.contains("github.com/org/repo.git"),
            "URL remainder preserved"
        );
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

    /// Regression test for AD-GP-3: the regex must not be over-greedy when a
    /// credential-bearing URL appears inside a query parameter of another URL.
    /// The primary host (`github.com`) must be preserved; only the nested
    /// credentials (`user:pass`) must be stripped.
    #[test]
    fn test_scrub_preserves_host_when_nested_url_in_path() {
        let input = "https://github.com/?cb=https://user:pass@bad.com";
        let result = scrub_git_url(input);
        assert!(
            result.contains("github.com"),
            "primary host must be preserved"
        );
        assert!(
            !result.contains("user:pass"),
            "nested credentials must be stripped"
        );
    }

    // ========================================================================
    // scrub_lines tests
    // ========================================================================

    #[test]
    fn test_scrub_lines_clean_input_returns_normalized_string() {
        // No credentials present — each line is Cow::Borrowed.
        // Output must equal the input (modulo \r\n normalisation).
        let input = "Everything up-to-date\nTo https://github.com/org/repo.git";
        let result = scrub_lines(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_scrub_lines_credentials_stripped() {
        let input = "remote: https://ghp_abc@github.com/repo.git\nEverything up-to-date";
        let result = scrub_lines(input);
        assert!(!result.contains("ghp_abc"), "token must be stripped");
        assert!(result.contains("github.com/repo.git"), "host/path preserved");
        assert!(result.contains("Everything up-to-date"), "clean line preserved");
    }

    #[test]
    fn test_scrub_lines_normalises_crlf() {
        // \r\n input must be normalised to \n in the output.
        let input = "line one\r\nline two\r\n";
        let result = scrub_lines(input);
        assert!(!result.contains('\r'), "\\r must be normalised away");
    }

    #[test]
    fn test_scrub_lines_empty_input() {
        assert_eq!(scrub_lines(""), "");
    }

    /// ssh:// URLs with embedded credentials are scrubbed (AD-GP-1).
    #[test]
    fn test_scrub_ssh_url() {
        let input = "ssh://token@github.com/repo.git";
        let result = scrub_git_url(input);
        assert!(
            result.contains("ssh://"),
            "scheme should be preserved: {result}"
        );
        assert!(
            !result.contains("token"),
            "credentials should be scrubbed: {result}"
        );
        assert!(
            result.contains("github.com/repo.git"),
            "host+path should be preserved: {result}"
        );
    }
}
