//! Caller-registered, component-specific invariant extensions.
//!
//! Beyond the eight core invariants, downstream tickets may register their own
//! per-component invariant checks with the harness. This module provides the
//! registration surface.
//!
//! # Extension invariants vs. core invariants
//!
//! Core invariants (1–8) are checked by the harness automatically for every
//! registered component. Extension invariants are opt-in, registered by a
//! specific component for a specific check it cares about.
//!
//! # Motivating example: marker-immutability (#308)
//!
//! The `rskim-proxy` crate (#308) injects `cache_control` markers. Once a
//! marker is injected, it must remain byte-identical in future passes
//! (marker-immutability invariant). This is not one of the eight core invariants
//! but can be registered as an extension:
//!
//! ```rust
//! use rskim_contract::extension::{InvariantCheck, ExtensionRegistry};
//!
//! let mut registry = ExtensionRegistry::new();
//!
//! registry.register(
//!     "marker-immutability",
//!     Box::new(|input: &[u8], output: &[u8]| {
//!         // If input contains markers, they must appear unchanged in output.
//!         let marker = b"\"cache_control\"";
//!         if input.windows(marker.len()).any(|w| w == marker) {
//!             output.windows(marker.len()).any(|w| w == marker)
//!         } else {
//!             true // No markers in input → invariant vacuously satisfied.
//!         }
//!     }),
//! );
//!
//! let input = b"{\"cache_control\":{\"type\":\"ephemeral\"}}";
//! let output = b"{\"cache_control\":{\"type\":\"ephemeral\"}}";
//! assert!(registry.check_all("marker-immutability", input, output));
//! ```

/// A single named invariant check: takes input and output bytes, returns `true`
/// if the invariant holds for this (input, output) pair.
///
/// A returning `false` means the component violated the registered invariant.
pub type InvariantCheck = dyn Fn(&[u8], &[u8]) -> bool + Send + Sync;

/// Result of running extension invariant checks.
#[derive(Debug, Clone)]
pub struct ExtensionCheckResult {
    /// The invariant name that was checked.
    pub invariant_name: String,
    /// `true` if the invariant held, `false` if it was violated.
    pub passed: bool,
}

/// Registry of caller-registered extension invariant checks.
///
/// The harness accepts a registry and runs all registered checks against each
/// (input, output) pair during the conformance run.
///
/// # Registration order
///
/// Checks are run in registration order. All registered checks are evaluated;
/// a failing check does not stop subsequent checks.
pub struct ExtensionRegistry {
    entries: Vec<(String, Box<InvariantCheck>)>,
}

impl ExtensionRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Register a named invariant check.
    ///
    /// The `name` is used in harness reports to identify which invariant failed.
    /// Names do not need to be unique, but uniqueness is recommended.
    pub fn register(&mut self, name: impl Into<String>, check: Box<InvariantCheck>) {
        self.entries.push((name.into(), check));
    }

    /// Run all checks registered under `invariant_name` against `(input, output)`.
    ///
    /// Returns `true` iff all matching checks pass. Returns `true` if no checks
    /// are registered under that name (vacuously true).
    pub fn check_all(&self, invariant_name: &str, input: &[u8], output: &[u8]) -> bool {
        self.entries
            .iter()
            .filter(|(name, _)| name == invariant_name)
            .all(|(_, check)| check(input, output))
    }

    /// Run all registered checks against `(input, output)`.
    ///
    /// Returns a result per registered entry.
    pub fn run_all(&self, input: &[u8], output: &[u8]) -> Vec<ExtensionCheckResult> {
        self.entries
            .iter()
            .map(|(name, check)| ExtensionCheckResult {
                invariant_name: name.clone(),
                passed: check(input, output),
            })
            .collect()
    }

    /// Returns the number of registered checks.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if no checks are registered.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for ExtensionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Built-in extension checks for downstream consumers
// ============================================================================

/// Create a marker-immutability invariant check (for #308 demonstration / AC16).
///
/// Returns a check that verifies: if `input` contains a `cache_control` marker,
/// the `output` must also contain it byte-identical.
///
/// This demonstrates the extension mechanism; the production #308 implementation
/// will register a more precise check.
pub fn marker_immutability_check() -> Box<InvariantCheck> {
    Box::new(|input: &[u8], output: &[u8]| {
        let marker = b"\"cache_control\"";
        let input_has_marker = input.windows(marker.len()).any(|w| w == marker.as_ref());
        if !input_has_marker {
            return true; // Vacuously satisfied: no markers in input.
        }
        output.windows(marker.len()).any(|w| w == marker.as_ref())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_empty_check_all_returns_true() {
        let registry = ExtensionRegistry::new();
        // No checks registered → vacuously true.
        assert!(registry.check_all("any-invariant", b"input", b"output"));
    }

    #[test]
    fn registry_check_all_passing() {
        let mut registry = ExtensionRegistry::new();
        registry.register("always-pass", Box::new(|_input, _output| true));
        assert!(registry.check_all("always-pass", b"input", b"output"));
    }

    #[test]
    fn registry_check_all_failing() {
        let mut registry = ExtensionRegistry::new();
        registry.register("always-fail", Box::new(|_input, _output| false));
        assert!(!registry.check_all("always-fail", b"input", b"output"));
    }

    #[test]
    fn registry_check_all_filters_by_name() {
        let mut registry = ExtensionRegistry::new();
        registry.register("pass", Box::new(|_, _| true));
        registry.register("fail", Box::new(|_, _| false));
        // Only "pass" checks run.
        assert!(registry.check_all("pass", b"x", b"y"));
        // Only "fail" checks run.
        assert!(!registry.check_all("fail", b"x", b"y"));
    }

    #[test]
    fn registry_run_all_returns_all_results() {
        let mut registry = ExtensionRegistry::new();
        registry.register("check-a", Box::new(|_, _| true));
        registry.register("check-b", Box::new(|_, _| false));
        let results = registry.run_all(b"in", b"out");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].invariant_name, "check-a");
        assert!(results[0].passed);
        assert_eq!(results[1].invariant_name, "check-b");
        assert!(!results[1].passed);
    }

    #[test]
    fn registry_len_and_is_empty() {
        let mut registry = ExtensionRegistry::new();
        assert!(registry.is_empty());
        registry.register("x", Box::new(|_, _| true));
        assert_eq!(registry.len(), 1);
        assert!(!registry.is_empty());
    }

    // ========================================================================
    // Marker-immutability extension check (AC16)
    // ========================================================================

    #[test]
    fn marker_immutability_input_has_marker_output_has_it() {
        let check = marker_immutability_check();
        let input = b"{\"cache_control\":{\"type\":\"ephemeral\"}}";
        let output = b"{\"cache_control\":{\"type\":\"ephemeral\"}}";
        assert!(check(input, output));
    }

    #[test]
    fn marker_immutability_input_has_marker_output_drops_it() {
        let check = marker_immutability_check();
        let input = b"{\"cache_control\":{\"type\":\"ephemeral\"}}";
        let output = b"{}"; // marker dropped
        assert!(!check(input, output));
    }

    #[test]
    fn marker_immutability_no_marker_in_input_vacuously_true() {
        let check = marker_immutability_check();
        let input = b"{\"model\":\"gpt-4o\"}";
        let output = b"{\"model\":\"gpt-4o-mini\"}"; // modified, but no marker
        assert!(check(input, output));
    }

    #[test]
    fn registry_with_marker_immutability() {
        let mut registry = ExtensionRegistry::new();
        registry.register("marker-immutability", marker_immutability_check());

        // Reference impl: input with marker, output preserves it → pass.
        let input = b"x\"cache_control\"y";
        let output = b"x\"cache_control\"y";
        assert!(registry.check_all("marker-immutability", input, output));

        // Marker-dropping impl: input with marker, output drops it → fail.
        let bad_output = b"xy";
        assert!(!registry.check_all("marker-immutability", input, bad_output));
    }
}
