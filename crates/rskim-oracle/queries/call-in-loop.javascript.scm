; Structural oracle for skim's `call-in-loop` --ast pattern (javascript).
; Grammar: tree-sitter-javascript 0.25.0.
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   A function call inside a for-of loop body (TypeScript/JavaScript).
;   Structural approximation: for_in_statement -> statement_block ->
;   expression_statement is the trigram showing a loop body with a statement.
;
; Encoded definition:
;   an expression_statement that is a direct child of the statement_block body
;   of a for_in_statement (tree-sitter's node for both for-in and for-of).
;
; Match line: the first line of the @match node, the expression_statement.
(for_in_statement
  (statement_block
    (expression_statement) @match))
