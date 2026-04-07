//! Debug-mode flag for conditional warning emission.
//!
//! Controls whether `emit_markers()` writes degradation warnings to stderr.
//! Disabled by default — enable with `--debug` CLI flag or `SKIM_DEBUG` env var.

use std::sync::atomic::{AtomicBool, Ordering};

/// Process-wide flag that enables debug output.
/// Set via [`force_enable_debug`] at startup, before any background threads
/// are spawned. Checked by [`is_debug_enabled`].
static DEBUG_FORCE_ENABLED: AtomicBool = AtomicBool::new(false);

/// Enable debug output for the lifetime of this process.
///
/// Thread-safe alternative to `std::env::set_var("SKIM_DEBUG", "1")`.
/// Call this early in `main()` when `--debug` is detected.
pub(crate) fn force_enable_debug() {
    DEBUG_FORCE_ENABLED.store(true, Ordering::Relaxed);
}

/// Check if debug output is enabled.
///
/// Returns `true` when:
/// - [`force_enable_debug`] has been called (e.g., `--debug` flag), OR
/// - `SKIM_DEBUG` env var is set to a truthy value
///   (`1`, `true`, or `yes`, case-insensitive).
///
/// Any other value (including `0`, `false`, `no`) keeps debug disabled.
/// Unsetting the variable also keeps debug disabled (the default).
pub(crate) fn is_debug_enabled() -> bool {
    if DEBUG_FORCE_ENABLED.load(Ordering::Relaxed) {
        return true;
    }
    std::env::var("SKIM_DEBUG")
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_force_enable_debug() {
        force_enable_debug();
        assert!(is_debug_enabled());
    }
}
