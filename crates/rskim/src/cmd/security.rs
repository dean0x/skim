//! Security helpers for the skim CLI.
//!
//! Centralises credential scrubbing and safe-display sanitization so that
//! these concerns are not scattered across the `cmd` subtree.

use std::borrow::Cow;

use crate::cmd::dispatch::redaction_is_mandatory;
use crate::cmd::file::env::is_sensitive_key as is_sensitive_env_key;
use crate::cmd::git::shared::scrub_credential_url;

/// Flags whose *immediately following* space-separated token is a credential.
///
/// **DB-family semantics**: `-h` maps to `--host` for psql/mysql.  This list
/// is intentionally scoped to the DB family; do NOT reuse it for infra tools
/// where `-h` means `--help`, not `--host`.
///
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
/// handled when building the redacted output string.  `classify_db_token` maps a
/// raw token to one of these actions; `scrub_db_args` drives the state machine.
///
/// The lifetime `'a` is tied to the input token slice so that `flag` and
/// `prefix` can borrow directly from the original argument string rather than
/// allocating new `String` values.  `ATTACHED_PREFIXES` entries are
/// `&'static str`, which coerces into `'a` on assignment.
#[derive(Debug)]
enum DbTokenAction<'a> {
    /// Token is a connection-string URI containing embedded credentials.
    /// Replace the entire token with `[REDACTED_URI]`.
    RedactUri,
    /// Token is `--flag=value` where `flag` is sensitive.
    /// Replace with `{flag}=[REDACTED]`; the `flag` field carries the prefix.
    RedactEqualsValue { flag: &'a str },
    /// Token is an attached short flag (`-pSecret`).
    /// Replace with `{prefix}[REDACTED]`; the `prefix` field carries the short flag.
    RedactAttached { prefix: &'a str },
    /// Token is a standalone sensitive flag (`-p`, `--password`, `--defaults-file`, …).
    /// Keep the flag token as-is, then redact the *next* token.
    RedactNext,
    /// Token carries no credential information; emit it verbatim.
    Preserve,
}

/// Classify a single whitespace-split token for credential scrubbing.
///
/// Returns the [`DbTokenAction`] that `scrub_db_args` should apply to this token.
/// All five classification branches from the original while-loop are preserved
/// exactly, now expressed as a pure function without let-chains.
fn classify_db_token<'a>(tok: &'a str) -> DbTokenAction<'a> {
    // 1. Connection string URIs: postgresql://…@…, postgres://…@…, mysql://…@…
    if (tok.starts_with("postgresql://")
        || tok.starts_with("postgres://")
        || tok.starts_with("mysql://"))
        && tok.contains('@')
    {
        return DbTokenAction::RedactUri;
    }

    // 2. `--flag=value` form (sensitive flags and config-file flags)
    if let Some(eq_pos) = tok.find('=') {
        let flag = &tok[..eq_pos];
        if SENSITIVE_FLAGS.contains(&flag) || CONFIG_FILE_FLAGS.contains(&flag) {
            return DbTokenAction::RedactEqualsValue { flag };
        }
    }

    // 3. Attached short flags: -pPassword, -uroot, -Uadmin (no space, single-dash only)
    if !tok.starts_with("--")
        && let Some(&prefix) = ATTACHED_PREFIXES
            .iter()
            .find(|&&p| tok.starts_with(p) && tok.len() > p.len())
    {
        return DbTokenAction::RedactAttached { prefix };
    }

    // 4. Space-separated sensitive flags and config-file flags
    if SENSITIVE_FLAGS.contains(&tok) || CONFIG_FILE_FLAGS.contains(&tok) {
        return DbTokenAction::RedactNext;
    }

    // 5. Non-sensitive token — preserve verbatim
    DbTokenAction::Preserve
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
        match classify_db_token(tok) {
            DbTokenAction::RedactUri => {
                out.push("[REDACTED_URI]".to_string());
                i += 1;
            }
            DbTokenAction::RedactEqualsValue { flag } => {
                out.push(format!("{flag}=[REDACTED]"));
                i += 1;
            }
            DbTokenAction::RedactAttached { prefix } => {
                out.push(format!("{prefix}[REDACTED]"));
                i += 1;
            }
            DbTokenAction::RedactNext => {
                out.push(tok.to_string());
                i += 1;
                // Redact the following value token if present.
                if i < tokens.len() {
                    out.push("[REDACTED]".to_string());
                    i += 1;
                }
            }
            DbTokenAction::Preserve => {
                out.push(tok.to_string());
                i += 1;
            }
        }
    }

    out.join(" ")
}

/// Sensitive flags for infra tools: value (space-separated or equals-joined)
/// must be redacted. `-H` / `--header` are handled separately via
/// [`InfraTokenAction::RedactAuthHeader`] because they require inspecting the
/// *following* token to determine whether it is an auth header.
///
/// `--key` is here for `cypress run --record --key <record-key>`, the only
/// documented way to pass a Cloud record key.  The short `-k` is deliberately
/// absent: `pytest -k <expr>` is a test selector, not a credential.
const INFRA_SENSITIVE_FLAGS: &[&str] = &[
    "--token",
    "--password",
    "--secret",
    "--api-key",
    "--access-key",
    "--private-key",
    "--aws-secret-access-key",
    "--auth-token",
    "--api-token",
    "--registry-token",
    "--cert-password",
    "--passphrase",
    "--key",
];

/// Property-name fragments that mark a `-D<name>=<value>` Java system property
/// or a `-P<name>=<value>` Gradle project property as carrying a credential.
///
/// Matched case-insensitively as a *substring* of the property name, because
/// real names are namespaced: `-Dsonar.token=`, `-Dgpg.passphrase=`,
/// `-DNuGetPassword=`, `-PmavenCentralPassword=`, `-Psigning.password=`.
///
/// A bare `key` fragment is deliberately absent — it would swallow
/// `-Dsonar.projectKey=my-project`, which is not a secret.  The three spelled-out
/// API-key forms cover the credential case instead.
const SENSITIVE_PROPERTY_FRAGMENTS: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "token",
    "secret",
    "credential",
    "_auth",
    "apikey",
    "api-key",
    "api_key",
];

/// Property-name **suffixes**, each anchored on a literal dot, that mark a
/// `-D<name>=<value>` / `-P<name>=<value>` property as carrying a credential.
///
/// These are a DIFFERENT KIND of rule from [`SENSITIVE_PROPERTY_FRAGMENTS`]:
/// those match anywhere in the name, these must match at the END and the byte
/// before the matched word must be the literal `.`.  Keeping the two lists
/// separate is the point — folding `key` or `login` into the substring list
/// would swallow `-Dsonar.projectKey=my-project` (`…projectkey`: the byte before
/// `key` is `t`) and `-Dlogin.url=…` (`login.url` does not END with `.login`),
/// which is exactly the false-positive class the fragment list is written to
/// avoid.  Add a dot-anchored name here; never to the fragment list.
///
/// Real forms: `-Psigning.key=`, `-Dssl.keystore.key=`, `-Dsonar.key=`,
/// `-Dsonar.login=` (a SonarQube token, not a username, in the versions that
/// still accept it).
const SENSITIVE_PROPERTY_NAME_SUFFIXES: &[&str] = &[".key", ".login"];

/// Flags that introduce an arbitrary NESTED COMMAND LINE inside the wrapped
/// tool's own argv: `find . -name '*.env' -exec curl -u admin:pw https://x ;`.
///
/// Everything after one of these belongs to a DIFFERENT program, whose flag
/// vocabulary is unbounded and therefore unscrubbable — no credential-flag list
/// can enumerate what an arbitrary child accepts.  Elision is the only sound
/// answer, so [`InfraTokenAction::ElideRest`] drops the remainder wholesale.
///
/// These four are `find`'s, but the rule is applied by token rather than by
/// program: a family that forgets to opt in cannot silently leak, and the cost
/// of an unexpected match is a shorter label, never a lost secret.
const NESTED_COMMAND_FLAGS: &[&str] = &["-exec", "-execdir", "-ok", "-okdir"];

/// Marker written in place of an elided nested command line.
///
/// Bracketed-uppercase to match this module's existing `[REDACTED]` /
/// `[REDACTED_URI]` label vocabulary.  Deliberately NOT `output::elision_marker`:
/// that renders an ADR-011 class-1 disclosure carrying a `SKIM_PASSTHROUGH=1`
/// remedy, which belongs to a view the reader actually sees.  An analytics label
/// is never rendered to the reader, so borrowing that marker would fabricate a
/// disclosure and put a remedy hint into a SQLite column.
const ELIDED: &str = "[ELIDED]";

/// npm passes registry credentials as config-as-flag arguments, e.g.
/// `--//registry.npmjs.org/:_authToken=TOKEN`.  A flag whose name ends with one
/// of these suffixes carries a credential value.
const NPM_CONFIG_SECRET_SUFFIXES: &[&str] = &["_authtoken", ":_auth", ":_password"];

/// Case-insensitive ASCII substring test, no allocation.
///
/// Compares bytes rather than `str` slices: every needle here is pure ASCII, so
/// the result is identical to the `str` version, but a byte window can never
/// panic on a non-char-boundary index when `haystack` contains multibyte UTF-8.
fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    let needle = needle.as_bytes();
    // Guard: `windows(0)` panics, and an empty needle matches nothing useful.
    !needle.is_empty()
        && haystack
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
}

/// Case-insensitive ASCII suffix test, no allocation (see
/// [`contains_ignore_ascii_case`] for the byte-comparison rationale).
fn ends_with_ignore_ascii_case(haystack: &str, suffix: &str) -> bool {
    let (haystack, suffix) = (haystack.as_bytes(), suffix.as_bytes());
    haystack.len() >= suffix.len()
        && haystack[haystack.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
}

/// `true` when `flag` is a `-D<name>` / `-P<name>` property whose name names a
/// credential.
///
/// Two independent rules, deliberately not merged (see
/// [`SENSITIVE_PROPERTY_NAME_SUFFIXES`] for why):
/// 1. [`SENSITIVE_PROPERTY_FRAGMENTS`] anywhere in the name (`-Dsonar.token=`).
/// 2. [`SENSITIVE_PROPERTY_NAME_SUFFIXES`] at the end of the name, each carrying
///    its own leading dot as the anchor (`-Psigning.key=`, `-Dsonar.login=`).
///
/// Both rules apply only to `-D` / `-P` properties, so they cannot reach the
/// `--private-key` / `--access-key` entries in [`INFRA_SENSITIVE_FLAGS`].
///
/// The `-D` / `-P` prefixes are matched case-**sensitively** on purpose: the
/// lowercase `-p` is cargo's package selector (`cargo test -p rskim-core`) and
/// pytest's plugin flag, and `-D` in `cargo clippy -- -D warnings` carries no
/// `=` so it never reaches this predicate.
fn is_sensitive_property_flag(flag: &str) -> bool {
    let Some(name) = flag.strip_prefix("-D").or_else(|| flag.strip_prefix("-P")) else {
        return false;
    };
    SENSITIVE_PROPERTY_FRAGMENTS
        .iter()
        .any(|fragment| contains_ignore_ascii_case(name, fragment))
        || SENSITIVE_PROPERTY_NAME_SUFFIXES
            .iter()
            .any(|suffix| ends_with_ignore_ascii_case(name, suffix))
}

/// `true` when `flag` is an npm config-as-flag credential
/// (`--//registry.npmjs.org/:_authToken`, `--//host/:_auth`, `--//host/:_password`).
fn is_npm_config_secret_flag(flag: &str) -> bool {
    NPM_CONFIG_SECRET_SUFFIXES
        .iter()
        .any(|suffix| ends_with_ignore_ascii_case(flag, suffix))
}

/// `true` when `name` is the left side of a bare `NAME=VALUE` override — a make
/// command-line variable (`make deploy NPM_TOKEN=xyz`) or an env-style argument
/// to mvn/gradle — whose name is a known secret.
///
/// The secret names are NOT re-declared here: this delegates to
/// [`is_sensitive_env_key`], which is `cmd::file::env`'s `SENSITIVE_EXACT` /
/// `SENSITIVE_SUFFIXES` — the canonical binary-facing secret list that
/// `tests/contract_secret_list_sync.rs` holds in sync with `rskim_contract::log`
/// — one list, never two, so a name can never be scrubbed on one path and leak
/// on the other.  Tokens carrying a leading `-` are excluded: those are
/// flags, handled by the branches above.
fn is_sensitive_bare_assignment(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('-') && is_sensitive_env_key(name)
}

/// Header flags that introduce a value which may be an auth header.
const INFRA_HEADER_FLAGS: &[&str] = &["-H", "--header"];

/// Classification of a single infra argument token for credential scrubbing.
///
/// Mirrors the [`DbTokenAction`] enum used by `scrub_db_args`, applying the same
/// pure-function decomposition to `scrub_infra_args`.  Each variant encodes
/// what `scrub_infra_args` should emit for this token (and possibly the next).
#[derive(Debug)]
enum InfraTokenAction<'a> {
    /// Token is a credential-bearing URL (`https://TOKEN@host/...`).
    /// Replace the entire token with the scheme + host, stripping the auth part.
    RedactCredentialUrl,
    /// Token is `--flag=value` where `flag` is a sensitive infra flag.
    /// Replace with `{flag}=[REDACTED]`; `flag` carries the prefix.
    RedactEqualsValue { flag: &'a str },
    /// Token is a header flag (`-H` / `--header`).
    /// The *next* token is the header value; if it starts with `Authorization:`
    /// or `Proxy-Authorization:`, redact it and all continuation tokens.
    RedactAuthHeader,
    /// Token is a standalone sensitive flag (`--token`, `--password`, …).
    /// Preserve the flag, then redact the *next* token.
    RedactNext,
    /// Token introduces an arbitrary nested command line (`find … -exec`).
    /// Preserve the flag, emit [`ELIDED`], and stop consuming tokens.
    ElideRest,
    /// Token carries no credential information; emit it verbatim.
    Preserve,
}

/// Classify a single whitespace-split infra token for credential scrubbing.
///
/// Returns the [`InfraTokenAction`] that `scrub_infra_args` should apply.
/// This is a pure function with no side effects — all state transitions live
/// in the caller's while-loop, keeping nesting depth at one level.
fn classify_infra_token<'a>(tok: &'a str) -> InfraTokenAction<'a> {
    // 1. Credential-bearing URLs: https://TOKEN@host/..., git://user@host/...
    //    Delegate detection to the shared regex from git/shared.rs so the two
    //    code paths cannot drift.
    if scrub_credential_url(tok).as_ref() != tok {
        return InfraTokenAction::RedactCredentialUrl;
    }

    // 2. `<name>=<value>` form: the name is before `=`.  Five credential
    //    shapes land here, all redacting the value and keeping the name:
    //      * a known sensitive infra flag  — `--token=…`, `--passphrase=…`
    //      * a Java system property        — `-Dpassword=…`, `-Dsonar.token=…`
    //      * a Gradle project property     — `-PmavenCentralPassword=…`
    //      * an npm config-as-flag         — `--//registry/:_authToken=…`
    //      * a bare `NAME=VALUE` override  — `make deploy NPM_TOKEN=…`
    //
    //    Branch 1 runs first, so a name from this list whose value is a
    //    credential URL keeps its scheme and host and loses only the userinfo.
    if let Some(eq_pos) = tok.find('=') {
        let flag = &tok[..eq_pos];
        if INFRA_SENSITIVE_FLAGS.contains(&flag)
            || is_sensitive_property_flag(flag)
            || is_npm_config_secret_flag(flag)
            || is_sensitive_bare_assignment(flag)
        {
            return InfraTokenAction::RedactEqualsValue { flag };
        }
    }

    // 3. Header flags: `-H` / `--header`.
    if INFRA_HEADER_FLAGS.contains(&tok) {
        return InfraTokenAction::RedactAuthHeader;
    }

    // 4. Space-separated sensitive flags: `--token TOKEN`.
    if INFRA_SENSITIVE_FLAGS.contains(&tok) {
        return InfraTokenAction::RedactNext;
    }

    // 5. Nested-command flags: everything after this belongs to another
    //    program and cannot be classified, so it is elided rather than scrubbed.
    if NESTED_COMMAND_FLAGS.contains(&tok) {
        return InfraTokenAction::ElideRest;
    }

    // 6. Non-sensitive token — preserve verbatim.
    InfraTokenAction::Preserve
}

/// Scrub credential values from an infra, build, test, or pkg argument string.
///
/// Infra tools (`curl`, `wget`, `aws`, `gh`, `kubectl`, `terraform`, `docker`)
/// frequently receive sensitive data as flags or credential URLs that would
/// otherwise persist verbatim to the analytics SQLite database.
///
/// The `build`, `test`, and `pkg` families route here too (see
/// `execution::format_analytics_label`), because their tools accept credentials
/// as Java/Gradle properties, npm config-as-flags, bare `NAME=VALUE` overrides,
/// and registry/index URLs with embedded userinfo.  The `db` scrubber must NOT
/// be used for them: its `-p` / `-u` / `-U` / `-h` rules would mangle
/// `cargo test -p <crate>`, `pytest -p <plugin>`, and `make -p`.
///
/// # Flags and patterns redacted
///
/// | Pattern                                     | Tools              |
/// |---------------------------------------------|--------------------|
/// | `https://TOKEN@host/...` credential URLs    | curl, wget, docker |
/// | `-H "Authorization: Bearer TOKEN"`          | curl               |
/// | `-H "Proxy-Authorization: Basic CREDS"`     | curl               |
/// | `--token TOKEN` / `--token=TOKEN`           | gh, kubectl, many  |
/// | `--password TOKEN` / `--password=TOKEN`     | aws, docker        |
/// | `--secret TOKEN` / `--secret=TOKEN`         | terraform, docker  |
/// | `--api-key TOKEN` / `--api-key=TOKEN`       | aws, general       |
/// | `--access-key TOKEN` / `--access-key=TOKEN` | aws                |
/// | `--private-key TOKEN` / `--private-key=TOKEN` | general          |
/// | `--aws-secret-access-key TOKEN/=TOKEN`      | aws                |
/// | `--key TOKEN` / `--key=TOKEN`               | cypress            |
/// | `--auth-token`, `--api-token`, `--registry-token` | many         |
/// | `--cert-password`, `--passphrase`           | many               |
/// | `-D<name>=VALUE` Java system properties     | mvn, gradle        |
/// | `-P<name>=VALUE` Gradle project properties  | gradle             |
/// | `-D…/-P…` names ending `.key` / `.login`    | mvn, gradle, sonar |
/// | `--//registry/:_authToken=VALUE`            | npm, pnpm, yarn    |
/// | bare `NAME=VALUE` secret overrides          | make, mvn, gradle  |
/// | `-exec`/`-execdir`/`-ok`/`-okdir` remainder | find (elided)      |
///
/// Only the flag *values* are redacted; the flag names are preserved in the
/// label so analytics can still identify which flags were used.
///
/// # Design
///
/// Uses [`InfraTokenAction`] + [`classify_infra_token`] decomposition, matching
/// the [`DbTokenAction`] + `classify_db_token` pattern from `scrub_db_args`.  The
/// state machine in this function's while-loop is kept at one nesting level;
/// all classification logic lives in the pure [`classify_infra_token`] function.
///
/// # Known limitations
///
/// * **Tokenisation is whitespace-based.** The caller joins argv and this
///   function re-splits on whitespace, so a credential containing a space
///   (`-Dsonar.password=my pass`) has its first word redacted and its tail
///   (`pass`) left in the label.  Recovering argv boundaries would require
///   changing every call site's signature; that is deliberately out of scope.
/// * **`-H` consumes its following token unconditionally.** On the infra family
///   that token is a header value; on `build`/`test`/`pkg` a literal `-H` means
///   something else, so its argument may be redacted needlessly.  This
///   over-redacts rather than under-redacts, which is the safe direction.
pub(crate) fn scrub_infra_args(args: &str) -> String {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut i = 0;

    while i < tokens.len() {
        let tok = tokens[i];
        match classify_infra_token(tok) {
            InfraTokenAction::RedactCredentialUrl => {
                // Replace only the `<auth>@` portion via scrub_credential_url,
                // preserving the scheme and host for analytics legibility.
                out.push(scrub_credential_url(tok).into_owned());
                i += 1;
            }
            InfraTokenAction::RedactEqualsValue { flag } => {
                out.push(format!("{flag}=[REDACTED]"));
                i += 1;
            }
            InfraTokenAction::RedactAuthHeader => {
                // Emit the header flag itself (`-H` or `--header`), then inspect
                // the following token.  When the shell passes
                // `-H "Authorization: Bearer TOKEN"` the entire value is a single
                // CLI argument; after `args.join(" ")` it splits into multiple
                // tokens: `Authorization:`, `Bearer`, `TOKEN`.  We redact the
                // first token and skip continuation tokens until the next flag
                // or URL-like boundary so the full header value is suppressed.
                out.push(tok.to_string());
                i += 1;
                if i < tokens.len() {
                    let lower = tokens[i].to_lowercase();
                    if lower.starts_with("authorization:")
                        || lower.starts_with("proxy-authorization:")
                    {
                        out.push("[REDACTED]".to_string());
                        i += 1;
                        // Consume continuation tokens (e.g. `Bearer`, `TOKEN`).
                        while i < tokens.len() {
                            let cont = tokens[i];
                            if cont.starts_with('-')
                                || cont.starts_with("http://")
                                || cont.starts_with("https://")
                            {
                                break;
                            }
                            i += 1;
                        }
                    } else {
                        out.push(tokens[i].to_string());
                        i += 1;
                    }
                }
            }
            InfraTokenAction::RedactNext => {
                out.push(tok.to_string());
                i += 1;
                if i < tokens.len() {
                    out.push("[REDACTED]".to_string());
                    i += 1;
                }
            }
            InfraTokenAction::ElideRest => {
                // Emit the flag so the label still says WHY the tail is gone,
                // then stop: no token after it can be classified safely.
                out.push(tok.to_string());
                out.push(ELIDED.to_string());
                break;
            }
            InfraTokenAction::Preserve => {
                out.push(tok.to_string());
                i += 1;
            }
        }
    }

    out.join(" ")
}

/// Redact the value of EVERY `NAME=VALUE` token when `program` is one whose
/// output redaction is mandatory (`env`, `printenv`).
///
/// For those two programs an argv assignment is either a real environment
/// override — whose value is a credential as often as not — or a malformed
/// variable name.  Neither is worth persisting, so the value goes regardless of
/// whether the name appears on the canonical secret list.  Every other program
/// keeps the name-list rule, which is why this is keyed on the program and not
/// folded into [`classify_infra_token`].
///
/// # This is a fourth site, not a duplicate of the other three
///
/// [`redaction_is_mandatory`] is consulted here, by `main.rs` D2b, and by
/// `dispatch_inner` D4; `cmd/file/env.rs`'s `never_passthrough` flag and the
/// convergence gate's literal `subcommand != "env"` are two further, DIFFERENT
/// controls. They are deliberately distinct concepts — an output-path raw-serve
/// gate, a per-handler passthrough refusal, a literal-name gate, and (here) a
/// label-content rule — and they do not have the same membership: the
/// convergence gate names only `env`, while this rule and
/// [`redaction_is_mandatory`] name `printenv` too. Do not merge them into one
/// predicate; the `printenv` spelling leaking its assignment values into the
/// analytics label is exactly what happens when one site's membership is
/// assumed to match another's.
///
/// Returns [`Cow::Borrowed`] when nothing matched (the overwhelmingly common
/// case), so non-env programs pay no allocation.
pub(crate) fn redact_mandatory_assignments<'a>(program: &str, args: &'a str) -> Cow<'a, str> {
    if !redaction_is_mandatory(program) || !args.contains('=') {
        return Cow::Borrowed(args);
    }

    let redacted: Vec<String> = args
        .split_whitespace()
        .map(|tok| match tok.find('=') {
            Some(eq_pos) => format!("{}=[REDACTED]", &tok[..eq_pos]),
            None => tok.to_string(),
        })
        .collect();

    Cow::Owned(redacted.join(" "))
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
    // classify_db_token tests
    // ========================================================================

    #[test]
    fn test_classify_db_token_uri_with_at_sign() {
        match classify_db_token("postgresql://admin:hunter2@db.prod:5432/myapp") {
            DbTokenAction::RedactUri => {}
            other => panic!("expected RedactUri, got {other:?}"),
        }
        match classify_db_token("mysql://root:password@localhost/db") {
            DbTokenAction::RedactUri => {}
            other => panic!("expected RedactUri, got {other:?}"),
        }
        match classify_db_token("postgres://user:pass@host/db") {
            DbTokenAction::RedactUri => {}
            other => panic!("expected RedactUri, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_db_token_uri_without_at_sign_preserved() {
        match classify_db_token("postgresql://localhost/mydb") {
            DbTokenAction::Preserve => {}
            other => panic!("expected Preserve, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_db_token_equals_password() {
        match classify_db_token("--password=S3cret") {
            DbTokenAction::RedactEqualsValue { flag } => {
                assert_eq!(flag, "--password");
            }
            other => panic!("expected RedactEqualsValue, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_db_token_equals_defaults_file() {
        match classify_db_token("--defaults-file=/home/user/.my.cnf") {
            DbTokenAction::RedactEqualsValue { flag } => {
                assert_eq!(flag, "--defaults-file");
            }
            other => panic!("expected RedactEqualsValue, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_db_token_attached_password() {
        match classify_db_token("-pS3cret") {
            DbTokenAction::RedactAttached { prefix } => {
                assert_eq!(prefix, "-p");
            }
            other => panic!("expected RedactAttached, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_db_token_standalone_sensitive_flag() {
        match classify_db_token("-p") {
            DbTokenAction::RedactNext => {}
            other => panic!("expected RedactNext, got {other:?}"),
        }
        match classify_db_token("--password") {
            DbTokenAction::RedactNext => {}
            other => panic!("expected RedactNext, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_db_token_port_not_redacted() {
        match classify_db_token("-P") {
            DbTokenAction::Preserve => {}
            other => panic!("expected Preserve, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_db_token_non_sensitive_preserved() {
        for tok in &["-e", "SELECT", "1", "--host-name", "localhost"] {
            match classify_db_token(tok) {
                DbTokenAction::Preserve => {}
                other => panic!("expected Preserve for {tok:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_classify_db_token_empty_string() {
        match classify_db_token("") {
            DbTokenAction::Preserve => {}
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
    // scrub_infra_args tests
    // ========================================================================

    #[test]
    fn test_scrub_infra_args_token_space_separated() {
        let result = scrub_infra_args("--token mysecrettoken repo list");
        assert!(
            !result.contains("mysecrettoken"),
            "--token value must be redacted: {result}"
        );
        assert!(
            result.contains("--token"),
            "flag name must be preserved: {result}"
        );
        assert!(
            result.contains("[REDACTED]"),
            "redaction marker must appear: {result}"
        );
        assert!(
            result.contains("repo list"),
            "non-sensitive args preserved: {result}"
        );
    }

    #[test]
    fn test_scrub_infra_args_token_equals_form() {
        let result = scrub_infra_args("--token=mysecrettoken repo list");
        assert!(
            !result.contains("mysecrettoken"),
            "--token=value must be redacted: {result}"
        );
        assert!(
            result.contains("--token="),
            "flag name must be preserved: {result}"
        );
        assert!(
            result.contains("[REDACTED]"),
            "redaction marker must appear: {result}"
        );
    }

    #[test]
    fn test_scrub_infra_args_authorization_header() {
        let result =
            scrub_infra_args("-H authorization: Bearer secrettoken https://api.example.com");
        assert!(
            !result.contains("secrettoken"),
            "auth header value must be redacted: {result}"
        );
        assert!(result.contains("-H"), "flag -H must be preserved: {result}");
        assert!(
            result.contains("[REDACTED]"),
            "redaction marker must appear: {result}"
        );
        assert!(
            result.contains("https://api.example.com"),
            "URL must be preserved: {result}"
        );
    }

    #[test]
    fn test_scrub_infra_args_non_auth_header_preserved() {
        let result = scrub_infra_args("-H content-type: application/json https://api.example.com");
        assert!(
            result.contains("content-type: application/json"),
            "non-auth header must NOT be redacted: {result}"
        );
    }

    #[test]
    fn test_scrub_infra_args_password_equals_form() {
        let result = scrub_infra_args("--password=S3cret123 --region us-east-1");
        assert!(
            !result.contains("S3cret123"),
            "--password=value must be redacted: {result}"
        );
        assert!(
            result.contains("us-east-1"),
            "non-sensitive args preserved: {result}"
        );
    }

    #[test]
    fn test_scrub_infra_args_no_sensitive_flags_unchanged() {
        let input = "get pods -n myns --output json";
        let result = scrub_infra_args(input);
        assert_eq!(
            result, input,
            "args with no sensitive flags must be unchanged"
        );
    }

    #[test]
    fn test_scrub_infra_args_empty_string() {
        assert_eq!(scrub_infra_args(""), "");
    }

    // ========================================================================
    // classify_infra_token tests
    // ========================================================================

    #[test]
    fn test_classify_infra_token_credential_url_https() {
        match classify_infra_token("https://ghp_secret@github.com/org/repo.git") {
            InfraTokenAction::RedactCredentialUrl => {}
            other => panic!("expected RedactCredentialUrl, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_infra_token_credential_url_user_password() {
        match classify_infra_token("https://user:hunter2@gitlab.com/org/repo") {
            InfraTokenAction::RedactCredentialUrl => {}
            other => panic!("expected RedactCredentialUrl, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_infra_token_clean_url_preserved() {
        match classify_infra_token("https://api.example.com/v1/resource") {
            InfraTokenAction::Preserve => {}
            other => panic!("expected Preserve for clean URL, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_infra_token_equals_form() {
        match classify_infra_token("--token=mysecret") {
            InfraTokenAction::RedactEqualsValue { flag } => {
                assert_eq!(flag, "--token");
            }
            other => panic!("expected RedactEqualsValue, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_infra_token_header_flag_short() {
        match classify_infra_token("-H") {
            InfraTokenAction::RedactAuthHeader => {}
            other => panic!("expected RedactAuthHeader for -H, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_infra_token_header_flag_long() {
        match classify_infra_token("--header") {
            InfraTokenAction::RedactAuthHeader => {}
            other => panic!("expected RedactAuthHeader for --header, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_infra_token_standalone_sensitive() {
        match classify_infra_token("--token") {
            InfraTokenAction::RedactNext => {}
            other => panic!("expected RedactNext for --token, got {other:?}"),
        }
        match classify_infra_token("--aws-secret-access-key") {
            InfraTokenAction::RedactNext => {}
            other => panic!("expected RedactNext for --aws-secret-access-key, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_infra_token_non_sensitive_preserved() {
        for tok in &["--output", "-n", "myns", "get", "pods"] {
            match classify_infra_token(tok) {
                InfraTokenAction::Preserve => {}
                other => panic!("expected Preserve for {tok:?}, got {other:?}"),
            }
        }
    }

    // ========================================================================
    // scrub_infra_args credential URL tests
    // ========================================================================

    #[test]
    fn test_scrub_infra_args_credential_url_https_token() {
        let result = scrub_infra_args("https://ghp_supersecret@github.com/org/repo.git");
        assert!(
            !result.contains("ghp_supersecret"),
            "token in URL must be scrubbed: {result}"
        );
        assert!(
            result.contains("github.com/org/repo.git"),
            "host+path must be preserved: {result}"
        );
    }

    #[test]
    fn test_scrub_infra_args_credential_url_user_password() {
        let result =
            scrub_infra_args("curl https://user:hunter2@api.example.com/upload --data @file.json");
        assert!(
            !result.contains("hunter2"),
            "password in URL must be scrubbed: {result}"
        );
        assert!(
            !result.contains("user:"),
            "username in URL must be scrubbed: {result}"
        );
        assert!(
            result.contains("api.example.com/upload"),
            "host+path must be preserved: {result}"
        );
        assert!(
            result.contains("--data"),
            "non-sensitive flags must be preserved: {result}"
        );
    }

    #[test]
    fn test_scrub_infra_args_clean_url_not_scrubbed() {
        let input = "curl https://api.example.com/v1/resource -X GET";
        let result = scrub_infra_args(input);
        assert_eq!(result, input, "clean URL must not be modified");
    }

    #[test]
    fn test_scrub_infra_args_credential_url_alongside_token_flag() {
        let result = scrub_infra_args("--token mysecret https://user:pass@api.example.com/v1");
        assert!(
            !result.contains("mysecret"),
            "flag credential must be scrubbed: {result}"
        );
        assert!(
            !result.contains("user:pass"),
            "URL credential must be scrubbed: {result}"
        );
        assert!(
            result.contains("api.example.com/v1"),
            "host+path must be preserved: {result}"
        );
    }

    #[test]
    fn test_scrub_infra_args_dangling_sensitive_flag() {
        // A sensitive flag with no following token must not panic or redact extra tokens.
        let result = scrub_infra_args("kubectl get pods --token");
        assert!(result.contains("--token"), "dangling flag kept: {result}");
        assert!(
            !result.contains("[REDACTED]"),
            "no value to redact: {result}"
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

    // ========================================================================
    // Build / test / pkg credential patterns (security-07)
    //
    // Each pattern has a positive case (secret redacted) and a negative case
    // (benign argument forwarded verbatim).  The negative cases guard the
    // real-usage shapes that a `-p`/`-u`/`-U`/`-h` style rule would mangle.
    // ========================================================================

    // ---- (a) `-D<name>=<value>` Java system properties -------------------

    #[test]
    fn test_scrub_infra_args_java_property_password_redacted() {
        let result = scrub_infra_args("deploy -Dpassword=hunter2");
        assert!(
            !result.contains("hunter2"),
            "-Dpassword value must be redacted: {result}"
        );
        assert!(
            result.contains("-Dpassword=[REDACTED]"),
            "property name must be preserved: {result}"
        );
        assert!(
            result.contains("deploy"),
            "goal must be preserved: {result}"
        );
    }

    #[test]
    fn test_scrub_infra_args_java_property_secret_name_variants_redacted() {
        for input in &[
            "-Dsonar.token=sqp_abcdef",
            "-Dgpg.passphrase=hunter2",
            "-DNuGetPassword=hunter2",
            "-Dmy.apikey=hunter2",
            "-Dmy.api-key=hunter2",
            "-Dmy.api_key=hunter2",
            "-Dsome_auth=hunter2",
            "-Dmy.secret=hunter2",
            "-Dmy.credential=hunter2",
            "-Ddb.passwd=hunter2",
        ] {
            let result = scrub_infra_args(input);
            assert!(
                result.ends_with("=[REDACTED]"),
                "{input} must have its value redacted, got: {result}"
            );
            assert!(
                !result.contains("hunter2") && !result.contains("sqp_abcdef"),
                "{input} leaked its value: {result}"
            );
        }
    }

    #[test]
    fn test_scrub_infra_args_java_property_project_key_preserved() {
        // HAZARD GUARD: a bare `key` substring rule would swallow this.
        let input = "sonar:sonar -Dsonar.projectKey=my-project";
        assert_eq!(
            scrub_infra_args(input),
            input,
            "-Dsonar.projectKey is not a credential and must be preserved"
        );
    }

    // ---- (b) `-P<name>=<value>` Gradle project properties ----------------

    #[test]
    fn test_scrub_infra_args_gradle_property_password_redacted() {
        for input in &[
            "publish -PmavenCentralPassword=hunter2",
            "publish -Psigning.password=hunter2",
        ] {
            let result = scrub_infra_args(input);
            assert!(
                !result.contains("hunter2"),
                "{input} leaked its value: {result}"
            );
            assert!(
                result.contains("=[REDACTED]"),
                "{input} must carry the redaction marker: {result}"
            );
            assert!(
                result.contains("publish"),
                "task name must be preserved: {result}"
            );
        }
    }

    #[test]
    fn test_scrub_infra_args_gradle_benign_property_preserved() {
        let input = "build -PsomeFlag=value";
        assert_eq!(
            scrub_infra_args(input),
            input,
            "a non-credential gradle project property must be preserved"
        );
    }

    // ---- (c) npm config-as-flag ------------------------------------------

    #[test]
    fn test_scrub_infra_args_npm_auth_token_config_flag_redacted() {
        for input in &[
            "install --//registry.npmjs.org/:_authToken=npm_secretvalue",
            "publish --//npm.pkg.github.com/:_authToken=npm_secretvalue",
            "install --//registry.example.com/:_auth=base64creds",
            "install --//registry.example.com/:_password=hunter2",
        ] {
            let result = scrub_infra_args(input);
            assert!(
                !result.contains("npm_secretvalue")
                    && !result.contains("base64creds")
                    && !result.contains("hunter2"),
                "{input} leaked its value: {result}"
            );
            assert!(
                result.contains("=[REDACTED]"),
                "{input} must carry the redaction marker: {result}"
            );
        }
    }

    #[test]
    fn test_scrub_infra_args_npm_benign_config_flag_preserved() {
        let input = "install --//registry.npmjs.org/:always-auth=true";
        assert_eq!(
            scrub_infra_args(input),
            input,
            "a non-secret npm config flag must be preserved"
        );
    }

    #[test]
    fn test_scrub_infra_args_clean_registry_url_preserved() {
        let input = "install --registry=https://registry.npmjs.org";
        assert_eq!(
            scrub_infra_args(input),
            input,
            "a registry URL with no userinfo carries no credential"
        );
    }

    #[test]
    fn test_scrub_infra_args_registry_url_with_userinfo_scrubbed() {
        let result = scrub_infra_args("install --registry=https://user:hunter2@npm.example.com");
        assert!(
            !result.contains("hunter2") && !result.contains("user:"),
            "userinfo in a registry URL must be scrubbed: {result}"
        );
        assert!(
            result.contains("npm.example.com"),
            "host must be preserved: {result}"
        );
    }

    #[test]
    fn test_scrub_infra_args_pip_index_url_credentials_scrubbed() {
        let result =
            scrub_infra_args("install --index-url https://user:t0ken@pypi.example.com/simple pkg");
        assert!(
            !result.contains("t0ken") && !result.contains("user:"),
            "userinfo in an index URL must be scrubbed: {result}"
        );
        assert!(
            result.contains("pypi.example.com/simple"),
            "host+path must be preserved: {result}"
        );
        assert!(result.contains("pkg"), "package name preserved: {result}");
    }

    #[test]
    fn test_scrub_infra_args_git_plus_https_token_scrubbed() {
        let result = scrub_infra_args("install git+https://ghp_secrettoken@github.com/org/repo");
        assert!(
            !result.contains("ghp_secrettoken"),
            "token in a git+https URL must be scrubbed: {result}"
        );
        assert!(
            result.contains("github.com/org/repo"),
            "host+path must be preserved: {result}"
        );
    }

    // ---- (d) bare `NAME=VALUE` overrides ---------------------------------

    #[test]
    fn test_scrub_infra_args_bare_secret_assignment_redacted() {
        for input in &[
            "deploy NPM_TOKEN=npm_secretvalue",
            "deploy MY_PASSWORD=hunter2",
            "deploy AWS_SECRET_ACCESS_KEY=hunter2",
            "deploy SERVICE_CREDENTIAL=hunter2",
            "deploy REGISTRY_AUTH=hunter2",
            "deploy DATABASE_URL=hunter2",
        ] {
            let result = scrub_infra_args(input);
            assert!(
                !result.contains("hunter2") && !result.contains("npm_secretvalue"),
                "{input} leaked its value: {result}"
            );
            assert!(
                result.contains("=[REDACTED]"),
                "{input} must carry the redaction marker: {result}"
            );
            assert!(
                result.starts_with("deploy "),
                "target must be preserved: {result}"
            );
        }
    }

    #[test]
    fn test_scrub_infra_args_bare_benign_assignment_preserved() {
        for input in &[
            "build CARGO_TARGET_DIR=/tmp/target",
            "test RUST_LOG=debug",
            "build PREFIX=/usr/local",
            "build SORT_KEY=name",
        ] {
            assert_eq!(
                &scrub_infra_args(input),
                input,
                "a non-secret make variable must be preserved"
            );
        }
    }

    // ---- (e) `--key` -----------------------------------------------------

    #[test]
    fn test_scrub_infra_args_record_key_redacted() {
        let space = scrub_infra_args("run --record --key recordkey123");
        assert!(
            !space.contains("recordkey123"),
            "--key value must be redacted: {space}"
        );
        assert!(
            space.contains("--key [REDACTED]"),
            "flag name must be preserved: {space}"
        );
        let equals = scrub_infra_args("run --record --key=recordkey123");
        assert!(
            !equals.contains("recordkey123"),
            "--key=value must be redacted: {equals}"
        );
    }

    #[test]
    fn test_scrub_infra_args_pytest_k_selector_preserved() {
        // HAZARD GUARD: `-k` is a test selector, never a credential.
        let input = "-k test_foo";
        assert_eq!(
            scrub_infra_args(input),
            input,
            "pytest -k selector must be preserved"
        );
    }

    // ---- (f) additional credential flags ---------------------------------

    #[test]
    fn test_scrub_infra_args_additional_credential_flags_redacted() {
        for flag in &[
            "--auth-token",
            "--api-token",
            "--registry-token",
            "--cert-password",
            "--passphrase",
        ] {
            let space = scrub_infra_args(&format!("publish {flag} hunter2"));
            assert!(
                !space.contains("hunter2"),
                "{flag} value must be redacted: {space}"
            );
            assert!(
                space.contains(&format!("{flag} [REDACTED]")),
                "{flag} name must be preserved: {space}"
            );
            let equals = scrub_infra_args(&format!("publish {flag}=hunter2"));
            assert!(
                !equals.contains("hunter2"),
                "{flag}=value must be redacted: {equals}"
            );
        }
    }

    #[test]
    fn test_scrub_infra_args_similar_benign_flags_preserved() {
        for input in &[
            "publish --access public",
            "list --api-version 2",
            "run --record",
        ] {
            assert_eq!(
                &scrub_infra_args(input),
                input,
                "a flag that merely resembles a credential flag must be preserved"
            );
        }
    }

    // ---- build / test selector shapes that must survive verbatim ---------

    // ---- (a)/(b) dot-anchored property-name suffixes ---------------------

    #[test]
    fn test_scrub_infra_args_dot_anchored_property_suffix_redacted() {
        for input in &[
            "publish -Psigning.key=hunter2",
            "sonar:sonar -Dsonar.login=sqp_abcdef",
            "verify -Dssl.keystore.key=hunter2",
            "sonar:sonar -Dsonar.key=hunter2",
        ] {
            let result = scrub_infra_args(input);
            assert!(
                !result.contains("hunter2") && !result.contains("sqp_abcdef"),
                "{input} leaked its value: {result}"
            );
            assert!(
                result.contains("=[REDACTED]"),
                "{input} must carry the redaction marker: {result}"
            );
        }
    }

    #[test]
    fn test_scrub_infra_args_dot_anchored_suffix_is_not_a_bare_substring() {
        // HAZARD GUARD for the `.key` / `.login` suffix rules.  Each of these
        // names would be swallowed by a bare `key` or `login` substring rule;
        // the required literal dot is what keeps them out.
        for input in &[
            // `sonar.projectkey` — the byte before `key` is `t`, not `.`.
            "sonar:sonar -Dsonar.projectKey=my-project",
            // `login.url` ends with `.url`; and the value carries no userinfo,
            // so the credential-URL branch must not fire either.
            "-Dlogin.url=https://sonar.example.com",
            // `monkey.count` — proves the rule is anchored, not a substring.
            "-Dmonkey.count=3",
        ] {
            assert_eq!(
                &scrub_infra_args(input),
                input,
                "a dot-anchored rule must not fire on this name"
            );
        }
    }

    #[test]
    fn test_scrub_infra_args_key_flags_keep_a_single_redaction_path() {
        // MEASUREMENT: the `.key` suffix rule lives inside
        // `is_sensitive_property_flag`, which requires a `-D` / `-P` prefix, so
        // the pre-existing `--private-key` / `--access-key` flags never reach it.
        // Even if two predicates matched, `classify_infra_token` returns ONE
        // action and `scrub_infra_args` emits ONE token, so output is unchanged.
        assert_eq!(
            scrub_infra_args("--private-key=hunter2"),
            "--private-key=[REDACTED]"
        );
        assert_eq!(
            scrub_infra_args("--private-key hunter2"),
            "--private-key [REDACTED]"
        );
        assert_eq!(
            scrub_infra_args("--access-key=hunter2"),
            "--access-key=[REDACTED]"
        );
        assert_eq!(
            scrub_infra_args("--access-key hunter2"),
            "--access-key [REDACTED]"
        );
    }

    // ---- nested-command elision (`find -exec <cmd> …`) -------------------

    #[test]
    fn test_scrub_infra_args_nested_command_flag_elides_the_remainder() {
        for flag in &["-exec", "-execdir", "-ok", "-okdir"] {
            let input =
                format!(". -name a.txt {flag} curl -u admin:hunter2 https://api.example.com ;");
            let result = scrub_infra_args(&input);
            assert!(
                !result.contains("hunter2") && !result.contains("admin"),
                "{flag} must not leak the nested command's credentials: {result}"
            );
            assert!(
                result.contains(&format!("{flag} [ELIDED]")),
                "{flag} must be followed by the elision marker: {result}"
            );
            assert!(
                result.starts_with(". -name a.txt "),
                "the predicate prefix must survive: {result}"
            );
            assert!(
                !result.contains("curl"),
                "nothing after the nested-command flag may survive: {result}"
            );
        }
    }

    #[test]
    fn test_scrub_infra_args_find_without_nested_command_is_unchanged() {
        for input in &[
            ". -name *.rs -type f -maxdepth 2",
            ". -name a.txt -print",
            "/var/log -mtime -7 -size +1M",
        ] {
            assert_eq!(
                &scrub_infra_args(input),
                input,
                "a find invocation with no nested command must be forwarded verbatim"
            );
        }
    }

    #[test]
    fn test_scrub_infra_args_flaglike_grep_pattern_is_over_redacted() {
        // ACCEPTED, INTENDED over-redaction (security-07 Layer 2).  The scrubber
        // does not honour `--` as an end-of-options separator, so a grep PATTERN
        // that looks like a credential flag loses its value in the analytics
        // label.  Over-redaction fails safe; a leak does not.  If this test
        // fails, someone taught the scrubber about `--` — that is a behaviour
        // change to review deliberately, not a bug to silence.
        assert_eq!(
            scrub_infra_args("-- --password= logs/"),
            "-- --password=[REDACTED] logs/"
        );
    }

    #[test]
    fn test_scrub_infra_args_build_and_test_selectors_unchanged() {
        for input in &[
            "test -p rskim-core",
            "-p no:cacheprovider",
            "-p",
            "-k test_foo",
            "--resolve-plugins-relative-to .",
            "build --release --locked",
            "install --index-url https://pypi.example.com/simple",
            "-u",
            "-U",
            "-h",
        ] {
            assert_eq!(
                &scrub_infra_args(input),
                input,
                "build/test argument shape must be forwarded verbatim"
            );
        }
    }
}
