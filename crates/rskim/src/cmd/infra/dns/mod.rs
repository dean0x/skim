//! DNS tool parsers for `dig` and `nslookup` with three-tier degradation (#168).
//!
//! Executes `dig` or `nslookup` and parses the output into structured `InfraResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: Regex for DNS records, status, and query metadata
//! - **Tier 2 (Degraded)**: Simpler regex fallback for partial output
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation
//!
//! Both tools use `combine_stdout_stderr: true` because DNS errors may appear
//! on stderr (e.g., SERVFAIL, connection refused).
//!
//! # Design
//!
//! Two independent entry points (`run_dig`, `run_nslookup`) with two separate
//! configs and two separate parse chains. They share regex utilities but have
//! independent tier logic because dig and nslookup have fundamentally different
//! output formats.
//!
//! The no-args guard on `run_nslookup` prevents interactive mode — nslookup
//! with no arguments drops into an interactive resolver shell, which hangs
//! indefinitely in agent contexts.

pub(crate) mod dig;
pub(crate) mod nslookup;

pub(crate) use dig::run_dig;
pub(crate) use nslookup::run_nslookup;

// ============================================================================
// Shared test helpers
// ============================================================================

#[cfg(test)]
pub(super) mod test_helpers {
    use crate::runner::CommandOutput;

    pub(super) fn load_fixture(name: &str) -> String {
        let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests/fixtures/cmd/infra");
        path.push(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("Failed to load fixture '{name}': {e}"))
    }

    pub(super) fn make_output(combined: &str) -> CommandOutput {
        CommandOutput {
            stdout: combined.to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        }
    }
}
