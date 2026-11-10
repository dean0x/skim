; Structural oracle for skim's `nested-loop` --ast pattern (javascript).
; Grammar: tree-sitter-javascript 0.25.0.
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   A loop nested inside another loop (TypeScript/JavaScript for-statements).
;   Exact: for_statement -> statement_block -> for_statement is the structural
;   trigram that identifies an outer for loop whose body contains an inner for
;   loop.
;
; Encoded definition:
;   a for_statement that is a direct child of the statement_block body of
;   another for_statement. The broader intent (any loop kind, any depth within
;   one function) is the separate intent oracle in structural.rs.
;
; Match line: the first line of the @match node, the inner for_statement (the
; loop that is nested).
(for_statement
  (statement_block
    (for_statement) @match))
