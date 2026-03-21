//! Canonical output types for structured command results (#42)
//!
//! Provides strongly-typed output representations for test results, git operations,
//! and build results. Each type carries a pre-rendered `String` field computed in
//! constructors, with `Display` implementations that are compact on success and
//! verbose on failure.

use serde::{Deserialize, Serialize};
use std::fmt;

// ============================================================================
// TestResult types
// ============================================================================

/// Outcome of a single test case
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum TestOutcome {
    Pass,
    Fail,
    Skip,
}

impl fmt::Display for TestOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TestOutcome::Pass => write!(f, "PASS"),
            TestOutcome::Fail => write!(f, "FAIL"),
            TestOutcome::Skip => write!(f, "SKIP"),
        }
    }
}

/// A single test entry with its outcome and optional detail
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TestEntry {
    pub(crate) name: String,
    pub(crate) outcome: TestOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
}

/// Aggregate test summary statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TestSummary {
    pub(crate) pass: usize,
    pub(crate) fail: usize,
    pub(crate) skip: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) duration_ms: Option<u64>,
}

impl fmt::Display for TestSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "PASS: {} | FAIL: {} | SKIP: {}",
            self.pass, self.fail, self.skip
        )?;
        if let Some(ms) = self.duration_ms {
            write!(f, " | Duration: {}ms", format_with_commas(ms))?;
        }
        Ok(())
    }
}

/// Complete test result with summary, entries, and pre-rendered display
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TestResult {
    pub(crate) summary: TestSummary,
    pub(crate) entries: Vec<TestEntry>,
    #[serde(default)]
    rendered: String,
}

impl TestResult {
    /// Create a new TestResult with pre-computed rendered output
    pub(crate) fn new(summary: TestSummary, entries: Vec<TestEntry>) -> Self {
        let rendered = Self::render(&summary, &entries);
        Self {
            summary,
            entries,
            rendered,
        }
    }

    /// Recompute rendered field if empty (e.g., after deserialization)
    pub(crate) fn ensure_rendered(&mut self) {
        if self.rendered.is_empty() {
            self.rendered = Self::render(&self.summary, &self.entries);
        }
    }

    fn render(summary: &TestSummary, entries: &[TestEntry]) -> String {
        use std::fmt::Write;

        let mut output = format!("{summary}");

        if summary.fail > 0 {
            for entry in entries {
                if entry.outcome == TestOutcome::Fail {
                    let _ = write!(output, "\n  FAIL: {}", entry.name);
                    if let Some(detail) = &entry.detail {
                        let _ = write!(output, "\n        {detail}");
                    }
                }
            }
        }

        output
    }
}

impl AsRef<str> for TestResult {
    fn as_ref(&self) -> &str {
        &self.rendered
    }
}

impl fmt::Display for TestResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.rendered)
    }
}

// ============================================================================
// GitResult
// ============================================================================

/// Result of a git operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct GitResult {
    pub(crate) operation: String,
    pub(crate) summary: String,
    pub(crate) details: Vec<String>,
    #[serde(default)]
    rendered: String,
}

impl GitResult {
    /// Create a new GitResult with pre-computed rendered output
    pub(crate) fn new(operation: String, summary: String, details: Vec<String>) -> Self {
        let rendered = Self::render(&operation, &summary, &details);
        Self {
            operation,
            summary,
            details,
            rendered,
        }
    }

    /// Recompute rendered field if empty (e.g., after deserialization)
    pub(crate) fn ensure_rendered(&mut self) {
        if self.rendered.is_empty() {
            self.rendered = Self::render(&self.operation, &self.summary, &self.details);
        }
    }

    fn render(operation: &str, summary: &str, details: &[String]) -> String {
        use std::fmt::Write;

        let mut output = format!("[{operation}] {summary}");
        for detail in details {
            let _ = write!(output, "\n  {detail}");
        }
        output
    }
}

impl AsRef<str> for GitResult {
    fn as_ref(&self) -> &str {
        &self.rendered
    }
}

impl fmt::Display for GitResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.rendered)
    }
}

// ============================================================================
// BuildResult
// ============================================================================

/// Result of a build operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BuildResult {
    pub(crate) success: bool,
    pub(crate) warnings: usize,
    pub(crate) errors: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) duration_ms: Option<u64>,
    pub(crate) error_messages: Vec<String>,
    #[serde(default)]
    rendered: String,
}

impl BuildResult {
    /// Create a new BuildResult with pre-computed rendered output
    pub(crate) fn new(
        success: bool,
        warnings: usize,
        errors: usize,
        duration_ms: Option<u64>,
        error_messages: Vec<String>,
    ) -> Self {
        let rendered = Self::render(success, warnings, errors, duration_ms, &error_messages);
        Self {
            success,
            warnings,
            errors,
            duration_ms,
            error_messages,
            rendered,
        }
    }

    /// Recompute rendered field if empty (e.g., after deserialization)
    pub(crate) fn ensure_rendered(&mut self) {
        if self.rendered.is_empty() {
            self.rendered = Self::render(
                self.success,
                self.warnings,
                self.errors,
                self.duration_ms,
                &self.error_messages,
            );
        }
    }

    fn render(
        success: bool,
        warnings: usize,
        errors: usize,
        duration_ms: Option<u64>,
        error_messages: &[String],
    ) -> String {
        use std::fmt::Write;

        let status = if success { "BUILD OK" } else { "BUILD FAILED" };
        let mut output = format!("{status} | warnings: {warnings} | errors: {errors}");
        if let Some(ms) = duration_ms {
            let _ = write!(output, " | {}ms", format_with_commas(ms));
        }

        if !success {
            for msg in error_messages {
                let _ = write!(output, "\n  {msg}");
            }
        }

        output
    }
}

impl AsRef<str> for BuildResult {
    fn as_ref(&self) -> &str {
        &self.rendered
    }
}

impl fmt::Display for BuildResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.rendered)
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Format a u64 with comma-separated thousands.
///
/// Delegates to [`crate::tokens::format_number`] to avoid duplication.
fn format_with_commas(n: u64) -> String {
    crate::tokens::format_number(n as usize)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // TestSummary Display tests
    // ========================================================================

    #[test]
    fn test_summary_display_without_duration() {
        let summary = TestSummary {
            pass: 42,
            fail: 0,
            skip: 3,
            duration_ms: None,
        };
        assert_eq!(format!("{summary}"), "PASS: 42 | FAIL: 0 | SKIP: 3");
    }

    #[test]
    fn test_summary_display_with_duration() {
        let summary = TestSummary {
            pass: 42,
            fail: 0,
            skip: 3,
            duration_ms: Some(1234),
        };
        assert_eq!(
            format!("{summary}"),
            "PASS: 42 | FAIL: 0 | SKIP: 3 | Duration: 1,234ms"
        );
    }

    // ========================================================================
    // TestResult Display tests
    // ========================================================================

    #[test]
    fn test_result_display_all_passing() {
        let summary = TestSummary {
            pass: 3,
            fail: 0,
            skip: 0,
            duration_ms: Some(100),
        };
        let entries = vec![
            TestEntry {
                name: "test_a".to_string(),
                outcome: TestOutcome::Pass,
                detail: None,
            },
            TestEntry {
                name: "test_b".to_string(),
                outcome: TestOutcome::Pass,
                detail: None,
            },
            TestEntry {
                name: "test_c".to_string(),
                outcome: TestOutcome::Pass,
                detail: None,
            },
        ];
        let result = TestResult::new(summary, entries);
        // Compact on success: summary only
        assert_eq!(
            format!("{result}"),
            "PASS: 3 | FAIL: 0 | SKIP: 0 | Duration: 100ms"
        );
    }

    #[test]
    fn test_result_display_with_failures() {
        let summary = TestSummary {
            pass: 1,
            fail: 2,
            skip: 0,
            duration_ms: None,
        };
        let entries = vec![
            TestEntry {
                name: "test_a".to_string(),
                outcome: TestOutcome::Pass,
                detail: None,
            },
            TestEntry {
                name: "test_b".to_string(),
                outcome: TestOutcome::Fail,
                detail: Some("expected 2, got 3".to_string()),
            },
            TestEntry {
                name: "test_c".to_string(),
                outcome: TestOutcome::Fail,
                detail: None,
            },
        ];
        let result = TestResult::new(summary, entries);
        let display = format!("{result}");
        assert!(display.contains("PASS: 1 | FAIL: 2 | SKIP: 0"));
        assert!(display.contains("FAIL: test_b"));
        assert!(display.contains("expected 2, got 3"));
        assert!(display.contains("FAIL: test_c"));
    }

    // ========================================================================
    // Serde round-trip tests
    // ========================================================================

    #[test]
    fn test_result_serde_roundtrip() {
        let summary = TestSummary {
            pass: 5,
            fail: 1,
            skip: 2,
            duration_ms: Some(500),
        };
        let entries = vec![TestEntry {
            name: "test_x".to_string(),
            outcome: TestOutcome::Fail,
            detail: Some("boom".to_string()),
        }];
        let original = TestResult::new(summary, entries);
        let json = serde_json::to_string(&original).unwrap();
        let mut deserialized: TestResult = serde_json::from_str(&json).unwrap();
        // rendered may be empty after deserialization if not serialized fully
        deserialized.ensure_rendered();
        assert_eq!(format!("{original}"), format!("{deserialized}"));
    }

    #[test]
    fn test_git_result_serde_roundtrip() {
        let original = GitResult::new(
            "commit".to_string(),
            "3 files changed".to_string(),
            vec!["main.rs".to_string(), "lib.rs".to_string()],
        );
        let json = serde_json::to_string(&original).unwrap();
        let mut deserialized: GitResult = serde_json::from_str(&json).unwrap();
        deserialized.ensure_rendered();
        assert_eq!(format!("{original}"), format!("{deserialized}"));
    }

    #[test]
    fn test_build_result_serde_roundtrip() {
        let original = BuildResult::new(
            false,
            3,
            1,
            Some(2500),
            vec!["error[E0308]: mismatched types".to_string()],
        );
        let json = serde_json::to_string(&original).unwrap();
        let mut deserialized: BuildResult = serde_json::from_str(&json).unwrap();
        deserialized.ensure_rendered();
        assert_eq!(format!("{original}"), format!("{deserialized}"));
    }

    // ========================================================================
    // ensure_rendered tests
    // ========================================================================

    #[test]
    fn test_ensure_rendered_recomputes_when_empty() {
        // Simulate deserialization with empty rendered field
        let mut result = TestResult {
            summary: TestSummary {
                pass: 1,
                fail: 0,
                skip: 0,
                duration_ms: None,
            },
            entries: vec![],
            rendered: String::new(),
        };
        assert_eq!(result.as_ref(), "");
        result.ensure_rendered();
        assert_eq!(result.as_ref(), "PASS: 1 | FAIL: 0 | SKIP: 0");
    }

    // ========================================================================
    // GitResult Display tests
    // ========================================================================

    #[test]
    fn test_git_result_display() {
        let result = GitResult::new(
            "push".to_string(),
            "pushed to origin/main".to_string(),
            vec!["abc123 feat: add feature".to_string()],
        );
        let display = format!("{result}");
        assert!(display.contains("[push]"));
        assert!(display.contains("pushed to origin/main"));
        assert!(display.contains("abc123 feat: add feature"));
    }

    // ========================================================================
    // BuildResult Display tests
    // ========================================================================

    #[test]
    fn test_build_result_display_success() {
        let result = BuildResult::new(true, 2, 0, Some(1500), vec![]);
        let display = format!("{result}");
        assert!(display.contains("BUILD OK"));
        assert!(display.contains("warnings: 2"));
        assert!(display.contains("errors: 0"));
        assert!(display.contains("1,500ms"));
        // Success: no error messages listed
        assert!(!display.contains('\n'));
    }

    #[test]
    fn test_build_result_display_failure() {
        let result = BuildResult::new(
            false,
            0,
            2,
            None,
            vec![
                "error: type mismatch".to_string(),
                "error: unused variable".to_string(),
            ],
        );
        let display = format!("{result}");
        assert!(display.contains("BUILD FAILED"));
        assert!(display.contains("error: type mismatch"));
        assert!(display.contains("error: unused variable"));
    }

    // ========================================================================
    // format_with_commas tests
    // ========================================================================

    #[test]
    fn test_format_with_commas() {
        assert_eq!(format_with_commas(0), "0");
        assert_eq!(format_with_commas(999), "999");
        assert_eq!(format_with_commas(1000), "1,000");
        assert_eq!(format_with_commas(1234567), "1,234,567");
    }

    // ========================================================================
    // TestOutcome Display tests
    // ========================================================================

    #[test]
    fn test_outcome_display() {
        assert_eq!(format!("{}", TestOutcome::Pass), "PASS");
        assert_eq!(format!("{}", TestOutcome::Fail), "FAIL");
        assert_eq!(format!("{}", TestOutcome::Skip), "SKIP");
    }
}
