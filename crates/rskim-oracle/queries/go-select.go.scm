; Structural oracle for skim's `go-select` --ast pattern (go).
; Grammar: tree-sitter-go 0.25.0.
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   A select statement (Go). Exact: select_statement is the CST node for `select
;   { case ... }`.
;
; Encoded definition:
;   every select_statement, including `select {}` and a select with only a
;   default case.
;
; Match line: the first line of the @match node, the select_statement.
(select_statement) @match
