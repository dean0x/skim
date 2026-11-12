//! Integration tests for `--offset` pagination on the lexical `skim search`
//! query path (#404).
//!
//! These drive the real `skim` binary via `assert_cmd` to prove that `--offset`
//! is honored end to end and that the pagination terminator (`has_more`) is
//! reported on the right pages.
//!
//! # Coverage
//!
//! - `--offset` pages a text query into disjoint pages that cover every match
//! - `--offset 0` produces the same TEXT stdout as no `--offset` (run time aside)
//! - paging past the end yields an empty page, exit 0
//! - a hostile `--offset` at `usize::MAX` does not overflow the candidate pool
//! - `has_more` is true when more results exist and absent on the last page
//! - the bounded-page notice goes to stderr, never to stdout

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use assert_cmd::Command;

// ============================================================================
// Project fixture helpers
// ============================================================================

/// Create N Rust files that each contain the sentinel token `qxz_shared_probe`
/// so a text query for it returns all N files.
///
/// Files are named `src/m01.rs` … `src/mNN.rs`.
fn make_multi_match_project(root: &Path, n: usize) {
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    for i in 1..=n {
        fs::write(
            src.join(format!("m{i:02}.rs")),
            format!("pub fn qxz_shared_probe_{i}() -> u32 {{ {i} }}\n"),
        )
        .unwrap();
    }
}

/// Build the lexical index for `proj`, routing all cache I/O to `cache`.
fn build_index(proj: &Path, cache: &Path) {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "--build", "--root"])
        .arg(proj)
        .env("SKIM_CACHE_DIR", cache)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success();
}

/// Run `skim search qxz_shared_probe <extra...> --root <proj>` against `cache`
/// and return the captured output, asserting exit 0.
fn search(proj: &Path, cache: &Path, extra: &[&str]) -> std::process::Output {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "qxz_shared_probe"])
        .args(extra)
        .arg("--root")
        .arg(proj)
        .env("SKIM_CACHE_DIR", cache)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success()
        .get_output()
        .clone()
}

/// TEXT stdout with the run time removed from the summary line (`... in 12ms`),
/// the one part of the output that differs between two identical runs.
fn without_timing(stdout: &[u8]) -> String {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(|line| {
            if line.contains(" result(s) for ") && line.ends_with("ms") {
                line.rsplit_once(" in ").map_or(line, |(head, _)| head)
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse a `--json` page and return its result paths in order.
fn result_paths(out: &std::process::Output) -> Vec<String> {
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("search --json stdout must be valid JSON");
    v["results"]
        .as_array()
        .expect("JSON must carry a results array")
        .iter()
        .map(|r| r["path"].as_str().expect("result path").to_string())
        .collect()
}

// ============================================================================
// Disjoint pages
// ============================================================================

/// Three consecutive pages of a 6-match text query are disjoint and together
/// cover all 6 files; a fourth page past the end is empty.
///
/// Discriminating: an `--offset` that is parsed but never applied returns the
/// same first page every time, so the pages would overlap and the union would
/// hold only 2 paths.
#[test]
fn offset_paginates_text_query_into_disjoint_pages() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    make_multi_match_project(proj.path(), 6);
    build_index(proj.path(), cache.path());

    let mut seen = BTreeSet::new();
    for offset in ["0", "2", "4"] {
        let page = result_paths(&search(
            proj.path(),
            cache.path(),
            &["--limit", "2", "--offset", offset, "--json"],
        ));
        assert_eq!(page.len(), 2, "offset {offset} must return a full page");
        for path in page {
            assert!(
                seen.insert(path.clone()),
                "offset {offset}: {path} already appeared on an earlier page"
            );
        }
    }
    assert_eq!(seen.len(), 6, "the three pages must cover all 6 matches");

    let past_end = result_paths(&search(
        proj.path(),
        cache.path(),
        &["--limit", "2", "--offset", "6", "--json"],
    ));
    assert!(
        past_end.is_empty(),
        "paging past the end must yield an empty page, got: {past_end:?}"
    );
}

/// `--offset 0` must produce the same TEXT stdout as omitting `--offset`, apart
/// from the run time in the summary line.
#[test]
fn offset_zero_identical_to_no_offset_text() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    make_multi_match_project(proj.path(), 4);
    build_index(proj.path(), cache.path());

    let without = search(proj.path(), cache.path(), &["--limit", "10"]);
    let with_zero = search(
        proj.path(),
        cache.path(),
        &["--limit", "10", "--offset", "0"],
    );

    assert!(
        !without.stdout.is_empty(),
        "precondition: the query must produce output"
    );
    assert_eq!(
        without_timing(&without.stdout),
        without_timing(&with_zero.stdout),
        "--offset 0 must not change TEXT stdout"
    );
}

/// Paging past the end yields an empty page: exit 0 and `total` of 0.
#[test]
fn offset_past_end_yields_empty_page() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    make_multi_match_project(proj.path(), 3);
    build_index(proj.path(), cache.path());

    let out = search(
        proj.path(),
        cache.path(),
        &["--limit", "5", "--offset", "100", "--json"],
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(
        v["total"].as_u64(),
        Some(0),
        "an offset past the last match must yield an empty page, got: {v}"
    );
    assert!(
        v.get("has_more").is_none(),
        "an empty past-the-end page must not claim more results, got: {v}"
    );
}

// ============================================================================
// Hostile offset
// ============================================================================

/// A `--offset` at `usize::MAX` must NOT overflow the additive candidate-pool
/// widening (`candidate_pool(limit, K) + offset`). Without the saturating add
/// this panics in debug and test builds ("attempt to add with overflow") and
/// wraps to a garbage pool size in release. Expected: exit 0, empty page, no
/// panic.
#[test]
fn hostile_max_offset_does_not_overflow() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    // Multi-word query: the only path that computes `candidate_pool(...) + offset`.
    make_multi_match_project(proj.path(), 3);
    build_index(proj.path(), cache.path());

    let out = Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search",
            "qxz_shared probe",
            "--limit",
            "5",
            // usize::MAX on a 64-bit target.
            "--offset",
            "18446744073709551615",
            "--json",
            "--root",
        ])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success() // must NOT panic/overflow
        .get_output()
        .stdout
        .clone();

    let v: serde_json::Value =
        serde_json::from_slice(&out).expect("hostile-offset page must be valid JSON");
    assert_eq!(
        v["total"].as_u64(),
        Some(0),
        "paging past usize::MAX must yield an empty page, got: {v}"
    );
}

// ============================================================================
// has_more and the bounded-page notice
// ============================================================================

/// `has_more` must be `true` on a text query when the result set is larger than
/// the page.
///
/// `resolve_paths_and_snippets_verified` uses a "probe one more" strategy:
/// collect `limit + 1` results after the skip, set `has_more = true` when the
/// probe item exists, then truncate to `limit`.
#[test]
fn has_more_true_on_text_query_when_results_exceed_limit() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    // 6 files each containing "qxz_shared_probe"; limit=2 → 4 remain → has_more=true.
    make_multi_match_project(proj.path(), 6);
    build_index(proj.path(), cache.path());

    let out = search(proj.path(), cache.path(), &["--limit", "2", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(
        v["has_more"].as_bool(),
        Some(true),
        "has_more must be true when limit=2 < total results (6 files); got: {v}"
    );
}

/// `has_more` must be absent (false) on the last page of a text query.
///
/// When all matches fit on one page, `has_more` is not serialized
/// (`#[serde(skip_serializing_if = "Not::not")]`).
#[test]
fn has_more_absent_on_text_query_when_all_results_fit_on_page() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    // 3 files; limit=100 → all fit on one page → has_more must be absent.
    make_multi_match_project(proj.path(), 3);
    build_index(proj.path(), cache.path());

    let out = search(proj.path(), cache.path(), &["--limit", "100", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert!(
        v.get("has_more").is_none(),
        "has_more must be ABSENT when all results fit on one page; got: {:?}",
        v.get("has_more")
    );
}

/// When a page is capped (limit < total), the notice that more results exist and
/// the next `--offset` appears on stderr. `--json` stdout stays byte-clean.
#[test]
fn bounded_page_notice_goes_to_stderr_not_stdout() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    make_multi_match_project(proj.path(), 6);
    build_index(proj.path(), cache.path());

    let out = search(proj.path(), cache.path(), &["--limit", "2", "--json"]);

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("more results exist") && stderr.contains("--offset 2"),
        "the bounded-page notice must name the next offset on stderr; got: {stderr:?}"
    );
    assert!(
        !stdout.contains("more results exist"),
        "the bounded-page notice must NOT appear in stdout; got: {stdout:?}"
    );
}
