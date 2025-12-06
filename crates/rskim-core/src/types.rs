//! Core type definitions for Skim
//!
//! ARCHITECTURE: This module defines ALL types used across the library.
//! Design principle: Type-first development with explicit error handling.

use std::path::{Path, PathBuf};
use thiserror::Error;

// ============================================================================
// Language Support
// ============================================================================

/// Supported programming languages and markup formats
///
/// ARCHITECTURE: Adding a new language requires:
/// 1. Add variant here
/// 2. Add tree-sitter grammar to Cargo.toml (unless special-cased like JSON)
/// 3. Implement `to_tree_sitter()` mapping (or handle specially like JSON)
/// 4. Add file extension in `from_extension()`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    TypeScript,
    JavaScript,
    Python,
    Rust,
    Go,
    Java,
    Markdown,
    Json,
    Yaml,
}

impl Language {
    /// Detect language from file extension
    ///
    /// # Examples
    /// ```
    /// use std::path::Path;
    /// use rskim_core::Language;
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
            "md" | "markdown" => Some(Self::Markdown),
            "json" => Some(Self::Json),
            "yaml" | "yml" => Some(Self::Yaml),
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
            Self::Markdown => "Markdown",
            Self::Json => "JSON",
            Self::Yaml => "YAML",
        }
    }

    /// Convert to tree-sitter Language
    ///
    /// ARCHITECTURE: This is the ONLY place where tree-sitter grammars are loaded.
    /// Pattern: Lazy loading per language (don't load all grammars upfront).
    ///
    /// # Note on Markdown
    /// tree-sitter-md has two parsers: LANGUAGE (block) and INLINE_LANGUAGE (inline).
    /// We only use LANGUAGE (block parser) since we're extracting headers, not inline formatting.
    ///
    /// # Note on JSON
    /// JSON returns None because it uses serde_json for parsing, not tree-sitter.
    /// JSON transformation is handled separately in the transform layer.
    pub(crate) fn to_tree_sitter(self) -> Option<tree_sitter::Language> {
        match self {
            Self::TypeScript => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
            Self::JavaScript => Some(tree_sitter_javascript::LANGUAGE.into()),
            Self::Python => Some(tree_sitter_python::LANGUAGE.into()),
            Self::Rust => Some(tree_sitter_rust::LANGUAGE.into()),
            Self::Go => Some(tree_sitter_go::LANGUAGE.into()),
            Self::Java => Some(tree_sitter_java::LANGUAGE.into()),
            Self::Markdown => Some(tree_sitter_md::LANGUAGE.into()),
            Self::Json => None, // Uses serde_json, not tree-sitter
            Self::Yaml => None, // Uses serde_yaml_ng, not tree-sitter
        }
    }

    /// Transform source code for this language
    ///
    /// ARCHITECTURE: Encapsulates language-specific parsing strategy.
    /// - JSON: Uses serde_json parser
    /// - All others: Use tree-sitter parser
    ///
    /// This eliminates special-case conditionals in the main transform function.
    ///
    /// # Errors
    /// Returns parsing or transformation errors specific to the language.
    pub(crate) fn transform_source(self, source: &str, config: &TransformConfig) -> Result<String> {
        match self {
            Self::Json => {
                // JSON uses serde_json, ignores transformation modes
                crate::transform::json::transform_json(source)
            }
            Self::Yaml => {
                // YAML uses serde_yaml_ng, ignores transformation modes
                crate::transform::yaml::transform_yaml(source)
            }
            _ => {
                // Tree-sitter based languages
                let mut parser = Parser::new(self)?;
                let tree = parser.parse(source)?;
                crate::transform::transform_tree(source, &tree, self, config)
            }
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
    pub fn parse(s: &str) -> Option<Self> {
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

    /// Whether to use caching
    ///
    /// NOTE: This field is reserved for future library users who want to implement
    /// their own caching. The CLI (rskim binary) implements caching separately
    /// and ignores this field. See: crates/rskim/src/cache.rs
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

    /// Cache error (reserved for future use)
    ///
    /// NOTE: The CLI implements its own caching layer and doesn't use this error.
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
// Cache Types (Reserved for Future Library Users)
// ============================================================================
//
// NOTE: The CLI (rskim binary) has its own caching implementation.
// See: crates/rskim/src/cache.rs
//
// These types are kept here for documentation and potential future use by
// library consumers who want to implement their own caching strategies.
// The core library itself remains pure and I/O-free.

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
    /// Returns `SkimError::ConfigError` for languages that don't use tree-sitter (e.g., JSON).
    pub fn new(language: Language) -> Result<Self> {
        let ts_language = language.to_tree_sitter().ok_or_else(|| {
            SkimError::ConfigError(format!(
                "{} does not use tree-sitter parser",
                language.name()
            ))
        })?;

        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_language)?;

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
        self.tree_sitter_parser.parse(source, None).ok_or_else(|| {
            SkimError::ParseError(format!("Failed to parse {} source", self.language.name()))
        })
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
    fn test_mode_parse() {
        assert_eq!(Mode::parse("structure"), Some(Mode::Structure));
        assert_eq!(Mode::parse("STRUCTURE"), Some(Mode::Structure));
        assert_eq!(Mode::parse("invalid"), None);
    }

    #[test]
    fn test_transform_config_builder() {
        let config = TransformConfig::with_mode(Mode::Signatures)
            .preserve_comments(false)
            .with_cache(true);

        assert_eq!(config.mode, Mode::Signatures);
        assert!(!config.preserve_comments);
        assert!(config.cache_enabled);
    }

    #[test]
    fn test_transform_result_reduction() {
        let mut result = TransformResult::new("transformed".to_string());
        result.original_tokens = Some(1000);
        result.transformed_tokens = Some(200);

        assert_eq!(result.reduction_percentage(), Some(80.0));
    }
}
