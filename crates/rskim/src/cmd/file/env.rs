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
//! - Key ends with `_TOKEN`, `_SECRET`, `_PASSWORD`, `_API_KEY`, `_SECRET_KEY`,
//!   `_PRIVATE_KEY`, `_ENCRYPTION_KEY`, `_SIGNING_KEY`, `_ACCESS_KEY`, `_HMAC_KEY`,
//!   `_CREDENTIAL`, or `_AUTH` (case-insensitive)
//! - Key matches the exact sensitive-key set (AWS_ACCESS_KEY_ID, GITHUB_TOKEN, etc.)
//!
//! URL credentials (scheme://user:pass@host) are replaced with `***:***` in values.

use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use crate::output::ParseResult;
use crate::output::canonical::FileResult;
use crate::runner::CommandOutput;

use super::MAX_INPUT_LINES;
use crate::analytics::CommandType;
use crate::cmd::{ToolRunConfig, run_tool};

/// CONFIG uses "printenv" as the binary name; `skim env` routes here via dispatch.
const CONFIG: ToolRunConfig<'static> = ToolRunConfig {
    program: "printenv",
    env_overrides: &[],
    install_hint: "printenv is typically pre-installed on Unix systems",
    family: "file",
    skip_ansi_strip: false,
    command_type: CommandType::FileOps,
    expected_exit_codes: &[],
    forward_stderr: true,
    // SECURITY: Redaction is a security control, not a compression tier.  The
    // net-savings guard normally rejects compressed output that is no shorter
    // than the raw tool output and falls back to the verbatim raw value.  For
    // credential-bearing keys (GITHUB_TOKEN, NPM_TOKEN, …) whose secret is
    // only a few characters long, replacing the secret with `***` does NOT
    // shrink the line — the guard would therefore emit the raw secret verbatim,
    // defeating the entire purpose of redaction.  Setting this to `true` ensures
    // the redacted view always wins, regardless of byte arithmetic. (#317)
    skip_net_savings_guard: true,
    synthesize_success_line: None,
    injected_format_flag: None,
    raw_override: None,
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

/// Key suffixes that indicate sensitive values (stored uppercase, matched case-insensitively).
///
/// `_KEY` was removed to avoid false positives on non-secret identifiers such as
/// `REGISTRY_KEY`, `PRIMARY_KEY`, and `SORT_KEY`. Specific compound suffixes
/// (`_API_KEY`, `_SECRET_KEY`, `_PRIVATE_KEY`, `_ENCRYPTION_KEY`, `_SIGNING_KEY`,
/// `_ACCESS_KEY`, `_HMAC_KEY`) are used instead to cover real crypto/signing secrets.
const SENSITIVE_SUFFIXES: &[&str] = &[
    "_TOKEN",
    "_SECRET",
    "_PASSWORD",
    "_API_KEY",
    "_SECRET_KEY",
    "_PRIVATE_KEY",
    "_ENCRYPTION_KEY",
    "_SIGNING_KEY",
    "_ACCESS_KEY",
    "_HMAC_KEY",
    "_CREDENTIAL",
    "_AUTH",
];

/// Run `skim env [args...]` or `skim printenv [args...]`.
pub(crate) fn run(args: &[String], ctx: &crate::cmd::RunContext) -> anyhow::Result<ExitCode> {
    run_tool(CONFIG, args, ctx, |_| {}, parse_impl)
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

    // Sibling-standard bound (#317, mod.rs:56-59): on very large env dumps return
    // None so the caller degrades to lossless Passthrough.  A mid-loop `break`
    // would leave `total_count` understated and cause the elision marker to
    // misreport the loss — early-return is the safe choice here (per diff.rs
    // precedent).
    if stdout.lines().count() > MAX_INPUT_LINES {
        return None;
    }

    // No display cap: every env variable is a unique, non-redundant datum.
    // A 100-entry cap would silently drop variables whose keys sort late
    // alphabetically (e.g. TOKEN_*, STRIPE_*) — precisely the high-value secrets
    // redaction exists to protect.  MAX_INPUT_LINES already bounds pathological
    // inputs; an additional per-entry display cap buys nothing here and actively
    // harms completeness.
    let mut entries: Vec<String> = Vec::new();
    let mut total_count = 0usize;

    for line in stdout.lines() {
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
        let displayed_value = if is_sensitive_key(key) {
            "***".to_string()
        } else if value.contains("://") && value.contains('@') {
            redact_url_credentials(value)
        } else {
            value.to_string()
        };
        entries.push(format!("{key}={displayed_value}"));
    }

    if total_count == 0 {
        return None;
    }

    let shown_count = entries.len();
    // elision_marker returns None when shown >= total, so no footer when all
    // variables are shown (the common case now that the display cap is removed).
    let footer = crate::output::elision_marker(shown_count, total_count, "variables");

    Some(FileResult::new(
        "env".to_string(),
        total_count,
        shown_count,
        entries,
        footer,
    ))
}

/// Return true if the key should have its value redacted.
///
/// Uses `eq_ignore_ascii_case` throughout to avoid `to_uppercase()` heap allocations
/// on every env variable (issue 3d).
fn is_sensitive_key(key: &str) -> bool {
    // Exact match check — zero allocation
    if SENSITIVE_EXACT
        .iter()
        .any(|exact| key.eq_ignore_ascii_case(exact))
    {
        return true;
    }

    // Suffix check — zero allocation: compare trailing slice case-insensitively.
    // Guard with `is_char_boundary` before byte-slicing to avoid a panic when
    // `key.len() - suffix.len()` falls mid-UTF-8 codepoint (non-ASCII env keys).
    SENSITIVE_SUFFIXES.iter().any(|suffix| {
        let offset = key.len().wrapping_sub(suffix.len());
        key.len() >= suffix.len()
            && key.is_char_boundary(offset)
            && key[offset..].eq_ignore_ascii_case(suffix)
    })
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
    use crate::cmd::test_utils::{load_fixture, make_output_full};

    #[test]
    fn test_tier1_env_basic() {
        let input = load_fixture("file", "env_basic.txt");
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
        // _KEY alone is no longer a sensitive suffix; _API_KEY and _SECRET_KEY are.
        let input = "SIGNING_API_KEY=abcdef1234\n";
        let result = try_parse_env(input).unwrap();
        assert_eq!(result.entries[0], "SIGNING_API_KEY=***");
    }

    #[test]
    fn test_redacts_secret_key_suffix() {
        let input = "MY_SECRET_KEY=abcdef1234\n";
        let result = try_parse_env(input).unwrap();
        assert_eq!(result.entries[0], "MY_SECRET_KEY=***");
    }

    #[test]
    fn test_redacts_private_key_suffix() {
        let input = "RSA_PRIVATE_KEY=-----BEGIN RSA PRIVATE KEY-----\n";
        let result = try_parse_env(input).unwrap();
        assert_eq!(result.entries[0], "RSA_PRIVATE_KEY=***");
    }

    #[test]
    fn test_no_false_positive_registry_key() {
        // REGISTRY_KEY ends with _KEY but NOT with _API_KEY, _SECRET_KEY, or _PRIVATE_KEY.
        // It must NOT be redacted.
        let input = "REGISTRY_KEY=HKLM\\\\Software\\\\Example\n";
        let result = try_parse_env(input).unwrap();
        assert!(
            result.entries[0].starts_with("REGISTRY_KEY=HKLM"),
            "REGISTRY_KEY should NOT be redacted, got: {}",
            result.entries[0]
        );
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
        let input = load_fixture("file", "env_url_creds.txt");
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
    fn test_case_insensitive_suffix_match() {
        // Verifies the suffix path (not the exact-match path) works case-insensitively.
        // "my_app_password" is not in SENSITIVE_EXACT, so it must be caught by the
        // suffix "_PASSWORD" even though the key is entirely lowercase.
        let input = "my_app_password=hunter2\n";
        let result = try_parse_env(input).unwrap();
        assert_eq!(
            result.entries[0], "my_app_password=***",
            "Lowercase suffix _password should be redacted via case-insensitive suffix match"
        );
    }

    #[test]
    fn test_no_panic_on_non_ascii_key() {
        // Non-ASCII env key: byte length of the key may not align with suffix byte
        // boundaries, which previously caused a panic in byte-slice indexing.
        // "clé_TOKEN" — "clé" contains a 2-byte UTF-8 sequence (é = U+00E9).
        let input = "cl\u{00e9}_TOKEN=secret\n";
        let result = try_parse_env(input).unwrap();
        assert_eq!(
            result.entries[0], "cl\u{00e9}_TOKEN=***",
            "Non-ASCII key ending with _TOKEN should be redacted without panic"
        );
    }

    #[test]
    fn test_redacts_new_compound_key_suffixes() {
        // Verify the four compound suffixes added for crypto/signing secrets.
        // These are PREFIXED keys ending with the compound suffix, e.g. APP_ENCRYPTION_KEY.
        // Bare names like ENCRYPTION_KEY (no prefix) would need to be in SENSITIVE_EXACT.
        let cases = [
            ("APP_ENCRYPTION_KEY=enc-val", "APP_ENCRYPTION_KEY=***"),
            ("SERVICE_SIGNING_KEY=sign-val", "SERVICE_SIGNING_KEY=***"),
            ("AWS_ACCESS_KEY=access-val", "AWS_ACCESS_KEY=***"),
            ("APP_HMAC_KEY=hmac-val", "APP_HMAC_KEY=***"),
        ];
        for (input_line, expected) in &cases {
            let result = try_parse_env(&format!("{input_line}\n")).unwrap();
            assert_eq!(
                result.entries[0], *expected,
                "Expected {expected}, got {}",
                result.entries[0]
            );
        }
    }

    // =========================================================================
    // Regression tests for fam-env-security fixes
    // =========================================================================

    // ISSUE 1 (SECURITY): Short-secret redaction must not be subject to byte
    // arithmetic.  Before the fix, `skip_net_savings_guard: false` caused the
    // net-savings guard to fall back to verbatim raw output when the redacted
    // form was no shorter — leaking secrets whose ciphertext is shorter than
    // "***".  These tests exercise the `try_parse_env` layer directly; the
    // guard itself lives in execution.rs and is not reachable from a unit test.
    // The unit tests confirm that `try_parse_env` always redacts regardless of
    // secret length.  An e2e test in crates/rskim/tests/ would be the honest
    // place to verify the guard is actually bypassed end-to-end — these unit
    // tests are the closest observable layer available without a running binary.
    #[test]
    fn test_redacts_short_github_token() {
        // 2-char secret: redacted form "***" is LONGER than "ab" in bytes.
        // Before the fix the guard rejected the compressed view and leaked "ab".
        let input = "GITHUB_TOKEN=ab\n";
        let result = try_parse_env(input).unwrap();
        assert_eq!(
            result.entries[0], "GITHUB_TOKEN=***",
            "Short GITHUB_TOKEN must be redacted regardless of byte length"
        );
    }

    #[test]
    fn test_redacts_short_npm_token() {
        // 2-char secret: same guard-bypass scenario as GITHUB_TOKEN=ab.
        let input = "NPM_TOKEN=xy\n";
        let result = try_parse_env(input).unwrap();
        assert_eq!(
            result.entries[0], "NPM_TOKEN=***",
            "Short NPM_TOKEN must be redacted regardless of byte length"
        );
    }

    #[test]
    fn test_redacts_medium_github_token() {
        // 8-char secret: previously verified to leak verbatim (issue evidence).
        let input = "GITHUB_TOKEN=abcd1234\n";
        let result = try_parse_env(input).unwrap();
        assert_eq!(
            result.entries[0], "GITHUB_TOKEN=***",
            "8-char GITHUB_TOKEN must be redacted"
        );
    }

    // ISSUE 2: MAX_INPUT_LINES must cause `try_parse_env` to return None (lossless
    // Passthrough) rather than break mid-loop and silently truncate.
    #[test]
    fn test_max_input_lines_returns_none() {
        // Build a string with MAX_INPUT_LINES + 1 valid KEY=VALUE pairs.
        let mut input = String::new();
        for i in 0..=MAX_INPUT_LINES {
            input.push_str(&format!("VAR_{i}=val_{i}\n"));
        }
        let result = try_parse_env(&input);
        assert!(
            result.is_none(),
            "try_parse_env must return None when input exceeds MAX_INPUT_LINES \
             so the caller can degrade to lossless Passthrough (mod.rs:56-59)"
        );
    }

    // ISSUE 3: All variables must be shown when count <= MAX_INPUT_LINES.
    // Before the fix, entries past MAX_DISPLAY_ENTRIES (100) were silently dropped.
    #[test]
    fn test_no_silent_variable_drop_above_display_cap() {
        // Build an input with 159 variables (previously measured to drop 59 entries).
        let count = 159usize;
        let mut input = String::new();
        for i in 0..count {
            input.push_str(&format!("MY_VAR_{i:03}=value_{i}\n"));
        }
        let result = try_parse_env(&input).unwrap();
        assert_eq!(
            result.total_count, count,
            "total_count must reflect all {count} variables"
        );
        assert_eq!(
            result.entries.len(),
            count,
            "All {count} variables must appear in entries; none should be silently dropped"
        );
        // No elision footer when all variables are shown.
        assert!(
            result.footer.is_none(),
            "elision footer must be absent when all variables are shown"
        );
    }

    #[test]
    fn test_tier3_empty_passthrough() {
        let output = make_output_full("", "", Some(1));
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Empty output should be passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_produces_full() {
        let input = load_fixture("file", "env_basic.txt");
        let output = make_output_full(&input, "", Some(0));
        let result = parse_impl(&output);
        assert!(
            result.is_full(),
            "parse_impl with exit code 0 and valid env output should return Full, got {}",
            result.tier_name()
        );
    }
}
