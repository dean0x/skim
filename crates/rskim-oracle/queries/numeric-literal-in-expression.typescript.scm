; Structural oracle for skim's `numeric-literal-in-expression` --ast pattern (typescript).
; Grammar: tree-sitter-typescript 0.23.2 (LANGUAGE_TYPESCRIPT).
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   Structural approximation: a number literal appearing inside a binary
;   expression (TypeScript/JavaScript).
;
; Encoded definition:
;   a number that is a direct child of a binary_expression.
;
; Match line: the first line of the @match node, the number.
(binary_expression
  (number) @match)
