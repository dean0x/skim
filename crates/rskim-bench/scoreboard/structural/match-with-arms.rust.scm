; Structural oracle for skim's `match-with-arms` --ast pattern (rust).
; Grammar: tree-sitter-rust 0.24.0.
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   A match expression with match arms (Rust). Exact: match_expression always
;   contains a match_block, which contains match_arm nodes.
;
; Encoded definition:
;   a match_expression whose match_block holds at least one match_arm (`match x
;   {}` has no arms and is not a match).
;
; Match line: the first line of the @match node, the match_expression.
(match_expression
  (match_block
    (match_arm))) @match
