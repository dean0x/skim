//! D3 companion: regression tests asserting that `skim git show --json <sha>` carries
//! the actual patch body — not just commit-level metadata.
//!
//! # Context (issue #510)
//!
//! Commit ae94d3f ("--json carries patch body") added a `patch` field to the JSON
//! envelope produced by both `skim git diff --json` and `skim git show --json`. The
//! diff side is pinned by three containment tests in `cli_git_diff_json_content.rs`.
//! This file pins the show side with a parallel set of three tests.
//!
//! # JSON field path
//!
//! `skim git show <sha> --json` serialises a `ShowCommitResult`
//! (`crates/rskim/src/output/canonical.rs` lines 2676–2705). The patch body lives at
//! `files[N].patch` — a `DiffFileEntry.patch: Option<String>` field defined at
//! `canonical.rs` lines 913–921, populated inside `render_show_diff`
//! (`crates/rskim/src/cmd/git/show.rs` lines 499–518) by walking each parsed hunk's
//! `old_start / old_count / new_start / new_count` and its `patch_lines`.
//!
//! # Fixture determinism
//!
//! All three tests operate on commit `b79f6e3`, the same commit referenced by
//! `TEST_RANGE` in `cli_git_diff_json_content.rs`. It touches one file (helpers.rs)
//! with 6 674 bytes of diff content (`git diff b79f6e3^..b79f6e3 | wc -c`).
//! The pinned commit MUST be reachable from `main` (verify with
//! `git merge-base --is-ancestor <sha> origin/main`), because branch-only commits
//! disappear after a squash-merge. Using a pinned SHA means the test results are
//! independent of the working-tree state and of which branch is checked out.
//!
//! # Baseline principle (PF-026 / PF-027)
//!
//! Raw byte counts come from `/usr/bin/git` invoked by absolute path, with
//! `SKIM_PASSTHROUGH` removed from the child env, to avoid the skim rewrite hook.

mod common;

use std::process::Command;

/// Commit SHA used as the test fixture.
///
/// `b79f6e3` touches one file (`helpers.rs`) with 6 674 bytes of diff content
/// (`git diff b79f6e3^..b79f6e3 | wc -c`). This is the same commit referenced
/// by `TEST_RANGE` in `cli_git_diff_json_content.rs`. The pinned commit MUST be
/// reachable from `main` (verify with `git merge-base --is-ancestor <sha>
/// origin/main`), because branch-only commits disappear after a squash-merge.
const TEST_SHA: &str = "b79f6e3";

// ============================================================================
// Helpers
// ============================================================================

/// Byte count of the diff body from raw `git show <sha>`.
///
/// The commit header — everything before the first `diff --git` line — is stripped
/// before measuring. The header is metadata (author, date, subject), not patch
/// content; stripping it makes the ratio assertion reflect actual diff-body coverage
/// rather than header size.
///
/// The split logic mirrors `parse_commit_header` in
/// `crates/rskim/src/cmd/git/show.rs` lines 379–385, where the same `"\ndiff --git "`
/// anchor is used to locate the split point.
///
/// Invokes `/usr/bin/git` by absolute path and removes `SKIM_PASSTHROUGH` from the
/// child env to avoid the skim rewrite hook (PF-026).
fn raw_git_show_diff_bytes() -> usize {
    let output = Command::new("/usr/bin/git")
        .args(["show", TEST_SHA])
        .env_remove("SKIM_PASSTHROUGH")
        .output()
        .expect("/usr/bin/git must be available");
    assert!(
        output.status.success(),
        "git show must succeed for TEST_SHA; stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let raw = String::from_utf8_lossy(&output.stdout);
    // Strip the commit header: everything before the first `diff --git` line.
    // The leading `\n` anchors the search to the start of a line, preventing
    // false positives if the commit message body mentions `diff --git` textually.
    let diff_start = raw
        .find("\ndiff --git ")
        .map(|p| p + 1)
        .unwrap_or(raw.len());
    raw[diff_start..].len()
}

/// Run `skim git show <sha> --json` and return the raw stdout bytes.
fn skim_show_json_bytes() -> Vec<u8> {
    common::skim()
        .args(["git", "show", TEST_SHA, "--json"])
        .output()
        .expect("skim git show --json must not fail to spawn")
        .stdout
}

// ============================================================================
// Tests
// ============================================================================

/// The concatenated patch bodies in `files[].patch` MUST account for ≥90% of
/// the raw diff-body bytes.
///
/// `ShowCommitResult` serialises a `files: Vec<DiffFileEntry>` array
/// (`canonical.rs` lines 2703); each entry carries a `patch: Option<String>` field
/// (`canonical.rs` line 921) populated by `render_show_diff` (`show.rs` lines
/// 499–518). After D3, those patch fields must contain the full hunk content.
///
/// A metadata-only JSON (no `patch`) yields ~0% of the raw diff bytes; a
/// content-bearing JSON yields ≥100% (each hunk line is preserved verbatim,
/// plus `@@ … @@` headers). The 90% threshold tolerates minor formatting
/// differences while definitively rejecting a metadata-only response.
///
/// The commit header of `git show` output is stripped before measuring the raw
/// baseline — see `raw_git_show_diff_bytes()`.
#[test]
fn show_json_patch_bytes_contain_raw_hunks() {
    let raw_diff_len = raw_git_show_diff_bytes();
    let json_bytes = skim_show_json_bytes();
    let json_str = String::from_utf8_lossy(&json_bytes);

    // Parse the JSON and concatenate all files[].patch bodies.
    // Field path: ShowCommitResult.files[N].patch (DiffFileEntry.patch: Option<String>).
    let parsed: serde_json::Value =
        serde_json::from_slice(&json_bytes).expect("skim git show --json must emit valid JSON");
    let patch_body: String = parsed["files"]
        .as_array()
        .expect("JSON must have a 'files' array (ShowCommitResult.files, canonical.rs:2703)")
        .iter()
        .filter_map(|f| f["patch"].as_str())
        .collect::<Vec<_>>()
        .join("");
    let patch_len = patch_body.len();

    assert!(
        patch_len >= (raw_diff_len * 9 / 10),
        "show --json: patch bodies dropped diff content\n  \
         raw diff bytes (git show, header stripped): {raw_diff_len}\n  \
         concatenated files[].patch bytes:           {patch_len}\n  \
         loss: {:.1}%\n  \
         json preview (first 400 chars):\n{}\n  \
         Expected: patch_len >= {} (90%% of raw diff)\n  \
         Fix: DiffFileEntry.patch must carry raw hunk content (show.rs:499-518).",
        if raw_diff_len > 0 {
            (1.0 - patch_len as f64 / raw_diff_len as f64) * 100.0
        } else {
            0.0
        },
        &json_str[..json_str.len().min(400)],
        raw_diff_len * 9 / 10,
    );
}

/// The JSON output MUST contain at least one unified-diff hunk header (`@@`).
///
/// A unified diff hunk begins with `@@`. If no `@@` appears anywhere in the JSON
/// output, the `files[].patch` fields are absent or empty — the content-loss defect
/// is confirmed at the character level without relying on byte counts.
///
/// The `patch` field lives at `files[N].patch` in the `ShowCommitResult` JSON
/// envelope (`canonical.rs` line 921, populated by `render_show_diff`,
/// `show.rs` lines 499–518).
#[test]
fn show_json_patch_has_hunk_headers() {
    let json_bytes = skim_show_json_bytes();
    let json_str = String::from_utf8_lossy(&json_bytes);

    assert!(
        json_str.contains("@@"),
        "show --json: JSON contains no hunk header (@@)\n  \
         This confirms the patch body is absent from files[].patch.\n  \
         json output:\n{json_str}\n  \
         Fix: DiffFileEntry.patch must carry raw hunk lines \
         (canonical.rs:921, show.rs:499-518).",
    );
}

/// The first `+`/`-` content line of raw `git show` MUST appear verbatim in the JSON patch.
///
/// When `skim git show --json` faithfully re-encodes the diff (`Completeness::Reencoded`),
/// every diff line from `/usr/bin/git show` must appear in one of the `files[].patch`
/// strings. This test picks the first `+`/`-` line (excluding `+++`/`---` file headers)
/// from the raw output and asserts it is present in the concatenated patch bodies.
///
/// Uses `/usr/bin/git` by absolute path with `SKIM_PASSTHROUGH` removed to avoid the
/// skim rewrite hook (PF-026).
#[test]
fn show_json_patch_contains_first_changed_line_verbatim() {
    // Collect the first added/removed line from raw git show output.
    let raw_output = Command::new("/usr/bin/git")
        .args(["show", TEST_SHA])
        .env_remove("SKIM_PASSTHROUGH")
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
        .expect("TEST_SHA must have at least one +/- diff line");

    let json_bytes = skim_show_json_bytes();
    let json_str = String::from_utf8_lossy(&json_bytes);

    // Concatenate all files[].patch bodies for the containment check.
    let parsed: serde_json::Value =
        serde_json::from_slice(&json_bytes).expect("skim git show --json must emit valid JSON");
    let patch_body: String = parsed["files"]
        .as_array()
        .expect("JSON must have a 'files' array")
        .iter()
        .filter_map(|f| f["patch"].as_str())
        .collect::<Vec<_>>()
        .join("");

    assert!(
        patch_body.contains(check_line),
        "show --json: first changed line missing from files[].patch\n  \
         expected line (verbatim from git show):\n    {check_line:?}\n  \
         json (first 600 chars):\n{}",
        &json_str[..json_str.len().min(600)],
    );
}
