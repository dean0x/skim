//! Data structures for the `skim heatmap` subcommand.
//!
//! No logic, no I/O. Pure data model.

use serde::Serialize;

// ============================================================================
// CLI configuration
// ============================================================================

/// Parsed CLI flags for `skim heatmap`.
#[derive(Debug, Clone)]
pub(crate) struct HeatmapConfig {
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
    pub(crate) window_preset: Option<String>,
    /// Limit analysis to last N commits.
    pub(crate) last_n: Option<usize>,
}

impl Default for HeatmapConfig {
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
        }
    }
}

// ============================================================================
// Git log data
// ============================================================================

/// A single commit extracted from git log.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CommitRecord {
    pub(crate) hash: String,
    pub(crate) author: String,
    /// Unix timestamp.
    pub(crate) timestamp: u64,
    pub(crate) subject: String,
    pub(crate) files: Vec<FileChange>,
}

/// A file touched in a commit, with line change counts.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FileChange {
    pub(crate) path: String,
    pub(crate) additions: u64,
    pub(crate) deletions: u64,
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
}

/// Information about the analysis window.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct WindowInfo {
    pub(crate) mode: String,
    pub(crate) since: String,
    pub(crate) until: String,
    pub(crate) commits_analyzed: usize,
    pub(crate) time_commits: Option<usize>,
    pub(crate) count_commits: Option<usize>,
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
#[derive(Debug, Clone, Serialize)]
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
