; Structural oracle for skim's `try-catch` --ast pattern (typescript).
; Grammar: tree-sitter-typescript 0.23.2 (LANGUAGE_TYPESCRIPT).
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   A try/catch block (TypeScript/JavaScript). Exact: try_statement always
;   contains a catch_clause.
;
; Encoded definition:
;   a try_statement with a catch_clause child.
;
; Match line: the first line of the @match node, the try_statement.
(try_statement
  (catch_clause)) @match
