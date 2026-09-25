//! E2E tests for the hook-rewrite transparency marker.
//!
//! The transparency marker is emitted to stderr when:
//! - `SKIM_REWRITTEN_FROM=<cat|head|tail>` is set in the environment, AND
//! - The served view differs from raw file bytes (mode != full, output != raw).
//!
//! It is NOT emitted when:
//! - The env var is absent (explicit `skim file.rs --mode=pseudo` invocation)
//! - The output is byte-identical to raw (e.g. `--mode=full`)
//! - The guardrail fired and raw bytes were served
//! - The file is a `.py` whose pseudo view happens to equal the raw content

use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;
mod common;

fn skim_cmd() -> assert_cmd::Command {
    let mut cmd = common::skim();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_REWRITTEN_FROM");
    cmd
}

// ============================================================================
// Fixtures sized to survive the ADR-001 savings guard
// ============================================================================
//
// Every test below asserts that a lossy-view marker FIRES. A marker exists only
// when the guard actually serves the compressed view, so these fixtures are a
// precondition of the subject under test, not decoration.
//
// Since the 2026-09-24 amendment the guard charges the stderr disclosure it is
// about to print against the compressed side:
//
//     Keep  iff  compressed + marker < raw   (in BOTH bytes and cl100k tokens)
//
// A fixture that saves less than its own marker costs therefore serves raw —
// losslessly and correctly — and no marker fires. The previous fixtures here
// were 73-124 B, smaller than the 76-124 B markers they were meant to trigger.
//
// Marker cost: structure 76 B / 22 t direct, 110 B / 30 t hook-origin;
//              pseudo    90 B / 24 t direct, 124 B / 32 t hook-origin.

/// Decorator- and type-dense TypeScript for `--mode=pseudo`.
///
/// Pseudo keeps bodies and parameter types and strips decorators, declaration
/// type annotations and semicolons — so the saving must come from annotation
/// density, not from body removal. (A body-heavy fixture barely moves pseudo:
/// measured 733 B of bodies yielded only 64 B / 14 t, far under the marker.)
///
/// Measured: raw 827 B / 179 t → pseudo 344 B / 77 t = saving 483 B / 102 t.
///   direct (90 B / 24 t):  margin +393 B / +78 t — 4.4x / 3.3x the marker
///   hook   (124 B / 32 t): margin +359 B / +70 t — 2.9x / 2.2x the marker
const PSEUDO_FIXTURE: &str = r#"@Injectable({ scope: "singleton" })
@Controller("/orders")
export class OrderService {
  @Inject("repository") private readonly repository: Repository<OrderEntity>;
  @Inject("cache") private readonly cache: CacheStore<string, OrderEntity>;
  @Inject("clock") private readonly clock: ClockProvider<Date>;
  @Inject("logger") private readonly logger: StructuredLogger<LogRecord>;
  @Inject("metrics") private readonly metrics: MetricsSink<Counter, Gauge>;
  @Inject("tracer") private readonly tracer: TraceProvider<SpanContext>;
  private readonly pending: Map<string, Array<OrderEntity>> = new Map();
  private readonly failures: Record<string, ReadonlyArray<Error>> = {};

  place(order: OrderEntity, region: string): number {
    this.repository.save(order);
    this.cache.set(order.id, order);
    return order.total;
  }
}
"#;

/// Body-heavy TypeScript for `--mode=structure`, which replaces each method
/// body with `{...}`.
///
/// Measured: raw 718 B / 193 t → structure 235 B / 58 t = saving 483 B / 135 t.
///   direct (76 B / 22 t):  margin +407 B / +113 t — 5.4x / 5.1x the marker
///   hook   (110 B / 30 t): margin +373 B / +105 t — 3.4x / 3.5x the marker
const STRUCTURE_FIXTURE: &str = r#"export class InvoiceTotals {
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

// ============================================================================
// Marker fires when origin tag is present and view differs
// ============================================================================

/// Tagged pseudo read of a TypeScript file: pseudo mode strips decorators and
/// declaration type annotations, so the view differs from raw bytes → marker
/// must appear.
#[test]
fn test_transparency_marker_fires_on_tagged_pseudo_read() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.ts");
    // Parameter types are PRESERVED (E1/ADR-008); the saving comes from the
    // decorators and declaration annotations. Sized to clear the 124 B / 32 t
    // hook-origin marker with a +359 B / +70 t margin.
    fs::write(&file, PSEUDO_FIXTURE).unwrap();

    skim_cmd()
        .env("SKIM_REWRITTEN_FROM", "cat")
        .arg(&file)
        .arg("--mode=pseudo")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim] transformed view"))
        .stderr(predicate::str::contains("cat"))
        .stderr(predicate::str::contains("pseudo"))
        .stderr(predicate::str::contains("SKIM_PASSTHROUGH=1"));
}

// ============================================================================
// Marker fires without origin tag when view differs (B3 behavior)
// ============================================================================

/// After B3: `skim file.ts --mode=pseudo` without the env tag DOES emit a
/// lossy-view marker when the pseudo view differs from raw bytes.
///
/// ADR-011 class 1: loss-bearing markers are unconditional — they do not
/// require `SKIM_REWRITTEN_FROM` to be set.
///
/// Previously: "must NOT emit a marker" (pre-B3 behavior).
/// Now: marker fires whenever view_differs (B3 generalization).
#[test]
fn test_lossy_marker_fires_without_origin_tag_b3() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.ts");
    // TypeScript pseudo mode strips decorators and non-parameter type annotations;
    // parameter types are preserved (E1/ADR-008). A plain function only loses its
    // semicolons, and a single-decorator class saves 20 B / 6 t — under the 90 B /
    // 24 t direct marker, so the guard would serve raw and no marker would fire.
    // This fixture clears it with a +393 B / +78 t margin.
    fs::write(&file, PSEUDO_FIXTURE).unwrap();

    skim_cmd()
        .arg(&file)
        .arg("--mode=pseudo")
        .arg("--no-cache")
        .assert()
        .success()
        // B3: marker fires even without SKIM_REWRITTEN_FROM.
        .stderr(predicate::str::contains("[skim]"))
        .stderr(predicate::str::contains("pseudo"));
}

// ============================================================================
// Marker is silent when output equals raw (--mode=full)
// ============================================================================

/// Full mode is byte-identical to raw — no marker even with origin tag.
#[test]
fn test_no_marker_when_output_equals_raw_full_mode() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.rs");
    fs::write(&file, "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n").unwrap();

    skim_cmd()
        .env("SKIM_REWRITTEN_FROM", "cat")
        .arg(&file)
        .arg("--mode=full")
        .assert()
        .success()
        // B3: marker still silent when view_differs=false (output == raw bytes).
        .stderr(predicate::str::is_empty());
}

// ============================================================================
// Marker is silent when guardrail fires (raw bytes served, view_differs=false)
// ============================================================================

/// When the fidelity guardrail fires (transformed output > raw bytes), skim serves
/// raw bytes, making `final_output == contents` and therefore `view_differs = false`.
/// The transparency marker must stay silent even though `SKIM_REWRITTEN_FROM` is set.
///
/// Fixture shape mirrors `cli_guardrail.rs::test_guardrail_triggers_when_output_inflates`:
/// 20 empty-body JS functions where structure mode replaces each `{ }` (3 bytes)
/// with ` {...}` (6 bytes), inflating the output past the raw budget.
#[test]
fn test_no_marker_when_guardrail_fires() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("inflating.ts");

    // 20 functions × ~18 bytes = ~360 bytes raw (well above guardrail activation size).
    // Each empty body `{ }` (3 bytes) → ` {...}` (6 bytes) = +3 bytes per function,
    // so the compressed output exceeds the raw size and the guardrail fires.
    let mut source = String::new();
    for i in 0..20 {
        source.push_str(&format!("function f{i}() {{ }}\n"));
    }
    assert!(
        source.len() >= 256,
        "fixture must be >= 256 bytes for guardrail, got {}",
        source.len()
    );
    fs::write(&file, &source).unwrap();

    let output = skim_cmd()
        .env("SKIM_DEBUG", "1")
        .env("SKIM_REWRITTEN_FROM", "cat")
        .arg(&file)
        .arg("--mode=structure")
        .arg("--no-cache")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);

    // Guardrail must have fired — detectable via SKIM_DEBUG=1.
    assert!(
        stderr.contains("[skim:guardrail]"),
        "expected guardrail notice on stderr (with SKIM_DEBUG=1), got: {stderr}"
    );

    // Transparency marker must be absent — guardrail served raw bytes so view_differs=false.
    assert!(
        !stderr.contains("[skim] transformed view"),
        "transparency marker must be silent when guardrail serves raw bytes, got: {stderr}"
    );
}

// ============================================================================
// Marker names the mode (structure vs pseudo)
// ============================================================================

/// Structure-mode read: the marker names "structure", not "pseudo".
///
/// The assertions are UNCONDITIONAL, deliberately. They were previously wrapped
/// in `if !stderr.is_empty()`, so any change that emptied stderr turned the test
/// into a silent no-op that still reported green — and the ADR-001 savings guard
/// charging its own disclosure does exactly that to an undersized fixture. A
/// test that stops testing without failing is worse than one that fails, so the
/// marker's presence is asserted first and the fixture is sized to guarantee it.
#[test]
fn test_transparency_marker_names_structure_mode() {
    let dir = TempDir::new().unwrap();
    // Structure mode replaces each method body with `{...}`. Sized to clear the
    // 110 B / 30 t hook-origin marker with a +373 B / +105 t margin; the previous
    // 73 B fixture saved 23 B / 6 t and so was served raw.
    let file = dir.path().join("api.ts");
    fs::write(&file, STRUCTURE_FIXTURE).unwrap();

    let stderr_bytes = skim_cmd()
        .env("SKIM_REWRITTEN_FROM", "cat")
        .arg(&file)
        .arg("--mode=structure")
        .arg("--no-cache")
        .output()
        .unwrap()
        .stderr;
    let stderr = String::from_utf8_lossy(&stderr_bytes);

    // B3/B4 format with SKIM_REWRITTEN_FROM=cat:
    //   "[skim] transformed view (cat → skim --mode=structure): structure view:
    //    bodies removed — SKIM_PASSTHROUGH=1 for raw output"
    assert!(
        stderr.contains("[skim] transformed view"),
        "the marker must fire: an empty stderr means the guard served raw, and \
         this assertion is what stops that from passing silently; got: {stderr:?}"
    );
    assert!(
        stderr.contains("structure"),
        "transparency marker must name 'structure' mode; got: {stderr}"
    );
    assert!(
        !stderr.contains("pseudo"),
        "transparency marker must not name 'pseudo' for structure mode; got: {stderr}"
    );
}

// ============================================================================
// head tag with --max-lines
// ============================================================================

/// head tag with `--max-lines`: the test drives `--mode=structure` explicitly via
/// `SKIM_REWRITTEN_FROM=head` and the `--mode=structure` argument — it is
/// independent of what the head rewrite handler actually emits (which has changed
/// over time: pseudo → structure → full).  Structure mode strips the function body,
/// guaranteeing the view differs from raw bytes, so the transparency marker must
/// appear and must name `head`.
#[test]
fn test_transparency_marker_with_head_tag() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("main.ts");
    // Structure mode strips the method bodies. Sized to clear the marker on its
    // own merits (+373 B / +105 t against the 110 B / 30 t hook-origin cost), so
    // the marker is guaranteed whether or not the `--max-lines` bound engages.
    fs::write(&file, STRUCTURE_FIXTURE).unwrap();

    skim_cmd()
        .env("SKIM_REWRITTEN_FROM", "head")
        .arg(&file)
        .arg("--mode=structure")
        .arg("--max-lines=10")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim] transformed view"))
        .stderr(predicate::str::contains("head"))
        .stderr(predicate::str::contains("SKIM_PASSTHROUGH=1"));
}

// ============================================================================
// Multi-file aggregate — exactly one marker line
// ============================================================================

/// Multi-file: when multiple files differ, only ONE aggregate marker is emitted.
/// Uses TypeScript files so pseudo mode definitely changes the output.
#[test]
fn test_multi_file_aggregate_marker_emitted_once() {
    let dir = TempDir::new().unwrap();
    let f1 = dir.path().join("a.ts");
    let f2 = dir.path().join("b.ts");
    // TS pseudo mode strips decorators → view differs from raw. Parameter types preserved (E1).
    fs::write(
        &f1,
        "@service()\nexport class Foo {\n  private x: number;\n  foo(x: number): number { return x * 2; }\n}\n",
    )
    .unwrap();
    fs::write(
        &f2,
        "@service()\nexport class Bar {\n  private x: number;\n  bar(x: number): number { return x + 1; }\n}\n",
    )
    .unwrap();

    let stderr_bytes = skim_cmd()
        .env("SKIM_REWRITTEN_FROM", "cat")
        .arg("--mode=pseudo")
        .arg(&f1)
        .arg(&f2)
        .output()
        .unwrap()
        .stderr;
    let stderr = String::from_utf8_lossy(&stderr_bytes);

    let marker_count = stderr.matches("[skim] transformed view").count();
    assert_eq!(
        marker_count, 1,
        "multi-file transparency marker must appear exactly once; got {marker_count} occurrences in stderr:\n{stderr}"
    );
    // B4 format: "... <class description>: 2/2 files — SKIM_PASSTHROUGH=1 for raw output"
    // (old format was "2/2 files not raw bytes")
    assert!(
        stderr.contains("2/2 files"),
        "multi-file marker must show 2/2 count; got: {stderr}"
    );
}

// ============================================================================
// Cache hit: marker fires on second read (repeat invocation)
// ============================================================================

/// Cache hit path: the marker must also fire when the result comes from the
/// skim cache (not just on fresh reads). Uses a TypeScript class with a decorator
/// so pseudo mode definitely produces a different view (E1: parameter types preserved).
#[test]
fn test_transparency_marker_fires_on_cache_hit() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("cached.ts");
    // TS pseudo mode strips decorators and declaration annotations → view differs
    // from raw. Parameter types preserved (E1). Sized to clear the 124 B / 32 t
    // hook-origin marker with a +359 B / +70 t margin, on both the cache-miss and
    // the cache-hit read.
    fs::write(&file, PSEUDO_FIXTURE).unwrap();

    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    // First run (cache miss): populate the cache.
    skim_cmd()
        .env("SKIM_REWRITTEN_FROM", "cat")
        .env("SKIM_CACHE_DIR", &cache_dir)
        .arg(&file)
        .arg("--mode=pseudo")
        .assert()
        .success();

    // Second run (cache hit): marker must still fire.
    skim_cmd()
        .env("SKIM_REWRITTEN_FROM", "cat")
        .env("SKIM_CACHE_DIR", &cache_dir)
        .arg(&file)
        .arg("--mode=pseudo")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim] transformed view"));
}
