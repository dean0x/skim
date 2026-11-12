//! The declaration rule: which syntax nodes declare a name, and what holds a byte.
//!
//! # Design
//!
//! One walk over a tree-sitter tree turns declaring nodes into [`Declaration`]
//! values. Rust, TypeScript, TSX, JavaScript, Python and Go have an explicit
//! kind list. Every other tree-sitter language uses one generic rule: a node
//! of structural priority 4 or 5 (functions and type definitions) or priority 2
//! (containers) that has a `name` field. A language whose declaring nodes carry
//! no `name` field (for example the C family, whose function names sit under a
//! `declarator` field) yields no declaration for those nodes.
//!
//! The walk uses [`AstWalkIter`] with the default caps. [`DeclScan::truncated`]
//! tells the caller that a cap cut the walk, so a missing name may be missing
//! because the walk never reached it.
//!
//! This module reads no files and keeps no state.

use crate::ast_walk::{AstWalkConfig, AstWalkIter};
use crate::{Language, node_kind_priority};
use std::ops::Range;
use tree_sitter::{Node, Tree};

/// How many wrapper nodes (references, pointers, generics, paths) are peeled
/// to reach the name of a receiver or impl type.
const MAX_TYPE_UNWRAP: usize = 8;

/// Kinds that declare a name but are neither a function nor a container.
/// [`enclosing_declaration`] skips them.
const LEAF_KINDS: &[&str] = &[
    "const_item",
    "static_item",
    "type_item",
    "field_declaration",
    "enum_variant",
    "type_alias_declaration",
    "const_spec",
    "var_spec",
];

/// One declared name and where it sits in the source.
///
/// Declarations come out of [`declarations`] in source order, so an outer
/// declaration comes before the declarations nested in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    /// The declared name, as written in the source.
    pub name: String,
    /// The tree-sitter node kind that declares it, for example `function_item`.
    pub kind: &'static str,
    /// Byte range of the name in the source.
    pub name_bytes: Range<usize>,
    /// 0-based row of the first line of the declaration.
    pub first_row: usize,
    /// 0-based row of the last line of the declaration.
    pub last_row: usize,
    /// The name of the container this declaration is a member of: the impl
    /// type, trait, module, struct, enum, class or interface that holds it.
    /// `None` at the top level and for declarations nested in a function body.
    pub container: Option<String>,
    /// Byte range of the whole declaration. [`enclosing_declaration`] uses it
    /// to decide whether a byte lies inside.
    pub byte_range: Range<usize>,
}

/// The result of one declaration walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclScan {
    /// The declarations found, in source order.
    pub decls: Vec<Declaration>,
    /// `true` when the walk stopped at the depth cap or the node cap. A
    /// declaration past the cap is absent from `decls`.
    pub truncated: bool,
}

/// Collect the declarations of `tree`, which was parsed from `source`.
///
/// A declaration whose name range lies outside `source` is dropped, so a
/// source that is not the one the tree came from gives fewer declarations
/// rather than a wrong name.
///
/// # Examples
///
/// ```
/// use rskim_core::{Language, Parser, declarations};
///
/// let source = "struct Point { x: i32 }\nfn norm() {}\n";
/// let mut parser = Parser::new(Language::Rust)?;
/// let tree = parser.parse(source)?;
/// let scan = declarations(&tree, source, Language::Rust);
///
/// let names: Vec<&str> = scan.decls.iter().map(|d| d.name.as_str()).collect();
/// assert_eq!(names, ["Point", "x", "norm"]);
/// assert!(!scan.truncated);
/// # Ok::<(), rskim_core::SkimError>(())
/// ```
#[must_use]
pub fn declarations(tree: &Tree, source: &str, lang: Language) -> DeclScan {
    let cap = AstWalkConfig::default();
    // Walk one node past the node cap. The first node past it proves the cap
    // cut the walk; a tree of exactly `max_nodes` nodes is complete.
    let probe = AstWalkConfig {
        max_nodes: cap.max_nodes.saturating_add(1),
        ..cap
    };
    let mut decls = Vec::new();
    let mut truncated = false;

    for (index, item) in AstWalkIter::new(tree.walk(), probe).enumerate() {
        if index >= cap.max_nodes as usize {
            truncated = true;
            break;
        }
        // The iterator skips the children of a node at the last allowed depth.
        if item.depth.saturating_add(1) >= cap.max_depth && item.node.child_count() > 0 {
            truncated = true;
        }
        if let Some(decl) = declaration_at(item.node, source, lang) {
            decls.push(decl);
        }
    }

    DeclScan { decls, truncated }
}

/// The innermost function or container that holds `byte`.
///
/// `decls` must be in the order [`declarations`] returns. A declaration that
/// is neither a function nor a container (a constant, a field, an enum
/// variant, a type alias) never holds a byte: the function or container around
/// it does. Returns `None` for a byte outside every such declaration.
#[must_use]
pub fn enclosing_declaration(decls: &[Declaration], byte: usize) -> Option<&Declaration> {
    // Source order: no declaration that starts after `byte` can hold it.
    let candidates = decls.get(..decls.partition_point(|d| d.byte_range.start <= byte))?;
    candidates
        .iter()
        .rev()
        .find(|d| !LEAF_KINDS.contains(&d.kind) && d.byte_range.contains(&byte))
}

fn declaration_at(node: Node<'_>, source: &str, lang: Language) -> Option<Declaration> {
    let kind = node.kind();
    if !is_declaration_kind(lang, kind) {
        return None;
    }
    let name_node = name_node(node)?;
    let name_bytes = name_node.byte_range();
    let name = source.get(name_bytes.clone()).filter(|n| !n.is_empty())?;

    let container = match placement(node, source, lang) {
        Placement::Member(container) => container,
        // A variable inside a function body is a local, not a declaration.
        Placement::Nested if kind == "variable_declarator" => return None,
        Placement::Nested => None,
    };

    let start = node.start_position();
    let end = node.end_position();
    // A node that ends after its trailing newline ends at column 0 of the
    // next row; that row holds none of the declaration.
    let last_row = if end.column == 0 && end.row > start.row {
        end.row - 1
    } else {
        end.row
    };

    Some(Declaration {
        name: name.to_owned(),
        kind,
        name_bytes,
        first_row: start.row,
        last_row,
        container,
        byte_range: node.byte_range(),
    })
}

fn is_declaration_kind(lang: Language, kind: &str) -> bool {
    match lang {
        Language::Rust => matches!(
            kind,
            "function_item"
                | "function_signature_item"
                | "struct_item"
                | "enum_item"
                | "union_item"
                | "trait_item"
                | "type_item"
                | "const_item"
                | "static_item"
                | "mod_item"
                | "macro_definition"
                | "enum_variant"
                | "field_declaration"
        ),
        Language::TypeScript | Language::JavaScript => matches!(
            kind,
            "function_declaration"
                | "generator_function_declaration"
                | "function_signature"
                | "class_declaration"
                | "abstract_class_declaration"
                | "interface_declaration"
                | "type_alias_declaration"
                | "enum_declaration"
                | "method_definition"
                | "variable_declarator"
        ),
        Language::Python => matches!(kind, "function_definition" | "class_definition"),
        Language::Go => matches!(
            kind,
            "function_declaration" | "method_declaration" | "type_spec" | "const_spec" | "var_spec"
        ),
        _ => matches!(node_kind_priority(kind), 2 | 4 | 5),
    }
}

/// The node that spells the declared name, if the declaration has a usable one.
fn name_node(node: Node<'_>) -> Option<Node<'_>> {
    let name = node.child_by_field_name("name")?;
    if name.is_missing() {
        return None;
    }
    // A destructuring pattern (`const { a, b } = x`) names no single thing.
    if node.kind() == "variable_declarator" && name.kind() != "identifier" {
        return None;
    }
    Some(name)
}

/// Where a declaration sits relative to the code around it.
enum Placement {
    /// Directly in a container (or at the top level, with `None`).
    Member(Option<String>),
    /// Inside a function body.
    Nested,
}

/// What an ancestor node means for the declarations below it.
enum Role {
    Container,
    Scope,
    Transparent,
}

fn placement(node: Node<'_>, source: &str, lang: Language) -> Placement {
    // Go declares the container of a method on the method itself: its
    // receiver type. Go has no other container.
    if lang == Language::Go {
        return Placement::Member(go_receiver_type(node, source));
    }
    let max_hops = AstWalkConfig::DEFAULT_MAX_DEPTH as usize;
    for ancestor in std::iter::successors(node.parent(), |n| n.parent()).take(max_hops) {
        match ancestor_role(lang, ancestor.kind()) {
            Role::Container => return Placement::Member(container_name(ancestor, source)),
            Role::Scope => return Placement::Nested,
            Role::Transparent => {}
        }
    }
    Placement::Member(None)
}

fn ancestor_role(lang: Language, kind: &str) -> Role {
    let (is_container, is_scope) = match lang {
        Language::Rust => (
            matches!(
                kind,
                "impl_item"
                    | "trait_item"
                    | "mod_item"
                    | "struct_item"
                    | "enum_item"
                    | "union_item"
                    | "enum_variant"
            ),
            kind == "block",
        ),
        Language::TypeScript | Language::JavaScript => (
            matches!(
                kind,
                "class_declaration"
                    | "abstract_class_declaration"
                    | "interface_declaration"
                    | "enum_declaration"
            ),
            kind == "statement_block",
        ),
        Language::Python => (kind == "class_definition", kind == "function_definition"),
        _ => {
            let priority = node_kind_priority(kind);
            (matches!(priority, 2 | 5), priority == 4)
        }
    };
    if is_container {
        Role::Container
    } else if is_scope {
        Role::Scope
    } else {
        Role::Transparent
    }
}

fn container_name(container: Node<'_>, source: &str) -> Option<String> {
    let name_node = if container.kind() == "impl_item" {
        type_name_node(container.child_by_field_name("type")?)?
    } else {
        container.child_by_field_name("name")?
    };
    source.get(name_node.byte_range()).map(str::to_owned)
}

fn go_receiver_type(method: Node<'_>, source: &str) -> Option<String> {
    if method.kind() != "method_declaration" {
        return None;
    }
    let receiver = method.child_by_field_name("receiver")?;
    let receiver_type = receiver.named_child(0)?.child_by_field_name("type")?;
    let name_node = type_name_node(receiver_type)?;
    source.get(name_node.byte_range()).map(str::to_owned)
}

/// The identifier of a type, without references, pointers, generic arguments
/// or path prefixes: `&a::b::Foo<T>` gives `Foo`. Other types have no name.
fn type_name_node(mut ty: Node<'_>) -> Option<Node<'_>> {
    for _ in 0..MAX_TYPE_UNWRAP {
        ty = match ty.kind() {
            "type_identifier" => return Some(ty),
            "generic_type" | "reference_type" => ty.child_by_field_name("type")?,
            "scoped_type_identifier" => ty.child_by_field_name("name")?,
            "pointer_type" => ty.named_child(0)?,
            _ => return None,
        };
    }
    None
}
