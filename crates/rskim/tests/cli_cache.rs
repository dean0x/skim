//! CLI integration tests for caching functionality
//!
//! Tests cache creation, reuse, invalidation, --no-cache flag,
//! and SKIM_CACHE_DIR / SKIM_ANALYTICS_DB env var behavior (B1–B7, PF-002 fix).

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;
mod common;

// ============================================================================
// B1–B7: SKIM_CACHE_DIR relocation tests (Phase B — PF-002 fix)
// ============================================================================

/// Helper: build a basic skim invocation against a temp TypeScript file.
fn skim_with_ts_file(cache_dir: &std::path::Path) -> (Command, std::path::PathBuf) {
    let src_dir = TempDir::new().unwrap();
    let file_path = src_dir.path().join("test.ts");
    fs::write(
        &file_path,
        "function greet(name: string): string { return `Hello ${name}`; }",
    )
    .unwrap();

    // Leak the TempDir so the file outlives the command; callers that need it can
    // take ownership.  For path-only tests we just need the PathBuf.
    let file_path_owned = file_path.clone();
    std::mem::forget(src_dir); // keep file alive for test duration

    let mut cmd = common::skim();
    cmd.env("SKIM_CACHE_DIR", cache_dir.as_os_str()); // analytics already OFF via common::skim()
    (cmd, file_path_owned)
}

/// B1: SKIM_CACHE_DIR alone causes analytics rows to land in `<cache_dir>/analytics.db`.
///
/// This is the primary behavior change from #359 Phase B: when `SKIM_CACHE_DIR` is set
/// (and `SKIM_ANALYTICS_DB` is NOT set), the analytics DB is created under the overridden
/// cache root rather than under `~/.cache/skim`. The test is hermetic: both the parser
/// cache and the analytics DB are confined to an isolated TempDir.
#[test]
fn test_b1_skim_cache_dir_relocates_analytics_db() {
    let cache_dir = TempDir::new().unwrap();

    // Write a tiny Rust fixture into a separate TempDir so the file outlives the command.
    let src_dir = TempDir::new().unwrap();
    let file_path = src_dir.path().join("sample.rs");
    fs::write(
        &file_path,
        "fn greet(name: &str) -> String { format!(\"Hello {name}\") }",
    )
    .unwrap();
    // Keep src_dir alive for the duration of the test.

    // Construct the command directly — NOT via common::skim() (which disables analytics)
    // and NOT via skim_with_analytics() (which sets SKIM_ANALYTICS_DB, defeating the test).
    // We need SKIM_CACHE_DIR to be the only override so the default analytics.db path
    // resolves to <cache_dir>/analytics.db.
    let mut cmd = assert_cmd::Command::cargo_bin("skim").unwrap();
    cmd.env("SKIM_CACHE_DIR", cache_dir.path())
        .env("NO_COLOR", "1")
        .env_remove("SKIM_DISABLE_ANALYTICS") // analytics ON
        .env_remove("SKIM_ANALYTICS_DB") // CRUCIAL: force the default-relocation path
        .env_remove("SKIM_PASSTHROUGH"); // ensure compression is active

    cmd.arg(&file_path).assert().success();

    // The analytics DB must appear under the overridden cache root.
    let expected_db = cache_dir.path().join("analytics.db");
    assert!(
        expected_db.exists(),
        "B1: analytics.db must be created under SKIM_CACHE_DIR={} when SKIM_ANALYTICS_DB is \
         not set, but the file was not found. Default ~/.cache/skim path may still be in use.",
        cache_dir.path().display()
    );

    // Count token_savings rows — flush_pending() joins the background writer before process
    // exit, so the row is present once assert_cmd returns. No sleep needed.
    let conn = rusqlite::Connection::open(&expected_db).expect("must open analytics DB");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM token_savings", [], |r| r.get(0))
        .unwrap_or(0);
    assert!(
        count >= 1,
        "B1: at least one token_savings row must be recorded in <SKIM_CACHE_DIR>/analytics.db \
         after a plain `skim <file>` run (got {count} rows)"
    );
}

/// B2: SKIM_CACHE_DIR relocates parser-cache entries.
///
/// After running skim with SKIM_CACHE_DIR=<dir>, JSON cache files must appear
/// directly under <dir> (not under ~/.cache/skim).
#[test]
fn test_b2_skim_cache_dir_relocates_parser_cache() {
    let cache_dir = TempDir::new().unwrap();
    let (mut cmd, file_path) = skim_with_ts_file(cache_dir.path());
    cmd.arg(&file_path).assert().success();

    // At least one .json cache file should land under <cache_dir>
    let json_files: Vec<_> = fs::read_dir(cache_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .collect();

    assert!(
        !json_files.is_empty(),
        "B2: parser-cache .json files must land under SKIM_CACHE_DIR={}, found none. \
         Default cache dir may be in use instead.",
        cache_dir.path().display()
    );
}

/// B3: SKIM_CACHE_DIR relocates tee output directory.
///
/// This is a structural test — we verify get_cache_dir() respects SKIM_CACHE_DIR
/// by confirming that skim creates its cache structure under the override dir.
/// (Tee files only appear on command failure, but the tee directory creation
/// is gated through get_cache_dir, which now honors SKIM_CACHE_DIR.)
#[test]
fn test_b3_skim_cache_dir_relocates_tee_dir() {
    let cache_dir = TempDir::new().unwrap();
    let (mut cmd, file_path) = skim_with_ts_file(cache_dir.path());
    cmd.arg(&file_path).assert().success();

    // After a run, the cache root should exist under override dir
    // (even if the tee subdir isn't created until a failure occurs)
    assert!(
        cache_dir.path().exists(),
        "B3: SKIM_CACHE_DIR directory should exist after a skim run"
    );

    // The parser cache files demonstrate the root is the override dir,
    // which means tee (which calls get_cache_dir() + /tee) also points there.
    let entries: Vec<_> = fs::read_dir(cache_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        !entries.is_empty(),
        "B3: cache root must have entries under SKIM_CACHE_DIR (tee roots here)"
    );
}

/// B4: SKIM_ANALYTICS_DB wins over SKIM_CACHE_DIR for the analytics DB path.
///
/// When both are set, the DB is created at SKIM_ANALYTICS_DB, NOT at
/// <SKIM_CACHE_DIR>/analytics.db.
#[test]
fn test_b4_skim_analytics_db_wins_over_cache_dir() {
    let cache_dir = TempDir::new().unwrap();
    let analytics_dir = TempDir::new().unwrap();
    let explicit_db = analytics_dir.path().join("my-analytics.db");

    // Run skim stats (a subcommand that opens the analytics DB read-only)
    // with both vars set.  We use `skim stats` which opens the default DB.
    common::skim()
        .args(["stats"])
        .env("SKIM_CACHE_DIR", cache_dir.path().as_os_str())
        .env("SKIM_ANALYTICS_DB", explicit_db.to_str().unwrap())
        .assert()
        .success();

    // The DB must appear at the explicit path, NOT under <cache_dir>.
    assert!(
        explicit_db.exists(),
        "B4: SKIM_ANALYTICS_DB must take precedence — db should exist at {}, \
         not under SKIM_CACHE_DIR={}",
        explicit_db.display(),
        cache_dir.path().display()
    );

    let default_db = cache_dir.path().join("analytics.db");
    assert!(
        !default_db.exists(),
        "B4: <SKIM_CACHE_DIR>/analytics.db must NOT be created when SKIM_ANALYTICS_DB is set, \
         but found: {}",
        default_db.display()
    );
}

/// B5: Neither SKIM_CACHE_DIR nor SKIM_ANALYTICS_DB set => default path unchanged.
///
/// We can only verify the absence of regressions here (the default location is
/// the real ~/.cache/skim which we must not corrupt).  We run skim normally and
/// confirm it succeeds, trusting the unit tests in cache.rs for the path value.
#[test]
fn test_b5_default_cache_behavior_unchanged() {
    let src_dir = TempDir::new().unwrap();
    let file_path = src_dir.path().join("hello.ts");
    fs::write(&file_path, "const x: number = 1;").unwrap();

    common::skim()
        .arg(&file_path)
        .env_remove("SKIM_CACHE_DIR")
        .env_remove("SKIM_ANALYTICS_DB")
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success();
}

/// B7: Empty SKIM_CACHE_DIR is treated as unset (falls back to default).
///
/// Verified at unit level in cache.rs; this integration test confirms the binary
/// does not error out when SKIM_CACHE_DIR is set to an empty string.
#[test]
fn test_b7_empty_skim_cache_dir_does_not_error() {
    let src_dir = TempDir::new().unwrap();
    let file_path = src_dir.path().join("hello.ts");
    fs::write(&file_path, "const y: number = 2;").unwrap();

    common::skim()
        .arg(&file_path)
        .env("SKIM_CACHE_DIR", "")
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success();
}

#[test]
fn test_cache_basic_reuse() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function test() { return 42; }").unwrap();

    // First run - should create cache
    let output1 = common::skim()
        .arg(&file_path)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // Second run - should use cache (output should be identical)
    let output2 = common::skim()
        .arg(&file_path)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_eq!(output1, output2, "Cached output should match original");
}

#[test]
fn test_cache_invalidation_on_file_modification() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function original() { return 1; }").unwrap();

    // First run - creates cache
    common::skim()
        .arg(&file_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("function original"));

    // Wait to ensure mtime changes (some filesystems have 1-second granularity)
    thread::sleep(Duration::from_secs(1));

    // Modify file
    fs::write(&file_path, "function modified() { return 2; }").unwrap();

    // Second run - should detect mtime change and invalidate cache
    common::skim()
        .arg(&file_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("function modified"))
        .stdout(predicate::str::contains("function original").not());
}

#[test]
fn test_cache_different_modes() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    // The subject is the CACHE KEY (signatures must not be served the structure
    // entry), and "outputs differ" is the evidence for it. That evidence needs
    // both modes to actually compress: the previous 30-byte fixture saved 8 B / 3 t
    // under structure and 15 B / 6 t under signatures, both far under their
    // 76 B / 22 t and 89 B / 24 t markers, so the ADR-001 guard served raw for
    // BOTH — making the two outputs byte-identical and the assertion unfalsifiable
    // no matter how the cache behaved.
    //
    // Measured on this fixture (raw 718 B / 193 t):
    //   structure  → 235 B /  58 t: margin +407 B / +113 t (5.4x / 5.1x marker)
    //   signatures → 115 B /  28 t: margin +514 B / +141 t (5.8x / 5.9x marker)
    fs::write(
        &file_path,
        r#"export class InvoiceTotals {
  private readonly rates: Map<string, number> = new Map();

  register(region: string, rate: number): void {
    if (rate < 0) { throw new RangeError(`negative rate for ${region}`); }
    this.rates.set(region, rate);
  }

  totalFor(region: string, subtotal: number): number {
    const rate = this.rates.get(region);
    if (rate === undefined) { throw new Error(`unknown region ${region}`); }
    const tax = subtotal * rate;
    return Math.round((subtotal + tax) * 100) / 100;
  }

  summarise(): string {
    const parts: string[] = [];
    for (const [region, rate] of this.rates) {
      parts.push(`${region}=${(rate * 100).toFixed(2)}%`);
    }
    return parts.join(", ");
  }
}
"#,
    )
    .unwrap();

    // Run with structure mode
    let structure_output = common::skim()
        .arg(&file_path)
        .arg("--mode=structure")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // Run with signatures mode (should produce different output, not use structure cache)
    let signatures_output = common::skim()
        .arg(&file_path)
        .arg("--mode=signatures")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_ne!(
        structure_output, signatures_output,
        "Different modes should produce different output"
    );
}

#[test]
fn test_no_cache_flag() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function test() { return 42; }").unwrap();

    // First run with --no-cache
    let output1 = common::skim()
        .arg(&file_path)
        .arg("--no-cache")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // Second run with --no-cache (should not use cache even if it exists)
    let output2 = common::skim()
        .arg(&file_path)
        .arg("--no-cache")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // Outputs should still match (deterministic transformation)
    assert_eq!(output1, output2);

    // Third run without --no-cache should still work
    common::skim().arg(&file_path).assert().success();
}

#[test]
fn test_clear_cache_command() {
    let temp_dir = TempDir::new().unwrap();
    let cache_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function test() { return 42; }").unwrap();

    // Create cache by running normally
    common::skim()
        .env("SKIM_CACHE_DIR", cache_dir.path())
        .arg(&file_path)
        .assert()
        .success();
    let entries = |dir: &std::path::Path| -> usize {
        fs::read_dir(dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == "json")
            })
            .count()
    };
    assert!(entries(cache_dir.path()) > 0, "the run must write an entry");

    // Clear cache
    common::skim()
        .env("SKIM_CACHE_DIR", cache_dir.path())
        .arg("--clear-cache")
        .assert()
        .success()
        .stdout(predicate::str::contains("Cache cleared successfully"));
    assert_eq!(entries(cache_dir.path()), 0, "the entries must be gone");

    // Should still work after cache clear
    common::skim()
        .env("SKIM_CACHE_DIR", cache_dir.path())
        .arg(&file_path)
        .assert()
        .success();
}

/// `SKIM_CACHE_DIR` can name any folder. `--clear-cache` removes the cache
/// entries in it (`<sha256 hex>.json`), the old search index files and nothing
/// else: not a `.json` file that is not an entry, not an unknown file, not a
/// link or its target.
#[test]
fn test_clear_cache_removes_only_skim_files() {
    let cache_dir = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let base = cache_dir.path();

    // Skim's own files.
    let entry = format!("{}.json", "ab12".repeat(16));
    fs::write(base.join(&entry), b"{}").unwrap();
    let shard = base.join("search").join("0123456789abcdef");
    fs::create_dir_all(&shard).unwrap();
    fs::write(shard.join("index.skidx"), vec![b'x'; 1500]).unwrap();
    fs::write(shard.join("temporal.db"), vec![b'y'; 500]).unwrap();

    // Files that are not skim's.
    let user_files = [
        "notes.json",
        "analytics.db",
        "report.txt",
        "temporal.db",
        "index.skidx",
    ];
    for name in user_files {
        fs::write(base.join(name), name.as_bytes()).unwrap();
    }
    fs::write(shard.join("notes.txt"), b"mine").unwrap();
    fs::write(base.join("search").join("readme.txt"), b"mine").unwrap();
    let target = outside.path().join("precious.db");
    fs::write(&target, b"precious").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, shard.join("index.skpost")).unwrap();

    common::skim()
        .env("SKIM_CACHE_DIR", base)
        .arg("--clear-cache")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Removed old search index data (1 folders, 0.0 MB)",
        ))
        .stdout(predicate::str::contains("Cache cleared successfully"));

    assert!(!base.join(&entry).exists(), "the cache entry must go");
    assert!(!shard.join("index.skidx").exists());
    assert!(!shard.join("temporal.db").exists());
    for name in user_files {
        assert_eq!(
            fs::read(base.join(name)).unwrap(),
            name.as_bytes(),
            "{name} is not a skim file and must stay"
        );
    }
    assert_eq!(fs::read(shard.join("notes.txt")).unwrap(), b"mine");
    assert_eq!(
        fs::read(base.join("search").join("readme.txt")).unwrap(),
        b"mine"
    );
    #[cfg(unix)]
    assert!(
        fs::symlink_metadata(shard.join("index.skpost"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "a link with a listed name must stay"
    );
    assert_eq!(fs::read(&target).unwrap(), b"precious");
}

#[test]
fn test_cache_with_glob_patterns() {
    let temp_dir = TempDir::new().unwrap();

    fs::write(temp_dir.path().join("file1.ts"), "function a() {}").unwrap();
    fs::write(temp_dir.path().join("file2.ts"), "function b() {}").unwrap();
    fs::write(temp_dir.path().join("file3.ts"), "function c() {}").unwrap();

    // First run with glob - creates cache for all files
    let output1 = common::skim()
        .arg("*.ts")
        .current_dir(temp_dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // Second run with glob - should use cache
    let output2 = common::skim()
        .arg("*.ts")
        .current_dir(temp_dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_eq!(output1, output2, "Cached glob output should match original");
}

#[test]
fn test_cache_stores_token_counts() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function test() { return 42; }").unwrap();

    // First run with --show-stats - should cache token counts
    let stderr1 = common::skim()
        .arg(&file_path)
        .arg("--show-stats")
        .assert()
        .success()
        .get_output()
        .stderr
        .clone();

    let stderr1_str = String::from_utf8_lossy(&stderr1);
    assert!(
        stderr1_str.contains("[skim]"),
        "First run should show token stats"
    );

    // Second run with --show-stats - should use cached token counts
    let stderr2 = common::skim()
        .arg(&file_path)
        .arg("--show-stats")
        .assert()
        .success()
        .get_output()
        .stderr
        .clone();

    let stderr2_str = String::from_utf8_lossy(&stderr2);
    assert!(
        stderr2_str.contains("[skim]"),
        "Second run should show cached token stats"
    );

    // Token counts should be identical
    assert_eq!(
        stderr1_str, stderr2_str,
        "Cached token counts should match original"
    );
}

#[test]
fn test_cache_with_explicit_language() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("noext");
    fs::write(&file_path, "function test() { return 42; }").unwrap();

    // First run with explicit language
    let output1 = common::skim()
        .arg(&file_path)
        .arg("--language=typescript")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // Second run - should use cache
    let output2 = common::skim()
        .arg(&file_path)
        .arg("--language=typescript")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_eq!(output1, output2);
}

#[test]
fn test_no_cache_with_show_stats() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function test() { return 42; }").unwrap();

    // Run with both --no-cache and --show-stats
    common::skim()
        .arg(&file_path)
        .arg("--no-cache")
        .arg("--show-stats")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"));
}

#[test]
fn test_cache_stats_computed_on_hit_when_missing() {
    // Scenario: First run without --show-stats caches (content, None, None).
    // Second run WITH --show-stats should still compute and display tokens.
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.ts");
    fs::write(&file_path, "function test() { return 42; }").unwrap();

    // First run without --show-stats (caches without token counts)
    common::skim().arg(&file_path).assert().success();

    // Second run with --show-stats (should compute tokens from cache hit)
    common::skim()
        .arg(&file_path)
        .arg("--show-stats")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim]"));
}

// ============================================================================
// `--symbol` cache identity (issue 540, PR 2)
// ============================================================================
//
// A symbol read caches the selected lines, so its cache key must name the
// symbol: a key that did not would serve the output of `--symbol a` to a read
// of `--symbol b`, or the whole file to either. The key itself is pinned byte
// for byte in `cache.rs`; these tests pin what a reader of the CLI sees.

/// Two functions that a symbol read can tell apart, and a whole-file read too.
const TWO_FUNCTIONS: &str = "fn a() -> u32 {\n    1\n}\n\nfn b() -> u32 {\n    2\n}\n";

/// Output of one run: the bytes the reader sees on each stream.
#[derive(Debug, PartialEq, Eq)]
struct Streams {
    stdout: String,
    stderr: String,
}

/// A sandboxed home that holds `name` with `source`. Its parser cache is
/// `<home>/.cache/skim`, which no other test shares.
fn symbol_home(name: &str, source: &str) -> TempDir {
    let home = TempDir::new().unwrap();
    fs::write(home.path().join(name), source).unwrap();
    home
}

/// Run `skim <args>` in the sandbox of `home`; the run must succeed.
fn read_ok(home: &TempDir, args: &[&str]) -> Streams {
    let output = common::skim_sandboxed(home.path())
        .args(args)
        .output()
        .expect("skim must start");
    assert!(
        output.status.success(),
        "skim {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Streams {
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

/// The parser-cache entries (`<key>.json`) of the sandbox of `home`.
fn cache_entries(home: &TempDir) -> Vec<std::path::PathBuf> {
    let cache_dir = common::sandbox_var_path(home.path(), ".cache/skim");
    let Ok(listing) = fs::read_dir(cache_dir) else {
        return Vec::new();
    };
    listing
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect()
}

/// Change the stored entry whose `content` is the stdout of `cold`.
///
/// A warm read that equals its cold read proves nothing: a cache miss prints
/// the same bytes. After `edit` changes the stored entry, a warm read shows
/// whether it was served from that entry or ran cold again.
fn edit_entry_of(home: &TempDir, cold: &Streams, edit: impl FnOnce(&mut serde_json::Value)) {
    let load = |path: &std::path::Path| -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    };
    let entry_path = cache_entries(home)
        .into_iter()
        .find(|path| load(path)["content"] == cold.stdout.as_str())
        .expect("the cold read wrote an entry that holds its stdout");
    let mut entry = load(&entry_path);
    edit(&mut entry);
    fs::write(&entry_path, entry.to_string()).unwrap();
}

/// Replace the stored lines of the entry that holds the stdout of `cold`.
fn replace_stored_lines(home: &TempDir, cold: &Streams, stored: &str) {
    edit_entry_of(home, cold, |entry| {
        entry["content"] = serde_json::Value::from(stored);
    });
}

#[test]
fn a_full_read_and_two_symbol_reads_of_one_file_keep_three_different_cached_outputs() {
    // TP-41
    let home = symbol_home("two.rs", TWO_FUNCTIONS);
    let full = ["two.rs", "--mode", "full"];
    let only_a = ["two.rs", "--symbol", "a"];
    let only_b = ["two.rs", "--symbol", "b"];

    // Cold: nothing is cached yet.
    let cold_full = read_ok(&home, &full);
    let cold_a = read_ok(&home, &only_a);
    let cold_b = read_ok(&home, &only_b);

    assert_eq!(cold_full.stdout, TWO_FUNCTIONS);
    assert_eq!(cold_a.stdout, "fn a() -> u32 {\n    1\n}\n");
    assert_eq!(cold_b.stdout, "fn b() -> u32 {\n    2\n}\n");
    assert_eq!(
        cache_entries(&home).len(),
        3,
        "a full read, --symbol a and --symbol b each own one entry"
    );

    // Give each entry its own stored text. A warm read prints the text of the
    // entry of its own key, and the notice that the entry kept: a read that
    // ran cold would print the source lines instead.
    replace_stored_lines(&home, &cold_full, "stored: whole file\n");
    replace_stored_lines(&home, &cold_a, "stored: a\n");
    replace_stored_lines(&home, &cold_b, "stored: b\n");

    // Warm, in the opposite order of the cold reads.
    let warm_b = read_ok(&home, &only_b);
    let warm_a = read_ok(&home, &only_a);
    let warm_full = read_ok(&home, &full);

    assert_eq!(warm_b.stdout, "stored: b\n");
    assert_eq!(warm_b.stderr, cold_b.stderr);
    assert_eq!(warm_a.stdout, "stored: a\n");
    assert_eq!(warm_a.stderr, cold_a.stderr);
    assert_eq!(warm_full.stdout, "stored: whole file\n");
    assert_eq!(warm_full.stderr, cold_full.stderr);
    assert_eq!(cache_entries(&home).len(), 3, "a warm read adds no entry");
}

#[test]
fn a_warm_symbol_read_is_served_from_the_entry_of_its_own_name() {
    // TP-41
    let home = symbol_home("two.rs", TWO_FUNCTIONS);
    let cold_a = read_ok(&home, &["two.rs", "--symbol", "a"]);
    let cold_b = read_ok(&home, &["two.rs", "--symbol", "b"]);

    // Replace the stored lines of the entry of `a` with a marker text. A warm
    // read of `a` must print it, and a warm read of `b` must not.
    replace_stored_lines(&home, &cold_a, "stored lines of a\n");

    let warm_a = read_ok(&home, &["two.rs", "--symbol", "a"]);
    let warm_b = read_ok(&home, &["two.rs", "--symbol", "b"]);

    assert_eq!(warm_a.stdout, "stored lines of a\n");
    assert_eq!(
        warm_a.stderr, cold_a.stderr,
        "the stderr notice is stored with the entry and printed again"
    );
    assert_eq!(warm_b, cold_b);
}

#[test]
fn a_stored_symbol_entry_without_a_notice_is_not_served() {
    // TP-41 (the notice is unconditional, so an entry that lost it is a miss)
    let home = symbol_home("two.rs", TWO_FUNCTIONS);
    let cold = read_ok(&home, &["two.rs", "--symbol", "a"]);
    assert!(
        !cold.stderr.is_empty(),
        "a symbol read prints a notice, or this test proves nothing"
    );

    // Drop the notice from the entry and change its lines, so a read that
    // served the entry anyway would show it on stdout and print no notice.
    edit_entry_of(&home, &cold, |entry| {
        entry
            .as_object_mut()
            .expect("an entry is a JSON object")
            .remove("notice")
            .expect("the cold read stored its notice");
        entry["content"] = serde_json::Value::from("stored lines of a\n");
    });

    let warm = read_ok(&home, &["two.rs", "--symbol", "a"]);

    assert_eq!(
        warm, cold,
        "the read runs cold: the source lines and the notice are printed again"
    );
}

#[test]
fn a_warm_read_of_several_declarations_prints_the_same_headers_and_notice() {
    // TP-41 (the notice of a multi-match read is cached with its entry)
    let source = "fn dup() -> u32 {\n    1\n}\n\nfn dup(x: u32) -> u32 {\n    x\n}\n";
    let home = symbol_home("dup.rs", source);

    let cold = read_ok(&home, &["dup.rs", "--symbol", "dup"]);

    assert!(
        cold.stdout.starts_with("==> dup.rs:1-3 dup (1 of 2) <==\n"),
        "stdout: {}",
        cold.stdout
    );
    assert_eq!(
        cold.stderr,
        "[skim] --symbol \"dup\": 2 declarations, 6 of 7 lines shown (1 not shown) \
         — omit --symbol for the whole file\n"
    );

    // Change one stored body line. Only a read served from the entry prints it,
    // and it keeps the stored headers and the stored notice.
    let stored = cold.stdout.replacen("    1\n", "    stored\n", 1);
    assert_ne!(stored, cold.stdout, "the edit must change the stored lines");
    replace_stored_lines(&home, &cold, &stored);

    let warm = read_ok(&home, &["dup.rs", "--symbol", "dup"]);

    assert_eq!(warm.stdout, stored);
    assert_eq!(warm.stderr, cold.stderr);
}

#[test]
fn a_symbol_read_that_fails_leaves_no_cache_entry() {
    // TP-41 (a refusal must not be replayed as if it were a result)
    let home = symbol_home("two.rs", TWO_FUNCTIONS);
    fs::write(home.path().join("notes.md"), "# Notes\n").unwrap();

    for args in [
        ["two.rs", "--symbol", "missing"],
        ["notes.md", "--symbol", "a"],
    ] {
        let output = common::skim_sandboxed(home.path())
            .args(args)
            .output()
            .expect("skim must start");
        assert!(!output.status.success(), "skim {args:?} must fail");
    }

    assert!(
        cache_entries(&home).is_empty(),
        "a failed symbol read stores nothing: {:?}",
        cache_entries(&home)
    );
}
