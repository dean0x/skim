//! Window resolution helpers for `skim heatmap`.
//!
//! Handles all time-window logic: preset mapping, dual-mode resolution,
//! `--since` / `--last` / `--window` precedence, and display formatting.

use super::git_source::GitDataSource;
use super::types::{HeatmapConfig, ResolvedWindow, WindowInfo};

// ============================================================================
// Preset mapping
// ============================================================================

/// Map a named preset to `--since` epoch seconds offset.
pub(super) fn preset_to_since_secs(preset: &str, now_epoch: u64) -> Option<u64> {
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

/// Resolve the effective `HeatmapConfig` by applying presets and `--last`.
///
/// Precedence: `--since` > `--last` > `--window` preset > dual default.
///
/// Returns a tuple of:
/// - `HeatmapConfig` with `since` set to the resolved epoch (used by `fetch_commits`)
/// - `ResolvedWindow` carrying window metadata (mode, dual fields) for `build_window_info`
pub(super) fn resolve_effective_config(
    config: &HeatmapConfig,
    source: &dyn GitDataSource,
    warnings: &mut Vec<String>,
    now_epoch: u64,
) -> anyhow::Result<(HeatmapConfig, ResolvedWindow)> {
    let mut effective = config.clone();
    let mut window = ResolvedWindow {
        since: None,
        dual_mode: false,
        dual_time_since: None,
        dual_count_since: None,
        window_preset: config.window_preset.clone(),
        last_n: config.last_n,
    };

    // Count explicit time-selection flags
    let explicit_count = [
        config.since.is_some(),
        config.last_n.is_some(),
        config.window_preset.is_some(),
    ]
    .into_iter()
    .filter(|b| *b)
    .count();

    if explicit_count > 1 {
        warnings.push(
            "Multiple window flags specified — using first (--since > --last > --window)."
                .to_string(),
        );
    }

    if let Some(since) = config.since {
        // Already set — highest precedence
        effective.since = Some(since);
        window.since = Some(since);
    } else if let Some(n) = config.last_n {
        // --last N: find the timestamp of the Nth commit
        match source.fetch_commit_count_since(n) {
            Ok(Some(ts)) => {
                effective.since = Some(ts);
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
    } else if let Some(ref preset) = config.window_preset {
        if let Some(since) = preset_to_since_secs(preset, now_epoch) {
            effective.since = Some(since);
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
        let resolved_since = time_since.min(count_since);
        effective.since = Some(resolved_since);
        window.since = Some(resolved_since);
        window.dual_mode = true;
        window.dual_time_since = Some(time_since);
        window.dual_count_since = Some(count_since);
    }

    Ok((effective, window))
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
#[allow(clippy::unwrap_used)]
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
        assert!(diff >= 13 * 86400 && diff <= 15 * 86400);
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
}
