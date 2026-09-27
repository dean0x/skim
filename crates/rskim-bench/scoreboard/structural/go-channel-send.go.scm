; Structural oracle for skim's `go-channel-send` --ast pattern (go).
; Grammar: tree-sitter-go 0.25.0.
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   A channel send operation (Go). Exact: send_statement is the CST node for `ch
;   <- val`.
;
; Encoded definition:
;   every send_statement, whatever its channel and value expressions are
;   (including a send in a select case).
;
; Match line: the first line of the @match node, the send_statement.
(send_statement) @match
