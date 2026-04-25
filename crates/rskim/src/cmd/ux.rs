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
            .unwrap(),
    );
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(Duration::from_millis(80));
    pb
}
