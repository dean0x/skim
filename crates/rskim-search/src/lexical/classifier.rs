//! AST-aware field classifier that maps source byte ranges to [`SearchField`] variants.
//!
//! # Algorithm
//!
//! 1. Parse `source` via [`rskim_core::Parser`] for the given language.
//! 2. Walk the tree in pre-order, collecting non-`Other` `(Range, SearchField)` tuples.
//!    Memory: O(AST nodes), not O(source bytes).
//! 3. Process ranges in reverse (innermost/children first) using interval subtraction
//!    against an "uncovered" set so that the deepest node wins each byte.
//! 4. Fill uncovered gaps with `SearchField::Other`, then merge adjacent same-field
//!    ranges.
//!
//! # Format-specific dispatchers
//!
//! JSON, YAML, TOML, and Markdown are dispatched to dedicated classifiers
//! **before** the generic tree-sitter path:
//!
//! - JSON/YAML/TOML: lightweight std-only scanners (infallible, no tree-sitter).
//! - Markdown: tree-sitter via [`rskim_core`] with custom heading-level logic.
//!
//! Only languages that are not handled by a dedicated classifier reach the
//! generic tree-sitter path below.
//!
//! # Invariants
//!
//! The returned `Vec` satisfies:
//! - Sorted ascending by `range.start`.
//! - Non-overlapping (no two ranges share a byte).
//! - Contiguous (covers every byte from 0 to `source.len()`).
//! - `sum(range.end - range.start) == source.len()`.
//! - For empty source, an empty `Vec` is returned.

use std::ops::Range;

use rskim_core::Language;

use crate::SearchField;

/// Returns `true` if `kind` is an identifier-family node type that receives
/// context-aware field classification (AD-411-1).
#[inline]
fn is_identifier_kind(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "type_identifier"
            | "field_identifier"
            | "property_identifier"
            | "variable_name"
            | "attribute_name"
    )
}

/// Returns `true` if `kind` is a value/const/binding/field/variant declaration
/// whose `name`-field child is a definition name that should receive a
/// definition-tier field (AD-411-1, OD4).
///
/// These kinds are NOT in the priority 3,4,5 set but their name-child identifiers
/// are definitions, not call sites.  Also includes C/C++ declarator helper nodes
/// so their identifier children receive the SymbolName degradation (not FunctionBody)
/// when the grammar does not use `"name"` as the field name.
///
/// See also [`is_container_decl_kind`] for the class/module container complement.
#[inline]
fn is_value_decl_kind(kind: &str) -> bool {
    matches!(
        kind,
        // Rust
        "const_item"
            | "static_item"
            | "let_declaration"
            // TypeScript / JavaScript
            | "lexical_declaration"
            | "variable_declarator"
            | "variable_declaration"
            | "public_field_definition"
            // Java / C# / Swift / Kotlin
            | "field_declaration"
            | "local_variable_declaration"
            | "constant_declaration"
            | "property_declaration"
            // C / C++ declaration helpers (name is nested under declarator, not name:)
            | "function_declarator"
            | "declarator"
            | "init_declarator"
            | "pointer_declarator"
            | "parameter_declaration"
            // Go
            | "var_declaration"
            | "const_declaration"
            | "short_var_declaration"
            | "var_spec"
            | "const_spec"
            // Enum variants (Rust enum_variant, Java/Kotlin enum_entry)
            | "enum_variant"
            | "enum_entry"
            // SQL: create_query holds the table/view/index name via name: identifier;
            // column_definition holds the column name similarly.
            // object_reference wraps the table name in CREATE TABLE via name: identifier
            // (tree-sitter-sequel: create_table → object_reference → name: identifier).
            // Adding it here ensures table names are never mis-classified as FunctionBody;
            // they degrade to FunctionSignature (definition tier) even in reference
            // contexts where the grandparent is not a CREATE statement.
            | "create_query"
            | "column_definition"
            | "object_reference"
    )
}

/// Returns `true` if `kind` is a class/module/impl container node (priority 2 in
/// `rskim_core::node_kind_info`) whose `name:` field child is the container's own
/// definition name and should rank at the [`SearchField::TypeDefinition`] tier rather
/// than the call-site tier.
///
/// # Semantic rationale
///
/// `rskim_core::node_kind_priority` assigns class/module/namespace containers priority 2
/// because the *truncation* algorithm (which shares the same table) must rank a class
/// *body* lower than a function body — priority there governs body retention ordering,
/// not the semantic tier of the class's *name*.  Consequently the `>= 3` threshold in
/// [`map_identifier_to_field`] excludes these containers from the declaration check, and
/// without this function the class *name* identifier falls through to
/// [`SearchField::FunctionBody`] (call-site tier) — the cross-language inconsistency
/// identified in the #411 review (applies ADR-007 dog-food gate; applies ADR-001
/// fix-immediately rule).
///
/// This function corrects that by marking all priority-2 container kinds as declaration
/// contexts.  When their `name:` child is an [`is_identifier_kind`] identifier,
/// `map_identifier_to_field` returns [`SearchField::TypeDefinition`] — the same tier
/// used for Rust `struct_item` (priority 5) and Python `class_definition` (priority 5),
/// giving class-based OO languages consistent definition-name ranking.
///
/// # Coverage of kinds whose identifier types are not yet in `is_identifier_kind`
///
/// Ruby `class` / `module` (name: `constant`), Kotlin `class_declaration`
/// (name: `simple_identifier`), and C++ `namespace_definition`
/// (name: `namespace_identifier`) use identifier types absent from
/// [`is_identifier_kind`], so their name children are not routed to
/// [`map_identifier_to_field`] today.  They are included here for
/// forward-compatibility: when their identifier types are added to
/// [`is_identifier_kind`] they will automatically inherit the correct tier.
///
/// Rust `impl_item`'s name child uses field `type:` (not `name:`), and Go's
/// `interface_type` / `struct_type` hold the type name in a parent `type_spec`,
/// so neither kind will hit the `field_name == Some("name")` branch — including
/// them here is harmless.
#[inline]
fn is_container_decl_kind(kind: &str) -> bool {
    matches!(
        kind,
        // TS / JS / Java / C# / Kotlin — class definitions (name: identifier or type_identifier)
        "class_declaration"
            // C++ — class/struct definitions (name: type_identifier)
            | "class_specifier"
            // Ruby — class (name: constant, not yet in is_identifier_kind; harmless today)
            | "class"
            // Ruby / TS / JS — module (name: constant or identifier)
            | "module"
            | "module_declaration"
            // Rust — impl blocks (child field is `type:`, not `name:` — never matches)
            | "impl_item"
            // C++ / C# — namespaces (name: namespace_identifier — not in is_identifier_kind)
            | "namespace_definition"
            | "namespace_declaration"
            // Go — interface_type / struct_type: name lives in parent type_spec, harmless
            | "interface_type"
            | "struct_type"
    )
}

/// Map an identifier node to a [`SearchField`] using parent context.
///
/// # AD-411-1
///
/// Declaration-name rule: an identifier-family node is a "declaration name"
/// iff `cursor.field_name()` returns `Some("name")` AND its direct parent is a
/// declaration kind (priority 3/4/5, a value/const/binding/field/variant kind,
/// OR a class/module container kind — see [`is_container_decl_kind`]).
///
/// - `field_name == Some("name")` + parent priority 4 (fn) → [`SearchField::FunctionSignature`]
/// - `field_name == Some("name")` + parent priority 5 (type) → [`SearchField::TypeDefinition`]
/// - `field_name == Some("name")` + parent priority 3 (import) → [`SearchField::ImportExport`]
/// - `field_name == Some("name")` + parent is container-decl kind → [`SearchField::TypeDefinition`]
///   (class / module / namespace names rank at the type-definition tier — cross-language
///   consistency fix for TS/JS/Java/C++/C# `class Foo {}` names; AD-411-1 container rule)
/// - `field_name == Some("name")` + parent is value-decl kind → [`SearchField::FunctionSignature`]
///   (**AD-411-6** option a: const/value/binding definition names map to
///   FunctionSignature so their boost on the exact-symbol path is strictly above
///   TypeDefinition, ensuring a const definition outranks a Markdown heading
///   whose `classify_markdown` stamps it as TypeDefinition — OD3 + OD4 satisfied)
/// - `field_name != Some("name")` + parent is ANY declaration kind →
///   [`SearchField::SymbolName`] (degradation: grammar uses a different field
///   name; never ranks below today's unconditional SymbolName — OD4 guarantee)
/// - otherwise (non-declaration parent) → [`SearchField::FunctionBody`]
///   (call sites, references, and type-reference identifiers)
fn map_identifier_to_field(
    field_name: Option<&'static str>,
    parent_kind: Option<&'static str>,
) -> SearchField {
    let parent = match parent_kind {
        Some(k) => k,
        None => return SearchField::FunctionBody,
    };

    let parent_priority = rskim_core::node_kind_priority(parent);
    let parent_is_decl =
        parent_priority >= 3 || is_value_decl_kind(parent) || is_container_decl_kind(parent);

    if !parent_is_decl {
        // Not in any declaration context → call site / reference / type reference.
        return SearchField::FunctionBody;
    }

    if field_name != Some("name") {
        // In a declaration but field_name is not "name": the grammar uses a
        // different field for its declaration name child (e.g. C's `declarator:`
        // or Kotlin's `simple_identifier`).  Degrade to SymbolName — guaranteed
        // not worse than the old unconditional SymbolName path (OD4).
        return SearchField::SymbolName;
    }

    // Container kinds (priority 2) define named types / modules.  Their `name:`
    // identifier child is a type-definition name, not a call site.
    // AD-411-1 container rule: map to TypeDefinition for cross-language consistency
    // with Rust struct_item (priority 5) and Python class_definition (priority 5).
    if is_container_decl_kind(parent) {
        return SearchField::TypeDefinition;
    }

    // We are the "name:" field child of a priority-3/4/5 declaration — inherit field.
    match parent_priority {
        5 => SearchField::TypeDefinition,
        4 => SearchField::FunctionSignature,
        3 => SearchField::ImportExport,
        _ => {
            // Parent is a value/const/binding/field/variant declaration (priority < 3,
            // caught by is_value_decl_kind above).
            // AD-411-6 (option a): map to FunctionSignature.
            // Rationale for option a over option b: mapping const/value names to
            // FunctionSignature keeps the multi-word BM25FConfig::default() SymbolName
            // boost (3.5) unchanged — it remains the degradation tier for grammars
            // without a "name:" field rather than being re-tuned as the primary
            // const-definition tier.  FunctionSignature (boost 4.0 default, 8.0
            // exact-symbol) is semantically loose but rank-correct: these are
            // definitions, and definition-tier boost is what the scorer needs.
            SearchField::FunctionSignature
        }
    }
}

/// Map a non-identifier AST node to a [`SearchField`].
///
/// Handles comments, string literals, body blocks, and priority-based
/// classification.  Identifier-family nodes are dispatched separately by
/// [`map_identifier_to_field`] which has access to cursor context (AD-411-1).
///
/// # Coupling
///
/// The specific node kinds matched here (comments, strings, body blocks) are
/// kept in sync with `rskim_core::transform::utils::node_kind_info` and
/// `rskim_core::transform::utils::find_body_child`.  Any new node kind
/// categories added to those functions may need a corresponding case here.
fn map_priority_to_field(kind: &str, priority: u8) -> SearchField {
    // First check for specific comment/string kinds regardless of priority.
    match kind {
        "comment" | "line_comment" | "block_comment" | "doc_comment" => {
            return SearchField::Comment;
        }
        "string_literal"
        | "string"
        | "interpreted_string_literal"
        | "raw_string_literal"
        | "string_content"
        | "raw_str_literal"
        | "template_string"
        | "template_literal"
        | "quoted_string" => {
            return SearchField::StringLiteral;
        }
        // Body/block nodes → FunctionBody.
        //
        // Without this arm, block nodes return Other from the priority match
        // below (priority 1) and are skipped by the innermost-wins rule
        // (Other is the default fill — we only overwrite non-Other). The parent
        // function node has already stamped those bytes as FunctionSignature,
        // so they remain FunctionSignature even though they are body bytes.
        // This inflates FunctionSignature field lengths by the full body size,
        // which artificially raises BM25F scores for body-content queries.
        //
        // Kinds here mirror rskim_core::transform::utils::find_body_child (the
        // union of all body kinds across supported languages).
        "block"              // Rust, Python, Go, Java, C#, Kotlin
        | "statement_block" // TypeScript, JavaScript
        | "compound_statement" // C, C++
        | "constructor_body"   // Java
        | "body_statement"     // Ruby
        | "function_body" => {
            // Kotlin, Swift
            return SearchField::FunctionBody;
        }
        _ => {}
    }

    match priority {
        5 => SearchField::TypeDefinition,
        4 => SearchField::FunctionSignature,
        3 => SearchField::ImportExport,
        _ => SearchField::Other,
    }
}

/// Maximum source size (in bytes) accepted by [`classify_source`].
///
/// Although the classifier allocates O(AST nodes) rather than O(source bytes),
/// an unbounded source could still trigger proportional tree-sitter parsing
/// time. 100 MiB is generous for any real source file.
pub const MAX_SOURCE_BYTES: usize = 100 * 1024 * 1024; // 100 MiB

/// Classify all bytes in `source` according to their AST-derived field.
///
/// Returns a sorted, non-overlapping, contiguous list of `(Range<usize>, SearchField)`
/// tuples covering every byte from `0` to `source.len()`.
///
/// For empty source, returns an empty vector.
/// For languages without tree-sitter support, returns a single `Other` range.
///
/// # Errors
///
/// Returns [`crate::SearchError`] if the tree-sitter parser fails to initialise
/// (grammar loading failure, which should not happen in practice for supported
/// languages).
pub fn classify_source(
    source: &str,
    lang: Language,
) -> crate::Result<Vec<(Range<usize>, SearchField)>> {
    if source.is_empty() {
        return Ok(Vec::new());
    }

    let len = source.len();

    if len > MAX_SOURCE_BYTES {
        return Err(crate::SearchError::FileTooLarge {
            size: len,
            limit: MAX_SOURCE_BYTES,
        });
    }

    // Format-specific classifiers dispatched BEFORE the generic tree-sitter path.
    // These scanners handle non-tree-sitter formats (JSON, YAML, TOML) and the
    // Markdown tree-sitter grammar which needs custom heading-level logic.
    match lang {
        Language::Json => {
            return Ok(crate::fields::serde_fields::classify_json(source));
        }
        Language::Yaml => {
            return Ok(crate::fields::serde_fields::classify_yaml(source));
        }
        Language::Toml => {
            return Ok(crate::fields::serde_fields::classify_toml(source));
        }
        Language::Markdown => {
            return crate::fields::markdown::classify_markdown(source);
        }
        _ => {}
    }

    // Generic tree-sitter path for all other languages.
    let mut parser = match rskim_core::Parser::new(lang) {
        Ok(p) => p,
        Err(_) => {
            // Language does not use tree-sitter — return single Other range.
            return Ok(vec![(0..len, SearchField::Other)]);
        }
    };

    let tree = parser.parse(source)?;
    let root = tree.root_node();

    // Collect non-Other AST node ranges in pre-order (parents before children).
    // Memory: O(AST_nodes) instead of the previous O(source_bytes) per-byte array.
    let mut node_ranges: Vec<(Range<usize>, SearchField)> = Vec::new();

    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        let byte_range = node.byte_range();

        // Clamp range to source bounds (safety against malformed AST).
        let start = byte_range.start.min(len);
        let end = byte_range.end.min(len);

        if start < end {
            let kind = node.kind();

            // AD-411-1: identifier-family nodes receive context-aware field
            // classification — their field depends on whether they are the
            // declaration name child of a declaring parent node.
            let field = if is_identifier_kind(kind) {
                // cursor.field_name() returns the field name of the current node
                // within its parent (e.g. Some("name") for `name:` fields).
                // node.parent().map(|p| p.kind()) is always &'static str in
                // tree-sitter 0.25 (grammar node kinds are interned statics).
                let field_name = cursor.field_name();
                let parent_kind = node.parent().map(|p| p.kind());
                map_identifier_to_field(field_name, parent_kind)
            } else {
                let priority = rskim_core::node_kind_priority(kind);
                map_priority_to_field(kind, priority)
            };

            if field != SearchField::Other {
                node_ranges.push((start..end, field));
            }
        }

        // Advance cursor in pre-order.
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok(build_field_ranges(node_ranges, len));
            }
        }
    }
}

/// Build the final contiguous range list from pre-order node ranges.
///
/// Implements "innermost wins": processes ranges in reverse (children first)
/// so deeper nodes claim bytes before their parents. Uncovered bytes become
/// [`SearchField::Other`]. Adjacent same-field ranges are merged.
///
/// # Preconditions
///
/// - `node_ranges` must be in **pre-order** (parents before children). The
///   reverse-iteration loop relies on children appearing after their parents.
/// - Ranges may overlap; the algorithm resolves overlaps via interval subtraction
///   so the deepest (innermost) node wins each byte.
/// - `source_len` must equal the byte length of the source that was parsed.
///
/// # Output guarantees
///
/// The returned `Vec` is:
/// - Sorted ascending by `range.start`.
/// - Non-overlapping (no two ranges share a byte).
/// - Contiguous (covers every byte `0..source_len`).
/// - `sum(range.end - range.start) == source_len`.
/// - For empty input (`node_ranges.is_empty()`), returns a single `Other`
///   range covering `0..source_len`.
pub(crate) fn build_field_ranges(
    node_ranges: Vec<(Range<usize>, SearchField)>,
    source_len: usize,
) -> Vec<(Range<usize>, SearchField)> {
    if node_ranges.is_empty() {
        return vec![(0..source_len, SearchField::Other)];
    }

    // Track which byte intervals are still unclaimed. Starts as the full source.
    // Double-buffer to reuse allocations across iterations.
    let mut uncovered: Vec<Range<usize>> = vec![];
    uncovered.push(0..source_len);
    let mut next_uncovered: Vec<Range<usize>> = Vec::new();
    let mut result: Vec<(Range<usize>, SearchField)> = Vec::with_capacity(node_ranges.len() * 2);

    // Reverse: children (later in pre-order) claim bytes before parents.
    for (range, field) in node_ranges.into_iter().rev() {
        next_uncovered.clear();

        for unc in &uncovered {
            if unc.end <= range.start || unc.start >= range.end {
                // No overlap — interval stays uncovered.
                next_uncovered.push(unc.clone());
            } else {
                // Overlap — split around the claimed region.
                if unc.start < range.start {
                    next_uncovered.push(unc.start..range.start);
                }
                result.push((unc.start.max(range.start)..unc.end.min(range.end), field));
                if unc.end > range.end {
                    next_uncovered.push(range.end..unc.end);
                }
            }
        }

        std::mem::swap(&mut uncovered, &mut next_uncovered);
    }

    // Remaining uncovered bytes → Other.
    for gap in uncovered {
        result.push((gap, SearchField::Other));
    }

    result.sort_unstable_by_key(|(r, _)| r.start);
    merge_adjacent(&mut result);
    result
}

/// Merge adjacent ranges that share the same field into a single range.
pub(crate) fn merge_adjacent(ranges: &mut Vec<(Range<usize>, SearchField)>) {
    if ranges.len() <= 1 {
        return;
    }
    let mut write = 0;
    for read in 1..ranges.len() {
        if ranges[read].1 == ranges[write].1 && ranges[read].0.start == ranges[write].0.end {
            ranges[write].0.end = ranges[read].0.end;
        } else {
            write += 1;
            ranges[write] = ranges[read].clone();
        }
    }
    ranges.truncate(write + 1);
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "classifier_tests.rs"]
mod tests;
