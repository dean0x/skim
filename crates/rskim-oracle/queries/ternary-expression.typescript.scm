; Structural oracle for skim's `ternary-expression` --ast pattern (typescript).
; Grammar: tree-sitter-typescript 0.23.2 (LANGUAGE_TYPESCRIPT).
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   A ternary conditional expression in assignment context
;   (TypeScript/JavaScript). Structural approximation: variable_declarator
;   contains ternary_expression -- this covers the assignment form but not all
;   ternary positions.
;
; Encoded definition:
;   a ternary_expression that is a direct child of a variable_declarator.
;
; Match line: the first line of the @match node, the ternary_expression.
(variable_declarator
  (ternary_expression) @match)
