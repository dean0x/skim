// AC24 compile-fail case: exhaustive match on #[non_exhaustive] ProxyProvider.
//
// An external crate matching on a #[non_exhaustive] enum WITHOUT a wildcard arm
// must fail to compile with E0004 (non-exhaustive patterns).
//
// This file must NOT compile — it is a trybuild negative test.
// Precedent: rskim-contract/tests/compile_fail/a_error_return.rs

use rskim_proxy::detect::ProxyProvider;

fn classify(p: ProxyProvider) -> &'static str {
    // This match is missing the wildcard arm `_ => ...`.
    // Because ProxyProvider is #[non_exhaustive], an external crate MUST include
    // a wildcard arm. Without it, this fails with E0004 (non-exhaustive patterns).
    match p {
        //~^ ERROR non-exhaustive patterns
        ProxyProvider::Anthropic => "anthropic",
        ProxyProvider::OpenAI => "openai",
        ProxyProvider::Unknown => "unknown",
        // Missing: _ => unreachable!()
    }
}

fn main() {
    let _ = classify(ProxyProvider::Anthropic);
}
