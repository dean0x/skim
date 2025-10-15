//! Core type definitions for Skim
//!
//! ARCHITECTURE: This module defines ALL types used across the library.
//! Design principle: Type-first development with explicit error handling.

use std::path::{Path, PathBuf};
use thiserror::Error;

// ============================================================================
// Language Support
// ============================================================================

/// Supported programming languages
///
/// ARCHITECTURE: Adding a new language requires:
/// 1. Add variant here
/// 2. Add tree-sitter grammar to Cargo.toml
/// 3. Implement `to_tree_sitter()` mapping
/// 4. Add file extension in `from_extension()`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    TypeScript,
    JavaScript,
    Python,
    Rust,
    Go,
    Java,
}

impl Language {
    /// Detect language from file extension
    ///
    /// # Examples
    /// ```
    /// use std::path::Path;
    /// use skim_core::Language;
    ///
    /// assert_eq!(Language::from_extension("ts"), Some(Language::TypeScript));
    /// assert_eq!(Language::from_extension("py"), Some(Language::Python));
    /// assert_eq!(Language::from_extension("unknown"), None);
    /// ```
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "ts" | "tsx" => Some(Self::TypeScript),
            "js" | "jsx" => Some(Self::JavaScript),
            "py" | "pyi" => Some(Self::Python),
            "rs" => Some(Self::Rust),
            "go" => Some(Self::Go),
            "java" => Some(Self::Java),
            _ => None,
        }
    }

    /// Detect language from file path
    ///
    /// # Security
    /// Rejects paths with parent directory traversal components (`..`)
    /// to prevent path traversal attacks in future caching features.
    /// Absolute paths are allowed.
    pub fn from_path(path: &Path) -> Option<Self> {
        use std::path::Component;

        // SECURITY: Reject paths with parent directory traversal
        // Allow absolute paths (RootDir is fine), but reject .. (ParentDir)
        for component in path.components() {
            if matches!(component, Component::ParentDir) {
                return None;
            }
        }

        path.extension()
            .and_then(|ext| ext.to_str())
            .and_then(Self::from_extension)
    }

    /// Get language name for display
    pub fn name(self) -> &'static str {
        match self {
            Self::TypeScript => "TypeScript",
            Self::JavaScript => "JavaScript",
            Self::Python => "Python",
            Self::Rust => "Rust",
            Self::Go => "Go",
            Self::Java => "Java",
        }
    }

    /// Convert to tree-sitter Language
    ///
    /// ARCHITECTURE: This is the ONLY place where tree-sitter grammars are loaded.
    /// Pattern: Lazy loading per language (don't load all grammars upfront).
    pub fn to_tree_sitter(self) -> tree_sitter::Language {
        match self {
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::Java => tree_sitter_java::LANGUAGE.into(),
        }
    }
}

// ============================================================================
// Transformation Modes
// ============================================================================

/// Output transformation mode
///
/// ARCHITECTURE: Modes define what to keep/remove from source code.
/// Each mode has different token reduction characteristics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Keep structure only - strip all implementation bodies
    ///
    /// Token reduction: ~70-80%
    ///
    /// Keeps:
    /// - Function/method signatures
    /// - Class declarations
    /// - Type definitions
    /// - Imports/exports
    ///
    /// Removes:
    /// - Function bodies (replaced with `/* ... */`)
    /// - Implementation details
    Structure,

    /// Keep only function/method signatures
    ///
    /// Token reduction: ~85-92%
    ///
    /// More aggressive than Structure mode.
    /// Keeps ONLY callable signatures, removes everything else.
    Signatures,

    /// Keep only type definitions
    ///
    /// Token reduction: ~90-95%
    ///
    /// Keeps:
    /// - Type aliases
    /// - Interface declarations
    /// - Enum definitions
    ///
    /// Removes:
    /// - All implementation code
    /// - Function bodies
    /// - Class implementations
    Types,

    /// No transformation - return original source
    ///
    /// Token reduction: 0%
    ///
    /// Useful for testing and comparing with other modes.
    Full,
}

impl Mode {
    /// Parse mode from string (for CLI/API)
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "structure" => Some(Self::Structure),
            "signatures" => Some(Self::Signatures),
            "types" => Some(Self::Types),
            "full" => Some(Self::Full),
            _ => None,
        }
    }

    /// Get mode name for display
    pub fn name(self) -> &'static str {
        match self {
            Self::Structure => "structure",
            Self::Signatures => "signatures",
            Self::Types => "types",
            Self::Full => "full",
        }
    }
}

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for transformation
///
/// ARCHITECTURE: This is injected into transform functions (no global state).
#[derive(Debug, Clone)]
pub struct TransformConfig {
    /// Transformation mode
    pub mode: Mode,

    /// Whether to preserve structural comments
    ///
    /// If true, keeps comments that describe structure (e.g., JSDoc, docstrings).
    /// If false, strips all comments.
    pub preserve_comments: bool,

    /// Whether to use caching (Phase 3)
    ///
    /// If true, transformed results are cached based on file mtime.
    pub cache_enabled: bool,
}

impl Default for TransformConfig {
    fn default() -> Self {
        Self {
            mode: Mode::Structure,
            preserve_comments: true,
            cache_enabled: false,
        }
    }
}

impl TransformConfig {
    /// Create config with specific mode
    pub fn with_mode(mode: Mode) -> Self {
        Self {
            mode,
            ..Default::default()
        }
    }

    /// Builder: Set comment preservation
    pub fn preserve_comments(mut self, preserve: bool) -> Self {
        self.preserve_comments = preserve;
        self
    }

    /// Builder: Enable caching
    pub fn with_cache(mut self, enabled: bool) -> Self {
        self.cache_enabled = enabled;
        self
    }
}

// ============================================================================
// Output Types
// ============================================================================

/// Result of transformation with optional metadata
///
/// ARCHITECTURE: Separate struct for future extensibility (token counts, timing, etc.)
#[derive(Debug, Clone)]
pub struct TransformResult {
    /// Transformed source code
    pub content: String,

    /// Original token count (optional, Phase 3)
    pub original_tokens: Option<usize>,

    /// Transformed token count (optional, Phase 3)
    pub transformed_tokens: Option<usize>,

    /// Time taken to transform in milliseconds (optional, for debugging)
    pub duration_ms: Option<u64>,
}

impl TransformResult {
    /// Create result with just content
    pub fn new(content: String) -> Self {
        Self {
            content,
            original_tokens: None,
            transformed_tokens: None,
            duration_ms: None,
        }
    }

    /// Get token reduction percentage (if counts available)
    pub fn reduction_percentage(&self) -> Option<f32> {
        match (self.original_tokens, self.transformed_tokens) {
            (Some(orig), Some(trans)) if orig > 0 => {
                Some(((orig - trans) as f32 / orig as f32) * 100.0)
            }
            _ => None,
        }
    }
}

// ============================================================================
// Error Types
// ============================================================================

/// Error types for Skim operations
///
/// ARCHITECTURE: Using thiserror for ergonomic error handling.
/// All library functions return Result<T, SkimError>.
/// NO panics allowed in library code (enforced by clippy lints).
#[derive(Debug, Error)]
pub enum SkimError {
    /// Language could not be detected from file path
    #[error("Unsupported language for file: {0}")]
    UnsupportedLanguage(PathBuf),

    /// tree-sitter failed to parse source code
    #[error("Failed to parse source code: {0}")]
    ParseError(String),

    /// tree-sitter language loading error
    #[error("Tree-sitter language error: {0}")]
    TreeSitterError(#[from] tree_sitter::LanguageError),

    /// File I/O error (NOTE: Should only occur in CLI, not core)
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Invalid configuration
    #[error("Invalid configuration: {0}")]
    ConfigError(String),

    /// Cache error (Phase 3)
    #[error("Cache error: {0}")]
    CacheError(String),

    /// UTF-8 conversion error
    #[error("UTF-8 error: {0}")]
    Utf8Error(#[from] std::str::Utf8Error),
}

/// Result type alias for Skim operations
///
/// ARCHITECTURE: Use this instead of std::result::Result throughout the library.
pub type Result<T> = std::result::Result<T, SkimError>;

// ============================================================================
// Cache Types (Phase 3)
// ============================================================================

/// Cache key for transformed results
///
/// ARCHITECTURE: Cache invalidation based on:
/// - File path
/// - File modification time (mtime)
/// - Transformation mode
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    /// File path (normalized)
    pub path: PathBuf,

    /// File modification time (seconds since Unix epoch)
    pub mtime: u64,

    /// Transformation mode
    pub mode: Mode,
}

impl CacheKey {
    /// Create cache key from file metadata
    pub fn new(path: PathBuf, mtime: u64, mode: Mode) -> Self {
        Self { path, mtime, mode }
    }

    /// Compute cache key hash for storage
    ///
    /// Uses Blake3 for fast, collision-resistant hashing (Phase 3).
    pub fn hash_key(&self) -> String {
        todo!("Implement Blake3 hashing in Phase 3")
    }
}

// ============================================================================
// Parser Types
// ============================================================================

/// Wrapper around tree-sitter Parser with language context
///
/// ARCHITECTURE: Parser is injected, not global.
/// Each Parser instance is bound to a specific language.
pub struct Parser {
    language: Language,
    tree_sitter_parser: tree_sitter::Parser,
}

impl Parser {
    /// Create parser for specific language
    ///
    /// # Errors
    /// Returns `SkimError::TreeSitterError` if grammar fails to load.
    pub fn new(language: Language) -> Result<Self> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language.to_tree_sitter())?;

        Ok(Self {
            language,
            tree_sitter_parser: parser,
        })
    }

    /// Parse source code into AST
    ///
    /// ARCHITECTURE: Returns tree-sitter Tree, not custom AST.
    /// Transformation layer operates directly on tree-sitter nodes.
    ///
    /// # Errors
    /// Returns `SkimError::ParseError` if parsing fails.
    pub fn parse(&mut self, source: &str) -> Result<tree_sitter::Tree> {
        self.tree_sitter_parser
            .parse(source, None)
            .ok_or_else(|| SkimError::ParseError(
                format!("Failed to parse {} source", self.language.name())
            ))
    }

    /// Get language for this parser
    pub fn language(&self) -> Language {
        self.language
    }
}

// ============================================================================
// Type Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_from_extension() {
        assert_eq!(Language::from_extension("ts"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("tsx"), Some(Language::TypeScript));
        assert_eq!(Language::from_extension("py"), Some(Language::Python));
        assert_eq!(Language::from_extension("unknown"), None);
    }

    #[test]
    fn test_language_from_path() {
        assert_eq!(
            Language::from_path(Path::new("src/main.rs")),
            Some(Language::Rust)
        );
        assert_eq!(
            Language::from_path(Path::new("test.py")),
            Some(Language::Python)
        );
        assert_eq!(Language::from_path(Path::new("no_extension")), None);
    }

    #[test]
    fn test_mode_from_str() {
        assert_eq!(Mode::from_str("structure"), Some(Mode::Structure));
        assert_eq!(Mode::from_str("STRUCTURE"), Some(Mode::Structure));
        assert_eq!(Mode::from_str("invalid"), None);
    }

    #[test]
    fn test_transform_config_builder() {
        let config = TransformConfig::with_mode(Mode::Signatures)
            .preserve_comments(false)
            .with_cache(true);

        assert_eq!(config.mode, Mode::Signatures);
        assert_eq!(config.preserve_comments, false);
        assert_eq!(config.cache_enabled, true);
    }

    #[test]
    fn test_transform_result_reduction() {
        let mut result = TransformResult::new("transformed".to_string());
        result.original_tokens = Some(1000);
        result.transformed_tokens = Some(200);

        assert_eq!(result.reduction_percentage(), Some(80.0));
    }
}
