; Structural oracle for skim's `try-finally` --ast pattern (typescript).
; Grammar: tree-sitter-typescript 0.23.2 (LANGUAGE_TYPESCRIPT).
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   A try/finally block (TypeScript/JavaScript). Exact: try_statement always
;   contains a finally_clause.
;
; Encoded definition:
;   a try_statement with a finally_clause child. The same node kinds and
;   relation exist in the Python grammar, so Python is covered too.
;
; Match line: the first line of the @match node, the try_statement.
(try_statement
  (finally_clause)) @match
