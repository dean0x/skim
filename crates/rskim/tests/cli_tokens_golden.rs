//! AC13: Behavior-freeze test — exact `--show-stats` output captured before migration.
//!
//! This golden was captured from the pre-migration binary using:
//!   `skim tests/fixtures/typescript/simple.ts --show-stats 2>&1 >/dev/null`
//!
//! After migrating `tokens.rs` to delegate to `rskim-tokens`, this test verifies
//! that `--show-stats` output is byte-identical to the pre-migration golden.
//!
//! The golden is: "[skim] 65 tokens → 45 tokens (30.8% reduction)"
//!
//! Pinned against: tiktoken-rs 0.7.0 cl100k_base (workspace version).
//! Source file: tests/fixtures/typescript/simple.ts

use assert_cmd::Command;
use std::path::PathBuf;
mod common;

/// Path to the fixture file used to capture the golden.
fn golden_fixture() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop();
    path.pop();
    path.join("tests/fixtures/typescript/simple.ts")
}

/// The exact stderr output captured before migration.
/// Must match byte-for-byte after migration (AC13).
const GOLDEN_STATS_LINE: &str = "[skim] 65 tokens → 45 tokens (30.8% reduction)";

#[test]
fn ac13_show_stats_exact_golden() {
    let fixture = golden_fixture();
    assert!(fixture.exists(), "Golden fixture must exist: {:?}", fixture);

    let output = common::skim()
        .arg(fixture.to_str().unwrap())
        .arg("--show-stats")
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DEBUG")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "skim must exit 0. stderr: {:?}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stderr = String::from_utf8(output.stderr).unwrap();
    let stderr_trimmed = stderr.trim();

    assert_eq!(
        stderr_trimmed, GOLDEN_STATS_LINE,
        "AC13: --show-stats output must be byte-identical to pre-migration golden.\n\
         Expected: {:?}\n\
         Got:      {:?}",
        GOLDEN_STATS_LINE, stderr_trimmed,
    );
}
