; Structural oracle for skim's `try-catch-finally` --ast pattern (javascript).
; Grammar: tree-sitter-javascript 0.25.0.
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   A try/catch/finally block (TypeScript/JavaScript). Exact: all three clauses
;   are children of try_statement.
;
; Encoded definition:
;   ONE try_statement carrying both a catch_clause and a finally_clause. A
;   try/finally wrapping a separate try/catch is not a match.
;
; Match line: the first line of the @match node, the try_statement.
(try_statement
  (catch_clause)
  (finally_clause)) @match
