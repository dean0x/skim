//! CLI tests for F1/F1b — `--mode=pseudo` preserves TypeScript type-level
//! declarations, observed through the binary rather than through the transform.
//!
//! # Commit ordering
//!
//! This file asserts the behaviour of F1 **and** F1b together, because the two
//! defects co-occur on one fixture and the same run measures both.  It therefore
//! belongs to the F1b commit: with F1 alone the annotations come back but the
//! member separators do not, and the assertions on `id: UserId;` would fail.
//! The F1-only half is pinned in `rskim-core`'s unit tests, which are
//! deliberately written without any `;` so they pass on F1 alone.
//!
//! # Why this file exists when `rskim-core` already has unit tests
//!
//! `rskim-core` has no dependency on `rskim`'s `output/`, so the ADR-001
//! net-savings guard does not exist there.  A green core test proves the
//! transform is right and says nothing about whether any reader ever sees it:
//! the guard is content-sensitive, and for a small enough file it serves the raw
//! source instead of the transformed view.  Under that passthrough stdout *is*
//! the raw file — and the raw file contains every annotation the fix restores,
//! so a naive "after" assertion on stdout passes green against a completely
//! unfixed binary.
//!
//! Every test here therefore asserts the PRECONDITION on stderr before it makes
//! any claim about stdout.
//!
//! # The precondition string, and the two ways to get it wrong
//!
//! The transparency marker's head DIFFERS BY ORIGIN (measured at `c2b4378`):
//!
//! ```text
//! direct   (129 B): [skim] pseudo view: … — SKIM_PASSTHROUGH=1 for full output
//! cat-origin(163 B): [skim] transformed view (cat → skim --mode=pseudo): … — SKIM_PASSTHROUGH=1 for full output
//! ```
//!
//! `[skim] pseudo view:` never appears on the hook-origin path — i.e. never on
//! skim's primary surface — so keying on that literal reads every hook-origin
//! run as masked.  [`MARKER_TAIL`] is the shared tail, byte-identical on both
//! origins and absent whenever the guard served raw.
//!
//! The other trap is the negative half.  `[skim:guardrail]` is an ADR-011
//! class-2 banner written to `io::sink()` unless debug is on
//! (`output/guardrail.rs`), so `assert!(!stderr.contains("[skim:guardrail]"))`
//! passes trivially without `SKIM_DEBUG=1` — a PF-025-genus guard, adopted
//! because it sounds like a precondition and satisfied by the very failure it is
//! meant to exclude.  These tests set `SKIM_DEBUG=1` so the clause has teeth,
//! and still lead with the positive assertion, which is the one that cannot be
//! satisfied by a passthrough.
//!
//! # Fixture sizes are load-bearing
//!
//! Both fixtures are asserted to their exact byte length.  The guard's verdict
//! is decided by `compressed + notice < raw` in BOTH bytes and tokens, so a
//! later edit that trims either fixture can silently move it across the
//! threshold and turn these tests vacuous.  `MIN_RAW_SIZE_FOR_GUARDRAIL` no
//! longer exists, so there is no small-file escape hatch to fall back on.

use assert_cmd::Command;
use tempfile::TempDir;

mod common;

/// The portion of the ADR-008 transparency marker that is byte-identical on the
/// direct and the `cat`-origin paths.  Its presence is the precondition: the
/// guard emitted a marker, therefore it served the transformed view.
const MARKER_TAIL: &str = "SKIM_PASSTHROUGH=1 for full output";

/// 1104 B.  Clears the ADR-001 guard at both marker costs before AND after the
/// fix, and carries all four constructs in one file: the byte-identical union
/// members, an interface `property_signature`, a `readonly` member, an
/// `index_signature`, and — as an in-fixture control — a class field whose
/// annotation and `readonly` must still be stripped.
///
/// The decorators are the only reason it clears: in TypeScript `strip_kinds` is
/// `["type_annotation", "decorator", "readonly", "abstract"]`, and header and
/// in-body comments are NOT stripped, so `decorator` is the sole high-yield
/// entry.  The type-level payload contributes nothing to the savings — which is
/// why restoring it does not re-mask the file.
///
/// The fifth decorator is deliberate BALLAST, and its size is the reason this
/// file is 1104 B rather than 1026 B.  The fix RESTORES bytes, so it spends its
/// own guard margin: at 1026 B the post-fix saving cleared the 163 B hook-origin
/// marker by only 1.72x in bytes and 1.68x in tokens, under the 2x that exists
/// so a later ADR-001 tuning cannot silently re-mask this file.  Measured with
/// the ballast: 2.20x bytes, 2.17x tokens.  A decorator is stripped whole, so
/// its full length lands in the saving and none of it in the served view.
const CANONICAL: &str = include_str!("../../../tests/fixtures/typescript/type_level_members.ts");

/// 235 B.  Too small for the fix to be observable at hook origin: the guard
/// serves the raw source byte-identically and emits no marker at all.
const GUARD_MASKED: &str =
    include_str!("../../../tests/fixtures/typescript/type_level_members_guard_masked.ts");

fn skim_cmd() -> Command {
    let mut cmd = common::skim();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd
}

/// Run `skim <file> --mode=pseudo` on `source`, returning `(stdout, stderr)`.
///
/// `--no-cache` is not optional: the cache key includes `notice_bytes`, so a
/// stale entry can serve a pre-fix verdict.  `SKIM_CACHE_DIR` is redirected as
/// well so nothing survives between tests, and `SKIM_DEBUG=1` is set so the
/// `[skim:guardrail]` clause in the precondition is not vacuous.
fn serve_pseudo(source: &str, hook_origin: bool) -> (String, String) {
    let dir = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let file = dir.path().join("fixture.ts");
    std::fs::write(&file, source).unwrap();

    let mut cmd = skim_cmd();
    cmd.env("SKIM_DEBUG", "1")
        .env("SKIM_CACHE_DIR", cache.path().as_os_str());
    if hook_origin {
        cmd.env("SKIM_REWRITTEN_FROM", "cat");
    }
    let out = cmd
        .arg(file.to_str().unwrap())
        .arg("--mode=pseudo")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "skim --mode=pseudo must exit 0; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    (
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

/// Gate 1.  Fails loudly when the ADR-001 guard served raw, instead of letting
/// the stdout assertions pass against the untransformed source.
fn assert_transformed_view_was_served(stdout: &str, stderr: &str, source: &str, label: &str) {
    assert!(
        stderr.contains(MARKER_TAIL),
        "{label}: PRECONDITION FAILED — no transparency marker on stderr, so the \
         ADR-001 guard served RAW and every assertion below would pass against an \
         unfixed binary.  Re-size the fixture rather than relaxing this.\nstderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("[skim:guardrail]"),
        "{label}: PRECONDITION FAILED — the guardrail banner fired (SKIM_DEBUG=1 is \
         set, so this clause is not vacuous).\nstderr:\n{stderr}"
    );
    assert_ne!(
        stdout, source,
        "{label}: PRECONDITION FAILED — stdout is byte-identical to the source, so \
         no transform reached the reader"
    );
}

/// The two union members must not be the same text.  Stated by comparing the
/// emitted lines: `contains` is structurally unable to say "and these two
/// differ" (PF-025 — assert the property, never a proxy that survives the
/// defect).
fn assert_union_members_distinct(stdout: &str, label: &str) {
    let members: Vec<&str> = stdout
        .lines()
        .filter(|l| l.trim_start().starts_with("| {"))
        .collect();
    assert_eq!(
        members.len(),
        2,
        "{label}: both union members must be emitted\nstdout:\n{stdout}"
    );
    assert_ne!(
        members[0], members[1],
        "{label}: two disjoint union members rendered as IDENTICAL bytes — the \
         discriminant the union exists for is gone and nothing signals it\nstdout:\n{stdout}"
    );
    assert!(
        members[0].contains("\"USD\"") && members[1].contains("\"EUR\""),
        "{label}: the discriminant literal is what distinguishes them\nstdout:\n{stdout}"
    );
}

fn assert_type_level_members_intact(stdout: &str, label: &str) {
    for expected in [
        "id: UserId;",
        "email: string;",
        "readonly createdAt: Date;",
        "[account: string]: number;",
    ] {
        assert!(
            stdout.contains(expected),
            "{label}: type-level declaration `{expected}` must survive \
             --mode=pseudo\nstdout:\n{stdout}"
        );
    }
    assert_union_members_distinct(stdout, label);

    // In-fixture control: the class field is a `public_field_definition` under
    // `class_body`, disjoint from `property_signature`, so BOTH its `readonly`
    // and its annotation must still be stripped.  Same file, same run — if this
    // fails, the exemption is wider than its scope argument.
    assert!(
        stdout.contains("private cache = new Map()"),
        "{label}: a class field keeps visibility, name and initializer\nstdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("readonly cache"),
        "{label}: a class field's `readonly` is outside F1's scope and must still \
         be stripped\nstdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("cache: Map<string, Money>"),
        "{label}: a class field's annotation must still be stripped\nstdout:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// Fixture size guards
// ---------------------------------------------------------------------------

#[test]
fn fixture_sizes_are_load_bearing() {
    assert_eq!(
        CANONICAL.len(),
        1104,
        "type_level_members.ts is sized so that `compressed + notice < raw` holds \
         in both bytes and tokens at the 163 B hook-origin marker cost AFTER the \
         fix restores its annotations, with 2.20x / 2.17x of headroom.  Changing \
         its size can silently re-mask every assertion in this file; trimming the \
         fifth decorator alone drops the margin under 2x."
    );
    assert_eq!(
        GUARD_MASKED.len(),
        235,
        "type_level_members_guard_masked.ts is sized to sit BELOW the hook-origin \
         threshold, which is the whole point of the test that uses it."
    );
}

// ---------------------------------------------------------------------------
// L1 — direct invocation, guard cleared
// ---------------------------------------------------------------------------

#[test]
fn direct_pseudo_view_preserves_type_level_declarations() {
    let (stdout, stderr) = serve_pseudo(CANONICAL, false);
    assert_transformed_view_was_served(&stdout, &stderr, CANONICAL, "L1 direct");
    assert_type_level_members_intact(&stdout, "L1 direct");
}

// ---------------------------------------------------------------------------
// L2 — hook origin, the worst case and skim's primary surface
// ---------------------------------------------------------------------------

#[test]
fn hook_origin_pseudo_view_preserves_type_level_declarations() {
    // The hook-origin marker costs 163 B against the direct path's 129 B, and
    // that 34 B decides the verdict for any fixture near the threshold.  A fix
    // verified only at L1 can be fixed for a human typing `skim` and still
    // masked for every agent, because the agent's `cat` is what the hook
    // rewrites.
    let (stdout, stderr) = serve_pseudo(CANONICAL, true);
    assert_transformed_view_was_served(&stdout, &stderr, CANONICAL, "L2 hook-origin");
    assert_type_level_members_intact(&stdout, "L2 hook-origin");
}

#[test]
fn hook_origin_and_direct_views_are_byte_identical() {
    // The origin tag changes only what is disclosed on stderr.  If it ever
    // changes stdout, one of the two surfaces is being served a different view
    // of the same bytes and the L1/L2 split above would stop being a pure
    // statement about the guard's threshold.
    let (direct, _) = serve_pseudo(CANONICAL, false);
    let (hook, _) = serve_pseudo(CANONICAL, true);
    assert_eq!(
        direct, hook,
        "the rewrite-origin tag must not change stdout (#317)"
    );
}

// ---------------------------------------------------------------------------
// The boundary: where the fix is real but unobservable through the CLI
// ---------------------------------------------------------------------------

/// At 235 B the fix exists in the transform and reaches no reader at hook
/// origin: the guard serves the raw source byte-identically and emits no marker,
/// so `id: UserId;` appears on stdout because it was never removed, not because
/// it was restored.
///
/// This is what makes the canonical fixture's size load-bearing rather than
/// incidental.  Arithmetic, from the guard's own rule
/// (`compressed + notice >= raw` → Passthrough, on `trim`med lengths):
///
/// ```text
/// raw(trimmed) = 234    hook-origin notice = 163
/// pseudo view  = 105 (post-fix; 87 pre-fix)
/// 105 + 163 = 268  >=  234   ->  Passthrough, and masked by 34 B to spare
/// ```
///
/// Only the hook-origin verdict is asserted.  The direct-path verdict for this
/// fixture lands on an exact tie post-fix (`105 + 129 = 234 >= 234`), and a
/// test whose expected outcome sits on a zero-byte margin is a test about
/// rounding, not about behaviour.
#[test]
fn small_file_stays_masked_at_hook_origin_so_the_fix_is_invisible_there() {
    let dir = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let file = dir.path().join("small.ts");
    std::fs::write(&file, GUARD_MASKED).unwrap();

    let out = skim_cmd()
        .env("SKIM_REWRITTEN_FROM", "cat")
        .env("SKIM_CACHE_DIR", cache.path().as_os_str())
        .arg(file.to_str().unwrap())
        .arg("--mode=pseudo")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(out.status.success(), "must still exit 0");
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();

    assert_eq!(
        stdout, GUARD_MASKED,
        "the guard must serve this file's raw bytes verbatim at hook origin"
    );
    assert!(
        !stderr.contains(MARKER_TAIL),
        "a raw passthrough owes no transparency marker — the reader sees the \
         source, and gets no cue that a transform was declined\nstderr:\n{stderr}"
    );
}
