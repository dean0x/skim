; Structural oracle for skim's `python-try-except` --ast pattern (python).
; Grammar: tree-sitter-python 0.25.0.
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   A try/except block (Python). Exact: try_statement contains except_clause.
;
; Encoded definition:
;   a try_statement with an except_clause child (tree-sitter-python 0.25 parses
;   `except*` as except_clause too).
;
; Match line: the first line of the @match node, the try_statement.
(try_statement
  (except_clause)) @match
