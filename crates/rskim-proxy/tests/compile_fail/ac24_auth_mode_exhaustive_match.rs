// AC24 compile-fail case: exhaustive match on #[non_exhaustive] AuthMode.
//
// An external crate matching on a #[non_exhaustive] enum WITHOUT a wildcard arm
// must fail to compile with E0004 (non-exhaustive patterns).
//
// This file must NOT compile — it is a trybuild negative test.

use rskim_proxy::authmode::AuthMode;

fn describe(m: AuthMode) -> &'static str {
    // This match is missing the wildcard arm `_ => ...`.
    // Because AuthMode is #[non_exhaustive], an external crate MUST include
    // a wildcard arm. Without it, this fails with E0004 (non-exhaustive patterns).
    match m {
        //~^ ERROR non-exhaustive patterns
        AuthMode::ApiKey => "api-key",
        AuthMode::Subscription => "subscription",
        AuthMode::Ambiguous => "ambiguous",
        // Missing: _ => unreachable!()
    }
}

fn main() {
    let _ = describe(AuthMode::Ambiguous);
}
