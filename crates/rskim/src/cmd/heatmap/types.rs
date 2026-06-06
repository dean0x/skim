//! Data structures for the `skim heatmap` subcommand.
//!
//! # Lifecycle
//!
//! ```text
//! CLI args  ──parse_args()──►  HeatmapArgs
//!                                  │
//!                      resolve_diff_files() mutates diff_base/files
//!                                  │
//!                         resolve_config()  (consumes HeatmapArgs)
//!                                  │
//!                     HeatmapConfig (resolved, immutable)
//! ```
//!
//! `HeatmapArgs` is the raw parse output and is mutable until resolution.
//! `HeatmapConfig` is the resolved, immutable form passed to all downstream
//! functions. Three fields from `HeatmapArgs` are consumed during resolution
//! and do NOT appear in `HeatmapConfig`:
//!
//! - `window_preset` — moved into [`ResolvedWindow`]
//! - `last_n` — copied into [`ResolvedWindow`]
//! - `diff_base` — consumed by `resolve_diff_files` via `.take()`
//!
//! No logic, no I/O. Pure data model.

use serde::Serialize;

// Re-export shared commit/file types from rskim-search so heatmap modules
// use a single canonical definition (eliminates the local duplicate).
pub(crate) use rskim_search::{CommitInfo, FileChangeInfo};

// ============================================================================
// CLI configuration
// ============================================================================

/// Raw CLI parse output for `skim heatmap`.
///
/// Populated by [`super::args::parse_args`] from the raw `&[String]` argv slice.
/// Fields map 1:1 to CLI flags — no resolution, no defaulting beyond what the
/// flag itself implies (e.g. `top_n` defaults to 20, `coupling_threshold` to 0.5).
///
/// After `parse_args` returns:
/// 1. `resolve_diff_files` borrows `&mut HeatmapArgs` to fill `files` from a
///    three-dot diff when `diff_base` is set.
/// 2. [`super::window::resolve_config`] consumes this struct by value, applies
///    presets and dual-window logic, and produces the immutable [`HeatmapConfig`].
#[derive(Debug)]
pub(crate) struct HeatmapArgs {
    /// Epoch seconds — only analyze commits since this timestamp.
    pub(crate) since: Option<u64>,
    /// Scope analysis to files under this path.
    pub(crate) path: Option<String>,
    /// Emit JSON output instead of text.
    pub(crate) format_json: bool,
    /// Maximum number of files to display (default 20).
    pub(crate) top_n: usize,
    /// Skip default exclude patterns.
    pub(crate) no_exclude: bool,
    /// Additional glob patterns to exclude.
    pub(crate) extra_excludes: Vec<String>,
    /// Coupling confidence threshold (default 0.5).
    pub(crate) coupling_threshold: f64,
    /// Fix-after-touch proximity window in commits (default 5).
    pub(crate) fix_window: usize,
    /// Enable debug output.
    pub(crate) debug: bool,
    /// Named window preset (e.g., "sprint", "quarter").
    ///
    /// Consumed by `resolve_config` — moved into [`ResolvedWindow`].
    pub(crate) window_preset: Option<String>,
    /// Limit analysis to last N commits.
    ///
    /// Consumed by `resolve_config` — copied into [`ResolvedWindow`].
    pub(crate) last_n: Option<usize>,
    /// Explicit file targets — scope display to these paths.
    pub(crate) files: Vec<String>,
    /// Base branch/ref for `--diff` three-dot diff.
    ///
    /// Consumed by `resolve_diff_files` via `.take()`.
    pub(crate) diff_base: Option<String>,
    /// True when `--top` was explicitly passed (vs. implied by file targeting).
    pub(crate) top_explicit: bool,
    /// Show only threshold-filtered one-liner insights.
    pub(crate) insights: bool,
}

impl Default for HeatmapArgs {
    fn default() -> Self {
        Self {
            since: None,
            path: None,
            format_json: false,
            top_n: 20,
            no_exclude: false,
            extra_excludes: Vec::new(),
            coupling_threshold: 0.5,
            fix_window: 5,
            debug: false,
            window_preset: None,
            last_n: None,
            files: Vec::new(),
            diff_base: None,
            top_explicit: false,
            insights: false,
        }
    }
}

/// Resolved, immutable configuration for the `skim heatmap` pipeline.
///
/// Produced by [`super::window::resolve_config`] from [`HeatmapArgs`]. The three
/// fields that are "consumed" during resolution (`window_preset`, `last_n`,
/// `diff_base`) are intentionally absent here — they live in [`ResolvedWindow`]
/// or are handled before resolution completes.
///
/// This type does not implement `Clone` or `Default` to enforce the invariant
/// that it can only be constructed through `resolve_config` (the type-state
/// transition), not by arbitrary callers.
#[derive(Debug)]
pub(crate) struct HeatmapConfig {
    /// Resolved epoch seconds — only analyze commits since this timestamp.
    /// Set by preset, `--last`, `--since`, or dual-default resolution.
    pub(crate) since: Option<u64>,
    /// Scope analysis to files under this path (passed through from args).
    pub(crate) path: Option<String>,
    /// Emit JSON output instead of text.
    pub(crate) format_json: bool,
    /// Maximum number of files to display (default 20).
    pub(crate) top_n: usize,
    /// Skip default exclude patterns.
    pub(crate) no_exclude: bool,
    /// Additional glob patterns to exclude.
    pub(crate) extra_excludes: Vec<String>,
    /// Coupling confidence threshold (default 0.5).
    pub(crate) coupling_threshold: f64,
    /// Fix-after-touch proximity window in commits (default 5).
    pub(crate) fix_window: usize,
    /// Enable debug output.
    pub(crate) debug: bool,
    /// Explicit file targets — scope display to these paths.
    pub(crate) files: Vec<String>,
    /// True when `--top` was explicitly passed (vs. implied by file targeting).
    pub(crate) top_explicit: bool,
    /// Show only threshold-filtered one-liner insights.
    pub(crate) insights: bool,
}

/// Resolved window metadata produced by [`super::window::resolve_config`].
///
/// Separates derived window state from raw CLI input (`HeatmapArgs`), so
/// `HeatmapArgs` remains a pure user-input struct and window metadata is
/// never confused with CLI flags.
///
/// The effective `since` epoch is NOT stored here — it lives exclusively in
/// [`HeatmapConfig::since`]. This eliminates the previously-redundant
/// `ResolvedWindow::since` field whose invariant ("always identical to
/// `HeatmapConfig::since`") could not be enforced by the type system. Callers
/// that need the epoch for display pass it explicitly via [`super::window::build_window_info`].
#[derive(Debug, Clone)]
pub(crate) struct ResolvedWindow {
    /// True when using dual default windowing (no explicit flag was set).
    pub(crate) dual_mode: bool,
    /// Epoch of the 90-day time window (populated in dual mode only).
    pub(crate) dual_time_since: Option<u64>,
    /// Epoch of the 200-commit window (populated in dual mode only).
    pub(crate) dual_count_since: Option<u64>,
    /// Named window preset string (e.g. "sprint"), if one was used.
    pub(crate) window_preset: Option<String>,
    /// Last-N commit count, if `--last` was used.
    pub(crate) last_n: Option<usize>,
}

// ============================================================================
// Heatmap output
// ============================================================================

/// Top-level output structure for `skim heatmap`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct HeatmapResult {
    /// Schema version (always 1).
    pub(crate) version: u8,
    pub(crate) generated_at: String,
    pub(crate) repository: String,
    pub(crate) window: WindowInfo,
    pub(crate) files: Vec<FileMetrics>,
    pub(crate) modules: Vec<ModuleHealth>,
    pub(crate) coupling_graph: Vec<CouplingEdge>,
    pub(crate) excluded_patterns: Vec<String>,
    pub(crate) warnings: Vec<String>,
    /// Files targeted by `--diff` or positional args (present only when scoped).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) file_targets: Option<Vec<String>>,
}

/// Information about the analysis window.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct WindowInfo {
    pub(crate) mode: String,
    pub(crate) since: String,
    pub(crate) until: String,
    pub(crate) commits_analyzed: usize,
    pub(crate) effective_strategy: Option<String>,
}

/// Risk and coupling metrics for a single file.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FileMetrics {
    pub(crate) path: String,
    pub(crate) churn: ChurnMetrics,
    pub(crate) stability_score: u8,
    pub(crate) authors: AuthorMetrics,
    pub(crate) fix_risk: FixRiskMetrics,
    pub(crate) blast_radius: Vec<CouplingEntry>,
}

/// Churn metrics for a file.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ChurnMetrics {
    /// Number of commits touching this file.
    pub(crate) commits: usize,
    /// Ratio of this file's commits to total commits (0.0–1.0).
    pub(crate) rate: f64,
}

/// Author diversity metrics for a file.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct AuthorMetrics {
    /// Unique author count (authors with >5% of commits).
    pub(crate) count: usize,
    /// Percentage of commits by the top author (0.0–100.0).
    pub(crate) top_author_pct: f64,
    /// True when a single author holds >80% of commits.
    pub(crate) single_owner_risk: bool,
}

/// Fix-after-touch risk metrics for a file.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FixRiskMetrics {
    /// Percentage of commits with fix keywords.
    pub(crate) keyword_pct: f64,
    /// Percentage of commits followed by a fix within the window.
    pub(crate) proximity_pct: f64,
    /// Union of keyword and proximity signals.
    pub(crate) combined_pct: f64,
    /// True when <2 commits — not enough data.
    pub(crate) insufficient_data: bool,
}

impl Default for FixRiskMetrics {
    fn default() -> Self {
        Self {
            keyword_pct: 0.0,
            proximity_pct: 0.0,
            combined_pct: 0.0,
            // Default to `true` so callers that skip the lookup (no history) are
            // marked as having insufficient data rather than misleadingly showing
            // zero-risk metrics.
            insufficient_data: true,
        }
    }
}

/// A coupling entry in a file's blast radius.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CouplingEntry {
    pub(crate) path: String,
    /// Confidence score (0.0–1.0).
    pub(crate) confidence: f64,
    /// Number of commits where both files changed together.
    pub(crate) support: usize,
}

/// A directed coupling edge in the global graph.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CouplingEdge {
    pub(crate) a: String,
    pub(crate) b: String,
    pub(crate) confidence: f64,
    pub(crate) support: usize,
}

/// Encapsulation health for a module directory.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ModuleHealth {
    pub(crate) path: String,
    /// Percentage of commits touching only this module (0.0–100.0).
    pub(crate) encapsulation_pct: f64,
    pub(crate) files_count: usize,
    pub(crate) total_commits: usize,
    pub(crate) cross_boundary_commits: usize,
}

// ============================================================================
// Insights types
// ============================================================================

/// Insight severity level for threshold-filtered findings.
///
/// Ord is derived so Critical < Warning (sorts critical first when ascending).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) enum Severity {
    /// Highest severity — file meets critical threshold.
    Critical,
    /// Moderate severity — file meets warning threshold.
    Warning,
}

/// Category of a threshold-filtered finding.
///
/// Using an enum (instead of `String`) gives compile-time exhaustiveness in
/// `sort_key` and eliminates `.to_string()` allocations at each insight push.
/// Serialized as kebab-case so JSON output matches the documented schema
/// (`"fix-risk"`, `"bus-factor"`, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum InsightCategory {
    Stability,
    FixRisk,
    BusFactor,
    Coupling,
    Encapsulation,
}

/// A single threshold-filtered finding.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Insight {
    pub(crate) severity: Severity,
    pub(crate) category: InsightCategory,
    pub(crate) file: String,
    pub(crate) message: String,
    pub(crate) metric_value: f64,
}

/// Standalone insights result (separate from HeatmapResult — does NOT add fields to it).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct InsightsResult {
    pub(crate) version: u8,
    pub(crate) repository: String,
    pub(crate) window: WindowInfo,
    pub(crate) insights: Vec<Insight>,
    pub(crate) top_files: Vec<CompactFileEntry>,
    pub(crate) flagged_modules: Vec<CompactModuleEntry>,
}

/// Condensed file entry for [`InsightsResult`].
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CompactFileEntry {
    pub(crate) path: String,
    pub(crate) stability: u8,
    pub(crate) churn_commits: usize,
    pub(crate) fix_risk_pct: f64,
    pub(crate) bus_factor_risk: bool,
}

/// Flagged module entry for [`InsightsResult`].
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CompactModuleEntry {
    pub(crate) path: String,
    pub(crate) encapsulation_pct: f64,
    pub(crate) cross_boundary: usize,
}
