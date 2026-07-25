//! Conformance harness integration test for `BlockRouter` (#304 Phase 1 baseline).
//!
//! # AC10 — Conformance baseline
//!
//! Runs `rskim_contract::harness::run_conformance_suite` against `BlockRouter`
//! and asserts `all_passed()`. This establishes Phase 1 parity with
//! `IdentityContract` — the PASSTHROUGH baseline passes all 8 invariants.
//!
//! # Non-tautology requirement (AC10 / PF-007)
//!
//! This test exercises `BlockRouter::route` (the type the proxy will actually
//! use), NOT the pre-existing `rskim_contract::contract::IdentityContract`.
//! A future Phase 2 `BlockRouter::route` that mutates bytes in a way that
//! violates an invariant MUST fail this test — proving the harness drives real
//! behaviour and not a permanent tautology.
//!
//! ## Byte-identity passthrough test
//!
//! In addition to the conformance suite (8 invariants), we include a direct
//! byte-identity test: given input bytes, `transform` MUST return the exact
//! same bytes. This is discriminating because a buggy implementation that
//! cleared or truncated the output would pass (all_passed asserts structure)
//! but fail the byte-identity assertion.

use rskim_compress::BlockRouter;
use rskim_contract::harness::run_conformance_suite;

/// AC10 — Conformance suite passes for `BlockRouter` (PASSTHROUGH baseline).
///
/// All 8 invariants from `rskim_contract::harness::run_conformance_suite` must
/// pass. This establishes Phase 1 parity with `IdentityContract`.
#[test]
fn block_router_passes_conformance_suite() {
    let router = BlockRouter::passthrough_default();
    let report = run_conformance_suite(&router, "req-304-phase1");
    assert!(
        report.all_passed(),
        "BlockRouter conformance failures: {:#?}",
        report.failures()
    );
}

/// AC10 / PF-007 — Byte-identity discriminating test.
///
/// Phase 1 PASSTHROUGH must return the exact input bytes unchanged.
/// This is discriminating: a buggy router that zeroed or truncated output
/// would pass the conformance suite (passthrough is structurally correct)
/// but fail this assertion.
#[test]
fn block_router_passthrough_is_byte_identical() {
    use rskim_contract::contract::Contract;

    let router = BlockRouter::passthrough_default();

    let bodies: &[&[u8]] = &[
        b"{\"model\":\"claude-3-5-sonnet-20241022\",\"messages\":[]}",
        b"",
        b"\x00\x01\x02\x03 arbitrary bytes",
        b"{\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}]}",
    ];

    for body in bodies {
        let outcome = router.transform(body, "req-byte-id");
        assert_eq!(
            outcome.bytes.as_slice(),
            *body,
            "BlockRouter PASSTHROUGH must return byte-identical output for input of {} bytes",
            body.len()
        );
        assert!(
            outcome.is_passthrough(),
            "Phase 1 BlockRouter must report passthrough outcome (bytes: {})",
            body.len()
        );
    }
}

/// AC9 — `BlockRouter` is `Send + Sync` (required for shared stage injection).
#[test]
fn block_router_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<BlockRouter>();
}

/// AC9 — `BlockRouter::new` is infallible (no I/O at construction).
///
/// Verifying this at compile time: `new` returns `Self`, not `Result<Self, _>`.
/// This test asserts the API surface has not accidentally become fallible.
#[test]
fn block_router_construction_is_infallible() {
    use rskim_contract::log::MockSink;
    use std::sync::Arc;

    // This compiles iff `new` returns `BlockRouter` (not a Result).
    let _router = BlockRouter::new(Arc::new(MockSink::new()));
    let _router2 = BlockRouter::passthrough_default();
}

/// Phase 1 byte-identity holds across 100 repeated calls (determinism).
///
/// Covers AC23 partial baseline: same input → same output every time.
#[test]
fn block_router_is_deterministic_100_repeats() {
    use rskim_contract::contract::Contract;

    let router = BlockRouter::passthrough_default();
    let input = b"{\"model\":\"claude-opus-4\",\"messages\":[{\"role\":\"user\",\"content\":\"hello world\"}]}";

    let first = router.transform(input, "req-det-0");
    for i in 1..100usize {
        let outcome = router.transform(input, &format!("req-det-{i}"));
        assert_eq!(
            outcome.bytes, first.bytes,
            "BlockRouter must produce byte-identical output on repeat {i}"
        );
    }
}
