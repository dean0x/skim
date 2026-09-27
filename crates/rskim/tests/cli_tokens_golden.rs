//! AC13: Behavior-freeze test — exact `--show-stats` output for a fixed input.
//!
//! After migrating `tokens.rs` to delegate to `rskim-tokens`, this test verifies
//! that `--show-stats` output is byte-identical to a pinned golden, so a change
//! in tokenisation shows up as a failing assertion rather than as silently
//! different numbers.
//!
//! Pinned against: tiktoken-rs 0.7.0 cl100k_base (workspace version).
//!
//! # Why this test owns its fixture (2026-09-24)
//!
//! The golden was originally captured against the shared
//! `tests/fixtures/typescript/simple.ts`. That fixture saves 71 B / 20 t under
//! structure mode, and since the ADR-001 guard began charging the ADR-011
//! class-1 disclosure it is about to print (structure: 76 B / 22 t), the saving
//! no longer covers its own marker — so skim correctly serves RAW and
//! `--show-stats` reports `65 tokens → 65 tokens (0.0% reduction)` with no
//! marker line.
//!
//! Recomputing the constant to that 0.0% line was rejected: it would keep only
//! the ORIGINAL token count and drop the TRANSFORMED count and the marker from
//! the assertion, narrowing an "exact output" freeze to half its surface
//! precisely where the name promises the whole of it. The shared fixture cannot
//! be enlarged instead — 76 insta snapshots in `rskim-core` consume it.
//!
//! So the test now carries its own fixture, sized to clear the marker with a
//! +407 B / +113 t margin (5.4x / 5.1x the 76 B / 22 t structure cost). The
//! trade-off is explicit: the constant below is a CURRENT-binary capture, not
//! the historical pre-migration one. It still freezes tokeniser behaviour
//! going forward, which is what the test is for.

use std::fs;
use tempfile::TempDir;
mod common;

/// Body-heavy TypeScript: structure mode replaces each method body with `{...}`.
///
/// Measured: raw 718 B / 193 t → structure 235 B / 58 t (saving 483 B / 135 t).
const GOLDEN_FIXTURE: &str = r#"export class InvoiceTotals {
  private readonly rates: Map<string, number> = new Map();

  register(region: string, rate: number): void {
    if (rate < 0) { throw new RangeError(`negative rate for ${region}`); }
    this.rates.set(region, rate);
  }

  totalFor(region: string, subtotal: number): number {
    const rate = this.rates.get(region);
    if (rate === undefined) { throw new Error(`unknown region ${region}`); }
    const tax = subtotal * rate;
    return Math.round((subtotal + tax) * 100) / 100;
  }

  summarise(): string {
    const parts: string[] = [];
    for (const [region, rate] of this.rates) {
      parts.push(`${region}=${(rate * 100).toFixed(2)}%`);
    }
    return parts.join(", ");
  }
}
"#;

/// The exact stderr `--show-stats` must produce for [`GOLDEN_FIXTURE`].
///
/// Line 1 is the token-reduction summary; line 2 is the ADR-011 class-1
/// lossy-view marker, which fires unconditionally when the served view differs
/// from raw bytes. `--no-cache` forces a cold read so `view_differs` is computed
/// from the actual transform rather than inferred on the cache-hit path.
const GOLDEN_STATS_LINE: &str = "[skim] 193 tokens \u{2192} 58 tokens (69.9% reduction)\n[skim] structure view: bodies removed \u{2014} SKIM_PASSTHROUGH=1 for full output";

#[test]
fn ac13_show_stats_exact_golden() {
    let dir = TempDir::new().unwrap();
    let fixture = dir.path().join("simple.ts");
    fs::write(&fixture, GOLDEN_FIXTURE).unwrap();

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
        "AC13: --show-stats output must be byte-identical to the pinned golden.\n\
         Expected: {GOLDEN_STATS_LINE:?}\n\
         Got:      {stderr_trimmed:?}",
    );
}
