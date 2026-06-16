//! Error types for `rskim-contract`.

use thiserror::Error;

/// Errors that can occur during construction or configuration.
///
/// `ContractError` is the only error type visible to callers. The transform
/// path (`Contract::transform`) never returns an error — it returns `Outcome`,
/// where passthrough is a success variant.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ContractError {
    /// The request body is structurally invalid JSON.
    ///
    /// The caller should fall back to passing the body through unmodified.
    #[error("invalid JSON in request body: {0}")]
    InvalidJson(#[from] serde_json::Error),

    /// A configuration value is out of range or otherwise invalid.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
}
