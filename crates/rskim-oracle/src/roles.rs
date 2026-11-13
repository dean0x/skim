//! Role oracle: where a name is defined, called and imported, from the
//! grammars alone.
//!
//! [`role_sites`] answers three questions about one name in one file, for the
//! oracle languages (Rust, Python, TypeScript, TSX, JavaScript, Go):
//!
//! - `def`: the lines where the name is declared;
//! - `call`: the lines where the name is in a callee position;
//! - `import`: the lines where the name is an identifier below an import node.
//!
//! [`definition_names`] lists every declared name of a file with its line,
//! from the same definition queries.
//!
//! The role names are those of the scoreboard's hand classification
//! (`SiteRole` in `rskim-bench`'s golden module) and of the search engine's
//! role rules. This crate reads neither: the rules are restated here from the
//! grammars (`node-types.json` of each pinned grammar), so a disagreement
//! between the engine and this oracle shows up as a scoreboard diff
//! (`tests/independence.rs` keeps the crate free of `rskim-*` dependencies).
//!
//! # Queries
//!
//! The grammar knowledge lives in 18 query files,
//! `crates/rskim-oracle/queries/roles/<role>.<lang>.scm`, compiled in with
//! `include_str!`. Line 2 of each is `; Grammar: <crate> <version>`, and a test
//! keeps it equal to the version the workspace `Cargo.lock` resolves (the
//! convention of the structural queries, [`crate::structural`]). `.tsx` is
//! parsed with the TSX grammar.
//!
//! - `definition.<lang>.scm` captures the name node of each declaration kind:
//!   Rust `function_item`, `function_signature_item`, `struct_item`,
//!   `enum_item`, `union_item`, `trait_item`, `type_item`, `const_item`,
//!   `static_item`, `mod_item`, `macro_definition`, `enum_variant`,
//!   `field_declaration`; TypeScript, TSX and JavaScript
//!   `function_declaration`, `generator_function_declaration`,
//!   `function_signature`, `class_declaration`, `abstract_class_declaration`,
//!   `interface_declaration`, `type_alias_declaration`, `enum_declaration`,
//!   `method_definition` (the kinds the grammar has) and `variable_declarator`;
//!   Python `function_definition`, `class_definition`; Go
//!   `function_declaration`, `method_declaration`, `type_spec`, `const_spec`,
//!   `var_spec`. A query cannot say "not inside a block", so the name of a
//!   `variable_declarator` carries the capture `@name.outside_block` and
//!   counts only when no ancestor is a `statement_block`. Every other name is
//!   `@name`.
//! - `call.<lang>.scm` captures `@callee`, the identifier in callee position:
//!   `call_expression.function`, `macro_invocation.macro` (Rust),
//!   `call.function` (Python), `new_expression.constructor` (TypeScript, TSX,
//!   JavaScript), seen through the wrappers that pass the name on
//!   (`scoped_identifier.name`, `field_expression.field`,
//!   `member_expression.property`, `generic_function.function`,
//!   `attribute.attribute`, `selector_expression.field`).
//! - `import.<lang>.scm` captures `@import`, the import node: Rust
//!   `use_declaration`; Python `import_statement` and `import_from_statement`;
//!   TypeScript, TSX and JavaScript `import_statement` and an
//!   `export_statement` that has a `source`; Go `import_declaration`. A query
//!   cannot reach every depth below a node, so the code then visits the
//!   identifier leaves under it (a leaf whose kind is `identifier` or ends in
//!   `_identifier`). The path string of an import is a string, not an
//!   identifier, so it never counts.
//!
//! A name counts where the identifier's text equals it exactly: a longer
//! identifier that contains the name does not, and neither does a comment or a
//! string. The three lists are independent; one line can be in several (a
//! method that calls itself on its declaration line).
//!
//! # Limits
//!
//! The declaration kinds are those above. Not covered: Go `type_alias`,
//! Python `future_import_statement`, a TypeScript private name (`#name`), a
//! destructured binding, and a Go generic call (`f[T](x)`, which the grammar
//! shares with indexing).

use std::sync::LazyLock;

use anyhow::Context;
use tree_sitter::{Node, Query, StreamingIterator, Tree};

use crate::structural::{OracleLang, OracleScratch, first_line};

/// The lines (1-based, sorted, de-duplicated) where one name has each role.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoleSites {
    /// Lines where the name is declared.
    pub defs: Vec<u32>,
    /// Lines where the name is in a callee position.
    pub calls: Vec<u32>,
    /// Lines where the name is an identifier below an import node.
    pub imports: Vec<u32>,
}

/// Where `name` is defined, called and imported in `source`, parsed as `lang`.
///
/// # Errors
///
/// `name` is empty; the parser returns no tree; a query does not compile, or
/// needed more in-progress matches than the oracle's limit (tree-sitter then
/// drops matches, which is a harness error, never a silent miss).
pub fn role_sites(lang: OracleLang, source: &str, name: &str) -> anyhow::Result<RoleSites> {
    anyhow::ensure!(!name.is_empty(), "the role oracle needs a non-empty name");
    let oracle = ORACLE
        .as_ref()
        .map_err(|message| anyhow::anyhow!("the role oracle does not compile: {message}"))?;
    let mut scratch = OracleScratch::new();
    let tree = scratch.parse(lang, source)?;
    let mut lines_of = |role: Role| oracle.lines(role, lang, &mut scratch, &tree, source, name);
    Ok(RoleSites {
        defs: lines_of(Role::Definition)?,
        calls: lines_of(Role::Call)?,
        imports: lines_of(Role::Import)?,
    })
}

/// Every name declared in `source`, parsed as `lang`, with the 1-based line of
/// its declaration: the names whose [`role_sites`] `defs` would list that line.
/// Sorted by `(name, line)` and de-duplicated.
///
/// # Errors
///
/// The parser returns no tree; a query does not compile, or needed more
/// in-progress matches than the oracle's limit.
pub fn definition_names(lang: OracleLang, source: &str) -> anyhow::Result<Vec<(String, u32)>> {
    let oracle = ORACLE
        .as_ref()
        .map_err(|message| anyhow::anyhow!("the role oracle does not compile: {message}"))?;
    let mut scratch = OracleScratch::new();
    let tree = scratch.parse(lang, source)?;
    oracle.definitions(lang, &mut scratch, &tree, source)
}

// ============================================================================
// Registry
// ============================================================================

/// The three roles the oracle answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Definition,
    Call,
    Import,
}

impl Role {
    /// The role's part of a query file name, `<role>.<lang>.scm`.
    fn file_stem(self) -> &'static str {
        match self {
            Role::Definition => "definition",
            Role::Call => "call",
            Role::Import => "import",
        }
    }

    /// What a capture named `capture` means in this role's queries; `None`
    /// for a name the role does not use.
    fn capture(self, capture: &str) -> Option<Capture> {
        match (self, capture) {
            (Role::Definition, "name") => Some(Capture::Name),
            (Role::Definition, "name.outside_block") => Some(Capture::NameOutsideBlock),
            (Role::Call, "callee") => Some(Capture::Callee),
            (Role::Import, "import") => Some(Capture::ImportNode),
            _ => None,
        }
    }

    /// The capture every query of this role must hold.
    fn required_capture(self) -> &'static str {
        match self {
            Role::Definition => "name",
            Role::Call => "callee",
            Role::Import => "import",
        }
    }
}

/// How a captured node becomes a site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capture {
    /// A declaration's name node.
    Name,
    /// A declaration's name node, counted only outside every `statement_block`.
    NameOutsideBlock,
    /// An identifier in callee position.
    Callee,
    /// An import node; the identifier leaves below it are the sites.
    ImportNode,
}

/// One registered role query.
#[derive(Debug, Clone, Copy)]
struct RoleQuery {
    role: Role,
    lang: OracleLang,
    source: &'static str,
}

impl RoleQuery {
    /// `<role>.<lang>.scm`.
    fn file_name(&self) -> String {
        format!("{}.{}.scm", self.role.file_stem(), self.lang)
    }
}

/// Registry row: `(role, OracleLang variant, role file stem, file-name
/// language)`. The text is `include_str!`ed from
/// `queries/roles/<stem>.<file language>.scm`; a unit test checks that the file
/// is the one the row names and that every file is registered.
macro_rules! role_query {
    ($role:ident, $stem:literal, $lang:ident, $file_lang:literal) => {
        RoleQuery {
            role: Role::$role,
            lang: OracleLang::$lang,
            source: include_str!(concat!("../queries/roles/", $stem, ".", $file_lang, ".scm")),
        }
    };
}

/// Every role query, sorted by `(role, file-name language)` as the files are.
const ROLE_QUERIES: &[RoleQuery] = &[
    role_query!(Call, "call", Go, "go"),
    role_query!(Call, "call", JavaScript, "javascript"),
    role_query!(Call, "call", Python, "python"),
    role_query!(Call, "call", Rust, "rust"),
    role_query!(Call, "call", Tsx, "tsx"),
    role_query!(Call, "call", TypeScript, "typescript"),
    role_query!(Definition, "definition", Go, "go"),
    role_query!(Definition, "definition", JavaScript, "javascript"),
    role_query!(Definition, "definition", Python, "python"),
    role_query!(Definition, "definition", Rust, "rust"),
    role_query!(Definition, "definition", Tsx, "tsx"),
    role_query!(Definition, "definition", TypeScript, "typescript"),
    role_query!(Import, "import", Go, "go"),
    role_query!(Import, "import", JavaScript, "javascript"),
    role_query!(Import, "import", Python, "python"),
    role_query!(Import, "import", Rust, "rust"),
    role_query!(Import, "import", Tsx, "tsx"),
    role_query!(Import, "import", TypeScript, "typescript"),
];

/// Node kind a `variable_declarator` must not sit inside to be a definition.
const BLOCK_KIND: &str = "statement_block";

// ============================================================================
// Compiled oracle
// ============================================================================

/// A query with the meaning of each of its captures, by capture index.
struct CompiledRole {
    role: Role,
    lang: OracleLang,
    file_name: String,
    query: Query,
    captures: Vec<Capture>,
}

/// Every role query, compiled once.
struct RoleOracle {
    queries: Vec<CompiledRole>,
}

/// Compiled on first use and shared (a `tree_sitter::Query` is `Send + Sync`).
/// The error is the rendered failure, since `anyhow::Error` is not `Clone`.
static ORACLE: LazyLock<Result<RoleOracle, String>> =
    LazyLock::new(|| RoleOracle::new().map_err(|e| format!("{e:#}")));

impl RoleOracle {
    /// Compile every registered query.
    ///
    /// # Errors
    ///
    /// A query that does not compile against its grammar, holds a capture its
    /// role does not use (a misspelt name would otherwise match nothing,
    /// silently), lacks the capture its role needs, or duplicates another
    /// `(role, language)` pair.
    fn new() -> anyhow::Result<Self> {
        Self::from_queries(ROLE_QUERIES)
    }

    fn from_queries(queries: &[RoleQuery]) -> anyhow::Result<Self> {
        let queries = queries
            .iter()
            .map(compile)
            .collect::<anyhow::Result<Vec<_>>>()?;
        for (i, a) in queries.iter().enumerate() {
            anyhow::ensure!(
                !queries[..i]
                    .iter()
                    .any(|b| b.role == a.role && b.lang == a.lang),
                "role query {} is registered twice",
                a.file_name
            );
        }
        Ok(RoleOracle { queries })
    }

    /// The 1-based lines where `name` has `role` in `tree` (parsed from
    /// `source` as `lang`), sorted and de-duplicated.
    ///
    /// # Errors
    ///
    /// No query for `(role, lang)`; a line that does not fit a `u32`; the
    /// query exceeded the cursor's match limit.
    fn lines(
        &self,
        role: Role,
        lang: OracleLang,
        scratch: &mut OracleScratch,
        tree: &Tree,
        source: &str,
        name: &str,
    ) -> anyhow::Result<Vec<u32>> {
        let mut lines = Vec::new();
        self.visit(role, lang, scratch, tree, source, |kind, node| {
            collect(kind, node, source, name, &mut lines)
        })?;
        lines.sort_unstable();
        lines.dedup();
        Ok(lines)
    }

    /// Every declared name with its 1-based line, sorted and de-duplicated.
    ///
    /// # Errors
    ///
    /// As [`RoleOracle::lines`].
    fn definitions(
        &self,
        lang: OracleLang,
        scratch: &mut OracleScratch,
        tree: &Tree,
        source: &str,
    ) -> anyhow::Result<Vec<(String, u32)>> {
        let mut names = Vec::new();
        self.visit(
            Role::Definition,
            lang,
            scratch,
            tree,
            source,
            |kind, node| {
                let counts = match kind {
                    Capture::Name => true,
                    Capture::NameOutsideBlock => !inside(node, BLOCK_KIND),
                    Capture::Callee | Capture::ImportNode => false,
                };
                if counts {
                    names.push((text(node, source)?.to_string(), first_line(node)?));
                }
                Ok(())
            },
        )?;
        names.sort_unstable();
        names.dedup();
        Ok(names)
    }

    /// Run the `(role, lang)` query over `tree` and hand every captured node
    /// to `on_capture` with its meaning.
    ///
    /// # Errors
    ///
    /// No query for `(role, lang)`; a capture the query did not declare; an
    /// `on_capture` error (named with the query file); the query exceeded the
    /// cursor's match limit.
    fn visit(
        &self,
        role: Role,
        lang: OracleLang,
        scratch: &mut OracleScratch,
        tree: &Tree,
        source: &str,
        mut on_capture: impl FnMut(Capture, Node<'_>) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let compiled = self
            .queries
            .iter()
            .find(|q| q.role == role && q.lang == lang)
            .ok_or_else(|| anyhow::anyhow!("no {} role query for {lang}", role.file_stem()))?;
        {
            let mut matches =
                scratch
                    .cursor
                    .matches(&compiled.query, tree.root_node(), source.as_bytes());
            while let Some(m) = matches.next() {
                for capture in m.captures {
                    let kind = usize::try_from(capture.index)
                        .ok()
                        .and_then(|index| compiled.captures.get(index))
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "role query {} matched capture {} it did not declare",
                                compiled.file_name,
                                capture.index
                            )
                        })?;
                    on_capture(*kind, capture.node)
                        .with_context(|| format!("role query {}", compiled.file_name))?;
                }
            }
        }
        anyhow::ensure!(
            !scratch.cursor.did_exceed_match_limit(),
            "role query {} needed more than the oracle's match limit of {} in-progress \
             matches, so tree-sitter dropped matches",
            compiled.file_name,
            scratch.cursor.match_limit()
        );
        Ok(())
    }
}

fn compile(spec: &RoleQuery) -> anyhow::Result<CompiledRole> {
    let file_name = spec.file_name();
    let query = Query::new(&spec.lang.grammar(), spec.source)
        .with_context(|| format!("compiling role query {file_name}"))?;
    let captures = query
        .capture_names()
        .iter()
        .map(|name| {
            spec.role.capture(name).ok_or_else(|| {
                anyhow::anyhow!(
                    "role query {file_name} has the capture @{name}, which a {} query does not use",
                    spec.role.file_stem()
                )
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    anyhow::ensure!(
        query
            .capture_index_for_name(spec.role.required_capture())
            .is_some(),
        "role query {file_name} has no @{} capture",
        spec.role.required_capture()
    );
    Ok(CompiledRole {
        role: spec.role,
        lang: spec.lang,
        file_name,
        query,
        captures,
    })
}

// ============================================================================
// Reading captures
// ============================================================================

/// Push the line of every site that one captured `node` gives for `name`.
fn collect(
    kind: Capture,
    node: Node<'_>,
    source: &str,
    name: &str,
    lines: &mut Vec<u32>,
) -> anyhow::Result<()> {
    match kind {
        Capture::Name | Capture::Callee => push_if_named(node, source, name, lines),
        Capture::NameOutsideBlock => {
            if inside(node, BLOCK_KIND) {
                Ok(())
            } else {
                push_if_named(node, source, name, lines)
            }
        }
        Capture::ImportNode => identifier_leaves_named(node, source, name, lines),
    }
}

/// Push the line of `node` when its text is exactly `name`.
fn push_if_named(
    node: Node<'_>,
    source: &str,
    name: &str,
    lines: &mut Vec<u32>,
) -> anyhow::Result<()> {
    if text(node, source)? == name {
        lines.push(first_line(node)?);
    }
    Ok(())
}

/// Push the line of every identifier leaf at or below `root` whose text is
/// exactly `name`, in one pre-order pass bounded by the subtree's node count.
fn identifier_leaves_named(
    root: Node<'_>,
    source: &str,
    name: &str,
    lines: &mut Vec<u32>,
) -> anyhow::Result<()> {
    let nodes = root.descendant_count();
    let mut cursor = root.walk();
    for _ in 0..nodes {
        let node = cursor.node();
        if is_identifier_leaf(node) {
            push_if_named(node, source, name, lines)?;
        }
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return Ok(());
            }
        }
    }
    anyhow::bail!("the walk below an import node did not end within its {nodes} nodes")
}

/// Whether `node` is a named leaf whose kind is `identifier` or ends in
/// `_identifier` (`type_identifier`, `package_identifier`, ...).
fn is_identifier_leaf(node: Node<'_>) -> bool {
    node.is_named()
        && node.child_count() == 0
        && (node.kind() == "identifier" || node.kind().ends_with("_identifier"))
}

/// Whether an ancestor of `node` has kind `kind`.
fn inside(node: Node<'_>, kind: &str) -> bool {
    std::iter::successors(node.parent(), Node::parent).any(|ancestor| ancestor.kind() == kind)
}

/// The text of `node` in `source`.
fn text<'s>(node: Node<'_>, source: &'s str) -> anyhow::Result<&'s str> {
    source
        .get(node.start_byte()..node.end_byte())
        .ok_or_else(|| anyhow::anyhow!("node {:?} is outside the source", node.kind()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code — unwrap/expect/panic acceptable for test assertions
#[path = "roles_tests.rs"]
mod tests;
