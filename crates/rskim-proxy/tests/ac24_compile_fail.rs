//! AC24 (NEGATIVE, type-level): Automated trybuild compile-fail tests.
//!
//! Proves that `#[non_exhaustive]` on [`ProxyProvider`] and [`AuthMode`] forces
//! external crates to include a wildcard arm in match expressions. Without the
//! wildcard arm, the compiler emits E0004 (non-exhaustive patterns).
//!
//! ## Why this matters (AC24 / plan §3.3)
//!
//! Adding a new provider (e.g., `GCP`) or auth mode MUST NOT be a breaking change
//! for downstream crates. `#[non_exhaustive]` achieves this: external crates are
//! forced to have `_ => ...` arms, which handle future variants automatically.
//!
//! ## Scope
//!
//! - [`ProxyProvider`]: `#[non_exhaustive]` enum — missing wildcard arm is E0004.
//! - [`AuthMode`]: `#[non_exhaustive]` enum — missing wildcard arm is E0004.
//!
//! ## Precedent
//!
//! Pattern from `rskim-contract/tests/ac2_compile_fail.rs`.

/// AC24 case 1: exhaustive match on `ProxyProvider` without wildcard arm must fail.
///
/// `ProxyProvider` is `#[non_exhaustive]`. An external crate that matches on it
/// without a `_ => ...` arm gets E0004 (non-exhaustive patterns).
#[test]
fn ac24_proxy_provider_exhaustive_match_is_compile_error() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/ac24_proxy_provider_exhaustive_match.rs");
}

/// AC24 case 2: exhaustive match on `AuthMode` without wildcard arm must fail.
///
/// `AuthMode` is `#[non_exhaustive]`. An external crate that matches on it
/// without a `_ => ...` arm gets E0004 (non-exhaustive patterns).
#[test]
fn ac24_auth_mode_exhaustive_match_is_compile_error() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/ac24_auth_mode_exhaustive_match.rs");
}
