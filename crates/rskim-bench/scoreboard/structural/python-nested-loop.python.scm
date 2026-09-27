; Structural oracle for skim's `python-nested-loop` --ast pattern (python).
; Grammar: tree-sitter-python 0.25.0.
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   A loop nested inside another loop (Python). Structural approximation:
;   for_statement -> block -> for_statement -- the inner loop appears in the
;   body block of the outer loop.
;
; Encoded definition:
;   a for_statement that is a direct child of the block body of another
;   for_statement.
;
; Match line: the first line of the @match node, the inner for_statement.
(for_statement
  (block
    (for_statement) @match))
