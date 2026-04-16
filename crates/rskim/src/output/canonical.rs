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
///
/// # AD-GIT-12 (2026-04-11) — parse_tier propagation through git handlers
///
/// Git handlers (log, status, fetch) each use a single parsing strategy
/// (unlike lint handlers which have Full/Degraded/Passthrough tiers). To make
/// the analytics DB consistent with the file/lint/infra command families, we
/// add a `parse_tier` field that each parser can tag with its tier name.
///
/// The field is skipped during serialization because it is a runtime annotation,
/// not part of the persistent JSON schema. Callers that do not set it default to
/// `None`, which is stored in the analytics DB as NULL (pre-existing behaviour).
///
/// # Relationship to `ParseResult::tier_name()` (parallel mechanism, not equivalent)
///
/// Non-git command families (lint, file, infra, pkg, test) convey their parse tier
/// via [`crate::output::ParseResult`] — specifically `ParseResult::tier_name()` —
/// which wraps the entire parsed value and is consumed by the generic
/// `finalize_output` / `record_analytics` paths.  Git handlers do not use
/// `ParseResult`; they call `finalize_git_output*` directly and carry the tier
/// annotation on `GitResult::parse_tier` instead.
///
/// These two mechanisms are parallel, not equivalent.
/// TECH_DEBT: Unifying them would require git handlers to wrap `GitResult` in
/// `ParseResult`, which would change the git analytics recording path. That
/// unification is tracked separately and not attempted in this batch.
///
/// # Call sites (files that call `.with_tier(...)`)
///
/// - `crates/rskim/src/cmd/git/log.rs`
/// - `crates/rskim/src/cmd/git/status.rs` (if present)
/// - `crates/rskim/src/cmd/git/fetch.rs`
/// - `crates/rskim/src/cmd/git/diff/mod.rs`
/// - `crates/rskim/src/cmd/git/show.rs`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct GitResult {
    pub(crate) operation: String,
    pub(crate) summary: String,
    pub(crate) details: Vec<String>,
    #[serde(default)]
    rendered: String,
    /// Parse tier label (e.g., `"full"`, `"degraded"`, `"passthrough"`).
    ///
    /// Skipped in JSON serialization — this is a runtime annotation for the
    /// analytics layer, not a schema field.
    #[serde(skip)]
    pub(crate) parse_tier: Option<&'static str>,
}

impl GitResult {
    /// Create a new GitResult with pre-computed rendered output.
    ///
    /// `parse_tier` defaults to `None`. Use [`GitResult::with_tier`] to set it.
    pub(crate) fn new(operation: String, summary: String, details: Vec<String>) -> Self {
        let rendered = Self::render(&operation, &summary, &details);
        Self {
            operation,
            summary,
            details,
            rendered,
            parse_tier: None,
        }
    }

    /// Set the parse tier and return `self` for builder-style chaining.
    ///
    /// ```ignore
    /// GitResult::new("log".to_string(), summary, lines).with_tier("full")
    /// ```
    pub(crate) fn with_tier(mut self, tier: &'static str) -> Self {
        self.parse_tier = Some(tier);
        self
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
// LintResult types
// ============================================================================

/// Severity level for a lint issue
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum LintSeverity {
    Error,
    Warning,
    Info,
}

impl fmt::Display for LintSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LintSeverity::Error => write!(f, "error"),
            LintSeverity::Warning => write!(f, "warning"),
            LintSeverity::Info => write!(f, "info"),
        }
    }
}

/// A single lint issue from any linter
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LintIssue {
    pub(crate) file: String,
    pub(crate) line: u32,
    pub(crate) rule: String,
    pub(crate) message: String,
    pub(crate) severity: LintSeverity,
}

/// A group of lint issues sharing the same rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LintGroup {
    pub(crate) rule: String,
    pub(crate) count: usize,
    pub(crate) severity: LintSeverity,
    pub(crate) locations: Vec<String>,
}

/// Complete lint result with summary and grouped issues
///
/// # AD-LINT-22 (2026-04-15) — files_formatted field for format-mode results
///
/// Format-mode runs (e.g., `ruff format`, `prettier --write`, `cargo fmt`) do not
/// produce lint issues — they rewrite files. `files_formatted: Some(N)` signals that
/// N files were reformatted; `None` means this is a check-mode result (existing
/// behaviour unchanged).
///
/// # AD-LINT-23 (2026-04-15) — render() output for format-mode
///
/// When `files_formatted.is_some()` AND `errors == 0 && warnings == 0`:
/// → `LINT OK | {tool} ({N} files formatted)`
///
/// When `files_formatted.is_some()` AND there are issues (i.e., check mode found
/// problems while files_formatted is also set — unlikely but handled):
/// → standard issue output, `files_formatted` ignored in render.
///
/// When `files_formatted.is_none()`: existing render behaviour unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LintResult {
    pub(crate) tool: String,
    pub(crate) errors: usize,
    pub(crate) warnings: usize,
    pub(crate) groups: Vec<LintGroup>,
    /// Number of files reformatted by a format-mode run. `None` for check-mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) files_formatted: Option<usize>,
    #[serde(default)]
    rendered: String,
}

// ============================================================================
// PkgResult types
// ============================================================================

/// Package operation type with operation-specific data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum PkgOperation {
    Install {
        added: usize,
        removed: usize,
        changed: usize,
        warnings: usize,
    },
    Audit {
        critical: usize,
        high: usize,
        moderate: usize,
        low: usize,
        total: usize,
    },
    Outdated {
        count: usize,
    },
    Check {
        issues: usize,
    },
    List {
        total: usize,
        flagged: usize,
    },
}

/// Complete package manager result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PkgResult {
    pub(crate) tool: String,
    pub(crate) operation: PkgOperation,
    pub(crate) success: bool,
    pub(crate) details: Vec<String>,
    #[serde(default)]
    rendered: String,
}

impl LintResult {
    /// Create a new check-mode LintResult with pre-computed rendered output.
    ///
    /// Pass `files_formatted: None` for all check-mode results. For format-mode
    /// results (where files were rewritten), use `LintResult::formatted()` instead.
    pub(crate) fn new(
        tool: String,
        errors: usize,
        warnings: usize,
        groups: Vec<LintGroup>,
    ) -> Self {
        let rendered = Self::render(&tool, errors, warnings, None, &groups);
        Self {
            tool,
            errors,
            warnings,
            groups,
            files_formatted: None,
            rendered,
        }
    }

    /// Create a format-mode LintResult recording how many files were reformatted.
    ///
    /// # AD-LINT-22 (2026-04-15) — format-mode constructor
    ///
    /// Used by `ruff format`, `prettier --write`, and `rustfmt` (format mode) parsers
    /// to signal that N files were reformatted. Rendered as
    /// `LINT OK | {tool} ({N} files formatted)` when no issues are present.
    pub(crate) fn formatted(tool: String, files_formatted: usize) -> Self {
        let rendered = Self::render(&tool, 0, 0, Some(files_formatted), &[]);
        Self {
            tool,
            errors: 0,
            warnings: 0,
            groups: vec![],
            files_formatted: Some(files_formatted),
            rendered,
        }
    }

    /// Recompute rendered field if empty (e.g., after deserialization)
    pub(crate) fn ensure_rendered(&mut self) {
        if self.rendered.is_empty() {
            self.rendered = Self::render(
                &self.tool,
                self.errors,
                self.warnings,
                self.files_formatted,
                &self.groups,
            );
        }
    }

    fn render(
        tool: &str,
        errors: usize,
        warnings: usize,
        files_formatted: Option<usize>,
        groups: &[LintGroup],
    ) -> String {
        use std::fmt::Write;

        let total = errors + warnings;
        if total == 0 {
            // AD-LINT-23: format-mode success includes file count
            if let Some(n) = files_formatted {
                return format!("LINT OK | {tool} ({n} files formatted)");
            }
            return format!("LINT OK | {tool} | 0 issues");
        }

        let mut output = format!("LINT: {errors} errors, {warnings} warnings | {tool}");
        for group in groups {
            let suffix = if group.count == 1 { "" } else { "s" };
            let _ = write!(
                output,
                "\n  {} ({} {}{suffix}):",
                group.rule, group.count, group.severity
            );
            for loc in &group.locations {
                let _ = write!(output, "\n    {loc}");
            }
        }

        output
    }
}

impl PkgResult {
    /// Create a new PkgResult with pre-computed rendered output
    pub(crate) fn new(
        tool: String,
        operation: PkgOperation,
        success: bool,
        details: Vec<String>,
    ) -> Self {
        let rendered = Self::render(&tool, &operation, &details);
        Self {
            tool,
            operation,
            success,
            details,
            rendered,
        }
    }

    /// Recompute rendered field if empty (e.g., after deserialization)
    pub(crate) fn ensure_rendered(&mut self) {
        if self.rendered.is_empty() {
            self.rendered = Self::render(&self.tool, &self.operation, &self.details);
        }
    }

    fn render(tool: &str, operation: &PkgOperation, details: &[String]) -> String {
        use std::fmt::Write;

        let mut output = match operation {
            PkgOperation::Install {
                added,
                removed,
                changed,
                warnings,
            } => {
                format!(
                    "PKG INSTALL | {tool} | added: {added} | removed: {removed} | changed: {changed} | warnings: {warnings}"
                )
            }
            PkgOperation::Audit {
                critical,
                high,
                moderate,
                low,
                total,
            } => {
                format!(
                    "PKG AUDIT | {tool} | critical: {critical} | high: {high} | moderate: {moderate} | low: {low} | total: {total}"
                )
            }
            PkgOperation::Outdated { count } => {
                format!("PKG OUTDATED | {tool} | {count} packages")
            }
            PkgOperation::Check { issues } => {
                format!("PKG CHECK | {tool} | {issues} issues")
            }
            PkgOperation::List { total, flagged } => {
                format!("PKG LIST | {tool} | {total} total | {flagged} flagged")
            }
        };

        for detail in details {
            let _ = write!(output, "\n  {detail}");
        }

        output
    }
}

impl AsRef<str> for LintResult {
    fn as_ref(&self) -> &str {
        &self.rendered
    }
}

impl AsRef<str> for PkgResult {
    fn as_ref(&self) -> &str {
        &self.rendered
    }
}

impl fmt::Display for LintResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.rendered)
    }
}

impl fmt::Display for PkgResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.rendered)
    }
}

// ============================================================================
// InfraResult types
// ============================================================================

/// Result of an infrastructure tool operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct InfraResult {
    pub(crate) tool: String,
    pub(crate) operation: String,
    pub(crate) summary: String,
    pub(crate) items: Vec<InfraItem>,
    #[serde(default)]
    rendered: String,
}

/// A single key-value item within an infrastructure result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct InfraItem {
    pub(crate) label: String,
    pub(crate) value: String,
}

impl InfraResult {
    pub(crate) fn new(
        tool: String,
        operation: String,
        summary: String,
        items: Vec<InfraItem>,
    ) -> Self {
        let rendered = Self::render(&tool, &operation, &summary, &items);
        Self {
            tool,
            operation,
            summary,
            items,
            rendered,
        }
    }

    /// Recompute rendered field if empty (e.g., after deserialization)
    pub(crate) fn ensure_rendered(&mut self) {
        if self.rendered.is_empty() {
            self.rendered = Self::render(&self.tool, &self.operation, &self.summary, &self.items);
        }
    }

    fn render(tool: &str, operation: &str, summary: &str, items: &[InfraItem]) -> String {
        use std::fmt::Write;
        let mut output = format!("INFRA: {tool} {operation} | {summary}");
        for item in items {
            let _ = write!(output, "\n  {}: {}", item.label, item.value);
        }
        output
    }
}

impl AsRef<str> for InfraResult {
    fn as_ref(&self) -> &str {
        &self.rendered
    }
}

impl fmt::Display for InfraResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.rendered)
    }
}

// ============================================================================
// DiffResult types
// ============================================================================

/// Status of a file in a diff
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum DiffFileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Binary,
}

impl fmt::Display for DiffFileStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiffFileStatus::Added => write!(f, "added"),
            DiffFileStatus::Modified => write!(f, "modified"),
            DiffFileStatus::Deleted => write!(f, "deleted"),
            DiffFileStatus::Renamed => write!(f, "renamed"),
            DiffFileStatus::Binary => write!(f, "binary"),
        }
    }
}

/// A single file entry within a diff result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DiffFileEntry {
    pub(crate) path: String,
    pub(crate) status: DiffFileStatus,
    pub(crate) changed_regions: usize,
}

/// Complete diff result with file entries and pre-rendered display
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DiffResult {
    #[serde(default)]
    pub(crate) files_changed: usize,
    pub(crate) files: Vec<DiffFileEntry>,
    #[serde(default)]
    rendered: String,
}

impl DiffResult {
    /// Create a new DiffResult with pre-computed rendered output
    pub(crate) fn new(files: Vec<DiffFileEntry>, rendered: String) -> Self {
        let files_changed = files.len();
        Self {
            files_changed,
            files,
            rendered,
        }
    }

    /// Consume `self` and return the pre-rendered text, avoiding a clone.
    ///
    /// Prefer this over `to_string()` at call sites that own the result and do
    /// not need the other fields afterwards.  The `Display` impl re-runs a
    /// `write!` through the formatter, which allocates; this method is zero-copy.
    pub(crate) fn into_rendered(self) -> String {
        self.rendered
    }

    /// Recompute rendered field if empty (e.g., after deserialization)
    pub(crate) fn ensure_rendered(&mut self) {
        if self.rendered.is_empty() {
            // Re-render from file entries as a summary fallback
            use std::fmt::Write;
            let mut output = format!("[diff] {} files changed", self.files_changed);
            for file in &self.files {
                let _ = write!(
                    output,
                    "\n  {} ({}, {} regions)",
                    file.path, file.status, file.changed_regions
                );
            }
            self.rendered = output;
        }
    }
}

impl AsRef<str> for DiffResult {
    fn as_ref(&self) -> &str {
        &self.rendered
    }
}

impl fmt::Display for DiffResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.rendered)
    }
}

// ============================================================================
// FileResult types
// ============================================================================

/// Result of a file operations tool (find, ls, tree, grep, rg)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FileResult {
    pub(crate) tool: String,
    pub(crate) total_count: usize,
    pub(crate) shown_count: usize,
    pub(crate) entries: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) footer: Option<String>,
    #[serde(default)]
    rendered: String,
}

impl FileResult {
    /// Create a new FileResult with pre-computed rendered output.
    pub(crate) fn new(
        tool: String,
        total_count: usize,
        shown_count: usize,
        entries: Vec<String>,
        footer: Option<String>,
    ) -> Self {
        let rendered = Self::render(&tool, total_count, shown_count, &entries, footer.as_deref());
        Self {
            tool,
            total_count,
            shown_count,
            entries,
            footer,
            rendered,
        }
    }

    /// Recompute rendered field if empty (e.g., after deserialization).
    pub(crate) fn ensure_rendered(&mut self) {
        if self.rendered.is_empty() {
            self.rendered = Self::render(
                &self.tool,
                self.total_count,
                self.shown_count,
                &self.entries,
                self.footer.as_deref(),
            );
        }
    }

    fn render(
        tool: &str,
        total_count: usize,
        shown_count: usize,
        entries: &[String],
        footer: Option<&str>,
    ) -> String {
        use std::fmt::Write;

        let tool_upper = tool.to_uppercase();
        let mut output =
            format!("{tool_upper}: {tool} | {total_count} entries (showing {shown_count})");
        for entry in entries {
            let _ = write!(output, "\n  {entry}");
        }
        if let Some(f) = footer {
            let _ = write!(output, "\n  {f}");
        }
        output
    }
}

impl AsRef<str> for FileResult {
    fn as_ref(&self) -> &str {
        &self.rendered
    }
}

impl fmt::Display for FileResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.rendered)
    }
}

// ============================================================================
// LogResult types
// ============================================================================

/// A single log entry with optional level and deduplication count
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LogEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) level: Option<String>,
    pub(crate) message: String,
    pub(crate) count: usize,
}

/// Result of log compression
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LogResult {
    pub(crate) total_lines: usize,
    pub(crate) unique_messages: usize,
    pub(crate) debug_hidden: usize,
    pub(crate) deduplicated_count: usize,
    pub(crate) entries: Vec<LogEntry>,
    /// True when --debug-only mode was requested.
    #[serde(default)]
    pub(crate) debug_only: bool,
    /// Number of stack frames elided from all captured traces (last 3 per trace kept).
    ///
    /// # AD-LOG-10 (2026-04-11)
    /// When non-zero, a `(+{n} stack frames elided)` footer is appended to the
    /// text render so agents know stack traces were truncated. Zero means either
    /// no traces were encountered or all frames fit within the 3-frame window.
    #[serde(default)]
    pub(crate) stack_frames_elided: usize,
    #[serde(default)]
    rendered: String,
}

impl LogResult {
    /// Create a new LogResult with pre-computed rendered output.
    ///
    /// `stack_frames_elided` is the total number of stack frames dropped across
    /// all captured traces (0 when no elision occurred). See AD-LOG-10.
    pub(crate) fn new(
        total_lines: usize,
        unique_messages: usize,
        debug_hidden: usize,
        deduplicated_count: usize,
        entries: Vec<LogEntry>,
        debug_only: bool,
    ) -> Self {
        Self::new_with_stack(
            total_lines,
            unique_messages,
            debug_hidden,
            deduplicated_count,
            entries,
            debug_only,
            0,
        )
    }

    /// Create a new LogResult with stack frame elision count.
    pub(crate) fn new_with_stack(
        total_lines: usize,
        unique_messages: usize,
        debug_hidden: usize,
        deduplicated_count: usize,
        entries: Vec<LogEntry>,
        debug_only: bool,
        stack_frames_elided: usize,
    ) -> Self {
        let rendered = Self::render(
            total_lines,
            unique_messages,
            debug_hidden,
            deduplicated_count,
            &entries,
            debug_only,
            stack_frames_elided,
        );
        Self {
            total_lines,
            unique_messages,
            debug_hidden,
            deduplicated_count,
            entries,
            debug_only,
            stack_frames_elided,
            rendered,
        }
    }

    /// Recompute rendered field if empty (e.g., after deserialization).
    pub(crate) fn ensure_rendered(&mut self) {
        if self.rendered.is_empty() {
            self.rendered = Self::render(
                self.total_lines,
                self.unique_messages,
                self.debug_hidden,
                self.deduplicated_count,
                &self.entries,
                self.debug_only,
                self.stack_frames_elided,
            );
        }
    }

    fn render(
        total_lines: usize,
        unique_messages: usize,
        debug_hidden: usize,
        deduplicated_count: usize,
        entries: &[LogEntry],
        debug_only: bool,
        stack_frames_elided: usize,
    ) -> String {
        use std::fmt::Write;

        let mut output = if debug_only {
            format!("LOG DEBUG: {debug_hidden} debug lines")
        } else {
            format!(
                "LOG: {total_lines} lines \u{2192} {unique_messages} unique ({deduplicated_count} duplicates removed)"
            )
        };

        if !debug_only && debug_hidden > 0 {
            let _ = write!(
                output,
                "\n[notice] {debug_hidden} DEBUG lines hidden. To see debug output: skim log --debug-only"
            );
        }

        for entry in entries {
            match &entry.level {
                Some(level) => {
                    if entry.count > 1 {
                        let _ = write!(
                            output,
                            "\n  [{level}] {} (\u{d7}{})",
                            entry.message, entry.count
                        );
                    } else {
                        let _ = write!(output, "\n  [{level}] {}", entry.message);
                    }
                }
                None => {
                    if entry.count > 1 {
                        let _ = write!(output, "\n  {} (\u{d7}{})", entry.message, entry.count);
                    } else {
                        let _ = write!(output, "\n  {}", entry.message);
                    }
                }
            }
        }

        // AD-LOG-10: append elision footer when frames were truncated.
        if stack_frames_elided > 0 {
            let _ = write!(output, "\n(+{stack_frames_elided} stack frames elided)");
        }

        output
    }
}

impl AsRef<str> for LogResult {
    fn as_ref(&self) -> &str {
        &self.rendered
    }
}

impl fmt::Display for LogResult {
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

    /// AD-GIT-12: `GitResult::new` must default `parse_tier` to `None` so callers
    /// that do not set a tier do not crash and the analytics DB records NULL.
    #[test]
    fn test_git_result_parse_tier_defaults_to_none() {
        let result = GitResult::new("op".to_string(), "summary".to_string(), vec![]);
        assert_eq!(
            result.parse_tier, None,
            "parse_tier must default to None for backwards-compat callers"
        );
    }

    /// AD-GIT-12: `GitResult::with_tier` must set the parse tier without affecting
    /// other fields or the rendered output.
    #[test]
    fn test_git_result_with_tier_sets_tier() {
        let result =
            GitResult::new("log".to_string(), "5 commits".to_string(), vec![]).with_tier("full");
        assert_eq!(
            result.parse_tier,
            Some("full"),
            "with_tier must store the tier name"
        );
        // Other fields must not be disturbed.
        assert_eq!(result.operation, "log");
        assert_eq!(result.summary, "5 commits");
    }

    /// AD-GIT-12: `parse_tier` must be skipped during JSON serialization because it
    /// is a runtime annotation, not a persistent schema field.
    #[test]
    fn test_git_result_parse_tier_skipped_in_json() {
        let result =
            GitResult::new("log".to_string(), "3 commits".to_string(), vec![]).with_tier("full");
        let json = serde_json::to_string(&result).unwrap();
        assert!(
            !json.contains("parse_tier"),
            "parse_tier must not appear in JSON serialization: {json}"
        );
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

    // ========================================================================
    // LintResult Display tests
    // ========================================================================

    #[test]
    fn test_lint_result_display_clean() {
        let result = LintResult::new("eslint".to_string(), 0, 0, vec![]);
        assert_eq!(format!("{result}"), "LINT OK | eslint | 0 issues");
    }

    #[test]
    fn test_lint_result_display_with_issues() {
        let groups = vec![
            LintGroup {
                rule: "no-unused-vars".to_string(),
                count: 3,
                severity: LintSeverity::Warning,
                locations: vec![
                    "src/api/auth.ts:12".to_string(),
                    "src/api/users.ts:34".to_string(),
                    "src/utils/format.ts:8".to_string(),
                ],
            },
            LintGroup {
                rule: "@typescript-eslint/no-explicit-any".to_string(),
                count: 2,
                severity: LintSeverity::Error,
                locations: vec![
                    "src/types.ts:45".to_string(),
                    "src/api/client.ts:89".to_string(),
                ],
            },
        ];
        let result = LintResult::new("eslint".to_string(), 2, 3, groups);
        let display = format!("{result}");
        assert!(display.contains("LINT: 2 errors, 3 warnings | eslint"));
        assert!(display.contains("no-unused-vars (3 warnings):"));
        assert!(display.contains("src/api/auth.ts:12"));
        assert!(display.contains("@typescript-eslint/no-explicit-any (2 errors):"));
        assert!(display.contains("src/types.ts:45"));
    }

    #[test]
    fn test_lint_result_serde_roundtrip() {
        let groups = vec![LintGroup {
            rule: "F401".to_string(),
            count: 2,
            severity: LintSeverity::Error,
            locations: vec!["src/main.py:1".to_string(), "src/main.py:2".to_string()],
        }];
        let original = LintResult::new("ruff".to_string(), 2, 0, groups);
        let json = serde_json::to_string(&original).unwrap();
        let mut deserialized: LintResult = serde_json::from_str(&json).unwrap();
        deserialized.ensure_rendered();
        assert_eq!(format!("{original}"), format!("{deserialized}"));
    }

    // ========================================================================
    // PkgResult Display tests
    // ========================================================================

    #[test]
    fn test_pkg_install_display() {
        let result = PkgResult::new(
            "npm".to_string(),
            PkgOperation::Install {
                added: 5,
                removed: 1,
                changed: 2,
                warnings: 3,
            },
            true,
            vec![],
        );
        let display = format!("{result}");
        assert!(display.contains("PKG INSTALL | npm"));
        assert!(display.contains("added: 5"));
        assert!(display.contains("removed: 1"));
        assert!(display.contains("changed: 2"));
        assert!(display.contains("warnings: 3"));
    }

    #[test]
    fn test_pkg_audit_display() {
        let result = PkgResult::new(
            "npm".to_string(),
            PkgOperation::Audit {
                critical: 0,
                high: 2,
                moderate: 3,
                low: 1,
                total: 6,
            },
            true,
            vec!["lodash: Prototype Pollution (high)".to_string()],
        );
        let display = format!("{result}");
        assert!(display.contains("PKG AUDIT | npm"));
        assert!(display.contains("critical: 0"));
        assert!(display.contains("high: 2"));
        assert!(display.contains("total: 6"));
        assert!(display.contains("lodash: Prototype Pollution (high)"));
    }

    #[test]
    fn test_pkg_outdated_display() {
        let result = PkgResult::new(
            "npm".to_string(),
            PkgOperation::Outdated { count: 4 },
            true,
            vec!["lodash 4.17.20 -> 4.17.21".to_string()],
        );
        let display = format!("{result}");
        assert!(display.contains("PKG OUTDATED | npm | 4 packages"));
        assert!(display.contains("lodash 4.17.20 -> 4.17.21"));
    }

    #[test]
    fn test_pkg_check_display() {
        let result = PkgResult::new(
            "pip".to_string(),
            PkgOperation::Check { issues: 3 },
            false,
            vec!["flask requires werkzeug>=3.0.1".to_string()],
        );
        let display = format!("{result}");
        assert!(display.contains("PKG CHECK | pip | 3 issues"));
        assert!(display.contains("flask requires werkzeug>=3.0.1"));
    }

    #[test]
    fn test_pkg_list_display() {
        let result = PkgResult::new(
            "npm".to_string(),
            PkgOperation::List {
                total: 42,
                flagged: 2,
            },
            true,
            vec!["debug@4.3.4: invalid".to_string()],
        );
        let display = format!("{result}");
        assert!(display.contains("PKG LIST | npm | 42 total | 2 flagged"));
        assert!(display.contains("debug@4.3.4: invalid"));
    }

    #[test]
    fn test_pkg_result_serde_roundtrip() {
        let original = PkgResult::new(
            "npm".to_string(),
            PkgOperation::Audit {
                critical: 1,
                high: 2,
                moderate: 0,
                low: 0,
                total: 3,
            },
            true,
            vec!["advisory detail".to_string()],
        );
        let json = serde_json::to_string(&original).unwrap();
        let mut deserialized: PkgResult = serde_json::from_str(&json).unwrap();
        deserialized.ensure_rendered();
        assert_eq!(format!("{original}"), format!("{deserialized}"));
    }

    #[test]
    fn test_lint_result_ensure_rendered_recomputes_when_empty() {
        let mut result = LintResult {
            tool: "mypy".to_string(),
            errors: 0,
            warnings: 0,
            groups: vec![],
            files_formatted: None,
            rendered: String::new(),
        };
        assert_eq!(result.as_ref(), "");
        result.ensure_rendered();
        assert_eq!(result.as_ref(), "LINT OK | mypy | 0 issues");
    }

    /// AD-LINT-22/AD-LINT-23 (2026-04-15) — LintResult::formatted() and format-mode render.
    #[test]
    fn test_lint_result_formatted_zero_files() {
        let result = LintResult::formatted("rustfmt".to_string(), 0);
        assert_eq!(
            format!("{result}"),
            "LINT OK | rustfmt (0 files formatted)",
            "Zero-file format run should render with count"
        );
    }

    #[test]
    fn test_lint_result_formatted_multiple_files() {
        let result = LintResult::formatted("ruff".to_string(), 5);
        assert_eq!(format!("{result}"), "LINT OK | ruff (5 files formatted)",);
        assert_eq!(result.files_formatted, Some(5));
        assert_eq!(result.errors, 0);
        assert_eq!(result.warnings, 0);
    }

    #[test]
    fn test_lint_result_check_mode_unaffected() {
        // Existing check-mode callers using LintResult::new() should produce
        // the same output as before (no files_formatted suffix).
        let result = LintResult::new("prettier".to_string(), 0, 0, vec![]);
        assert_eq!(format!("{result}"), "LINT OK | prettier | 0 issues");
        assert_eq!(result.files_formatted, None);
    }

    #[test]
    fn test_lint_result_formatted_serde_roundtrip() {
        let original = LintResult::formatted("prettier".to_string(), 3);
        let json = serde_json::to_string(&original).unwrap();
        let mut deserialized: LintResult = serde_json::from_str(&json).unwrap();
        deserialized.ensure_rendered();
        assert_eq!(format!("{original}"), format!("{deserialized}"));
        assert_eq!(deserialized.files_formatted, Some(3));
    }

    #[test]
    fn test_lint_severity_display() {
        assert_eq!(format!("{}", LintSeverity::Error), "error");
        assert_eq!(format!("{}", LintSeverity::Warning), "warning");
        assert_eq!(format!("{}", LintSeverity::Info), "info");
    }

    #[test]
    fn test_pkg_ensure_rendered_recomputes_when_empty() {
        let mut result = PkgResult {
            tool: "pip".to_string(),
            operation: PkgOperation::Check { issues: 0 },
            success: true,
            details: vec![],
            rendered: String::new(),
        };
        assert_eq!(result.as_ref(), "");
        result.ensure_rendered();
        assert_eq!(result.as_ref(), "PKG CHECK | pip | 0 issues");
    }

    // ========================================================================
    // DiffResult ensure_rendered lossy fallback (#103 review batch-7)
    // ========================================================================

    // ========================================================================
    // InfraResult tests
    // ========================================================================

    #[test]
    fn test_infra_result_display() {
        let items = vec![
            InfraItem {
                label: "#1".to_string(),
                value: "fix: update deps (open)".to_string(),
            },
            InfraItem {
                label: "#2".to_string(),
                value: "feat: add feature (merged)".to_string(),
            },
        ];
        let result = InfraResult::new(
            "gh".to_string(),
            "pr list".to_string(),
            "2 items".to_string(),
            items,
        );
        let display = format!("{result}");
        assert!(display.contains("INFRA: gh pr list | 2 items"));
        assert!(display.contains("#1: fix: update deps (open)"));
        assert!(display.contains("#2: feat: add feature (merged)"));
    }

    #[test]
    fn test_infra_result_serde_roundtrip() {
        let items = vec![InfraItem {
            label: "bucket".to_string(),
            value: "my-bucket".to_string(),
        }];
        let original = InfraResult::new(
            "aws".to_string(),
            "s3 ls".to_string(),
            "1 bucket".to_string(),
            items,
        );
        let json = serde_json::to_string(&original).unwrap();
        let mut deserialized: InfraResult = serde_json::from_str(&json).unwrap();
        deserialized.ensure_rendered();
        assert_eq!(format!("{original}"), format!("{deserialized}"));
    }

    #[test]
    fn test_infra_result_ensure_rendered_recomputes_when_empty() {
        let mut result = InfraResult {
            tool: "curl".to_string(),
            operation: "GET".to_string(),
            summary: "200 OK".to_string(),
            items: vec![],
            rendered: String::new(),
        };
        assert_eq!(result.as_ref(), "");
        result.ensure_rendered();
        assert!(result.as_ref().contains("INFRA: curl GET | 200 OK"));
    }

    #[test]
    fn test_diff_result_ensure_rendered_produces_summary_fallback() {
        // When `rendered` is empty (e.g., after deserialization that strips the
        // rendered field), `ensure_rendered` produces a *lossy* summary: only
        // file paths, statuses, and region counts -- not the full diff content.
        // This is intentional: the rendered field is the source of truth; the
        // fallback exists solely to provide a human-readable overview.
        let mut result = DiffResult::new(
            vec![
                DiffFileEntry {
                    path: "src/main.rs".to_string(),
                    status: DiffFileStatus::Modified,
                    changed_regions: 3,
                },
                DiffFileEntry {
                    path: "src/lib.rs".to_string(),
                    status: DiffFileStatus::Added,
                    changed_regions: 1,
                },
            ],
            String::new(), // simulate empty rendered field
        );

        result.ensure_rendered();
        let output = result.as_ref();

        // Summary header
        assert!(
            output.starts_with("[diff] 2 files changed"),
            "expected summary header, got: {output}"
        );
        // Per-file entries with status and region counts
        assert!(
            output.contains("src/main.rs (modified, 3 regions)"),
            "expected main.rs entry, got: {output}"
        );
        assert!(
            output.contains("src/lib.rs (added, 1 regions)"),
            "expected lib.rs entry, got: {output}"
        );
        // Intentionally does NOT contain actual diff hunks -- this is the lossy
        // nature of the fallback.
        assert!(
            !output.contains('+') && !output.contains('-'),
            "fallback should not contain diff markers"
        );
    }

    // ========================================================================
    // FileResult tests
    // ========================================================================

    #[test]
    fn test_file_result_display_basic() {
        let result = FileResult::new(
            "find".to_string(),
            5,
            5,
            vec![
                "./src/main.rs".to_string(),
                "./src/lib.rs".to_string(),
                "./Cargo.toml".to_string(),
                "./README.md".to_string(),
                "./Makefile".to_string(),
            ],
            None,
        );
        let output = format!("{result}");
        assert!(output.starts_with("FIND: find | 5 entries (showing 5)"));
        assert!(output.contains("  ./src/main.rs"));
        assert!(output.contains("  ./Cargo.toml"));
    }

    #[test]
    fn test_file_result_display_with_footer() {
        let result = FileResult::new(
            "find".to_string(),
            200,
            100,
            (0..100).map(|i| format!("./path/file{i}.rs")).collect(),
            Some("... and 100 more".to_string()),
        );
        let output = format!("{result}");
        assert!(output.contains("FIND: find | 200 entries (showing 100)"));
        assert!(output.contains("... and 100 more"));
    }

    #[test]
    fn test_file_result_serde_roundtrip() {
        let original = FileResult::new(
            "ls".to_string(),
            3,
            3,
            vec![
                "a.txt".to_string(),
                "b.txt".to_string(),
                "c.txt".to_string(),
            ],
            None,
        );
        let json = serde_json::to_string(&original).unwrap();
        let mut deserialized: FileResult = serde_json::from_str(&json).unwrap();
        deserialized.ensure_rendered();
        assert_eq!(deserialized.tool, "ls");
        assert_eq!(deserialized.total_count, 3);
        assert!(!deserialized.as_ref().is_empty());
    }

    #[test]
    fn test_file_result_ensure_rendered() {
        let mut result = FileResult {
            tool: "rg".to_string(),
            total_count: 2,
            shown_count: 2,
            entries: vec!["src/a.rs".to_string(), "src/b.rs".to_string()],
            footer: None,
            rendered: String::new(),
        };
        result.ensure_rendered();
        assert!(!result.rendered.is_empty());
        assert!(result.rendered.contains("RG: rg | 2 entries"));
    }

    #[test]
    fn test_file_result_empty_entries() {
        let result = FileResult::new("find".to_string(), 0, 0, vec![], None);
        let output = format!("{result}");
        assert!(output.contains("FIND: find | 0 entries (showing 0)"));
    }

    // ========================================================================
    // LogResult tests
    // ========================================================================

    #[test]
    fn test_log_result_display_default() {
        let entries = vec![
            LogEntry {
                level: Some("ERROR".to_string()),
                message: "connection refused".to_string(),
                count: 47,
            },
            LogEntry {
                level: Some("INFO".to_string()),
                message: "request completed".to_string(),
                count: 312,
            },
        ];
        let result = LogResult::new(4281, 87, 0, 3194, entries, false);
        let output = format!("{result}");
        assert!(output.contains("LOG: 4281 lines"));
        assert!(output.contains("87 unique"));
        assert!(output.contains("3194 duplicates removed"));
        assert!(output.contains("[ERROR] connection refused (\u{d7}47)"));
        assert!(output.contains("[INFO] request completed (\u{d7}312)"));
    }

    #[test]
    fn test_log_result_display_debug_hidden() {
        let entries = vec![LogEntry {
            level: Some("ERROR".to_string()),
            message: "connection refused".to_string(),
            count: 47,
        }];
        let result = LogResult::new(4281, 87, 847, 3194, entries, false);
        let output = format!("{result}");
        assert!(output.contains("[notice] 847 DEBUG lines hidden"));
        assert!(output.contains("skim log --debug-only"));
    }

    #[test]
    fn test_log_result_display_debug_only() {
        let entries = vec![LogEntry {
            level: Some("DEBUG".to_string()),
            message: "cache miss for key=user:123".to_string(),
            count: 203,
        }];
        let result = LogResult::new(847, 1, 847, 846, entries, true);
        let output = format!("{result}");
        assert!(output.starts_with("LOG DEBUG: 847 debug lines"));
        assert!(!output.contains("[notice]"));
        assert!(output.contains("[DEBUG] cache miss for key=user:123 (\u{d7}203)"));
    }

    #[test]
    fn test_log_result_serde_roundtrip() {
        let entries = vec![LogEntry {
            level: Some("WARN".to_string()),
            message: "retrying".to_string(),
            count: 5,
        }];
        let original = LogResult::new(100, 10, 0, 90, entries, false);
        let json = serde_json::to_string(&original).unwrap();
        let mut deserialized: LogResult = serde_json::from_str(&json).unwrap();
        deserialized.ensure_rendered();
        assert_eq!(deserialized.total_lines, 100);
        assert!(!deserialized.as_ref().is_empty());
    }

    #[test]
    fn test_log_result_ensure_rendered() {
        let mut result = LogResult {
            total_lines: 50,
            unique_messages: 5,
            debug_hidden: 0,
            deduplicated_count: 45,
            entries: vec![],
            debug_only: false,
            stack_frames_elided: 0,
            rendered: String::new(),
        };
        result.ensure_rendered();
        assert!(!result.rendered.is_empty());
        assert!(result.rendered.contains("LOG: 50 lines"));
    }

    #[test]
    fn test_log_result_no_level_entry() {
        let entries = vec![LogEntry {
            level: None,
            message: "plain message".to_string(),
            count: 1,
        }];
        let result = LogResult::new(1, 1, 0, 0, entries, false);
        let output = format!("{result}");
        assert!(output.contains("  plain message"));
        assert!(!output.contains('['));
    }

    // ========================================================================
    // ShowCommitResult tests (#132)
    // ========================================================================

    #[test]
    fn test_show_commit_result_display_basic() {
        let files = vec![DiffFileEntry {
            path: "src/main.rs".to_string(),
            status: DiffFileStatus::Modified,
            changed_regions: 2,
        }];
        let result = ShowCommitResult::new(
            "abc1234567".to_string(),
            "Alice <alice@example.com>".to_string(),
            "2024-01-15 10:00:00 +0000".to_string(),
            "feat: add feature".to_string(),
            String::new(),
            None,
            files,
            "diff content here",
        );
        let output = format!("{result}");
        // Hash is truncated to 7 chars
        assert!(output.contains("abc1234"), "hash must appear: {output}");
        assert!(
            output.contains("Alice <alice@example.com>"),
            "author must appear: {output}"
        );
        assert!(
            output.contains("feat: add feature"),
            "subject must appear: {output}"
        );
        assert!(
            output.contains("diff content here"),
            "diff must appear: {output}"
        );
    }

    #[test]
    fn test_show_commit_result_short_hash() {
        // Hash shorter than 7 chars must not panic; used as-is.
        let result = ShowCommitResult::new(
            "abc".to_string(),
            "Bob".to_string(),
            "2024-01-15".to_string(),
            "short hash commit".to_string(),
            String::new(),
            None,
            vec![],
            "",
        );
        let output = format!("{result}");
        assert!(output.contains("abc"), "short hash must appear: {output}");
    }

    #[test]
    fn test_show_commit_result_files_changed_field() {
        let files = vec![
            DiffFileEntry {
                path: "a.rs".to_string(),
                status: DiffFileStatus::Added,
                changed_regions: 1,
            },
            DiffFileEntry {
                path: "b.rs".to_string(),
                status: DiffFileStatus::Deleted,
                changed_regions: 3,
            },
        ];
        let result = ShowCommitResult::new(
            "deadbeef".to_string(),
            "Carol".to_string(),
            "2024-01-16".to_string(),
            "fix: remove b".to_string(),
            String::new(),
            None,
            files,
            "",
        );
        assert_eq!(
            result.files_changed, 2,
            "files_changed must equal files.len()"
        );
    }

    #[test]
    fn test_show_commit_result_serialize_deserialize() {
        let files = vec![DiffFileEntry {
            path: "src/lib.rs".to_string(),
            status: DiffFileStatus::Modified,
            changed_regions: 5,
        }];
        let original = ShowCommitResult::new(
            "cafebabe".to_string(),
            "Dave <dave@example.com>".to_string(),
            "2024-02-01 12:00:00 +0000".to_string(),
            "refactor: clean up".to_string(),
            String::new(),
            None,
            files,
            "the diff body",
        );
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: ShowCommitResult = serde_json::from_str(&json).unwrap();

        // Scalar fields survive round-trip.
        assert_eq!(deserialized.hash, original.hash);
        assert_eq!(deserialized.author, original.author);
        assert_eq!(deserialized.date, original.date);
        assert_eq!(deserialized.subject, original.subject);
        assert_eq!(deserialized.files_changed, original.files_changed);
        assert_eq!(deserialized.files.len(), original.files.len());
        // rendered is preserved when serialized with the full field set.
        assert_eq!(deserialized.as_ref(), original.as_ref());
    }

    #[test]
    fn test_show_commit_result_ensure_rendered_recomputes_when_empty() {
        // Simulate deserialization that strips `rendered` (e.g., consumer
        // writes their own JSON without that private field).
        let mut result = ShowCommitResult {
            hash: "1234567890ab".to_string(),
            author: "Eve".to_string(),
            date: "2024-03-01".to_string(),
            subject: "chore: cleanup".to_string(),
            body: String::new(),
            parents: None,
            files_changed: 1,
            files: vec![DiffFileEntry {
                path: "src/foo.rs".to_string(),
                status: DiffFileStatus::Modified,
                changed_regions: 4,
            }],
            rendered: String::new(),
        };
        assert_eq!(result.as_ref(), "", "rendered must start empty");

        result.ensure_rendered();

        let output = result.as_ref();
        assert!(!output.is_empty(), "ensure_rendered must populate rendered");
        assert!(
            output.contains("1234567"),
            "short hash must appear: {output}"
        );
        assert!(output.contains("Eve"), "author must appear: {output}");
        assert!(
            output.contains("chore: cleanup"),
            "subject must appear: {output}"
        );
        assert!(
            output.contains("src/foo.rs"),
            "file path must appear: {output}"
        );
    }

    #[test]
    fn test_show_commit_result_ensure_rendered_empty_diff() {
        // Empty diff and files list — no panic, minimal output.
        let mut result = ShowCommitResult::new(
            "aabbccd".to_string(),
            "Frank".to_string(),
            "2024-04-01".to_string(),
            "docs: update readme".to_string(),
            String::new(),
            None,
            vec![],
            "",
        );
        // rendered is set by new() even with empty diff.
        assert!(!result.as_ref().is_empty(), "non-empty diff still renders");
        // Calling ensure_rendered when already populated is a no-op.
        let before = result.as_ref().to_string();
        result.ensure_rendered();
        assert_eq!(
            result.as_ref(),
            before,
            "ensure_rendered must not overwrite existing rendered"
        );
    }

    #[test]
    fn test_show_commit_result_date_is_json_only() {
        // date must appear in JSON but not in the text render.
        let result = ShowCommitResult::new(
            "fedcba9".to_string(),
            "Grace".to_string(),
            "2024-05-15 09:30:00 +0000".to_string(),
            "test: add coverage".to_string(),
            String::new(),
            None,
            vec![],
            "",
        );
        let text = result.to_string();
        assert!(
            !text.contains("2024-05-15"),
            "date must NOT appear in text render (JSON-only): {text}"
        );
        let json = serde_json::to_string(&result).unwrap();
        assert!(
            json.contains("2024-05-15"),
            "date MUST appear in JSON output: {json}"
        );
    }

    // AD-GIT-8 tests: body and parents preservation
    #[test]
    fn test_show_commit_result_body_in_render() {
        let result = ShowCommitResult::new(
            "abc1234567".to_string(),
            "Alice".to_string(),
            "2026-04-11".to_string(),
            "feat: multi-paragraph".to_string(),
            "Paragraph 1 of body.\n\nParagraph 2 of body.".to_string(),
            None,
            vec![],
            "",
        );
        let text = result.to_string();
        assert!(
            text.contains("Paragraph 1 of body."),
            "body paragraph 1 must appear: {text}"
        );
        assert!(
            text.contains("Paragraph 2 of body."),
            "body paragraph 2 must appear: {text}"
        );
    }

    #[test]
    fn test_show_commit_result_empty_body_no_trailing_newlines() {
        let result = ShowCommitResult::new(
            "abc1234".to_string(),
            "Bob".to_string(),
            "2026-04-11".to_string(),
            "fix: subject only".to_string(),
            String::new(),
            None,
            vec![],
            "",
        );
        let text = result.to_string();
        // Subject-only commits must not have trailing blank lines (compact output).
        assert!(!text.ends_with("\n\n"), "no trailing blank lines: {text:?}");
    }

    #[test]
    fn test_show_commit_result_parents_in_render() {
        let result = ShowCommitResult::new(
            "fedcba9".to_string(),
            "Merger".to_string(),
            "2026-04-11".to_string(),
            "Merge pull request #42".to_string(),
            String::new(),
            Some("abc123 def456 fed321".to_string()),
            vec![],
            "",
        );
        let text = result.to_string();
        assert!(
            text.contains("Merge: abc123 def456 fed321"),
            "parents must appear as Merge: line: {text}"
        );
    }

    #[test]
    fn test_show_commit_result_parents_before_summary() {
        let result = ShowCommitResult::new(
            "cafebabe".to_string(),
            "Merger".to_string(),
            "2026-04-11".to_string(),
            "Merge feature branch".to_string(),
            String::new(),
            Some("aaa111 bbb222".to_string()),
            vec![],
            "",
        );
        let text = result.to_string();
        let merge_pos = text.find("Merge:").unwrap();
        let summary_pos = text.find('\u{2014}').unwrap();
        assert!(
            merge_pos < summary_pos,
            "Merge: line must appear before summary em-dash: {text}"
        );
    }
}

// ============================================================================
// ShowCommitResult types (#132)
// ============================================================================

/// Result of `skim git show <hash>` (commit mode).
///
/// Follows the same pattern as `DiffResult`: a pre-rendered `String` is stored
/// so JSON and text consumers share the same rendering logic. Files are
/// represented as [`DiffFileEntry`] — the same type used by `DiffResult` — to
/// keep the JSON shape consistent across all diff-bearing results.
///
/// # Field visibility
///
/// The `date` field is serialized to JSON but intentionally omitted from the
/// text render: the single-line summary (`<hash> <author> — <subject>`) is
/// already compact; callers that need the full date should use `--json`.
///
/// # AD-GIT-8 (2026-04-11) — body and parents
///
/// `body` stores the full multi-paragraph commit message below the subject
/// line. It is appended to the text render only when non-empty, keeping
/// subject-only commits compact. `parents` captures the tail of `Merge: `
/// header lines; when present it is rendered as `Merge: {parents}` on a
/// dedicated line before the summary, matching `git show` output order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ShowCommitResult {
    /// Short commit hash (first 7 characters).
    pub(crate) hash: String,
    /// Author string (name + email).
    pub(crate) author: String,
    /// Commit date (JSON-only; omitted from text render for terseness).
    pub(crate) date: String,
    /// Commit subject (first line of commit message).
    pub(crate) subject: String,
    /// Full commit message body below the subject line (empty for subject-only commits).
    ///
    /// # AD-GIT-8 (2026-04-11)
    /// Preserved verbatim with 4-space indent stripped. Appended to text render
    /// as `\n\n{body}` only when non-empty.
    #[serde(default)]
    pub(crate) body: String,
    /// Merge parent hashes, when present (e.g. `"abc123 def456"`).
    ///
    /// # AD-GIT-8 (2026-04-11)
    /// Rendered as `Merge: {parents}\n` before the summary line in text output.
    /// Octopus merges store all parent hashes space-separated in one string.
    #[serde(default)]
    pub(crate) parents: Option<String>,
    /// Number of files changed (mirrors `files.len()` for quick JSON access).
    #[serde(default)]
    pub(crate) files_changed: usize,
    /// Files changed in this commit.
    pub(crate) files: Vec<DiffFileEntry>,
    #[serde(default)]
    rendered: String,
}

impl ShowCommitResult {
    /// Create a new `ShowCommitResult` with pre-computed rendered output.
    ///
    /// # AD-GIT-8 (2026-04-11)
    /// `body` and `parents` are new required parameters. `parents` renders as
    /// `Merge: {parents}\n` before the summary line. `body` renders as
    /// `\n\n{body}` after the summary only when non-empty.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        hash: String,
        author: String,
        date: String,
        subject: String,
        body: String,
        parents: Option<String>,
        files: Vec<DiffFileEntry>,
        diff_output: &str,
    ) -> Self {
        let files_changed = files.len();
        let rendered = Self::render(
            &hash,
            &author,
            &subject,
            &body,
            parents.as_deref(),
            diff_output,
        );
        Self {
            hash,
            author,
            date,
            subject,
            body,
            parents,
            files_changed,
            files,
            rendered,
        }
    }

    /// Render the commit result to a human-readable string.
    ///
    /// # AD-GIT-8 (2026-04-11)
    /// - `parents` (when `Some`) is prepended as `Merge: {parents}\n` before the summary.
    /// - `body` (when non-empty) is appended as `\n\n{body}` after the summary.
    /// - Empty body produces no trailing newlines, keeping subject-only commits compact.
    fn render(
        hash: &str,
        author: &str,
        subject: &str,
        body: &str,
        parents: Option<&str>,
        diff_output: &str,
    ) -> String {
        use std::fmt::Write;
        let short = hash.get(..7).unwrap_or(hash);
        let mut output = String::new();
        if let Some(p) = parents {
            let _ = writeln!(output, "Merge: {p}");
        }
        let _ = write!(output, "{short} {author} \u{2014} {subject}");
        if !body.is_empty() {
            let _ = write!(output, "\n\n{body}");
        }
        if !diff_output.is_empty() {
            let _ = write!(output, "\n\n{diff_output}");
        }
        output
    }

    /// Consume `self` and return the pre-rendered text, avoiding a clone.
    ///
    /// Prefer this over `to_string()` at call sites that own the result and do
    /// not need the other fields afterwards.  The `Display` impl re-runs a
    /// `write!` through the formatter, which allocates; this method is zero-copy.
    pub(crate) fn into_rendered(self) -> String {
        self.rendered
    }

    /// Recompute `rendered` if empty (e.g. after JSON deserialization that
    /// stripped the field).  Produces a lossy summary — file paths, statuses,
    /// and region counts — because the original diff body is not stored.
    ///
    /// # AD-GIT-8 (2026-04-11)
    /// Respects `parents` (prepends `Merge: {parents}\n`) and `body` (appends
    /// when non-empty) for consistency with `render()`.
    pub(crate) fn ensure_rendered(&mut self) {
        if self.rendered.is_empty() {
            use std::fmt::Write;
            let short = self.hash.get(..7).unwrap_or(&self.hash);
            let mut output = String::new();
            if let Some(p) = &self.parents {
                let _ = writeln!(output, "Merge: {p}");
            }
            let _ = write!(
                output,
                "{short} {} \u{2014} {} [{} files]",
                self.author, self.subject, self.files_changed
            );
            for file in &self.files {
                let _ = write!(
                    output,
                    "\n  {} ({}, {} regions)",
                    file.path, file.status, file.changed_regions
                );
            }
            if !self.body.is_empty() {
                let _ = write!(output, "\n\n{}", self.body);
            }
            self.rendered = output;
        }
    }
}

impl AsRef<str> for ShowCommitResult {
    fn as_ref(&self) -> &str {
        &self.rendered
    }
}

impl fmt::Display for ShowCommitResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.rendered)
    }
}
