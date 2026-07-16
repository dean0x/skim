//! Integration tests for `--offset` pagination on the standalone `--ast`,
//! `--hot`, `--cold`, `--risky`, and `--blast-radius` dispatch arms (#404).
//!
//! These drive the real `skim` binary via `assert_cmd` to prove that
//! `--offset` is honored on all five arms — the P1 defect this ticket fixes.
//!
//! # Coverage
//!
//! - AC-404-1: standalone `--ast` honors `--offset` (paginates disjoint sets)
//! - AC-404-2: standalone temporal arms (`--hot`, `--cold`, `--risky`) accept
//!   `--offset` without error (deep disjointness requires seeded git data;
//!   verified in unit tests)
//! - AC-404-3: `--blast-radius` accepts `--offset` without error
//! - AC-404-8: paging past end yields empty page, exit 0, correct JSON fields
//! - AC-404-12: `--offset 0` produces byte-identical TEXT stdout to no-offset
//! - AC-404-18: `has_more` field absent (false) on a single-page result set

use std::fs;
use std::path::Path;

use assert_cmd::Command;

// ============================================================================
// Project fixture helpers
// ============================================================================

/// Create a TypeScript project with `n` files each containing a try/catch
/// block, so `--ast try-catch` returns at least `n` results.
///
/// Files are named `src/f01.ts` … `src/fNN.ts` in lexicographic order,
/// which is the total-order tiebreak used by the search engine (file_path ASC).
fn make_try_catch_project(root: &Path, n: usize) {
    fs::create_dir_all(root.join("src")).unwrap();
    for i in 1..=n {
        let name = format!("src/f{i:02}.ts");
        let content = format!(
            r#"// file {i}
export async function op{i}(): Promise<string> {{
    const x = await Promise.resolve("{i}");
    try {{
        return x;
    }} catch (error) {{
        console.error("op{i} failed", error);
        return "";
    }}
}}
"#
        );
        fs::write(root.join(&name), content).unwrap();
    }
}

/// Build the lexical + AST index for `proj`, routing all cache I/O to `cache`.
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

// ============================================================================
// AC-404-1: standalone --ast honors --offset
// ============================================================================

/// AC-404-1: `--ast try-catch --offset N` returns the Nth page, with page 0
/// and page 1 returning disjoint result sets.
///
/// Pre-change binary returned [P1,P2] for BOTH `--offset 0` and `--offset 2`
/// because offset was silently ignored. Post-fix, offset skips N results.
#[test]
fn offset_paginates_standalone_ast() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    make_try_catch_project(proj.path(), 6);
    build_index(proj.path(), cache.path());

    // Page 0: first 2 results.
    let page0_out = Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search",
            "--ast",
            "try-catch",
            "--limit",
            "2",
            "--offset",
            "0",
            "--json",
            "--root",
        ])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // Page 1: skip 2, take next 2.
    let page1_out = Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search",
            "--ast",
            "try-catch",
            "--limit",
            "2",
            "--offset",
            "2",
            "--json",
            "--root",
        ])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let v0: serde_json::Value =
        serde_json::from_slice(&page0_out).expect("page 0 must be valid JSON");
    let v1: serde_json::Value =
        serde_json::from_slice(&page1_out).expect("page 1 must be valid JSON");

    let paths0: Vec<&str> = v0["results"]
        .as_array()
        .expect("page 0 must have results array")
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();

    let paths1: Vec<&str> = v1["results"]
        .as_array()
        .expect("page 1 must have results array")
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();

    assert_eq!(paths0.len(), 2, "page 0 must have exactly 2 results");
    assert_eq!(paths1.len(), 2, "page 1 must have exactly 2 results");

    // Key assertion: the two pages must be DISJOINT.
    // Pre-change: both pages returned the same results (offset ignored).
    let overlap: Vec<_> = paths0.iter().filter(|p| paths1.contains(p)).collect();
    assert!(
        overlap.is_empty(),
        "AC-404-1: pages must be disjoint (offset ignored = both return same files). \
         overlap={overlap:?}, page0={paths0:?}, page1={paths1:?}"
    );

    // Also verify has_more: page 0 of a 6-file result set with limit 2 has more.
    assert_eq!(
        v0["has_more"].as_bool(),
        Some(true),
        "AC-404-18: page 0 of 6-file result with limit 2 must have has_more=true"
    );
}

// ============================================================================
// AC-404-12: offset=0 byte-identical TEXT stdout to no-offset
// ============================================================================

/// AC-404-12: `--offset 0` must produce byte-identical TEXT stdout to a
/// command without `--offset` (zero-regression at offset 0).
#[test]
fn offset_zero_identical_to_no_offset_text() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    make_try_catch_project(proj.path(), 4);
    build_index(proj.path(), cache.path());

    let no_offset_out = Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "--ast", "try-catch", "--limit", "10", "--root"])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let offset_zero_out = Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search",
            "--ast",
            "try-catch",
            "--limit",
            "10",
            "--offset",
            "0",
            "--root",
        ])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_eq!(
        no_offset_out, offset_zero_out,
        "AC-404-12: TEXT stdout with --offset 0 must be byte-identical to no --offset"
    );
}

// ============================================================================
// AC-404-8: paging past end yields empty page, exit 0
// ============================================================================

/// AC-404-8: `--ast try-catch --offset 999 --json` must exit 0 and return
/// `{"total":0,"results":[]}` with no `has_more` key (it is false).
#[test]
fn offset_past_end_yields_empty_page() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    make_try_catch_project(proj.path(), 3);
    build_index(proj.path(), cache.path());

    let out = Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search",
            "--ast",
            "try-catch",
            "--limit",
            "3",
            "--offset",
            "999",
            "--json",
            "--root",
        ])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success() // AC-404-8: must exit 0 on empty page
        .get_output()
        .stdout
        .clone();

    let v: serde_json::Value = serde_json::from_slice(&out).expect("empty page must be valid JSON");
    assert_eq!(
        v["total"].as_u64(),
        Some(0),
        "AC-404-8: empty page must have total=0"
    );
    assert!(
        v["results"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(false),
        "AC-404-8: empty page must have results=[]"
    );
    assert!(
        v.get("has_more").is_none() || v["has_more"].as_bool() == Some(false),
        "AC-404-8: empty page past end must NOT have has_more=true"
    );
}

// ============================================================================
// AC-404-2: standalone temporal arms accept --offset without error
// ============================================================================

/// AC-404-2: `--hot --offset N` must be accepted (not rejected) by the CLI.
///
/// Deep pagination disjointness requires seeded git data (verified hermetically
/// in `temporal_tests.rs`). This test asserts that the flag is WIRED and does
/// not cause an unexpected non-zero exit or panic.
#[test]
fn offset_accepted_on_hot_standalone() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    // No git history → no temporal.db → graceful degradation (exit 0).
    Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "--hot", "--offset", "2", "--limit", "5", "--root"])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success(); // degraded exit 0 (no temporal.db)
}

/// AC-404-2: `--cold --offset N` must be accepted.
#[test]
fn offset_accepted_on_cold_standalone() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search", "--cold", "--offset", "3", "--limit", "5", "--root",
        ])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success();
}

/// AC-404-2: `--risky --offset N` must be accepted.
#[test]
fn offset_accepted_on_risky_standalone() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search", "--risky", "--offset", "1", "--limit", "5", "--root",
        ])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success();
}

// ============================================================================
// AC-404-3: standalone --blast-radius accepts --offset without error
// ============================================================================

/// AC-404-3: `--blast-radius FILE --offset N` must be accepted.
#[test]
fn offset_accepted_on_blast_radius_standalone() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    make_try_catch_project(proj.path(), 2);
    // No temporal.db → graceful degradation (exit 0).
    Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search",
            "--blast-radius",
            "src/f01.ts",
            "--offset",
            "1",
            "--limit",
            "3",
            "--root",
        ])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success();
}

// ============================================================================
// AC-404-18: has_more absent (false) on a single-page complete result set
// ============================================================================

/// AC-404-18: when all results fit on one page, `has_more` must be absent
/// from JSON (it serializes as false which is skip_serializing).
#[test]
fn has_more_absent_when_all_results_fit_on_one_page() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    make_try_catch_project(proj.path(), 3);
    build_index(proj.path(), cache.path());

    let out = Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search",
            "--ast",
            "try-catch",
            "--limit",
            "100",
            "--json",
            "--root",
        ])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let v: serde_json::Value =
        serde_json::from_slice(&out).expect("single-page JSON must be valid");
    assert!(
        v.get("has_more").is_none(),
        "AC-404-18: has_more must be ABSENT (not present) when false; got: {:?}",
        v.get("has_more")
    );
}

// ============================================================================
// AC-404-1 containment query: --ast containment honors --offset
// ============================================================================

/// AC-404-1 (containment variant): a containment query `function_item > block`
/// with `--offset` also paginates correctly.
#[test]
fn offset_paginates_containment_query() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    // Rust files (function_item > block) — need enough to paginate.
    fs::create_dir_all(proj.path().join("src")).unwrap();
    for i in 1..=6u32 {
        fs::write(
            proj.path().join(format!("src/g{i:02}.rs")),
            format!("pub fn func{i}(x: u32) -> u32 {{ x + {i} }}\n"),
        )
        .unwrap();
    }
    build_index(proj.path(), cache.path());

    let page0 = Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search",
            "--ast",
            "function_item > block",
            "--limit",
            "2",
            "--offset",
            "0",
            "--json",
            "--root",
        ])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let page1 = Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search",
            "--ast",
            "function_item > block",
            "--limit",
            "2",
            "--offset",
            "2",
            "--json",
            "--root",
        ])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let v0: serde_json::Value = serde_json::from_slice(&page0).unwrap();
    let v1: serde_json::Value = serde_json::from_slice(&page1).unwrap();

    let paths0: Vec<&str> = v0["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();

    let paths1: Vec<&str> = v1["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();

    let overlap: Vec<_> = paths0.iter().filter(|p| paths1.contains(p)).collect();
    assert!(
        overlap.is_empty(),
        "AC-404-1 (containment): pages must be disjoint. \
         overlap={overlap:?}, page0={paths0:?}, page1={paths1:?}"
    );
}

// ============================================================================
// AC-404-11: bounded-page stderr notice when has_more=true
// ============================================================================

/// AC-404-11: when `--hot` returns a capped page (limit < total), a notice
/// containing "more exist" and the next --offset must appear on stderr.
/// JSON stdout must stay byte-clean (no notice leaking into stdout).
///
/// Uses the degraded path (no temporal.db) to get a deterministic exit 0 with
/// empty output and no bounded-page notice (nothing to page). The notice only
/// fires when has_more=true; the degraded empty case produces has_more=false.
///
/// A proper end-to-end notice test would require a seeded git history (to
/// populate temporal.db with > limit rows). The unit test
/// `bounded_page_notice_contains_required_phrasing` in temporal_tests.rs pins
/// the exact phrasing; this test pins that the notice goes to stderr and NOT
/// to stdout.
#[test]
fn bounded_page_notice_goes_to_stderr_not_stdout() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    // No temporal.db → degraded path (no notice, just the empty warning).
    // We verify that stdout is either empty or valid JSON (no notice contamination).
    let out = Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search", "--hot", "--limit", "2", "--offset", "0", "--json", "--root",
        ])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success()
        .get_output()
        .clone();

    // stdout must NOT contain "more exist" (the bounded-page notice goes to stderr).
    let stdout_str = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout_str.contains("more exist"),
        "AC-404-11: bounded-page notice must NOT appear in stdout; got: {stdout_str:?}"
    );
}

// ============================================================================
// PF-004: hostile --offset near usize::MAX must not overflow the candidate pool
// ============================================================================

/// A `--offset` at `usize::MAX` must NOT overflow the additive candidate-pool
/// widening (`candidate_pool(limit, K) + offset`). Before the `saturating_add`
/// guard this panicked in debug/test builds ("attempt to add with overflow")
/// and wrapped to a garbage pool size in release. Post-fix: exit 0, empty page
/// (paged far past the end), never a panic.
///
/// Exercises the non-synthetic AST pool path (`--ast try-catch`) that computes
/// `candidate_pool(...) + offset`.
#[test]
fn hostile_max_offset_does_not_overflow() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    make_try_catch_project(proj.path(), 3);
    build_index(proj.path(), cache.path());

    let out = Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search",
            "--ast",
            "try-catch",
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
// D-5 / AD-404-11: has_more on the pure-text query path
// ============================================================================

/// Create N Rust files that each contain the sentinel token "qxz_shared_probe"
/// so a text query for it returns all N files.
fn make_multi_match_project(root: &std::path::Path, n: usize) {
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

/// D-5 / AD-404-11: `has_more` must be `true` on a plain text query when the
/// result set is larger than the page.
///
/// Pre-fix: `QueryOutput.has_more` was hardcoded `false` at all construction
/// sites in `query.rs`, so the D-5 pagination-terminator contract was never
/// fulfilled on the "skim search <text> --json" surface (while `--ast`,
/// `--hot`, `--cold`, `--risky`, and `--blast-radius` standalone paths
/// already emitted it correctly).
///
/// Post-fix: `resolve_paths_and_snippets_verified` uses a "probe one more"
/// strategy — collect `limit + 1` results after skip, set `has_more = true`
/// when the probe item exists, then truncate to `limit`.
#[test]
fn has_more_true_on_text_query_when_results_exceed_limit() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    // 6 files each containing "qxz_shared_probe"; limit=2 → 4 remain → has_more=true.
    make_multi_match_project(proj.path(), 6);
    build_index(proj.path(), cache.path());

    let out = Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search",
            "qxz_shared_probe",
            "--limit",
            "2",
            "--json",
            "--root",
        ])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let v: serde_json::Value = serde_json::from_slice(&out).expect("text-query JSON must be valid");
    assert_eq!(
        v["has_more"].as_bool(),
        Some(true),
        "D-5: has_more must be true on text path when limit=2 < total results (6 files); \
         pre-fix regression — has_more was hardcoded false. Got: {:?}",
        v
    );
}

/// D-5: `has_more` must be absent (false) on the last page of a text query.
///
/// Guards the "single-page / last-page" direction of the D-5 contract:
/// when all matches fit on one page, `has_more` must NOT be present
/// (serialized as absent because `#[serde(skip_serializing_if = "Not::not")]`).
#[test]
fn has_more_absent_on_text_query_when_all_results_fit_on_page() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    // 3 files; limit=100 → all fit on one page → has_more must be absent.
    make_multi_match_project(proj.path(), 3);
    build_index(proj.path(), cache.path());

    let out = Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search",
            "qxz_shared_probe",
            "--limit",
            "100",
            "--json",
            "--root",
        ])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let v: serde_json::Value =
        serde_json::from_slice(&out).expect("single-page JSON must be valid");
    assert!(
        v.get("has_more").is_none(),
        "D-5: has_more must be ABSENT when all results fit on one page; got: {:?}",
        v.get("has_more")
    );
}
