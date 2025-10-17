//! Skim Core - Smart code reading and transformation library
//!
//! # Overview
//!
//! `skim-core` is a pure library for transforming source code by stripping
//! implementation details while preserving structure, signatures, and types.
//! Optimized for AI/LLM context windows.
//!
//! # Architecture
//!
//! **IMPORTANT: This is a LIBRARY with NO I/O.**
//! - Accepts `&str` (source code), not file paths
//! - Returns `Result<String>`, not stdout writes
//! - Pure transformations, no side effects
//!
//! CLI/SDK/MCP interfaces handle I/O separately.
//!
//! # Example
//!
//! ```no_run
//! use rskim_core::{transform, Language, Mode};
//!
//! let source = "function add(a: number, b: number) { return a + b; }";
//! let result = transform(source, Language::TypeScript, Mode::Structure)?;
//!
//! // Result: "function add(a: number, b: number) { /* ... */ }"
//! # Ok::<(), rskim_core::SkimError>(())
//! ```
//!
//! # Design Principles
//!
//! 1. **Zero-copy where possible** - Use `&str` slices, avoid allocations
//! 2. **Result types everywhere** - NO panics (enforced by clippy)
//! 3. **Dependency injection** - NO global state
//! 4. **Type-first** - Complete type schema before implementation

// Re-export core types for public API
pub use types::{
    Language,
    Mode,
    TransformConfig,
    TransformResult,
    SkimError,
    Result,
    Parser,
};

mod types;
mod parser;
mod transform;

// NOTE: Caching is implemented at the CLI layer (rskim binary), not in the core library.
// The core library remains pure and I/O-free.
// See: crates/rskim/src/cache.rs for the caching implementation.

// ============================================================================
// Public API - Core Transformation Functions
// ============================================================================

/// Transform source code based on mode
///
/// This is the PRIMARY function for transformation.
///
/// # Arguments
///
/// * `source` - Source code as string slice (zero-copy)
/// * `language` - Programming language for parsing
/// * `mode` - Transformation mode (Structure, Signatures, Types, Full)
///
/// # Returns
///
/// Transformed source code as `String`, or error if parsing fails.
///
/// # Performance
///
/// Target: <50ms for 1000-line files
/// - Parse: ~5-10ms (tree-sitter)
/// - Transform: ~10-20ms (AST traversal)
/// - String building: ~5-10ms
///
/// # Errors
///
/// - `SkimError::ParseError` - tree-sitter failed to parse
/// - `SkimError::TreeSitterError` - Grammar loading failed
///
/// # Examples
///
/// ```no_run
/// use rskim_core::{transform, Language, Mode};
///
/// let typescript = "function greet(name: string) { console.log(`Hello, ${name}`); }";
/// let result = transform(typescript, Language::TypeScript, Mode::Structure)?;
///
/// assert!(result.contains("function greet(name: string)"));
/// assert!(!result.contains("console.log"));
/// # Ok::<(), rskim_core::SkimError>(())
/// ```
pub fn transform(
    source: &str,
    language: Language,
    mode: Mode,
) -> Result<String> {
    // ARCHITECTURE: Use default config for simple API
    transform_with_config(source, language, &TransformConfig::with_mode(mode))
}

/// Transform source code with custom configuration
///
/// Advanced API that accepts full configuration struct.
///
/// # Arguments
///
/// * `source` - Source code as string slice
/// * `language` - Programming language
/// * `config` - Full transformation configuration
///
/// # Examples
///
/// ```no_run
/// use rskim_core::{transform_with_config, Language, Mode, TransformConfig};
///
/// let config = TransformConfig::with_mode(Mode::Signatures)
///     .preserve_comments(false);
///
/// let result = transform_with_config("fn main() {}", Language::Rust, &config)?;
/// # Ok::<(), rskim_core::SkimError>(())
/// ```
pub fn transform_with_config(
    source: &str,
    language: Language,
    config: &TransformConfig,
) -> Result<String> {
    // 1. Create parser for language
    let mut parser = Parser::new(language)?;

    // 2. Parse source code
    let tree = parser.parse(source)?;

    // 3. Transform based on mode
    transform::transform_tree(source, &tree, language, config)
}

/// Transform source code with automatic language detection from file path
///
/// Convenience function that detects language from file extension.
///
/// # Arguments
///
/// * `source` - Source code as string slice
/// * `path` - File path for language detection (NOT read from disk)
/// * `mode` - Transformation mode
///
/// # Errors
///
/// - `SkimError::UnsupportedLanguage` - Could not detect language from path
/// - All errors from `transform()`
///
/// # Examples
///
/// ```no_run
/// use rskim_core::{transform_auto, Mode};
/// use std::path::Path;
///
/// let source = "def hello(): pass";
/// let path = Path::new("script.py");
/// let result = transform_auto(source, path, Mode::Structure)?;
/// # Ok::<(), rskim_core::SkimError>(())
/// ```
pub fn transform_auto(
    source: &str,
    path: &std::path::Path,
    mode: Mode,
) -> Result<String> {
    let language = Language::from_path(path)
        .ok_or_else(|| SkimError::UnsupportedLanguage(path.to_path_buf()))?;

    transform(source, language, mode)
}

/// Transform source code with full result metadata
///
/// Returns `TransformResult` with optional token counts and timing.
/// Useful for benchmarking and analysis.
///
/// # Phase 3 Feature
///
/// Token counting requires `token-counting` feature flag.
///
/// # Examples
///
/// ```no_run
/// use rskim_core::{transform_detailed, Language, Mode};
///
/// let result = transform_detailed("code", Language::Python, Mode::Structure)?;
///
/// println!("Transformed: {}", result.content);
/// if let Some(reduction) = result.reduction_percentage() {
///     println!("Token reduction: {:.1}%", reduction);
/// }
/// # Ok::<(), rskim_core::SkimError>(())
/// ```
pub fn transform_detailed(
    source: &str,
    language: Language,
    mode: Mode,
) -> Result<TransformResult> {
    let start = std::time::Instant::now();

    let content = transform(source, language, mode)?;

    let duration_ms = start.elapsed().as_millis() as u64;

    Ok(TransformResult {
        content,
        original_tokens: None,      // TODO: Implement in Phase 3
        transformed_tokens: None,   // TODO: Implement in Phase 3
        duration_ms: Some(duration_ms),
    })
}

// ============================================================================
// Language Detection Utilities
// ============================================================================

/// Detect language from file extension
///
/// # Examples
///
/// ```
/// use rskim_core::{detect_language, Language};
///
/// assert_eq!(detect_language("ts"), Some(Language::TypeScript));
/// assert_eq!(detect_language("py"), Some(Language::Python));
/// assert_eq!(detect_language("unknown"), None);
/// ```
pub fn detect_language(extension: &str) -> Option<Language> {
    Language::from_extension(extension)
}

/// Detect language from file path
///
/// # Examples
///
/// ```
/// use rskim_core::{detect_language_from_path, Language};
/// use std::path::Path;
///
/// let path = Path::new("src/main.rs");
/// assert_eq!(detect_language_from_path(path), Some(Language::Rust));
/// ```
pub fn detect_language_from_path(path: &std::path::Path) -> Option<Language> {
    Language::from_path(path)
}

// ============================================================================
// Version Information
// ============================================================================

/// Get library version
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Get list of supported languages
pub fn supported_languages() -> &'static [Language] {
    &[
        Language::TypeScript,
        Language::JavaScript,
        Language::Python,
        Language::Rust,
        Language::Go,
        Language::Java,
    ]
}

// ============================================================================
// Module Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version() {
        assert!(!version().is_empty());
    }

    #[test]
    fn test_supported_languages() {
        assert_eq!(supported_languages().len(), 6);
    }

    #[test]
    fn test_detect_language() {
        assert_eq!(detect_language("ts"), Some(Language::TypeScript));
        assert_eq!(detect_language("unknown"), None);
    }

    // NOTE: Actual transformation tests require implementation
    // These are placeholders for schema validation
}
