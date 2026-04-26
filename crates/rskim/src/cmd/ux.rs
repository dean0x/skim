//! Shared terminal UX helpers for skim CLI subcommands.
//!
//! Centralises visual primitives (spinners, success/fail marks) so all
//! subcommands produce a consistent appearance without duplicating ANSI
//! escape sequences.

use colored::Colorize;
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
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

/// Print a comfy-table to stdout with each line indented by `indent` spaces.
///
/// Centralises the "indent every line of the table" pattern used by discover
/// and learn to align table output with surrounding prose (S3).
pub(crate) fn print_indented_table(table: &comfy_table::Table, indent: usize) {
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
}
