; Structural oracle for skim's `switch-with-cases` --ast pattern (typescript).
; Grammar: tree-sitter-typescript 0.23.2 (LANGUAGE_TYPESCRIPT).
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   A switch statement with a switch body (TypeScript/JavaScript). Exact:
;   switch_statement always contains a switch_body.
;
; Encoded definition:
;   a switch_statement with a switch_body child.
;
; Match line: the first line of the @match node, the switch_statement.
(switch_statement
  (switch_body)) @match
