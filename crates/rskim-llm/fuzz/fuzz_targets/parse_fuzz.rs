//! LibFuzzer fuzz target for rskim-llm parse+serialize+mutate pipeline (AC15).
//!
//! # Coverage goals
//!
//! - **Parse robustness:** `parse(arbitrary_bytes)` must never panic, abort, or
//!   hang — it must either return `Ok` or `Err` with a diagnostic.
//! - **Round-trip invariant:** When parse returns `Ok`, `serialize(parse(bytes))`
//!   must succeed and the result must be valid UTF-8 JSON.
//! - **Mutation robustness:** `list_blocks` and `mutate_block` on a parsed body
//!   must never panic.
//! - **No hang:** libFuzzer is invoked with `-timeout=5` in CI to enforce the
//!   5s per-input timeout (AC15 definition). No explicit loop guard needed here.
//!
//! # Running
//!
//! ```sh
//! # Install cargo-fuzz (nightly toolchain required for fuzzing):
//! cargo install cargo-fuzz
//! rustup toolchain install nightly
//!
//! # 60-second smoke run (matches CI gate):
//! cd crates/rskim-llm
//! cargo +nightly fuzz run parse_fuzz -- -max_total_time=60 -timeout=5
//!
//! # Run with the committed seed corpus:
//! cargo +nightly fuzz run parse_fuzz fuzz/corpus/parse_fuzz -- -max_total_time=60 -timeout=5
//! ```
//!
//! The fuzz binary itself only uses stable-compatible constructs in the target code;
//! the nightly requirement is libFuzzer's sanitizer integration, not the target logic.

#![no_main]

use libfuzzer_sys::fuzz_target;
use rskim_llm::{list_blocks, mutate_block, parse, serialize};

fuzz_target!(|data: &[u8]| {
    // --- Step 1: parse ---
    // Must never panic.  May return Err (expected for most random inputs).
    let Ok(mut body) = parse(data) else {
        return;
    };

    // --- Step 2: serialize (unmutated round-trip) ---
    // Must succeed for any Ok body.
    let serialized = serialize(&body).expect("serialize of Ok body must not fail");

    // Serialized output must be valid UTF-8 JSON (serde_json always emits UTF-8).
    assert!(
        std::str::from_utf8(&serialized).is_ok(),
        "serialize output must be valid UTF-8"
    );

    // Round-trip invariant: serialize(parse(data)) == data byte-for-byte.
    // serialize() returns raw_bytes verbatim, which is set to the original input
    // in parse_as().  avoids PF-007 (vacuous test — asserting only valid-UTF-8
    // would pass even if byte identity were broken).
    assert_eq!(
        serialized, data,
        "serialize(parse(data)) must be byte-identical to the original input"
    );

    // --- Step 3: list_blocks ---
    // Must never panic.
    let blocks = list_blocks(&body);

    // --- Step 4: mutate each mutable block ---
    // Must never panic; may return Err if the block path is not found.
    for block in &blocks {
        if block.mutable {
            let _ = mutate_block(&mut body, &block.id, "fuzz_replacement");
        }
    }
});
