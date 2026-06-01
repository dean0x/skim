//! Security helpers for the skim CLI.
//!
//! Centralises credential scrubbing and safe-display sanitization so that
//! these concerns are not scattered across the `cmd` subtree.

/// Flags whose *immediately following* space-separated token is a credential.
/// Note: `-P` (port) intentionally omitted — it is not a credential.
const SENSITIVE_FLAGS: &[&str] = &[
    "-p",
    "-U",
    "-u",
    "-h",
    "-W",
    "--password",
    "--user",
    "--username",
    "--host",
];
/// Short flags that may have their value *attached* with no space (e.g. `-pS3cret`).
const ATTACHED_PREFIXES: &[&str] = &["-p", "-u", "-U"];
/// MySQL config-file flags whose value (path) must also be redacted.
const CONFIG_FILE_FLAGS: &[&str] = &["--defaults-file", "--defaults-extra-file"];

/// Classification of a single DB argument token for credential scrubbing.
///
/// Each variant encodes how the token (and possibly the next token) should be
/// handled when building the redacted output string.  `classify_token` maps a
/// raw token to one of these actions; `scrub_db_args` drives the state machine.
#[derive(Debug)]
enum TokenAction {
    /// Token is a connection-string URI containing embedded credentials.
    /// Replace the entire token with `[REDACTED_URI]`.
    RedactUri,
    /// Token is `--flag=value` where `flag` is sensitive.
    /// Replace with `{flag}=[REDACTED]`; the `flag` field carries the prefix.
    RedactEqualsValue { flag: String },
    /// Token is an attached short flag (`-pSecret`).
    /// Replace with `{prefix}[REDACTED]`; the `prefix` field carries the short flag.
    RedactAttached { prefix: String },
    /// Token is a standalone sensitive flag (`-p`, `--password`, `--defaults-file`, …).
    /// Keep the flag token as-is, then redact the *next* token.
    RedactNext,
    /// Token carries no credential information; emit it verbatim.
    Preserve,
}

/// Classify a single whitespace-split token for credential scrubbing.
///
/// Returns the [`TokenAction`] that `scrub_db_args` should apply to this token.
/// All five classification branches from the original while-loop are preserved
/// exactly, now expressed as a pure function without let-chains.
fn classify_token(tok: &str) -> TokenAction {
    // 1. Connection string URIs: postgresql://…@…, postgres://…@…, mysql://…@…
    if (tok.starts_with("postgresql://")
        || tok.starts_with("postgres://")
        || tok.starts_with("mysql://"))
        && tok.contains('@')
    {
        return TokenAction::RedactUri;
    }

    // 2. `--flag=value` form (sensitive flags and config-file flags)
    if let Some(eq_pos) = tok.find('=') {
        let flag = &tok[..eq_pos];
        if SENSITIVE_FLAGS.contains(&flag) || CONFIG_FILE_FLAGS.contains(&flag) {
            return TokenAction::RedactEqualsValue {
                flag: flag.to_string(),
            };
        }
    }

    // 3. Attached short flags: -pPassword, -uroot, -Uadmin (no space, single-dash only)
    if !tok.starts_with("--")
        && let Some(&prefix) = ATTACHED_PREFIXES
            .iter()
            .find(|&&p| tok.starts_with(p) && tok.len() > p.len())
    {
        return TokenAction::RedactAttached {
            prefix: prefix.to_string(),
        };
    }

    // 4. Space-separated sensitive flags and config-file flags
    if SENSITIVE_FLAGS.contains(&tok) || CONFIG_FILE_FLAGS.contains(&tok) {
        return TokenAction::RedactNext;
    }

    // 5. Non-sensitive token — preserve verbatim
    TokenAction::Preserve
}

/// Scrub credential values from a DB tool argument string.
///
/// DB CLIs accept credentials as flag-value pairs.  This function replaces the
/// value of every sensitive flag with `[REDACTED]` so that analytics labels
/// never persist passwords, usernames, or hostnames to disk.
///
/// # Flags redacted
///
/// | Short form  | Long form        | Tools      |
/// |-------------|------------------|------------|
/// | `-p`        | `--password`     | mysql      |
/// | `-P`        | (none)           | mysql port |
/// | `-U`        | `--username`     | psql       |
/// | `-u`        | `--user`         | mysql      |
/// | `-h`        | `--host`         | psql/mysql |
/// | `-W`        | `--password`     | psql       |
///
/// Both space-separated (`-p S3cret`) and equals-joined (`--password=S3cret`)
/// forms are redacted.
///
/// # Design
///
/// Operates on the pre-joined argument string (one token at a time after
/// splitting on whitespace) because that is what the call site produces.
/// This avoids a separate allocation path for every DB command invocation.
///
/// SQL query arguments (positional, no flag prefix) are preserved verbatim —
/// only known sensitive flag values are redacted.
///
/// Handles:
/// 1. Connection string URIs (`postgresql://user:pass@host`, `mysql://user:pass@host`)
/// 2. `--flag=value` form for sensitive and config-file flags
/// 3. Attached short flags with no space: `-pPassword`, `-uroot`, `-Uadmin`
/// 4. Space-separated sensitive flags: `-p secret`, `--password secret`
/// 5. `--defaults-file` / `--defaults-extra-file` MySQL config file flags
/// 6. `-P` (port) is NOT redacted — it is not a credential
pub(crate) fn scrub_db_args(args: &str) -> String {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut i = 0;

    while i < tokens.len() {
        let tok = tokens[i];
        match classify_token(tok) {
            TokenAction::RedactUri => {
                out.push("[REDACTED_URI]".to_string());
                i += 1;
            }
            TokenAction::RedactEqualsValue { flag } => {
                out.push(format!("{flag}=[REDACTED]"));
                i += 1;
            }
            TokenAction::RedactAttached { prefix } => {
                out.push(format!("{prefix}[REDACTED]"));
                i += 1;
            }
            TokenAction::RedactNext => {
                out.push(tok.to_string());
                i += 1;
                // Redact the following value token if present.
                if i < tokens.len() {
                    out.push("[REDACTED]".to_string());
                    i += 1;
                }
            }
            TokenAction::Preserve => {
                out.push(tok.to_string());
                i += 1;
            }
        }
    }

    out.join(" ")
}

/// Sanitize user input for safe display in error messages.
///
/// Filters to printable ASCII characters to prevent terminal escape
/// injection attacks. Non-printable and non-ASCII bytes are replaced
/// with `?`, and the string is truncated to 64 characters.
pub(crate) fn sanitize_for_display(input: &str) -> String {
    input
        .chars()
        .take(64)
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '?'
            }
        })
        .collect()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // classify_token tests
    // ========================================================================

    #[test]
    fn test_classify_token_uri_with_at_sign() {
        match classify_token("postgresql://admin:hunter2@db.prod:5432/myapp") {
            TokenAction::RedactUri => {}
            other => panic!("expected RedactUri, got {other:?}"),
        }
        match classify_token("mysql://root:password@localhost/db") {
            TokenAction::RedactUri => {}
            other => panic!("expected RedactUri, got {other:?}"),
        }
        match classify_token("postgres://user:pass@host/db") {
            TokenAction::RedactUri => {}
            other => panic!("expected RedactUri, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_token_uri_without_at_sign_preserved() {
        match classify_token("postgresql://localhost/mydb") {
            TokenAction::Preserve => {}
            other => panic!("expected Preserve, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_token_equals_password() {
        match classify_token("--password=S3cret") {
            TokenAction::RedactEqualsValue { flag } => {
                assert_eq!(flag, "--password");
            }
            other => panic!("expected RedactEqualsValue, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_token_equals_defaults_file() {
        match classify_token("--defaults-file=/home/user/.my.cnf") {
            TokenAction::RedactEqualsValue { flag } => {
                assert_eq!(flag, "--defaults-file");
            }
            other => panic!("expected RedactEqualsValue, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_token_attached_password() {
        match classify_token("-pS3cret") {
            TokenAction::RedactAttached { prefix } => {
                assert_eq!(prefix, "-p");
            }
            other => panic!("expected RedactAttached, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_token_standalone_sensitive_flag() {
        match classify_token("-p") {
            TokenAction::RedactNext => {}
            other => panic!("expected RedactNext, got {other:?}"),
        }
        match classify_token("--password") {
            TokenAction::RedactNext => {}
            other => panic!("expected RedactNext, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_token_port_not_redacted() {
        match classify_token("-P") {
            TokenAction::Preserve => {}
            other => panic!("expected Preserve, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_token_non_sensitive_preserved() {
        for tok in &["-e", "SELECT", "1", "--host-name", "localhost"] {
            match classify_token(tok) {
                TokenAction::Preserve => {}
                other => panic!("expected Preserve for {tok:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_classify_token_empty_string() {
        match classify_token("") {
            TokenAction::Preserve => {}
            other => panic!("expected Preserve for empty string, got {other:?}"),
        }
    }

    // ========================================================================
    // scrub_db_args tests
    // ========================================================================

    #[test]
    fn test_scrub_db_args_mysql_short_password() {
        let input = "-u root -p S3cret -e SELECT 1";
        let result = scrub_db_args(input);
        assert!(
            !result.contains("root"),
            "username after -u must be redacted: {result}"
        );
        assert!(
            !result.contains("S3cret"),
            "password after -p must be redacted: {result}"
        );
        assert!(
            result.contains("[REDACTED]"),
            "redaction marker must appear: {result}"
        );
        assert!(result.contains("SELECT"), "SQL must be preserved: {result}");
    }

    #[test]
    fn test_scrub_db_args_psql_equals_form() {
        let input = "--host=myhost --username=admin -c SELECT 1";
        let result = scrub_db_args(input);
        assert!(
            !result.contains("myhost"),
            "--host=value must be redacted: {result}"
        );
        assert!(
            !result.contains("admin"),
            "--username=value must be redacted: {result}"
        );
        assert!(
            result.contains("--host="),
            "flag name --host must be retained: {result}"
        );
        assert!(
            result.contains("--username="),
            "flag name --username must be retained: {result}"
        );
        assert!(
            result.contains("-c"),
            "non-sensitive flag preserved: {result}"
        );
        assert!(result.contains("SELECT"), "SQL must be preserved: {result}");
    }

    #[test]
    fn test_scrub_db_args_no_credentials_unchanged() {
        let input = "-e SELECT 1 FROM users";
        let result = scrub_db_args(input);
        assert_eq!(result, input, "args with no credentials must be unchanged");
    }

    #[test]
    fn test_scrub_db_args_empty_string() {
        assert_eq!(scrub_db_args(""), "");
    }

    #[test]
    fn test_scrub_db_args_dangling_sensitive_flag() {
        let input = "-c SELECT 1 -p";
        let result = scrub_db_args(input);
        assert!(result.contains("-p"), "dangling flag kept: {result}");
        assert!(
            !result.contains("[REDACTED]"),
            "no token to redact: {result}"
        );
    }

    #[test]
    fn test_scrub_db_args_mysql_attached_password() {
        let input = "-pS3cret -e SELECT 1";
        let result = scrub_db_args(input);
        assert!(
            !result.contains("S3cret"),
            "attached password must be redacted: {result}"
        );
        assert!(
            result.contains("-p[REDACTED]"),
            "redacted form must preserve flag name: {result}"
        );
        assert!(result.contains("SELECT"), "SQL must be preserved: {result}");
    }

    #[test]
    fn test_scrub_db_args_attached_user() {
        let input = "-uroot -pS3cret -e SELECT 1";
        let result = scrub_db_args(input);
        assert!(
            !result.contains("root"),
            "attached username must be redacted: {result}"
        );
        assert!(
            !result.contains("S3cret"),
            "attached password must be redacted: {result}"
        );
        assert!(result.contains("SELECT"), "SQL must be preserved: {result}");
    }

    #[test]
    fn test_scrub_db_args_connection_uri_psql() {
        let input = "postgresql://admin:hunter2@db.prod:5432/myapp -c SELECT 1";
        let result = scrub_db_args(input);
        assert!(
            !result.contains("admin"),
            "username in URI must be redacted: {result}"
        );
        assert!(
            !result.contains("hunter2"),
            "password in URI must be redacted: {result}"
        );
        assert!(
            result.contains("[REDACTED_URI]"),
            "URI redaction marker must appear: {result}"
        );
        assert!(result.contains("SELECT"), "SQL must be preserved: {result}");
    }

    #[test]
    fn test_scrub_db_args_connection_uri_mysql() {
        let input = "mysql://root:password@localhost/db -e SHOW TABLES";
        let result = scrub_db_args(input);
        assert!(
            !result.contains("password"),
            "password in URI must be redacted: {result}"
        );
        assert!(
            result.contains("[REDACTED_URI]"),
            "URI redaction marker must appear: {result}"
        );
        assert!(
            result.contains("SHOW TABLES"),
            "SQL must be preserved: {result}"
        );
    }

    #[test]
    fn test_scrub_db_args_defaults_file_equals() {
        let input = "--defaults-file=/home/user/.my.cnf -e SELECT 1";
        let result = scrub_db_args(input);
        assert!(
            !result.contains("/home/user/.my.cnf"),
            "config file path must be redacted: {result}"
        );
        assert!(
            result.contains("--defaults-file=[REDACTED]"),
            "flag name must be preserved: {result}"
        );
    }

    #[test]
    fn test_scrub_db_args_defaults_file_space() {
        let input = "--defaults-file /home/user/.my.cnf -e SELECT 1";
        let result = scrub_db_args(input);
        assert!(
            !result.contains("/home/user/.my.cnf"),
            "config file path in space-sep form must be redacted: {result}"
        );
        assert!(
            result.contains("--defaults-file"),
            "flag name must be preserved: {result}"
        );
    }

    #[test]
    fn test_scrub_db_args_port_not_redacted() {
        let input = "-P 3306 -e SELECT 1";
        let result = scrub_db_args(input);
        assert!(
            result.contains("3306"),
            "port number must NOT be redacted: {result}"
        );
        assert!(
            !result.contains("[REDACTED]"),
            "no redaction should occur for port: {result}"
        );
    }

    // ========================================================================
    // sanitize_for_display tests
    // ========================================================================

    #[test]
    fn test_sanitize_for_display_clean_input() {
        assert_eq!(sanitize_for_display("hello-world"), "hello-world");
    }

    #[test]
    fn test_sanitize_for_display_rejects_non_ascii() {
        let input = "tool\x1b[31mred\x1b[0m";
        let sanitized = sanitize_for_display(input);
        assert!(!sanitized.contains('\x1b'));
    }

    #[test]
    fn test_sanitize_for_display_truncates_at_64() {
        let long_input = "a".repeat(100);
        let sanitized = sanitize_for_display(&long_input);
        assert_eq!(sanitized.len(), 64);
    }
}
