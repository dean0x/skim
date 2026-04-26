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
    if ok { success_mark() } else { fail_mark() }
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

/// Run `f` wrapped in a spinner (suppressed in JSON mode or non-TTY contexts).
///
/// Creates a spinner before calling `f` and clears it when `f` returns,
/// regardless of success or failure. In JSON mode the spinner is omitted
/// entirely so it does not contaminate stdout (D5, S5, S16).
pub(crate) fn with_spinner<T, E>(
    json_output: bool,
    msg: &str,
    f: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    let pb = (!json_output).then(|| spinner(msg));
    let result = f();
    if let Some(s) = pb {
        s.finish_and_clear();
    }
    result
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
