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
//! # API Stability
//!
//! As of v1.0.0, all publicly exported types and functions are considered stable.
//! Breaking changes will follow semver (major version bump).
//!
//! # Design Principles
//!
//! 1. **Zero-copy where possible** - Use `&str` slices, avoid allocations
//! 2. **Result types everywhere** - NO panics (enforced by clippy)
//! 3. **Dependency injection** - NO global state
//! 4. **Type-first** - Complete type schema before implementation

// Public API — stable as of v1.0.0
pub use types::{Language, Mode, Parser, Result, SkimError, TransformConfig, TransformResult};

mod parser;
mod transform;
mod types;

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
/// * `mode` - Transformation mode (Structure, Signatures, Types, Full, Minimal, Pseudo)
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
pub fn transform(source: &str, language: Language, mode: Mode) -> Result<String> {
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
    // ARCHITECTURE: Language encapsulates parsing strategy (tree-sitter vs serde_json)
    // This eliminates special-case conditionals - each language handles its own parsing
    language.transform_source(source, config)
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
pub fn transform_auto(source: &str, path: &std::path::Path, mode: Mode) -> Result<String> {
    let language = Language::from_path(path)
        .ok_or_else(|| SkimError::UnsupportedLanguage(path.to_path_buf()))?;

    transform(source, language, mode)
}

/// Transform source code with automatic language detection and custom configuration
///
/// Convenience function that detects language from file extension and applies
/// the provided configuration. Useful for applying max_lines truncation with
/// auto-detected language.
///
/// # Arguments
///
/// * `source` - Source code as string slice
/// * `path` - File path for language detection (NOT read from disk)
/// * `config` - Full transformation configuration
///
/// # Errors
///
/// - `SkimError::UnsupportedLanguage` - Could not detect language from path
/// - All errors from `transform_with_config()`
///
/// # Examples
///
/// ```no_run
/// use rskim_core::{transform_auto_with_config, Mode, TransformConfig};
/// use std::path::Path;
///
/// let config = TransformConfig::with_mode(Mode::Structure)
///     .with_max_lines(50);
///
/// let source = "def hello(): pass";
/// let path = Path::new("script.py");
/// let result = transform_auto_with_config(source, path, &config)?;
/// # Ok::<(), rskim_core::SkimError>(())
/// ```
pub fn transform_auto_with_config(
    source: &str,
    path: &std::path::Path,
    config: &TransformConfig,
) -> Result<String> {
    let language = Language::from_path(path)
        .ok_or_else(|| SkimError::UnsupportedLanguage(path.to_path_buf()))?;

    transform_with_config(source, language, config)
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
pub fn transform_detailed(source: &str, language: Language, mode: Mode) -> Result<TransformResult> {
    let start = std::time::Instant::now();

    let content = transform(source, language, mode)?;

    let duration_ms = start.elapsed().as_millis() as u64;

    Ok(TransformResult {
        content,
        original_tokens: None, // Token counting is performed at the CLI layer (see rskim/src/tokens.rs)
        transformed_tokens: None, // Token counting is performed at the CLI layer (see rskim/src/tokens.rs)
        duration_ms: Some(duration_ms),
    })
}

// ============================================================================
// Token Budget Truncation
// ============================================================================

/// Truncate transformed output to fit within a token budget
///
/// Uses binary search to find the maximum number of lines that fit
/// within the budget, then appends a language-appropriate omission marker.
/// If the text already fits, it is returned unchanged.
///
/// # Arguments
/// * `text` - Previously transformed output to truncate
/// * `language` - Language for comment syntax in omission markers
/// * `token_budget` - Maximum number of tokens allowed
/// * `count_tokens` - Closure that counts tokens in a string slice
/// * `known_token_count` - Pre-computed token count of `text`, if available.
///   When `Some(count)`, skips the initial full-text tokenization.
///   Pass `None` when the count is unknown.
///
/// # Returns
/// Text fitting within the token budget, with omission marker if truncated.
/// If `token_budget` is 0 or smaller than the omission marker itself (~5-7
/// tokens), an empty string is returned rather than violating the budget
/// invariant. Callers should validate the budget upstream or handle the
/// empty-string edge case.
///
/// # Examples
///
/// ```
/// use rskim_core::{truncate_to_token_budget, Language};
///
/// let output = "line 1\nline 2\nline 3\nline 4\nline 5";
/// let word_count = |s: &str| -> usize { s.split_whitespace().count() };
/// let truncated = truncate_to_token_budget(output, Language::TypeScript, 5, word_count, None)?;
/// # Ok::<(), rskim_core::SkimError>(())
/// ```
pub fn truncate_to_token_budget<F>(
    text: &str,
    language: Language,
    token_budget: usize,
    count_tokens: F,
    known_token_count: Option<usize>,
) -> Result<String>
where
    F: Fn(&str) -> usize,
{
    transform::truncate::truncate_to_token_budget(
        text,
        language,
        token_budget,
        count_tokens,
        known_token_count,
    )
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
        Language::Markdown,
        Language::Json,
        Language::Yaml,
        Language::C,
        Language::Cpp,
        Language::Toml,
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
        assert_eq!(supported_languages().len(), 12);
        assert!(supported_languages().contains(&Language::Markdown));
        assert!(supported_languages().contains(&Language::Json));
        assert!(supported_languages().contains(&Language::Yaml));
    }

    #[test]
    fn test_detect_language() {
        assert_eq!(detect_language("ts"), Some(Language::TypeScript));
        assert_eq!(detect_language("unknown"), None);
    }

    // NOTE: Actual transformation tests require implementation
    // These are placeholders for schema validation
}
