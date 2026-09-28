; Structural oracle for skim's `method-with-body` --ast pattern (typescript).
; Grammar: tree-sitter-typescript 0.23.2 (LANGUAGE_TYPESCRIPT).
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   A method definition with a body (TypeScript/JavaScript). Exact:
;   method_definition always contains a statement_block.
;
; Encoded definition:
;   a method_definition with a statement_block child (class and object literal
;   methods alike).
;
; Match line: the first line of the @match node, the method_definition.
(method_definition
  (statement_block)) @match
