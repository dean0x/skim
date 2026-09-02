//! D1 — containment proof for the two `Reencoded` JSON exits that had none.
//!
//! `Completeness::Reencoded` is a *declaration*: "this envelope carries every
//! byte the tool produced, just in a different encoding".  A declaration with
//! no test behind it is a comment.  `git diff` and `git show` already have
//! containment coverage (`cli_git_diff_json_content.rs`,
//! `cli_git_show_json_content.rs`); the two generic execution-layer exits did
//! not, and this file supplies it.
//!
//! | exit | path | tier |
//! |---|---|---|
//! | 5 | `run_parsed_command_with_exit` RawPassthrough arm | `RawPassthrough` |
//! | 6 | `render_output` → `to_json_envelope` | `Passthrough(String)` |
//!
//! Both build `{"tier":"passthrough","raw":…}`.  The assertion is the same for
//! both and is the strongest form available: `parsed["raw"]` must equal the
//! fixture **byte for byte** — not a prefix, not a ratio.  If a future change
//! truncates, re-wraps, or ANSI-strips on either path, the declaration becomes
//! false and this test says so.
//!
//! # Why the two exits are tested separately
//!
//! They are different code, distinguishable from the outside by one byte: exit
//! 5 appends a trailing newline (`println!` semantics), exit 6 appends nothing
//! (`write_to_stdout`).  That difference is a hard constraint — see
//! `LineTermination` in `cmd/execution.rs` — so the tests pin it too.
//!
//! # Surface under test
//!
//! Rewrite-engine surface only (skim binary invoked as a subcommand).

mod common;

/// Shared fixture: multi-line text with `|`, backticks and a blank line, none
/// of which any parser in either family models — so both handlers degrade to a
/// verbatim passthrough envelope.
const FIXTURE: &str = include_str!("fixtures/cmd/file/tree_basic.txt");

/// Parse `stdout` as JSON or fail with the raw bytes attached.
fn parse_json(stdout: &[u8], what: &str) -> serde_json::Value {
    serde_json::from_slice(stdout).unwrap_or_else(|e| {
        panic!(
            "{what}: stdout must be a valid JSON envelope; parse error: {e}\nstdout: {}",
            String::from_utf8_lossy(stdout)
        )
    })
}

// ============================================================================
// Exit 5 — RawPassthrough arm of run_parsed_command_with_exit
// ============================================================================

/// A passthrough-family tool (`wc`) with `--json` over stdin routes to the
/// buffered sink (`choose_passthrough_sink`: `json_output` forces `Buffered`),
/// whose parser returns `ParseResult::RawPassthrough`.
///
/// That arm builds the envelope from `output.stdout` directly — `to_json_envelope`
/// is unreachable for the payload-less variant — and declares `Reencoded`.  The
/// declaration holds only if `raw` is the fixture verbatim.
#[test]
fn exit5_rawpassthrough_json_contains_stdin_byte_for_byte() {
    let out = common::skim()
        .args(["wc", "--json"])
        .write_stdin(FIXTURE)
        .output()
        .expect("skim wc --json must not fail to spawn");

    assert!(
        out.status.success(),
        "skim wc --json must exit 0; got {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );

    let parsed = parse_json(&out.stdout, "wc --json");
    assert_eq!(
        parsed["tier"], "passthrough",
        "the RawPassthrough arm must label the envelope \"passthrough\"; got: {parsed}"
    );
    assert_eq!(
        parsed["raw"].as_str().expect("raw must be a JSON string"),
        FIXTURE,
        "Completeness::Reencoded containment: the envelope must carry the tool's \
         bytes verbatim — any truncation, re-wrap or strip makes the declaration false"
    );

    // Byte contract (R1): this exit appends exactly one newline.
    assert!(
        out.stdout.ends_with(b"}\n"),
        "exit 5 writes its envelope with LineTermination::Newline; \
         stdout tail: {:?}",
        String::from_utf8_lossy(&out.stdout[out.stdout.len().saturating_sub(8)..])
    );
}

// ============================================================================
// Exit 6 — render_output / to_json_envelope with Passthrough(String)
// ============================================================================

/// `skim eslint --json` over content no eslint parser tier can read degrades to
/// `ParseResult::Passthrough(String)`, which reaches `render_output` and
/// serialises through `to_json_envelope`.
///
/// `ParseResult::completeness()` derives `Reencoded` for that tier; the
/// declaration holds only if `raw` is the fixture verbatim.
#[test]
fn exit6_passthrough_string_json_contains_stdin_byte_for_byte() {
    let out = common::skim()
        .args(["eslint", "--json"])
        .write_stdin(FIXTURE)
        .output()
        .expect("skim eslint --json must not fail to spawn");

    assert!(
        out.status.success(),
        "skim eslint --json must exit 0 on unparseable input; got {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );

    let parsed = parse_json(&out.stdout, "eslint --json");
    assert_eq!(
        parsed["tier"], "passthrough",
        "unparseable input must degrade to the passthrough tier; got: {parsed}"
    );
    assert_eq!(
        parsed["raw"].as_str().expect("raw must be a JSON string"),
        FIXTURE,
        "Completeness::Reencoded containment: the envelope must carry the input \
         bytes verbatim"
    );

    // Byte contract (R1): this exit appends NOTHING.  `render_output` has always
    // written through `write_to_stdout`, and routing it through the disclosure
    // sink must not have changed that.
    assert!(
        out.stdout.ends_with(b"}"),
        "exit 6 writes its envelope with LineTermination::None — no trailing \
         newline may appear; stdout tail: {:?}",
        String::from_utf8_lossy(&out.stdout[out.stdout.len().saturating_sub(8)..])
    );
}
