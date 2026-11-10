; Structural oracle for skim's `rust-nested-loop` --ast pattern (rust).
; Grammar: tree-sitter-rust 0.24.0.
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   A for-expression inside a block statement (Rust). Structural approximation:
;   block -> expression_statement -> for_expression -- this trigram appears in
;   nested loops (the inner loop is an expression_statement in the outer loop's
;   block) but also matches any for loop inside a block.
;
; Encoded definition:
;   a for_expression that is the expression_statement child of a block. As the
;   description says, this also matches an un-nested for loop in a function
;   body; the nested-loop intent is the separate intent oracle in structural.rs.
;
; Match line: the first line of the @match node, the for_expression.
(block
  (expression_statement
    (for_expression) @match))
