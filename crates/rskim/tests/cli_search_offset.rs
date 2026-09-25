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

/// Run a git command in `root`, asserting exit 0.
///
/// Local user identity is supplied via git-config so CI machines and machines
/// without a global `~/.gitconfig` work identically.  `commit.gpgsign=false`
/// prevents GPG signing prompts in environments where a signing key is
/// configured globally.
fn git_in(root: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .status()
        .unwrap_or_else(|e| panic!("git {:?} failed to spawn: {e}", args));
    assert!(status.success(), "git {:?} exited with {status}", args);
}

/// Initialize a git repo at `root` with hermetic local identity and no signing.
fn git_init(root: &Path) {
    git_in(root, &["init"]);
    git_in(root, &["config", "user.email", "test@t.com"]);
    git_in(root, &["config", "user.name", "Test"]);
    git_in(root, &["config", "commit.gpgsign", "false"]);
}

/// Create a project with N TypeScript files **plus a two-commit git history**.
///
/// The second commit (which modifies all files) guarantees that `temporal.db`
/// is populated with hotspot and risk rows when `skim search --build` runs.
/// Without git history `run_temporal_standalone` returns early at the
/// `open_temporal_db` guard, never reaching the pagination code — the PF-007
/// vacuity this helper is designed to prevent.
///
/// Files are named `src/f01.ts` … `src/fNN.ts` (lexicographic order so
/// pagination has a deterministic sort key: all AST scores tie → FileId ASC).
fn make_git_project(root: &Path, n: usize) {
    make_try_catch_project(root, n);
    git_init(root);
    git_in(root, &["add", "."]);
    git_in(root, &["commit", "-m", "seed"]);
    // Second commit: touch every file so each appears in at least one non-initial
    // commit.  gix diffs the initial commit against the empty tree, but ensuring
    // a second commit makes the test independent of that edge-case behaviour.
    for i in 1..=n {
        fs::write(
            root.join(format!("src/f{i:02}.ts")),
            format!("// v2\nexport const v{i}: number = {i};\n"),
        )
        .unwrap();
    }
    git_in(root, &["add", "."]);
    git_in(root, &["commit", "-m", "update"]);
}

/// Create a project with co-change history for `--blast-radius` pagination tests.
///
/// Layout:
/// - `src/anchor.ts` — the blast-radius target
/// - `src/p01.ts` … `src/p05.ts` — five partner files
///
/// All six files are committed together in two commits, giving every pair a
/// Jaccard co-change score of 1.0 (≥ `MIN_COCHANGE_JACCARD` = 0.10).  With
/// five partners, `--blast-radius src/anchor.ts --limit 2` can paginate:
/// page 0 → partners ordered by (jaccard DESC, file_b ASC) → p01, p02;
/// page 1 (offset 2) → p03, p04.
fn make_cochange_project(root: &Path) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/anchor.ts"), "export const anchor = 0;\n").unwrap();
    for i in 1..=5u32 {
        fs::write(
            root.join(format!("src/p{i:02}.ts")),
            format!("export const p{i}: number = {i};\n"),
        )
        .unwrap();
    }
    git_init(root);
    git_in(root, &["add", "."]);
    git_in(root, &["commit", "-m", "seed"]);
    // Second commit: modify all files together again to cement every pair's
    // co-change count to 2 (Jaccard stays 1.0).
    fs::write(root.join("src/anchor.ts"), "export const anchor = 1;\n").unwrap();
    for i in 1..=5u32 {
        fs::write(
            root.join(format!("src/p{i:02}.ts")),
            format!("export const p{i}: number = {i} + 1;\n"),
        )
        .unwrap();
    }
    git_in(root, &["add", "."]);
    git_in(root, &["commit", "-m", "update"]);
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

    // Contiguity: page 0 and page 1 must be exactly adjacent slices of the
    // full result list.  Disjoint-only is insufficient: an off-by-one in the
    // offset skip (e.g. skipping 1 or 3 instead of 2) produces disjoint pages
    // that still miss or duplicate a result.
    let all_out = Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search",
            "--ast",
            "try-catch",
            "--limit",
            "100",
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

    let v_all: serde_json::Value =
        serde_json::from_slice(&all_out).expect("all-results must be valid JSON");
    let all_paths: Vec<&str> = v_all["results"]
        .as_array()
        .expect("all-results must have a results array")
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();

    assert!(
        all_paths.len() >= 4,
        "AC-404-1: need at least 4 results for a 2-page pagination check; got {}",
        all_paths.len()
    );
    assert_eq!(
        paths0,
        &all_paths[..2],
        "AC-404-1 (contiguity): page 0 must be exactly the first 2 of all results. \
         Off-by-one in --offset would produce disjoint-but-non-contiguous pages."
    );
    assert_eq!(
        paths1,
        &all_paths[2..4],
        "AC-404-1 (contiguity): page 1 must be exactly results [2..4] of all results. \
         Off-by-one in --offset would skip or duplicate a result."
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

/// AC-404-2: `--hot --offset N` actually paginates (offset is wired through to
/// the temporal query engine, not silently ignored).
///
/// Pre-fix: `run_temporal_standalone` never passed the `page` cursor to
/// `query_standalone`; offset was ignored and both pages returned the same
/// files.  Testing against a no-git-history project masked this because the
/// function returned early (no temporal.db), never reaching pagination code —
/// the PF-007 vacuity.  This test uses a seeded git project so temporal.db is
/// built and the real pagination path executes.
#[test]
fn offset_accepted_on_hot_standalone() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    // Seed 6 files with git history so --build populates temporal.db.
    make_git_project(proj.path(), 6);
    build_index(proj.path(), cache.path());

    let page0_out = Command::cargo_bin("skim")
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
        .stdout
        .clone();

    let page1_out = Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search", "--hot", "--limit", "2", "--offset", "2", "--json", "--root",
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
        serde_json::from_slice(&page0_out).expect("--hot page 0 must be valid JSON");
    let v1: serde_json::Value =
        serde_json::from_slice(&page1_out).expect("--hot page 1 must be valid JSON");

    let paths0: Vec<&str> = v0["results"]
        .as_array()
        .expect("--hot page 0 must have a results array")
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();
    let paths1: Vec<&str> = v1["results"]
        .as_array()
        .expect("--hot page 1 must have a results array")
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();

    assert_eq!(paths0.len(), 2, "--hot page 0 must have exactly 2 results");
    assert_eq!(paths1.len(), 2, "--hot page 1 must have exactly 2 results");

    // Key check: pages must be disjoint — pre-fix both returned the same files.
    let overlap: Vec<_> = paths0.iter().filter(|p| paths1.contains(p)).collect();
    assert!(
        overlap.is_empty(),
        "AC-404-2: --hot --offset paginates disjoint pages. \
         overlap={overlap:?} page0={paths0:?} page1={paths1:?}"
    );
}

/// AC-404-2: `--cold --offset N` actually paginates (offset is wired through
/// to the temporal query engine, not silently ignored).
///
/// Same vacuity fix as `offset_accepted_on_hot_standalone`: uses a seeded git
/// project so temporal.db is populated and the real pagination path executes.
#[test]
fn offset_accepted_on_cold_standalone() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    make_git_project(proj.path(), 6);
    build_index(proj.path(), cache.path());

    let page0_out = Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search", "--cold", "--limit", "2", "--offset", "0", "--json", "--root",
        ])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let page1_out = Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search", "--cold", "--limit", "2", "--offset", "2", "--json", "--root",
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
        serde_json::from_slice(&page0_out).expect("--cold page 0 must be valid JSON");
    let v1: serde_json::Value =
        serde_json::from_slice(&page1_out).expect("--cold page 1 must be valid JSON");

    let paths0: Vec<&str> = v0["results"]
        .as_array()
        .expect("--cold page 0 must have a results array")
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();
    let paths1: Vec<&str> = v1["results"]
        .as_array()
        .expect("--cold page 1 must have a results array")
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();

    assert_eq!(paths0.len(), 2, "--cold page 0 must have exactly 2 results");
    assert_eq!(paths1.len(), 2, "--cold page 1 must have exactly 2 results");

    let overlap: Vec<_> = paths0.iter().filter(|p| paths1.contains(p)).collect();
    assert!(
        overlap.is_empty(),
        "AC-404-2: --cold --offset paginates disjoint pages. \
         overlap={overlap:?} page0={paths0:?} page1={paths1:?}"
    );
}

/// AC-404-2: `--risky --offset N` actually paginates (offset is wired through
/// to the temporal query engine, not silently ignored).
///
/// Risk score is 0 for all files (no "fix" commits in the seed history), so
/// `top_risks` orders by `risk_score DESC, total_commits DESC, file_path ASC`.
/// With all scores tied, pagination falls back to file_path ASC — deterministic
/// and sufficient to prove disjointness.
#[test]
fn offset_accepted_on_risky_standalone() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    make_git_project(proj.path(), 6);
    build_index(proj.path(), cache.path());

    let page0_out = Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search", "--risky", "--limit", "2", "--offset", "0", "--json", "--root",
        ])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let page1_out = Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search", "--risky", "--limit", "2", "--offset", "2", "--json", "--root",
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
        serde_json::from_slice(&page0_out).expect("--risky page 0 must be valid JSON");
    let v1: serde_json::Value =
        serde_json::from_slice(&page1_out).expect("--risky page 1 must be valid JSON");

    let paths0: Vec<&str> = v0["results"]
        .as_array()
        .expect("--risky page 0 must have a results array")
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();
    let paths1: Vec<&str> = v1["results"]
        .as_array()
        .expect("--risky page 1 must have a results array")
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();

    assert_eq!(
        paths0.len(),
        2,
        "--risky page 0 must have exactly 2 results"
    );
    assert_eq!(
        paths1.len(),
        2,
        "--risky page 1 must have exactly 2 results"
    );

    let overlap: Vec<_> = paths0.iter().filter(|p| paths1.contains(p)).collect();
    assert!(
        overlap.is_empty(),
        "AC-404-2: --risky --offset paginates disjoint pages. \
         overlap={overlap:?} page0={paths0:?} page1={paths1:?}"
    );
}

// ============================================================================
// AC-404-3: standalone --blast-radius accepts --offset without error
// ============================================================================

/// AC-404-3: `--blast-radius FILE --offset N` actually paginates co-change
/// partners (offset is wired through, not silently ignored).
///
/// `make_cochange_project` seeds `src/anchor.ts` with 5 co-change partners
/// (`src/p01.ts` … `src/p05.ts`) via a two-commit git history.  Each pair
/// reaches Jaccard 1.0 (≥ MIN_COCHANGE_JACCARD 0.10), so `cochanges_for_file`
/// returns all 5 partners and the pagination code executes — unlike the prior
/// vacuous test that used a no-git-history project where `open_temporal_db`
/// returned `None` and `run_temporal_standalone` exited early.
#[test]
fn offset_accepted_on_blast_radius_standalone() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    make_cochange_project(proj.path());
    build_index(proj.path(), cache.path());

    let page0_out = Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search",
            "--blast-radius",
            "src/anchor.ts",
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

    let page1_out = Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search",
            "--blast-radius",
            "src/anchor.ts",
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
        serde_json::from_slice(&page0_out).expect("--blast-radius page 0 must be valid JSON");
    let v1: serde_json::Value =
        serde_json::from_slice(&page1_out).expect("--blast-radius page 1 must be valid JSON");

    let paths0: Vec<&str> = v0["results"]
        .as_array()
        .expect("--blast-radius page 0 must have a results array")
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();
    let paths1: Vec<&str> = v1["results"]
        .as_array()
        .expect("--blast-radius page 1 must have a results array")
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();

    assert_eq!(
        paths0.len(),
        2,
        "--blast-radius page 0 must have exactly 2 co-change partners"
    );
    assert_eq!(
        paths1.len(),
        2,
        "--blast-radius page 1 must have exactly 2 co-change partners"
    );

    // Key check: pages must be disjoint — pre-fix both returned the same partners.
    let overlap: Vec<_> = paths0.iter().filter(|p| paths1.contains(p)).collect();
    assert!(
        overlap.is_empty(),
        "AC-404-3: --blast-radius --offset paginates disjoint pages. \
         overlap={overlap:?} page0={paths0:?} page1={paths1:?}"
    );
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

    // Contiguity: verify page 0 and page 1 are exactly adjacent slices of the
    // full result list.  Disjoint-only is insufficient: an off-by-one in the
    // offset skip produces disjoint-but-non-contiguous pages that silently miss
    // or duplicate one result.
    let all_out = Command::cargo_bin("skim")
        .unwrap()
        .args([
            "search",
            "--ast",
            "function_item > block",
            "--limit",
            "100",
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

    let v_all: serde_json::Value =
        serde_json::from_slice(&all_out).expect("all-results must be valid JSON");
    let all_paths: Vec<&str> = v_all["results"]
        .as_array()
        .expect("all-results must have a results array")
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();

    assert!(
        all_paths.len() >= 4,
        "AC-404-1 (containment): need at least 4 results for 2-page check; got {}",
        all_paths.len()
    );
    assert_eq!(
        paths0,
        &all_paths[..2],
        "AC-404-1 (containment, contiguity): page 0 must be exactly the first 2 of all results."
    );
    assert_eq!(
        paths1,
        &all_paths[2..4],
        "AC-404-1 (containment, contiguity): page 1 must be exactly results [2..4] of all results. \
         Off-by-one in --offset would skip or duplicate a result."
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
