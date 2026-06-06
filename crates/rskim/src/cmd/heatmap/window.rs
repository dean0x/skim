//! Window resolution helpers for `skim heatmap`.
//!
//! Handles all time-window logic: preset mapping, dual-mode resolution,
//! `--since` / `--last` / `--window` precedence, and display formatting.

use super::git_source::GitDataSource;
use super::types::{HeatmapArgs, HeatmapConfig, ResolvedWindow, WindowInfo};

// ============================================================================
// Preset mapping
// ============================================================================

/// Map a named preset to `--since` epoch seconds offset.
fn preset_to_since_secs(preset: &str, now_epoch: u64) -> Option<u64> {
    match preset {
        "sprint" => Some(now_epoch.saturating_sub(14 * 86400)),
        "month" => Some(now_epoch.saturating_sub(30 * 86400)),
        "quarter" => Some(now_epoch.saturating_sub(90 * 86400)),
        "half" => Some(now_epoch.saturating_sub(180 * 86400)),
        "year" => Some(now_epoch.saturating_sub(365 * 86400)),
        "all" => Some(0),
        _ => None,
    }
}

// ============================================================================
// Config resolution
// ============================================================================

/// Consume [`HeatmapArgs`] and produce a resolved, immutable [`HeatmapConfig`].
///
/// This is the type-state transition point: `HeatmapArgs` is mutable raw CLI
/// parse output; `HeatmapConfig` is the resolved, immutable form that all
/// downstream functions operate on.
///
/// Precedence: `--since` > `--last` > `--window` preset > dual default.
///
/// Three fields from `HeatmapArgs` are consumed here and do NOT appear in
/// `HeatmapConfig`:
/// - `window_preset` — moved into the returned [`ResolvedWindow`]
/// - `last_n` — copied into the returned [`ResolvedWindow`]
/// - `diff_base` — already consumed by `resolve_diff_files` before this call
///
/// Returns a tuple of:
/// - `HeatmapConfig` with `since` set to the resolved epoch (used by `fetch_commits`)
/// - `ResolvedWindow` carrying window metadata (mode, dual fields) for `build_window_info`
pub(super) fn resolve_config(
    args: HeatmapArgs,
    source: &dyn GitDataSource,
    warnings: &mut Vec<String>,
    now_epoch: u64,
) -> anyhow::Result<(HeatmapConfig, ResolvedWindow)> {
    // Move window_preset and last_n into ResolvedWindow first so we can read
    // them from `window.*` rather than from `args` after partial move.
    let mut window = ResolvedWindow {
        since: None,
        dual_mode: false,
        dual_time_since: None,
        dual_count_since: None,
        window_preset: args.window_preset,
        last_n: args.last_n,
    };

    // Count explicit time-selection flags
    let explicit_count = usize::from(args.since.is_some())
        + usize::from(window.last_n.is_some())
        + usize::from(window.window_preset.is_some());

    if explicit_count > 1 {
        warnings.push(
            "Multiple window flags specified — using first (--since > --last > --window)."
                .to_string(),
        );
    }

    if let Some(since) = args.since {
        // Already set — highest precedence
        window.since = Some(since);
    } else if let Some(n) = window.last_n {
        // --last N: find the timestamp of the Nth commit
        match source.fetch_commit_count_since(n) {
            Ok(Some(ts)) => {
                window.since = Some(ts);
            }
            Ok(None) => {
                warnings.push(format!(
                    "Repository has fewer than {n} commits — analyzing all history."
                ));
            }
            Err(e) => {
                warnings.push(format!("Could not resolve --last {n}: {e}"));
            }
        }
    } else if let Some(ref preset) = window.window_preset {
        if let Some(since) = preset_to_since_secs(preset, now_epoch) {
            window.since = Some(since);
        } else {
            warnings.push(format!(
                "Unknown window preset '{preset}' — valid: sprint, month, quarter, half, year, all. Analyzing all history."
            ));
        }
    } else {
        // Dual default: max(last 90 days, last 200 commits)
        let time_since = now_epoch.saturating_sub(90 * 86400);

        let count_since = match source.fetch_commit_count_since(200) {
            Ok(Some(ts)) => ts,
            _ => time_since, // fallback to time-based if lookup fails
        };

        // Use whichever captures more history (lower epoch = more history)
        let dual_resolved = time_since.min(count_since);
        window.since = Some(dual_resolved);
        window.dual_mode = true;
        window.dual_time_since = Some(time_since);
        window.dual_count_since = Some(count_since);
    }

    // Build the resolved, immutable HeatmapConfig. Fields are moved from args
    // (heap allocations) or copied (scalars). `window_preset`, `last_n`, and
    // `diff_base` are intentionally absent — they were consumed above.
    let config = HeatmapConfig {
        since: window.since,
        path: args.path,
        format_json: args.format_json,
        top_n: args.top_n,
        no_exclude: args.no_exclude,
        extra_excludes: args.extra_excludes,
        coupling_threshold: args.coupling_threshold,
        fix_window: args.fix_window,
        debug: args.debug,
        files: args.files,
        top_explicit: args.top_explicit,
        insights: args.insights,
    };

    Ok((config, window))
}

// ============================================================================
// Window info construction
// ============================================================================

/// Convert a [`ResolvedWindow`] + runtime context into a [`WindowInfo`] for output.
pub(super) fn build_window_info(
    window: &ResolvedWindow,
    commits_analyzed: usize,
    now_epoch: u64,
) -> WindowInfo {
    let mode = if window.dual_mode {
        "dual".to_string()
    } else if let Some(ref preset) = window.window_preset {
        preset.clone()
    } else if window.last_n.is_some() {
        "count".to_string()
    } else if window.since.is_some() {
        "time".to_string()
    } else {
        "dual".to_string()
    };

    let since_str = window
        .since
        .map(format_epoch)
        .unwrap_or_else(|| "all".to_string());

    let effective_strategy = if window.dual_mode {
        let time_since = window.dual_time_since.unwrap_or(0);
        let count_since = window.dual_count_since.unwrap_or(0);
        let strategy = if time_since <= count_since {
            "time"
        } else {
            "count"
        };
        Some(strategy.to_string())
    } else {
        None
    };

    WindowInfo {
        mode,
        since: since_str,
        until: format_epoch(now_epoch),
        commits_analyzed,
        effective_strategy,
    }
}

// ============================================================================
// Formatting helpers
// ============================================================================

/// Format a Unix epoch as a simple date string (YYYY-MM-DD).
pub(super) fn format_epoch(epoch: u64) -> String {
    // Manual calculation — no chrono dependency
    // Days since 1970-01-01
    let days = epoch / 86400;
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Convert days since 1970-01-01 to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // year of era
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // month prime
    let d = doy - (153 * mp + 2) / 5 + 1; // day [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // month [1, 12]
    let y_adj = if m <= 2 { y + 1 } else { y };
    (y_adj, m, d)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn base_window() -> ResolvedWindow {
        ResolvedWindow {
            since: None,
            dual_mode: false,
            dual_time_since: None,
            dual_count_since: None,
            window_preset: None,
            last_n: None,
        }
    }

    const NOW: u64 = 1_704_067_200; // 2024-01-01

    #[test]
    fn test_format_epoch_known_date() {
        // 2024-01-01 = 1704067200
        assert_eq!(format_epoch(1_704_067_200), "2024-01-01");
    }

    #[test]
    fn test_format_epoch_unix_epoch() {
        assert_eq!(format_epoch(0), "1970-01-01");
    }

    #[test]
    fn test_preset_to_since_secs_sprint() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let since = preset_to_since_secs("sprint", now).unwrap();
        let diff = now - since;
        assert!((13 * 86400..=15 * 86400).contains(&diff));
    }

    #[test]
    fn test_preset_to_since_secs_unknown() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(preset_to_since_secs("unknown-preset", now).is_none());
    }

    #[test]
    fn test_days_to_ymd_epoch() {
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
    }

    #[test]
    fn test_days_to_ymd_known_date() {
        // 2024-01-01 = 19723 days since epoch
        assert_eq!(days_to_ymd(19723), (2024, 1, 1));
    }

    #[test]
    fn test_build_window_info_dual_mode() {
        let window = ResolvedWindow {
            dual_mode: true,
            // time_since <= count_since → effective_strategy = "time"
            dual_time_since: Some(1_000_000),
            dual_count_since: Some(2_000_000),
            ..base_window()
        };

        let info = build_window_info(&window, 42, NOW);

        assert_eq!(info.mode, "dual");
        assert_eq!(info.commits_analyzed, 42);
        assert_eq!(info.effective_strategy.as_deref(), Some("time"));
    }

    #[test]
    fn test_build_window_info_dual_mode_count_wins() {
        let window = ResolvedWindow {
            dual_mode: true,
            // time_since > count_since → effective_strategy = "count"
            dual_time_since: Some(2_000_000),
            dual_count_since: Some(1_000_000),
            ..base_window()
        };

        let info = build_window_info(&window, 10, NOW);

        assert_eq!(info.mode, "dual");
        assert_eq!(info.effective_strategy.as_deref(), Some("count"));
    }

    #[test]
    fn test_build_window_info_preset_mode() {
        let window = ResolvedWindow {
            window_preset: Some("quarter".to_string()),
            ..base_window()
        };

        let info = build_window_info(&window, 50, NOW);

        assert_eq!(info.mode, "quarter");
        assert!(info.effective_strategy.is_none());
    }

    #[test]
    fn test_build_window_info_count_mode() {
        let window = ResolvedWindow {
            last_n: Some(200),
            ..base_window()
        };

        let info = build_window_info(&window, 200, NOW);

        assert_eq!(info.mode, "count");
        assert!(info.effective_strategy.is_none());
    }

    #[test]
    fn test_build_window_info_time_mode() {
        let window = ResolvedWindow {
            since: Some(1_700_000_000),
            ..base_window()
        };

        let info = build_window_info(&window, 77, NOW);

        assert_eq!(info.mode, "time");
        assert_eq!(info.since, "2023-11-14"); // epoch 1_700_000_000
        assert!(info.effective_strategy.is_none());
    }

    #[test]
    fn test_build_window_info_default_falls_back_to_dual() {
        // No flags set → falls through to the "dual" fallback mode string.
        // dual_mode=false, so effective_strategy is None (only set in the dual_mode branch).
        let window = base_window();

        let info = build_window_info(&window, 0, NOW);

        assert_eq!(info.mode, "dual");
        assert!(info.effective_strategy.is_none());
    }

    #[test]
    fn test_build_window_info_no_since_shows_all() {
        let window = base_window();

        let info = build_window_info(&window, 0, NOW);

        assert_eq!(info.since, "all");
    }

    #[test]
    fn test_build_window_info_commits_analyzed_passthrough() {
        let window = base_window();

        let info = build_window_info(&window, 999, NOW);

        assert_eq!(info.commits_analyzed, 999);
    }

    // -----------------------------------------------------------------------
    // resolve_config — type-state transition tests
    // -----------------------------------------------------------------------

    /// Minimal mock GitDataSource for unit-testing `resolve_config` without
    /// spawning a real git process.
    struct MockGitSource {
        commit_count_since: Option<u64>,
    }

    impl super::super::git_source::GitDataSource for MockGitSource {
        fn is_git_repo(&self) -> bool {
            true
        }
        fn get_repo_root(&self) -> anyhow::Result<String> {
            Ok("/mock/repo".to_string())
        }
        fn detect_shallow_clone(&self) -> bool {
            false
        }
        fn fetch_commit_count_since(&self, _n: usize) -> anyhow::Result<Option<u64>> {
            Ok(self.commit_count_since)
        }
        fn fetch_commits(
            &self,
            _config: &super::super::types::HeatmapConfig,
        ) -> anyhow::Result<Vec<super::super::types::CommitInfo>> {
            Ok(vec![])
        }
    }

    /// Mock that always returns `Err` from `fetch_commit_count_since`, exercising
    /// the graceful-degradation path in the `--last N` branch of `resolve_config`.
    struct MockGitSourceErr;

    impl super::super::git_source::GitDataSource for MockGitSourceErr {
        fn is_git_repo(&self) -> bool {
            true
        }
        fn get_repo_root(&self) -> anyhow::Result<String> {
            Ok("/mock/repo".to_string())
        }
        fn detect_shallow_clone(&self) -> bool {
            false
        }
        fn fetch_commit_count_since(&self, _n: usize) -> anyhow::Result<Option<u64>> {
            Err(anyhow::anyhow!("simulated git error"))
        }
        fn fetch_commits(
            &self,
            _config: &super::super::types::HeatmapConfig,
        ) -> anyhow::Result<Vec<super::super::types::CommitInfo>> {
            Ok(vec![])
        }
    }

    fn base_args() -> super::super::types::HeatmapArgs {
        super::super::types::HeatmapArgs::default()
    }

    /// `window_preset` and `last_n` must appear in `ResolvedWindow`, not in
    /// `HeatmapConfig`. `HeatmapConfig` has neither field.
    ///
    /// Note: this test intentionally covers two independent fields in one pass
    /// (`window_preset` move and `last_n` copy) because both are consumed at the
    /// same point in `resolve_config`. A failure here implicates the
    /// `ResolvedWindow` construction block, not a single field. If one of them
    /// fails in isolation, add a targeted test for that field specifically.
    #[test]
    fn test_resolve_config_consumed_fields_go_to_window() {
        let source = MockGitSource {
            commit_count_since: Some(1_700_000_000),
        };
        let mut args = base_args();
        args.window_preset = Some("sprint".to_string());
        args.last_n = Some(50);
        let mut warnings = Vec::new();

        let (_config, window) = resolve_config(args, &source, &mut warnings, NOW).unwrap();

        // `window_preset` moved into ResolvedWindow (independent of last_n)
        assert_eq!(window.window_preset.as_deref(), Some("sprint"));
        // `last_n` copied into ResolvedWindow (independent of window_preset)
        assert_eq!(window.last_n, Some(50));
    }

    /// When `fetch_commit_count_since` returns `Err` during `--last N` resolution,
    /// `resolve_config` must degrade gracefully: `since` stays `None` (all history)
    /// and a warning is emitted. The function must not return `Err` itself.
    #[test]
    fn test_resolve_config_last_n_git_error_degrades_to_all_history() {
        let source = MockGitSourceErr;
        let mut args = base_args();
        args.last_n = Some(50);
        let mut warnings = Vec::new();

        let (config, window) =
            resolve_config(args, &source, &mut warnings, NOW).unwrap();

        // Graceful degradation: since is None (analyze all history)
        assert!(
            config.since.is_none(),
            "expected since=None (all history) on git error, got {:?}",
            config.since
        );
        assert!(
            window.since.is_none(),
            "expected window.since=None on git error, got {:?}",
            window.since
        );
        // Warning must be emitted so the user knows why the fallback occurred
        assert!(
            !warnings.is_empty(),
            "expected at least one warning on git error"
        );
        assert!(
            warnings.iter().any(|w| w.contains("Could not resolve --last")),
            "warning must mention 'Could not resolve --last', got: {:?}",
            warnings
        );
    }

    /// Passthrough scalar and heap fields carry over unchanged from args to config.
    #[test]
    fn test_resolve_config_passthrough_fields() {
        let source = MockGitSource {
            commit_count_since: None,
        };
        let mut args = base_args();
        args.top_n = 42;
        args.format_json = true;
        args.no_exclude = true;
        args.coupling_threshold = 0.7;
        args.fix_window = 10;
        args.debug = true;
        args.top_explicit = true;
        args.insights = true;
        args.path = Some("src/".to_string());
        args.extra_excludes = vec!["*.generated.ts".to_string()];
        args.files = vec!["src/main.rs".to_string()];
        let mut warnings = Vec::new();

        let (config, _window) = resolve_config(args, &source, &mut warnings, NOW).unwrap();

        assert_eq!(config.top_n, 42);
        assert!(config.format_json);
        assert!(config.no_exclude);
        assert!((config.coupling_threshold - 0.7).abs() < 1e-9);
        assert_eq!(config.fix_window, 10);
        assert!(config.debug);
        assert!(config.top_explicit);
        assert!(config.insights);
        assert_eq!(config.path.as_deref(), Some("src/"));
        assert_eq!(config.extra_excludes, vec!["*.generated.ts"]);
        assert_eq!(config.files, vec!["src/main.rs"]);
    }

    /// When `--since` is set, it wins regardless of preset/last_n.
    #[test]
    fn test_resolve_config_since_takes_precedence() {
        let source = MockGitSource {
            commit_count_since: Some(999_999),
        };
        let mut args = base_args();
        args.since = Some(1_700_000_000);
        args.window_preset = Some("sprint".to_string());
        args.last_n = Some(100);
        let mut warnings = Vec::new();

        let (config, window) = resolve_config(args, &source, &mut warnings, NOW).unwrap();

        assert_eq!(config.since, Some(1_700_000_000));
        assert_eq!(window.since, Some(1_700_000_000));
        // Multiple flags → warning emitted
        assert!(!warnings.is_empty());
    }

    /// When no window flags are set, dual-mode default is applied.
    #[test]
    fn test_resolve_config_dual_default() {
        let source = MockGitSource {
            commit_count_since: Some(1_680_000_000),
        };
        let args = base_args();
        let mut warnings = Vec::new();

        let (config, window) = resolve_config(args, &source, &mut warnings, NOW).unwrap();

        assert!(config.since.is_some());
        assert!(window.dual_mode);
        assert!(window.dual_time_since.is_some());
        assert!(window.dual_count_since.is_some());
    }
}
