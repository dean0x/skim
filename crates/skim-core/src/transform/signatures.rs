//! Signatures mode transformation
//!
//! ARCHITECTURE: Extract ONLY function/method signatures.
//!
//! Token reduction target: 85-92%

use crate::{Language, Result, TransformConfig};
use tree_sitter::Tree;

/// Transform to signatures-only
///
/// # What to Keep
///
/// - Function/method signatures ONLY
/// - Type signatures (minimal context)
///
/// # What to Remove
///
/// - ALL implementation code
/// - Class bodies (keep class name + methods)
/// - Type implementations
/// - Comments
///
/// # Implementation Notes (Week 3)
///
/// More aggressive than structure mode.
/// Extract callable signatures, discard everything else.
pub(crate) fn transform_signatures(
    source: &str,
    tree: &Tree,
    language: Language,
    config: &TransformConfig,
) -> Result<String> {
    todo!("Week 3: Implement signature extraction")
}
