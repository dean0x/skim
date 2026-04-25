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
/// The spinner ticks every 80 ms and writes to stderr so it does not
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
    pb.enable_steady_tick(Duration::from_millis(80));
    pb
}
