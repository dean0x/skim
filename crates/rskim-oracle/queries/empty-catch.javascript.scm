; Structural oracle for skim's `empty-catch` --ast pattern (javascript).
; Grammar: tree-sitter-javascript 0.25.0.
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   A catch clause with an empty body (TypeScript/JavaScript). Exact: EMPTY_BODY
;   is emitted for a statement_block with zero counted children, keyed on the
;   enclosing catch_clause.
;
; Encoded definition:
;   a catch_clause whose statement_block body has zero body elements. Post-
;   filter (Rust, structural.rs `PostFilter::Empty`): a body element is a named,
;   non-extra child that is not an attribute, so a comment-only body is empty.
;
; Match line: the first line of the @match node, the catch_clause.
(catch_clause
  (statement_block) @body) @match
