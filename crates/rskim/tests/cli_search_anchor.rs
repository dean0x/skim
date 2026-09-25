//! Integration tests: lexical result anchoring for substring-only matches.
//!
//! Owned by the F-C4-01 fix (dog-food campaign 2026-09, corpus `c4-merge`).
//!
//! # What is covered
//!
//! A file whose content contains the query only **inside a longer identifier**
//! (e.g. `mk4_longline_marker` inside the 919-byte token
//! `AAAA…AAAAmk4_longline_marker`) is a *substring-only* candidate: the AD-411-7
//! `token_length` gate deliberately gives it zero aligned whole-token
//! occurrences, so `search_exact_intersection` emits it with score `0.0` and an
//! empty `match_positions` vec.  ADR-007 requires it to stay in the result set
//! (git-grep recall parity) — and AD-396-8 requires it to carry a real
//! `line_number`, resolved by the substring-verify gate from the file bytes.
//!
//! These tests drive the real `skim` binary so the whole reader → verify →
//! snippet chain is exercised (PF-007), not just the snippet helper.

use std::path::Path;
use std::process::Command as StdCommand;

use assert_cmd::cargo::cargo_bin;
use serde_json::Value;
use tempfile::TempDir;

// ============================================================================
// Helpers
// ============================================================================

/// Initialize a git repo with hermetic, non-signing identity.
fn git_init(dir: &Path) {
    for args in &[
        vec!["init"],
        vec!["config", "user.email", "test@t.com"],
        vec!["config", "user.name", "T"],
        vec!["config", "commit.gpgsign", "false"],
    ] {
        let s = StdCommand::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(s.success(), "git {args:?} failed in {dir:?}");
    }
}

/// Stage everything in the working tree and create one commit.
fn git_add_commit(dir: &Path, msg: &str) {
    let s = StdCommand::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .status()
        .expect("git add");
    assert!(s.success(), "git add failed");
    let s = StdCommand::new("git")
        .args(["commit", "-m", msg])
        .current_dir(dir)
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .status()
        .expect("git commit");
    assert!(s.success(), "git commit failed");
}

/// Build the search index for `proj` into the isolated `cache` directory.
fn build_index(proj: &Path, cache: &Path) {
    let out = StdCommand::new(cargo_bin("skim"))
        .args(["search", "--build", "--root"])
        .arg(proj)
        .env("SKIM_CACHE_DIR", cache)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .output()
        .expect("skim search --build");
    assert!(
        out.status.success(),
        "skim search --build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Run `skim search <args> --root <root>` and return `(stdout, stderr, exit_code)`.
fn skim_search(args: &[&str], root: &Path, cache: &Path) -> (String, String, i32) {
    let out = StdCommand::new(cargo_bin("skim"))
        .arg("search")
        .args(args)
        .arg("--root")
        .arg(root)
        .env("SKIM_CACHE_DIR", cache)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .output()
        .expect("skim search");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

/// Build the c4-merge `src/longline.rs` shape: one 955-byte line whose marker is
/// glued to a 900-byte `A` run, plus a 20 KB variant and a normal control file.
fn make_longline_repo(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("create src");
    let pad = "A".repeat(900);
    let longline = format!("fn long_one() {{ let payload = \"{pad}mk4_longline_marker\"; }}\n");
    assert_eq!(
        (longline.len(), longline.lines().count()),
        (955, 1),
        "fixture premise: longline.rs must be 955 bytes on a single line"
    );
    std::fs::write(root.join("src/longline.rs"), &longline).expect("write longline.rs");

    let big = "B".repeat(20_000);
    let hugeline = format!("fn huge_one() {{ let payload = \"{big}mk4_huge_marker\"; }}\n");
    std::fs::write(root.join("src/hugeline.rs"), &hugeline).expect("write hugeline.rs");

    // Control: the same marker as its own word token on a normal-length line.
    std::fs::write(
        root.join("src/normal.rs"),
        "fn normal_one() { let x = 1; }\n// mk4_longline_marker lives here\n",
    )
    .expect("write normal.rs");

    git_init(root);
    git_add_commit(root, "feat: adversarial line shapes");
}

/// Locate the result object for `path` in a `--json` search envelope.
fn result_for<'a>(json: &'a Value, path: &str) -> &'a Value {
    json["results"]
        .as_array()
        .unwrap_or_else(|| panic!("results must be an array; got {json}"))
        .iter()
        .find(|r| r["path"] == path)
        .unwrap_or_else(|| panic!("{path} must appear in results; got {json}"))
}

// ============================================================================
// Tests
// ============================================================================

/// AD-396-8 / F-C4-01 — a 955-byte single line whose marker is glued to a long
/// identifier is recalled AND anchored: `line_number == 1`.
///
/// PF-007 discriminating observable: `line_number` is `1`, not `null`.  Before
/// the fix the AD-396-5 guard nulled the content-derived anchor for every
/// empty-`match_positions` Substring candidate, so this file came back with
/// `line_number: null` and an agent piping `path:line` into a read got nothing.
#[test]
fn f_c4_01_long_line_substring_match_is_anchored() {
    let dir = TempDir::new().expect("TempDir");
    let root = dir.path().join("repo");
    make_longline_repo(&root);
    let cache = TempDir::new().expect("cache TempDir");
    build_index(&root, cache.path());

    let (stdout, stderr, code) = skim_search(
        &["mk4_longline_marker", "--limit", "10", "--json"],
        &root,
        cache.path(),
    );
    assert_eq!(code, 0, "F-C4-01: query must exit 0; stderr:\n{stderr}");

    let v: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("F-C4-01: --json parse error: {e}\n{stdout}"));

    let longline = result_for(&v, "src/longline.rs");
    assert_eq!(
        longline["line_number"].as_u64(),
        Some(1),
        "F-C4-01: a verified match on a 955-byte line must carry line_number 1, \
         never null; got {longline}"
    );

    // The control file (marker as its own token) keeps its exact-token anchor and
    // a non-zero BM25F score — proving the fix did not flatten normal ranking.
    let normal = result_for(&v, "src/normal.rs");
    assert_eq!(
        normal["line_number"].as_u64(),
        Some(2),
        "F-C4-01 control: normal.rs must anchor to line 2; got {normal}"
    );
    assert!(
        normal["score"].as_f64().unwrap_or(0.0) > 0.0,
        "F-C4-01 control: an exact whole-token match must keep a non-zero BM25F \
         score; got {normal}"
    );
}

/// AD-396-8 / F-C4-01 — no hard line-length cap was introduced: a 20 KB single
/// line resolves an anchor on the same path.
#[test]
fn f_c4_01_20kb_line_substring_match_is_anchored() {
    let dir = TempDir::new().expect("TempDir");
    let root = dir.path().join("repo");
    make_longline_repo(&root);
    let cache = TempDir::new().expect("cache TempDir");
    build_index(&root, cache.path());

    let (stdout, stderr, code) = skim_search(
        &["mk4_huge_marker", "--limit", "10", "--json"],
        &root,
        cache.path(),
    );
    assert_eq!(
        code, 0,
        "F-C4-01: 20 KB query must exit 0; stderr:\n{stderr}"
    );

    let v: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("F-C4-01: --json parse error: {e}\n{stdout}"));
    let huge = result_for(&v, "src/hugeline.rs");
    assert_eq!(
        huge["line_number"].as_u64(),
        Some(1),
        "F-C4-01: a 20 KB single line must carry line_number 1, never null; got {huge}"
    );
}

/// AD-355-7 scope guard — the AD-396-8 narrowing must NOT widen short-query
/// behaviour: a query too short to produce a trigram stays snippet-less and
/// unanchored, exactly as before.
///
/// PF-007 discriminating observable: `line_number` is `null` for the `fn` query
/// while it is `1` for the full-length query in the test above.  If the
/// narrowing had removed the guard rather than scoping it, this would anchor.
#[test]
fn f_c4_01_short_query_remains_unanchored() {
    let dir = TempDir::new().expect("TempDir");
    let root = dir.path().join("repo");
    make_longline_repo(&root);
    let cache = TempDir::new().expect("cache TempDir");
    build_index(&root, cache.path());

    let (stdout, stderr, code) =
        skim_search(&["fn", "--limit", "10", "--json"], &root, cache.path());
    assert_eq!(code, 0, "short query must exit 0; stderr:\n{stderr}");

    let v: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("short query --json parse error: {e}\n{stdout}"));
    let results = v["results"].as_array().expect("results array");
    assert!(
        !results.is_empty(),
        "premise: the 2-byte query 'fn' must still recall files; got {stdout}"
    );
    for r in results {
        assert!(
            r["line_number"].is_null(),
            "AD-355-7 / AD-396-5: a <3-byte query must stay unanchored; got {r}"
        );
    }
}
