//! The syntactic field of a source byte range.
//!
//! [`SearchField`] is what the classifier ([`super::classifier`]) says a byte
//! range of a source file is: a comment, a string, code or nothing in
//! particular. The role rules of the search engine read the comment and string
//! fields from it ([`super::roles::Labels::role`]) and nothing else.
//!
//! The variants have no numeric value. Nothing stores them.

/// Which syntactic region of a source file a byte range belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum SearchField {
    /// Code that can sit inside a string or a comment: a name, a function
    /// body, a declaration. The role rules do not read it. It exists so that
    /// the innermost range wins: a name in the `${…}` of a template string is
    /// not part of the string.
    Code,
    /// Comments (line and block).
    Comment,
    /// String literals.
    StringLiteral,
    /// Unclassified content not matching any of the above.
    Other,
}
