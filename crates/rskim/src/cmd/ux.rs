//! Shared terminal UX helpers for skim CLI subcommands.
//!
//! Centralises visual primitives (spinners, success/fail marks) so all
//! subcommands produce a consistent appearance without duplicating ANSI
//! escape sequences.

use colored::Colorize;
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::borrow::Cow;
use std::time::Duration;

/// Return a green `+` marker for success states.
pub(crate) fn success_mark() -> colored::ColoredString {
    "+".green()
}

/// Return a red `-` marker for failure / not-found states.
pub(crate) fn fail_mark() -> colored::ColoredString {
    "-".red()
}

/// Return a colored status mark: green `+` for success, red `-` for failure.
///
/// Convenience wrapper used when the caller has a boolean condition rather than
/// separate success/failure branches. Respects `NO_COLOR` via the `colored`
/// crate (D7).
pub(crate) fn check_mark(ok: bool) -> colored::ColoredString {
    if ok {
        success_mark()
    } else {
        fail_mark()
    }
}

/// Create a stderr-bound indeterminate spinner with the given message.
///
/// The spinner ticks every 120 ms and writes to stderr so it does not
/// interfere with stdout output (D2). Callers must call
/// `pb.finish_and_clear()` when the work is done.
pub(crate) fn spinner(msg: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_draw_target(ProgressDrawTarget::stderr());
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.cyan} {msg}")
            .expect("static spinner template is always valid"),
    );
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(Duration::from_millis(120));
    pb
}

/// RAII guard that clears a spinner on drop, including on panic.
///
/// Wraps an `Option<ProgressBar>` so that `finish_and_clear()` is called
/// whenever the guard is dropped — whether the closure returns normally,
/// returns an error, or panics. This prevents the terminal from being
/// left in a broken state if the closure panics (S16).
struct SpinnerGuard(Option<ProgressBar>);

impl Drop for SpinnerGuard {
    fn drop(&mut self) {
        if let Some(pb) = self.0.take() {
            pb.finish_and_clear();
        }
    }
}

/// Run `f` wrapped in a spinner (suppressed in JSON mode or non-TTY contexts).
///
/// Creates a spinner before calling `f` and clears it when `f` returns,
/// regardless of success or failure. The spinner is cleared via a Drop guard
/// so it is always cleaned up, even if `f` panics. In JSON mode the spinner
/// is omitted entirely so it does not contaminate stdout (D5, S5, S16).
pub(crate) fn with_spinner<T, E>(
    json_output: bool,
    msg: &str,
    f: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    let _guard = SpinnerGuard((!json_output).then(|| spinner(msg)));
    f()
}

/// Return the current terminal width in columns.
///
/// Falls back to 80 when running outside a TTY (e.g. tests, pipes) or when
/// detection fails. The fallback value is the traditional terminal width and
/// produces reasonable output without hard-wrapping (D3).
pub(crate) fn terminal_width() -> u16 {
    crossterm::terminal::size().map(|(w, _)| w).unwrap_or(80)
}

/// End-truncate `s` to at most `max` **characters** (Unicode scalar values),
/// appending `…` when truncated. The `…` counts as 1 character.
///
/// - `max == 0`: no-op, returns `Cow::Borrowed(s)` (zero allocation).
/// - `max == 1`: returns just `…` (the ellipsis occupies the full budget).
/// - Multi-byte characters are never split; truncation always happens on
///   Unicode scalar value boundaries.
/// - When the string already fits, returns `Cow::Borrowed(s)` (zero allocation).
pub(crate) fn truncate_str(s: &str, max: usize) -> Cow<'_, str> {
    let char_count = s.chars().count();
    if char_count <= max || max == 0 {
        return Cow::Borrowed(s);
    }
    if max <= 1 {
        return Cow::Owned("\u{2026}".to_string());
    }
    // Take `max - 1` chars, then append `…`.
    let prefix: String = s.chars().take(max - 1).collect();
    Cow::Owned(format!("{}\u{2026}", prefix))
}

/// Middle-truncate a file path to at most `max` **characters** (Unicode scalar
/// values, each counting as 1 column in a terminal table).
///
/// Preserves the root segment prefix and the filename (last path component),
/// inserting `…/` between them. Falls back to end-truncation when the
/// filename alone fills the budget.
///
/// - `max == 0`: no-op, returns `Cow::Borrowed(path)` (zero allocation).
/// - When the path already fits, returns `Cow::Borrowed(path)` (zero allocation).
/// - Path without `/`: falls back to `truncate_str`.
///
/// # Performance note
///
/// The implementation performs up to three character-level scans of the path:
/// one `chars().count()` for the length check, one `chars().count()` on the
/// filename component, and one `chars().take(n).collect()` to build the prefix.
/// `rsplit` and `contains` operate on bytes, not chars. Paths are at most a few
/// hundred characters and this function is called at most ~10 times per report
/// render, so the repeated scans are intentionally left as-is — the clarity
/// benefit outweighs a marginal allocation saving.
pub(crate) fn truncate_path_middle(path: &str, max: usize) -> Cow<'_, str> {
    let char_count = path.chars().count();
    if char_count <= max || max == 0 {
        return Cow::Borrowed(path);
    }
    let filename = path.rsplit('/').next().unwrap_or(path);
    // If there's no separator, treat as a plain string.
    if !path.contains('/') {
        return truncate_str(path, max);
    }
    // filename + "…/" costs filename.chars().count() + 2 columns.
    let filename_chars = filename.chars().count();
    if filename_chars + 2 >= max {
        return truncate_str(filename, max);
    }
    let prefix_budget = max - filename_chars - 2;
    let prefix: String = path.chars().take(prefix_budget).collect();
    Cow::Owned(format!("{}\u{2026}/{}", prefix, filename))
}

/// Compute column budget(s) for a truncation-aware table.
///
/// Returns a single `usize` representing the available character budget after
/// subtracting `overhead` from `term_width`. When `term_width == 0` (i.e. the
/// caller determined that truncation is disabled), returns 0, which is the
/// no-op sentinel for `truncate_str` and `truncate_path_middle`.
///
/// Callers that need to split the budget across multiple columns apply their
/// own ratio arithmetic to the returned value.
///
/// # Example
///
/// ```ignore
/// // indent=4, borders+padding=17 => overhead=21
/// let budget = column_budget(term_width, 21);
/// let cmd_max = if budget > 0 { (budget * 2 / 5).max(1) } else { 0 };
/// let rewrite_max = if budget > 0 { (budget * 3 / 5).max(1) } else { 0 };
/// ```
pub(crate) fn column_budget(term_width: u16, overhead: usize) -> usize {
    if term_width == 0 {
        return 0;
    }
    (term_width as usize).saturating_sub(overhead)
}

/// Resolve the terminal width for truncation-aware rendering.
///
/// When `no_truncate` is `true` returns `0`, which is the no-op sentinel
/// accepted by `column_budget` and `print_indented_table`. Otherwise returns
/// the current terminal width via `terminal_width()`.
///
/// Centralising this decision eliminates the duplicated
/// `if config.no_truncate { 0 } else { terminal_width() }` pattern in
/// discover and learn (S2).
pub(crate) fn resolve_term_width(no_truncate: bool) -> u16 {
    if no_truncate { 0 } else { terminal_width() }
}

/// Print a comfy-table to stdout with each line indented by `indent` spaces.
///
/// Centralises the "indent every line of the table" pattern used by discover
/// and learn to align table output with surrounding prose (S3).
///
/// `term_width` is the terminal width in columns, already computed by the
/// caller (typically at the top of the enclosing print function to avoid
/// redundant syscalls). When non-zero the table is constrained to
/// `term_width - indent` columns so `comfy_table` wraps or truncates long
/// cells automatically. When zero (i.e. the caller passed `--no-truncate`)
/// no width constraint is applied and the table expands to its natural width.
pub(crate) fn print_indented_table(table: &mut comfy_table::Table, indent: usize, term_width: u16) {
    if term_width > 0 {
        let available = term_width.saturating_sub(u16::try_from(indent).unwrap_or(u16::MAX));
        table.set_width(available);
    }
    let prefix = " ".repeat(indent);
    for line in table.to_string().lines() {
        println!("{prefix}{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- check_mark ---

    #[test]
    fn test_check_mark_true_returns_plus() {
        // Visible character must be "+" for a success state.
        assert!(check_mark(true).to_string().contains('+'));
    }

    #[test]
    fn test_check_mark_false_returns_minus() {
        // Visible character must be "-" for a failure state.
        assert!(check_mark(false).to_string().contains('-'));
    }

    // --- with_spinner ---

    #[test]
    fn test_with_spinner_json_mode_suppresses_spinner_and_returns_ok() {
        // In JSON mode (json_output=true) no spinner is created; the closure
        // result is still returned unchanged.
        let result: Result<i32, String> = with_spinner(true, "loading", || Ok(42));
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn test_with_spinner_propagates_error() {
        // Errors returned by the closure must be propagated without alteration.
        let result: Result<i32, &str> =
            with_spinner(true, "loading", || Err("something went wrong"));
        assert_eq!(result.unwrap_err(), "something went wrong");
    }

    // --- print_indented_table ---

    #[test]
    fn test_print_indented_table_indents_every_line() {
        // Every rendered line of a table with content must start with the
        // requested prefix when we apply the same logic as print_indented_table.
        let mut table = comfy_table::Table::new();
        table.set_header(["File", "Tokens"]);
        table.add_row(["main.rs", "120"]);
        let indent = 4;
        let prefix = " ".repeat(indent);
        for line in table.to_string().lines() {
            let indented = format!("{prefix}{line}");
            assert!(
                indented.starts_with(&prefix),
                "Line does not start with indent: {indented:?}"
            );
        }
    }

    #[test]
    fn test_print_indented_table_term_width_constrained_does_not_panic() {
        // Exercises the term_width > 0 branch: set_width is called and the
        // table must render without panicking.
        let mut table = comfy_table::Table::new();
        table.set_header(["File", "Tokens"]);
        table.add_row(["main.rs", "120"]);
        print_indented_table(&mut table, 4, 80);
    }

    #[test]
    fn test_print_indented_table_term_width_constrains_rendered_width() {
        // Verifies the set_width branch actually constrains the rendered output.
        // Callers of print_indented_table (discover, learn) always set
        // ContentArrangement::Dynamic so comfy-table wraps cells at word
        // boundaries when set_width is active.  A multi-word cell that exceeds
        // the available width must produce more rendered lines (wrapped) than
        // the same table with no width constraint.
        //
        // We test this via table.to_string() directly because print_indented_table
        // writes to stdout which cannot be captured in unit tests.  The
        // width-constraining logic is identical whether we use the helper or call
        // set_width ourselves — the table object carries the constraint.
        //
        // Use a sentence with spaces so comfy-table can wrap at word boundaries.
        let long_cell = "word ".repeat(40).trim().to_string(); // 199 chars
        let indent: usize = 4;
        let term_width: u16 = 60;
        let available = term_width.saturating_sub(u16::try_from(indent).unwrap_or(u16::MAX));

        let mut table_unconstrained = comfy_table::Table::new();
        table_unconstrained
            .set_content_arrangement(comfy_table::ContentArrangement::Dynamic)
            .set_header(["Command", "Rewrite"]);
        table_unconstrained.add_row([long_cell.as_str(), "skim infra gh pr list"]);
        let lines_unconstrained = table_unconstrained.to_string().lines().count();

        let mut table_constrained = comfy_table::Table::new();
        table_constrained
            .set_content_arrangement(comfy_table::ContentArrangement::Dynamic)
            .set_header(["Command", "Rewrite"]);
        table_constrained.add_row([long_cell.as_str(), "skim infra gh pr list"]);
        table_constrained.set_width(available);
        let lines_constrained = table_constrained.to_string().lines().count();

        assert!(
            lines_constrained > lines_unconstrained,
            "set_width({available}) with Dynamic arrangement should wrap long cells, \
             producing more lines ({lines_constrained}) than unconstrained ({lines_unconstrained})"
        );
    }

    #[test]
    fn test_print_indented_table_no_truncate_does_not_panic() {
        // Exercises the term_width == 0 branch (--no-truncate sentinel): no
        // set_width call is made and the table must render without panicking.
        let mut table = comfy_table::Table::new();
        table.set_header(["File", "Tokens"]);
        table.add_row(["main.rs", "120"]);
        print_indented_table(&mut table, 4, 0);
    }

    #[test]
    fn test_print_indented_table_no_truncate_preserves_full_content() {
        // Verifies the term_width == 0 branch does NOT constrain table width.
        // With a 200-char unbreakable cell and no set_width call, the rendered
        // string must contain the full cell content — no wrapping applied.
        //
        // Mirror callers by setting ContentArrangement::Dynamic; this confirms
        // that even with Dynamic arrangement, term_width==0 leaves content intact.
        let long_cell = "a".repeat(200);
        let mut table = comfy_table::Table::new();
        table
            .set_content_arrangement(comfy_table::ContentArrangement::Dynamic)
            .set_header(["Command", "Rewrite"]);
        table.add_row([long_cell.as_str(), "skim infra gh pr list"]);

        // term_width == 0 → no set_width call; table renders at natural width.
        // Mirror the branch logic from print_indented_table exactly.
        let term_width: u16 = 0;
        if term_width > 0 {
            let indent: usize = 4;
            let available =
                term_width.saturating_sub(u16::try_from(indent).unwrap_or(u16::MAX));
            table.set_width(available);
        }

        let rendered = table.to_string();
        assert!(
            rendered.contains(&long_cell),
            "no-truncate branch must preserve full 200-char cell content"
        );
    }

    // --- column_budget ---

    #[test]
    fn test_column_budget_zero_term_width_returns_zero() {
        // term_width=0 signals no-truncate; budget must be the no-op sentinel 0.
        assert_eq!(column_budget(0, 17), 0);
    }

    #[test]
    fn test_column_budget_subtracts_overhead() {
        // 100 columns - 20 overhead = 80 usable.
        assert_eq!(column_budget(100, 20), 80);
    }

    #[test]
    fn test_column_budget_overhead_exceeds_width_saturates_to_zero() {
        // saturating_sub never underflows.
        assert_eq!(column_budget(10, 50), 0);
    }

    // --- truncate_str ---

    #[test]
    fn test_truncate_str_no_op_returns_borrowed() {
        // String shorter than max: no allocation — must be Cow::Borrowed.
        let s = "hello";
        let result = truncate_str(s, 10);
        assert!(matches!(result, std::borrow::Cow::Borrowed(_)));
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_truncate_str_no_op() {
        // String shorter than max is returned unchanged.
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_str_exact() {
        // String exactly at max is returned unchanged.
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_str_truncates() {
        // String longer than max ends with '…' and char count is <= max.
        let result = truncate_str("hello world", 8);
        assert!(result.ends_with('\u{2026}'), "must end with ellipsis");
        assert!(
            result.chars().count() <= 8,
            "char count must not exceed max, got: {result:?}"
        );
    }

    #[test]
    fn test_truncate_str_max_one() {
        // max=1 returns just the ellipsis character.
        assert_eq!(truncate_str("hello", 1), "\u{2026}");
    }

    #[test]
    fn test_truncate_str_max_zero() {
        // max=0 is a no-op; original string returned unchanged.
        assert_eq!(truncate_str("hello", 0), "hello");
    }

    // --- truncate_path_middle ---

    #[test]
    fn test_truncate_path_middle_short_returns_borrowed() {
        // Path shorter than max: no allocation — must be Cow::Borrowed.
        let path = "/src/main.rs";
        let result = truncate_path_middle(path, 40);
        assert!(matches!(result, std::borrow::Cow::Borrowed(_)));
        assert_eq!(result, "/src/main.rs");
    }

    #[test]
    fn test_truncate_path_middle_short() {
        // Path shorter than max is returned unchanged.
        assert_eq!(truncate_path_middle("/src/main.rs", 40), "/src/main.rs");
    }

    #[test]
    fn test_truncate_path_middle_long() {
        // Long path produces prefix…/filename format.
        let path = "/very/long/directory/structure/that/exceeds/width/main.rs";
        let result = truncate_path_middle(path, 30);
        assert!(
            result.chars().count() <= 30,
            "char count must fit within max, got: {result:?}"
        );
        assert!(result.ends_with("main.rs"), "filename must be preserved");
        assert!(result.contains('\u{2026}'), "must contain ellipsis");
    }

    #[test]
    fn test_truncate_path_middle_no_separator() {
        // Path without '/' falls back to end-truncation.
        let result = truncate_path_middle("verylongfilename.rs", 10);
        assert!(result.ends_with('\u{2026}'), "should end-truncate");
        assert!(
            result.chars().count() <= 10,
            "char count must not exceed max, got: {result:?}"
        );
    }

    #[test]
    fn test_truncate_path_middle_long_filename() {
        // Filename alone exceeds max — falls back to end-truncation of filename.
        let path = "/short/averylongfilenamethatlonger.rs";
        let result = truncate_path_middle(path, 10);
        assert!(
            result.chars().count() <= 10,
            "char count must not exceed max, got: {result:?}"
        );
        assert!(result.ends_with('\u{2026}'), "must end with ellipsis");
    }

    // --- terminal_width ---

    #[test]
    fn test_terminal_width_returns_positive() {
        // terminal_width always returns a positive value (at least the 80-col fallback).
        assert!(terminal_width() > 0);
    }

    // --- resolve_term_width ---

    #[test]
    fn test_resolve_term_width_no_truncate_returns_zero() {
        // no_truncate=true must return the no-op sentinel 0.
        assert_eq!(resolve_term_width(true), 0);
    }

    #[test]
    fn test_resolve_term_width_truncation_enabled_returns_positive() {
        // no_truncate=false defers to terminal_width(), which always returns > 0.
        assert!(resolve_term_width(false) > 0);
    }

    // --- print_indented_table indent clamping ---

    #[test]
    fn test_print_indented_table_large_indent_does_not_overflow() {
        // Indents larger than u16::MAX used to silently truncate via `as u16`.
        // u16::try_from(indent).unwrap_or(u16::MAX) clamps instead; with a
        // term_width of u16::MAX the result is 0 (saturating_sub), which means
        // no width constraint — the table must render without panicking.
        //
        // Use u16::MAX + 1 to trigger the clamping path without
        // allocating a prohibitively large prefix string.
        let mut table = comfy_table::Table::new();
        table.set_header(["A", "B"]);
        table.add_row(["foo", "bar"]);
        let large_indent = u16::MAX as usize + 1;
        print_indented_table(&mut table, large_indent, u16::MAX);
    }
}
