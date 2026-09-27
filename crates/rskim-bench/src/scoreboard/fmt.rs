//! Value formatting shared across the scoreboard (#203): report floats
//! rounded to 4 decimal places, path lists cut to a short sample for a
//! failure detail, and the `entry` / `entries` noun for a count.
//!
//! A leaf module: it imports nothing from the scoreboard, so `metrics`,
//! `structural_metrics`, `report`, `golden_gen`, `pipeline` and the
//! `scoreboard` binary all use it without importing one another.

/// Paths quoted in a failure detail before "+N more".
const SAMPLE_PATHS: usize = 5;

/// Round to 4 decimal places (report and baseline floats).
pub fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

/// Up to [`SAMPLE_PATHS`] paths, then `(+N more)`.
pub(crate) fn sample<'a>(items: impl IntoIterator<Item = &'a str>) -> String {
    let items: Vec<&str> = items.into_iter().collect();
    let shown = items
        .iter()
        .take(SAMPLE_PATHS)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    match items.len().saturating_sub(SAMPLE_PATHS) {
        0 => shown,
        more => format!("{shown} (+{more} more)"),
    }
}

/// `entry` or `entries`, whichever agrees with a count of `n`.
pub fn entries_noun(n: usize) -> &'static str {
    if n == 1 { "entry" } else { "entries" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round4_keeps_four_decimal_places() {
        assert_eq!(round4(1.0 / 3.0), 0.3333);
        assert_eq!(round4(2.0 / 3.0), 0.6667);
        assert_eq!(round4(0.5), 0.5);
    }

    #[test]
    fn a_sample_shows_five_paths_then_counts_the_rest() {
        assert_eq!(sample([]), "");
        assert_eq!(sample(["a", "b"]), "a, b");
        assert_eq!(sample(["a", "b", "c", "d", "e"]), "a, b, c, d, e");
        assert_eq!(
            sample(["a", "b", "c", "d", "e", "f", "g"]),
            "a, b, c, d, e (+2 more)"
        );
    }

    #[test]
    fn entries_noun_agrees_with_the_count() {
        assert_eq!(entries_noun(0), "entries");
        assert_eq!(entries_noun(1), "entry");
        assert_eq!(entries_noun(2), "entries");
    }
}
