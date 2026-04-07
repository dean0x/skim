//! Debug-mode flag for conditional warning emission.
//!
//! Controls whether `emit_markers()` writes degradation warnings to stderr.
//! Disabled by default — enable with `--debug` CLI flag or `SKIM_DEBUG` env var.
//!
//! # Startup sequence
//!
//! Call [`init_debug_from_env`] once in `main()` before any threads are spawned.
//! After that, [`is_debug_enabled`] is a single atomic load with no syscalls.

use std::sync::atomic::{AtomicBool, Ordering};

/// Process-wide flag that enables debug output.
/// Written with `Release` ordering so all subsequent `Acquire` loads observe
/// the store even on weakly-ordered architectures.
static DEBUG_FORCE_ENABLED: AtomicBool = AtomicBool::new(false);

/// Enable debug output for the lifetime of this process.
///
/// Thread-safe alternative to `std::env::set_var("SKIM_DEBUG", "1")`.
/// Call this early in `main()` when `--debug` is detected, before spawning
/// any background threads.
pub(crate) fn force_enable_debug() {
    DEBUG_FORCE_ENABLED.store(true, Ordering::Release);
}

/// Initialise the debug flag from the `SKIM_DEBUG` environment variable.
///
/// Call once in `main()` before spawning any threads.  After this call,
/// [`is_debug_enabled`] never touches the environment again — it is a single
/// atomic load.
pub(crate) fn init_debug_from_env() {
    let enabled = std::env::var("SKIM_DEBUG")
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    if enabled {
        force_enable_debug();
    }
}

/// Check if debug output is enabled.
///
/// Returns `true` when [`force_enable_debug`] or [`init_debug_from_env`] has
/// been called (i.e., `--debug` flag or a truthy `SKIM_DEBUG` env var was
/// detected at startup).
///
/// This is a pure atomic load — no allocations, no syscalls.
pub(crate) fn is_debug_enabled() -> bool {
    DEBUG_FORCE_ENABLED.load(Ordering::Acquire)
}

/// Reset the debug flag to `false`.
///
/// Only available in test builds.  Call at the start of any test that invokes
/// [`force_enable_debug`] so it does not poison the state of later tests that
/// run in the same process.
#[cfg(test)]
pub(crate) fn reset_debug_for_tests() {
    DEBUG_FORCE_ENABLED.store(false, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_force_enable_debug() {
        reset_debug_for_tests();
        force_enable_debug();
        assert!(is_debug_enabled());
        reset_debug_for_tests();
    }

    #[test]
    fn test_reset_clears_flag() {
        force_enable_debug();
        reset_debug_for_tests();
        assert!(!is_debug_enabled());
    }
}
