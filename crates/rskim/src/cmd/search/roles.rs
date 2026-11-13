//! The role of a hit: what a matching line says about the name it holds.
//!
//! A hit gets one [`Role`]. The rules read three things: the declaration rule
//! of `rskim-core` (the byte range of a declared name), the syntax tree (the
//! callee position of a call, the import node above an identifier) and the
//! classifier (comment and string ranges). One file is labelled once: one
//! bounded parse serves all three.
//!
//! # Rule order for one match
//!
//! 1. `def` when the match range equals the name range of a declaration.
//! 2. `call` when the identifier is in a callee position (see [`call_kinds`]).
//! 3. `comment` or `string` from the classifier field at the match start.
//! 4. `import` when an ancestor of the match is an import node.
//! 5. `ref` for any other code.
//!
//! A hit in a file with no grammar, and a hit in Markdown prose, is `text`.
//! The classifier marks Markdown prose as a `Comment` field to rank it, and
//! [`Labels::new`] maps that field to `Other`, because a Markdown file has no
//! comments. A hit in a data format outside a comment or a string is `text`;
//! a data format keeps its real comment and string roles.
//!
//! A hit with an identifier character before or after it, in an identifier
//! query, is `partial`; it is decided from the line and never needs a parse. A
//! file that could not be labelled gives `unlabelled`. A line takes the best
//! role among its matches, in the tier order of [`Role`].

use std::collections::HashMap;
use std::ops::Range;
use std::time::Duration;

use rskim_core::{Declaration, Language, Parser, declarations, enclosing_declaration};
use tree_sitter::{Node, Tree};

use super::classifier::{classify_data_format, classify_tree};
use super::field::SearchField;

/// How many wrapper nodes (a scoped name, a member access) are climbed from an
/// identifier to a call node.
const MAX_CALLEE_HOPS: usize = 8;

/// How many ancestors are searched for an import node.
const MAX_IMPORT_HOPS: usize = 256;

// ============================================================================
// Roles
// ============================================================================

/// The role of a hit. The variants are in tier order: a lower tier is a better
/// role, and a line takes the lowest tier among its matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum Role {
    /// A declared name.
    Def,
    /// An identifier in callee position.
    Call,
    /// Any other code.
    Ref,
    /// The file could not be labelled.
    Unlabelled,
    /// An identifier below an import node.
    Import,
    /// Inside a string literal.
    StringLit,
    /// Inside a comment.
    Comment,
    /// A file with no grammar, or prose and data outside a comment or string.
    Text,
    /// Part of a longer identifier.
    Partial,
}

impl Role {
    /// Every role, in tier order.
    pub(super) const ALL: [Role; 9] = [
        Role::Def,
        Role::Call,
        Role::Ref,
        Role::Unlabelled,
        Role::Import,
        Role::StringLit,
        Role::Comment,
        Role::Text,
        Role::Partial,
    ];

    /// The name of the role in the output.
    pub(super) fn key(self) -> &'static str {
        match self {
            Role::Def => "def",
            Role::Call => "call",
            Role::Ref => "ref",
            Role::Unlabelled => "unlabelled",
            Role::Import => "import",
            Role::StringLit => "string",
            Role::Comment => "comment",
            Role::Text => "text",
            Role::Partial => "partial",
        }
    }

    /// The tier of the role: `0` for `def` up to `8` for `partial`.
    pub(super) fn tier(self) -> u32 {
        self as u32
    }
}

/// How many matching lines had each role.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct RoleCounts([u32; Role::ALL.len()]);

impl RoleCounts {
    /// Count one more line with `role`.
    pub(super) fn add(&mut self, role: Role) {
        let slot = &mut self.0[role as usize];
        *slot = slot.saturating_add(1);
    }

    /// The count of one role.
    pub(super) fn get(&self, role: Role) -> u32 {
        self.0[role as usize]
    }

    /// The count over all roles.
    #[cfg(test)]
    pub(super) fn total(&self) -> u32 {
        self.0.iter().fold(0u32, |sum, n| sum.saturating_add(*n))
    }

    /// The best role with a non-zero count.
    pub(super) fn best(&self) -> Option<Role> {
        Role::ALL.into_iter().find(|r| self.get(*r) > 0)
    }
}

// ============================================================================
// Files that could not be labelled
// ============================================================================

/// Why a matched file has no labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum Unlabelled {
    /// The parse budget (files or bytes) was spent before this file.
    Budget,
    /// The file is above the per-file parse cap.
    ParseCap,
    /// The parse passed its time bound.
    ParseTimeout,
    /// The parser failed.
    ParseFailed,
    /// The file changed between the match read and the label read.
    Changed,
    /// The declaration walk stopped at a node or depth cap, so a name may be
    /// missing.
    WalkCap,
}

impl Unlabelled {
    /// The name of the reason in the output.
    pub(super) fn key(self) -> &'static str {
        match self {
            Unlabelled::Budget => "budget",
            Unlabelled::ParseCap => "parse_cap",
            Unlabelled::ParseTimeout => "parse_timeout",
            Unlabelled::ParseFailed => "parse_failed",
            Unlabelled::Changed => "changed",
            Unlabelled::WalkCap => "walk_cap",
        }
    }
}

// ============================================================================
// Labels of one file
// ============================================================================

/// The declaration a line sits in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Holder {
    pub(super) name: String,
    /// 1-based first and last line of the declaration.
    pub(super) first_line: u32,
    pub(super) last_line: u32,
}

/// What the rules decided for one matching line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LineHit {
    pub(super) role: Role,
    /// Byte offset in the line of the match that gave the role.
    pub(super) column: usize,
    /// The enclosing declaration, `None` at the top level.
    pub(super) holder: Option<Holder>,
    /// For a `def`: the 1-based first and last line of the declaration.
    pub(super) def_lines: Option<(u32, u32)>,
}

/// The labels of one file, or the reason it has none.
pub(super) enum Labelling {
    Labelled(Labels),
    Unlabelled(Unlabelled),
}

/// What the rules read from one labelled file.
pub(super) struct Labels {
    parts: Parts,
}

enum Parts {
    /// No grammar: every hit is `text`.
    Text,
    /// Markdown and the data formats: the classifier fields only.
    Fields(Vec<(Range<usize>, SearchField)>),
    /// A code file: syntax tree, declarations and classifier fields.
    Code {
        lang: Language,
        tree: Tree,
        decls: Vec<Declaration>,
        /// `(start, end)` of each declared name, to its index in `decls`.
        names: HashMap<(usize, usize), usize>,
        fields: Vec<(Range<usize>, SearchField)>,
    },
}

impl Labels {
    /// Labels for a file with no grammar: every hit is `text`.
    pub(super) fn text() -> Self {
        Labels { parts: Parts::Text }
    }

    /// Parse and classify `source`.
    ///
    /// `ext` selects the grammar variant (`tsx`). `time` bounds the parse.
    ///
    /// # Errors
    ///
    /// The [`Unlabelled`] reason: the parse passed `time`, the parser failed,
    /// or the declaration walk was cut by a node or depth cap (a name past the
    /// cap would be missing, so the file gets no labels at all).
    pub(super) fn new(
        source: &str,
        lang: Language,
        ext: &str,
        time: Duration,
    ) -> Result<Self, Unlabelled> {
        if let Some(fields) = classify_data_format(source, lang) {
            return Ok(Labels {
                parts: Parts::Fields(fields),
            });
        }
        let mut parser =
            Parser::new_for_extension(lang, ext).map_err(|_| Unlabelled::ParseFailed)?;
        let tree = match parser.parse_bounded(source, time) {
            Ok(Some(tree)) => tree,
            Ok(None) => return Err(Unlabelled::ParseTimeout),
            Err(_) => return Err(Unlabelled::ParseFailed),
        };
        if lang == Language::Markdown {
            // The classifier marks Markdown prose as `Comment` to rank it. A
            // Markdown file has no comments, so its prose is `text`.
            let fields = classify_tree(&tree, source, lang)
                .into_iter()
                .map(|(range, field)| match field {
                    SearchField::Comment => (range, SearchField::Other),
                    other => (range, other),
                })
                .collect();
            return Ok(Labels {
                parts: Parts::Fields(fields),
            });
        }
        // The declaration walk is bounded by node and depth caps; a file past
        // them is refused before the classifier walks it.
        let scan = declarations(&tree, source, lang);
        if scan.truncated {
            return Err(Unlabelled::WalkCap);
        }
        let fields = classify_tree(&tree, source, lang);
        let mut names = HashMap::new();
        for (index, decl) in scan.decls.iter().enumerate() {
            names
                .entry((decl.name_bytes.start, decl.name_bytes.end))
                .or_insert(index);
        }
        Ok(Labels {
            parts: Parts::Code {
                lang,
                tree,
                decls: scan.decls,
                names,
                fields,
            },
        })
    }

    /// The role of one match, as a byte range of the source, by the rule
    /// order of this module. `partial` is not decided here.
    pub(super) fn role(&self, range: &Range<usize>) -> Role {
        match &self.parts {
            Parts::Text => Role::Text,
            Parts::Fields(fields) => match field_at(fields, range.start) {
                SearchField::Comment => Role::Comment,
                SearchField::StringLiteral => Role::StringLit,
                _ => Role::Text,
            },
            Parts::Code {
                lang,
                tree,
                names,
                fields,
                ..
            } => {
                if names.contains_key(&(range.start, range.end)) {
                    return Role::Def;
                }
                let node = node_at(tree, range.start);
                if node.is_some_and(|n| is_identifier(n) && is_callee(n, *lang)) {
                    return Role::Call;
                }
                match field_at(fields, range.start) {
                    SearchField::Comment => Role::Comment,
                    SearchField::StringLiteral => Role::StringLit,
                    _ if node.is_some_and(|n| is_identifier(n) && below_import(n, *lang)) => {
                        Role::Import
                    }
                    _ => Role::Ref,
                }
            }
        }
    }

    /// The innermost function or container that holds `byte`.
    pub(super) fn holder(&self, byte: usize) -> Option<Holder> {
        let Parts::Code { decls, .. } = &self.parts else {
            return None;
        };
        let decl = enclosing_declaration(decls, byte)?;
        Some(Holder {
            name: decl.name.clone(),
            first_line: row_to_line(decl.first_row),
            last_line: row_to_line(decl.last_row),
        })
    }

    /// The 1-based first and last line of the declaration whose name is
    /// exactly `range`.
    pub(super) fn def_lines(&self, range: &Range<usize>) -> Option<(u32, u32)> {
        let Parts::Code { decls, names, .. } = &self.parts else {
            return None;
        };
        let decl = decls.get(*names.get(&(range.start, range.end))?)?;
        Some((row_to_line(decl.first_row), row_to_line(decl.last_row)))
    }
}

fn row_to_line(row: usize) -> u32 {
    u32::try_from(row.saturating_add(1)).unwrap_or(u32::MAX)
}

impl Labelling {
    /// Decide the role of one matching line.
    ///
    /// `line_start` is the byte offset of `line` in the file. `matches` are
    /// the byte ranges of the matches in `line`. With `identifier_query`, a
    /// match with an identifier character right before or after it is
    /// `partial`. Returns `None` when `matches` is empty.
    pub(super) fn classify_line(
        &self,
        line: &str,
        line_start: usize,
        matches: impl IntoIterator<Item = Range<usize>>,
        identifier_query: bool,
    ) -> Option<LineHit> {
        let mut best: Option<(Role, Range<usize>)> = None;
        for range in matches {
            let role = if identifier_query && is_partial(line, &range) {
                Role::Partial
            } else {
                match self {
                    Labelling::Labelled(labels) => {
                        labels.role(&(line_start + range.start..line_start + range.end))
                    }
                    Labelling::Unlabelled(_) => Role::Unlabelled,
                }
            };
            if best.as_ref().is_none_or(|(b, _)| role < *b) {
                best = Some((role, range));
            }
        }
        let (role, range) = best?;
        let absolute = line_start + range.start..line_start + range.end;
        let (holder, def_lines) = match self {
            Labelling::Labelled(labels) if role != Role::Partial => (
                labels.holder(absolute.start),
                (role == Role::Def)
                    .then(|| labels.def_lines(&absolute))
                    .flatten(),
            ),
            _ => (None, None),
        };
        Some(LineHit {
            role,
            column: range.start,
            holder,
            def_lines,
        })
    }
}

/// Whether the match has an identifier character right before or after it.
pub(super) fn is_partial(line: &str, range: &Range<usize>) -> bool {
    line[..range.start]
        .chars()
        .next_back()
        .is_some_and(is_identifier_char)
        || line[range.end..]
            .chars()
            .next()
            .is_some_and(is_identifier_char)
}

/// A character that can continue an identifier.
pub(super) fn is_identifier_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

// ============================================================================
// Tree rules
// ============================================================================

/// The classifier field at `byte`. The ranges are sorted and contiguous, so a
/// byte outside them (an empty source) has no field.
fn field_at(fields: &[(Range<usize>, SearchField)], byte: usize) -> SearchField {
    let index = fields.partition_point(|(range, _)| range.end <= byte);
    match fields.get(index) {
        Some((range, field)) if range.start <= byte => *field,
        _ => SearchField::Other,
    }
}

/// The leaf node that holds the byte `start`.
fn node_at(tree: &Tree, start: usize) -> Option<Node<'_>> {
    let root = tree.root_node();
    root.descendant_for_byte_range(
        start,
        start.saturating_add(1).min(root.end_byte()).max(start),
    )
}

/// Whether `node` is a name token.
fn is_identifier(node: Node<'_>) -> bool {
    let kind = node.kind();
    kind.ends_with("identifier")
        || matches!(
            kind,
            "variable_name" | "attribute_name" | "constant" | "word"
        )
}

/// The call node kinds of a language, each with the field that holds the
/// callee. A call node without such a field takes its first named child.
fn call_kinds(lang: Language) -> &'static [(&'static str, &'static str)] {
    match lang {
        Language::Rust => &[
            ("call_expression", "function"),
            ("macro_invocation", "macro"),
        ],
        Language::Python => &[("call", "function")],
        Language::Ruby => &[("call", "method")],
        Language::TypeScript | Language::JavaScript => &[
            ("call_expression", "function"),
            ("new_expression", "constructor"),
        ],
        Language::Go | Language::C | Language::Kotlin | Language::Swift => {
            &[("call_expression", "function")]
        }
        Language::Cpp => &[("call_expression", "function"), ("new_expression", "type")],
        Language::Java => &[
            ("method_invocation", "name"),
            ("object_creation_expression", "type"),
        ],
        Language::CSharp => &[
            ("invocation_expression", "function"),
            ("object_creation_expression", "type"),
        ],
        Language::Bash => &[("command", "name")],
        Language::Markdown | Language::Json | Language::Yaml | Language::Toml | Language::Sql => {
            &[]
        }
    }
}

/// Whether the identifier `node` names the callee of a call.
///
/// The name passes up through the wrappers of [`passes_through`] (a scoped
/// path, a member access): the receiver of a member access is not the callee.
fn is_callee(node: Node<'_>, lang: Language) -> bool {
    let calls = call_kinds(lang);
    let mut current = node;
    for _ in 0..MAX_CALLEE_HOPS {
        let Some(parent) = current.parent() else {
            return false;
        };
        if let Some((_, field)) = calls.iter().find(|(kind, _)| *kind == parent.kind()) {
            let callee = parent
                .child_by_field_name(field)
                .or_else(|| parent.named_child(0));
            return callee.is_some_and(|c| c.id() == current.id());
        }
        if !passes_through(parent, current) {
            return false;
        }
        current = parent;
    }
    false
}

/// Whether `child` is the part of `wrapper` that names the thing: the part a
/// call passes through to its name.
fn passes_through(wrapper: Node<'_>, child: Node<'_>) -> bool {
    let is_field = |field: &str| {
        wrapper
            .child_by_field_name(field)
            .is_some_and(|c| c.id() == child.id())
    };
    match wrapper.kind() {
        "scoped_identifier" | "member_access_expression" => is_field("name"),
        "field_expression" | "selector_expression" => is_field("field"),
        "member_expression" => is_field("property"),
        "generic_function" => is_field("function"),
        "attribute" => is_field("attribute"),
        "command_name" | "navigation_suffix" => true,
        "navigation_expression" => wrapper
            .named_child(wrapper.named_child_count().saturating_sub(1))
            .is_some_and(|last| last.id() == child.id()),
        _ => false,
    }
}

/// Whether an ancestor of `node` (or `node`) is an import node of `lang`.
fn below_import(node: Node<'_>, lang: Language) -> bool {
    std::iter::successors(Some(node), |n| n.parent())
        .take(MAX_IMPORT_HOPS)
        .any(|n| is_import_node(n, lang))
}

fn is_import_node(node: Node<'_>, lang: Language) -> bool {
    let kind = node.kind();
    match lang {
        Language::Rust => kind == "use_declaration",
        Language::Python => matches!(
            kind,
            "import_statement" | "import_from_statement" | "future_import_statement"
        ),
        Language::TypeScript | Language::JavaScript => {
            kind == "import_statement"
                || (kind == "export_statement" && node.child_by_field_name("source").is_some())
        }
        Language::Go | Language::Java => kind == "import_declaration",
        Language::CSharp => kind == "using_directive",
        Language::C
        | Language::Cpp
        | Language::Ruby
        | Language::Kotlin
        | Language::Swift
        | Language::Bash
        | Language::Sql
        | Language::Markdown
        | Language::Json
        | Language::Yaml
        | Language::Toml => false,
    }
}

// ============================================================================
// The declaration keyword
// ============================================================================

/// The words that start a declaration in `lang`. A matched line that holds one
/// of them before the match is a likely definition, and its file is parsed
/// first.
pub(super) fn declaration_keywords(lang: Language) -> &'static [&'static str] {
    match lang {
        Language::Rust => &[
            "fn",
            "struct",
            "enum",
            "trait",
            "type",
            "const",
            "static",
            "mod",
            "union",
            "macro_rules",
        ],
        Language::TypeScript | Language::JavaScript => &[
            "function",
            "class",
            "interface",
            "type",
            "enum",
            "const",
            "let",
            "var",
        ],
        Language::Python => &["def", "class"],
        Language::Go => &["func", "type", "const", "var"],
        Language::Java | Language::CSharp | Language::Kotlin | Language::Swift => &[
            "class",
            "interface",
            "enum",
            "struct",
            "record",
            "fun",
            "func",
        ],
        Language::Ruby => &["def", "class", "module"],
        Language::C | Language::Cpp => &["struct", "enum", "class", "typedef"],
        Language::Markdown
        | Language::Json
        | Language::Yaml
        | Language::Toml
        | Language::Sql
        | Language::Bash => &[],
    }
}

/// Whether the text before a match holds one of `keywords` as a whole word.
pub(super) fn has_declaration_keyword(before: &str, keywords: &[&str]) -> bool {
    !keywords.is_empty()
        && before
            .split(|c: char| !is_identifier_char(c))
            .any(|word| keywords.contains(&word))
}

#[cfg(test)]
#[path = "roles_tests.rs"]
mod tests;
