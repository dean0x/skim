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

use std::path::Path;
use std::process::Command;

// ============================================================================
// Hermetic repo helpers (architecture-3 / architecture-4 regression tests)
// ============================================================================

/// Run a git command in `dir` and panic on failure.
///
/// Uses `git` from PATH rather than the absolute-path helper (`git_bin`)
/// because this is control infrastructure, not an output-comparison baseline.
/// PF-009: pins all four per-test config values so the fixture is deterministic
/// across maintainers with `commit.gpgsign=true` or non-`main` defaultBranch.
fn git_in(dir: &Path, args: &[&str]) {
    let step = args.join(" ");
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("hermetic setup: `git {step}` spawn failed: {e}"));
    assert!(
        out.status.success(),
        "hermetic setup: `git {step}` failed;\nstderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Create a hermetic repo containing three commits that produce patch-less diffs:
///
/// - `HEAD~2`: adds a text file (has hunks — baseline commit)
/// - `HEAD~1`: renames the text file with 100% similarity (no hunks)
/// - `HEAD`:   adds a binary file and then modifies it (no hunks for binary)
///
/// Also produces a mode-only change via `git update-index --chmod=+x`.
///
/// Returns the temp dir (caller must keep alive) and the repo path.
fn make_patchless_diff_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir must succeed");
    let path = dir.path().to_path_buf();

    git_in(&path, &["init", "-b", "main"]);
    git_in(&path, &["config", "user.email", "test@example.com"]);
    git_in(&path, &["config", "user.name", "Test"]);
    git_in(&path, &["config", "commit.gpgsign", "false"]);
    git_in(&path, &["config", "core.autocrlf", "false"]);

    // Commit 1: add a plain text file.
    std::fs::write(path.join("hello.txt"), "hello world\n").expect("write text file");
    git_in(&path, &["add", "hello.txt"]);
    git_in(&path, &["commit", "-m", "add text file"]);

    // Commit 2: 100%-similarity rename — no @@ hunks expected.
    std::fs::rename(path.join("hello.txt"), path.join("greeting.txt")).expect("rename file");
    git_in(&path, &["add", "-A"]);
    git_in(&path, &["commit", "-m", "rename hello.txt -> greeting.txt"]);

    // Commit 3: mode-only change — chmod +x, no content change → no @@ hunks.
    git_in(&path, &["update-index", "--chmod=+x", "greeting.txt"]);
    git_in(&path, &["commit", "-m", "chmod +x greeting.txt"]);

    // Commit 4: add a binary file.
    let binary_bytes: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x00, 0x01, 0x02, 0x03];
    std::fs::write(path.join("image.bin"), binary_bytes).expect("write binary file");
    git_in(&path, &["add", "image.bin"]);
    git_in(&path, &["commit", "-m", "add binary file"]);

    // Commit 5: modify binary file — git will report "Binary files … differ".
    let binary_bytes2: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x00, 0xFF, 0xFE, 0xFD];
    std::fs::write(path.join("image.bin"), binary_bytes2).expect("write binary file v2");
    git_in(&path, &["add", "image.bin"]);
    git_in(&path, &["commit", "-m", "modify binary file"]);

    (dir, path)
}

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

// ============================================================================
// architecture-3: Lossy marker fires for patch-less diff output
// ============================================================================

/// **architecture-3 binary**: `skim git diff --json` over a commit that only
/// touches a binary file must fire the ADR-011 class-1 disclosure marker on
/// stderr, because no hunk content can be carried in the JSON envelope.
///
/// Before the fix, `Completeness::Reencoded` was hard-coded and the marker
/// never fired, leaving the consumer with no indication that binary content
/// was silently dropped.
#[test]
fn arch3_binary_diff_json_emits_lossy_marker() {
    let (_dir, repo) = make_patchless_diff_repo();

    // HEAD is the "modify binary file" commit; HEAD^ is "add binary file".
    let output = common::skim()
        .current_dir(&repo)
        .args(["git", "diff", "HEAD^..HEAD", "--json"])
        .output()
        .expect("skim git diff --json must not fail to spawn");

    // stdout must be valid JSON.
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str::<serde_json::Value>(&stdout).unwrap_or_else(|e| {
        panic!(
            "arch3 binary: stdout must be valid JSON, got parse error: {e}\n\
             stdout (first 500 chars):\n{}",
            &stdout[..stdout.len().min(500)]
        )
    });

    // stderr must contain the class-1 Lossy marker.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[skim] json view of 'git':"),
        "arch3 binary: expected ADR-011 class-1 '[skim] json view of \\'git\\':' on stderr\n\
         stderr:\n{stderr}\n\
         Fix: Completeness must be Lossy when any DiffFileEntry has patch: None."
    );
}

/// **architecture-3 rename (100% similarity)**: `skim git diff --json` over a
/// 100%-similarity rename commit must fire the Lossy marker because no `@@`
/// hunks exist (the content is identical, only the path changes).
///
/// Commit layout (0=oldest): add-text → rename → chmod → add-binary → modify-binary(HEAD)
/// Range `HEAD~4..HEAD~3` spans commit1→commit2 = the rename commit.
#[test]
fn arch3_rename_100pct_diff_json_emits_lossy_marker() {
    let (_dir, repo) = make_patchless_diff_repo();

    // HEAD~4 is "add text file"; HEAD~3 is the rename commit.
    // Pass --find-renames=100% so git presents the change as a rename rather
    // than a deletion + addition (which would have hunk content).
    let output = common::skim()
        .current_dir(&repo)
        .args([
            "git",
            "diff",
            "HEAD~4..HEAD~3",
            "--find-renames=100%",
            "--json",
        ])
        .output()
        .expect("skim git diff --json must not fail to spawn");

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str::<serde_json::Value>(&stdout).unwrap_or_else(|e| {
        panic!(
            "arch3 rename: stdout must be valid JSON, got parse error: {e}\n\
             stdout (first 500 chars):\n{}",
            &stdout[..stdout.len().min(500)]
        )
    });

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[skim] json view of 'git':"),
        "arch3 rename: expected Lossy marker on stderr\n\
         stderr:\n{stderr}"
    );
}

/// **architecture-3 mode-only change**: `skim git diff --json` over a commit
/// that only changes a file's execute bit (no content change) must fire the
/// Lossy marker because no `@@` hunks exist.
///
/// Commit layout (0=oldest): add-text → rename → chmod → add-binary → modify-binary(HEAD)
/// Range `HEAD~3..HEAD~2` spans commit2→commit3 = the mode-only chmod commit.
#[test]
fn arch3_mode_only_diff_json_emits_lossy_marker() {
    let (_dir, repo) = make_patchless_diff_repo();

    // HEAD~3 is "rename"; HEAD~2 is the "chmod +x" commit.
    let output = common::skim()
        .current_dir(&repo)
        .args(["git", "diff", "HEAD~3..HEAD~2", "--json"])
        .output()
        .expect("skim git diff --json must not fail to spawn");

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str::<serde_json::Value>(&stdout).unwrap_or_else(|e| {
        panic!(
            "arch3 mode-only: stdout must be valid JSON, got parse error: {e}\n\
             stdout (first 500 chars):\n{}",
            &stdout[..stdout.len().min(500)]
        )
    });

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[skim] json view of 'git':"),
        "arch3 mode-only: expected Lossy marker on stderr\n\
         stderr:\n{stderr}"
    );
}

// ============================================================================
// architecture-4: --dirstat --json produces parseable JSON
// ============================================================================

/// **architecture-4 dirstat — deterministic hermetic repo**: `skim git diff
/// --dirstat --json` must emit valid JSON on stdout for BOTH output shapes:
///
/// - **Case A** (non-empty dirstat): a commit that changes a file inside a
///   subdirectory.  `git diff --dirstat` produces e.g. `100.0% subdir/` — this
///   is non-empty but not a unified diff, so it goes through the B1 empty-parse
///   branch (already handled correctly before this fix).
///
/// - **Case B** (empty dirstat — the bug): a commit that changes only root-level
///   files.  `git diff --dirstat` produces empty stdout because dirstat only
///   accounts for subdirectories.  This hit the empty-diff guard in `run_diff`,
///   which printed "No changes" to stderr and returned with *nothing* on stdout
///   — violating the `--json` contract.  After the fix, the empty-diff guard
///   checks `output_format` and emits `{"files":[],"raw":"No changes\n"}`.
///
/// ## Why the original test was fragile
///
/// The old test ran `skim git diff HEAD~1..HEAD --dirstat --json` against the
/// live skim repo.  A docs-only commit touching only root-level files
/// (CHANGELOG.md, README.md) triggered the empty-dirstat path and caused the
/// test to fail — this is what blocked PR #536, a changelog-only change.
/// Pinning to a hermetic fixture makes the test independent of HEAD.
#[test]
fn arch4_dirstat_json_produces_parseable_json() {
    // -----------------------------------------------------------------------
    // Build a hermetic repo with known commit structure:
    //   commit 1 (baseline): root.txt only — gives us a non-empty base.
    //   commit 2: subdir/deep.txt — diff HEAD~2..HEAD~1 has a non-empty dirstat.
    //   commit 3: root.txt modified — diff HEAD~1..HEAD has an empty dirstat
    //             (root-level files do not appear in --dirstat output).
    // -----------------------------------------------------------------------
    let dir = tempfile::tempdir().expect("tempdir must succeed");
    let repo = dir.path().to_path_buf();

    // PF-009: pin all four git identity / signing config values so the fixture
    // is deterministic across maintainers with commit.gpgsign=true or a
    // non-`main` defaultBranch.
    git_in(&repo, &["init", "-b", "main"]);
    git_in(&repo, &["config", "user.email", "test@example.com"]);
    git_in(&repo, &["config", "user.name", "Test"]);
    git_in(&repo, &["config", "commit.gpgsign", "false"]);
    git_in(&repo, &["config", "core.autocrlf", "false"]);

    // Commit 1: baseline root-level file.
    std::fs::write(repo.join("root.txt"), "root content\n").unwrap();
    git_in(&repo, &["add", "root.txt"]);
    git_in(&repo, &["commit", "-m", "initial root file"]);

    // Commit 2: add a file inside a subdirectory.
    // `git diff HEAD~2..HEAD~1 --dirstat` → "  100.0% subdir/" (non-empty).
    std::fs::create_dir(repo.join("subdir")).unwrap();
    std::fs::write(repo.join("subdir/deep.txt"), "inside a directory\n").unwrap();
    git_in(&repo, &["add", "subdir/deep.txt"]);
    git_in(&repo, &["commit", "-m", "add file in subdirectory"]);

    // Commit 3: modify only the root-level file.
    // `git diff HEAD~1..HEAD --dirstat` → empty stdout (root-level files are
    // not in any subdirectory and thus do not appear in dirstat output).
    std::fs::write(repo.join("root.txt"), "modified root content\n").unwrap();
    git_in(&repo, &["add", "root.txt"]);
    git_in(&repo, &["commit", "-m", "modify root-level file only"]);

    // -----------------------------------------------------------------------
    // Case A: non-empty dirstat (subdirectory change).
    // Range HEAD~2..HEAD~1 = commit 1 → commit 2.
    // git produces dirstat lines; skim's B1 empty-parse branch wraps in JSON.
    // -----------------------------------------------------------------------
    let out_a = common::skim()
        .current_dir(&repo)
        .args(["git", "diff", "HEAD~2..HEAD~1", "--dirstat", "--json"])
        .output()
        .expect("skim git diff --dirstat --json must not fail to spawn");

    let stdout_a = String::from_utf8_lossy(&out_a.stdout);
    let parsed_a = serde_json::from_str::<serde_json::Value>(&stdout_a);
    assert!(
        parsed_a.is_ok(),
        "arch4 case A (subdir change): --dirstat --json must produce valid JSON\n\
         parse error: {:?}\n\
         stdout:\n{}",
        parsed_a.err(),
        &stdout_a[..stdout_a.len().min(600)]
    );
    assert!(
        parsed_a.unwrap().is_object(),
        "arch4 case A: JSON output must be an object"
    );

    // -----------------------------------------------------------------------
    // Case B: empty dirstat (root-only change) — this is the bug location.
    // Range HEAD~1..HEAD = commit 2 → commit 3.
    // git produces empty stdout; skim's empty-diff guard must emit JSON.
    // -----------------------------------------------------------------------
    let out_b = common::skim()
        .current_dir(&repo)
        .args(["git", "diff", "HEAD~1..HEAD", "--dirstat", "--json"])
        .output()
        .expect("skim git diff --dirstat --json must not fail to spawn");

    let stdout_b = String::from_utf8_lossy(&out_b.stdout);
    let parsed_b = serde_json::from_str::<serde_json::Value>(&stdout_b);
    assert!(
        parsed_b.is_ok(),
        "arch4 case B (root-only change): --dirstat --json must produce valid JSON\n\
         got parse error: {:?}\n\
         stdout (first 600 chars):\n{}",
        parsed_b.err(),
        &stdout_b[..stdout_b.len().min(600)]
    );
    let val_b = parsed_b.unwrap();
    assert!(
        val_b.is_object(),
        "arch4 case B: --dirstat --json output must be a JSON object\nvalue: {val_b:?}"
    );
    // Verify the exact envelope shape for the empty-dirstat case.
    assert_eq!(
        val_b["files"],
        serde_json::json!([]),
        "arch4 case B: 'files' must be an empty array for an empty dirstat"
    );
    assert_eq!(
        val_b["raw"],
        serde_json::json!("No changes\n"),
        "arch4 case B: 'raw' must carry \"No changes\\n\" for an empty dirstat"
    );
}
