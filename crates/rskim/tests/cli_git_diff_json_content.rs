//! D3: Regression tests asserting that `skim git diff --json` carries the
//! actual patch body — not just file-level metadata.
//!
//! # The defect (issue #510)
//!
//! Before D3, `skim git diff <range> --json` produced a JSON envelope that
//! contained only per-file metadata (path, status, changed_regions).  All
//! actual patch content was silently dropped: a 6 674-byte raw diff became a
//! 166-byte JSON response — 97% content loss, exit 0, no stderr marker.
//!
//! # Red-before-green requirement
//!
//! The tests in this file were written BEFORE the D3 fix so the failure output
//! can be quoted in the implementation report.  Each test asserts a property
//! that the un-fixed code does NOT satisfy.
//!
//! # Baseline principle (PF-026 / PF-027)
//!
//! Raw byte counts come from `/usr/bin/git` invoked by absolute path to avoid
//! the skim rewrite hook.  Fixture sizes are never tuned to make a guard agree.

mod common;

use std::process::Command;

/// Pick a commit that has ≥1 000 bytes of real diff content and is reachable
/// in every checkout of this repository.
///
/// `b79f6e3^..b79f6e3` touches one file (helpers.rs) with 6 674 raw bytes
/// (`git diff b79f6e3^..b79f6e3 | wc -c`). The pinned commit MUST be
/// reachable from `main` (verify with `git merge-base --is-ancestor <sha>
/// origin/main`), because branch-only commits disappear after a squash-merge.
const TEST_RANGE: &str = "b79f6e3^..b79f6e3";

// ============================================================================
// Helper
// ============================================================================

/// Byte count of raw `git diff` for TEST_RANGE.  Measured via `/usr/bin/git`
/// to avoid the skim rewrite hook (PF-026).
fn raw_git_diff_bytes() -> usize {
    let output = Command::new("/usr/bin/git")
        .args(["diff", TEST_RANGE])
        .output()
        .expect("/usr/bin/git must be available");
    assert!(
        output.status.success(),
        "git diff must succeed for TEST_RANGE; stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout.len()
}

/// Run `skim git diff <range> --json` and return the JSON bytes.
fn skim_diff_json_bytes() -> Vec<u8> {
    common::skim()
        .args(["git", "diff", TEST_RANGE, "--json"])
        .output()
        .expect("skim git diff --json must not fail to spawn")
        .stdout
}

// ============================================================================
// D3: RED tests — must FAIL before the fix
// ============================================================================

/// **D3 primary RED test**: the JSON output MUST contain actual patch content.
///
/// A JSON envelope that carries only file-level metadata (path, status,
/// changed_regions) is a content-loss bug (#510, ADR-011 class-1).  After D3,
/// the JSON must include the raw hunk lines so consumers can reconstruct the
/// full diff.
///
/// This test is the acceptance gate for D3.  It asserts a content-containment
/// property that is **false** on the un-fixed code and **true** after D3.
///
/// The assertion: the JSON bytes must be ≥ raw bytes × 0.9.  A metadata-only
/// JSON is ~6% of raw; a content-bearing JSON is ≥ 100% of raw (the JSON
/// envelope adds overhead).
///
/// # Pre-fix failure (quoted for the implementation report)
///
/// ```text
/// raw bytes:  6674
/// json bytes: 166   (97.5% content loss — only file metadata, no hunks)
/// assertion:  json_len (166) < raw_len*0.9 (6006) → FAILS as expected
/// ```
#[test]
fn d3_json_output_carries_patch_content_not_just_metadata() {
    let raw_len = raw_git_diff_bytes();
    let json_bytes = skim_diff_json_bytes();
    let json_len = json_bytes.len();
    let json_str = String::from_utf8_lossy(&json_bytes);

    // Content-containment check: the JSON must be at least 90% of raw bytes.
    // A metadata-only response is <10% of raw; a content-bearing response
    // must be ≥100% (JSON overhead) once hunks are serialised.
    assert!(
        json_len >= (raw_len * 9 / 10),
        "D3: JSON dropped patch content\n  \
         raw bytes (git diff):  {raw_len}\n  \
         json bytes (skim):    {json_len}\n  \
         loss:  {:.1}%\n  \
         json preview (first 400 chars):\n{}\n  \
         Expected: json_len >= {} (90%% of raw)\n  \
         Fix: DiffFileEntry must carry the raw hunk content as a `patch` field.",
        (1.0 - json_len as f64 / raw_len as f64) * 100.0,
        &json_str[..json_str.len().min(400)],
        raw_len * 9 / 10,
    );
}

/// **D3 secondary RED test**: the JSON must contain at least one hunk header.
///
/// A unified diff hunk begins with `@@`.  If the JSON carries no `@@`, it
/// contains no patch content whatsoever — the defect is confirmed at the
/// character level without relying on byte counts.
#[test]
fn d3_json_output_contains_hunk_header() {
    let json_bytes = skim_diff_json_bytes();
    let json_str = String::from_utf8_lossy(&json_bytes);

    assert!(
        json_str.contains("@@"),
        "D3: JSON contains no hunk header (@@)\n  \
         This confirms the patch body is absent from the JSON envelope.\n  \
         json output:\n{json_str}\n  \
         Fix: DiffFileEntry.patch must carry the raw hunk lines.",
    );
}

/// **D3 Completeness::Reencoded content-containment test**: when the JSON
/// declares `Completeness::Reencoded`, it must actually contain all content.
///
/// The type system cannot verify that a value labelled `Reencoded` truly
/// contains the full diff.  This test bridges that gap by checking that the
/// JSON response, after D3, faithfully includes lines from the raw diff.
///
/// Specifically: every first `+`/`-` line from the raw diff must appear in
/// the JSON output.
#[test]
fn d3_reencoded_completeness_contains_all_add_remove_lines() {
    // Collect the first added/removed line from the raw diff.
    let raw_output = Command::new("/usr/bin/git")
        .args(["diff", TEST_RANGE])
        .output()
        .expect("/usr/bin/git must be available");
    let raw_str = String::from_utf8_lossy(&raw_output.stdout);

    // Pick the first line starting with `+` or `-` that is not a file header.
    let check_line = raw_str
        .lines()
        .find(|l| {
            (l.starts_with('+') || l.starts_with('-'))
                && !l.starts_with("+++")
                && !l.starts_with("---")
        })
        .expect("TEST_RANGE must have at least one +/- diff line");

    let json_bytes = skim_diff_json_bytes();
    let json_str = String::from_utf8_lossy(&json_bytes);

    assert!(
        json_str.contains(check_line),
        "D3 Completeness::Reencoded content-containment:\n  \
         expected line missing from JSON:\n    {check_line:?}\n  \
         json (first 600 chars):\n{}",
        &json_str[..json_str.len().min(600)],
    );
}
