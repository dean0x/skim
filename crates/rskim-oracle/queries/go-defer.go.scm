; Structural oracle for skim's `go-defer` --ast pattern (go).
; Grammar: tree-sitter-go 0.25.0.
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   A defer statement (Go). Exact: defer_statement is emitted for `defer f()`.
;
; Encoded definition:
;   every defer_statement.
;
; Match line: the first line of the @match node, the defer_statement.
(defer_statement) @match
