//! CLI golden tests for `skim log` — byte-stability regression guard (#427).
//!
//! Three goldens are captured here:
//!
//! **STABLE golden** (`cli_log_golden_stable`): exercises a log with no continuation
//! lines, no Traceback headers, and no chained-exception separators. Output is
//! byte-identical to the original capture; any change is a regression.
//!
//! **COUNTERFIX golden** (`cli_log_golden_counterfix`): exercises continuation +
//! traceback + chained-exception separators with significant deduplication (5×
//! repeated ERROR entries and 2× DEBUG entries). The input has enough redundancy
//! to pass the #317 net-savings guard (~22% byte savings), so skim log emits the
//! compressed annotated form. Header reports "19 lines → 6 unique (11 duplicates
//! removed)"; P1.1 invariant X − Z − D = Y holds: 19 − 11 − 2 = 6. Any change
//! is a regression.
//!
//! **PASSTHROUGH golden** (`cli_log_golden_passthrough`): exercises the #317
//! net-savings guard path. The original `stack_trace_python_chained.txt` fixture
//! does not have enough redundancy to qualify for compression; skim log must emit
//! it byte-identical to the input (no header, no annotations).
//!
//! ## Capture procedure
//!
//! ```sh
//! # From repo root — use the debug binary (goldens are stdout text):
//! cargo build -p rskim
//! ./target/debug/skim log < crates/rskim/tests/fixtures/cmd/log/plaintext_mixed.txt
//! ./target/debug/skim log < crates/rskim/tests/fixtures/cmd/log/stack_trace_python_chained_dedup.txt
//! ```
//!
//! Update the corresponding `GOLDEN_*` constant with the captured output.

use assert_cmd::Command;

fn skim_cmd() -> Command {
    let mut cmd = Command::cargo_bin("skim").unwrap();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd
}

// ============================================================================
// STABLE golden — plaintext_mixed.txt
//
// Input: standard timestamped log with duplicates and debug lines.
// No traceback headers, no continuation lines, no chained-exception separators.
// This output must stay byte-identical through ALL passes of #427.
// ============================================================================

const STABLE_INPUT: &str = include_str!("fixtures/cmd/log/plaintext_mixed.txt");

/// Captured from `./target/debug/skim log < plaintext_mixed.txt` on 2026-07-11.
/// Pinned against: rskim-compress::log at wave/l3-wave2 HEAD c6585948.
const GOLDEN_STABLE: &str = "12 lines \u{2192} 7 unique (3 duplicates removed)\n\
2 debug lines hidden (skim log --debug-only)\n \
INFO: server starting on port 8080\n \
INFO: database connected to localhost:5432\n \
INFO: request received GET /api/users (\u{00d7}2)\n \
WARN: slow query detected (1200ms)\n \
ERROR: connection refused: redis:6379 (\u{00d7}3)\n \
INFO: request completed 200 OK\n \
INFO: request completed 500 Internal Server Error";

/// STABLE golden: must be byte-identical through all #427 passes.
///
/// Discriminating: if the log engine's dedup/format output changes for
/// standard timestamped logs (no tracebacks, no chained exceptions),
/// this test fails immediately — no silently-broken output can sneak through.
#[test]
fn cli_log_golden_stable() {
    let output = skim_cmd()
        .arg("log")
        .write_stdin(STABLE_INPUT)
        .output()
        .expect("skim log must run successfully");

    assert!(
        output.status.success(),
        "skim log must exit 0. stderr: {:?}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout must be UTF-8");
    let stdout_trimmed = stdout.trim_end_matches('\n');

    assert_eq!(
        stdout_trimmed, GOLDEN_STABLE,
        "STABLE golden mismatch — byte-identical output required through all #427 passes.\n\
         Expected:\n{GOLDEN_STABLE:?}\n\
         Got:\n{stdout_trimmed:?}",
    );
}

// ============================================================================
// COUNTERFIX golden — stack_trace_python_chained_dedup.txt
//
// Input: log with chained-exception separator, two Traceback blocks,
// continuation stack-frame lines, 5× repeated ERROR entries, and 2× repeated
// DEBUG entries. The input has enough byte/token savings to pass the #317
// net-savings guard (~22%), so skim log emits the compressed annotated output.
//
// Header: "19 lines → 6 unique (11 duplicates removed)"
// P1.1 invariant: X − Z − D = Y → 19 − 11 − 2 = 6 ✓
//
// Any change to this golden is a regression.
// ============================================================================

const COUNTERFIX_INPUT: &str =
    include_str!("fixtures/cmd/log/stack_trace_python_chained_dedup.txt");

/// Captured from `./target/debug/skim log < stack_trace_python_chained_dedup.txt`
/// on 2026-07-16. Pinned against: rskim-compress::log at wave/l3-wave2 HEAD
/// (post-P1.1 fix, #427).
///
/// The fixture has 5× "ERROR: retrying connection" + 2× DEBUG entries; these
/// create enough savings (~22% byte reduction) to pass the #317 net-savings guard,
/// so skim log emits the annotated compressed form (not passthrough). The P1.1
/// invariant holds: X − Z − D = Y: 19 − 11 − 2 = 6.
const GOLDEN_COUNTERFIX: &str = "19 lines \u{2192} 6 unique (11 duplicates removed)\n\
2 debug lines hidden (skim log --debug-only)\n \
ERROR: Operation failed\n\
Traceback (most recent call last):\n\
File \"/app/db.py\", line 45, in query\n\
cursor.execute(sql)\n\
File \"/app/db.py\", line 102, in execute\n\
return self._run(stmt)\n \
DatabaseError: connection timeout\n \
The above exception was the direct cause of the following exception:\n\
Traceback (most recent call last):\n\
File \"/app/api.py\", line 30, in handle\n\
db.query(q)\n\
File \"/app/api.py\", line 55, in respond\n\
return handle(req)\n\
File \"/app/main.py\", line 10, in run\n\
respond(request)\n \
ServiceError: failed to process request\n \
ERROR: retrying connection to db.internal:5432 (\u{00d7}5)\n \
INFO: recovered";

/// COUNTERFIX golden: compressed annotated output from the dedup fixture.
///
/// Discriminating: asserts byte-equality to the golden constant AND verifies the
/// P1.1 arithmetic invariant X − Z − D = Y at runtime by parsing actual stdout
/// values (independent of the constant). Expected: 19 − 11 − 2 = 6.
/// Any change to this golden is a regression.
#[test]
fn cli_log_golden_counterfix() {
    let output = skim_cmd()
        .arg("log")
        .write_stdin(COUNTERFIX_INPUT)
        .output()
        .expect("skim log must run successfully");

    assert!(
        output.status.success(),
        "skim log must exit 0. stderr: {:?}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout must be UTF-8");
    let stdout_trimmed = stdout.trim_end_matches('\n');

    assert_eq!(
        stdout_trimmed, GOLDEN_COUNTERFIX,
        "COUNTERFIX golden mismatch — investigate the log engine regression.\n\
         Expected:\n{GOLDEN_COUNTERFIX:?}\n\
         Got:\n{stdout_trimmed:?}",
    );

    // ── Runtime P1.1 arithmetic invariant: X − Z − D = Y ──────────────────
    // Parse live stdout (not the constant) to independently verify the header
    // arithmetic. Catches engine regressions that produce a "passing" golden
    // by emitting a self-consistent but wrong header.
    let lines: Vec<&str> = stdout_trimmed.lines().collect();

    // Header line: "19 lines → 6 unique (11 duplicates removed)"
    // '\u{2192}' is → (U+2192 RIGHTWARDS ARROW); split_once operates on char
    // boundaries so the multi-byte sequence is handled correctly by Rust's str.
    let header = lines.first().expect("stdout must have at least one line");
    let (x_str, after_arrow) = header
        .split_once(" lines \u{2192} ")
        .expect("header must contain ' lines → '");
    let (y_str, after_unique) = after_arrow
        .split_once(" unique (")
        .expect("header must contain ' unique ('");
    let (z_str, _) = after_unique
        .split_once(" duplicates removed)")
        .expect("header must contain ' duplicates removed)'");
    let x: i64 = x_str
        .trim()
        .parse()
        .expect("x (total lines) must be an integer");
    let y: i64 = y_str
        .trim()
        .parse()
        .expect("y (unique lines) must be an integer");
    let z: i64 = z_str
        .trim()
        .parse()
        .expect("z (duplicates removed) must be an integer");

    // Annotation line: "2 debug lines hidden (skim log --debug-only)"
    let debug_line = lines
        .iter()
        .find(|l| l.contains("debug lines hidden"))
        .expect("output must contain a 'debug lines hidden' annotation line");
    let (d_str, _) = debug_line
        .trim()
        .split_once(" debug lines hidden")
        .expect("debug annotation must begin with an integer");
    let d: i64 = d_str
        .trim()
        .parse()
        .expect("d (debug lines hidden) must be an integer");

    // Guard against i64 underflow before the subtraction.
    assert!(
        x >= z + d,
        "P1.1 precondition: x ({x}) must be >= z ({z}) + d ({d})"
    );
    assert_eq!(
        x - z - d,
        y,
        "P1.1 arithmetic invariant X − Z − D = Y failed: \
         {x} − {z} − {d} = {} ≠ {y}",
        x - z - d,
    );
}

// ============================================================================
// PASSTHROUGH golden — stack_trace_python_chained.txt
//
// Input: the original Python chained-exception fixture (no repeated entries).
// The #317 net-savings guard fires because the fixture does not have enough
// redundancy; skim log must emit it byte-identical to the raw input. No header
// or annotations may appear.
//
// Any change is a regression — the savings guard was bypassed, or the engine
// is incorrectly compressing this fixture.
// ============================================================================

const PASSTHROUGH_INPUT: &[u8] = include_bytes!("fixtures/cmd/log/stack_trace_python_chained.txt");

/// PASSTHROUGH golden: skim log must emit the original fixture byte-identical.
///
/// Discriminating: asserts byte-exact identity between stdout and input. This
/// guards the #317 net-savings guard — the original fixture does not have
/// enough redundancy to qualify for compression, so the engine must pass
/// through raw bytes unchanged (no header, no dedup annotations).
///
/// PF-007: compare content, not just exit code.
#[test]
fn cli_log_golden_passthrough() {
    let output = skim_cmd()
        .arg("log")
        .write_stdin(PASSTHROUGH_INPUT)
        .output()
        .expect("skim log must run successfully");

    assert!(
        output.status.success(),
        "skim log must exit 0 even in passthrough mode. stderr: {:?}",
        String::from_utf8_lossy(&output.stderr),
    );

    assert_eq!(
        output.stdout,
        PASSTHROUGH_INPUT,
        "passthrough output must be byte-identical to input — the #317 net-savings \
         guard must have fired; any header or annotation is a regression.\n\
         Expected ({} bytes): {:?}\n\
         Got ({} bytes): {:?}",
        PASSTHROUGH_INPUT.len(),
        String::from_utf8_lossy(PASSTHROUGH_INPUT),
        output.stdout.len(),
        String::from_utf8_lossy(&output.stdout),
    );
}
