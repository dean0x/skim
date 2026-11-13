//! The syntactic field of a source byte range.
//!
//! [`SearchField`] is what the classifier ([`super::classifier`]) says a byte
//! range of a source file is: a type or function definition, a code identifier,
//! an import, a comment, a string or nothing in particular. The role rules of
//! the search engine read the comment and string fields from it.
//!
//! The variants have no numeric value. Nothing stores them.

/// Which structural region of a source file a byte range belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum SearchField {
    /// Type definitions: structs, enums, interfaces, type aliases.
    TypeDefinition,
    /// Function and method signatures (declaration lines only, not body).
    FunctionSignature,
    /// Symbol names: variable names, identifiers, labels.
    SymbolName,
    /// Import and export declarations.
    ImportExport,
    /// Function and method bodies (implementation, excluding signature).
    FunctionBody,
    /// Comments (line and block).
    Comment,
    /// String literals.
    StringLiteral,
    /// Unclassified content not matching any of the above.
    Other,
}
