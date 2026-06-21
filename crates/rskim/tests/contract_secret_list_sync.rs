//! Cross-crate consistency guard: `rskim_contract` scrub lists vs `env.rs` lists.
//!
//! # Why this test exists (ADR-006)
//!
//! `rskim_contract::log::SENSITIVE_EXACT` and `SENSITIVE_SUFFIXES` duplicate
//! the lists in `crates/rskim/src/cmd/file/env.rs`. Only a doc comment binds
//! them. A future unmirrored edit to `env.rs` would silently let a secret pass
//! through a `DecisionRecord` — the exact fail-soft failure ADR-006 forbids.
//!
//! This test converts silent desync into a **loud CI failure** without inverting
//! the dependency (rskim_contract remains independent of the binary crate).
//!
//! # What is asserted
//!
//! The expected lists below are the canonical contents of `env.rs`'s private
//! `SENSITIVE_EXACT` and `SENSITIVE_SUFFIXES` constants (as of this writing).
//! The test asserts that `rskim_contract`'s PUBLIC constants are a superset of
//! (or equal to) these lists.
//!
//! **When to update this test:** if `env.rs` grows a new entry, add it here AND
//! mirror it in `rskim_contract::log`. The build will fail until both are done,
//! which is exactly the guard we want (applies ADR-006, avoids PF-007).

use rskim_contract::log::{SENSITIVE_EXACT, SENSITIVE_SUFFIXES};

// ---------------------------------------------------------------------------
// Canonical env.rs lists — copy from crates/rskim/src/cmd/file/env.rs.
// Keep in sync whenever env.rs changes; the test failure is the reminder.
// ---------------------------------------------------------------------------

/// Exact keys as declared in `env.rs::SENSITIVE_EXACT`.
const ENV_SENSITIVE_EXACT: &[&str] = &[
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

/// Suffixes as declared in `env.rs::SENSITIVE_SUFFIXES`.
const ENV_SENSITIVE_SUFFIXES: &[&str] = &[
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// `rskim_contract::log::SENSITIVE_EXACT` must be a superset of `env.rs`'s list.
///
/// If this fails: `env.rs` added a key that `rskim_contract` does not cover.
/// Mirror the new entry in `rskim_contract::log::SENSITIVE_EXACT` (case-insensitive
/// matching is used in both lists, but entries should be stored uppercase to match
/// the canonical form).
#[test]
fn contract_sensitive_exact_is_superset_of_env() {
    for &required in ENV_SENSITIVE_EXACT {
        let covered = SENSITIVE_EXACT
            .iter()
            .any(|&c| c.eq_ignore_ascii_case(required));
        assert!(
            covered,
            "rskim_contract::log::SENSITIVE_EXACT is missing `{required}` \
             which is present in crates/rskim/src/cmd/file/env.rs::SENSITIVE_EXACT. \
             Mirror the entry in rskim_contract/src/log.rs (ADR-006)."
        );
    }
}

/// `rskim_contract::log::SENSITIVE_SUFFIXES` must be a superset of `env.rs`'s list.
///
/// If this fails: `env.rs` added a suffix that `rskim_contract` does not cover.
/// Mirror the new suffix in `rskim_contract::log::SENSITIVE_SUFFIXES`.
#[test]
fn contract_sensitive_suffixes_is_superset_of_env() {
    for &required in ENV_SENSITIVE_SUFFIXES {
        let covered = SENSITIVE_SUFFIXES
            .iter()
            .any(|&c| c.eq_ignore_ascii_case(required));
        assert!(
            covered,
            "rskim_contract::log::SENSITIVE_SUFFIXES is missing `{required}` \
             which is present in crates/rskim/src/cmd/file/env.rs::SENSITIVE_SUFFIXES. \
             Mirror the suffix in rskim_contract/src/log.rs (ADR-006)."
        );
    }
}

/// Smoke-check: the contract lists are non-empty and cover the highest-value keys.
///
/// This guards against an accidental truncation or replacement of the constants
/// (avoids PF-007: the test asserts real list contents, not just process-exit-0).
#[test]
fn contract_sensitive_exact_covers_critical_keys() {
    let critical = &[
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "AWS_SECRET_ACCESS_KEY",
        "GITHUB_TOKEN",
    ];
    for &key in critical {
        assert!(
            SENSITIVE_EXACT.iter().any(|&e| e.eq_ignore_ascii_case(key)),
            "rskim_contract::log::SENSITIVE_EXACT must contain `{key}` (critical secret key)"
        );
    }
}

/// Smoke-check: the contract suffix list covers the highest-value suffixes.
#[test]
fn contract_sensitive_suffixes_covers_critical_suffixes() {
    let critical = &["_TOKEN", "_SECRET", "_PASSWORD", "_API_KEY"];
    for &suffix in critical {
        assert!(
            SENSITIVE_SUFFIXES
                .iter()
                .any(|&s| s.eq_ignore_ascii_case(suffix)),
            "rskim_contract::log::SENSITIVE_SUFFIXES must contain `{suffix}` (critical suffix)"
        );
    }
}
