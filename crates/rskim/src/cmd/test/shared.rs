//! Shared helpers for test parser Tier-2 fallback paths.
//!
//! Provides [`scrape_failures`] which extracts failing test entries from
//! plain-text runner output when JSON parsing is unavailable.

use std::sync::LazyLock;

use regex::Regex;

use crate::output::canonical::{TestEntry, TestOutcome};

/// Identifies which test runner produced the text being scraped.
///
/// Each runner has a distinct output format for failed tests, so kind-sensitive
/// regex patterns are required to avoid false positives across runners.
///
/// Variants `Pytest` and `Go` are provided for completeness and future use.
/// Currently only `Cargo` and `Vitest` are consumed by Tier-2 regex paths;
/// `Go`'s Tier-2 already extracts test names directly and `Pytest` uses
/// passthrough for its Tier-2.
#[derive(Debug, Clone, Copy)]
pub(super) enum TestKind {
    /// `cargo test` plain-text format: `test <path> ... FAILED`
    Cargo,
    /// `pytest` plain-text format: `FAILED tests/test_foo.py::test_bar - ...`
    #[allow(dead_code)]
    Pytest,
    /// `go test` plain-text format: `--- FAIL: TestFoo (0.01s)`
    #[allow(dead_code)]
    Go,
    /// `vitest` / `jest` plain-text format: `✕ <describe> > <name>` or `✗ <name>`
    Vitest,
}

/// ANSI color-code strip pattern (ESC [ ... m sequences).
static RE_ANSI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*m").expect("valid ANSI regex"));

/// Per-kind failure patterns — compiled once.
static RE_CARGO_FAIL: LazyLock<Regex> = LazyLock::new(|| {
    // `test my_module::test_foo ... FAILED`
    Regex::new(r"^test\s+(\S+)\s+\.\.\.\s+FAILED").expect("valid cargo fail regex")
});

static RE_PYTEST_FAIL: LazyLock<Regex> = LazyLock::new(|| {
    // `FAILED tests/test_math.py::test_divide - ZeroDivisionError`
    Regex::new(r"^FAILED\s+(\S+)").expect("valid pytest fail regex")
});

static RE_GO_FAIL: LazyLock<Regex> = LazyLock::new(|| {
    // `--- FAIL: TestFoo (0.01s)`
    Regex::new(r"^--- FAIL:\s+(\S+)\s+\(").expect("valid go fail regex")
});

static RE_VITEST_FAIL: LazyLock<Regex> = LazyLock::new(|| {
    // AD-19 (2026-04-11): `^\s*` prefix added so real vitest output like
    // `   × divides by zero` matches. Without `\s*` the regex anchors at
    // column 0 and silently misses all indented failure lines. The fix was
    // surfaced by scrutinizer review (commit ea4e52f).
    //
    // `✕ describe > it name`, `✗ test name`, or `× test name` — with optional
    // leading whitespace because vitest and jest indent failing-test lines.
    // Example real output: `   × divides by zero`.
    Regex::new(r"^\s*[✕✗×]\s+(.+?)$").expect("valid vitest fail regex")
});

/// Extract failing test entries from plain-text runner output when JSON parsing
/// is unavailable (Tier 2 fallback).
///
/// # Design decision (Commit 2, 2026-04-11)
/// All four test handlers previously returned `vec![]` from their Tier-2 regex
/// paths, so LLMs saw `FAIL: 2` with zero failing-test names. Scraping names
/// additively preserves the name signal without inflating Tier-1 complexity.
/// Durations and messages stay `None` in Tier-2 — they would require parsing
/// the runner's full output format, which is precisely what Tier-1 JSON exists
/// to avoid.
///
/// Cap matches Tier-1's entry cap (100) to keep output size predictable
/// regardless of tier.
pub(super) fn scrape_failures(text: &str, kind: TestKind) -> Vec<TestEntry> {
    // Strip ANSI escape codes so color-enabled output (e.g. pytest --color=yes,
    // vitest with TTY detected) does not break pattern matching.
    //
    // Fast-path: when the caller has already stripped ANSI (no ESC bytes remain),
    // borrow the slice directly rather than running the regex over it a second time.
    // This eliminates the double-strip in `vitest::try_parse_regex`, which calls
    // `output::strip_ansi(raw)` → `cleaned` and then passes `cleaned` here.
    let cleaned: std::borrow::Cow<str> = if text.as_bytes().contains(&0x1b) {
        std::borrow::Cow::Owned(RE_ANSI.replace_all(text, "").into_owned())
    } else {
        std::borrow::Cow::Borrowed(text)
    };

    let re = match kind {
        TestKind::Cargo => &*RE_CARGO_FAIL,
        TestKind::Pytest => &*RE_PYTEST_FAIL,
        TestKind::Go => &*RE_GO_FAIL,
        TestKind::Vitest => &*RE_VITEST_FAIL,
    };

    let mut entries: Vec<TestEntry> = Vec::new();
    for line in cleaned.lines() {
        if entries.len() >= 100 {
            break;
        }
        if let Some(caps) = re.captures(line) {
            let name = caps[1].trim().to_string();
            if !name.is_empty() {
                entries.push(TestEntry {
                    name,
                    outcome: TestOutcome::Fail,
                    detail: None,
                });
            }
        }
    }

    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scrape_failures_cargo_basic() {
        let text = "test my_module::test_foo ... FAILED\n\
                    test other::test_bar ... ok\n";
        let entries = scrape_failures(text, TestKind::Cargo);
        assert!(
            !entries.is_empty(),
            "should find at least one failure: {entries:?}"
        );
        assert!(
            entries[0].name.contains("test_foo"),
            "first entry should be test_foo: {:?}",
            entries[0].name
        );
        assert_eq!(entries[0].outcome, TestOutcome::Fail);
    }

    #[test]
    fn test_scrape_failures_pytest_basic() {
        let text = "FAILED tests/test_math.py::test_divide - ZeroDivisionError\n\
                    FAILED tests/test_api.py::test_endpoint\n";
        let entries = scrape_failures(text, TestKind::Pytest);
        assert!(!entries.is_empty(), "should find failures: {entries:?}");
        assert!(
            entries[0].name.contains("test_divide"),
            "first entry: {:?}",
            entries[0].name
        );
    }

    #[test]
    fn test_scrape_failures_go_basic() {
        let text = "--- FAIL: TestFoo (0.01s)\n\
                    --- PASS: TestBar (0.00s)\n";
        let entries = scrape_failures(text, TestKind::Go);
        assert!(!entries.is_empty(), "should find TestFoo: {entries:?}");
        assert!(
            entries[0].name.contains("TestFoo"),
            "entry: {:?}",
            entries[0].name
        );
    }

    #[test]
    fn test_scrape_failures_vitest_basic() {
        let text = "✕ math > adds correctly\n\
                    ✓ math > multiplies\n";
        let entries = scrape_failures(text, TestKind::Vitest);
        assert!(
            !entries.is_empty(),
            "should find vitest failure: {entries:?}"
        );
        assert!(
            entries[0].name.contains("adds correctly"),
            "entry: {:?}",
            entries[0].name
        );
    }

    /// Regression: vitest indents failing-test lines with leading whitespace
    /// (e.g. `   × divides by zero`). The regex must tolerate optional
    /// leading whitespace so real vitest output matches, not just the
    /// hand-crafted unit fixture.
    #[test]
    fn test_scrape_failures_vitest_indented_failure_line() {
        let text = " ❯ src/utils.test.ts (3 tests | 1 failed)\n\
                     ✓ adds numbers\n\
                     ✓ subtracts numbers\n\
                     × divides by zero\n";
        let entries = scrape_failures(text, TestKind::Vitest);
        assert!(
            !entries.is_empty(),
            "indented vitest fail line must match: {entries:?}"
        );
        assert!(
            entries.iter().any(|e| e.name.contains("divides by zero")),
            "entries must contain 'divides by zero': {entries:?}"
        );
    }

    #[test]
    fn test_scrape_failures_ansi_stripped() {
        // Cargo output with ANSI color codes.
        let text = "\x1b[31mtest my_mod::test_colored ... FAILED\x1b[0m\n";
        let entries = scrape_failures(text, TestKind::Cargo);
        assert!(
            !entries.is_empty(),
            "ANSI-stripped output should still match: {entries:?}"
        );
        assert!(
            entries[0].name.contains("test_colored"),
            "name: {:?}",
            entries[0].name
        );
    }

    #[test]
    fn test_scrape_failures_cap_at_100() {
        // Build 200-failure fixture.
        let mut text = String::new();
        for i in 0..200 {
            text.push_str(&format!("test test_{i} ... FAILED\n"));
        }
        let entries = scrape_failures(&text, TestKind::Cargo);
        assert_eq!(
            entries.len(),
            100,
            "must be capped at 100: {}",
            entries.len()
        );
    }

    #[test]
    fn test_scrape_failures_no_matches_returns_empty() {
        let text = "test foo ... ok\ntest bar ... ok\n";
        let entries = scrape_failures(text, TestKind::Cargo);
        assert!(
            entries.is_empty(),
            "no failures should return empty: {entries:?}"
        );
    }
}
