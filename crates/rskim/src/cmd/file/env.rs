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
    // SECURITY (PF-012): `never_passthrough = true` prevents SKIM_PASSTHROUGH=1
    // from bypassing the `parse_impl` redaction pass.  Credential redaction is a
    // non-negotiable safety property — it must hold on ALL paths, including the
    // SKIM_PASSTHROUGH=1 escape hatch.  Without this flag, execution.rs would
    // stream raw `printenv` output directly to stdout, leaking secrets.
    never_passthrough: true,
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
    let requested = requested_names(args);
    run_tool(
        CONFIG,
        args,
        ctx,
        |_| {},
        move |output| parse_impl(output, &requested),
    )
}

/// The variable names that the caller asked `printenv` for (every argument
/// that is not a flag).  With names, `printenv` prints bare values, one after
/// the other, with no `KEY=` prefix, so the listing parse cannot see which
/// line belongs to which name.
fn requested_names(args: &[String]) -> Vec<String> {
    args.iter()
        .filter(|arg| !arg.starts_with('-'))
        .cloned()
        .collect()
}

/// Three-tier parse function for env/printenv output.
///
/// Every path that can carry a value goes through a mask step: no input
/// shape (a listing of any size, a value over several lines, NUL-separated
/// records, or bare values for named variables) reaches stdout unmasked.
/// Only an empty output, or a listing with no `KEY=` record and no requested
/// name, passes through.
fn parse_impl(output: &CommandOutput, requested: &[String]) -> ParseResult<FileResult> {
    if !requested.is_empty() {
        return match mask_named_values(&output.stdout, requested) {
            Some(result) => ParseResult::Full(result),
            None => ParseResult::Passthrough(output.stdout.clone()),
        };
    }

    if let Some(result) = try_parse_env(&output.stdout) {
        return ParseResult::Full(result);
    }

    ParseResult::Passthrough(output.stdout.clone())
}

// ============================================================================
// Tier 1: env output parsing with redaction
// ============================================================================

/// Mask the bare values that `printenv NAME...` prints.
///
/// The output has no keys, and a value can span several lines, so a line
/// cannot be mapped to its name.  When any requested name is sensitive,
/// every line is masked (fail closed).  Otherwise each line only gets the
/// URL-credential redaction that the listing applies to values.
fn mask_named_values(stdout: &str, requested: &[String]) -> Option<FileResult> {
    if stdout.trim().is_empty() {
        return None;
    }

    let mask_all = requested.iter().any(|name| is_sensitive_key(name));
    let entries: Vec<String> = stdout
        .split(['\n', '\0'])
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            if mask_all {
                "***".to_string()
            } else {
                redact_value(line)
            }
        })
        .collect();

    let count = entries.len();
    Some(FileResult::new(
        "env".to_string(),
        count,
        count,
        entries,
        None,
    ))
}

/// Parse a `KEY=value` listing and mask the sensitive values.
///
/// Records are NUL-separated when the output holds a NUL byte (`printenv -0`),
/// and newline-separated otherwise.  In the NUL form each record is one
/// variable, so a value with newlines is exact.  In the newline form a line
/// that does not start with a valid variable name and `=` is a continuation
/// of the previous value: it is hidden when that value is masked, and shown
/// as part of it otherwise.
///
/// There is no line cap: every record goes through the mask, whatever the
/// size of the listing.  The listing is already in memory, and the parse is
/// one linear pass over it.
fn try_parse_env(stdout: &str) -> Option<FileResult> {
    if stdout.trim().is_empty() {
        return None;
    }

    if stdout.contains('\0') {
        return parse_nul_records(stdout);
    }

    let mut entries: Vec<String> = Vec::new();
    let mut total_count = 0usize;
    // `None` before the first variable; `Some(masked)` for the current one.
    let mut current_masked: Option<bool> = None;
    // True while the lines after a masked value are continuation lines.
    let mut in_masked_continuation = false;

    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }

        let variable = match line.split_once('=') {
            // Inside a masked value over several lines, a line such as
            // `abc==` (a padding tail) is part of that value, not a variable.
            Some((key, value))
                if is_variable_name(key)
                    && !(in_masked_continuation && value.chars().all(|c| c == '=')) =>
            {
                Some((key, value))
            }
            // Outside a masked value, a sensitive key is always masked, also
            // when it is not a portable name (for example a non-ASCII key).
            Some((key, value)) if current_masked != Some(true) && is_sensitive_key(key) => {
                Some((key, value))
            }
            _ => None,
        };

        match variable {
            Some((key, value)) => {
                total_count += 1;
                let masked = is_sensitive_key(key);
                current_masked = Some(masked);
                in_masked_continuation = false;
                let displayed_value = if masked {
                    "***".to_string()
                } else {
                    redact_value(value)
                };
                entries.push(format!("{key}={displayed_value}"));
            }
            None => match current_masked {
                // Part of a masked value: it stays behind the mask.
                Some(true) => in_masked_continuation = true,
                // Part of a value that is not masked, or text before the
                // first variable: shown, with the URL-credential redaction.
                Some(false) | None => entries.push(redact_value(line)),
            },
        }
    }

    if total_count == 0 {
        return None;
    }

    Some(FileResult::new(
        "env".to_string(),
        total_count,
        total_count,
        entries,
        None,
    ))
}

/// Parse NUL-separated `KEY=value` records (`printenv -0`).
fn parse_nul_records(stdout: &str) -> Option<FileResult> {
    let mut entries: Vec<String> = Vec::new();
    for record in stdout.split('\0') {
        if record.trim().is_empty() {
            continue;
        }
        let entry = match record.split_once('=') {
            Some((key, _)) if is_sensitive_key(key) => format!("{key}=***"),
            Some((key, value)) => format!("{key}={}", redact_value(value)),
            // A record with no `=` has no key to judge it by: fail closed.
            None => "***".to_string(),
        };
        entries.push(entry);
    }

    let count = entries.len();
    if count == 0 {
        return None;
    }
    Some(FileResult::new(
        "env".to_string(),
        count,
        count,
        entries,
        None,
    ))
}

/// True when `key` is a portable variable name (`[A-Za-z_][A-Za-z0-9_]*`).
fn is_variable_name(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The value as shown: URL credentials replaced, everything else unchanged.
fn redact_value(value: &str) -> String {
    if value.contains("://") && value.contains('@') {
        redact_url_credentials(value)
    } else {
        value.to_string()
    }
}

/// Return true if the key should have its value redacted.
///
/// Uses `eq_ignore_ascii_case` throughout to avoid `to_uppercase()` heap allocations
/// on every env variable (issue 3d).
///
/// [`SENSITIVE_EXACT`] / [`SENSITIVE_SUFFIXES`] are the canonical binary-facing
/// secret list (`tests/contract_secret_list_sync.rs` holds them in sync with
/// `rskim_contract::log`), so this predicate is also the single name
/// authority for `cmd::security`'s bare `NAME=VALUE` analytics scrubbing —
/// hence `pub(crate)` rather than private.
pub(crate) fn is_sensitive_key(key: &str) -> bool {
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

    /// Render a parse result as the text that reaches stdout, for the
    /// "no fake value anywhere" assertions below.
    fn rendered(result: &ParseResult<FileResult>) -> String {
        match result {
            ParseResult::Full(r) | ParseResult::Degraded(r, _) => r.as_ref().to_string(),
            ParseResult::Passthrough(raw) => raw.clone(),
            ParseResult::RawPassthrough => panic!("the env handler never returns RawPassthrough"),
        }
    }

    // A listing above the shared input line cap is masked like any other
    // listing: there is no size at which the raw text is served.
    #[test]
    fn test_large_listing_is_masked() {
        let mut input = String::from("FAKE_TOKEN=fake-value-123\n");
        for i in 0..=crate::cmd::file::MAX_INPUT_LINES {
            input.push_str(&format!("VAR_{i}=x\n"));
        }
        let output = make_output_full(&input, "", Some(0));
        let result = parse_impl(&output, &[]);
        assert!(result.is_full(), "got {}", result.tier_name());
        let text = rendered(&result);
        assert!(text.contains("FAKE_TOKEN=***"));
        assert!(!text.contains("fake-value-123"));
        assert!(text.contains("VAR_0=x"), "plain values stay visible");
    }

    // A value over several lines: a continuation line that holds `=` stays
    // behind the mask of its variable.
    #[test]
    fn test_continuation_with_equals_sign_stays_masked() {
        let input = "FAKE_TOKEN=fake-part-one\nfake-part-two=fake-part-three\nPLAIN=ok\n";
        let result = try_parse_env(input).unwrap();
        let text = result.as_ref().to_string();
        assert!(text.contains("FAKE_TOKEN=***"));
        assert!(
            !text.contains("fake-part"),
            "no part of the value shows: {text}"
        );
        assert!(text.contains("PLAIN=ok"));
        assert_eq!(result.total_count, 2);
    }

    // A padding tail such as `abc==` inside a masked value is part of it.
    #[test]
    fn test_padding_tail_of_masked_value_stays_masked() {
        let input = "FAKE_PRIVATE_KEY=-----BEGIN FAKE-----\nZmFrZWZha2VmYWtl\nZmFrZQ==\n-----END FAKE-----\nPLAIN=ok\n";
        let text = try_parse_env(input).unwrap().as_ref().to_string();
        assert!(text.contains("FAKE_PRIVATE_KEY=***"));
        assert!(!text.contains("ZmFrZ"), "no body line shows: {text}");
        assert!(!text.contains("FAKE-----"), "no armour line shows: {text}");
        assert!(text.contains("PLAIN=ok"));
    }

    // A value over several lines that is not sensitive stays visible.
    #[test]
    fn test_continuation_of_plain_value_is_shown() {
        let input = "PLAIN=line-one\nline-two\nOTHER=ok\n";
        let text = try_parse_env(input).unwrap().as_ref().to_string();
        assert!(text.contains("PLAIN=line-one"));
        assert!(text.contains("line-two"));
        assert!(text.contains("OTHER=ok"));
    }

    // NUL-separated records (`printenv -0`): each record is masked by its key,
    // and a record with no `=` is masked whole.
    #[test]
    fn test_nul_records_are_masked() {
        let input = "PLAIN=1\0FAKE_TOKEN=fake-value-123\nfake-line-two\0fake-orphan\0";
        let output = make_output_full(input, "", Some(0));
        let result = parse_impl(&output, &[]);
        assert!(result.is_full(), "got {}", result.tier_name());
        let text = rendered(&result);
        assert!(text.contains("PLAIN=1"));
        assert!(text.contains("FAKE_TOKEN=***"));
        assert!(!text.contains("fake-"), "no fake value shows: {text}");
    }

    // `printenv NAME`: bare values. A sensitive name masks every line.
    #[test]
    fn test_named_sensitive_value_is_masked() {
        let output = make_output_full("fake-value-123\nfake-line-two\n", "", Some(0));
        let requested = vec!["FAKE_TOKEN".to_string(), "PLAIN".to_string()];
        let result = parse_impl(&output, &requested);
        assert!(result.is_full(), "got {}", result.tier_name());
        let text = rendered(&result);
        assert!(!text.contains("fake-"), "no fake value shows: {text}");
        assert!(text.contains("***"));
    }

    // `printenv NAME` for a name that is not sensitive: the value shows, with
    // URL credentials replaced.
    #[test]
    fn test_named_plain_value_is_shown_with_url_redaction() {
        let output = make_output_full("https://fakeuser:fakepass@example.invalid/x\n", "", Some(0));
        let requested = vec!["PLAIN_URL".to_string()];
        let text = rendered(&parse_impl(&output, &requested));
        assert!(text.contains("example.invalid/x"));
        assert!(!text.contains("fakeuser"));
        assert!(!text.contains("fakepass"));
    }

    #[test]
    fn test_requested_names_skip_flags() {
        let args: Vec<String> = ["-0", "FAKE_TOKEN", "--null", "PLAIN"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(requested_names(&args), vec!["FAKE_TOKEN", "PLAIN"]);
    }

    #[test]
    fn test_is_variable_name() {
        assert!(is_variable_name("A_1"));
        assert!(is_variable_name("_X"));
        assert!(!is_variable_name("fake-part"));
        assert!(!is_variable_name("1X"));
        assert!(!is_variable_name(""));
        assert!(!is_variable_name("cl\u{00e9}_TOKEN"));
    }

    // A key that is not a portable name but is sensitive is still masked,
    // after a plain value too.
    #[test]
    fn test_sensitive_key_with_unusual_name_is_masked_after_plain_value() {
        let input = "PLAIN=ok\nfake-app_TOKEN=fake-value-123\n";
        let text = try_parse_env(input).unwrap().as_ref().to_string();
        assert!(text.contains("fake-app_TOKEN=***"));
        assert!(!text.contains("fake-value-123"), "{text}");
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
        let result = parse_impl(&output, &[]);
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
        let result = parse_impl(&output, &[]);
        assert!(
            result.is_full(),
            "parse_impl with exit code 0 and valid env output should return Full, got {}",
            result.tier_name()
        );
    }
}
