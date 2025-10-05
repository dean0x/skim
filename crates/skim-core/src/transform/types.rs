//! Types mode transformation
//!
//! ARCHITECTURE: Extract ONLY type definitions.
//!
//! Token reduction target: 90-95%

use crate::{Language, Result, TransformConfig};
use tree_sitter::Tree;

/// Transform to types-only
///
/// # What to Keep
///
/// - Type aliases
/// - Interface declarations
/// - Enum definitions
/// - Struct definitions (Rust/Go)
/// - Class declarations (structure only, no methods)
///
/// # What to Remove
///
/// - ALL implementation code
/// - Function bodies
/// - Method implementations
/// - Comments
///
/// # Implementation Notes (Week 3)
///
/// Most aggressive mode. Types only, no code.
pub(crate) fn transform_types(
    source: &str,
    tree: &Tree,
    language: Language,
    config: &TransformConfig,
) -> Result<String> {
    todo!("Week 3: Implement type extraction")
}
