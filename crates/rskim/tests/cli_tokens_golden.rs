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
///
/// Updated for B3 (ADR-011): the lossy-view marker now fires unconditionally
/// when view differs from raw bytes (structure mode strips bodies → differs).
/// The marker appears on the line after the token stats line.
/// `--no-cache` ensures a cold-path read so view_differs is always computed
/// from the actual transform (not the cache-path inference).
const GOLDEN_STATS_LINE: &str = "[skim] 65 tokens \u{2192} 45 tokens (30.8% reduction)\n[skim] structure view: bodies removed \u{2014} SKIM_PASSTHROUGH=1 for raw output";

#[test]
fn ac13_show_stats_exact_golden() {
    let fixture = golden_fixture();
    assert!(fixture.exists(), "Golden fixture must exist: {:?}", fixture);

    let output = common::skim()
        .arg(fixture.to_str().unwrap())
        .arg("--show-stats")
        .arg("--no-cache")
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
