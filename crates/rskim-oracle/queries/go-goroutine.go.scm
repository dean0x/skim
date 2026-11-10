; Structural oracle for skim's `go-goroutine` --ast pattern (go).
; Grammar: tree-sitter-go 0.25.0.
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   A goroutine launch (Go). Exact: go_statement is the tree-sitter node for `go
;   f()` calls.
;
; Encoded definition:
;   every go_statement.
;
; Match line: the first line of the @match node, the go_statement.
(go_statement) @match
