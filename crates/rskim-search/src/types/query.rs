//! Search query types: `SearchQuery`, `TemporalFlags`, and `SearchField`.

use serde::{Deserialize, Serialize};

/// Semantic field within a source file used for field-boosted search scoring.
///
/// Each variant corresponds to a distinct syntactic region of a file.
/// Field weights are defined by [`SearchField::default_boost`] and are applied
/// during BM25F scoring in the lexical search layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SearchField {
    /// Top-level type, interface, struct, class, or enum definition.
    TypeDefinition,
    /// Function or method signature (name + parameters + return type).
    FunctionSignature,
    /// Bare symbol name (identifier without surrounding context).
    SymbolName,
    /// Import or export statement.
    ImportExport,
    /// Function or method body (implementation detail).
    FunctionBody,
    /// Doc comment or regular comment.
    Comment,
    /// String literal value.
    StringLiteral,
}

impl SearchField {
    /// Return the default BM25F boost factor for this field.
    ///
    /// Higher values cause matches in this field to score more strongly.
    pub fn default_boost(self) -> f32 {
        match self {
            Self::TypeDefinition => 5.0,
            Self::FunctionSignature => 4.0,
            Self::SymbolName => 3.5,
            Self::ImportExport => 3.0,
            Self::FunctionBody => 1.0,
            Self::Comment => 0.8,
            Self::StringLiteral => 0.5,
        }
    }

    /// Convert to a compact `u8` for on-disk posting entries.
    ///
    /// Stable mapping — must not change between index format versions.
    pub fn as_u8(self) -> u8 {
        match self {
            Self::TypeDefinition => 0,
            Self::FunctionSignature => 1,
            Self::SymbolName => 2,
            Self::ImportExport => 3,
            Self::FunctionBody => 4,
            Self::Comment => 5,
            Self::StringLiteral => 6,
        }
    }

    /// Reconstruct from `u8`. Returns `None` for unknown values.
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::TypeDefinition),
            1 => Some(Self::FunctionSignature),
            2 => Some(Self::SymbolName),
            3 => Some(Self::ImportExport),
            4 => Some(Self::FunctionBody),
            5 => Some(Self::Comment),
            6 => Some(Self::StringLiteral),
            _ => None,
        }
    }

    /// Return all variants in discriminant order.
    pub fn all() -> &'static [Self] {
        &[
            Self::TypeDefinition,
            Self::FunctionSignature,
            Self::SymbolName,
            Self::ImportExport,
            Self::FunctionBody,
            Self::Comment,
            Self::StringLiteral,
        ]
    }
}

/// Temporal filter flags for query-time filtering by git activity signals.
///
/// All flags default to `false` (disabled). Any combination is valid.
#[derive(Debug, Clone, Default)]
pub struct TemporalFlags {
    /// Include only files with high blast radius (many dependents).
    pub blast_radius: bool,
    /// Include only files with recent commit activity ("hot" files).
    pub hot: bool,
    /// Include only files with no recent changes ("cold" files).
    pub cold: bool,
    /// Include only files with high churn or complexity.
    pub risky: bool,
}

/// Query to execute against the search index.
///
/// Constructed via [`SearchQuery::new`] or the convenience [`SearchQuery::text`],
/// then customised with builder methods.
///
/// # Examples
///
/// ```
/// use rskim_search::SearchQuery;
///
/// let q = SearchQuery::text("parse_file").with_limit(20);
/// ```
#[must_use]
#[derive(Debug, Clone)]
pub struct SearchQuery {
    /// Free-text query string for lexical matching.
    pub text_query: Option<String>,
    /// AST pattern string for structural matching.
    pub ast_pattern: Option<String>,
    /// Temporal filter flags.
    pub temporal_flags: TemporalFlags,
    /// Maximum number of results to return.
    pub limit: usize,
    /// Number of results to skip (pagination offset).
    pub offset: usize,
}

impl SearchQuery {
    /// Create a query with default settings (no text, limit 50, offset 0).
    pub fn new() -> Self {
        Self {
            text_query: None,
            ast_pattern: None,
            temporal_flags: TemporalFlags::default(),
            limit: 50,
            offset: 0,
        }
    }

    /// Convenience constructor: create a query with the given text.
    ///
    /// Equivalent to `SearchQuery::new().with_text(query)`.
    pub fn text(query: &str) -> Self {
        Self::new().with_text(query)
    }

    /// Set the free-text query string.
    pub fn with_text(mut self, text: &str) -> Self {
        self.text_query = Some(text.to_string());
        self
    }

    /// Set the maximum number of results.
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// Set the pagination offset.
    pub fn with_offset(mut self, offset: usize) -> Self {
        self.offset = offset;
        self
    }

    /// Set the AST pattern for structural matching.
    pub fn with_ast_pattern(mut self, pattern: &str) -> Self {
        self.ast_pattern = Some(pattern.to_string());
        self
    }

    /// Set the temporal filter flags.
    pub fn with_temporal_flags(mut self, flags: TemporalFlags) -> Self {
        self.temporal_flags = flags;
        self
    }
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_query_new_defaults() {
        let q = SearchQuery::new();
        assert_eq!(q.text_query, None);
        assert_eq!(q.ast_pattern, None);
        assert_eq!(q.limit, 50);
        assert_eq!(q.offset, 0);
        assert!(!q.temporal_flags.blast_radius);
        assert!(!q.temporal_flags.hot);
        assert!(!q.temporal_flags.cold);
        assert!(!q.temporal_flags.risky);
    }

    #[test]
    fn search_query_text_convenience() {
        let q = SearchQuery::text("parse_file");
        assert_eq!(q.text_query, Some("parse_file".to_string()));
        // All other fields remain at defaults.
        assert_eq!(q.limit, 50);
        assert_eq!(q.offset, 0);
        assert_eq!(q.ast_pattern, None);
    }

    #[test]
    fn temporal_flags_default_all_false() {
        let flags = TemporalFlags::default();
        assert!(!flags.blast_radius);
        assert!(!flags.hot);
        assert!(!flags.cold);
        assert!(!flags.risky);
    }

    #[test]
    fn search_query_builder_chain() {
        let flags = TemporalFlags {
            hot: true,
            ..Default::default()
        };
        let q = SearchQuery::new()
            .with_text("bar")
            .with_limit(10)
            .with_offset(5)
            .with_ast_pattern("fn _()")
            .with_temporal_flags(flags);

        assert_eq!(q.text_query, Some("bar".to_string()));
        assert_eq!(q.limit, 10);
        assert_eq!(q.offset, 5);
        assert_eq!(q.ast_pattern, Some("fn _()".to_string()));
        assert!(q.temporal_flags.hot);
        assert!(!q.temporal_flags.blast_radius);
    }

    #[test]
    fn field_boost_exact_values() {
        // These exact values are the contract for BM25F scoring weights.
        // Changing them is a breaking change to search quality.
        assert!((SearchField::TypeDefinition.default_boost() - 5.0).abs() < f32::EPSILON);
        assert!((SearchField::FunctionSignature.default_boost() - 4.0).abs() < f32::EPSILON);
        assert!((SearchField::SymbolName.default_boost() - 3.5).abs() < f32::EPSILON);
        assert!((SearchField::ImportExport.default_boost() - 3.0).abs() < f32::EPSILON);
        assert!((SearchField::FunctionBody.default_boost() - 1.0).abs() < f32::EPSILON);
        assert!((SearchField::Comment.default_boost() - 0.8).abs() < f32::EPSILON);
        assert!((SearchField::StringLiteral.default_boost() - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn field_boosts_rank_type_definition_highest() {
        // Verify the ordering assumption that scoring relies on.
        assert!(
            SearchField::TypeDefinition.default_boost()
                > SearchField::FunctionSignature.default_boost()
        );
        assert!(
            SearchField::FunctionSignature.default_boost()
                > SearchField::SymbolName.default_boost()
        );
        assert!(
            SearchField::SymbolName.default_boost() > SearchField::ImportExport.default_boost()
        );
        assert!(SearchField::FunctionBody.default_boost() > SearchField::Comment.default_boost());
        assert!(SearchField::Comment.default_boost() > SearchField::StringLiteral.default_boost());
    }
}
