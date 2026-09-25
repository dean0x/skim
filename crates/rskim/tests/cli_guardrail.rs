//! CLI tests for output guardrail (#53)
//!
//! Tests that the guardrail triggers when compressed output is larger than raw,
//! and does not trigger for normal files or in full mode.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;
mod common;

/// Get a command for the skim binary
fn skim_cmd() -> Command {
    let mut cmd = common::skim();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd.env_remove("SKIM_REWRITTEN_FROM");
    cmd
}

#[test]
fn test_guardrail_skips_tiny_files() {
    // Tiny files (< 256 bytes) should skip the guardrail entirely because
    // transformation overhead is expected for small inputs.
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("tiny.ts");
    std::fs::write(&file, "const x = 1;\n").unwrap();

    skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--mode=structure")
        .arg("--no-cache")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim:guardrail]").not());
}

#[test]
fn test_guardrail_does_not_trigger_on_normal_file() {
    // A normal-sized file should compress well and not trigger the guardrail.
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("normal.ts");
    std::fs::write(
        &file,
        "import { something } from 'somewhere';\n\
         type UserId = string;\n\
         interface User {\n\
           id: UserId;\n\
           name: string;\n\
           email: string;\n\
         }\n\
         function createUser(name: string, email: string): User {\n\
           const id = generateId();\n\
           return { id, name, email };\n\
         }\n\
         function deleteUser(id: UserId): void {\n\
           const user = findUser(id);\n\
           if (!user) throw new Error('not found');\n\
           removeFromDatabase(user);\n\
         }\n\
         export { createUser, deleteUser };\n",
    )
    .unwrap();

    skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--mode=structure")
        .arg("--no-cache")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim:guardrail]").not());
}

#[test]
fn test_guardrail_skipped_in_full_mode() {
    // Full mode should skip the guardrail entirely.
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("tiny.ts");
    std::fs::write(&file, "const x = 1;\n").unwrap();

    skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--mode=full")
        .arg("--no-cache")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim:guardrail]").not());
}

#[test]
fn test_guardrail_triggers_when_output_inflates() {
    // Structure mode replaces function bodies with ` {...}` (6 bytes).
    // For functions with empty bodies `{ }` (3 bytes), each replacement ADDS
    // 3 bytes. With enough short functions (>= 256 bytes total raw), the
    // compressed output exceeds the raw size in both bytes and tokens,
    // triggering the guardrail warning on stderr.
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("inflating.ts");

    // Each line is ~18 bytes: `function XX() { }\n`
    // 20 functions = ~360 bytes raw (well above the guardrail activation size).
    // Each function body `{ }` (3 bytes) -> ` {...}` (6 bytes) = +3 bytes
    // Total output growth: 20 * 3 = 60 extra bytes -> ~420 bytes output vs ~360 raw
    let mut source = String::new();
    for i in 0..20 {
        source.push_str(&format!("function f{i}() {{ }}\n"));
    }
    assert!(
        source.len() >= 256,
        "Test file must be >= 256 bytes for guardrail to activate, got {}",
        source.len()
    );

    std::fs::write(&file, &source).unwrap();

    skim_cmd()
        .env("SKIM_DEBUG", "1")
        .arg(file.to_str().unwrap())
        .arg("--mode=structure")
        .arg("--no-cache")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim:guardrail]"));
}

/// SKIM_DEBUG-unset negative twin of `test_guardrail_triggers_when_output_inflates`.
///
/// Same inflating-file scenario (guardrail DOES fire), but with SKIM_DEBUG removed
/// by `skim_cmd()`.  Per ADR-011, when the guardrail fires the banner routes to
/// `io::sink()` when debug mode is off — stderr must be empty.
///
/// Paired with the SKIM_DEBUG=1 positive assertion above to make the gate
/// revert-detectable: reverting `apply_to_stderr` to always-write leaves the
/// positive test green (banner still appears) but breaks this test (stderr would no
/// longer be empty) — CI catches the regression.  Without this twin, all four Fix-5
/// gating edits can be reverted and the suite stays green.
#[test]
fn test_guardrail_fires_silently_without_debug() {
    // Identical inflating source to test_guardrail_triggers_when_output_inflates:
    // 20 functions with empty bodies inflate under --mode=structure (guardrail fires).
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("inflating.ts");

    let mut source = String::new();
    for i in 0..20 {
        source.push_str(&format!("function f{i}() {{ }}\n"));
    }
    assert!(
        source.len() >= 256,
        "Test file must be >= 256 bytes for guardrail to activate, got {}",
        source.len()
    );

    std::fs::write(&file, &source).unwrap();

    // SKIM_DEBUG is unset — skim_cmd() removes it at construction.
    // The guardrail fires (output inflates) but apply_to_stderr routes the banner
    // to io::sink() when debug mode is off.  Stderr must be empty.
    skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--mode=structure")
        .arg("--no-cache")
        .assert()
        .success()
        .stderr(predicate::str::is_empty());
}

// ============================================================================
// ADR-001 amendment 2026-09-24 — the guard is charged the stderr disclosure
// ============================================================================
//
// The guard compares stdout sizes and has never seen the ADR-008 / ADR-011
// class-1 marker the same invocation is about to print. Agent harnesses capture
// stderr into the same context window as stdout, so a view that saves less than
// its own disclosure costs is a net loss the guard used to score as a win.
//
// The two tests below are the behavioural pair: same mode, same language, same
// shape of source — only the size differs, and the size is what decides.
//
// Measured with `target/debug/skim` at efac056 (the transform is untouched by
// this change), and against the pinned marker costs in `output/mod.rs`
// (`test_lossy_view_marker_composed_cost_ceiling`): a direct `structure` marker
// costs 76 bytes / 22 cl100k tokens.

/// SMALL file — the transform cannot pay for its own disclosure, so raw wins.
///
/// Measured: 89 B / 24 tokens raw; 71 B / 16 tokens under `--mode=structure`.
/// The saving is 18 B / 8 tokens against a 76 B / 22 token marker, so the
/// disclosure costs 4.2x what the transform saves. The byte gate alone decides
/// (71 + 76 = 147 >= 89), which is why this verdict cannot move under a
/// tokeniser patch bump.
///
/// Three assertions, and the third is the load-bearing one: stdout must be the
/// source verbatim, no marker may fire (nothing was elided, so an ADR-011
/// class-1 marker would be a lie), and under `SKIM_DEBUG=1` the
/// `[skim:guardrail]` banner MUST appear — that banner is what distinguishes
/// "the guard decided to serve raw" from "the transform silently no-op'd".
///
/// RED at ac817b2 (verified by running that binary): stdout was the 71-byte
/// structure view, the marker fired, and no banner appeared.
#[test]
fn test_small_file_cannot_pay_for_its_disclosure_and_serves_raw() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("small.ts");
    let source = "export function a(): number {\n  return 1;\n}\n\n\
                  export function b(): number {\n  return 2;\n}\n";
    assert_eq!(source.len(), 89, "fixture size is load-bearing; see doc");
    std::fs::write(&file, source).unwrap();

    // 1 + 2: raw bytes verbatim on stdout, and no lossy-view marker on stderr.
    let assert = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--mode=structure")
        .arg("--no-cache")
        .assert()
        .success();
    let out = assert.get_output();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        source,
        "a view that cannot pay for its own disclosure must serve raw bytes"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("[skim]"),
        "nothing was elided, so no class-1 marker may fire; got: {stderr:?}"
    );

    // 3: the guard is what decided — not a transform that happened to no-op.
    skim_cmd()
        .env("SKIM_DEBUG", "1")
        .arg(file.to_str().unwrap())
        .arg("--mode=structure")
        .arg("--no-cache")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim:guardrail]"));
}

/// LARGER file — the transform pays for its disclosure several times over, so
/// the compressed view and its marker both survive the charge.
///
/// Measured: 2537 B / 732 tokens raw; 698 B / 144 tokens under
/// `--mode=structure`. Charged, that is 698 + 76 = 774 B against 2537 B
/// (3.3x margin) and 144 + 22 = 166 tokens against 732 (4.4x margin). Both
/// margins clear the 2x bar, so neither a tokeniser patch bump nor a modest
/// marker-wording edit can flip this verdict.
///
/// The byte margin is asserted from the actual output rather than pinned to a
/// literal, so the test states the property it relies on instead of restating
/// a measurement that could silently stop being true.
#[test]
fn test_larger_file_pays_for_its_disclosure_and_stays_compressed() {
    /// Direct `structure` marker cost, pinned in `output/mod.rs`.
    const STRUCTURE_MARKER_BYTES: usize = 76;

    let dir = TempDir::new().unwrap();
    let file = dir.path().join("large.ts");
    let mut source = String::new();
    for i in 0..12u32 {
        let mult = i + 1;
        source.push_str(&format!(
            "export function compute{i}(input: number[]): number {{\n  \
             let total = 0;\n  for (const value of input) {{\n    \
             if (value > 0) {{\n      total += value * {mult};\n    \
             }} else {{\n      total -= value;\n    }}\n  }}\n  \
             return total;\n}}\n\n"
        ));
    }
    std::fs::write(&file, &source).unwrap();

    let assert = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--mode=structure")
        .arg("--no-cache")
        .assert()
        .success();
    let out = assert.get_output();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();

    assert_ne!(stdout, source, "the compressed view must be served");
    assert!(
        stdout.contains("{...}"),
        "structure mode replaces bodies; got: {stdout:?}"
    );
    assert!(
        source.len() >= 2 * (stdout.len() + STRUCTURE_MARKER_BYTES),
        "the fixture must clear the disclosure by at least 2x or this test \
         proves nothing about the charge: raw={} served={} marker={}",
        source.len(),
        stdout.len(),
        STRUCTURE_MARKER_BYTES
    );

    // The view IS lossy, so the class-1 marker is owed and must still fire.
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("structure view:"),
        "a lossy view still discloses; got: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The hook-rewritten read and the hand-typed read of the SAME file must not
/// share a cache entry — the end-to-end twin of
/// `cache::tests::test_cache_key_separates_hook_origin_from_direct_and_batch`.
///
/// This fixture sits inside the window where the two disagree. Measured:
/// 120 B / 90 tokens raw, 26 B / 7 tokens under `--mode=structure`.
///
/// | invocation | marker | charged bytes | verdict |
/// |---|---|---|---|
/// | `cat foo.ts` (origin `cat`) | 110 B | 26 + 110 = 136 >= 120 | RAW |
/// | `skim foo.ts --mode=structure` | 76 B | 26 + 76 = 102 < 120 | COMPRESSED |
///
/// Margins are 16 B over on the origin side and 18 B under on the direct side;
/// the token gate passes both (7 + 30 = 37 and 7 + 22 = 29, against 90), so the
/// discrimination is pure byte arithmetic over the pinned marker costs.
///
/// The origin read runs FIRST so it populates the cache. Without `notice_bytes`
/// in the cache key the second, direct read hits that entry and is served the
/// 120-byte raw source — the assertion below is what fails.
#[test]
fn test_hook_origin_and_direct_reads_do_not_share_a_cache_entry() {
    let cache_dir = TempDir::new().unwrap();
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("disc.ts");
    let source = "function f(): void { a=1;v0=0;v1=1;v2=2;v3=3;v4=4;v5=5;v6=6;\
                  v7=7;v8=8;v9=9;v10=10;v11=11;v12=12;v13=13;v14=14;v15=15; }\n";
    assert_eq!(
        source.len(),
        120,
        "fixture size is load-bearing: it must sit between the two marker costs"
    );
    std::fs::write(&file, source).unwrap();

    // Hook-rewritten read: the 110-byte origin marker outweighs the saving.
    let origin = skim_cmd()
        .env("SKIM_CACHE_DIR", cache_dir.path().as_os_str())
        .env("SKIM_REWRITTEN_FROM", "cat")
        .arg(file.to_str().unwrap())
        .arg("--mode=structure")
        .assert()
        .success();
    assert_eq!(
        String::from_utf8_lossy(&origin.get_output().stdout),
        source,
        "the cat-origin read pays a 110-byte disclosure and must serve raw"
    );

    // Direct read of the SAME file, same mode, same mtime — only the marker
    // cost differs, and it differs enough to flip the verdict.
    let direct = skim_cmd()
        .env("SKIM_CACHE_DIR", cache_dir.path().as_os_str())
        .arg(file.to_str().unwrap())
        .arg("--mode=structure")
        .assert()
        .success();
    let direct_stdout = String::from_utf8_lossy(&direct.get_output().stdout).into_owned();
    assert_ne!(
        direct_stdout, source,
        "the direct read pays only 76 bytes and must serve the compressed \
         view — receiving the raw source here means it was handed the \
         cat-origin entry, i.e. `notice_bytes` is missing from the cache key"
    );
    assert!(
        direct_stdout.contains("{...}"),
        "the direct read must serve the structure view; got: {direct_stdout:?}"
    );
}
