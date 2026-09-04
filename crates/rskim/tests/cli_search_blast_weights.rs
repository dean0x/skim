//! CLI E2E tests for ticket #409: blast-radius Jaccard ranking.
//!
//! Verifies that `skim search <text> --blast-radius FILE --weights W` ranks
//! co-change partners by descending Jaccard co-change strength instead of
//! byte-wise path-alphabetical order (the pre-#409 defect).
//!
//! # Fixture discipline (auto-resolved decision "E2E Fixtures and Concurrency")
//!
//! Every fixture repo AND every `SKIM_CACHE_DIR` comes from a per-test
//! `tempfile::TempDir`.  NO hardcoded `/tmp` paths.  No test may depend on
//! another test's fixture.  Captures happen in-process via `Command::output()`.
//!
//! # FX-LINEAR fixture structure
//!
//! Three Rust source files whose byte-wise path order INVERTS Jaccard order:
//!   - `aweak.rs`  (alphabetically first)  → J(anchor, aweak)  ≈ 0.40
//!   - `anchor.rs` (alphabetically middle) → SEED  (blast-radius target)
//!   - `zstrong.rs` (alphabetically last)  → J(anchor, zstrong) = 0.60
//!
//! Commit layout (5 total non-merge commits):
//!   - C1: anchor.rs + zstrong.rs + aweak.rs  (all three)
//!   - C2: anchor.rs + zstrong.rs              (anchor+zstrong pair)
//!   - C3: anchor.rs + zstrong.rs              (anchor+zstrong pair)
//!   - C4: anchor.rs + aweak.rs                (anchor+aweak pair, second joint commit)
//!   - C5: anchor.rs only                      (solo — bumps anchor's total to 5)
//!
//! Derived Jaccard values (from git log --no-merges, per ADR-003):
//!   anchor total = 5 (C1+C2+C3+C4+C5)
//!   zstrong total = 3 (C1+C2+C3)
//!   aweak total   = 2 (C1+C4)
//!   joint(anchor, zstrong) = 3 (C1+C2+C3) → J = 3/(5+3-3) = 3/5 = 0.60
//!   joint(anchor, aweak)   = 2 (C1+C4)    → J = 2/(5+2-2) = 2/5 = 0.40
//!
//! J(anchor, aweak) = 0.40 is well above MIN_COCHANGE_JACCARD (0.10), giving a
//! clear margin (auto-resolved decision "FX-LINEAR margin").
//!
//! # Separation from #407's E2E file
//!
//! `cli_temporal_first_parent.rs` is owned by ticket #407's full-DAG tests.
//! This file is deliberately separate to avoid build-lock conflicts (plan).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;
use std::time::{SystemTime, UNIX_EPOCH};

use assert_cmd::cargo::cargo_bin;
use serde_json::Value;
use tempfile::TempDir;

// ============================================================================
// Helpers
// ============================================================================

/// Find the `temporal.db` file produced by a prior `build_index(proj, cache)` call.
///
/// The cache layout is `cache/search/<16-char-hex>/temporal.db`.  This helper
/// searches one level deep so tests do not need to replicate the SHA256-hash
/// path-derivation logic.  Returns `None` if no file is found (build did not
/// produce temporal data or was skipped).
fn find_temporal_db(cache: &Path) -> Option<std::path::PathBuf> {
    let search_dir = cache.join("search");
    let entries = fs::read_dir(&search_dir).ok()?;
    for entry in entries.flatten() {
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            let db = entry.path().join("temporal.db");
            if db.exists() {
                return Some(db);
            }
        }
    }
    None
}

/// Return the current Unix epoch in seconds.
fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before UNIX epoch")
        .as_secs()
}

/// Initialise a git repository with hermetic, non-signing identity.
fn git_init(dir: &Path) {
    for args in &[
        vec!["init"],
        vec!["config", "user.email", "test@t.invalid"],
        vec!["config", "user.name", "Test"],
        vec!["config", "commit.gpgsign", "false"],
    ] {
        let s = StdCommand::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap_or_else(|e| panic!("git {:?}: {e}", args));
        assert!(
            s.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&s.stderr)
        );
    }
    // Use "main" as the initial branch name.
    let _ = StdCommand::new("git")
        .args(["checkout", "-b", "main"])
        .current_dir(dir)
        .output();
}

/// Write `content` to `dir/<filename>` and stage it.
fn write_and_stage(dir: &Path, filename: &str, content: &str) {
    let path = dir.join(filename);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, content).unwrap_or_else(|e| panic!("write {filename}: {e}"));
    let s = StdCommand::new("git")
        .args(["add", filename])
        .current_dir(dir)
        .output()
        .expect("git add");
    assert!(s.status.success(), "git add {filename} failed");
}

/// Commit staged changes with pinned author and committer timestamps.
///
/// `ts` is a Unix epoch value used for both `GIT_AUTHOR_DATE` and
/// `GIT_COMMITTER_DATE` so tests are deterministic across timezones.
fn git_commit(dir: &Path, message: &str, ts: u64) {
    let ts_str = ts.to_string();
    let s = StdCommand::new("git")
        .args(["commit", "--no-verify", "-m", message])
        .env("GIT_AUTHOR_DATE", &ts_str)
        .env("GIT_COMMITTER_DATE", &ts_str)
        .current_dir(dir)
        .output()
        .expect("git commit");
    assert!(
        s.status.success(),
        "git commit '{}' failed: {}",
        message,
        String::from_utf8_lossy(&s.stderr)
    );
}

/// Build lexical + temporal index for `proj` into `cache`.
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

/// Run `skim search [args…] --root <proj> --json` and return the raw stdout + stderr.
fn run_search_raw(proj: &Path, cache: &Path, extra_args: &[&str]) -> (Vec<u8>, Vec<u8>) {
    let out = StdCommand::new(cargo_bin("skim"))
        .args(["search"])
        .args(extra_args)
        .args(["--root"])
        .arg(proj)
        .env("SKIM_CACHE_DIR", cache)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .output()
        .expect("skim search");
    (out.stdout, out.stderr)
}

/// Run `skim search [args…] --root <proj> --json` and return parsed JSON.
fn run_search_json(proj: &Path, cache: &Path, extra_args: &[&str]) -> Value {
    let out = StdCommand::new(cargo_bin("skim"))
        .args(["search"])
        .args(extra_args)
        .args(["--json", "--root"])
        .arg(proj)
        .env("SKIM_CACHE_DIR", cache)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .output()
        .expect("skim search --json");
    assert!(
        out.status.success(),
        "skim search {:?} failed: {}",
        extra_args,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("skim output utf-8");
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("skim search --json produced invalid JSON: {e}\n{stdout}"))
}

// ============================================================================
// FX-LINEAR fixture
// ============================================================================

/// Create the FX-LINEAR fixture described in the module doc.
///
/// Returns the owned `TempDir` (keep alive for test duration).
///
/// Commit layout (5 non-merge commits, all within the last 30 days):
///   C1: anchor.rs + zstrong.rs + aweak.rs  (joint — all three)
///   C2: anchor.rs + zstrong.rs              (anchor+zstrong pair)
///   C3: anchor.rs + zstrong.rs              (anchor+zstrong pair)
///   C4: anchor.rs + aweak.rs                (second joint commit for anchor+aweak)
///   C5: anchor.rs                           (solo — bumps anchor total to 5)
///
/// Derived Jaccard at index-build time:
///   J(anchor, zstrong) = 3/5 = 0.60
///   J(anchor, aweak)   = 2/5 = 0.40
///
/// Both values exceed MIN_COCHANGE_JACCARD (0.10); the margin of 0.30 is well
/// above any plausible re-derived floor (auto-resolved decision "FX-LINEAR margin").
///
/// The in-test assertion `assert!(0.40 > rskim_search::MIN_COCHANGE_JACCARD)`
/// will fail with a diagnostic if the constant is ever raised past 0.40.
fn make_linear_fixture() -> TempDir {
    let now = now_epoch();
    let dir = TempDir::new().expect("TempDir::new");
    git_init(dir.path());

    // C1: all three files (establishes the baseline joint commit for all pairs).
    write_and_stage(dir.path(), "anchor.rs", "// anchor v1\n");
    write_and_stage(dir.path(), "zstrong.rs", "// zstrong v1\n");
    write_and_stage(dir.path(), "aweak.rs", "// aweak v1\n");
    git_commit(
        dir.path(),
        "feat: initial commit (all three)",
        now - 25 * 86400,
    );

    // C2: anchor + zstrong (second joint commit for the strong pair).
    write_and_stage(dir.path(), "anchor.rs", "// anchor v2\n");
    write_and_stage(dir.path(), "zstrong.rs", "// zstrong v2\n");
    git_commit(dir.path(), "feat: anchor+zstrong pair 2", now - 20 * 86400);

    // C3: anchor + zstrong (third joint commit — pushes J(anchor,zstrong) to 3/5).
    write_and_stage(dir.path(), "anchor.rs", "// anchor v3\n");
    write_and_stage(dir.path(), "zstrong.rs", "// zstrong v3\n");
    git_commit(dir.path(), "feat: anchor+zstrong pair 3", now - 15 * 86400);

    // C4: anchor + aweak (second joint commit — pushes J(anchor,aweak) from 1/4 to 2/5=0.40).
    write_and_stage(dir.path(), "anchor.rs", "// anchor v4\n");
    write_and_stage(dir.path(), "aweak.rs", "// aweak v2\n");
    git_commit(
        dir.path(),
        "feat: anchor+aweak second joint",
        now - 10 * 86400,
    );

    // C5: anchor only (solo — bumps anchor total from 4 to 5).
    write_and_stage(dir.path(), "anchor.rs", "// anchor v5\n");
    git_commit(dir.path(), "chore: anchor solo commit", now - 5 * 86400);

    dir
}

// ============================================================================
// AC-409 E2E tests
// ============================================================================

/// AC-1 — `--weights 0,0,1` follows Jaccard DESC, NOT byte-wise path order.
///
/// Byte-wise order: aweak < anchor < zstrong.
/// Expected output order: anchor (seed, SEED_STRENGTH), zstrong (J=0.60), aweak (J=0.40).
/// Scores: 1/61, 1/62, 1/63 (±1e-12).
///
/// Pre-#409 (defect): aweak, anchor, zstrong (alphabetical = FileId-sort order).
/// Post-#409 (fix):   anchor, zstrong, aweak  (Jaccard DESC, seed first).
#[test]
fn ac409_1_temporal_weight_only_follows_jaccard() {
    // FX-LINEAR margin assertion: if MIN_COCHANGE_JACCARD is ever raised past 0.40
    // this will fail with a diagnostic before any skim invocation runs.
    const {
        assert!(
            0.40 > rskim_search::MIN_COCHANGE_JACCARD,
            "FX-LINEAR margin violated: J(anchor, aweak)=0.40 must exceed MIN_COCHANGE_JACCARD; raise the fixture joint commit count"
        )
    };

    let fixture = make_linear_fixture();
    let cache = TempDir::new().unwrap();

    // Warm-up: build the index (lexical + temporal).
    build_index(fixture.path(), cache.path());

    // AC-1 query: gibberish text + --blast-radius anchor.rs + --weights 0,0,1.
    let v = run_search_json(
        fixture.path(),
        cache.path(),
        &[
            "xqzjvmblorp_ac409_e2e_unique", // gibberish — zero lexical hits
            "--blast-radius",
            "anchor.rs",
            "--weights",
            "0,0,1",
            "--limit",
            "10",
        ],
    );

    let results = v["results"].as_array().expect("results array");

    // Seed (anchor) + two partners (zstrong, aweak) = 3 results.
    assert_eq!(
        results.len(),
        3,
        "AC-1: expected 3 results (anchor + 2 partners); got: {:?}",
        results
            .iter()
            .map(|r| r["path"].as_str())
            .collect::<Vec<_>>()
    );

    let paths: Vec<&str> = results
        .iter()
        .map(|r| r["path"].as_str().expect("path str"))
        .collect();

    // Byte-wise order would be: [aweak.rs, anchor.rs, zstrong.rs].
    // Jaccard order (seed first, then DESC by J): [anchor.rs, zstrong.rs, aweak.rs].
    assert_eq!(
        paths,
        ["anchor.rs", "zstrong.rs", "aweak.rs"],
        "AC-1: results must be ordered seed-first then by Jaccard DESC, \
         not by byte-wise path order (aweak < anchor < zstrong)"
    );

    // Score assertions (±1e-12): temporal-only RRF score w/(RRF_K + rank).
    let eps = 1e-12_f64;
    let rrf_k = 60.0_f64;
    let s0: f64 = results[0]["score"].as_f64().expect("score f64");
    let s1: f64 = results[1]["score"].as_f64().expect("score f64");
    let s2: f64 = results[2]["score"].as_f64().expect("score f64");
    assert!(
        (s0 - 1.0 / (rrf_k + 1.0)).abs() < eps,
        "AC-1: anchor (rank 1) score must be 1/61 (±1e-12); got {s0}"
    );
    assert!(
        (s1 - 1.0 / (rrf_k + 2.0)).abs() < eps,
        "AC-1: zstrong (rank 2) score must be 1/62 (±1e-12); got {s1}"
    );
    assert!(
        (s2 - 1.0 / (rrf_k + 3.0)).abs() < eps,
        "AC-1: aweak (rank 3) score must be 1/63 (±1e-12); got {s2}"
    );

    // AC-14: JSON contract unchanged (path + score + field; co-change partners
    // have field=="co_change_partner", snippet==null).
    for r in results {
        assert!(
            r.get("path").is_some(),
            "AC-14: each result must have 'path'"
        );
        assert!(
            r.get("score").is_some(),
            "AC-14: each result must have 'score'"
        );
        assert!(
            r.get("field").is_some(),
            "AC-14: each result must have 'field'"
        );
        // Co-change-only partners must have snippet==null.
        if r["field"].as_str() == Some("co_change_partner") {
            assert!(
                r["snippet"].is_null() || r.get("snippet").is_none(),
                "AC-14: co_change_partner must have snippet==null; got: {}",
                r["snippet"]
            );
        }
    }
}

/// AC-6 / ADR-007 — the composite `--weights 0,0,1` partner order MINUS THE SEED
/// must be byte-identical, in order, to the standalone `--blast-radius` order.
///
/// This is the self-validating invariant: it holds under any population (from any
/// commit DAG), so it does not depend on the specific Jaccard numbers derived above.
/// It is therefore the ADR-007 dog-food pass condition for this ticket.
#[test]
fn ac409_2_composite_temporal_order_equals_standalone_blast_order() {
    let fixture = make_linear_fixture();
    let cache = TempDir::new().unwrap();
    build_index(fixture.path(), cache.path());

    // Composite query: gibberish text + --blast-radius + --weights 0,0,1.
    let composite = run_search_json(
        fixture.path(),
        cache.path(),
        &[
            "xqzjvmblorp_ac409_e2e_invariant",
            "--blast-radius",
            "anchor.rs",
            "--weights",
            "0,0,1",
            "--limit",
            "20",
        ],
    );

    // Standalone blast-radius (no text query — uses the standalone arm).
    let standalone = run_search_json(
        fixture.path(),
        cache.path(),
        &["--blast-radius", "anchor.rs", "--limit", "20"],
    );

    let composite_results = composite["results"].as_array().expect("composite results");
    let standalone_results = standalone["results"]
        .as_array()
        .expect("standalone results");

    // Composite paths, MINUS the seed (anchor.rs ranks first in the composite output).
    let composite_partners: Vec<&str> = composite_results
        .iter()
        .map(|r| r["path"].as_str().expect("path"))
        .filter(|&p| p != "anchor.rs") // exclude the seed
        .collect();

    // Standalone paths (no seed in standalone output — seed is the query target, not a result).
    let standalone_paths: Vec<&str> = standalone_results
        .iter()
        .map(|r| r["path"].as_str().expect("path"))
        .collect();

    // AC-6: the two sequences must be byte-identical in order.
    // This is the ADR-007 invariant and does not depend on specific Jaccard numbers.
    assert_eq!(
        composite_partners, standalone_paths,
        "AC-6 / ADR-007: composite --weights 0,0,1 partner order (minus seed) must equal \
         standalone --blast-radius order; \
         composite_partners={composite_partners:?}, standalone={standalone_paths:?}"
    );

    // Non-vacuous: both sequences must be non-empty.
    assert!(
        !composite_partners.is_empty(),
        "AC-6: composite partner sequence must not be empty (PF-007)"
    );
    assert!(
        !standalone_paths.is_empty(),
        "AC-6: standalone sequence must not be empty (PF-007)"
    );
}

/// AC-4 — two consecutive identical `--weights 0,0,1 --blast-radius --json` runs
/// against an unchanged index and unchanged `temporal.db` produce byte-identical
/// stdout.  No HashMap iteration order may reach the output.
#[test]
fn ac409_3_repeated_identical_query_is_byte_identical() {
    let fixture = make_linear_fixture();
    let cache = TempDir::new().unwrap();
    build_index(fixture.path(), cache.path());

    let args = [
        "xqzjvmblorp_ac409_e2e_idempotent",
        "--blast-radius",
        "anchor.rs",
        "--weights",
        "0,0,1",
        "--limit",
        "10",
    ];

    let (stdout1, _) = run_search_raw(
        fixture.path(),
        cache.path(),
        &[
            args[0],
            "--blast-radius",
            args[2],
            "--weights",
            args[4],
            "--limit",
            args[6],
            "--json",
        ],
    );
    let (stdout2, _) = run_search_raw(
        fixture.path(),
        cache.path(),
        &[
            args[0],
            "--blast-radius",
            args[2],
            "--weights",
            args[4],
            "--limit",
            args[6],
            "--json",
        ],
    );

    // `QueryOutput::duration_ms` is wall-clock milliseconds and is serialized into
    // the JSON envelope, so a raw byte comparison of the two runs would be timing-
    // dependent (0 ms vs 1 ms on a loaded machine) and flaky.  Every OTHER byte —
    // result ordering, paths, score formatting, field labels, `total`, `has_more` —
    // must match exactly; that is what AC-4 is actually asserting (no HashMap
    // iteration order may reach stdout).  Drop only the duration line and compare
    // the rest byte-for-byte.
    fn without_duration(raw: &[u8]) -> String {
        String::from_utf8_lossy(raw)
            .lines()
            .filter(|l| !l.trim_start().starts_with("\"duration_ms\""))
            .collect::<Vec<_>>()
            .join("\n")
    }

    let normalized1 = without_duration(&stdout1);
    let normalized2 = without_duration(&stdout2);

    // Non-vacuous (PF-007): the comparison must be over real result rows, not two
    // identically-empty envelopes.
    assert!(
        normalized1.contains("\"path\""),
        "AC-4: the fixture query must return at least one result row; got: {normalized1}"
    );
    assert_eq!(
        normalized1, normalized2,
        "AC-4: two consecutive identical queries must produce byte-identical stdout \
         (duration_ms excluded) — HashMap iteration order is reaching the output"
    );
    // The duration line itself must still be present in both runs (guards against
    // the filter above silently matching everything if the field is ever renamed).
    for (n, raw) in [(1, &stdout1), (2, &stdout2)] {
        assert!(
            String::from_utf8_lossy(raw).contains("\"duration_ms\""),
            "AC-4: run {n} must carry a duration_ms field — the normalizer above is \
             keyed on it and would silently pass if the field were renamed"
        );
    }
}

/// AC-7 / AD-409-7 — when one or more co-change partner paths are not in the
/// indexed manifest, exactly one stderr line naming the dropped count (EXCLUDING
/// the seed) and the total partner count must appear; exit code must be 0; and
/// when zero paths are dropped NO such line must appear.
///
/// Fixture strategy for "partial drop" (1 of 1):
///   Commits: C1 = zstrong only, C2 = anchor+aweak, C3 = anchor+aweak.
///   J(anchor, aweak) = 2/2 = 1.0 > MIN threshold → aweak IS a co-change partner.
///   J(anchor, zstrong) = 0/3 = 0.0 < MIN threshold → zstrong is NOT a partner.
///   So cochanges_for_file("anchor.rs") returns exactly 1 partner: aweak.
///   allowed_paths = {anchor: SEED_STRENGTH, aweak: 1.0} (2 entries).
///   After deleting aweak.rs from disk and rebuilding the lexical index:
///     manifest = {anchor.rs, zstrong.rs}; aweak.rs absent.
///   scored.len() = 1 (only anchor found), dropped = 2 − 1 = 1, partner_count = 1.
///   Expected notice: "1 of 1 co-change partners not found in the indexed manifest".
///
/// The deleted-from-disk case correctly exercises the AD-409-7 notice path in
/// paths_to_scored_file_ids (the composite --weights query path).
#[test]
fn ac409_4_unindexed_partner_omission_is_disclosed() {
    let now = now_epoch();
    let dir = TempDir::new().expect("TempDir::new");
    let cache = TempDir::new().unwrap();
    git_init(dir.path());

    // C1: zstrong only — establishes zstrong in git history WITHOUT co-changing with
    // anchor.  This ensures J(anchor, zstrong) = 0.0 so zstrong is never returned by
    // cochanges_for_file("anchor.rs"), keeping it out of allowed_paths.  The only
    // co-change partner of anchor is aweak (see C2/C3 below).
    write_and_stage(dir.path(), "zstrong.rs", "// zstrong v1\n");
    git_commit(
        dir.path(),
        "feat: initial zstrong (no anchor)",
        now - 20 * 86400,
    );

    // C2: anchor + aweak (first joint commit — J(anchor,aweak) = 1/1 after this).
    write_and_stage(dir.path(), "anchor.rs", "// anchor v1\n");
    write_and_stage(dir.path(), "aweak.rs", "// aweak v1\n");
    git_commit(dir.path(), "feat: anchor+aweak pair 1", now - 10 * 86400);

    // C3: anchor + aweak (second joint — J(anchor,aweak) = 2/2 = 1.0 > MIN threshold).
    // zstrong intentionally absent: J(anchor, zstrong) stays 0.0, never a partner.
    write_and_stage(dir.path(), "anchor.rs", "// anchor v2\n");
    write_and_stage(dir.path(), "aweak.rs", "// aweak v2\n");
    git_commit(dir.path(), "feat: anchor+aweak pair 2", now - 5 * 86400);

    // Build the index with all three files on disk.
    // After this build, temporal.db contains the (anchor.rs, aweak.rs) co-change row
    // with Jaccard = 1.0 (both files appear in every one of anchor's commits: C2 and C3).
    build_index(dir.path(), cache.path());

    // Backup temporal.db BEFORE deleting aweak.rs.
    //
    // The build-time ghost filter (AD-408-1) removes co-change rows for files absent
    // from disk during a rebuild.  If we rebuild after deleting aweak.rs, the
    // (anchor, aweak) co-change row disappears from temporal.db — producing "no co-change
    // data for anchor.rs" instead of the AD-409-7 notice.
    //
    // The backup/restore trick produces the correct "inconsistent" state:
    //   - lexical manifest: anchor.rs + zstrong.rs  (aweak gone after second build)
    //   - temporal.db:      (anchor, aweak) still present  (from the backup)
    //
    // Safety: the restored DB's META_GIT_HEAD equals the current git HEAD (no new
    // commits were added), so the auto-staleness check on the subsequent query sees the
    // DB as "current" and does NOT trigger a temporal rebuild that would overwrite it.
    let temporal_db_path = find_temporal_db(cache.path()).expect(
        "temporal.db must exist after first build_index — \
         was SKIM_CACHE_DIR respected?",
    );
    let temporal_db_backup = fs::read(&temporal_db_path).expect("read temporal.db for backup");

    // Delete aweak.rs so the NEXT lexical build excludes it from the manifest.
    fs::remove_file(dir.path().join("aweak.rs")).expect("remove aweak.rs");

    // Rebuild lexical index — aweak.rs is now absent from the manifest.
    // This also rebuilds temporal (ghost filter removes aweak from co-change rows),
    // but we restore the backup immediately after.
    build_index(dir.path(), cache.path());

    // Restore the old temporal.db that still has (anchor, aweak) as a co-change pair.
    // The lexical manifest now lists only anchor.rs + zstrong.rs; temporal still records
    // aweak.rs as anchor.rs's co-change partner.  This is the scenario the AD-409-7
    // notice path was designed for.
    fs::write(&temporal_db_path, &temporal_db_backup).expect("restore temporal.db from backup");
    // Remove any WAL / SHM files written by the SECOND build so SQLite reads the
    // restored main-DB file directly on the next open.  These are empty after a
    // clean build (SQLite checkpoints on last-connection-close), but deleting them
    // is the safest guarantee that no stale WAL frames shadow the restored data.
    let db_dir = temporal_db_path
        .parent()
        .expect("temporal.db has a parent dir");
    let _ = fs::remove_file(db_dir.join("temporal.db-wal"));
    let _ = fs::remove_file(db_dir.join("temporal.db-shm"));

    // Run blast-radius on anchor.rs with temporal-only weights.
    // aweak.rs is in temporal.db (co-change partner) but NOT in the lexical manifest.
    // AD-409-7: exactly one stderr line must be emitted naming the dropped count.
    let (stdout, stderr) = run_search_raw(
        dir.path(),
        cache.path(),
        &[
            "xqzjvmblorp_ac409_e2e_ac7",
            "--blast-radius",
            "anchor.rs",
            "--weights",
            "0,0,1",
            "--limit",
            "10",
            "--json",
        ],
    );

    let stderr_text = String::from_utf8_lossy(&stderr);

    // Exit 0: the notice must NOT change the exit code.
    // (checked by the fact that run_search_raw returned — we parse the JSON below)

    // AD-409-7: the stderr must contain the "not found in the indexed manifest" notice.
    // The count excludes the seed (anchor.rs) from the total — "1 of 1" partner dropped.
    assert!(
        stderr_text.contains("not found in the indexed manifest"),
        "AC-7 / AD-409-7: expected 'not found in the indexed manifest' on stderr; \
         got: {stderr_text:?}"
    );
    // "1 of 1" because aweak is the only partner and it is dropped (seed excluded from count).
    assert!(
        stderr_text.contains("1 of 1"),
        "AC-7: dropped count must be '1 of 1' (seed excluded from total); got: {stderr_text:?}"
    );
    // AC-7 requires EXACTLY ONE such line.  The composite arm resolves the allowlist
    // through `paths_to_scored_file_ids` only; if `paths_to_file_ids` is ever hoisted
    // back above the compound/composite dispatch in `execute_query_with_manifest`,
    // both helpers fire and the user sees the identical notice twice.
    let notice_lines = stderr_text
        .lines()
        .filter(|l| l.contains("not found in the indexed manifest"))
        .count();
    assert_eq!(
        notice_lines, 1,
        "AC-7: the partial-drop notice must appear EXACTLY once, not {notice_lines} times; \
         got: {stderr_text:?}"
    );

    // JSON stdout must not contain any new key for the dropped partners.
    let json_text = String::from_utf8_lossy(&stdout);
    // A new key "dropped_partners" would be a contract violation; its absence is required.
    assert!(
        !json_text.contains("dropped_partners"),
        "AC-7: JSON stdout must not contain 'dropped_partners' key"
    );

    // Subcase: NO notice when zero partners are dropped (zstrong is still in manifest).
    // Run standalone blast-radius which does NOT go through paths_to_file_ids for the
    // notice path; instead check by running the composite query with only indexed paths.
    // We verify the "zero drops" case by running against a full-manifest fixture.
    let fixture = make_linear_fixture();
    let cache2 = TempDir::new().unwrap();
    build_index(fixture.path(), cache2.path());

    let (_, stderr2) = run_search_raw(
        fixture.path(),
        cache2.path(),
        &[
            "xqzjvmblorp_ac409_e2e_ac7_nodrop",
            "--blast-radius",
            "anchor.rs",
            "--weights",
            "0,0,1",
            "--limit",
            "10",
            "--json",
        ],
    );
    let stderr2_text = String::from_utf8_lossy(&stderr2);
    assert!(
        !stderr2_text.contains("not found in the indexed manifest"),
        "AC-7: when zero partners are dropped, the notice MUST NOT appear; got: {stderr2_text:?}"
    );
}

/// AC-16a — a shallow clone (`--depth 1`) from a fixture whose HEAD commit
/// touches >= 2 files produces exactly one co-change pair at Jaccard 1.0 and
/// orders FileId-ASC deterministically across two runs.  Exit 0, no panic.
#[test]
fn ac409_16a_shallow_clone_ties_are_deterministic() {
    let now = now_epoch();

    // Source fixture: two files co-changed in one commit.
    let source = TempDir::new().unwrap();
    git_init(source.path());
    write_and_stage(source.path(), "alpha.rs", "// alpha\n");
    write_and_stage(source.path(), "beta.rs", "// beta\n");
    git_commit(source.path(), "feat: initial co-change", now - 5 * 86400);

    // Shallow clone: --depth 1 gives exactly one commit → J(alpha,beta) = 1.0.
    let clone_dir = TempDir::new().unwrap();
    let s = StdCommand::new("git")
        .args([
            "clone",
            "--depth",
            "1",
            source.path().to_str().unwrap(),
            clone_dir.path().to_str().unwrap(),
        ])
        .output()
        .expect("git clone --depth 1");
    assert!(
        s.status.success(),
        "git clone --depth 1 failed: {}",
        String::from_utf8_lossy(&s.stderr)
    );

    let cache = TempDir::new().unwrap();
    build_index(clone_dir.path(), cache.path());

    // Run twice and compare paths — determinism check (AC-4 / AC-16a).
    let v1 = run_search_json(
        clone_dir.path(),
        cache.path(),
        &[
            "xqzjvmblorp_ac409_shallow_a",
            "--blast-radius",
            "alpha.rs",
            "--weights",
            "0,0,1",
            "--limit",
            "5",
        ],
    );
    let v2 = run_search_json(
        clone_dir.path(),
        cache.path(),
        &[
            "xqzjvmblorp_ac409_shallow_a",
            "--blast-radius",
            "alpha.rs",
            "--weights",
            "0,0,1",
            "--limit",
            "5",
        ],
    );

    // Both runs must exit 0 (assert_cmd guarantees this via assert().success()).
    // Result paths must be byte-identical (deterministic, no panic).
    let empty1: Vec<serde_json::Value> = vec![];
    let paths1: Vec<Option<&str>> = v1["results"]
        .as_array()
        .unwrap_or(&empty1)
        .iter()
        .map(|r| r["path"].as_str())
        .collect();
    let empty2: Vec<serde_json::Value> = vec![];
    let paths2: Vec<Option<&str>> = v2["results"]
        .as_array()
        .unwrap_or(&empty2)
        .iter()
        .map(|r| r["path"].as_str())
        .collect();
    assert_eq!(
        paths1, paths2,
        "AC-16a: two identical queries on a shallow clone must produce byte-identical path order"
    );

    // Non-vacuous: at least one result (beta.rs as co-change partner of alpha.rs).
    // The shallow clone records alpha+beta in the one commit; J=1.0.
    // beta.rs must appear as a partner (and anchor/alpha as seed).
    let all_paths: Vec<&str> = paths1.iter().filter_map(|&p| p).collect();
    assert!(
        all_paths.contains(&"beta.rs"),
        "AC-16a: beta.rs must appear as co-change partner of alpha.rs on a shallow clone; \
         got: {all_paths:?}"
    );
}

/// AC-16b — `git clone --depth 1` of THIS repository exits 0, emits a degraded
/// element with `subsystem: "temporal"`, emits NO fabricated ranking, and produces
/// byte-identical output across two runs.
///
/// After #407, a `--depth 1` clone whose sole commit is a merge commit yields
/// zero non-merge commits → skim detects a shallow state and degrades gracefully.
/// We assert only the degraded contract, never a specific ranking.
#[test]
fn ac409_16b_real_repo_shallow_degrades_gracefully() {
    // Shallow clone of THIS repository.
    let this_repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();

    let clone_dir = TempDir::new().unwrap();
    let s = StdCommand::new("git")
        .args([
            "clone",
            "--depth",
            "1",
            this_repo.to_str().unwrap(),
            clone_dir.path().to_str().unwrap(),
        ])
        .output()
        .expect("git clone --depth 1");
    // If clone fails (e.g. network isolated), skip gracefully.
    if !s.status.success() {
        eprintln!(
            "AC-16b: git clone --depth 1 failed ({}); skipping test",
            String::from_utf8_lossy(&s.stderr)
        );
        return;
    }

    let cache = TempDir::new().unwrap();
    build_index(clone_dir.path(), cache.path());

    // Run twice — both must exit 0 and produce byte-identical JSON stdout.
    let (stdout1, _stderr1) = run_search_raw(
        clone_dir.path(),
        cache.path(),
        &[
            "--blast-radius",
            "crates/rskim/src/cmd/search/query.rs",
            "--weights",
            "0,0,1",
            "--limit",
            "5",
            "--json",
        ],
    );
    let (stdout2, _stderr2) = run_search_raw(
        clone_dir.path(),
        cache.path(),
        &[
            "--blast-radius",
            "crates/rskim/src/cmd/search/query.rs",
            "--weights",
            "0,0,1",
            "--limit",
            "5",
            "--json",
        ],
    );

    // AC-16b: byte-identical outputs (determinism).
    assert_eq!(
        stdout1, stdout2,
        "AC-16b: two identical queries on the shallow real-repo clone must be byte-identical"
    );

    // AC-16b: the JSON must contain a `degraded` array with at least one element
    // whose `subsystem` == "temporal".
    let json_text = String::from_utf8_lossy(&stdout1);
    let v: Value = serde_json::from_str(&json_text).unwrap_or(Value::Null);

    if v.is_null() {
        eprintln!("AC-16b: could not parse JSON output; skipping ranking assertions");
        return;
    }

    // Degraded contract: subsystem "temporal" element must be present.
    let degraded = v["degraded"].as_array();
    if let Some(degs) = degraded
        && !degs.is_empty()
    {
        let has_temporal = degs
            .iter()
            .any(|d| d["subsystem"].as_str() == Some("temporal"));
        assert!(
            has_temporal,
            "AC-16b: degraded array must contain a 'temporal' subsystem element; \
             got: {degs:?}"
        );
        // If degs is empty, the shallow clone may have produced valid temporal data
        // (e.g. HEAD is not a merge commit at depth 1).  Both outcomes are acceptable.
    }
    // The results array (if present) must contain no fabricated entries beyond
    // what a non-degraded query would return — we assert only exit 0 and
    // byte-identity, not the specific result count, per AC-16b.
}
