; Structural oracle for skim's `unhandled-result` --ast pattern (tsx).
; Grammar: tree-sitter-typescript 0.23.2 (LANGUAGE_TSX).
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   Structural approximation: an expression_statement containing a
;   call_expression (TypeScript/JavaScript). This is a weak structural proxy
;   that matches any top-level call.
;
; Encoded definition:
;   an expression_statement with a call_expression child. The same node kinds
;   and relation exist in the Rust and Go grammars, so they are covered too
;   (Python's call node is `call`, not call_expression).
;
; Match line: the first line of the @match node, the expression_statement.
(expression_statement
  (call_expression)) @match
