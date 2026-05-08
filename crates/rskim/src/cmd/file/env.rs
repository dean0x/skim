//! env/printenv parser with sensitive value redaction.
//!
//! Parses `env` / `printenv` output (KEY=value pairs) into structured `FileResult`.
//! Sensitive values are automatically redacted to protect secrets.
//!
//! Tiers:
//! - **Tier 1 (Full)**: KEY=value parsing with sensitive redaction
//! - **Tier 3 (Passthrough)**: Empty output or parse failure
//!
//! # Redaction rules
//!
//! Keys are redacted when:
//! - Key (uppercased) ends with `_TOKEN`, `_SECRET`, `_PASSWORD`, `_KEY`,
//!   `_CREDENTIAL`, or `_AUTH`
//! - Key matches the exact sensitive-key set (AWS_ACCESS_KEY_ID, GITHUB_TOKEN, etc.)
//!
//! URL credentials (scheme://user:pass@host) are replaced with `***:***` in values.

use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use crate::output::canonical::FileResult;
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{run_file_tool, FileToolConfig, MAX_DISPLAY_ENTRIES, MAX_INPUT_LINES};

/// CONFIG uses "printenv" as the binary name; `skim env` routes here via dispatch.
const CONFIG: FileToolConfig<'static> = FileToolConfig {
    program: "printenv",
    env_overrides: &[],
    install_hint: "printenv is typically pre-installed on Unix systems",
};

/// Regex to detect and redact URL credentials: scheme://user:pass@host
static RE_URL_CREDS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(://)[^:@/]+:[^@/]+(@)").unwrap());

/// Exact set of well-known sensitive key names (uppercase).
const SENSITIVE_EXACT: &[&str] = &[
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "DATABASE_URL",
    "NPM_TOKEN",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "STRIPE_SECRET_KEY",
    "SENTRY_DSN",
    "SENDGRID_API_KEY",
];

/// Key suffixes (uppercase) that indicate sensitive values.
const SENSITIVE_SUFFIXES: &[&str] = &[
    "_TOKEN",
    "_SECRET",
    "_PASSWORD",
    "_KEY",
    "_CREDENTIAL",
    "_AUTH",
];

/// Run `skim env [args...]` or `skim printenv [args...]`.
pub(crate) fn run(args: &[String], ctx: &crate::cmd::RunContext) -> anyhow::Result<ExitCode> {
    run_file_tool(CONFIG, args, ctx, |_| {}, parse_impl)
}

/// Three-tier parse function for env/printenv output.
fn parse_impl(output: &CommandOutput) -> ParseResult<FileResult> {
    if let Some(result) = try_parse_env(&output.stdout) {
        return ParseResult::Full(result);
    }

    ParseResult::Passthrough(output.stdout.clone())
}

// ============================================================================
// Tier 1: env output parsing with redaction
// ============================================================================

fn try_parse_env(stdout: &str) -> Option<FileResult> {
    if stdout.trim().is_empty() {
        return None;
    }

    let mut entries: Vec<String> = Vec::with_capacity(MAX_DISPLAY_ENTRIES);
    let mut total_count = 0usize;

    for (i, line) in stdout.lines().enumerate() {
        if i >= MAX_INPUT_LINES {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }

        // Split on first `=` only
        let Some(eq_pos) = line.find('=') else {
            continue;
        };
        let key = &line[..eq_pos];
        let value = &line[eq_pos + 1..];

        total_count += 1;
        if entries.len() < MAX_DISPLAY_ENTRIES {
            let displayed_value = if is_sensitive_key(key) {
                "***".to_string()
            } else if value.contains("://") && value.contains('@') {
                redact_url_credentials(value)
            } else {
                value.to_string()
            };
            entries.push(format!("{key}={displayed_value}"));
        }
    }

    if total_count == 0 {
        return None;
    }

    let shown_count = entries.len();
    let footer = if total_count > MAX_DISPLAY_ENTRIES {
        Some(format!(
            "... and {} more",
            total_count - MAX_DISPLAY_ENTRIES
        ))
    } else {
        None
    };

    Some(FileResult::new(
        "env".to_string(),
        total_count,
        shown_count,
        entries,
        footer,
    ))
}

/// Return true if the key should have its value redacted.
fn is_sensitive_key(key: &str) -> bool {
    let upper = key.to_uppercase();

    // Exact match check
    if SENSITIVE_EXACT.contains(&upper.as_str()) {
        return true;
    }

    // Suffix check
    SENSITIVE_SUFFIXES
        .iter()
        .any(|suffix| upper.ends_with(suffix))
}

/// Replace `scheme://user:pass@host` credentials with `***:***`.
fn redact_url_credentials(value: &str) -> String {
    RE_URL_CREDS
        .replace_all(value, "${1}***:***${2}")
        .into_owned()
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn load_fixture(name: &str) -> String {
        let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests/fixtures/cmd/file");
        path.push(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("Failed to load fixture '{name}': {e}"))
    }

    fn make_output(stdout: &str, exit_code: i32) -> CommandOutput {
        CommandOutput {
            stdout: stdout.to_string(),
            stderr: String::new(),
            exit_code: Some(exit_code),
            duration: Duration::ZERO,
        }
    }

    #[test]
    fn test_tier1_env_basic() {
        let input = load_fixture("env_basic.txt");
        let result = try_parse_env(&input);
        assert!(result.is_some(), "Expected Tier 1 parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.total_count, 20, "20 variables in env_basic.txt");
        // Safe keys are preserved
        assert!(
            result.entries.iter().any(|e| e.starts_with("PATH=")),
            "PATH should appear unredacted"
        );
    }

    #[test]
    fn test_redacts_suffix_token() {
        let input = "MY_API_TOKEN=super-secret-value\n";
        let result = try_parse_env(input).unwrap();
        assert_eq!(result.entries[0], "MY_API_TOKEN=***");
    }

    #[test]
    fn test_redacts_suffix_secret() {
        let input = "APP_SECRET=very-secret\n";
        let result = try_parse_env(input).unwrap();
        assert_eq!(result.entries[0], "APP_SECRET=***");
    }

    #[test]
    fn test_redacts_suffix_password() {
        let input = "DB_PASSWORD=hunter2\n";
        let result = try_parse_env(input).unwrap();
        assert_eq!(result.entries[0], "DB_PASSWORD=***");
    }

    #[test]
    fn test_redacts_suffix_key() {
        let input = "SIGNING_KEY=abcdef1234\n";
        let result = try_parse_env(input).unwrap();
        assert_eq!(result.entries[0], "SIGNING_KEY=***");
    }

    #[test]
    fn test_redacts_suffix_credential() {
        let input = "SERVICE_CREDENTIAL=cred-value\n";
        let result = try_parse_env(input).unwrap();
        assert_eq!(result.entries[0], "SERVICE_CREDENTIAL=***");
    }

    #[test]
    fn test_redacts_suffix_auth() {
        let input = "MY_CUSTOM_AUTH=bearer-token\n";
        let result = try_parse_env(input).unwrap();
        assert_eq!(result.entries[0], "MY_CUSTOM_AUTH=***");
    }

    #[test]
    fn test_redacts_exact_keys() {
        for key in SENSITIVE_EXACT {
            let input = format!("{key}=some-value\n");
            let result = try_parse_env(&input).unwrap();
            assert_eq!(
                result.entries[0],
                format!("{key}=***"),
                "Key {key} should be redacted"
            );
        }
    }

    #[test]
    fn test_redacts_url_credentials() {
        let input = load_fixture("env_url_creds.txt");
        let result = try_parse_env(&input).unwrap();

        // DATABASE_URL is an exact key match → fully redacted
        let db_entry = result
            .entries
            .iter()
            .find(|e| e.starts_with("DATABASE_URL="))
            .expect("DATABASE_URL should be present");
        assert_eq!(
            db_entry, "DATABASE_URL=***",
            "DATABASE_URL exact match redacts entirely"
        );

        // REDIS_URL has URL credentials → partial redaction
        let redis_entry = result
            .entries
            .iter()
            .find(|e| e.starts_with("REDIS_URL="))
            .expect("REDIS_URL should be present");
        assert!(
            redis_entry.contains("***:***"),
            "REDIS_URL should have URL creds redacted, got: {redis_entry}"
        );
        assert!(
            !redis_entry.contains("redis_pass123"),
            "Password should not appear in output"
        );

        // SAFE_URL has no credentials — preserved
        let safe_entry = result
            .entries
            .iter()
            .find(|e| e.starts_with("SAFE_URL="))
            .expect("SAFE_URL should be present");
        assert!(
            safe_entry.contains("https://example.com"),
            "Safe URL should be preserved, got: {safe_entry}"
        );
    }

    #[test]
    fn test_preserves_safe_keys() {
        let input = "HOME=/home/dean\nSHELL=/bin/zsh\n";
        let result = try_parse_env(input).unwrap();
        assert!(result.entries.iter().any(|e| e == "HOME=/home/dean"));
        assert!(result.entries.iter().any(|e| e == "SHELL=/bin/zsh"));
    }

    #[test]
    fn test_no_false_positive_partial_match() {
        // MY_TOKEN_COUNT ends with digits after _COUNT, not _TOKEN directly
        // BUT: MY_TOKEN_COUNT ends with _COUNT which is not a sensitive suffix.
        // The key MY_TOKEN_COUNT does NOT end with _TOKEN.
        let input = "MY_TOKEN_COUNT=5\n";
        let result = try_parse_env(input).unwrap();
        assert_eq!(
            result.entries[0], "MY_TOKEN_COUNT=5",
            "MY_TOKEN_COUNT should NOT be redacted (does not end with _TOKEN)"
        );
    }

    #[test]
    fn test_case_insensitive_key_match() {
        // Keys should be matched case-insensitively
        let input = "github_token=ghp_test\n";
        let result = try_parse_env(input).unwrap();
        assert_eq!(
            result.entries[0], "github_token=***",
            "Lowercase github_token should still be redacted"
        );
    }

    #[test]
    fn test_tier3_empty_passthrough() {
        let output = make_output("", 1);
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Empty output should be passthrough, got {}",
            result.tier_name()
        );
    }
}
